use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    object::{Object, Parser, RequestResult},
    protocol_generated::{xwayland_surface_v1, WaylandId, WlSurfaceId, XwaylandSurfaceV1Id},
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

#[derive(Debug)]
pub struct XwaylandSurfaceV1 {
    id: XwaylandSurfaceV1Id,
    surface: Resolved<WlSurfaceId>,
}

impl XwaylandSurfaceV1 {
    pub(crate) fn new(id: XwaylandSurfaceV1Id, surface: Resolved<WlSurfaceId>) -> Self {
        Self { id, surface }
    }
}

impl Object for XwaylandSurfaceV1 {
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
        use xwayland_surface_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            SetSerial { serial_lo, serial_hi } => {
                let serial = u64::from(serial_lo) | (u64::from(serial_hi) << 32);
                let Some(mut surface) = self.surface.try_ref_mut(state) else {
                    return Ok(());
                };
                if surface.latest_xwayland_serial().is_some() {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        xwayland_surface_v1::Error::AlreadyAssociated as u32,
                        "wl_surface is already associated with an X11 window",
                    );
                    return Ok(());
                }
                if serial <= client.last_xwayland_serial() {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        xwayland_surface_v1::Error::InvalidSerial as u32,
                        "xwayland serial is zero or not strictly increasing",
                    );
                    return Ok(());
                }
                client.set_last_xwayland_serial(serial);
                surface.set_pending_xwayland_serial(serial);
            }
            Destroy => {
                let this = client.resolve_this(&self, state);
                if let Some(mut surface) = self.surface.try_ref_mut(state) {
                    surface.clear_xwayland_surface_object(this);
                }
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
            clients::{Client, ClientId, ClientManagerAction, RawEvent},
            protocol_generated::{WaylandId, WlSurfaceId},
            runner::Dispatcher,
            state::State,
            test_support::state_for_tests,
            wayland_objects::wl_surface::WlSurface,
        },
    };

    fn set_serial(object: u32, serial_lo: u32, serial_hi: u32) -> RawEvent {
        let mut event = RawEvent::new(WaylandId::from(object), 0, 2);
        event.uint(serial_lo);
        event.uint(serial_hi);
        event
    }

    fn dispatch(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        event: RawEvent,
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(XwaylandSurfaceV1Id::from(5).into());
        client.grab_object(XwaylandSurfaceV1Id::from(5), state).on(
            client,
            state,
            (&*event.into_bytes()).try_into().unwrap(),
            &mut client_dispatcher,
        );
    }

    fn closed(dispatcher: &Dispatcher, client: &Client) -> bool {
        dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == client.client_id())
        })
    }

    struct Fixture {
        client: Client,
        state: State,
        dispatcher: Dispatcher,
        surface: Resolved<WlSurfaceId>,
    }

    fn fixture(client_id: u64) -> Fixture {
        let mut state = state_for_tests();
        let mut client = Client::new_xwayland_for_tests(ClientId::from_raw(client_id));
        let surface = client
            .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(4), client.client_id(), 6))
            .0;
        assert!(surface.grab(&mut state).assign_xwayland_role());
        let (xsurface, _) = client.insert_object(
            &mut state,
            XwaylandSurfaceV1::new(XwaylandSurfaceV1Id::from(5), surface),
        );
        surface
            .grab(&mut state)
            .set_xwayland_surface_object(xsurface);
        Fixture { client, state, dispatcher: Dispatcher::default(), surface }
    }

    fn pending(fixture: &Fixture) -> Option<u64> {
        fixture
            .surface
            .rref(&fixture.state)
            .pending_xwayland_serial()
    }

    #[test]
    fn set_serial_combines_hi_and_lo_and_stays_pending() {
        let mut fixture = fixture(4);
        let serial = u64::from(0x11u32) | (u64::from(0x22u32) << 32);
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            set_serial(5, 0x11, 0x22),
        );

        assert_eq!(fixture.client.last_xwayland_serial(), serial);
        assert_eq!(pending(&fixture), Some(serial));
        assert!(fixture
            .surface
            .rref(&fixture.state)
            .xwayland_serial()
            .is_none());
        assert!(!closed(&fixture.dispatcher, &fixture.client));
    }

    #[test]
    fn a_zero_serial_is_rejected() {
        let mut fixture = fixture(4);
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            set_serial(5, 0, 0),
        );

        assert!(closed(&fixture.dispatcher, &fixture.client));
        assert_eq!(fixture.client.last_xwayland_serial(), 0);
        assert!(pending(&fixture).is_none());
    }

    #[test]
    fn an_equal_or_lower_serial_is_rejected() {
        let mut fixture = fixture(4);
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            set_serial(5, 5, 0),
        );
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            set_serial(5, 5, 0),
        );
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            set_serial(5, 4, 0),
        );

        assert!(closed(&fixture.dispatcher, &fixture.client));
        assert_eq!(fixture.client.last_xwayland_serial(), 5);
        assert_eq!(pending(&fixture), Some(5));
        assert!(fixture
            .surface
            .rref(&fixture.state)
            .xwayland_serial()
            .is_none());
    }

    #[test]
    fn a_later_set_serial_replaces_the_pending_one() {
        let mut fixture = fixture(4);
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            set_serial(5, 1, 0),
        );
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            set_serial(5, 3, 0),
        );

        assert!(!closed(&fixture.dispatcher, &fixture.client));
        assert_eq!(fixture.client.last_xwayland_serial(), 3);
        assert_eq!(pending(&fixture), Some(3));
        assert!(fixture
            .surface
            .rref(&fixture.state)
            .xwayland_serial()
            .is_none());
    }

    #[test]
    fn the_serial_counter_is_per_client() {
        let mut first = fixture(4);
        let mut second = fixture(8);
        dispatch(&mut first.client, &mut first.state, &mut first.dispatcher, set_serial(5, 1, 0));
        dispatch(
            &mut second.client,
            &mut second.state,
            &mut second.dispatcher,
            set_serial(5, 1, 0),
        );

        assert!(!closed(&first.dispatcher, &first.client));
        assert!(!closed(&second.dispatcher, &second.client));
        assert_eq!(first.client.last_xwayland_serial(), 1);
        assert_eq!(second.client.last_xwayland_serial(), 1);
        assert_eq!(pending(&first), Some(1));
        assert_eq!(pending(&second), Some(1));
    }

    #[test]
    fn set_serial_on_a_missing_surface_does_nothing() {
        let mut fixture = fixture(4);
        fixture.state.remove_object(fixture.surface.object_id());
        dispatch(
            &mut fixture.client,
            &mut fixture.state,
            &mut fixture.dispatcher,
            set_serial(5, 1, 0),
        );

        assert!(!closed(&fixture.dispatcher, &fixture.client));
        assert_eq!(fixture.client.last_xwayland_serial(), 0);
    }
}
