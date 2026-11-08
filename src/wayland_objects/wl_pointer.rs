use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest, Resolved},
    object::{Object, Parser, RequestResult},
    protocol_generated::{wl_pointer, WaylandId, WlPointerId, WlSurfaceId},
    seat::{
        pointer_scroll_protocol_events, PointerScrollAxis, PointerScrollDirection,
        PointerScrollFrame, PointerScrollSource, PointerScrollWireEvent,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

#[derive(Debug)]
pub struct WlPointer {
    id: WlPointerId,
    /// `wl_seat` version the client used when creating this object (`get_pointer`).
    version: u32,
}

impl WlPointer {
    pub fn new(id: WlPointerId, version: u32) -> Self {
        Self { id, version }
    }

    pub fn send_enter(
        &self,
        serial: u32,
        surface: Resolved<WlSurfaceId>,
        x: f32,
        y: f32,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.send_event(
            wl_pointer::Event::Enter {
                serial,
                surface: surface.wayland_id(),
                surface_x: x,
                surface_y: y,
            },
            dispatcher,
        );
    }

    pub fn send_leave(
        &self,
        serial: u32,
        surface: Resolved<WlSurfaceId>,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.send_event(
            wl_pointer::Event::Leave { serial, surface: surface.wayland_id() },
            dispatcher,
        );
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn send_motion(&self, time: u32, x: f32, y: f32, dispatcher: &mut ClientDispatcher) {
        self.send_event(wl_pointer::Event::Motion { time, surface_x: x, surface_y: y }, dispatcher);
    }

    pub fn send_button(
        &self,
        serial: u32,
        time: u32,
        button: u32,
        state: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        self.send_event(wl_pointer::Event::Button { serial, time, button, state }, dispatcher);
    }

    pub fn send_warp(&self, x: f32, y: f32, dispatcher: &mut ClientDispatcher) {
        self.send_event(wl_pointer::Event::Warp { surface_x: x, surface_y: y }, dispatcher);
    }

    pub fn send_frame(&self, dispatcher: &mut ClientDispatcher) {
        if self.version < wl_pointer::FRAME_SINCE {
            return;
        }
        self.send_event(wl_pointer::Event::Frame, dispatcher);
    }

    pub fn send_scroll(&self, frame: &PointerScrollFrame, dispatcher: &mut ClientDispatcher) {
        for event in pointer_scroll_protocol_events(self.version, frame) {
            self.send_event(scroll_wire_event(event), dispatcher);
        }
    }
}

fn scroll_wire_event(event: PointerScrollWireEvent) -> wl_pointer::Event {
    use wl_pointer::Event::*;

    match event {
        PointerScrollWireEvent::Source(source) => {
            AxisSource { axis_source: scroll_source_code(source) }
        }
        PointerScrollWireEvent::Discrete { axis, discrete } => {
            AxisDiscrete { axis: scroll_axis_code(axis), discrete }
        }
        PointerScrollWireEvent::Value120 { axis, value120 } => {
            AxisValue120 { axis: scroll_axis_code(axis), value120 }
        }
        PointerScrollWireEvent::RelativeDirection { axis, direction } => AxisRelativeDirection {
            axis: scroll_axis_code(axis),
            direction: scroll_direction_code(direction),
        },
        PointerScrollWireEvent::Axis { time, axis, value_bits } => Axis {
            time,
            axis: scroll_axis_code(axis),
            value: f32::from_bits(value_bits),
        },
        PointerScrollWireEvent::Stop { time, axis } => {
            AxisStop { time, axis: scroll_axis_code(axis) }
        }
        PointerScrollWireEvent::Frame => Frame,
    }
}

fn scroll_axis_code(axis: PointerScrollAxis) -> u32 {
    match axis {
        PointerScrollAxis::Vertical => wl_pointer::Axis::VerticalScroll as u32,
        PointerScrollAxis::Horizontal => wl_pointer::Axis::HorizontalScroll as u32,
    }
}

fn scroll_source_code(source: PointerScrollSource) -> u32 {
    match source {
        PointerScrollSource::Wheel => wl_pointer::AxisSource::Wheel as u32,
        PointerScrollSource::Finger => wl_pointer::AxisSource::Finger as u32,
        PointerScrollSource::Continuous => wl_pointer::AxisSource::Continuous as u32,
        PointerScrollSource::WheelTilt => wl_pointer::AxisSource::WheelTilt as u32,
    }
}

fn scroll_direction_code(direction: PointerScrollDirection) -> u32 {
    match direction {
        PointerScrollDirection::Identical => wl_pointer::AxisRelativeDirection::Identical as u32,
        PointerScrollDirection::Inverted => wl_pointer::AxisRelativeDirection::Inverted as u32,
    }
}

impl Object for WlPointer {
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
        use wl_pointer::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            SetCursor { serial, surface, hotspot_x, hotspot_y } => {
                let surface = match surface.to_opt() {
                    Some(surface) => {
                        Some(client.resolve_or_disconnect(surface, state, dispatcher)?)
                    }
                    None => None,
                };
                if !state
                    .seat
                    .pointer_cursor_request_allowed(client.client_id(), serial)
                {
                    return Ok(());
                }
                let cursor_changed = match surface {
                    None => state.seat.clear_cursor_for_client(client.client_id()),
                    Some(surface) => {
                        {
                            let mut surface_object = surface.ref_mut(state);
                            if surface_object.set_cursor_role(dispatcher).is_err() {
                                return Ok(());
                            }
                        }
                        // The hotspot arrives in the client's wire units. The
                        // cursor surface's attach offset is already logical:
                        // its commit divided that offset by S.
                        let scale = dispatcher.wire_scale();
                        state.seat.set_cursor_surface(
                            surface,
                            scale.pos_to_logical(hotspot_x),
                            scale.pos_to_logical(hotspot_y),
                        )
                    }
                };
                if cursor_changed {
                    dispatcher.dispatch_to(
                        client.client_id(),
                        crate::clients::ClientAction::RefreshSurfaceOutputs,
                    );
                    let pointer_pos = state.seat.pointer_position();
                    crate::drm::request_outputs_for_points(state, [pointer_pos], dispatcher);
                }
            }
            Release => {
                if let Some(pointer) = client.resolve(self.id, state) {
                    state.seat.unregister_pointer(pointer);
                    crate::capture::pointer_released(state, dispatcher, pointer);
                }
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::{Arguments, Client, ClientId, RawRequest},
        cursor::CursorSourceRef,
        input::{
            types::{SurfaceFocusPick, SurfaceRect},
            SurfaceFocus,
        },
        protocol_generated::{WlPointerId, WlSurfaceId},
        runner::Dispatcher,
        test_support::{state_for_tests, test_client_and_dispatcher},
        wayland_objects::wl_surface::WlSurface,
        wire_scale::WireScale,
    };

    use super::WlPointer;

    fn set_cursor(scale: WireScale) -> (i32, i32) {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        client.set_wire_scale_for_tests(scale);
        let (surface, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(4), ClientId::from_raw(1), 6),
        );
        let (pointer, _) =
            client.insert_object(&mut state, WlPointer::new(WlPointerId::from(5), 9));
        state.seat.pointer_focus = Some(SurfaceFocusPick {
            target: SurfaceFocus::new(surface),
            rect: SurfaceRect { x: 0, y: 0, width: 10, height: 10 },
        });
        state.seat.record_pointer_enter(client.client_id(), 7);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(pointer.wayland_id().into());
        state
            .objects
            .grab(pointer.object_id())
            .on(
                &mut client,
                &mut state,
                RawRequest {
                    object_id: 5,
                    request_id: 0,
                    arguments: Arguments::from_words(std::collections::VecDeque::from(vec![
                        7, 4, 9, 9,
                    ])),
                    fds: None,
                },
                &mut client_dispatcher,
            )
            .unwrap();
        drop(client_dispatcher);
        match state.seat.visible_cursor_source() {
            Some(CursorSourceRef::Surface { cursor }) => (cursor.hotspot_x, cursor.hotspot_y),
            other => panic!("cursor was not the surface: {other:?}"),
        }
    }

    #[test]
    fn a_wire_scaled_hotspot_is_stored_in_logical_units() {
        assert_eq!(set_cursor(WireScale::new(2)), (4, 4));
    }

    #[test]
    fn wire_scale_one_leaves_the_hotspot_unchanged() {
        assert_eq!(set_cursor(WireScale::ONE), (9, 9));
    }
}
