use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest, Resolved},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        ext_background_effect_manager_v1, Constants, ExtBackgroundEffectManagerV1Id,
        ExtBackgroundEffectSurfaceV1Id, NewId, WaylandId,
    },
    state::{GrabbedObject, State},
    wayland_objects::{
        ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1, wl_display::WlDisplay,
        wl_surface::WlSurface,
    },
};

/// `Blur` while `effects.blur.enabled` is set, otherwise none.
///
/// An effect object is accepted either way. The region is stored, and
/// `State::root_surface_blur_region` returns nothing while the flag is off,
/// so the compositor draws no client blur.
pub(crate) fn background_effect_capabilities(state: &State) -> u32 {
    if state.config.effects.blur.enabled {
        ext_background_effect_manager_v1::Capability::Blur as u32
    } else {
        0
    }
}

#[derive(Debug)]
pub struct ExtBackgroundEffectManagerV1Global;

impl ExtBackgroundEffectManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for ExtBackgroundEffectManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (
            ExtBackgroundEffectManagerV1::INTERFACE_NAME,
            ExtBackgroundEffectManagerV1::INTERFACE_VERSION,
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
        let id = ExtBackgroundEffectManagerV1Id::from(id);
        let (manager, _) = client.insert_object(state, ExtBackgroundEffectManagerV1 { id });
        let manager = manager.grab(state);
        manager.send_event(
            ext_background_effect_manager_v1::Event::Capabilities {
                flags: background_effect_capabilities(state),
            },
            dispatcher,
        );
    }
}

#[derive(Debug)]
pub struct ExtBackgroundEffectManagerV1 {
    id: ExtBackgroundEffectManagerV1Id,
}

