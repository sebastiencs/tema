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
        self.role = next.role;
        std::mem::replace(&mut self.presentation_feedbacks, next.presentation_feedbacks)
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
}
