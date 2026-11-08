use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    object::{Object, Parser, RequestResult},
    protocol_generated::{zxdg_output_v1, WaylandId, ZxdgOutputV1Id},
    state::{GrabbedObject, Output, OutputId, State},
};

/// `xdg_output.done` is not sent for client objects at version 3+ (use `wl_output.done` instead).
const DONE_EVENT_NOT_SENT_SINCE: u32 = 3;

#[derive(Debug)]
pub struct ZxdgOutputV1 {
    id: ZxdgOutputV1Id,
    output_id: Option<OutputId>,
    version: u32,
}

impl ZxdgOutputV1 {
    pub fn new(id: ZxdgOutputV1Id, output_id: OutputId, version: u32) -> Self {
        Self { id, output_id: Some(output_id), version }
    }

    pub fn matches_output(&self, output_id: OutputId) -> bool {
        self.output_id == Some(output_id)
    }

    pub fn mark_removed(&mut self) {
        self.output_id = None;
    }

    fn output<'a>(&self, state: &'a State) -> Option<&'a Output> {
        self.output_id
            .and_then(|output_id| state.outputs.try_get(output_id))
    }

    pub fn send_state(&self, state: &State, dispatcher: &mut ClientDispatcher) {
        if self.output(state).is_none() {
            return;
        }
        self.send_logical_position(state, dispatcher);
        self.send_logical_size(state, dispatcher);
        self.send_name(state, dispatcher);
        self.send_description(state, dispatcher);
        self.send_done(dispatcher);
    }

    pub fn send_logical_position(&self, state: &State, dispatcher: &mut ClientDispatcher) {
        let Some(output) = self.output(state) else {
            return;
        };
        let wire_scale = dispatcher.wire_scale();
        self.send_event(
            zxdg_output_v1::Event::LogicalPosition {
                x: wire_scale.to_wire(output.x),
                y: wire_scale.to_wire(output.y),
            },
            dispatcher,
        );
    }

    pub fn send_logical_size(&self, state: &State, dispatcher: &mut ClientDispatcher) {
        let Some(output) = self.output(state) else {
            return;
        };
        let wire_scale = dispatcher.wire_scale();
        let (width, height) = output.logical_size();
        self.send_event(
            zxdg_output_v1::Event::LogicalSize {
                width: wire_scale.to_wire(width),
                height: wire_scale.to_wire(height),
            },
            dispatcher,
        );
    }

    pub fn send_name(&self, state: &State, dispatcher: &mut ClientDispatcher) {
        if self.version < zxdg_output_v1::NAME_SINCE {
            return;
        }
        let Some(output) = self.output(state) else {
            return;
        };
        let name = output.name.clone();
        self.send_event(zxdg_output_v1::Event::Name { name }, dispatcher);
    }

    pub fn send_description(&self, state: &State, dispatcher: &mut ClientDispatcher) {
        if self.version < zxdg_output_v1::DESCRIPTION_SINCE {
            return;
        }
        let Some(output) = self.output(state) else {
            return;
        };
        let description = output.description.clone();
        self.send_event(zxdg_output_v1::Event::Description { description }, dispatcher);
    }

    pub fn send_done(&self, dispatcher: &mut ClientDispatcher) {
        if self.version >= DONE_EVENT_NOT_SENT_SINCE {
            return;
        }
        self.send_event(zxdg_output_v1::Event::Done, dispatcher);
    }
}

impl Object for ZxdgOutputV1 {
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
        use zxdg_output_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                use crate::wayland_objects::wl_display::WlDisplay;
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            clients::ClientAction,
            io_uring::model::IoUringAction,
            runner::{Dispatcher, Model},
            state::{BackendOutputRef, DrmId, FractionalScale, OutputId},
            test_support::{state_for_tests, test_client_and_dispatcher, test_output},
            wire_scale::WireScale,
        },
    };

    /// A 2x output: mode 2560x1440 gives a logical size of 1280x720.
    fn output_state() -> (State, OutputId) {
        let mut state = state_for_tests();
        let output_id = state.insert_output(test_output(
            BackendOutputRef::new(DrmId::from(1usize)),
            1,
            1280,
            0,
            2560,
            1440,
            "test-output",
        ));
        state.outputs.try_get_mut(output_id).unwrap().scale = FractionalScale::TWO;
        (state, output_id)
    }

    const XDG_OUTPUT_ID: u32 = 14;

    fn queued_state(scale: WireScale) -> Dispatcher {
        let (state, output_id) = output_state();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        client.set_wire_scale_for_tests(scale);
        let xdg_output = ZxdgOutputV1::new(ZxdgOutputV1Id::from(XDG_OUTPUT_ID), output_id, 3);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            xdg_output.send_state(&state, &mut client_dispatcher);
        }
        dispatcher
    }

    /// Argument words of the first queued event with the given object/opcode.
    fn event_args(dispatcher: &Dispatcher, object: u32, opcode: u32) -> Vec<u32> {
        let mut args = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::Sendmsg { buffer, .. } = action {
                let words: Vec<u32> = buffer
                    .chunks_exact(4)
                    .map(|chunk| u32::from_ne_bytes(chunk.try_into().unwrap()))
                    .collect();
                if words.len() >= 2 && words[0] == object && (words[1] & 0xffff) == opcode {
                    args = words[2..].to_vec();
                }
            }
        });
        args
    }

    #[test]
    fn wire_scaled_client_gets_doubled_logical_position_and_size() {
        let dispatcher = queued_state(WireScale::new(2));
        assert_eq!(&event_args(&dispatcher, XDG_OUTPUT_ID, 0)[0..2], &[2560, 0]);
        assert_eq!(&event_args(&dispatcher, XDG_OUTPUT_ID, 1)[0..2], &[2560, 1440]);
    }

    #[test]
    fn normal_client_logical_position_and_size_match_today() {
        let dispatcher = queued_state(WireScale::ONE);
        assert_eq!(&event_args(&dispatcher, XDG_OUTPUT_ID, 0)[0..2], &[1280, 0]);
        assert_eq!(&event_args(&dispatcher, XDG_OUTPUT_ID, 1)[0..2], &[1280, 720]);
    }

    #[test]
    fn set_wire_scale_resends_xdg_output_at_the_new_scale() {
        let (mut state, output_id) = output_state();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        client.insert_object(
            &mut state,
            ZxdgOutputV1::new(ZxdgOutputV1Id::from(XDG_OUTPUT_ID), output_id, 3),
        );
        client
            .process(
                &mut state,
                ClientAction::SetWireScale { scale: WireScale::new(2) },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(&event_args(&dispatcher, XDG_OUTPUT_ID, 0)[0..2], &[2560, 0]);
        assert_eq!(&event_args(&dispatcher, XDG_OUTPUT_ID, 1)[0..2], &[2560, 1440]);
    }
}
