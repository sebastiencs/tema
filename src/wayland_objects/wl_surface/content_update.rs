use super::*;

/// One `wl_surface.commit` (or several merged synchronized commits), captured
/// at request time and applied when ready.
#[derive(Debug)]
pub(crate) struct ContentUpdate {
    /// `WlSurface::next_update_seq` of the first commit merged into this update.
    pub(super) first_seq: u64,
    /// Same, of the last one.
    pub(super) last_seq: u64,
    /// `WlSurface::role_epoch` when captured.
    pub(super) role_epoch: u64,
    pub(super) payload: SurfaceCommitPayload,
    pub(super) frame_callbacks: Vec<Resolved<WlCallbackId>>,
    pub(super) presentation_feedbacks: Vec<SurfacePresentationFeedback>,
    pub(super) pointer_constraint: Option<PointerConstraintUpdate>,
    pub(super) xwayland_serial: Option<u64>,
    /// The acquire wait failed or could not be set up; apply without it.
    pub(super) acquire_forced: bool,
    /// The role part of the commit, applied only while the role object of
    /// `role_epoch` is still the surface's role.
    pub(super) role: RoleUpdate,
    /// The subsurface stack and child positions at capture.
    pub(super) tree: TreeUpdate,
    /// The cached updates of synchronized children, taken at capture.
    pub(super) children: Vec<ChildUpdate>,
    /// The commit set a fifo barrier: applying it sets the surface's barrier.
    pub(super) fifo_set_barrier: bool,
    /// The commit waits while its surface holds a fifo barrier.
    pub(super) fifo_wait_barrier: bool,
    /// The commit may not apply before this time (commit timing).
    pub(super) commit_target: Option<Instant>,
    /// Test-only: the readiness check holds this update until it is released.
    #[cfg(test)]
    pub(super) test_hold: bool,
}

/// A synchronized child's cached update, carried by its parent's update.
#[derive(Debug)]
pub(crate) struct ChildUpdate {
    pub(super) surface: Resolved<WlSurfaceId>,
    pub(super) update: Box<ContentUpdate>,
}

/// The subsurface tree as it was pending when the commit was captured.
#[derive(Clone, Debug, Default)]
pub(crate) struct TreeUpdate {
    pub(super) stack: Vec<StackEntry>,
    /// (child surface, its `wl_subsurface` at capture, pending position).
    pub(super) positions: Vec<(Resolved<WlSurfaceId>, Resolved<WlSubsurfaceId>, (i32, i32))>,
}

#[derive(Clone, Debug)]
pub(crate) struct PointerConstraintUpdate {
    pub(super) constraint: SurfacePointerConstraint,
    pub(super) region: Option<Option<RegionValue>>,
    pub(super) hint: Option<(f32, f32)>,
}

/// The role part of a captured commit.
#[derive(Clone, Debug, Default)]
pub(crate) enum RoleUpdate {
    #[default]
    None,
    Xdg {
        xdg_surface: Resolved<XdgSurfaceId>,
        capture: crate::wayland_objects::xdg_surface::XdgCommitCapture,
    },
    Layer {
        layer_surface: Resolved<ZwlrLayerSurfaceV1Id>,
        capture: crate::wayland_objects::zwlr_layer_surface_v1::LayerCommitCapture,
    },
}

impl ContentUpdate {
    pub(super) fn new(
        seq: u64,
        role_epoch: u64,
        payload: SurfaceCommitPayload,
        frame_callbacks: Vec<Resolved<WlCallbackId>>,
        presentation_feedbacks: Vec<SurfacePresentationFeedback>,
    ) -> Self {
        Self {
            first_seq: seq,
            last_seq: seq,
            role_epoch,
            payload,
            frame_callbacks,
            presentation_feedbacks,
            pointer_constraint: None,
            xwayland_serial: None,
            acquire_forced: false,
            role: RoleUpdate::None,
            tree: TreeUpdate::default(),
            children: Vec::new(),
            fifo_set_barrier: false,
            fifo_wait_barrier: false,
            commit_target: None,
            #[cfg(test)]
            test_hold: false,
        }
    }

    #[cfg(test)]
    pub(super) fn test_new(
        payload: SurfaceCommitPayload,
        frame_callbacks: Vec<Resolved<WlCallbackId>>,
        presentation_feedbacks: Vec<SurfacePresentationFeedback>,
    ) -> Self {
        Self::new(1, 0, payload, frame_callbacks, presentation_feedbacks)
    }