impl Object for ExtBackgroundEffectManagerV1 {
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
        use ext_background_effect_manager_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetBackgroundEffect { id, surface } => {
                let surface = client.resolve_or_disconnect(surface, state, dispatcher)?;
                {
                    let surface = surface.rref(state);
                    if surface.has_background_effect_object() {
                        client.send_protocol_error(
                            dispatcher,
                            self.id.into(),
                            ext_background_effect_manager_v1::Error::BackgroundEffectExists as u32,
                            "wl_surface already has an ext_background_effect_surface_v1 object",
                        );
                        return Ok(());
                    }
                }
                let (effect, _) =
                    client.insert_object(state, ExtBackgroundEffectSurfaceV1::new(id, surface));
                surface
                    .grab(state)
                    .set_background_effect_object(effect)
                    .expect("background effect existence was checked before insertion");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::{Client, RawEvent, Resolved},
        globals::Globals,
        io_uring::model::IoUringAction,
        object::{IdToObject, Object, RequestResult},
        protocol_generated::{
            ext_background_effect_surface_v1, ExtBackgroundEffectManagerV1Id,
            ExtBackgroundEffectSurfaceV1Id, NewId, WaylandId, WlRegionId, WlSurfaceId,
        },
        runner::Model,
        test_support::{state_for_tests, test_client_and_dispatcher},
        wayland_objects::{wl_region::WlRegion, wl_surface::WlSurface},
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
        dispatcher: &mut crate::runner::Dispatcher,
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

    fn display_errors(dispatcher: &crate::runner::Dispatcher) -> Vec<(u32, u32)> {
        let mut found = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            let mut offset = 0;
            while offset + 16 <= buffer.len() {
                let object = u32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap());
                let header = u32::from_ne_bytes(buffer[offset + 4..offset + 8].try_into().unwrap());
                let size = (header >> 16) as usize;
                if size < 8 || offset + size > buffer.len() {
                    break;
                }
                if object == 1 && size >= 16 {
                    let target =
                        u32::from_ne_bytes(buffer[offset + 8..offset + 12].try_into().unwrap());
                    let code =
                        u32::from_ne_bytes(buffer[offset + 12..offset + 16].try_into().unwrap());
                    found.push((target, code));
                }
                offset += size;
            }
        });
        found
    }

    fn closed(dispatcher: &crate::runner::Dispatcher, client: &Client) -> bool {
        dispatcher.test_has_queued_action::<crate::clients::ClientManagerAction>(|action| {
            matches!(
                action,
                crate::clients::ClientManagerAction::Close { id } if *id == client.client_id()
            )
        })
    }

    fn capability_flags(dispatcher: &crate::runner::Dispatcher, manager: u32) -> Vec<u32> {
        let mut flags = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            let mut offset = 0;
            while offset + 12 <= buffer.len() {
                let object = u32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap());
                let header = u32::from_ne_bytes(buffer[offset + 4..offset + 8].try_into().unwrap());
                let size = (header >> 16) as usize;
                let opcode = header as u16;
                if size < 8 || offset + size > buffer.len() {
                    break;
                }
                if object == manager && opcode == 0 && size >= 12 {
                    flags.push(u32::from_ne_bytes(
                        buffer[offset + 8..offset + 12].try_into().unwrap(),
                    ));
                }
                offset += size;
            }
        });
        flags
    }

    fn commit(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut crate::runner::Dispatcher,
        surface: WlSurfaceId,
    ) {
        dispatch(client, state, dispatcher, surface, 6, &[]).unwrap();
    }

    #[test]
    fn bind_sends_no_blur_capability_when_blur_is_disabled() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(ExtBackgroundEffectManagerV1::INTERFACE_NAME));

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut global = ExtBackgroundEffectManagerV1Global;
            global.bind(&mut client, &mut state, NewId::from(6), 1, &mut client_dispatcher);
        }
        assert!(client.contains_object(ExtBackgroundEffectManagerV1Id::from(6), &state));
        assert_eq!(background_effect_capabilities(&state), 0);
        assert_eq!(capability_flags(&dispatcher, 6), vec![0]);
    }

    #[test]
    fn enabling_blur_advertises_the_capability_and_a_reload_resends_it() {
        let mut globals = Globals::new();
        globals.initialize();
        let mut state = state_for_tests();
        state.config.effects.blur.enabled = true;
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut global = ExtBackgroundEffectManagerV1Global;
            global.bind(&mut client, &mut state, NewId::from(6), 1, &mut client_dispatcher);
        }
        assert_eq!(
            background_effect_capabilities(&state),
            ext_background_effect_manager_v1::Capability::Blur as u32
        );
        assert_eq!(capability_flags(&dispatcher, 6), vec![1]);

        state.config.effects.blur.enabled = false;
        client
            .process(
                &mut state,
                crate::clients::ClientAction::BackgroundEffectCapabilitiesChanged,
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(capability_flags(&dispatcher, 6), vec![1, 0]);
    }

    #[test]
    fn a_second_background_effect_on_the_same_surface_is_rejected() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let manager = ExtBackgroundEffectManagerV1Id::from(3);
        let surface = WlSurfaceId::from(4);
        client.insert_object(&mut state, ExtBackgroundEffectManagerV1 { id: manager });
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));

        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[5, surface.into()])
            .unwrap();
        assert!(client.contains_object(ExtBackgroundEffectSurfaceV1Id::from(5), &state));
        assert!(client
            .resolve(surface, &state)
            .unwrap()
            .rref(&state)
            .has_background_effect_object());

        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[6, surface.into()])
            .unwrap();
        assert!(!client.contains_object(ExtBackgroundEffectSurfaceV1Id::from(6), &state));
        assert_eq!(
            display_errors(&dispatcher),
            vec![(
                u32::from(manager),
                ext_background_effect_manager_v1::Error::BackgroundEffectExists as u32
            )]
        );
        assert!(!closed(&dispatcher, &client));
    }

    #[test]
    fn the_blur_region_stays_pending_until_commit() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(3);
        let surface = WlSurfaceId::from(4);
        let effect = ExtBackgroundEffectSurfaceV1Id::from(5);
        let region = WlRegionId::from(8);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        client
            .insert_object(&mut state, ExtBackgroundEffectSurfaceV1::new(effect, surface_resolved));
        surface_resolved
            .grab(&mut state)
            .set_background_effect_object(client.resolve(effect, &state).unwrap())
            .unwrap();
        client.insert_object(&mut state, WlRegion::new(region));
        dispatch(&mut client, &mut state, &mut dispatcher, region, 1, &[0, 0, 10, 20]).unwrap();

        assert_eq!(surface_resolved.rref(&state).committed_blur_region(), None);
        dispatch(&mut client, &mut state, &mut dispatcher, effect, 1, &[u32::from(region)])
            .unwrap();
        assert_eq!(surface_resolved.rref(&state).committed_blur_region(), None);

        commit(&mut client, &mut state, &mut dispatcher, surface);
        let committed = surface_resolved.rref(&state).committed_blur_region();
        assert_eq!(
            committed,
            Some(
                client
                    .resolve(region, &state)
                    .unwrap()
                    .rref(&state)
                    .snapshot()
            )
        );
    }

    #[test]
    fn the_blur_region_is_copied_so_the_wl_region_can_be_destroyed() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(4);
        let surface = WlSurfaceId::from(4);
        let effect = ExtBackgroundEffectSurfaceV1Id::from(5);
        let region = WlRegionId::from(8);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        client
            .insert_object(&mut state, ExtBackgroundEffectSurfaceV1::new(effect, surface_resolved));
        surface_resolved
            .grab(&mut state)
            .set_background_effect_object(client.resolve(effect, &state).unwrap())
            .unwrap();
        client.insert_object(&mut state, WlRegion::new(region));
        dispatch(&mut client, &mut state, &mut dispatcher, region, 1, &[1, 2, 3, 4]).unwrap();
        let expected = client
            .resolve(region, &state)
            .unwrap()
            .rref(&state)
            .snapshot();

        dispatch(&mut client, &mut state, &mut dispatcher, effect, 1, &[u32::from(region)])
            .unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, region, 0, &[]).unwrap();
        assert!(!client.contains_object(region, &state));
        assert_eq!(surface_resolved.rref(&state).committed_blur_region(), None);

        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(surface_resolved.rref(&state).committed_blur_region(), Some(expected));
    }

    #[test]
    fn destroy_clears_the_blur_region_on_the_next_commit() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(5);
        let surface = WlSurfaceId::from(4);
        let effect = ExtBackgroundEffectSurfaceV1Id::from(5);
        let region = WlRegionId::from(8);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        client
            .insert_object(&mut state, ExtBackgroundEffectSurfaceV1::new(effect, surface_resolved));
        surface_resolved
            .grab(&mut state)
            .set_background_effect_object(client.resolve(effect, &state).unwrap())
            .unwrap();
        client.insert_object(&mut state, WlRegion::new(region));
        dispatch(&mut client, &mut state, &mut dispatcher, region, 1, &[0, 0, 8, 8]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, effect, 1, &[u32::from(region)])
            .unwrap();
        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert!(surface_resolved
            .rref(&state)
            .committed_blur_region()
            .is_some());

        dispatch(&mut client, &mut state, &mut dispatcher, effect, 0, &[]).unwrap();
        assert!(!client.contains_object(effect, &state));
        assert!(!surface_resolved.rref(&state).has_background_effect_object());
        assert!(surface_resolved
            .rref(&state)
            .committed_blur_region()
            .is_some());

        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(surface_resolved.rref(&state).committed_blur_region(), None);
    }

    #[test]
    fn set_blur_region_after_the_surface_is_destroyed_raises_surface_destroyed() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(6);
        let surface = WlSurfaceId::from(4);
        let effect = ExtBackgroundEffectSurfaceV1Id::from(5);
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        let surface_resolved = client.resolve(surface, &state).unwrap();
        client
            .insert_object(&mut state, ExtBackgroundEffectSurfaceV1::new(effect, surface_resolved));
        surface_resolved
            .grab(&mut state)
            .set_background_effect_object(client.resolve(effect, &state).unwrap())
            .unwrap();

        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let mut surface_object = client.grab_object(surface, &mut state);
            surface_object.delete_self(&mut client, &mut state, &mut client_dispatcher);
        }
        assert!(!client.contains_object(surface, &state));
        assert!(client.contains_object(effect, &state));

        dispatch(&mut client, &mut state, &mut dispatcher, effect, 1, &[0]).unwrap();
        assert_eq!(
            display_errors(&dispatcher),
            vec![(
                u32::from(effect),
                ext_background_effect_surface_v1::Error::SurfaceDestroyed as u32
            )]
        );
        assert!(!closed(&dispatcher, &client));
        assert!(client.contains_object(effect, &state));

        dispatch(&mut client, &mut state, &mut dispatcher, effect, 0, &[]).unwrap();
        assert!(!client.contains_object(effect, &state));
        assert_eq!(display_errors(&dispatcher).len(), 1);
    }

    #[test]
    fn client_disconnect_clears_the_blur_region_on_the_next_commit() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(7);
        let surface = WlSurfaceId::from(4);
        let effect = ExtBackgroundEffectSurfaceV1Id::from(5);
        let region = WlRegionId::from(8);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        client
            .insert_object(&mut state, ExtBackgroundEffectSurfaceV1::new(effect, surface_resolved));
        surface_resolved
            .grab(&mut state)
            .set_background_effect_object(client.resolve(effect, &state).unwrap())
            .unwrap();
        client.insert_object(&mut state, WlRegion::new(region));
        dispatch(&mut client, &mut state, &mut dispatcher, region, 1, &[0, 0, 2, 2]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, effect, 1, &[u32::from(region)])
            .unwrap();
        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert!(surface_resolved
            .rref(&state)
            .committed_blur_region()
            .is_some());

        {
            let mut effect_object = client.grab_object(effect, &mut state);
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            effect_object.on_client_disconnect(&mut client, &mut state, &mut client_dispatcher);
        }
        assert!(!surface_resolved.rref(&state).has_background_effect_object());
        assert!(surface_resolved
            .rref(&state)
            .committed_blur_region()
            .is_some());
        commit(&mut client, &mut state, &mut dispatcher, surface);
        assert_eq!(surface_resolved.rref(&state).committed_blur_region(), None);
    }
}
