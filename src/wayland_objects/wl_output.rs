//! `wl_output` global backed by compositor-facing logical outputs. The DRM backend still owns
//! presentation/runtime state, but protocol-facing geometry and metadata live in
//! [`crate::state::Output`].

use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{wl_output, Constants, NewId, WaylandId, WlOutputId},
    state::{GrabbedObject, Output, OutputId, State},
};

#[derive(Debug)]
pub struct WlOutputGlobal {
    output_id: OutputId,
}

impl WlOutputGlobal {
    pub fn new(output_id: OutputId) -> Box<dyn Global> {
        Box::new(Self { output_id })
    }
}

impl Global for WlOutputGlobal {
    fn interface(&self) -> (&'static str, u32) {
        (WlOutput::INTERFACE_NAME, WlOutput::INTERFACE_VERSION)
    }
    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        version: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        let id = WlOutputId::from(id);
        let _ = client.insert_object(state, WlOutput::new(id, self.output_id, version));
        let Some(wl_output) = client.resolve(id, state) else {
            return;
        };
        wl_output.rref(state).send_state(state, dispatcher);
        client.replay_surface_output_for_output(state, dispatcher, self.output_id);
        crate::capture::replay_wlr_toplevel_output(
            client,
            state,
            dispatcher,
            self.output_id,
            wl_output,
        );
        client.refresh_workspace_protocols(state, dispatcher);
    }
}

#[derive(Debug)]
pub struct WlOutput {
    id: WlOutputId,
    output_id: Option<OutputId>,
    version: u32,
}

impl Object for WlOutput {
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
        use wl_output::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Release => {
                use crate::wayland_objects::wl_display::WlDisplay;
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
        }
        Ok(())
    }
}

impl WlOutput {
    pub fn new(id: WlOutputId, output_id: OutputId, version: u32) -> Self {
        Self { id, output_id: Some(output_id), version }
    }

    pub fn wl_output_id(&self) -> WlOutputId {
        self.id
    }

    pub fn output_id(&self) -> Option<OutputId> {
        self.output_id
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
        self.send_geometry(state, dispatcher);
        self.send_mode(state, dispatcher);
        self.send_scale(state, dispatcher);
        self.send_name(state, dispatcher);
        self.send_description(state, dispatcher);
        self.send_done(dispatcher);
    }

    pub fn send_geometry(&self, state: &State, dispatcher: &mut ClientDispatcher) {
        let Some(output) = self.output(state) else {
            return;
        };

        let wire_scale = dispatcher.wire_scale();
        let geometry = wl_output::Event::Geometry {
            x: wire_scale.to_wire(output.x),
            y: wire_scale.to_wire(output.y),
            physical_width: output.physical_width,
            physical_height: output.physical_height,
            subpixel: wl_output::Subpixel::Unknown as i32,
            make: output.make.clone(),
            model: output.model.clone(),
            transform: output.transform,
        };
        self.send_event(geometry, dispatcher);
    }

    pub fn send_mode(&self, state: &State, dispatcher: &mut ClientDispatcher) {
        let Some(output) = self.output(state) else {
            return;
        };
        // Keep the physical mode unchanged: rootless Xwayland derives its RandR
        // layout from zxdg_output logical_position/logical_size and ignores
        // wl_output.mode once it has bound xdg_output.
        self.send_event(mode_event(output), dispatcher);
    }

    pub fn send_scale(&self, state: &State, dispatcher: &mut ClientDispatcher) {
        if self.version < wl_output::SCALE_SINCE {
            return;
        }
        // A wire-scaled client sees its geometry in X pixels and renders its
        // buffers at scale 1, so report 1 instead of the output's legacy scale.
        let factor = if dispatcher.wire_scale().is_one() {
            self.output(state)
                .map(|output| output.legacy_scale())
                .unwrap_or(1)
        } else {
            1
        };
        self.send_event(wl_output::Event::Scale { factor }, dispatcher);
    }

    pub fn send_name(&self, state: &State, dispatcher: &mut ClientDispatcher) {
        if self.version < wl_output::NAME_SINCE {
            return;
        }
        let Some(output) = self.output(state) else {
            return;
        };
        let name = output.name.clone();
        self.send_event(wl_output::Event::Name { name }, dispatcher);
    }

    pub fn send_description(&self, state: &State, dispatcher: &mut ClientDispatcher) {
        if self.version < wl_output::DESCRIPTION_SINCE {
            return;
        }
        let Some(output) = self.output(state) else {
            return;
        };
        let description = output.description.clone();
        self.send_event(wl_output::Event::Description { description }, dispatcher);
    }

    pub fn send_done(&self, dispatcher: &mut ClientDispatcher) {
        if self.version < wl_output::DONE_SINCE {
            return;
        }
        self.send_event(wl_output::Event::Done, dispatcher);
    }
}

fn mode_event(output: &Output) -> wl_output::Event {
    let mode = &output.current_mode;
    let mut flags = wl_output::Mode::Current as u32;
    if output
        .preferred_mode
        .as_ref()
        .is_some_and(|preferred| preferred == mode)
    {
        flags |= wl_output::Mode::Preferred as u32;
    }

    wl_output::Event::Mode {
        flags,
        width: mode.width,
        height: mode.height,
        refresh: mode.refresh_millihz,
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            io_uring::model::IoUringAction,
            runner::Dispatcher,
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

    const WL_OUTPUT_ID: u32 = 12;

    fn queued_state(scale: WireScale) -> Dispatcher {
        let (state, output_id) = output_state();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        client.set_wire_scale_for_tests(scale);
        let wl_output = WlOutput::new(WlOutputId::from(WL_OUTPUT_ID), output_id, 4);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            wl_output.send_state(&state, &mut client_dispatcher);
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
    fn wire_scaled_client_gets_doubled_geometry_and_scale_one() {
        let dispatcher = queued_state(WireScale::new(2));
        let geometry = event_args(&dispatcher, WL_OUTPUT_ID, 0);
        assert_eq!(&geometry[0..2], &[2560, 0]);
        assert_eq!(event_args(&dispatcher, WL_OUTPUT_ID, 3)[0], 1);
        // The physical mode stays unchanged.
        assert_eq!(&event_args(&dispatcher, WL_OUTPUT_ID, 1)[1..3], &[2560, 1440]);
    }

    #[test]
    fn normal_client_output_state_matches_today() {
        let dispatcher = queued_state(WireScale::ONE);
        let geometry = event_args(&dispatcher, WL_OUTPUT_ID, 0);
        assert_eq!(&geometry[0..2], &[1280, 0]);
        assert_eq!(event_args(&dispatcher, WL_OUTPUT_ID, 3)[0], 2);
        assert_eq!(&event_args(&dispatcher, WL_OUTPUT_ID, 1)[1..3], &[2560, 1440]);
    }

    #[test]
    fn mode_event_uses_output_nominal_refresh() {
        let mut state = state_for_tests();
        let output_id = state.insert_output(test_output(
            BackendOutputRef::new(DrmId::from(0usize)),
            1,
            0,
            0,
            1920,
            1080,
            "test-output",
        ));
        let output = state.outputs.try_get_mut(output_id).unwrap();
        output.current_mode.refresh_millihz = 143_855;
        output.preferred_mode = Some(output.current_mode.clone());

        let event = mode_event(output);
        match event {
            wl_output::Event::Mode { refresh, .. } => assert_eq!(refresh, 143_855),
            _ => panic!("expected wl_output::Event::Mode"),
        }
    }
}