    /// Folds a later commit of the same surface into this update. Returns the
    /// presentation feedback that the newer commit superseded.
    pub(super) fn merge(
        &mut self,
        next: ContentUpdate,
        retired: &mut Vec<RetiredBufferCandidate>,
    ) -> Vec<SurfacePresentationFeedback> {
        let payload = next.payload;
        let cached = &mut self.payload;
        if payload.buffer.is_some() {
            if let Some(old_buffer) = cached.buffer.as_ref().and_then(|b| b.as_ref()) {
                if payload.buffer.as_ref().and_then(|b| b.as_ref()) != Some(old_buffer) {
                    // Always retired, even without release callbacks, so the
                    // replaced buffer gets its wl_buffer.release.
                    retired.push(RetiredBufferCandidate {
                        buffer: old_buffer.object,
                        sync: cached.sync.clone(),
                        release_callbacks: std::mem::take(&mut cached.release_callbacks),
                    });
                }
            }
            cached.buffer = payload.buffer;
            cached.offset = payload.offset;
            cached.sync = payload.sync;
            cached.release_callbacks.extend(payload.release_callbacks);
        } else {
            cached.release_callbacks.extend(payload.release_callbacks);
        }
        cached.buffer_transform = payload.buffer_transform;
        cached.buffer_scale = payload.buffer_scale;
        cached.viewport = payload.viewport;
        cached.presentation_hint = payload.presentation_hint;
        cached.alpha_multiplier = payload.alpha_multiplier;
        cached.blur_region = payload.blur_region.clone();
        if payload.color.is_some() {
            cached.color = payload.color;
        }
        cached.representation.merge(payload.representation);
        cached.damage_surface.extend(payload.damage_surface);
        cached.damage_buffer.extend(payload.damage_buffer);
        cached.input_region = payload.input_region;
        cached.opaque_region = payload.opaque_region;
        cached.wire_inputs = payload.wire_inputs;

        self.frame_callbacks.extend(next.frame_callbacks);
        self.last_seq = next.last_seq;
        self.role_epoch = next.role_epoch;
        if next.xwayland_serial.is_some() {
            self.xwayland_serial = next.xwayland_serial;
        }
        if let Some(next_constraint) = next.pointer_constraint {
            match self.pointer_constraint.as_mut() {
                Some(current) if current.constraint == next_constraint.constraint => {
                    current.region = next_constraint.region.or(current.region.take());
                    current.hint = next_constraint.hint.or(current.hint);
                }
                _ => self.pointer_constraint = Some(next_constraint),
            }
        }
        self.acquire_forced |= next.acquire_forced;
        self.fifo_set_barrier |= next.fifo_set_barrier;
        self.fifo_wait_barrier |= next.fifo_wait_barrier;
        self.commit_target = self.commit_target.max(next.commit_target);
        #[cfg(test)]
        {
            self.test_hold |= next.test_hold;
        }
        self.role = next.role;
        self.tree = next.tree;
        let mut discarded =
            std::mem::replace(&mut self.presentation_feedbacks, next.presentation_feedbacks);
        for child in next.children {
            match self
                .children
                .iter_mut()
                .find(|carried| carried.surface == child.surface)
            {
                Some(carried) => discarded.extend(carried.update.merge(*child.update, retired)),
                None => self.children.push(child),
            }
        }
        discarded
    }
}

/// Drops an update that will never be applied: marks its surface (`owner`)
/// applied up to `last_seq`, releases its buffer, completes its release
/// callbacks, discards its presentation feedback, completes its frame callbacks.
/// `owner_seq` is the owner's `applied_update_seq`, updated in place (the owner
/// is usually grabbed by the caller).
pub(super) fn discard_update(
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    owner_seq: &mut u64,
    update: ContentUpdate,
) {
    *owner_seq = (*owner_seq).max(update.last_seq);
    if let Some(Some(buffer)) = update.payload.buffer {
        crate::sync::resolve_unused_candidate_immediately(
            state,
            dispatcher,
            RetiredBufferCandidate {
                buffer: buffer.object,
                sync: update.payload.sync,
                release_callbacks: update.payload.release_callbacks,
            },
        );
    }
    WlSurface::discard_feedbacks(client, state, dispatcher, update.presentation_feedbacks);
    WlSurface::complete_frame_callbacks(client, state, dispatcher, update.frame_callbacks);
    for child in update.children {
        let mut seq = child
            .surface
            .try_ref(state)
            .map_or(0, |s| s.applied_update_seq);
        discard_update(client, state, dispatcher, &mut seq, *child.update);
        if let Some(owner) = child.surface.try_ref_mut(state) {
            owner.applied_update_seq = owner.applied_update_seq.max(seq);
        }
    }
}
