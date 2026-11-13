use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest, Resolved},
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        zwp_pointer_gesture_pinch_v1, WaylandId, WlPointerId, WlSurfaceId,
        ZwpPointerGesturePinchV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

#[derive(Debug)]
pub struct ZwpPointerGesturePinchV1 {
    id: ZwpPointerGesturePinchV1Id,
    pointer: Resolved<WlPointerId>,
}

impl ZwpPointerGesturePinchV1 {
    pub(crate) fn new(id: ZwpPointerGesturePinchV1Id, pointer: Resolved<WlPointerId>) -> Self {
        Self { id, pointer }
    }

    fn unregister(&self, state: &mut State) {
        state
            .seat
            .unregister_pointer_gesture(self.pointer, self.id.into());
    }

    pub(crate) fn send_begin(
        &self,
        serial: u32,
        time: u32,
        surface: Resolved<WlSurfaceId>,
        fingers: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.send_event(
            zwp_pointer_gesture_pinch_v1::Event::Begin {
                serial,
                time,
                surface: surface.wayland_id(),
                fingers,
            },
            dispatcher,
        );
    }

    pub(crate) fn send_update(
        &self,
        time: u32,
        dx: f32,
        dy: f32,
        scale: f32,
        rotation: f32,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.send_event(
            zwp_pointer_gesture_pinch_v1::Event::Update { time, dx, dy, scale, rotation },
            dispatcher,
        );
    }

    pub(crate) fn send_end(
        &self,
        serial: u32,
        time: u32,
        cancelled: bool,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.send_event(
            zwp_pointer_gesture_pinch_v1::Event::End {
                serial,
                time,
                cancelled: i32::from(cancelled),
            },
            dispatcher,
        );
    }
}

impl Object for ZwpPointerGesturePinchV1 {
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
            zwp_pointer_gesture_pinch_v1::Request::Destroy => {
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
