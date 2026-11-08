//! The X window manager for one Xwayland run.
//!
//! It owns the `-wm` connection. Displayfd readiness starts it. A dead connection
//! or a failed startup ends the run.

#[cfg(test)]
mod capture_tests;
mod cursor;
mod focus;
#[cfg(test)]
mod focus_tests;
mod hints;
#[cfg(test)]
mod manage_tests;
mod mime;
mod props;
pub(super) mod selection;
#[cfg(test)]
mod selection_tests;
mod setup;
#[cfg(test)]
mod tests;
mod timer;
mod toplevel;
mod transfer;
mod window;

use std::{
    collections::{BTreeMap, VecDeque},
    fmt::{self, Display, Formatter},
    os::fd::{OwnedFd, RawFd},
    rc::Rc,
    time::Instant,
};

use crate::{
    io_uring::ring::{ReadWriteCompleted, RingId},
    runner::{Dispatcher, Redispatcher},
    state::ObjectId,
    x11::{
        AtomCache, Atoms, Extensions, Property, PropertyError, RenderQueryPictFormatsReply,
        ResQueryClientIdsReply, Startup, StartupEvent, State as ConnectionState, Status, Window,
        XConnection, XIo, XReplyError,
    },
    State,
};

use super::{SurfaceSerials, Xwayland, XwaylandAction};

type Completion = std::io::Result<ReadWriteCompleted>;

pub(super) struct Wm {
    conn: XConnection,
    io: XIo,
    startup: Startup,
    phase: WmPhase,
    display: u32,
    windows: BTreeMap<Window, window::XWindow>,
    by_serial: BTreeMap<u64, Window>,
    last_serial: u64,
    /// The X window that owns input focus.
    focus: Option<Window>,
    /// Sequence of the last `SetInputFocus` this WM sent.
    last_input_sequence: u64,
    /// Normal windows, bottom to top.
    stack: Vec<Window>,
    /// Normal windows in map order.
    map_order: Vec<Window>,
    /// Increments each time an override-redirect window is shown. Later is higher.
    shown_order: u64,
    timer: timer::TransferTimer,
    incoming: BTreeMap<selection::SelectionKind, VecDeque<transfer::Incoming>>,
    outgoing: BTreeMap<u64, transfer::Outgoing>,
    /// Ids for incoming and outgoing transfers. One counter keeps timer ids unique.
    next_transfer: u64,
}

enum WmPhase {
    Connecting,
    Starting,
    Ready(Ready),
    Failed,
}

struct Ready {
    atoms: Atoms,
    extensions: Extensions,
    root: Window,
    check_window: Window,
    cursor: Option<crate::cursor::CursorImage>,
    /// The X cursor currently set on the root. Freed after the next upload replaces it.
    root_cursor: Option<crate::x11::Cursor>,
    /// Tema has published `Xft.dpi` / `Xcursor.size` and still owes the unscaled values.
    hints_written: bool,
    /// Latest `RESOURCE_MANAGER` read. An older reply is ignored.
    hints_fetch: u64,
    selections: selection::Selections,
    atom_cache: AtomCache,
}

/// Why the XWM stopped. The run is killed after this.
#[derive(Debug)]
pub(crate) struct WmFailure {
    reason: String,
}

