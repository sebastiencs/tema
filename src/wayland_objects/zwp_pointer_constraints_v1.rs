use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    globals::Global,
    object::{AbortRequest, Object, Parser, RequestResult},
    protocol_generated::{
        wl_display, zwp_pointer_constraints_v1, Constants, NewId, WaylandId, WlPointerId,
        WlRegionId, WlSurfaceId, ZwpConfinedPointerV1Id, ZwpLockedPointerV1Id,
        ZwpPointerConstraintsV1Id,
    },
    region::RegionValue,
    state::{GrabbedObject, State},
    wayland_objects::{
        pointer_constraint::{constraint_lifetime, SurfacePointerConstraint},
        wl_display::WlDisplay,
        wl_surface::WlSurface,
        zwp_confined_pointer_v1::ZwpConfinedPointerV1,
        zwp_locked_pointer_v1::ZwpLockedPointerV1,
    },
};

#[derive(Debug)]
pub struct ZwpPointerConstraintsV1Global;

impl ZwpPointerConstraintsV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for ZwpPointerConstraintsV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (ZwpPointerConstraintsV1::INTERFACE_NAME, ZwpPointerConstraintsV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(
            state,
            ZwpPointerConstraintsV1 { id: ZwpPointerConstraintsV1Id::from(id) },
        );
    }
}

#[derive(Debug)]
pub struct ZwpPointerConstraintsV1 {
    id: ZwpPointerConstraintsV1Id,
}

impl ZwpPointerConstraintsV1 {
    fn snapshot_region(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        region: WlRegionId,
    ) -> Result<Option<RegionValue>, AbortRequest> {
        let Some(region) = region.to_opt() else {
            return Ok(None);
        };
        let region = client.resolve_or_disconnect(region, state, dispatcher)?;
        let snapshot = region.rref(state).snapshot();
        Ok(Some(dispatcher.wire_scale().region_to_logical_outward(snapshot)))
    }

    fn reject_constraint(
        &self,
        client: &mut Client,
        dispatcher: &mut ClientDispatcher,
        code: u32,
        message: &str,
    ) {
        client.protocol_violation_disconnect(dispatcher, self.id.into(), code, message);
    }
}

enum ConstraintRequest {
    Lock(ZwpLockedPointerV1Id),
    Confine(ZwpConfinedPointerV1Id),
}

impl Object for ZwpPointerConstraintsV1 {
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
        use zwp_pointer_constraints_v1::Request::*;

        let (request, surface_id, pointer_id, region_id, lifetime) =
            match client.parse::<Self>(req, dispatcher)? {
                Destroy => {
                    client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
                    return Ok(());
                }
                LockPointer { id, surface, pointer, region, lifetime } => {
                    (ConstraintRequest::Lock(id), surface, pointer, region, lifetime)
                }
                ConfinePointer { id, surface, pointer, region, lifetime } => {
                    (ConstraintRequest::Confine(id), surface, pointer, region, lifetime)
                }
            };

        let surface =
            client.resolve_or_disconnect(WlSurfaceId::from(surface_id), state, dispatcher)?;
        let pointer =
            client.resolve_or_disconnect(WlPointerId::from(pointer_id), state, dispatcher)?;
        let Some(lifetime) = constraint_lifetime(lifetime) else {
            self.reject_constraint(
                client,
                dispatcher,
                wl_display::Error::InvalidMethod as u32,
                "invalid pointer constraint lifetime",
            );
            return Ok(());
        };
        if surface.rref(state).pointer_constraint().is_some() {
            self.reject_constraint(
                client,
                dispatcher,
                zwp_pointer_constraints_v1::Error::AlreadyConstrained as u32,
                "pointer constraint already requested on that surface",
            );
            return Ok(());
        }
        let region = Self::snapshot_region(client, state, dispatcher, region_id)?;
        let slot = match request {
            ConstraintRequest::Lock(id) => {
                let (resolved, _) = client.insert_object(
                    state,
                    ZwpLockedPointerV1::new(id, surface, pointer, lifetime, region),
                );
                SurfacePointerConstraint::Lock(resolved)
            }
            ConstraintRequest::Confine(id) => {
                let (resolved, _) = client.insert_object(
                    state,
                    ZwpConfinedPointerV1::new(id, surface, pointer, lifetime, region),
                );
                SurfacePointerConstraint::Confine(resolved)
            }
        };
        surface.ref_mut(state).set_pointer_constraint(slot);
        crate::input::pointer_lock::maybe_activate_pointer_lock(state, dispatcher);
        crate::input::pointer_confine::maybe_activate_pointer_confine(state, dispatcher);
        Ok(())
    }
}

