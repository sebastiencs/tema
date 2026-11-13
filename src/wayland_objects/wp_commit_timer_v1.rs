use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    object::{Object, Parser, RequestResult},
    protocol_generated::{wp_commit_timer_v1, WaylandId, WlSurfaceId, WpCommitTimerV1Id},
    scheduler::presentation::monotonic_to_instant,
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, wl_surface::WlSurface},
};

#[derive(Debug)]
pub struct WpCommitTimerV1 {
    id: WpCommitTimerV1Id,
    /// `None` once the `wl_surface` is destroyed.
    surface: Option<Resolved<WlSurfaceId>>,
}

impl WpCommitTimerV1 {
    pub fn new(id: WpCommitTimerV1Id, surface: Resolved<WlSurfaceId>) -> Self {
        Self { id, surface: Some(surface) }
    }

    pub(crate) fn mark_surface_destroyed(&mut self) {
        self.surface = None;
    }

    /// The surface, while it is alive. Only this object is grabbed in its
    /// handlers, so the surface can be read.
    fn live_surface(&self, state: &State) -> Option<Resolved<WlSurfaceId>> {
        let surface = self.surface?;
        surface.exists(state).then_some(surface)
    }

    fn surface_destroyed(&self, client: &mut Client, dispatcher: &mut ClientDispatcher) {
        client.protocol_violation_disconnect(
            dispatcher,
            self.id.into(),
            wp_commit_timer_v1::Error::SurfaceDestroyed as u32,
            "wl_surface was destroyed",
        );
    }
}

