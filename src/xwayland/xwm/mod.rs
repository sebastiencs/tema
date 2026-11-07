//! The X window manager for one Xwayland run.
//!
//! It owns the `-wm` connection. Displayfd readiness starts it. A dead connection
//! or a failed startup ends the run.

mod cursor;
mod props;
mod setup;
mod tests;
mod window;

use std::{
    collections::BTreeMap,
    fmt::{self, Display, Formatter},
    os::fd::RawFd,
};

use crate::{
    io_uring::ring::{ReadWriteCompleted, RingId},
    runner::{Dispatcher, Redispatcher},
    x11::{
        Atoms, Extensions, Property, PropertyError, RenderQueryPictFormatsReply, Startup,
        StartupEvent, State as ConnectionState, Status, Window, XConnection, XIo, XReplyError,
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
    Read { generation: u64, read: RingId, result: Completion },
    Written { generation: u64, token: u64, result: Completion },
    Startup(StartupEvent),
    PictFormats(Result<RenderQueryPictFormatsReply, XReplyError>),
    Property { window: Window, atom: crate::x11::Atom, result: Result<Property, PropertyError> },
}

pub(super) struct WmCtx<'a> {
    pub(super) run: u64,
    pub(super) state: &'a mut State,
    pub(super) dispatcher: &'a mut Dispatcher,
    pub(super) surfaces: &'a SurfaceSerials,
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
                WmEvent::Property { window, atom, result } => {
                    self.on_property_reply(window, atom, result)
                }
            }
        };
        self.io.flush(&mut self.conn, ctx.dispatcher);
        outcome
    }

    /// Cancels the armed read. The caller drops the `-wm` fd.
    pub(super) fn stop(&mut self, dispatcher: &mut Dispatcher) {
        self.io.stop(dispatcher);
        self.phase = WmPhase::Failed;
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
        let mut ready = Ready {
            atoms: bringup.atoms,
            extensions: bringup.extensions,
            root,
            check_window: Window(0),
            cursor: None,
        };
        if let Err(error) = setup::claim(&mut self.conn, &mut ready) {
            self.phase = WmPhase::Failed;
            return Err(WmFailure { reason: error.to_string() });
        }
        log::info!("XWM ready on :{}", self.display);
        self.phase = WmPhase::Ready(ready);
        self.begin_cursor(ctx)
    }
}
