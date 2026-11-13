use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        wp_fifo_manager_v1, Constants, NewId, WaylandId, WpFifoManagerV1Id, WpFifoV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, wl_surface::WlSurface, wp_fifo_v1::WpFifoV1},
};

#[derive(Debug)]
pub struct WpFifoManagerV1Global;

impl WpFifoManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for WpFifoManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (WpFifoManagerV1::INTERFACE_NAME, WpFifoManagerV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(state, WpFifoManagerV1 { id: id.into() });
    }
}

#[derive(Debug)]
pub struct WpFifoManagerV1 {
    id: WpFifoManagerV1Id,
}

impl Object for WpFifoManagerV1 {
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
        use wp_fifo_manager_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetFifo { id, surface } => {
                let surface = client.resolve_or_disconnect(surface, state, dispatcher)?;
                if surface.rref(state).has_fifo_object() {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wp_fifo_manager_v1::Error::AlreadyExists as u32,
                        "wl_surface already has a wp_fifo_v1",
                    );
                    return Ok(());
                }
                let (fifo, _) = client.insert_object(state, WpFifoV1::new(id, surface));
                surface.ref_mut(state).set_fifo_object(fifo);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::{Client, ClientAction, RawEvent},
        globals::Globals,
        io_uring::model::IoUringAction,
        object::{IdToObject, Object, RequestResult},
        protocol_generated::{wp_fifo_v1, WaylandId, WlSurfaceId, WpFifoV1Id},
        runner::{Dispatcher, Model},
        state::{BackendOutputRef, DrmId},
        test_support::{state_for_tests, test_client_and_dispatcher, test_output},
        wayland_objects::wl_surface::WlSurface,
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

