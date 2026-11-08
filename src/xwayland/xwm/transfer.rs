//! Selection bytes in both directions.
//!
//! One conversion runs per selection from X to Wayland. Further receives wait.
//! An `INCR` property is read one chunk at a time, and the next chunk is
//! fetched only after the pipe write completes. Wayland to X reads the source
//! pipe and writes one property when the value fits. A value that reaches the
//! chunk size before end of file uses `INCR`.

use std::{
    os::fd::{AsRawFd, OwnedFd},
    rc::Rc,
    time::Instant,
};

use crate::{
    io_uring::{model::IoUringAction, ring::RingId},
    process::pipe_cloexec,
    runner::Redispatcher,
    state::{ObjectId, PrimarySelectionSourceState, SelectionSourceState},
    x11::{
        change_property_u32s, change_property_u8s, get_property_deleting, Atom,
        ChangeWindowAttributes, ChangeWindowAttributesAux, ConvertSelection, EventMask, Property,
        PropertyError, PropertyNotify, PropertyState, SelectionRequest,
    },
    xwayland::{XSelectionSourceObject, Xwayland, XwaylandAction},
};

use super::{selection::SelectionKind, Wm, WmCtx, WmEvent, WmFailure, WmPhase};

/// One Wayland `receive` waiting on an X selection.
pub(super) struct Incoming {
    id: u64,
    kind: SelectionKind,
    target: Atom,
    fd: Rc<OwnedFd>,
    state: IncomingState,
    /// Set once the owner answers with `INCR`. Later reads are chunks.
    incremental: bool,
    write: Option<RingId>,
    new_value_pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IncomingState {
    Converting,
    Reading,
    Incremental,
    Writing,
}

impl Wm {
    pub(super) fn on_selection_send(
        &mut self,
        source: ObjectId,
        mime_type: String,
        fd: Rc<OwnedFd>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let kind = ctx.state.objects.try_get(source).and_then(|object| {
            object
                .downcast_ref::<XSelectionSourceObject>()
                .and_then(|source| {
                    source
                        .mime_types()
                        .iter()
                        .any(|mime| mime == &mime_type)
                        .then(|| source.kind())
                })
        });
        let Some(kind) = kind else {
            drop(fd);
            return Ok(());
        };
        let Some(target) = self.target_for_mime(&mime_type) else {
            drop(fd);
            return Ok(());
        };
        self.enqueue(kind, target, fd, ctx)
    }

    pub(super) fn on_incoming_notify(
        &mut self,
        event: crate::x11::SelectionNotify,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let kind = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            let Some(kind) = selection_kind(&ready.selections, event.requestor) else {
                return Ok(());
            };
            let Some(front) = self.incoming.get(&kind).and_then(|queue| queue.front()) else {
                return Ok(());
            };
            if front.target != event.target || front.state != IncomingState::Converting {
                return Ok(());
            }
            kind
        };
        if event.property.0 == 0 {
            return self.finish_front(kind, ctx);
        }
        self.fetch_property(kind, ctx)
    }

