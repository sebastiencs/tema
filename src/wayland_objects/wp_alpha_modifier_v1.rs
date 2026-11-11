use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{wp_alpha_modifier_v1, Constants, NewId, WaylandId, WpAlphaModifierV1Id},
    state::{GrabbedObject, State},
    wayland_objects::{
        wl_display::WlDisplay, wl_surface::WlSurface,
        wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1,
    },
};

#[derive(Debug)]
pub struct WpAlphaModifierV1Global;

impl WpAlphaModifierV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for WpAlphaModifierV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (WpAlphaModifierV1::INTERFACE_NAME, WpAlphaModifierV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(state, WpAlphaModifierV1 { id: id.into() });
    }
}

#[derive(Debug)]
pub struct WpAlphaModifierV1 {
    id: WpAlphaModifierV1Id,
}

impl Object for WpAlphaModifierV1 {
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
        use wp_alpha_modifier_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetSurface { id, surface } => {
                let surface = client.resolve_or_disconnect(surface, state, dispatcher)?;
                {
                    let surface = surface.rref(state);
                    if surface.has_alpha_modifier_object() {
                        client.send_protocol_error(
                            dispatcher,
                            self.id.into(),
                            wp_alpha_modifier_v1::Error::AlreadyConstructed as u32,
                            "wl_surface already has a wp_alpha_modifier_surface_v1 object",
                        );
                        return Ok(());
                    }
                }
                let (modifier, _) =
                    client.insert_object(state, WpAlphaModifierSurfaceV1::new(id, surface));
                surface
                    .grab(state)
                    .set_alpha_modifier_object(modifier)
                    .expect("alpha modifier existence was checked before insertion");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::{Client, ClientId, RawEvent, Resolved},
        drm::ARGB8888,
        globals::Globals,
        io_uring::model::IoUringAction,
        object::{IdToObject, Object, RequestResult},
        protocol_generated::{
            wp_alpha_modifier_surface_v1, NewId, WaylandId, WlBufferId, WlSubsurfaceId,
            WlSurfaceId, WpAlphaModifierSurfaceV1Id, WpAlphaModifierV1Id,
        },
        region::RectI,
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
        state: &mut State,
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

    fn closed(dispatcher: &Dispatcher, client: &Client) -> bool {
        dispatcher.test_has_queued_action::<crate::clients::ClientManagerAction>(|action| {
            matches!(action, crate::clients::ClientManagerAction::Close { id } if *id == client.client_id())
        })
    }

    fn insert_buffer(client: &mut Client, state: &mut State, id: u32, size: i32) {
        let bytes = vec![0u8; (size * size * 4) as usize];
        client.insert_object(
            state,
            WlBuffer::new(NewWlBufferParams {
                id: WlBufferId::from(id),
                owner_client_id: client.client_id(),
                offset: 0,
                width: size,
                height: size,
                stride: size.saturating_mul(4),
                format: &ARGB8888,
                mem: shared_memory_from_bytes(&bytes),
            })
            .unwrap(),
        );
    }

    fn map_surface(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        surface: WlSurfaceId,
        buffer: u32,
        size: i32,
    ) {
        insert_buffer(client, state, buffer, size);
        dispatch(client, state, dispatcher, surface, 1, &[buffer, 0, 0]).unwrap();
        dispatch(client, state, dispatcher, surface, 6, &[]).unwrap();
    }

    #[test]
    fn the_global_is_advertised_at_version_one() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(WpAlphaModifierV1::INTERFACE_NAME));

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let mut global = WpAlphaModifierV1Global;
        global.bind(&mut client, &mut state, NewId::from(6), 1, &mut client_dispatcher);
        assert!(client.contains_object(WpAlphaModifierV1Id::from(6), &state));
        assert_eq!(WpAlphaModifierV1Global.interface(), ("wp_alpha_modifier_v1", 1));
    }

    #[test]
    fn a_second_modifier_on_the_same_surface_is_rejected() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let manager = WpAlphaModifierV1Id::from(3);
        let surface = WlSurfaceId::from(4);
        client.insert_object(&mut state, WpAlphaModifierV1 { id: manager });
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));

        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[5, surface.into()])
            .unwrap();
        assert!(client.contains_object(WpAlphaModifierSurfaceV1Id::from(5), &state));
        assert!(client
            .resolve(surface, &state)
            .unwrap()
            .rref(&state)
            .has_alpha_modifier_object());

        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[6, surface.into()])
            .unwrap();
        assert!(!client.contains_object(WpAlphaModifierSurfaceV1Id::from(6), &state));
        assert_eq!(
            display_error(&dispatcher),
            Some((u32::from(manager), wp_alpha_modifier_v1::Error::AlreadyConstructed as u32))
        );
        assert!(!closed(&dispatcher, &client));
    }

    #[test]
    fn the_multiplier_applies_on_commit_and_damages_the_whole_surface() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(4);
        let surface = WlSurfaceId::from(4);
        let modifier = WpAlphaModifierSurfaceV1Id::from(5);
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        let surface_resolved = client.resolve(surface, &state).unwrap();
        client.insert_object(&mut state, WpAlphaModifierSurfaceV1::new(modifier, surface_resolved));
        surface_resolved
            .grab(&mut state)
            .set_alpha_modifier_object(client.resolve(modifier, &state).unwrap())
            .unwrap();
        map_surface(&mut client, &mut state, &mut dispatcher, surface, 20, 8);
        surface_resolved
            .grab(&mut state)
            .take_render_state_at(surface_resolved, 0, 0)
            .unwrap();

        assert_eq!(surface_resolved.rref(&state).committed_alpha_multiplier(), u32::MAX);
        dispatch(&mut client, &mut state, &mut dispatcher, modifier, 1, &[1]).unwrap();
        assert_eq!(surface_resolved.rref(&state).committed_alpha_multiplier(), u32::MAX);
        assert!(surface_resolved
            .rref(&state)
            .render_state()
            .unwrap()
            .damage
            .rects()
            .is_empty());

        dispatch(&mut client, &mut state, &mut dispatcher, surface, 6, &[]).unwrap();
        let render = surface_resolved.rref(&state).render_state().unwrap();
        assert_eq!(render.alpha_multiplier, 1);
        assert_eq!(render.damage.rects(), &[RectI::new(0, 0, 8, 8).unwrap()]);
        assert_eq!(surface_resolved.rref(&state).committed_alpha_multiplier(), 1);
    }

    #[test]
    fn destroy_reverts_to_opaque_on_the_next_commit() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(6);
        let manager = WpAlphaModifierV1Id::from(3);
        let surface = WlSurfaceId::from(4);
        let modifier = WpAlphaModifierSurfaceV1Id::from(5);
        client.insert_object(&mut state, WpAlphaModifierV1 { id: manager });
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            manager,
            1,
            &[u32::from(modifier), u32::from(surface)],
        )
        .unwrap();
        map_surface(&mut client, &mut state, &mut dispatcher, surface, 21, 4);
        dispatch(&mut client, &mut state, &mut dispatcher, modifier, 1, &[7]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, surface, 6, &[]).unwrap();
        let surface_resolved = client.resolve(surface, &state).unwrap();
        assert_eq!(
            surface_resolved
                .rref(&state)
                .render_state()
                .unwrap()
                .alpha_multiplier,
            7
        );

        dispatch(&mut client, &mut state, &mut dispatcher, modifier, 0, &[]).unwrap();
        assert!(!client.contains_object(modifier, &state));
        assert!(!surface_resolved.rref(&state).has_alpha_modifier_object());
        assert_eq!(surface_resolved.rref(&state).committed_alpha_multiplier(), 7);
        dispatch(&mut client, &mut state, &mut dispatcher, surface, 6, &[]).unwrap();
        assert_eq!(
            surface_resolved
                .rref(&state)
                .render_state()
                .unwrap()
                .alpha_multiplier,
            u32::MAX
        );

        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[7, u32::from(surface)])
            .unwrap();
        assert!(client.contains_object(WpAlphaModifierSurfaceV1Id::from(7), &state));
    }

    #[test]
    fn a_synchronized_subsurface_applies_the_multiplier_when_the_parent_commits() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let parent = WlSurfaceId::from(4);
        let child = WlSurfaceId::from(5);
        let modifier = WpAlphaModifierSurfaceV1Id::from(6);
        let (parent_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(parent, client.client_id(), 6));
        state.register_root_surface(parent_resolved);
        let (child_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(child, client.client_id(), 6));
        let (subsurface, _) = client.insert_object(
            &mut state,
            WlSubsurface::new(WlSubsurfaceId::from(8), child_resolved, parent_resolved),
        );
        child_resolved
            .grab(&mut state)
            .assign_subsurface(subsurface, parent_resolved);
        parent_resolved
            .grab(&mut state)
            .add_pending_child(child_resolved);
        client.insert_object(&mut state, WpAlphaModifierSurfaceV1::new(modifier, child_resolved));
        child_resolved
            .grab(&mut state)
            .set_alpha_modifier_object(client.resolve(modifier, &state).unwrap())
            .unwrap();

        dispatch(&mut client, &mut state, &mut dispatcher, modifier, 1, &[9]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, child, 6, &[]).unwrap();
        assert_eq!(child_resolved.rref(&state).committed_alpha_multiplier(), u32::MAX);
        dispatch(&mut client, &mut state, &mut dispatcher, parent, 6, &[]).unwrap();
        assert_eq!(child_resolved.rref(&state).committed_alpha_multiplier(), 9);
    }

    #[test]
    fn set_multiplier_after_the_surface_is_destroyed_raises_no_surface() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(9);
        let surface = WlSurfaceId::from(4);
        let modifier = WpAlphaModifierSurfaceV1Id::from(5);
        client.insert_object(&mut state, WlSurface::new(surface, ClientId::from_raw(9), 6));
        let surface_resolved = client.resolve(surface, &state).unwrap();
        client.insert_object(&mut state, WpAlphaModifierSurfaceV1::new(modifier, surface_resolved));
        surface_resolved
            .grab(&mut state)
            .set_alpha_modifier_object(client.resolve(modifier, &state).unwrap())
            .unwrap();

        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface_object = client.grab_object(surface, &mut state);
            surface_object.delete_self(&mut client, &mut state, &mut client_dispatcher);
        }
        assert!(!client.contains_object(surface, &state));
        assert!(client.contains_object(modifier, &state));
        dispatch(&mut client, &mut state, &mut dispatcher, modifier, 1, &[3]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((u32::from(modifier), wp_alpha_modifier_surface_v1::Error::NoSurface as u32))
        );
        assert!(!closed(&dispatcher, &client));
        assert!(client.contains_object(modifier, &state));

        dispatch(&mut client, &mut state, &mut dispatcher, modifier, 0, &[]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((u32::from(modifier), wp_alpha_modifier_surface_v1::Error::NoSurface as u32))
        );
        assert!(client.contains_object(modifier, &state));
    }

    #[test]
    fn client_disconnect_clears_the_surface_link() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(8);
        let surface = WlSurfaceId::from(4);
        let modifier = WpAlphaModifierSurfaceV1Id::from(5);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        client.insert_object(&mut state, WpAlphaModifierSurfaceV1::new(modifier, surface_resolved));
        let modifier_resolved = client.resolve(modifier, &state).unwrap();
        surface_resolved
            .grab(&mut state)
            .set_alpha_modifier_object(modifier_resolved)
            .unwrap();
        surface_resolved
            .grab(&mut state)
            .set_pending_alpha_multiplier(4);

        {
            let mut modifier_object = client.grab_object(modifier, &mut state);
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            modifier_object.on_client_disconnect(&mut client, &mut state, &mut client_dispatcher);
        }
        assert!(!surface_resolved.rref(&state).has_alpha_modifier_object());
        dispatch(&mut client, &mut state, &mut dispatcher, surface, 6, &[]).unwrap();
        assert_eq!(surface_resolved.rref(&state).committed_alpha_multiplier(), u32::MAX);
    }
}
