use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    globals::Global,
    object::{Object, RequestResult},
    protocol_generated::{
        wp_color_representation_manager_v1, wp_color_representation_surface_v1, Constants, NewId,
        WaylandId, WpColorRepresentationManagerV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::{
        wl_display::WlDisplay, wp_color_representation_surface_v1::WpColorRepresentationSurfaceV1,
    },
};

#[derive(Debug)]
pub struct WpColorRepresentationManagerV1Global;

impl WpColorRepresentationManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for WpColorRepresentationManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (
            WpColorRepresentationManagerV1::INTERFACE_NAME,
            WpColorRepresentationManagerV1::INTERFACE_VERSION,
        )
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        let manager = WpColorRepresentationManagerV1 { id: id.into() };
        manager.send_supported(dispatcher);
        client.insert_object(state, manager);
    }
}

#[derive(Debug)]
pub struct WpColorRepresentationManagerV1 {
    id: WpColorRepresentationManagerV1Id,
}

impl WpColorRepresentationManagerV1 {
    fn send_supported(&self, dispatcher: &mut ClientDispatcher) {
        use {
            wp_color_representation_manager_v1::Event::*,
            wp_color_representation_surface_v1::{AlphaMode, Coefficients, Range},
        };
        for alpha_mode in [
            AlphaMode::PremultipliedElectrical,
            AlphaMode::PremultipliedOptical,
            AlphaMode::Straight,
        ] {
            self.send_event(SupportedAlphaMode { alpha_mode: alpha_mode as u32 }, dispatcher);
        }
        for (coefficients, range) in [
            (Coefficients::Identity, Range::Full),
            (Coefficients::Bt601, Range::Limited),
            (Coefficients::Bt601, Range::Full),
            (Coefficients::Bt709, Range::Limited),
            (Coefficients::Bt709, Range::Full),
            (Coefficients::Bt2020, Range::Limited),
            (Coefficients::Bt2020, Range::Full),
        ] {
            self.send_event(
                SupportedCoefficientsAndRanges {
                    coefficients: coefficients as u32,
                    range: range as u32,
                },
                dispatcher,
            );
        }
        self.send_event(Done, dispatcher);
    }
}

impl Object for WpColorRepresentationManagerV1 {
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
        use wp_color_representation_manager_v1::Request::*;
        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetSurface { id, surface } => {
                let surface = client.resolve_or_disconnect(surface, state, dispatcher)?;
                {
                    let surface = surface.rref(state);
                    if surface.has_color_representation_object() {
                        client.send_protocol_error(
                            dispatcher,
                            self.id.into(),
                            wp_color_representation_manager_v1::Error::SurfaceExists as u32,
                            "wl_surface already has a wp_color_representation_surface_v1",
                        );
                        return Ok(());
                    }
                }
                let (representation, _) =
                    client.insert_object(state, WpColorRepresentationSurfaceV1::new(id, surface));
                surface
                    .grab(state)
                    .set_color_representation_object(representation)
                    .expect("color representation existence was checked before insertion");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::{Client, RawEvent},
        globals::Globals,
        io_uring::model::IoUringAction,
        object::{IdToObject, Object, RequestResult},
        protocol_generated::{
            wp_color_representation_manager_v1, wp_color_representation_surface_v1, NewId,
            WaylandId, WlBufferId, WlSubsurfaceId, WlSurfaceId, WpColorRepresentationManagerV1Id,
            WpColorRepresentationSurfaceV1Id,
        },
        runner::Dispatcher,
        test_support::{state_for_tests, test_client_and_dispatcher},
        wayland_objects::{
            wl_buffer::{NewWlBufferParams, WlBuffer},
            wl_shm_pool::shared_memory_from_bytes,
            wl_subsurface::WlSubsurface,
            wl_surface::WlSurface,
        },
    };

    use super::*;

    fn request(
        object: WaylandId,
        opcode: u16,
        args: &[u32],
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(object, opcode, args.len());
        for arg in args {
            event.uint(*arg);
        }
        (&*event.into_bytes()).try_into().unwrap()
    }

