//! CLIPBOARD and PRIMARY windows for one Xwayland run.
//!
//! Each selection has its own window. An owner notify for that window records
//! the selection timestamp. A foreign owner is offered to Wayland after its
//! `TARGETS` reply, and only while an X window has keyboard focus.

use crate::{
    runner::Redispatcher,
    seat::install_x_selection,
    state::{ObjectId, PrimarySelectionSourceState, SelectionSourceState, XSelectionSource},
    x11::{
        change_property_atoms, change_property_u32s, event_bytes, get_property_deleting, Atom,
        AtomInternEvent, Atoms, ConvertSelection, EventMask, GetAtomName, GetAtomNameReply,
        InternAtomReply, Property, PropertyError, SelectionClear, SelectionNotify,
        SelectionRequest, SendEvent, SetSelectionOwner, Window, XFixesSelectionNotify, XReplyError,
    },
    xwayland::{XSelectionSourceObject, Xwayland, XwaylandAction},
};

use super::{
    mime::{target_names_for_mimes, x_mime_types, TargetName},
    Wm, WmCtx, WmEvent, WmFailure, WmPhase,
};

/// Which selection a window and its transfers belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SelectionKind {
    Clipboard,
    Primary,
}

/// One selection the WM tracks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SelectionSide {
    pub(super) kind: SelectionKind,
    pub(super) atom: Atom,
    pub(super) window: Window,
    pub(super) owned: bool,
    pub(super) own_timestamp: u32,
    /// The compositor-owned Wayland source currently published for this selection.
    source: Option<ObjectId>,
    /// Bumped whenever a `TARGETS` name batch is discarded.
    names_generation: u64,
    names: Option<NameBatch>,
    /// Wayland offer currently represented by this selection on X.
    pub(super) offer: Option<OutgoingOffer>,
    /// Requests that arrived while a target atom was still being interned.
    pending: Vec<SelectionRequest>,
}

/// Atom names still being resolved for one `TARGETS` reply.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NameBatch {
    generation: u64,
    atoms: Vec<Atom>,
    outstanding: u32,
}

/// The clipboard and the primary selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Selections {
    pub(super) clipboard: SelectionSide,
    pub(super) primary: SelectionSide,
}

impl Selections {
    /// Placeholder sides. [`crate::xwayland::xwm::setup::claim`] fills the windows.
    pub(super) fn pending(atoms: &Atoms) -> Self {
        Self {
            clipboard: SelectionSide::new(SelectionKind::Clipboard, atoms.CLIPBOARD, Window(0)),
            primary: SelectionSide::new(SelectionKind::Primary, atoms.PRIMARY, Window(0)),
        }
    }

    pub(super) fn contains(&self, window: Window) -> bool {
        window == self.clipboard.window || window == self.primary.window
    }
}

impl SelectionSide {
    pub(super) fn new(kind: SelectionKind, atom: Atom, window: Window) -> Self {
        Self {
            kind,
            atom,
            window,
            owned: false,
            own_timestamp: 0,
            source: None,
            names_generation: 0,
            names: None,
            offer: None,
            pending: Vec::new(),
        }
    }
}

enum RequestDecision {
    Queued,
    Refuse,
    Atoms { property: Atom, values: Vec<Atom> },
    Timestamp { property: Atom, value: u32 },
    Data { property: Atom, mime: String, target_type: Atom },
}

/// Targets of the Wayland source that owns this X selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OutgoingOffer {
    pub(super) targets: Vec<(Atom, String)>,
    pub(super) interns_outstanding: u32,
}

impl Wm {
    pub(super) fn selection_window(&self, window: Window) -> bool {
        match &self.phase {
            WmPhase::Ready(ready) => ready.selections.contains(window),
            _ => false,
        }
    }