impl Display for WmFailure {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

pub(crate) enum WmEvent {
    Read {
        generation: u64,
        read: RingId,
        result: Completion,
    },
    Written {
        generation: u64,
        token: u64,
        result: Completion,
    },
    Startup(StartupEvent),
    PictFormats(Result<RenderQueryPictFormatsReply, XReplyError>),
    /// `GetProperty` of the root `RESOURCE_MANAGER`.
    Resources {
        generation: u64,
        result: Result<Property, PropertyError>,
    },
    /// Rewrite DPI and cursor hints, and upload the root cursor again.
    RefreshHints,
    Property {
        window: Window,
        atom: crate::x11::Atom,
        result: Result<Property, PropertyError>,
    },
    /// The X-Resource pid for one window.
    ClientPid {
        window: Window,
        result: Result<ResQueryClientIdsReply, XReplyError>,
    },
    /// Ask the X client to close. Applied in a later phase.
    Close {
        window: Window,
    },
    /// The size of an interactive resize. Applied in a later phase.
    Resize {
        window: Window,
        width: i32,
        height: i32,
    },
    /// Send `ConfigureWindow` for each managed window whose layout rect changed.
    Reconfigure,
    /// Keyboard focus changed. The WM reads the seat focus when it handles this.
    KeyboardFocus,
    /// A Wayland `receive` asked for the bytes of an X selection.
    SelectionSend {
        source: ObjectId,
        mime_type: String,
        fd: Rc<OwnedFd>,
    },
    /// The seat dropped an X selection source.
    SelectionCancelled {
        source: ObjectId,
    },
    /// The Wayland clipboard or primary selection changed.
    SelectionChanged(selection::SelectionKind),
    /// The transfer timerfd read completed.
    TransferTimer,
    /// A deleting `GetProperty` of a selection window's `TARGETS` reply.
    SelectionTargets {
        kind: selection::SelectionKind,
        result: Result<crate::x11::Property, crate::x11::PropertyError>,
    },
    /// `InternAtom` for a MIME type a Wayland source offered.
    Interned {
        name: Vec<u8>,
        result: Result<crate::x11::InternAtomReply, crate::x11::XReplyError>,
    },
    /// `GetAtomName` for an atom listed in `TARGETS`.
    AtomName {
        kind: selection::SelectionKind,
        generation: u64,
        atom: crate::x11::Atom,
        result: Result<crate::x11::GetAtomNameReply, crate::x11::XReplyError>,
    },
    /// A deleting `GetProperty` of a selection window's data reply.
    IncomingProperty {
        kind: selection::SelectionKind,
        result: Result<Property, PropertyError>,
    },
    /// The pipe write of one selection chunk finished.
    IncomingWritten {
        id: u64,
        result: Completion,
    },
    /// The pipe read of one Wayland selection request finished.
    OutgoingRead {
        id: u64,
        result: Completion,
    },
}

pub(super) struct WmCtx<'a> {
    pub(super) run: u64,
    pub(super) state: &'a mut State,
    pub(super) dispatcher: &'a mut Dispatcher,
    pub(super) surfaces: &'a SurfaceSerials,
    /// The Xwayland Wayland client for this run, once it has connected.
    pub(super) client: Option<crate::clients::ClientId>,
}

impl WmCtx<'_> {
    /// Integer wire scale for this Xwayland client. One is the identity.
    pub(super) fn wire_scale(&self) -> crate::wire_scale::WireScale {
        self.state.xwayland_wire_scale
    }
}