    fn dispatch<K>(
        client: &mut Client,
        state: &mut crate::state::State,
        dispatcher: &mut Dispatcher,
        id: K,
        opcode: u16,
        args: &[u32],
    ) -> RequestResult
    where
        K: IdToObject + Copy + Into<WaylandId>,
        K::Object: Object,
    {
        let wire: WaylandId = id.into();
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(wire);
        let mut object = client.grab_object(id, state);
        object.on(client, state, request(wire, opcode, args), &mut client_dispatcher)
    }

    fn display_error(dispatcher: &Dispatcher) -> Option<(u32, u32)> {
        let mut found = None;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 16 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            if object != 1 {
                return;
            }
            let target = u32::from_ne_bytes(buffer[8..12].try_into().unwrap());
            let code = u32::from_ne_bytes(buffer[12..16].try_into().unwrap());
            found = Some((target, code));
        });
        found
    }

    fn events(dispatcher: &Dispatcher, object: u32) -> Vec<(u16, Vec<u32>)> {
        let mut found = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let mut words = Vec::new();
            for chunk in buffer.chunks_exact(4) {
                words.push(u32::from_ne_bytes(chunk.try_into().unwrap()));
            }
            if words[0] != object {
                return;
            }
            let opcode = (words[1] & 0xffff) as u16;
            found.push((opcode, words[2..].to_vec()));
        });
        found
    }

    fn insert_surface(client: &mut Client, state: &mut crate::state::State) -> WlSurfaceId {
        let id = WlSurfaceId::from(4);
        client.insert_object(state, WlSurface::new(id, client.client_id(), 6));
        id
    }

    fn attach(
        client: &mut Client,
        state: &mut crate::state::State,
        dispatcher: &mut Dispatcher,
        surface: WlSurfaceId,
        format: &'static crate::drm::Format,
        buffer: u32,
    ) {
        let bytes = vec![0u8; 16];
        client.insert_object(
            state,
            WlBuffer::new(NewWlBufferParams {
                id: WlBufferId::from(buffer),
                owner_client_id: client.client_id(),
                offset: 0,
                width: 4,
                height: 4,
                stride: 4,
                format,
                mem: shared_memory_from_bytes(&bytes),
            })
            .unwrap(),
        );
        dispatch(client, state, dispatcher, surface, 1, &[buffer, 0, 0]).unwrap();
    }

    fn commit(
        client: &mut Client,
        state: &mut crate::state::State,
        dispatcher: &mut Dispatcher,
        surface: WlSurfaceId,
    ) {
        dispatch(client, state, dispatcher, surface, 6, &[]).unwrap();
    }

    #[test]
    fn bind_advertises_alpha_modes_and_coefficient_pairs() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(WpColorRepresentationManagerV1::INTERFACE_NAME));
        assert_eq!(
            WpColorRepresentationManagerV1Global.interface(),
            ("wp_color_representation_manager_v1", 1)
        );

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut global = WpColorRepresentationManagerV1Global;
            global.bind(&mut client, &mut state, NewId::from(8), 1, &mut client_dispatcher);
        }
        let sent = events(&dispatcher, 8);
        let alpha: Vec<_> = sent.iter().filter(|event| event.0 == 0).cloned().collect();
        let pairs: Vec<_> = sent.iter().filter(|event| event.0 == 1).cloned().collect();
        assert_eq!(alpha, vec![(0, vec![0]), (0, vec![1]), (0, vec![2])]);
        assert_eq!(
            pairs,
            vec![
                (1, vec![1, 1]),
                (1, vec![4, 2]),
                (1, vec![4, 1]),
                (1, vec![2, 2]),
                (1, vec![2, 1]),
                (1, vec![6, 2]),
                (1, vec![6, 1]),
            ]
        );
        assert_eq!(sent.last().map(|event| event.0), Some(2));
    }

    #[test]
    fn a_second_representation_on_the_same_surface_is_rejected() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let manager = WpColorRepresentationManagerV1Id::from(3);
        client.insert_object(&mut state, WpColorRepresentationManagerV1 { id: manager });
        let surface = insert_surface(&mut client, &mut state);
        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[5, surface.into()])
            .unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[6, surface.into()])
            .unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((
                u32::from(manager),
                wp_color_representation_manager_v1::Error::SurfaceExists as u32
            ))
        );
        assert!(!client.contains_object(WpColorRepresentationSurfaceV1Id::from(6), &state));
    }

    #[test]
    fn unadvertised_values_raise_their_protocol_errors() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let representation = WpColorRepresentationSurfaceV1Id::from(5);
        let surface = insert_surface(&mut client, &mut state);
        let resolved = client.resolve(surface, &state).unwrap();
        client.insert_object(
            &mut state,
            WpColorRepresentationSurfaceV1::new(representation, resolved),
        );
        resolved
            .grab(&mut state)
            .set_color_representation_object(client.resolve(representation, &state).unwrap())
            .unwrap();

        dispatch(&mut client, &mut state, &mut dispatcher, representation, 1, &[9]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((
                u32::from(representation),
                wp_color_representation_surface_v1::Error::AlphaMode as u32
            ))
        );

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(4);
        let representation = WpColorRepresentationSurfaceV1Id::from(5);
        let surface = insert_surface(&mut client, &mut state);
        let resolved = client.resolve(surface, &state).unwrap();
        client.insert_object(
            &mut state,
            WpColorRepresentationSurfaceV1::new(representation, resolved),
        );
        dispatch(&mut client, &mut state, &mut dispatcher, representation, 2, &[3, 1]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((
                u32::from(representation),
                wp_color_representation_surface_v1::Error::Coefficients as u32
            ))
        );

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(5);
        let representation = WpColorRepresentationSurfaceV1Id::from(5);
        let surface = insert_surface(&mut client, &mut state);
        let resolved = client.resolve(surface, &state).unwrap();
        client.insert_object(
            &mut state,
            WpColorRepresentationSurfaceV1::new(representation, resolved),
        );
        dispatch(&mut client, &mut state, &mut dispatcher, representation, 3, &[0]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((
                u32::from(representation),
                wp_color_representation_surface_v1::Error::ChromaLocation as u32
            ))
        );
    }

    #[test]
    fn commit_rejects_a_matrix_the_buffer_cannot_use() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(6);
        let representation = WpColorRepresentationSurfaceV1Id::from(5);
        let surface = insert_surface(&mut client, &mut state);
        let resolved = client.resolve(surface, &state).unwrap();
        client.insert_object(
            &mut state,
            WpColorRepresentationSurfaceV1::new(representation, resolved),
        );
        resolved
            .grab(&mut state)
            .set_color_representation_object(client.resolve(representation, &state).unwrap())
            .unwrap();
        attach(&mut client, &mut state, &mut dispatcher, surface, crate::drm::ARGB8888, 20);
        commit(&mut client, &mut state, &mut dispatcher, surface);
        dispatch(&mut client, &mut state, &mut dispatcher, representation, 2, &[2, 2]).unwrap();
        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(
            display_error(&dispatcher),
            Some((
                u32::from(representation),
                wp_color_representation_surface_v1::Error::PixelFormat as u32
            ))
        );
        assert!(resolved
            .rref(&state)
            .committed_representation()
            .coefficients
            .is_none());

        dispatch(&mut client, &mut state, &mut dispatcher, representation, 3, &[1]).unwrap();
        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert!(resolved
            .rref(&state)
            .committed_representation()
            .chroma
            .is_none());
    }

    #[test]
    fn identity_is_legal_on_rgb_and_illegal_on_yuv() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let representation = WpColorRepresentationSurfaceV1Id::from(5);
        let surface = insert_surface(&mut client, &mut state);
        let resolved = client.resolve(surface, &state).unwrap();
        client.insert_object(
            &mut state,
            WpColorRepresentationSurfaceV1::new(representation, resolved),
        );
        resolved
            .grab(&mut state)
            .set_color_representation_object(client.resolve(representation, &state).unwrap())
            .unwrap();
        attach(&mut client, &mut state, &mut dispatcher, surface, crate::drm::ARGB8888, 20);
        commit(&mut client, &mut state, &mut dispatcher, surface);
        dispatch(&mut client, &mut state, &mut dispatcher, representation, 2, &[1, 1]).unwrap();
        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(
            resolved
                .rref(&state)
                .committed_representation()
                .coefficients
                .map(|coefficients| coefficients.kind),
            Some(crate::color::yuv::CoefficientKind::Identity)
        );

        let nv12 = crate::drm::client_format(drm::buffer::DrmFourcc::Nv12).unwrap();
        attach(&mut client, &mut state, &mut dispatcher, surface, nv12, 21);
        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(
            display_error(&dispatcher),
            Some((
                u32::from(representation),
                wp_color_representation_surface_v1::Error::PixelFormat as u32
            ))
        );
    }

    #[test]
    fn destroy_unsets_on_the_next_commit_and_a_dead_surface_is_inert() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(8);
        let representation = WpColorRepresentationSurfaceV1Id::from(5);
        let surface = insert_surface(&mut client, &mut state);
        let resolved = client.resolve(surface, &state).unwrap();
        client.insert_object(
            &mut state,
            WpColorRepresentationSurfaceV1::new(representation, resolved),
        );
        resolved
            .grab(&mut state)
            .set_color_representation_object(client.resolve(representation, &state).unwrap())
            .unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, representation, 1, &[2]).unwrap();
        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(
            resolved.rref(&state).committed_representation().alpha_mode,
            Some(crate::color::AlphaMode::Straight)
        );
        dispatch(&mut client, &mut state, &mut dispatcher, representation, 0, &[]).unwrap();
        assert_eq!(
            resolved.rref(&state).committed_representation().alpha_mode,
            Some(crate::color::AlphaMode::Straight)
        );
        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(resolved.rref(&state).committed_representation().alpha_mode, None);

        let representation = WpColorRepresentationSurfaceV1Id::from(9);
        let surface = WlSurfaceId::from(7);
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        let resolved = client.resolve(surface, &state).unwrap();
        client.insert_object(
            &mut state,
            WpColorRepresentationSurfaceV1::new(representation, resolved),
        );
        dispatch(&mut client, &mut state, &mut dispatcher, surface, 0, &[]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, representation, 1, &[0]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((
                u32::from(representation),
                wp_color_representation_surface_v1::Error::Inert as u32
            ))
        );
    }

    #[test]
    fn a_synchronized_subsurface_applies_representation_when_the_parent_commits() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(4);
        let parent = WlSurfaceId::from(10);
        let child = WlSurfaceId::from(11);
        let (parent_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(parent, client.client_id(), 6));
        state.register_root_surface(parent_resolved);
        let (child_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(child, client.client_id(), 6));
        let (subsurface, _) = client.insert_object(
            &mut state,
            WlSubsurface::new(WlSubsurfaceId::from(12), child_resolved, parent_resolved),
        );
        child_resolved
            .grab(&mut state)
            .assign_subsurface(subsurface, parent_resolved);
        parent_resolved
            .grab(&mut state)
            .add_pending_child(child_resolved);

        let representation = WpColorRepresentationSurfaceV1Id::from(13);
        client.insert_object(
            &mut state,
            WpColorRepresentationSurfaceV1::new(representation, child_resolved),
        );
        child_resolved
            .grab(&mut state)
            .set_color_representation_object(client.resolve(representation, &state).unwrap())
            .unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, representation, 1, &[2]).unwrap();
        commit(&mut client, &mut state, &mut dispatcher, child);
        assert_eq!(
            child_resolved
                .rref(&state)
                .committed_representation()
                .alpha_mode,
            None
        );
        commit(&mut client, &mut state, &mut dispatcher, parent);
        assert_eq!(
            child_resolved
                .rref(&state)
                .committed_representation()
                .alpha_mode,
            Some(crate::color::AlphaMode::Straight)
        );
    }
}