    #[test]
    fn the_fifo_global_is_advertised_at_version_one() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(WpFifoManagerV1::INTERFACE_NAME));

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let mut global = WpFifoManagerV1Global;
        global.bind(&mut client, &mut state, NewId::from(6), 1, &mut client_dispatcher);
        assert!(client.contains_object(WpFifoManagerV1Id::from(6), &state));
        assert_eq!(WpFifoManagerV1Global.interface(), ("wp_fifo_manager_v1", 1));
    }

    #[test]
    fn get_fifo_records_the_object_on_the_surface() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let manager = WpFifoManagerV1Id::from(3);
        let surface = WlSurfaceId::from(4);
        let fifo = WpFifoV1Id::from(5);
        client.insert_object(&mut state, WpFifoManagerV1 { id: manager });
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));

        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            manager,
            1,
            &[fifo.into(), surface.into()],
        )
        .unwrap();
        assert!(client.contains_object(fifo, &state));
        assert!(client
            .resolve(surface, &state)
            .unwrap()
            .rref(&state)
            .has_fifo_object());
        assert!(!closed(&dispatcher, &client));
    }

    #[test]
    fn a_second_get_fifo_is_already_exists() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let manager = WpFifoManagerV1Id::from(3);
        let surface = WlSurfaceId::from(4);
        client.insert_object(&mut state, WpFifoManagerV1 { id: manager });
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));

        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[5, surface.into()])
            .unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[6, surface.into()])
            .unwrap();
        assert!(!client.contains_object(WpFifoV1Id::from(6), &state));
        assert_eq!(
            display_error(&dispatcher),
            Some((u32::from(manager), wp_fifo_manager_v1::Error::AlreadyExists as u32))
        );
        assert!(closed(&dispatcher, &client));
    }

    #[test]
    fn get_fifo_after_destroy_is_allowed() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let manager = WpFifoManagerV1Id::from(3);
        let surface = WlSurfaceId::from(4);
        let fifo = WpFifoV1Id::from(5);
        client.insert_object(&mut state, WpFifoManagerV1 { id: manager });
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));

        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[5, surface.into()])
            .unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, fifo, 2, &[]).unwrap();
        assert!(!client
            .resolve(surface, &state)
            .unwrap()
            .rref(&state)
            .has_fifo_object());

        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[6, surface.into()])
            .unwrap();
        assert!(client.contains_object(WpFifoV1Id::from(6), &state));
        assert!(display_error(&dispatcher).is_none());
        assert!(!closed(&dispatcher, &client));
    }

    #[test]
    fn set_barrier_then_commit_sets_a_barrier() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let manager = WpFifoManagerV1Id::from(3);
        let surface = WlSurfaceId::from(4);
        let fifo = WpFifoV1Id::from(5);
        client.insert_object(&mut state, WpFifoManagerV1 { id: manager });
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[5, surface.into()])
            .unwrap();

        dispatch(&mut client, &mut state, &mut dispatcher, fifo, 0, &[]).unwrap();
        let surface_resolved = client.resolve(surface, &state).unwrap();
        assert!(!state.commit_gate.has_barrier(surface_resolved));
        dispatch(&mut client, &mut state, &mut dispatcher, surface, 6, &[]).unwrap();
        assert!(state.commit_gate.has_barrier(surface_resolved));
    }

    #[test]
    fn fifo_pacing_round_trip() {
        let mut state = state_for_tests();
        let out = state.insert_output(test_output(
            BackendOutputRef::new(DrmId::from(1usize)),
            1,
            0,
            0,
            1920,
            1080,
            "out",
        ));
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let manager = WpFifoManagerV1Id::from(3);
        let surface = WlSurfaceId::from(4);
        let fifo = WpFifoV1Id::from(5);
        client.insert_object(&mut state, WpFifoManagerV1 { id: manager });
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[5, surface.into()])
            .unwrap();
        let s = client.resolve(surface, &state).unwrap();
        s.ref_mut(&mut state).update_output_membership(vec![out]);

        // Frame A sets a barrier and applies at once.
        dispatch(&mut client, &mut state, &mut dispatcher, fifo, 0, &[]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, surface, 6, &[]).unwrap();
        let first = state.commit_gate.barrier(s).unwrap().seq;

        // Frame B waits for that barrier and sets a new one when it applies.
        dispatch(&mut client, &mut state, &mut dispatcher, fifo, 0, &[]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, fifo, 1, &[]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, surface, 6, &[]).unwrap();
        assert_eq!(s.rref(&state).test_queue_len(), 1);
        assert_eq!(state.commit_gate.barrier(s).unwrap().seq, first);

        let mut model = crate::commit_gate::CommitGateModel::new();
        model
            .process(
                &mut state,
                crate::commit_gate::CommitGateAction::OutputLatched {
                    output_id: out,
                    kind: crate::commit_gate::LatchKind::Hard,
                    barriers: vec![crate::commit_gate::NotedBarrier {
                        surface: s,
                        seq: first,
                        in_frame: true,
                    }],
                },
                &mut dispatcher,
            )
            .unwrap();
        client
            .process(&mut state, ClientAction::FlushCommitQueues, &mut dispatcher)
            .unwrap();
        assert_eq!(s.rref(&state).test_queue_len(), 0);
        assert_ne!(state.commit_gate.barrier(s).unwrap().seq, first);
    }

    #[test]
    fn destroying_the_fifo_keeps_pending_state() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let manager = WpFifoManagerV1Id::from(3);
        let surface = WlSurfaceId::from(4);
        let fifo = WpFifoV1Id::from(5);
        client.insert_object(&mut state, WpFifoManagerV1 { id: manager });
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[5, surface.into()])
            .unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, fifo, 0, &[]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, fifo, 2, &[]).unwrap();

        let surface_resolved = client.resolve(surface, &state).unwrap();
        assert!(!surface_resolved.rref(&state).has_fifo_object());
        dispatch(&mut client, &mut state, &mut dispatcher, surface, 6, &[]).unwrap();
        assert!(state.commit_gate.has_barrier(surface_resolved));
    }

    /// Destroys `surface` the way a client's `wl_surface.destroy` does.
    fn destroy_surface(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        surface: WlSurfaceId,
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        let mut surface_object = client.grab_object(surface, state);
        surface_object.delete_self(client, state, &mut client_dispatcher);
    }

    #[test]
    fn set_barrier_after_surface_destroy_is_surface_destroyed() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let surface = WlSurfaceId::from(4);
        let fifo = WpFifoV1Id::from(5);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        client.insert_object(&mut state, WpFifoV1::new(fifo, surface_resolved));
        let fifo_resolved = client.resolve(fifo, &state).unwrap();
        surface_resolved
            .grab(&mut state)
            .set_fifo_object(fifo_resolved);

        destroy_surface(&mut client, &mut state, &mut dispatcher, surface);
        assert!(!client.contains_object(surface, &state));
        dispatch(&mut client, &mut state, &mut dispatcher, fifo, 0, &[]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((u32::from(fifo), wp_fifo_v1::Error::SurfaceDestroyed as u32))
        );
        assert!(closed(&dispatcher, &client));
    }

    #[test]
    fn wait_barrier_after_surface_destroy_is_surface_destroyed() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let surface = WlSurfaceId::from(4);
        let fifo = WpFifoV1Id::from(5);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        client.insert_object(&mut state, WpFifoV1::new(fifo, surface_resolved));
        let fifo_resolved = client.resolve(fifo, &state).unwrap();
        surface_resolved
            .grab(&mut state)
            .set_fifo_object(fifo_resolved);

        destroy_surface(&mut client, &mut state, &mut dispatcher, surface);
        dispatch(&mut client, &mut state, &mut dispatcher, fifo, 1, &[]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((u32::from(fifo), wp_fifo_v1::Error::SurfaceDestroyed as u32))
        );
        assert!(closed(&dispatcher, &client));
    }

    #[test]
    fn destroy_after_surface_destroy_is_not_an_error() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let surface = WlSurfaceId::from(4);
        let fifo = WpFifoV1Id::from(5);
        let (surface_resolved, _) =
            client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        client.insert_object(&mut state, WpFifoV1::new(fifo, surface_resolved));
        let fifo_resolved = client.resolve(fifo, &state).unwrap();
        surface_resolved
            .grab(&mut state)
            .set_fifo_object(fifo_resolved);

        destroy_surface(&mut client, &mut state, &mut dispatcher, surface);
        dispatch(&mut client, &mut state, &mut dispatcher, fifo, 2, &[]).unwrap();
        assert!(display_error(&dispatcher).is_none());
        assert!(!closed(&dispatcher, &client));
        assert!(!client.contains_object(fifo, &state));
    }
}