impl Wm {
    /// Queues the setup request and arms the connection.
    /// `queue_bytes` before `start` only stores the setup request. `start` arms the read and writes it.
    pub(super) fn start(fd: RawFd, run: u64, display: u32, dispatcher: &mut Dispatcher) -> Self {
        let (conn, setup) = XConnection::new();
        let mut io = XIo::new(fd);
        io.queue_bytes(&setup, dispatcher);
        io.start(
            dispatcher,
            move |generation, read| {
                Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
                    run,
                    event: WmEvent::Read { generation, read, result },
                })
            },
            move |generation, token| {
                Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
                    run,
                    event: WmEvent::Written { generation, token, result },
                })
            },
        );
        Self {
            conn,
            io,
            startup: Startup::new(),
            phase: WmPhase::Connecting,
            display,
            windows: BTreeMap::new(),
            by_serial: BTreeMap::new(),
            last_serial: 0,
            focus: None,
            last_input_sequence: 0,
            stack: Vec::new(),
            map_order: Vec::new(),
            shown_order: 0,
            timer: timer::TransferTimer::new(run),
            incoming: BTreeMap::new(),
            outgoing: BTreeMap::new(),
            next_transfer: 0,
        }
    }

    pub(super) fn handle(&mut self, event: WmEvent, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let outcome = if matches!(self.phase, WmPhase::Failed) {
            Ok(())
        } else {
            match event {
                WmEvent::Read { generation, read, result } => {
                    let read =
                        self.io
                            .on_read(generation, read, result, &mut self.conn, ctx.dispatcher);
                    self.after_read(read, ctx)
                }
                WmEvent::Written { generation, token, result } => {
                    let read = self.io.on_written(
                        generation,
                        token,
                        result,
                        &mut self.conn,
                        ctx.dispatcher,
                    );
                    self.after_read(read, ctx)
                }
                WmEvent::Startup(event) => self.on_startup(event, ctx),
                WmEvent::PictFormats(result) => self.on_pict_formats(result),
                WmEvent::Resources { generation, result } => {
                    self.on_resources(generation, result, ctx)
                }
                WmEvent::RefreshHints => self.refresh_hints(ctx),
                WmEvent::Property { window, atom, result } => {
                    self.on_property_reply(window, atom, result, ctx)
                }
                WmEvent::ClientPid { window, result } => self.on_client_pid(window, result),
                WmEvent::Close { window } => self.close(window),
                WmEvent::Resize { window, width, height } => {
                    self.resize(window, width, height, ctx)
                }
                WmEvent::Reconfigure => self.reconfigure(ctx),
                WmEvent::KeyboardFocus => self.on_keyboard_focus(ctx),
                WmEvent::SelectionSend { source, mime_type, fd } => {
                    self.on_selection_send(source, mime_type, fd, ctx)
                }
                WmEvent::SelectionCancelled { source } => {
                    ctx.state.remove_object(source);
                    Ok(())
                }
                WmEvent::SelectionChanged(kind) => self.on_selection_changed(kind, ctx),
                WmEvent::SelectionTargets { kind, result } => {
                    self.on_selection_targets(kind, result, ctx)
                }
                WmEvent::AtomName { kind, generation, atom, result } => {
                    self.on_atom_name(kind, generation, atom, result, ctx)
                }
                WmEvent::Interned { name, result } => self.on_interned(name, result, ctx),
                WmEvent::IncomingProperty { kind, result } => {
                    self.on_incoming_property(kind, result, ctx)
                }
                WmEvent::IncomingWritten { id, result } => {
                    self.on_incoming_written(id, result, ctx)
                }
                WmEvent::OutgoingRead { id, result } => self.on_outgoing_read(id, result, ctx),
                WmEvent::TransferTimer => {
                    self.timer.read_finished();
                    self.expire_transfers(Instant::now(), ctx)
                }
            }
        };
        self.io.flush(&mut self.conn, ctx.dispatcher);
        outcome
    }

    pub(super) fn flush_outgoing(&mut self, dispatcher: &mut Dispatcher) {
        self.io.flush(&mut self.conn, dispatcher);
    }

    /// Cancels selection transfers, drops published X sources, stops the transfer
    /// timer, and releases managed windows. The caller drops the `-wm` fd.
    pub(super) fn stop(&mut self, state: &mut State, dispatcher: &mut Dispatcher) {
        self.cancel_transfers(dispatcher);
        self.clear_published_sources(state, dispatcher);
        self.timer.stop(dispatcher);
        self.release_managed(state, dispatcher);
        self.io.stop(dispatcher);
        self.phase = WmPhase::Failed;
    }

    fn expire_transfers(&mut self, now: Instant, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        for id in self.timer.expire(now) {
            self.transfer_timed_out(id, ctx)?;
        }
        self.timer.arm(ctx.dispatcher);
        Ok(())
    }

    /// Expire transfer deadlines at `now` and arm whatever remains.
    #[cfg(test)]
    pub(super) fn test_expire_transfers(&mut self, now: Instant, ctx: &mut WmCtx<'_>) {
        self.expire_transfers(now, ctx).expect("transfer timer");
    }

    fn after_read(&mut self, read: crate::x11::Read, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        for event in read.events {
            self.on_core_event(event, ctx)?;
        }
        if let Some(reason) = read.dead {
            self.phase = WmPhase::Failed;
            return Err(WmFailure { reason: reason.to_string() });
        }
        self.begin_if_connected(ctx)
    }

    fn begin_if_connected(&mut self, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        if self.conn.state() != ConnectionState::Ready || !matches!(self.phase, WmPhase::Connecting)
        {
            return Ok(());
        }
        let run = ctx.run;
        let redispatch = Redispatcher::to::<Xwayland>(move |event| XwaylandAction::Wm {
            run,
            event: WmEvent::Startup(event),
        });
        match self.startup.start(&mut self.conn, redispatch) {
            Status::Pending => {
                self.phase = WmPhase::Starting;
                Ok(())
            }
            Status::Ready(bringup) => self.become_ready(bringup, ctx),
            Status::Failed(error) => {
                self.phase = WmPhase::Failed;
                Err(WmFailure { reason: error.to_string() })
            }
        }
    }

    fn on_startup(&mut self, event: StartupEvent, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        match self.startup.handle(event, &mut self.conn) {
            Status::Pending => Ok(()),
            Status::Ready(bringup) => self.become_ready(bringup, ctx),
            Status::Failed(error) => {
                self.phase = WmPhase::Failed;
                Err(WmFailure { reason: error.to_string() })
            }
        }
    }

    fn become_ready(
        &mut self,
        bringup: crate::x11::Bringup,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let root = self.conn.root().expect("a ready connection has a root");
        let mut atom_cache = AtomCache::default();
        atom_cache.seed(&bringup.atoms);
        let mut ready = Ready {
            atoms: bringup.atoms,
            extensions: bringup.extensions,
            root,
            check_window: Window(0),
            cursor: None,
            root_cursor: None,
            hints_written: false,
            hints_fetch: 0,
            selections: selection::Selections::pending(&bringup.atoms),
            atom_cache,
        };
        if let Err(error) = setup::claim(&mut self.conn, &mut ready) {
            self.phase = WmPhase::Failed;
            return Err(WmFailure { reason: error.to_string() });
        }
        log::info!("XWM ready on :{}", self.display);
        self.phase = WmPhase::Ready(ready);
        self.on_selection_changed(selection::SelectionKind::Clipboard, ctx)?;
        self.on_selection_changed(selection::SelectionKind::Primary, ctx)?;
        self.refresh_hints(ctx)
    }
}
