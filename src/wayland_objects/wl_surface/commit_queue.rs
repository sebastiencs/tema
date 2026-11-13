//! The per-surface queue of captured commits. A captured update is applied at
//! once when it is ready and no earlier update of its surface waits; otherwise
//! it waits in the surface's queue, behind the updates before it.

use std::collections::{BTreeMap, BTreeSet};

use super::*;

/// Updates a client may have waiting across all of its surfaces (jay's limit).
/// The next one gets `wl_display.error(no_memory)` on its surface and the
/// client is disconnected.
pub(crate) const MAX_QUEUED_UPDATES_PER_CLIENT: usize = 256;

/// Which surfaces of each client have queued updates, and how many updates
/// each client has queued in total.
#[derive(Debug, Default)]
pub(crate) struct CommitQueues {
    surfaces: BTreeMap<ClientId, BTreeSet<Resolved<WlSurfaceId>>>,
    depth: BTreeMap<ClientId, usize>,
}

impl CommitQueues {
    pub(crate) fn depth(&self, client: ClientId) -> usize {
        self.depth.get(&client).copied().unwrap_or(0)
    }

    pub(crate) fn has_queued(&self, client: ClientId) -> bool {
        self.depth(client) > 0
    }

    pub(crate) fn queued_surfaces(&self, client: ClientId) -> Vec<Resolved<WlSurfaceId>> {
        self.surfaces
            .get(&client)
            .map(|surfaces| surfaces.iter().copied().collect())
            .unwrap_or_default()
    }

    pub(crate) fn note_enqueued(&mut self, surface: Resolved<WlSurfaceId>) {
        let client = surface.client_id();
        *self.depth.entry(client).or_insert(0) += 1;
        self.surfaces.entry(client).or_default().insert(surface);
    }

    /// One update of `surface` left its queue. `now_empty` when it was the last.
    pub(crate) fn note_dequeued(&mut self, surface: Resolved<WlSurfaceId>, now_empty: bool) {
        self.reduce_depth(surface.client_id(), 1);
        if now_empty {
            self.remove_surface(surface);
        }
    }

    /// `surface` has no queued updates left; `count` of them were dropped.
    pub(crate) fn forget_surface(&mut self, surface: Resolved<WlSurfaceId>, count: usize) {
        self.reduce_depth(surface.client_id(), count);
        self.remove_surface(surface);
    }

    pub(crate) fn remove_client(&mut self, client: ClientId) {
        self.surfaces.remove(&client);
        self.depth.remove(&client);
    }

    fn reduce_depth(&mut self, client: ClientId, count: usize) {
        let remaining = self.depth.get_mut(&client).map(|depth| {
            *depth = depth.saturating_sub(count);
            *depth
        });
        if remaining == Some(0) {
            self.depth.remove(&client);
        }
    }

    fn remove_surface(&mut self, surface: Resolved<WlSurfaceId>) {
        let client = surface.client_id();
        if let Some(surfaces) = self.surfaces.get_mut(&client) {
            surfaces.remove(&surface);
        }
        if self.surfaces.get(&client).is_some_and(BTreeSet::is_empty) {
            self.surfaces.remove(&client);
        }
    }
}

/// Inputs only the top-level update needs (fifo / commit timing).
#[derive(Clone, Copy, Debug)]
pub(super) struct TopLevel {
    #[allow(dead_code)] // read by FIFO_COMMIT_TIMING_PLAN
    pub(super) active_output: Option<OutputId>,
    #[allow(dead_code)] // read by FIFO_COMMIT_TIMING_PLAN
    pub(super) now: std::time::Instant,
    #[allow(dead_code)] // read by FIFO_COMMIT_TIMING_PLAN
    pub(super) output_count: usize,
}

#[derive(Debug)]
pub(super) enum Readiness {
    Ready,
    WaitSequence,
    #[allow(dead_code)] // returned by the acquire check of the explicit-sync phase
    WaitAcquire {
        acquire: crate::sync::AcquireSync,
    },
    #[cfg(test)]
    WaitTestHold,
}

