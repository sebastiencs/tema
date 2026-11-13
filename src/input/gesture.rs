//! `zwp_pointer_gestures_v1` resource bookkeeping and gesture routing.

use crate::{
    clients::Resolved,
    protocol_generated::{
        WaylandId, ZwpPointerGestureHoldV1Id, ZwpPointerGesturePinchV1Id,
        ZwpPointerGestureSwipeV1Id,
    },
};

/// One gesture object created through `zwp_pointer_gestures_v1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerGestureResource {
    Swipe(Resolved<ZwpPointerGestureSwipeV1Id>),
    Pinch(Resolved<ZwpPointerGesturePinchV1Id>),
    Hold(Resolved<ZwpPointerGestureHoldV1Id>),
}

impl PointerGestureResource {
    pub(crate) fn wayland_id(self) -> WaylandId {
        match self {
            Self::Swipe(resource) => resource.wayland_id().into(),
            Self::Pinch(resource) => resource.wayland_id().into(),
            Self::Hold(resource) => resource.wayland_id().into(),
        }
    }
}