    pub(super) fn on_selection_owner(
        &mut self,
        event: XFixesSelectionNotify,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let foreign = {
            let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
            let Some(side) = side_mut(&mut ready.selections, event.selection) else {
                log::debug!("X selection notify for atom {}", event.selection.0);
                return Ok(());
            };
            if event.owner == side.window {
                side.own_timestamp = event.selection_timestamp;
                return Ok(());
            }
            side.names = None;
            side.names_generation = side.names_generation.wrapping_add(1);
            Some(ForeignOwner {
                kind: side.kind,
                window: side.window,
                selection: side.atom,
                targets: ready.atoms.TARGETS,
                property: ready.atoms._WL_SELECTION,
                source: side.source.take(),
                owner: event.owner,
                time: event.timestamp,
            })
        };
        let Some(foreign) = foreign else { return Ok(()) };
        self.drop_x_source(foreign.kind, foreign.source, ctx);
        self.clear_incoming(foreign.kind, ctx);
        if foreign.owner == Window::None {
            return Ok(());
        }
        match self.conn.send_void(&ConvertSelection {
            requestor: foreign.window,
            selection: foreign.selection,
            target: foreign.targets,
            property: foreign.property,
            time: foreign.time,
        }) {
            Ok(_) => Ok(()),
            Err(error) => self.failed(error),
        }
    }