/// Whether `update` of `surface` may be applied now. `applied_seq` is the
/// surface's `applied_update_seq`; `forced` skips the waits that can be skipped.
pub(super) fn readiness(
    state: &State,
    surface: Resolved<WlSurfaceId>,
    applied_seq: u64,
    update: &ContentUpdate,
    forced: bool,
    top: Option<TopLevel>,
) -> Readiness {
    if applied_seq.saturating_add(1) < update.first_seq {
        return Readiness::WaitSequence;
    }
    let forced = forced || update.acquire_forced;
    // Phase 10: acquire check here, skipped when `forced`.
    #[cfg(test)]
    {
        if update.test_hold {
            return Readiness::WaitTestHold;
        }
    }
    for child in &update.children {
        if !child.surface.exists(state) {
            continue; // destroyed: discarded at apply
        }
        if state.objects.is_grabbed(child.surface.object_id()) {
            return Readiness::WaitSequence; // grabbed: try again later
        }
        let Some(c) = child.surface.try_ref(state) else {
            continue;
        };
        if child.update.last_seq <= c.dropped_through_seq {
            continue; // the child was dropped after capture: discarded at apply
        }
        match readiness(state, child.surface, c.applied_update_seq, &child.update, forced, None) {
            Readiness::Ready => {}
            other => return other,
        }
    }
    let _ = (surface, top); // used by FIFO_COMMIT_TIMING_PLAN
    Readiness::Ready
}

fn top_level(surface: &WlSurface) -> TopLevel {
    TopLevel {
        active_output: surface.active_output_id(),
        now: std::time::Instant::now(),
        output_count: surface.output_membership_ids().len(),
    }
}

/// Applies `update` of `this` now when it is ready and nothing of the surface
/// waits; otherwise queues it behind the surface's earlier updates.
pub(super) fn submit(
    this: GrabbedObject<WlSurface>,
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    update: ContentUpdate,
) {
    let surface = client.resolve_this(&this, state);
    let top = top_level(&this);
    if this.queue.is_empty()
        && matches!(
            readiness(state, surface, this.applied_update_seq, &update, false, Some(top)),
            Readiness::Ready
        )
    {
        WlSurface::apply_update(this, client, state, dispatcher, update);
        if state.commit_queues.has_queued(client.client_id()) {
            flush_client(client, state, dispatcher);
        }
        return;
    }
    if state.commit_queues.depth(client.client_id()) >= MAX_QUEUED_UPDATES_PER_CLIENT {
        let id = this.id;
        let mut this = this;
        content_update::discard_update(
            client,
            state,
            dispatcher,
            &mut this.applied_update_seq,
            update,
        );
        drop(this);
        client.protocol_violation_disconnect(
            dispatcher,
            id.into(),
            wl_display::Error::NoMemory as u32,
            "too many wl_surface commits waiting",
        );
        return;
    }
    let mut this = this;
    this.queue.push_back(update);
    state.commit_queues.note_enqueued(surface);
    drop(this);
    flush_client(client, state, dispatcher);
}

/// Applies every queued update of `client` that is now ready, in order per
/// surface, until no more can be applied.
pub(crate) fn flush_client(
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
) {
    loop {
        let mut progressed = false;
        for surface in state.commit_queues.queued_surfaces(client.client_id()) {
            while try_apply_head(client, state, dispatcher, surface) {
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
}

/// Applies the head of `surface`'s queue when it is ready. Returns whether it
/// applied one.
fn try_apply_head(
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    surface: Resolved<WlSurfaceId>,
) -> bool {
    if !surface.exists(state) {
        state.commit_queues.forget_surface(surface, 0);
        return false;
    }
    let next = {
        // A grabbed surface keeps its queue; it is tried again on a later flush.
        if state.objects.is_grabbed(surface.object_id()) {
            return false;
        }
        let Some(wl) = surface.try_ref(state) else {
            return false;
        };
        wl.queue.front().map(|head| {
            readiness(state, surface, wl.applied_update_seq, head, false, Some(top_level(wl)))
        })
    };
    let Some(next) = next else {
        state.commit_queues.forget_surface(surface, 0);
        return false;
    };
    match next {
        Readiness::Ready => {
            let mut this = surface.grab(state);
            let update = this.queue.pop_front().unwrap();
            let now_empty = this.queue.is_empty();
            state.commit_queues.note_dequeued(surface, now_empty);
            WlSurface::apply_update(this, client, state, dispatcher, update);
            true
        }
        _ => false, // Phase 10 handles WaitAcquire
    }
}
