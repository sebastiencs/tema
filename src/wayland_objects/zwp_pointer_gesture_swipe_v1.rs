use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        zwp_pointer_gesture_swipe_v1, WaylandId, WlPointerId, ZwpPointerGestureSwipeV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

#[derive(Debug)]
pub struct ZwpPointerGestureSwipeV1 {
    id: ZwpPointerGestureSwipeV1Id,
    pointer: Resolved<WlPointerId>,
}

impl ZwpPointerGestureSwipeV1 {
    pub(crate) fn new(id: ZwpPointerGestureSwipeV1Id, pointer: Resolved<WlPointerId>) -> Self {
        Self { id, pointer }
    }

    fn unregister(&self, state: &mut State) {
        state
            .seat
            .unregister_pointer_gesture(self.pointer, self.id.into());
    }
}

impl Object for ZwpPointerGestureSwipeV1 {
    fn id(&self) -> WaylandId {
        self.id.into()
    }

    fn on(
        mut self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        match client.parse::<Self>(req, dispatcher)? {
            zwp_pointer_gesture_swipe_v1::Request::Destroy => {
                self.unregister(state);
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
        }
        Ok(())
    }

    fn on_client_disconnect(
        &mut self,
        _client: &mut Client,
        state: &mut State,
        _dispatcher: &mut ClientDispatcher,
    ) {
        self.unregister(state);
    }
}