    pub(super) fn on_selection_notify(
        &mut self,
        event: SelectionNotify,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let targets = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            let Some(side) = side_for_window(&ready.selections, event.requestor) else {
                return Ok(());
            };
            if event.target != ready.atoms.TARGETS {
                None
            } else if event.property.0 == 0 {
                return Ok(());
            } else {
                Some((side.kind, side.window, ready.atoms._WL_SELECTION))
            }
        };
        let Some((kind, window, property)) = targets else {
            return self.on_incoming_notify(event, ctx);
        };
        let run = ctx.run;
        let redispatch = Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
            run,
            event: WmEvent::SelectionTargets { kind, result },
        });
        match get_property_deleting(&mut self.conn, window, property, redispatch) {
            Ok(_) => Ok(()),
            Err(error) => self.failed(error),
        }
    }

    pub(super) fn on_selection_targets(
        &mut self,
        kind: SelectionKind,
        result: Result<Property, PropertyError>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let Ok(property) = result else {
            log::debug!("X selection TARGETS read failed");
            return Ok(());
        };
        if property.r#type != Atom::ATOM || property.format != 32 {
            log::debug!("X selection TARGETS reply is not an atom list");
            return Ok(());
        }
        let Ok(listed) = property.as_atoms() else {
            log::debug!("X selection TARGETS atom list is truncated");
            return Ok(());
        };
        let (generation, missing) = {
            let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
            let mut missing = Vec::new();
            for atom in &listed {
                if ready.atom_cache.name(*atom).is_none() && !missing.contains(atom) {
                    missing.push(*atom);
                }
            }
            let side = side_mut_kind(&mut ready.selections, kind).expect("selection");
            side.names_generation = side.names_generation.wrapping_add(1);
            let generation = side.names_generation;
            side.names = Some(NameBatch {
                generation,
                atoms: listed,
                outstanding: u32::try_from(missing.len()).unwrap_or(u32::MAX),
            });
            (generation, missing)
        };
        if missing.is_empty() {
            return self.publish_ready(kind, ctx);
        }
        for atom in missing {
            self.request_atom_name(kind, generation, atom, ctx)?;
        }
        Ok(())
    }

    pub(super) fn on_atom_name(
        &mut self,
        kind: SelectionKind,
        generation: u64,
        atom: Atom,
        result: Result<GetAtomNameReply, XReplyError>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let (done, name) = {
            let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
            let Some(side) = side_mut_kind(&mut ready.selections, kind) else {
                return Ok(());
            };
            let Some(batch) = side.names.as_mut() else { return Ok(()) };
            if batch.generation != generation || batch.outstanding == 0 {
                return Ok(());
            }
            batch.outstanding -= 1;
            (batch.outstanding == 0, result.ok().map(|reply| reply.name))
        };
        if let Some(name) = name {
            if let WmPhase::Ready(ready) = &mut self.phase {
                ready.atom_cache.insert(&name, atom);
            }
        }
        if done {
            self.publish_ready(kind, ctx)
        } else {
            Ok(())
        }
    }

    pub(super) fn on_selection_changed(
        &mut self,
        kind: SelectionKind,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        // The seat slot already holds the source this bridge published. Taking
        // the X selection here would echo the same offer back to Wayland.
        if self.slot_is_x_source(kind, ctx) {
            return Ok(());
        }
        let Some(mimes) = self.wayland_mimes(kind, ctx) else {
            return self.release_selection(kind, ctx);
        };
        self.own_selection(kind, mimes, ctx)
    }

    pub(super) fn on_selection_clear(
        &mut self,
        event: SelectionClear,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let (selection, pending) = {
            let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
            let Some(side) = side_mut(&mut ready.selections, event.selection) else {
                return Ok(());
            };
            if event.owner != side.window {
                return Ok(());
            }
            side.owned = false;
            side.offer = None;
            (side.atom, std::mem::take(&mut side.pending))
        };
        for request in pending {
            self.refuse_selection_request(request)?;
        }
        self.cancel_outgoing_selection(selection, ctx)
    }

    pub(super) fn on_interned(
        &mut self,
        name: Vec<u8>,
        result: Result<InternAtomReply, XReplyError>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let atom = match result {
            Ok(reply) => {
                if let WmPhase::Ready(ready) = &mut self.phase {
                    ready.atom_cache.insert(&name, reply.atom);
                }
                Some(reply.atom)
            }
            Err(error) => {
                log::debug!("X selection intern failed: {error}");
                None
            }
        };
        let finished = {
            let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
            let mut finished = None;
            for side in [&mut ready.selections.clipboard, &mut ready.selections.primary] {
                let Some(offer) = side.offer.as_mut() else { continue };
                if offer.interns_outstanding == 0 {
                    continue;
                }
                let Some(index) = offer
                    .targets
                    .iter()
                    .position(|(target, mime)| target.0 == 0 && mime.as_bytes() == name)
                else {
                    continue;
                };
                match atom {
                    Some(atom) => offer.targets[index].0 = atom,
                    None => {
                        offer.targets.remove(index);
                    }
                }
                offer.interns_outstanding -= 1;
                if offer.interns_outstanding == 0 {
                    finished = Some(side.kind);
                }
                break;
            }
            finished
        };
        if let Some(kind) = finished {
            self.answer_pending(kind, ctx)?;
        }
        Ok(())
    }

    fn wayland_mimes(&self, kind: SelectionKind, ctx: &WmCtx<'_>) -> Option<Vec<String>> {
        match kind {
            SelectionKind::Clipboard => ctx
                .state
                .seat
                .selection_source()
                .and_then(|source| source.mime_types(ctx.state)),
            SelectionKind::Primary => ctx
                .state
                .seat
                .primary_selection_source()
                .and_then(|source| source.mime_types(ctx.state)),
        }
    }

    fn release_selection(
        &mut self,
        kind: SelectionKind,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let (selection, pending) = {
            let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
            let Some(side) = side_mut_kind(&mut ready.selections, kind) else {
                return Ok(());
            };
            if !side.owned {
                return Ok(());
            }
            side.owned = false;
            side.offer = None;
            (side.atom, std::mem::take(&mut side.pending))
        };
        for request in pending {
            self.refuse_selection_request(request)?;
        }
        self.cancel_outgoing_selection(selection, ctx)?;
        match self
            .conn
            .send_void(&SetSelectionOwner { owner: Window(0), selection, time: 0 })
        {
            Ok(_) => Ok(()),
            Err(error) => self.failed(error),
        }
    }

    fn own_selection(
        &mut self,
        kind: SelectionKind,
        mimes: Vec<String>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let (owner, selection, targets, missing) = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            let Some(side) = side_for_kind(&ready.selections, kind) else { return Ok(()) };
            let mut targets = Vec::new();
            let mut missing = Vec::new();
            for name in target_names_for_mimes(&mimes) {
                match name {
                    TargetName::Utf8 => {
                        targets.push((ready.atoms.UTF8_STRING, utf8_mime()));
                    }
                    TargetName::Text => targets.push((ready.atoms.TEXT, utf8_mime())),
                    TargetName::Latin1 => {
                        targets.push((ready.atoms.STRING, "text/plain".to_string()))
                    }
                    TargetName::Named(bytes) => {
                        let Some(mime) = String::from_utf8(bytes.clone()).ok() else {
                            continue;
                        };
                        match ready.atom_cache.atom(&bytes) {
                            Some(atom) => targets.push((atom, mime)),
                            None => {
                                targets.push((Atom(0), mime));
                                missing.push(bytes);
                            }
                        }
                    }
                }
            }
            (side.window, side.atom, targets, missing)
        };
        let outstanding = self.intern_missing(&missing, ctx)?;
        if let WmPhase::Ready(ready) = &mut self.phase {
            if let Some(side) = side_mut_kind(&mut ready.selections, kind) {
                side.offer = Some(OutgoingOffer { targets, interns_outstanding: outstanding });
                side.owned = true;
            }
        }
        match self
            .conn
            .send_void(&SetSelectionOwner { owner, selection, time: 0 })
        {
            Ok(_) => {}
            Err(error) => return self.failed(error),
        }
        let ready = match &self.phase {
            WmPhase::Ready(ready) => side_for_kind(&ready.selections, kind)
                .and_then(|side| side.offer.as_ref())
                .is_some_and(|offer| offer.interns_outstanding == 0),
            _ => false,
        };
        if ready {
            self.answer_pending(kind, ctx)?;
        }
        Ok(())
    }

    fn intern_missing(&mut self, names: &[Vec<u8>], ctx: &WmCtx<'_>) -> Result<u32, WmFailure> {
        if names.is_empty() {
            return Ok(0);
        }
        let run = ctx.run;
        let redispatch =
            Redispatcher::to::<Xwayland>(move |event: AtomInternEvent| XwaylandAction::Wm {
                run,
                event: WmEvent::Interned { name: event.name, result: event.result },
            });
        let borrowed: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
        let sent = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(0) };
            ready
                .atom_cache
                .intern_more(&borrowed, &mut self.conn, redispatch)
        };
        match sent {
            Ok(sent) => Ok(u32::try_from(sent).unwrap_or(u32::MAX)),
            Err(error) => self.failed(error).map(|()| 0),
        }
    }

    fn publish_ready(&mut self, kind: SelectionKind, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let listed = {
            let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
            let Some(side) = side_mut_kind(&mut ready.selections, kind) else {
                return Ok(());
            };
            let Some(batch) = side.names.take() else { return Ok(()) };
            if batch.outstanding != 0 {
                side.names = Some(batch);
                return Ok(());
            }
            batch.atoms
        };
        let mime_types = {
            let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
            let pairs: Vec<_> = listed
                .iter()
                .map(|atom| (*atom, ready.atom_cache.name(*atom).map(|name| name.to_vec())))
                .collect();
            x_mime_types(&pairs, &ready.atoms)
        };
        self.publish_x_offer(kind, mime_types, ctx)
    }

    fn publish_x_offer(
        &mut self,
        kind: SelectionKind,
        mime_types: Vec<String>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        if mime_types.is_empty() || !x_keyboard_focus(ctx.state) {
            return Ok(());
        }
        let Some(client) = ctx.client else { return Ok(()) };
        let (_, object) = ctx
            .state
            .insert_object(XSelectionSourceObject::new(ctx.run, kind, mime_types));
        if let WmPhase::Ready(ready) = &mut self.phase {
            if let Some(side) = side_mut_kind(&mut ready.selections, kind) {
                side.source = Some(object);
            }
        }
        install_x_selection(
            ctx.state,
            ctx.dispatcher,
            kind,
            Some(XSelectionSource { client, object }),
        );
        Ok(())
    }

    fn request_atom_name(
        &mut self,
        kind: SelectionKind,
        generation: u64,
        atom: Atom,
        ctx: &WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let run = ctx.run;
        let redispatch = Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
            run,
            event: WmEvent::AtomName { kind, generation, atom, result },
        });
        match self.conn.send(&GetAtomName { atom }, redispatch) {
            Ok(_) => Ok(()),
            Err(error) => self.failed(error),
        }
    }

    pub(super) fn clear_published_sources(
        &mut self,
        state: &mut crate::State,
        dispatcher: &mut crate::runner::Dispatcher,
    ) {
        let sources = match &mut self.phase {
            WmPhase::Ready(ready) => [
                (SelectionKind::Clipboard, ready.selections.clipboard.source.take()),
                (SelectionKind::Primary, ready.selections.primary.source.take()),
            ],
            _ => [(SelectionKind::Clipboard, None), (SelectionKind::Primary, None)],
        };
        let surfaces = crate::xwayland::surfaces::SurfaceSerials::default();
        let mut ctx = WmCtx { run: 0, state, dispatcher, surfaces: &surfaces, client: None };
        for (kind, source) in sources {
            self.drop_x_source(kind, source, &mut ctx);
        }
        for kind in [SelectionKind::Clipboard, SelectionKind::Primary] {
            let Some(object) = published_x_object(kind, &ctx) else { continue };
            ctx.state.remove_object(object);
            install_x_selection(ctx.state, ctx.dispatcher, kind, None);
        }
    }

    fn drop_x_source(
        &mut self,
        kind: SelectionKind,
        source: Option<ObjectId>,
        ctx: &mut WmCtx<'_>,
    ) {
        let Some(object) = source else { return };
        ctx.state.remove_object(object);
        if self.slot_holds(kind, object, ctx) {
            install_x_selection(ctx.state, ctx.dispatcher, kind, None);
        }
    }

    fn slot_holds(&self, kind: SelectionKind, object: ObjectId, ctx: &WmCtx<'_>) -> bool {
        match kind {
            SelectionKind::Clipboard => ctx.state.seat.selection_source().is_some_and(|source| {
                matches!(source, SelectionSourceState::Xwayland(source) if source.object == object)
            }),
            SelectionKind::Primary => {
                ctx.state.seat.primary_selection_source().is_some_and(|source| {
                    matches!(
                        source,
                        PrimarySelectionSourceState::Xwayland(source) if source.object == object
                    )
                })
            }
        }
    }

    fn slot_is_x_source(&self, kind: SelectionKind, ctx: &WmCtx<'_>) -> bool {
        match kind {
            SelectionKind::Clipboard => {
                matches!(ctx.state.seat.selection_source(), Some(SelectionSourceState::Xwayland(_)))
            }
            SelectionKind::Primary => matches!(
                ctx.state.seat.primary_selection_source(),
                Some(PrimarySelectionSourceState::Xwayland(_))
            ),
        }
    }

    pub(super) fn on_selection_request(
        &mut self,
        event: SelectionRequest,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let Some(kind) = self.kind_for_selection(event.selection) else {
            log::warn!("Unknown selection request");
            return Ok(());
        };
        if !x_keyboard_focus(ctx.state) {
            return self.refuse_selection_request(event);
        }
        let decision = {
            let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
            let Some(side) = side_mut_kind(&mut ready.selections, kind) else {
                return Ok(());
            };
            if side.offer.is_none() {
                RequestDecision::Refuse
            } else if side
                .offer
                .as_ref()
                .is_some_and(|offer| offer.interns_outstanding > 0)
            {
                side.pending.push(event);
                RequestDecision::Queued
            } else {
                let property = if event.property.0 == 0 { event.target } else { event.property };
                let timestamp = side.own_timestamp;
                let targets = ready.atoms.TARGETS;
                let timestamp_atom = ready.atoms.TIMESTAMP;
                let multiple = ready.atoms.MULTIPLE;
                let text = ready.atoms.TEXT;
                let utf8 = ready.atoms.UTF8_STRING;
                let offer = side.offer.as_ref().unwrap();
                if event.target == targets {
                    let mut values = vec![targets, timestamp_atom];
                    values.extend(
                        offer
                            .targets
                            .iter()
                            .map(|(atom, _)| *atom)
                            .filter(|atom| atom.0 != 0),
                    );
                    RequestDecision::Atoms { property, values }
                } else if event.target == timestamp_atom {
                    RequestDecision::Timestamp { property, value: timestamp }
                } else if event.target == multiple {
                    RequestDecision::Refuse
                } else if let Some(mime) = offer
                    .targets
                    .iter()
                    .find(|(atom, _)| *atom == event.target)
                    .map(|(_, mime)| mime.clone())
                {
                    let target_type = if event.target == text { utf8 } else { event.target };
                    RequestDecision::Data { property, mime, target_type }
                } else {
                    RequestDecision::Refuse
                }
            }
        };
        match decision {
            RequestDecision::Queued => Ok(()),
            RequestDecision::Refuse => self.refuse_selection_request(event),
            RequestDecision::Atoms { property, values } => {
                self.reply_atoms(event, property, &values)
            }
            RequestDecision::Timestamp { property, value } => {
                self.reply_timestamp(event, property, value)
            }
            RequestDecision::Data { property, mime, target_type } => {
                self.begin_outgoing(kind, event, property, target_type, mime, ctx)
            }
        }
    }

    fn answer_pending(
        &mut self,
        kind: SelectionKind,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let pending = {
            let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
            let Some(side) = side_mut_kind(&mut ready.selections, kind) else {
                return Ok(());
            };
            std::mem::take(&mut side.pending)
        };
        for request in pending {
            self.on_selection_request(request, ctx)?;
        }
        Ok(())
    }

    pub(super) fn refuse_selection_request(
        &mut self,
        event: SelectionRequest,
    ) -> Result<(), WmFailure> {
        self.send_selection_notify(
            event.time,
            event.requestor,
            event.selection,
            event.target,
            Atom(0),
        )
    }

    pub(super) fn send_selection_notify(
        &mut self,
        time: u32,
        requestor: Window,
        selection: Atom,
        target: Atom,
        property: Atom,
    ) -> Result<(), WmFailure> {
        let message = SelectionNotify {
            send_event: true,
            sequence: 0,
            time,
            requestor,
            selection,
            target,
            property,
        };
        let event = event_bytes(|writer| message.serialize(writer));
        self.send_request(&SendEvent {
            propagate: 0,
            destination: requestor,
            event_mask: EventMask::NoEvent,
            event: &event,
        })
    }

    fn reply_atoms(
        &mut self,
        event: SelectionRequest,
        property: Atom,
        values: &[Atom],
    ) -> Result<(), WmFailure> {
        let change = change_property_atoms(event.requestor, property, Atom::ATOM, values);
        self.send_request(&change.request())?;
        self.send_selection_notify(
            event.time,
            event.requestor,
            event.selection,
            event.target,
            property,
        )
    }

    fn reply_timestamp(
        &mut self,
        event: SelectionRequest,
        property: Atom,
        value: u32,
    ) -> Result<(), WmFailure> {
        let change = change_property_u32s(event.requestor, property, Atom::INTEGER, &[value]);
        self.send_request(&change.request())?;
        self.send_selection_notify(
            event.time,
            event.requestor,
            event.selection,
            event.target,
            property,
        )
    }

    fn kind_for_selection(&self, selection: Atom) -> Option<SelectionKind> {
        let WmPhase::Ready(ready) = &self.phase else { return None };
        if selection == ready.selections.clipboard.atom {
            Some(SelectionKind::Clipboard)
        } else if selection == ready.selections.primary.atom {
            Some(SelectionKind::Primary)
        } else {
            None
        }
    }

    pub(super) fn failed(&mut self, error: impl ToString) -> Result<(), WmFailure> {
        self.phase = WmPhase::Failed;
        Err(WmFailure { reason: error.to_string() })
    }
}