impl Object for WpCommitTimerV1 {
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
        use wp_commit_timer_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            SetTimestamp { tv_sec_hi, tv_sec_lo, tv_nsec } => {
                let Some(surface) = self.live_surface(state) else {
                    self.surface_destroyed(client, dispatcher);
                    return Ok(());
                };
                if tv_nsec > 999_999_999 || tv_sec_hi > 0x7fff_ffff {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wp_commit_timer_v1::Error::InvalidTimestamp as u32,
                        "invalid timestamp",
                    );
                    return Ok(());
                }
                if surface.rref(state).has_pending_commit_target() {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wp_commit_timer_v1::Error::TimestampExists as u32,
                        "the commit already has a timestamp",
                    );
                    return Ok(());
                }
                let sec = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                surface
                    .ref_mut(state)
                    .set_pending_commit_target(monotonic_to_instant(sec, tv_nsec));
            }
            Destroy => {
                let this = client.resolve(self.id, state).unwrap();
                if let Some(surface) = self.live_surface(state) {
                    surface.ref_mut(state).clear_commit_timer_object(this);
                }
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
        }
        Ok(())
    }

    fn on_client_disconnect(
        &mut self,
        client: &mut Client,
        state: &mut State,
        _dispatcher: &mut ClientDispatcher,
    ) {
        let Some(object) = client.resolve(self.id, state) else {
            return;
        };
        if let Some(surface) = self.live_surface(state) {
            surface.ref_mut(state).clear_commit_timer_object(object);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        clients::{Client, RawEvent},
        globals::{Global, Globals},
        io_uring::model::IoUringAction,
        object::{IdToObject, Object, RequestResult},
        protocol_generated::{
            wp_commit_timing_manager_v1, Constants, NewId, WaylandId, WlSurfaceId,
            WpCommitTimingManagerV1Id,
        },
        runner::Dispatcher,
        test_support::{state_for_tests, test_client_and_dispatcher},
        wayland_objects::wp_commit_timing_manager_v1::{
            WpCommitTimingManagerV1, WpCommitTimingManagerV1Global,
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

    /// A client with a timing manager (id 3), a surface (id 4) and a timer (id 5).
    fn setup_timer(
    ) -> (State, Client, Dispatcher, WpCommitTimingManagerV1Id, WlSurfaceId, WpCommitTimerV1Id)
    {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(2);
        let manager = WpCommitTimingManagerV1Id::from(3);
        let surface = WlSurfaceId::from(4);
        let timer = WpCommitTimerV1Id::from(5);
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            WpCommitTimingManagerV1Global.bind(
                &mut client,
                &mut state,
                NewId::from(3),
                1,
                &mut client_dispatcher,
            );
        }
        client.insert_object(&mut state, WlSurface::new(surface, client.client_id(), 6));
        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[5, surface.into()])
            .unwrap();
        (state, client, dispatcher, manager, surface, timer)
    }

    #[test]
    fn the_timing_global_is_advertised_at_version_one() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(WpCommitTimingManagerV1::INTERFACE_NAME));

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let mut global = WpCommitTimingManagerV1Global;
        global.bind(&mut client, &mut state, NewId::from(6), 1, &mut client_dispatcher);
        assert!(client.contains_object(WpCommitTimingManagerV1Id::from(6), &state));
        assert_eq!(WpCommitTimingManagerV1Global.interface(), ("wp_commit_timing_manager_v1", 1));
    }

    #[test]
    fn get_timer_records_the_object_on_the_surface() {
        let (state, client, dispatcher, _, surface, timer) = setup_timer();
        assert!(client.contains_object(timer, &state));
        assert!(client
            .resolve(surface, &state)
            .unwrap()
            .rref(&state)
            .has_commit_timer_object());
        assert!(!closed(&dispatcher, &client));
    }

    #[test]
    fn a_second_get_timer_is_commit_timer_exists() {
        let (mut state, mut client, mut dispatcher, manager, surface, _) = setup_timer();
        dispatch(&mut client, &mut state, &mut dispatcher, manager, 1, &[6, surface.into()])
            .unwrap();
        assert!(!client.contains_object(WpCommitTimerV1Id::from(6), &state));
        assert_eq!(
            display_error(&dispatcher),
            Some((
                u32::from(manager),
                wp_commit_timing_manager_v1::Error::CommitTimerExists as u32
            ))
        );
        assert!(closed(&dispatcher, &client));
    }

    #[test]
    fn tv_nsec_of_one_second_is_invalid_timestamp() {
        let (mut state, mut client, mut dispatcher, _, _, timer) = setup_timer();
        dispatch(&mut client, &mut state, &mut dispatcher, timer, 0, &[0, 0, 1_000_000_000])
            .unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((u32::from(timer), wp_commit_timer_v1::Error::InvalidTimestamp as u32))
        );
        assert!(closed(&dispatcher, &client));
    }

    #[test]
    fn seconds_above_i64_max_are_invalid_timestamp() {
        let (mut state, mut client, mut dispatcher, _, _, timer) = setup_timer();
        dispatch(&mut client, &mut state, &mut dispatcher, timer, 0, &[0x8000_0000, 0, 0]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((u32::from(timer), wp_commit_timer_v1::Error::InvalidTimestamp as u32))
        );
        assert!(closed(&dispatcher, &client));
    }

    #[test]
    fn a_second_timestamp_before_commit_is_timestamp_exists() {
        let (mut state, mut client, mut dispatcher, _, _, timer) = setup_timer();
        dispatch(&mut client, &mut state, &mut dispatcher, timer, 0, &[0, 10, 0]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, timer, 0, &[0, 11, 0]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((u32::from(timer), wp_commit_timer_v1::Error::TimestampExists as u32))
        );
        assert!(closed(&dispatcher, &client));
    }

    #[test]
    fn a_timestamp_after_commit_is_accepted() {
        let (mut state, mut client, mut dispatcher, _, surface, timer) = setup_timer();
        dispatch(&mut client, &mut state, &mut dispatcher, timer, 0, &[0, 10, 0]).unwrap();
        let surface_resolved = client.resolve(surface, &state).unwrap();
        assert!(surface_resolved.rref(&state).has_pending_commit_target());
        dispatch(&mut client, &mut state, &mut dispatcher, surface, 6, &[]).unwrap();
        assert!(!surface_resolved.rref(&state).has_pending_commit_target());

        dispatch(&mut client, &mut state, &mut dispatcher, timer, 0, &[0, 11, 0]).unwrap();
        assert!(display_error(&dispatcher).is_none());
        assert!(!closed(&dispatcher, &client));
        assert!(surface_resolved.rref(&state).has_pending_commit_target());
    }

    #[test]
    fn set_timestamp_after_surface_destroy_is_surface_destroyed() {
        let (mut state, mut client, mut dispatcher, _, surface, timer) = setup_timer();
        destroy_surface(&mut client, &mut state, &mut dispatcher, surface);
        dispatch(&mut client, &mut state, &mut dispatcher, timer, 0, &[0, 10, 0]).unwrap();
        assert_eq!(
            display_error(&dispatcher),
            Some((u32::from(timer), wp_commit_timer_v1::Error::SurfaceDestroyed as u32))
        );
        assert!(closed(&dispatcher, &client));
    }

    #[test]
    fn destroying_the_timer_keeps_the_pending_timestamp() {
        let (mut state, mut client, mut dispatcher, _, surface, timer) = setup_timer();
        dispatch(&mut client, &mut state, &mut dispatcher, timer, 0, &[0, 10, 0]).unwrap();
        dispatch(&mut client, &mut state, &mut dispatcher, timer, 1, &[]).unwrap();

        let surface_resolved = client.resolve(surface, &state).unwrap();
        assert!(!surface_resolved.rref(&state).has_commit_timer_object());
        assert!(surface_resolved.rref(&state).has_pending_commit_target());
        assert!(display_error(&dispatcher).is_none());
    }
}