    pub(super) fn on_incoming_property(
        &mut self,
        kind: SelectionKind,
        result: Result<Property, PropertyError>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let reading = self
            .incoming
            .get(&kind)
            .and_then(|queue| queue.front())
            .is_some_and(|front| front.state == IncomingState::Reading);
        if !reading {
            return Ok(());
        }
        let property = match result {
            Ok(property) => property,
            Err(error) => {
                log::debug!("X selection read failed: {error}");
                return self.finish_front(kind, ctx);
            }
        };
        let (incremental, incr) = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            let incremental = self
                .incoming
                .get(&kind)
                .and_then(|queue| queue.front())
                .is_some_and(|front| front.incremental);
            (incremental, ready.atoms.INCR)
        };
        if !incremental && property.r#type == incr {
            let id = {
                let front = self.front_mut(kind).expect("front");
                front.incremental = true;
                front.state = IncomingState::Incremental;
                front.id
            };
            self.timer.touch(id, Instant::now(), ctx.dispatcher);
            return Ok(());
        }
        if incremental && property.data.is_empty() {
            return self.finish_front(kind, ctx);
        }
        self.write_chunk(kind, property.data, ctx);
        Ok(())
    }

    pub(super) fn on_incoming_written(
        &mut self,
        id: u64,
        result: std::io::Result<crate::io_uring::ring::ReadWriteCompleted>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let Some(kind) = self.kind_of_front(id) else { return Ok(()) };
        let incremental = {
            let front = self.front_mut(kind).expect("front");
            front.write = None;
            front.incremental
        };
        if result.is_err() || !incremental {
            if let Err(error) = &result {
                log::debug!("X selection write failed: {error}");
            }
            return self.finish_front(kind, ctx);
        }
        let pending = {
            let front = self.front_mut(kind).expect("front");
            front.state = IncomingState::Incremental;
            let pending = front.new_value_pending;
            front.new_value_pending = false;
            pending
        };
        if pending {
            self.fetch_property(kind, ctx)
        } else {
            self.timer.touch(id, Instant::now(), ctx.dispatcher);
            Ok(())
        }
    }

    /// `PropertyNotify` on a selection window's `_WL_SELECTION`.
    ///
    /// Returns whether the event belonged to a selection window.
    pub(super) fn on_selection_property(
        &mut self,
        event: PropertyNotify,
        ctx: &mut WmCtx<'_>,
    ) -> Result<bool, WmFailure> {
        let kind = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(false) };
            let Some(kind) = selection_kind(&ready.selections, event.window) else {
                return Ok(false);
            };
            if event.atom != ready.atoms._WL_SELECTION {
                return Ok(false);
            }
            if event.state != crate::x11::PropertyState::NewValue {
                return Ok(true);
            }
            kind
        };
        let Some((incremental, busy)) = self.incoming.get(&kind).and_then(|queue| {
            queue.front().map(|front| {
                (front.incremental, front.write.is_some() || front.state == IncomingState::Reading)
            })
        }) else {
            return Ok(true);
        };
        if !incremental {
            return Ok(true);
        }
        if busy {
            self.front_mut(kind).expect("front").new_value_pending = true;
            return Ok(true);
        }
        self.fetch_property(kind, ctx)?;
        Ok(true)
    }

    pub(super) fn transfer_timed_out(
        &mut self,
        id: u64,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        if let Some(kind) = self.kind_of_front(id) {
            return self.finish_front(kind, ctx);
        }
        self.refuse_outgoing(id, ctx)
    }

    /// Cancel every pipe read and write and drop the fds.
    pub(super) fn cancel_transfers(&mut self, dispatcher: &mut crate::runner::Dispatcher) {
        let incoming = std::mem::take(&mut self.incoming);
        for (_, queue) in incoming {
            for entry in queue {
                if let Some(write) = entry.write {
                    dispatcher.dispatch(IoUringAction::Cancel { target: write });
                }
                self.timer.remove(entry.id, dispatcher);
            }
        }
        let outgoing = std::mem::take(&mut self.outgoing);
        for (id, outgoing) in outgoing {
            if let Some(read) = outgoing.read {
                dispatcher.dispatch(IoUringAction::Cancel { target: read });
            }
            self.timer.remove(id, dispatcher);
        }
    }

    pub(super) fn clear_incoming(&mut self, kind: SelectionKind, ctx: &mut WmCtx<'_>) {
        let Some(queue) = self.incoming.remove(&kind) else { return };
        for entry in queue {
            if let Some(write) = entry.write {
                ctx.dispatcher
                    .dispatch(IoUringAction::Cancel { target: write });
            }
            self.timer.remove(entry.id, ctx.dispatcher);
        }
    }

    fn enqueue(
        &mut self,
        kind: SelectionKind,
        target: Atom,
        fd: Rc<OwnedFd>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        self.next_transfer += 1;
        let incoming = Incoming {
            id: self.next_transfer,
            kind,
            target,
            fd,
            state: IncomingState::Converting,
            incremental: false,
            write: None,
            new_value_pending: false,
        };
        let idle = self
            .incoming
            .get(&kind)
            .map(|queue| queue.is_empty())
            .unwrap_or(true);
        self.incoming.entry(kind).or_default().push_back(incoming);
        if idle {
            self.start_front(kind, ctx)?;
        }
        Ok(())
    }

    fn start_front(&mut self, kind: SelectionKind, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let Some(front) = self.incoming.get(&kind).and_then(|queue| queue.front()) else {
            return Ok(());
        };
        if front.state != IncomingState::Converting {
            return Ok(());
        }
        let id = front.id;
        let target = front.target;
        let (window, selection, property) = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            let side = match kind {
                SelectionKind::Clipboard => &ready.selections.clipboard,
                SelectionKind::Primary => &ready.selections.primary,
            };
            (side.window, side.atom, ready.atoms._WL_SELECTION)
        };
        if let Err(error) = self.conn.send_void(&ConvertSelection {
            requestor: window,
            selection,
            target,
            property,
            time: 0,
        }) {
            return self.failed(error);
        }
        self.timer.touch(id, Instant::now(), ctx.dispatcher);
        Ok(())
    }

    fn fetch_property(
        &mut self,
        kind: SelectionKind,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let (window, property, id) = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            let Some(front) = self.incoming.get(&kind).and_then(|queue| queue.front()) else {
                return Ok(());
            };
            let side = match kind {
                SelectionKind::Clipboard => &ready.selections.clipboard,
                SelectionKind::Primary => &ready.selections.primary,
            };
            (side.window, ready.atoms._WL_SELECTION, front.id)
        };
        self.front_mut(kind).expect("front").state = IncomingState::Reading;
        let run = ctx.run;
        let redispatch = Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
            run,
            event: WmEvent::IncomingProperty { kind, result },
        });
        if let Err(error) = get_property_deleting(&mut self.conn, window, property, redispatch) {
            return self.failed(error);
        }
        self.timer.touch(id, Instant::now(), ctx.dispatcher);
        Ok(())
    }

    fn write_chunk(&mut self, kind: SelectionKind, data: Vec<u8>, ctx: &mut WmCtx<'_>) {
        let (id, fd, ring) = {
            let front = self.front_mut(kind).expect("front");
            let ring = RingId::fresh();
            front.write = Some(ring);
            front.state =
                if front.incremental { IncomingState::Incremental } else { IncomingState::Writing };
            (front.id, AsRawFd::as_raw_fd(&*front.fd), ring)
        };
        let run = ctx.run;
        ctx.dispatcher.dispatch(IoUringAction::Write {
            id: Some(ring),
            fd,
            offset: 0,
            buffer: data.into_boxed_slice(),
            dispatch_to: Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
                run,
                event: WmEvent::IncomingWritten { id, result },
            }),
        });
        self.timer.touch(id, Instant::now(), ctx.dispatcher);
    }

    fn finish_front(&mut self, kind: SelectionKind, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let Some(entry) = self
            .incoming
            .get_mut(&kind)
            .and_then(|queue| queue.pop_front())
        else {
            return Ok(());
        };
        if let Some(write) = entry.write {
            ctx.dispatcher
                .dispatch(IoUringAction::Cancel { target: write });
        }
        self.timer.remove(entry.id, ctx.dispatcher);
        drop(entry);
        if self
            .incoming
            .get(&kind)
            .is_some_and(|queue| queue.is_empty())
        {
            self.incoming.remove(&kind);
        }
        self.start_front(kind, ctx)
    }

    fn target_for_mime(&self, mime: &str) -> Option<Atom> {
        let WmPhase::Ready(ready) = &self.phase else { return None };
        if mime == "text/plain;charset=utf-8" {
            Some(ready.atoms.UTF8_STRING)
        } else if mime == "text/plain" {
            Some(ready.atoms.STRING)
        } else {
            ready.atom_cache.atom(mime.as_bytes())
        }
    }

    fn front_mut(&mut self, kind: SelectionKind) -> Option<&mut Incoming> {
        self.incoming
            .get_mut(&kind)
            .and_then(|queue| queue.front_mut())
    }

    fn kind_of_front(&self, id: u64) -> Option<SelectionKind> {
        self.incoming.values().find_map(|queue| {
            queue
                .front()
                .filter(|entry| entry.id == id)
                .map(|entry| entry.kind)
        })
    }

    /// Read a Wayland source into one X property for `event`.
    pub(super) fn begin_outgoing(
        &mut self,
        kind: SelectionKind,
        event: SelectionRequest,
        property: Atom,
        target_type: Atom,
        mime: String,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let (read_fd, write_fd) = match pipe_cloexec() {
            Ok(ends) => ends,
            Err(error) => {
                log::debug!("X selection pipe failed: {error}");
                return self.refuse_selection_request(event);
            }
        };
        let write_fd = Rc::new(write_fd);
        let sent = match kind {
            SelectionKind::Clipboard => match ctx.state.seat.selection_source() {
                Some(source) if !matches!(source, SelectionSourceState::Xwayland(_)) => {
                    source.send(mime, write_fd, ctx.dispatcher);
                    true
                }
                _ => false,
            },
            SelectionKind::Primary => match ctx.state.seat.primary_selection_source() {
                Some(source) if !matches!(source, PrimarySelectionSourceState::Xwayland(_)) => {
                    source.send(mime, write_fd, ctx.dispatcher);
                    true
                }
                _ => false,
            },
        };
        if !sent {
            drop(read_fd);
            return self.refuse_selection_request(event);
        }
        self.next_transfer += 1;
        let id = self.next_transfer;
        self.outgoing.insert(
            id,
            Outgoing {
                requestor: event.requestor,
                property,
                target_type,
                time: event.time,
                selection: event.selection,
                target: event.target,
                read_fd,
                buffer: Vec::new(),
                read: None,
                notified: false,
                phase: OutgoingPhase::Reading,
                eof: false,
                delete_pending: false,
            },
        );
        self.arm_outgoing_read(id, ctx);
        self.timer.touch(id, Instant::now(), ctx.dispatcher);
        Ok(())
    }

    pub(super) fn on_outgoing_read(
        &mut self,
        id: u64,
        result: std::io::Result<crate::io_uring::ring::ReadWriteCompleted>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        if !self.outgoing.contains_key(&id) {
            return Ok(());
        }
        match result {
            Ok(done) => {
                let phase = {
                    let Some(outgoing) = self.outgoing.get_mut(&id) else {
                        return Ok(());
                    };
                    outgoing.buffer.extend_from_slice(&done.buffer);
                    outgoing.read = None;
                    outgoing.phase
                };
                match phase {
                    OutgoingPhase::Reading => {
                        let limit = self.property_limit();
                        let full = self
                            .outgoing
                            .get(&id)
                            .is_some_and(|outgoing| outgoing.buffer.len() >= limit);
                        if full {
                            self.start_incr(id, ctx)
                        } else {
                            self.arm_outgoing_read(id, ctx);
                            self.timer.touch(id, Instant::now(), ctx.dispatcher);
                            Ok(())
                        }
                    }
                    OutgoingPhase::Incr => self.incr_read_finished(id, ctx),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                let phase = {
                    let Some(outgoing) = self.outgoing.get_mut(&id) else {
                        return Ok(());
                    };
                    outgoing.eof = true;
                    outgoing.read = None;
                    outgoing.phase
                };
                match phase {
                    OutgoingPhase::Reading => self.finish_outgoing(id, ctx),
                    OutgoingPhase::Incr => self.incr_read_finished(id, ctx),
                }
            }
            Err(error) => {
                log::debug!("X selection read failed: {error}");
                if let Some(outgoing) = self.outgoing.get_mut(&id) {
                    outgoing.read = None;
                }
                self.refuse_outgoing(id, ctx)
            }
        }
    }

    /// `Delete` on an incremental requestor's property writes the next chunk.
    pub(super) fn on_outgoing_delete(
        &mut self,
        event: PropertyNotify,
        ctx: &mut WmCtx<'_>,
    ) -> Result<bool, WmFailure> {
        if event.state != PropertyState::Delete {
            return Ok(false);
        }
        let Some(id) = self.outgoing.iter().find_map(|(id, outgoing)| {
            (outgoing.phase == OutgoingPhase::Incr
                && outgoing.requestor == event.window
                && outgoing.property == event.atom)
                .then_some(*id)
        }) else {
            return Ok(false);
        };
        self.write_incr_chunk(id, ctx)?;
        Ok(true)
    }

    /// The requestor window is gone. Drop its transfers without another notify.
    pub(super) fn drop_outgoing_requestor(
        &mut self,
        requestor: crate::x11::Window,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let ids: Vec<u64> = self
            .outgoing
            .iter()
            .filter(|(_, outgoing)| outgoing.requestor == requestor)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.drop_outgoing(id, ctx);
        }
        Ok(())
    }

    /// Drop every outgoing read for `selection` and refuse any that was not answered.
    pub(super) fn cancel_outgoing_selection(
        &mut self,
        selection: Atom,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let ids: Vec<u64> = self
            .outgoing
            .iter()
            .filter(|(_, outgoing)| outgoing.selection == selection)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.refuse_outgoing(id, ctx)?;
        }
        Ok(())
    }

    fn finish_outgoing(&mut self, id: u64, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let limit = self.property_limit();
        let ready = {
            let Some(outgoing) = self.outgoing.get_mut(&id) else {
                return Ok(());
            };
            outgoing.read = None;
            if outgoing.buffer.len() > limit {
                outgoing.eof = true;
                None
            } else {
                outgoing.notified = true;
                Some((
                    outgoing.requestor,
                    outgoing.property,
                    outgoing.target_type,
                    std::mem::take(&mut outgoing.buffer),
                    outgoing.time,
                    outgoing.selection,
                    outgoing.target,
                ))
            }
        };
        let Some((requestor, property, target_type, buffer, time, selection, target)) = ready
        else {
            return self.start_incr(id, ctx);
        };
        let change = change_property_u8s(requestor, property, target_type, &buffer);
        self.send_request(&change.request())?;
        self.send_selection_notify(time, requestor, selection, target, property)?;
        self.timer.remove(id, ctx.dispatcher);
        self.outgoing.remove(&id);
        Ok(())
    }

    fn refuse_outgoing(&mut self, id: u64, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let Some(outgoing) = self.outgoing.remove(&id) else {
            return Ok(());
        };
        if let Some(read) = outgoing.read {
            ctx.dispatcher
                .dispatch(IoUringAction::Cancel { target: read });
        }
        self.timer.remove(id, ctx.dispatcher);
        if outgoing.notified {
            return Ok(());
        }
        self.send_selection_notify(
            outgoing.time,
            outgoing.requestor,
            outgoing.selection,
            outgoing.target,
            Atom(0),
        )
    }

    fn arm_outgoing_read(&mut self, id: u64, ctx: &mut WmCtx<'_>) {
        let Some(outgoing) = self.outgoing.get_mut(&id) else {
            return;
        };
        let fd = outgoing.read_fd.as_raw_fd();
        let ring = RingId::fresh();
        outgoing.read = Some(ring);
        let run = ctx.run;
        ctx.dispatcher.dispatch(IoUringAction::ReadSome {
            id: Some(ring),
            fd,
            offset: 0,
            length: 65536,
            dispatch_to: Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
                run,
                event: WmEvent::OutgoingRead { id, result },
            }),
        });
    }

    fn property_limit(&self) -> usize {
        let units = self.conn.maximum_request_length().unwrap_or(65535) as usize;
        units.saturating_mul(4).saturating_sub(24).min(65536)
    }

    fn start_incr(&mut self, id: u64, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let chunk = u32::try_from(self.property_limit()).unwrap_or(u32::MAX);
        let (requestor, property, time, selection, target) = {
            let Some(outgoing) = self.outgoing.get_mut(&id) else {
                return Ok(());
            };
            if outgoing.phase == OutgoingPhase::Incr {
                return Ok(());
            }
            outgoing.phase = OutgoingPhase::Incr;
            outgoing.notified = true;
            (
                outgoing.requestor,
                outgoing.property,
                outgoing.time,
                outgoing.selection,
                outgoing.target,
            )
        };
        if !self.windows.contains_key(&requestor) {
            self.send_request(&ChangeWindowAttributes {
                window: requestor,
                value_list: ChangeWindowAttributesAux::default()
                    .event_mask(EventMask::PropertyChange),
            })?;
        }
        let incr = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            ready.atoms.INCR
        };
        let change = change_property_u32s(requestor, property, incr, &[chunk]);
        self.send_request(&change.request())?;
        self.send_selection_notify(time, requestor, selection, target, property)?;
        self.timer.touch(id, Instant::now(), ctx.dispatcher);
        Ok(())
    }

    fn incr_read_finished(&mut self, id: u64, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let pending = self
            .outgoing
            .get(&id)
            .is_some_and(|outgoing| outgoing.delete_pending);
        if pending {
            self.write_incr_chunk(id, ctx)
        } else {
            self.timer.touch(id, Instant::now(), ctx.dispatcher);
            Ok(())
        }
    }

    fn write_incr_chunk(&mut self, id: u64, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let limit = self.property_limit();
        let step = {
            let Some(outgoing) = self.outgoing.get_mut(&id) else {
                return Ok(());
            };
            outgoing.delete_pending = false;
            if outgoing.buffer.is_empty() && outgoing.eof {
                IncrStep::Zero {
                    requestor: outgoing.requestor,
                    property: outgoing.property,
                    target_type: outgoing.target_type,
                }
            } else if outgoing.buffer.is_empty() {
                outgoing.delete_pending = true;
                IncrStep::NeedRead
            } else {
                let count = limit.min(outgoing.buffer.len());
                let data: Vec<u8> = outgoing.buffer.drain(..count).collect();
                let need_read = !outgoing.eof && outgoing.buffer.len() < limit;
                IncrStep::Data {
                    requestor: outgoing.requestor,
                    property: outgoing.property,
                    target_type: outgoing.target_type,
                    data,
                    need_read,
                }
            }
        };
        match step {
            IncrStep::Zero { requestor, property, target_type } => {
                let change = change_property_u8s(requestor, property, target_type, &[]);
                self.send_request(&change.request())?;
                self.timer.remove(id, ctx.dispatcher);
                self.outgoing.remove(&id);
                Ok(())
            }
            IncrStep::Data { requestor, property, target_type, data, need_read } => {
                let change = change_property_u8s(requestor, property, target_type, &data);
                self.send_request(&change.request())?;
                if need_read
                    && self
                        .outgoing
                        .get(&id)
                        .is_some_and(|outgoing| outgoing.read.is_none())
                {
                    self.arm_outgoing_read(id, ctx);
                }
                self.timer.touch(id, Instant::now(), ctx.dispatcher);
                Ok(())
            }
            IncrStep::NeedRead => {
                if self
                    .outgoing
                    .get(&id)
                    .is_some_and(|outgoing| outgoing.read.is_none())
                {
                    self.arm_outgoing_read(id, ctx);
                }
                self.timer.touch(id, Instant::now(), ctx.dispatcher);
                Ok(())
            }
        }
    }

    fn drop_outgoing(&mut self, id: u64, ctx: &mut WmCtx<'_>) {
        let Some(outgoing) = self.outgoing.remove(&id) else {
            return;
        };
        if let Some(read) = outgoing.read {
            ctx.dispatcher
                .dispatch(IoUringAction::Cancel { target: read });
        }
        self.timer.remove(id, ctx.dispatcher);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutgoingPhase {
    Reading,
    Incr,
}

enum IncrStep {
    Zero {
        requestor: crate::x11::Window,
        property: Atom,
        target_type: Atom,
    },
    Data {
        requestor: crate::x11::Window,
        property: Atom,
        target_type: Atom,
        data: Vec<u8>,
        need_read: bool,
    },
    NeedRead,
}

/// One X `SelectionRequest` reading a Wayland source.
pub(super) struct Outgoing {
    requestor: crate::x11::Window,
    property: Atom,
    target_type: Atom,
    time: u32,
    selection: Atom,
    target: Atom,
    read_fd: OwnedFd,
    buffer: Vec<u8>,
    read: Option<RingId>,
    /// `SelectionNotify` has already been sent.
    notified: bool,
    phase: OutgoingPhase,
    eof: bool,
    /// The requestor deleted the property before the next chunk was buffered.
    delete_pending: bool,
}

fn selection_kind(
    selections: &super::selection::Selections,
    window: crate::x11::Window,
) -> Option<SelectionKind> {
    if window == selections.clipboard.window {
        Some(SelectionKind::Clipboard)
    } else if window == selections.primary.window {
        Some(SelectionKind::Primary)
    } else {
        None
    }
}