pub(crate) fn apply_pointer_constraint_commit(
    surface: &WlSurface,
    expected: SurfacePointerConstraint,
    state: &mut State,
    region: Option<Option<RegionValue>>,
    hint: Option<(f32, f32)>,
) {
    if surface.pointer_constraint() != Some(expected) {
        return;
    }
    let constraint = expected;
    match constraint {
        SurfacePointerConstraint::Lock(id) => {
            let Some(mut lock) = id.try_ref_mut(state) else {
                return;
            };
            if let Some(region) = region {
                lock.core_mut().apply_region(region);
            }
            if let Some(hint) = hint {
                lock.core_mut().apply_hint(hint);
            }
        }
        SurfacePointerConstraint::Confine(id) => {
            let Some(mut confine) = id.try_ref_mut(state) else {
                return;
            };
            if let Some(region) = region {
                confine.core_mut().apply_region(region);
            }
        }
    }
}

pub(crate) fn defunct_pointer_constraint(
    constraint: Option<SurfacePointerConstraint>,
    state: &mut State,
    dispatcher: &mut crate::runner::Dispatcher,
    notify_client: bool,
    geometry: Option<crate::input::pointer_lock::PointerLockWarpGeometry>,
) {
    match constraint {
        Some(SurfacePointerConstraint::Lock(id)) => {
            let Some(surface) = id.try_ref(state).map(|lock| lock.surface()) else {
                return;
            };
            crate::input::pointer_lock::release_pointer_lock_for_surface(
                state,
                dispatcher,
                surface,
                geometry,
                crate::input::pointer_lock::PointerLockRelease {
                    send_unlocked: notify_client,
                    apply_lifetime: false,
                    warp: notify_client,
                },
            );
            if let Some(mut lock) = id.try_ref_mut(state) {
                lock.core_mut().mark_terminal();
            }
        }
        Some(SurfacePointerConstraint::Confine(id)) => {
            let Some(surface) = id.try_ref(state).map(|confine| confine.surface()) else {
                return;
            };
            crate::input::pointer_confine::release_pointer_confine_for_surface(
                state,
                dispatcher,
                surface,
                notify_client,
            );
            if let Some(mut confine) = id.try_ref_mut(state) {
                confine.core_mut().mark_terminal();
            }
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use crate::{
        clients::{Arguments, Client, ClientId, ClientManagerAction, RawRequest},
        globals::Globals,
        protocol_generated::{
            zwp_pointer_constraints_v1, Constants, WlPointerId, WlRegionId, WlSurfaceId,
            ZwpConfinedPointerV1Id, ZwpLockedPointerV1Id, ZwpPointerConstraintsV1Id,
        },
        runner::Dispatcher,
        test_support::state_for_tests,
        wayland_objects::{
            pointer_constraint::{ConstraintLifetime, ConstraintStatus, SurfacePointerConstraint},
            wl_pointer::WlPointer,
            wl_region::WlRegion,
            wl_surface::WlSurface,
        },
    };

    use super::ZwpPointerConstraintsV1;

    fn request(object_id: u32, request_id: u16, words: &[u32]) -> RawRequest<'_> {
        RawRequest {
            object_id,
            request_id,
            arguments: Arguments::from_words(VecDeque::from(words.to_vec())),
            fds: None,
        }
    }

    fn dispatch(
        state: &mut crate::state::State,
        client: &mut Client,
        dispatcher: &mut Dispatcher,
        object: crate::state::ObjectId,
        request: RawRequest,
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(request.object_id.into());
        state
            .objects
            .grab(object)
            .on(client, state, request, &mut client_dispatcher)
            .unwrap();
    }

    struct Fixture {
        state: crate::state::State,
        client: Client,
        dispatcher: Dispatcher,
        surface: crate::clients::Resolved<WlSurfaceId>,
        pointer: crate::clients::Resolved<WlPointerId>,
        constraints: crate::clients::Resolved<ZwpPointerConstraintsV1Id>,
    }

    fn fixture() -> Fixture {
        let mut state = state_for_tests();
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let (surface, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(4), ClientId::from_raw(1), 6),
        );
        let (pointer, _) =
            client.insert_object(&mut state, WlPointer::new(WlPointerId::from(5), 9));
        let (constraints, _) = client.insert_object(
            &mut state,
            ZwpPointerConstraintsV1 { id: ZwpPointerConstraintsV1Id::from(6) },
        );
        Fixture {
            state,
            client,
            dispatcher: Dispatcher::default(),
            surface,
            pointer,
            constraints,
        }
    }

    fn lock(fixture: &mut Fixture, id: u32, region: u32, lifetime: u32) {
        let object = fixture.constraints.object_id();
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            object,
            request(6, 1, &[id, 4, 5, region, lifetime]),
        );
    }

    #[test]
    fn pointer_constraints_global_is_advertised() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(ZwpPointerConstraintsV1::INTERFACE_NAME));
        assert!(globals.contains_interface(
            crate::wayland_objects::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1::INTERFACE_NAME,
        ));
    }

    #[test]
    fn a_second_constraint_on_the_same_surface_is_already_constrained() {
        let mut fixture = fixture();
        lock(&mut fixture, 7, 0, zwp_pointer_constraints_v1::Lifetime::Oneshot as u32);
        assert!(fixture
            .client
            .contains_object(ZwpLockedPointerV1Id::from(7), &fixture.state));
        lock(&mut fixture, 8, 0, zwp_pointer_constraints_v1::Lifetime::Persistent as u32);
        assert!(!fixture
            .client
            .contains_object(ZwpConfinedPointerV1Id::from(8), &fixture.state));
        assert!(!fixture
            .client
            .contains_object(ZwpLockedPointerV1Id::from(8), &fixture.state));
        assert!(fixture
            .dispatcher
            .test_has_queued_action(|action: &ClientManagerAction| {
                matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(1))
            }));
        let lock = fixture
            .client
            .resolve(ZwpLockedPointerV1Id::from(7), &fixture.state)
            .unwrap();
        assert_eq!(lock.rref(&fixture.state).status(), ConstraintStatus::Inactive);
        assert_eq!(lock.rref(&fixture.state).lifetime(), ConstraintLifetime::Oneshot);
        assert_eq!(
            fixture.surface.rref(&fixture.state).pointer_constraint(),
            Some(SurfacePointerConstraint::Lock(lock))
        );
    }

    #[test]
    fn an_invalid_lifetime_is_a_protocol_error() {
        let mut fixture = fixture();
        lock(&mut fixture, 7, 0, 0);
        assert!(!fixture
            .client
            .contains_object(ZwpLockedPointerV1Id::from(7), &fixture.state));
        assert!(fixture
            .surface
            .rref(&fixture.state)
            .pointer_constraint()
            .is_none());
        assert!(fixture
            .dispatcher
            .test_has_queued_action(|action: &ClientManagerAction| {
                matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(1))
            }));
    }

    #[test]
    fn region_and_hint_apply_on_surface_commit() {
        let mut fixture = fixture();
        let (region, _) = fixture
            .client
            .insert_object(&mut fixture.state, WlRegion::new(WlRegionId::from(9)));
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            region.object_id(),
            request(9, 1, &[1, 2, 3, 4]),
        );
        let snapshot = region.rref(&fixture.state).snapshot();
        lock(&mut fixture, 7, 0, zwp_pointer_constraints_v1::Lifetime::Persistent as u32);
        let lock = fixture
            .client
            .resolve(ZwpLockedPointerV1Id::from(7), &fixture.state)
            .unwrap();
        assert_eq!(lock.rref(&fixture.state).region(), None);
        assert_eq!(lock.rref(&fixture.state).committed_hint(), None);

        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            lock.object_id(),
            request(7, 2, &[9]),
        );
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            lock.object_id(),
            request(7, 1, &[256, 512]),
        );
        assert_eq!(lock.rref(&fixture.state).region(), None);
        assert_eq!(lock.rref(&fixture.state).committed_hint(), None);

        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            fixture.surface.object_id(),
            request(4, 6, &[]),
        );
        assert_eq!(lock.rref(&fixture.state).region(), Some(&snapshot));
        assert_eq!(lock.rref(&fixture.state).committed_hint(), Some((1.0, 2.0)));
        assert!(snapshot.contains(1, 2));
    }

    #[test]
    fn factory_region_applies_immediately_and_null_set_region_waits_for_commit() {
        let mut fixture = fixture();
        let (region, _) = fixture
            .client
            .insert_object(&mut fixture.state, WlRegion::new(WlRegionId::from(9)));
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            region.object_id(),
            request(9, 1, &[1, 2, 3, 4]),
        );
        let snapshot = region.rref(&fixture.state).snapshot();
        lock(&mut fixture, 7, 9, zwp_pointer_constraints_v1::Lifetime::Oneshot as u32);
        let lock = fixture
            .client
            .resolve(ZwpLockedPointerV1Id::from(7), &fixture.state)
            .unwrap();
        assert_eq!(lock.rref(&fixture.state).region(), Some(&snapshot));

        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            lock.object_id(),
            request(7, 2, &[0]),
        );
        assert_eq!(lock.rref(&fixture.state).region(), Some(&snapshot));
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            fixture.surface.object_id(),
            request(4, 6, &[]),
        );
        assert_eq!(lock.rref(&fixture.state).region(), None);
    }

    #[test]
    fn destroying_a_constraint_clears_the_surface_slot() {
        let mut fixture = fixture();
        lock(&mut fixture, 7, 0, zwp_pointer_constraints_v1::Lifetime::Oneshot as u32);
        let lock = fixture
            .client
            .resolve(ZwpLockedPointerV1Id::from(7), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            lock.object_id(),
            request(7, 0, &[]),
        );
        assert!(fixture
            .surface
            .rref(&fixture.state)
            .pointer_constraint()
            .is_none());
        assert!(!fixture
            .client
            .contains_object(ZwpLockedPointerV1Id::from(7), &fixture.state));
    }

    #[test]
    fn destroying_the_surface_makes_the_constraint_terminal() {
        let mut fixture = fixture();
        lock(&mut fixture, 7, 0, zwp_pointer_constraints_v1::Lifetime::Persistent as u32);
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            fixture.surface.object_id(),
            request(4, 0, &[]),
        );
        let lock = fixture
            .client
            .resolve(ZwpLockedPointerV1Id::from(7), &fixture.state)
            .unwrap();
        assert_eq!(lock.rref(&fixture.state).status(), ConstraintStatus::Terminal);
    }

    #[test]
    fn confine_rejects_a_surface_that_already_has_a_lock() {
        let mut fixture = fixture();
        lock(&mut fixture, 7, 0, zwp_pointer_constraints_v1::Lifetime::Persistent as u32);
        let object = fixture.constraints.object_id();
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            object,
            request(6, 2, &[8, 4, 5, 0, zwp_pointer_constraints_v1::Lifetime::Oneshot as u32]),
        );
        assert!(!fixture
            .client
            .contains_object(ZwpConfinedPointerV1Id::from(8), &fixture.state));
        assert!(fixture
            .client
            .contains_object(ZwpLockedPointerV1Id::from(7), &fixture.state));
    }

    fn rect_region(x: i32, y: i32, width: i32, height: i32) -> crate::region::RegionValue {
        let mut spec = crate::region::RegionSpec::default();
        spec.add_rect(crate::region::RectI::new(x, y, width, height).unwrap());
        crate::region::RegionValue::from_spec(spec)
    }

    fn add_region(fixture: &mut Fixture, id: u32, x: i32, y: i32, width: i32, height: i32) {
        let (region, _) = fixture.client.insert_object(
            &mut fixture.state,
            WlRegion::new(crate::protocol_generated::WlRegionId::from(id)),
        );
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            region.object_id(),
            request(id, 1, &[x as u32, y as u32, width as u32, height as u32]),
        );
    }

    #[test]
    fn a_wire_scaled_confine_region_is_stored_in_logical_units() {
        let mut scaled = fixture();
        scaled
            .client
            .set_wire_scale_for_tests(crate::wire_scale::WireScale::new(2));
        add_region(&mut scaled, 9, 0, 0, 100, 100);
        let object = scaled.constraints.object_id();
        dispatch(
            &mut scaled.state,
            &mut scaled.client,
            &mut scaled.dispatcher,
            object,
            request(6, 2, &[8, 4, 5, 9, zwp_pointer_constraints_v1::Lifetime::Persistent as u32]),
        );
        let confine = scaled
            .client
            .resolve(ZwpConfinedPointerV1Id::from(8), &scaled.state)
            .unwrap();
        assert_eq!(confine.rref(&scaled.state).region(), Some(&rect_region(0, 0, 50, 50)));

        let mut at_one = fixture();
        add_region(&mut at_one, 9, 0, 0, 100, 100);
        let object = at_one.constraints.object_id();
        dispatch(
            &mut at_one.state,
            &mut at_one.client,
            &mut at_one.dispatcher,
            object,
            request(6, 2, &[8, 4, 5, 9, zwp_pointer_constraints_v1::Lifetime::Oneshot as u32]),
        );
        let confine = at_one
            .client
            .resolve(ZwpConfinedPointerV1Id::from(8), &at_one.state)
            .unwrap();
        assert_eq!(confine.rref(&at_one.state).region(), Some(&rect_region(0, 0, 100, 100)));
    }

    #[test]
    fn a_wire_scaled_set_region_and_hint_are_divided_on_commit() {
        let mut fixture = fixture();
        fixture
            .client
            .set_wire_scale_for_tests(crate::wire_scale::WireScale::new(2));
        add_region(&mut fixture, 9, 0, 0, 100, 100);
        lock(&mut fixture, 7, 0, zwp_pointer_constraints_v1::Lifetime::Persistent as u32);
        let lock = fixture
            .client
            .resolve(ZwpLockedPointerV1Id::from(7), &fixture.state)
            .unwrap();
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            lock.object_id(),
            request(7, 2, &[9]),
        );
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            lock.object_id(),
            request(7, 1, &[9 * 256, 9 * 256]),
        );
        dispatch(
            &mut fixture.state,
            &mut fixture.client,
            &mut fixture.dispatcher,
            fixture.surface.object_id(),
            request(4, 6, &[]),
        );
        assert_eq!(lock.rref(&fixture.state).region(), Some(&rect_region(0, 0, 50, 50)));
        assert_eq!(lock.rref(&fixture.state).committed_hint(), Some((4.5, 4.5)));
    }
}