struct ForeignOwner {
    kind: SelectionKind,
    window: Window,
    selection: Atom,
    targets: Atom,
    property: Atom,
    source: Option<ObjectId>,
    owner: Window,
    time: u32,
}

fn side_mut(selections: &mut Selections, atom: Atom) -> Option<&mut SelectionSide> {
    if atom == selections.clipboard.atom {
        Some(&mut selections.clipboard)
    } else if atom == selections.primary.atom {
        Some(&mut selections.primary)
    } else {
        None
    }
}

fn side_mut_kind(selections: &mut Selections, kind: SelectionKind) -> Option<&mut SelectionSide> {
    match kind {
        SelectionKind::Clipboard => Some(&mut selections.clipboard),
        SelectionKind::Primary => Some(&mut selections.primary),
    }
}

fn side_for_kind(selections: &Selections, kind: SelectionKind) -> Option<&SelectionSide> {
    match kind {
        SelectionKind::Clipboard => Some(&selections.clipboard),
        SelectionKind::Primary => Some(&selections.primary),
    }
}

fn utf8_mime() -> String {
    "text/plain;charset=utf-8".to_string()
}

fn side_for_window(selections: &Selections, window: Window) -> Option<&SelectionSide> {
    if window == selections.clipboard.window {
        Some(&selections.clipboard)
    } else if window == selections.primary.window {
        Some(&selections.primary)
    } else {
        None
    }
}

fn published_x_object(kind: SelectionKind, ctx: &WmCtx<'_>) -> Option<ObjectId> {
    match kind {
        SelectionKind::Clipboard => match ctx.state.seat.selection_source() {
            Some(SelectionSourceState::Xwayland(source)) => Some(source.object),
            _ => None,
        },
        SelectionKind::Primary => match ctx.state.seat.primary_selection_source() {
            Some(PrimarySelectionSourceState::Xwayland(source)) => Some(source.object),
            _ => None,
        },
    }
}

fn x_keyboard_focus(state: &crate::State) -> bool {
    let Some(focus) = state.seat.keyboard_focus() else {
        return false;
    };
    let root = state
        .root_surface_for_surface(focus.surface)
        .unwrap_or(focus.surface);
    root.try_ref(state)
        .is_some_and(|surface| surface.has_xwayland_role())
}
