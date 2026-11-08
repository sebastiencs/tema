use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    drm::{request_active_output, request_outputs_for_rect},
    object::{Object, Parser, RequestResult},
    protocol_generated::{wl_subsurface, WaylandId, WlSubsurfaceId, WlSurfaceId},
    state::{GrabbedObject, ObjectId, State},
    wayland_objects::wl_surface::WlSurface,
};

#[derive(Debug)]
pub struct WlSubsurface {
    id: WlSubsurfaceId,
    surface: Resolved<WlSurfaceId>,
    parent: Resolved<WlSurfaceId>,
}

impl WlSubsurface {
    pub fn new(
        id: WlSubsurfaceId,
        surface: Resolved<WlSurfaceId>,
        parent: Resolved<WlSurfaceId>,
    ) -> Self {
        Self { id, surface, parent }
    }

    fn sibling_target(
        &self,
        state: &State,
        sibling_surface: Resolved<WlSurfaceId>,
    ) -> Result<Option<Resolved<WlSurfaceId>>, ()> {
        let sibling_surface_ref = sibling_surface.rref(state);
        if sibling_surface == self.surface {
            return Err(());
        }
        if sibling_surface == self.parent {
            return Ok(None);
        }
        if sibling_surface_ref
            .parent_surface()
            .map(|surface| surface.object_id())
            != Some(self.parent.object_id())
        {
            return Err(());
        }
        Ok(Some(sibling_surface))
    }

    fn bad_surface(&self, client: &Client, dispatcher: &mut ClientDispatcher, message: &str) {
        client.send_protocol_error(
            dispatcher,
            self.id.into(),
            wl_subsurface::Error::BadSurface as u32,
            message,
        );
    }

    /// The child still has this object as its subsurface role. Parent destruction
    /// clears that role and leaves this protocol object until the client destroys it.
    fn role_is_live(&self, state: &State) -> bool {
        self.surface
            .try_ref(state)
            .and_then(|surface| surface.subsurface_object())
            .is_some_and(|object| object.wayland_id() == self.id)
    }
}

impl Object for WlSubsurface {
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
        use wl_subsurface::Request::*;

        let request = client.parse::<Self>(req, dispatcher)?;
        // Parent destruction unmaps the child and clears the role, then the client
        // still sends destroy (and sometimes position) for this object. Those
        // requests stay valid; only destroy removes the protocol object.
        if !matches!(request, Destroy) && !self.role_is_live(state) {
            return Ok(());
        }
        match request {
            Destroy => {
                let old_root_surface = state.root_surface_for_surface(self.parent);
                if let Some(mut surface) = self.surface.try_grab(state) {
                    surface.unmap_due_to_parent_loss(client, state, dispatcher);
                    surface.destroy_subsurface_role();
                }
                if let Some(mut parent_surface) = self.parent.try_grab(state) {
                    parent_surface.remove_child_from_tree(self.surface);
                }
                state.register_root_surface(self.surface);
                if let Some(root_surface) = old_root_surface {
                    state.mark_root_geometry_dirty_for_surface(root_surface);
                }
                let child_root_surface = self.surface.clone();
                state.mark_root_geometry_dirty_for_surface(child_root_surface);
                use crate::wayland_objects::wl_display::WlDisplay;
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
                if let Some(bounds) = state
                    .root_surface_for_surface(self.surface)
                    .and_then(|root_surface| state.root_surface_surface_bounds_global(root_surface))
                {
                    request_outputs_for_rect(state, bounds, dispatcher);
                } else {
                    request_active_output(state, dispatcher);
                }
            }
            SetPosition { x, y } => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    return Ok(());
                };
                // A wire-scaled client sends X pixels. Converted once here and
                // not recomputed when S changes (Xwayland does not use
                // subsurfaces).
                let scale = dispatcher.wire_scale();
                surface.set_subsurface_position(scale.pos_to_logical(x), scale.pos_to_logical(y));
                drop(surface);
                state.mark_root_geometry_dirty_for_surface(self.parent);
            }
            PlaceAbove { sibling } => {
                let sibling = client.resolve_or_disconnect(sibling, state, dispatcher)?;
                let Ok(sibling_surface) = self.sibling_target(state, sibling) else {
                    self.bad_surface(
                        client,
                        dispatcher,
                        "place_above sibling must be the parent or a sibling surface",
                    );
                    return Ok(());
                };
                let Some(mut parent_surface) = self.parent.try_grab(state) else {
                    return Ok(());
                };
                if !parent_surface.restack_child_relative(self.surface, sibling_surface, true) {
                    self.bad_surface(
                        client,
                        dispatcher,
                        "place_above sibling must be in the same parent stack",
                    );
                }
            }
            PlaceBelow { sibling } => {
                let sibling = client.resolve_or_disconnect(sibling, state, dispatcher)?;
                let Ok(sibling_surface) = self.sibling_target(state, sibling) else {
                    self.bad_surface(
                        client,
                        dispatcher,
                        "place_below sibling must be the parent or a sibling surface",
                    );
                    return Ok(());
                };
                let Some(mut parent_surface) = self.parent.try_grab(state) else {
                    return Ok(());
                };
                if !parent_surface.restack_child_relative(self.surface, sibling_surface, false) {
                    self.bad_surface(
                        client,
                        dispatcher,
                        "place_below sibling must be in the same parent stack",
                    );
                }
            }
            SetSync => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    return Ok(());
                };
                surface.set_subsurface_sync_mode(true);
            }
            SetDesync => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    return Ok(());
                };
                surface.set_subsurface_sync_mode(false);
                surface.apply_cached_if_parent_desynchronized(client, state, dispatcher);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::WlSubsurface,
        crate::{
            clients::{Client, ClientId, RawEvent, Resolved},
            object::{Object, RequestResult},
            protocol_generated::{WlSubsurfaceId, WlSurfaceId},
            runner::Dispatcher,
            state::{GrabbedObject, ObjectId, State},
            test_support::{state_for_tests, test_client_and_dispatcher},
            wayland_objects::wl_surface::{StackEntry, WlSurface},
            wire_scale::WireScale,
        },
    };

    fn place_above_request(
        object_id: WlSubsurfaceId,
        sibling: WlSurfaceId,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(object_id.into(), 2, 1);
        event.id(sibling);
        (&*event.into_bytes()).try_into().unwrap()
    }

    fn set_position_request(
        object_id: WlSubsurfaceId,
        x: i32,
        y: i32,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(object_id.into(), 1, 2);
        event.uint(x as u32);
        event.uint(y as u32);
        (&*event.into_bytes()).try_into().unwrap()
    }

    /// A parent with one subsurface child, both registered as roots. Returns
    /// the object ids of the parent, the child and the subsurface object.
    fn parent_child_and_subsurface(
        client: &mut Client,
        state: &mut State,
    ) -> (ObjectId, ObjectId, ObjectId) {
        let (parent_resolved, parent_object_id) = {
            let (resolved, _) = client
                .insert_object(state, WlSurface::new(WlSurfaceId::from(10), client.client_id(), 6));
            (resolved, resolved.object_id())
        };
        state.register_root_surface(parent_resolved);
        let (child_resolved, child_object_id) = {
            let (resolved, _) = client
                .insert_object(state, WlSurface::new(WlSurfaceId::from(11), client.client_id(), 6));
            (resolved, resolved.object_id())
        };
        state.register_root_surface(child_resolved);
        let (subsurface_resolved, subsurface_object_id) = {
            let (resolved, _) = client.insert_object(
                state,
                WlSubsurface::new(WlSubsurfaceId::from(30), child_resolved, parent_resolved),
            );
            (resolved, resolved.object_id())
        };
        {
            let parent = state
                .objects
                .get_mut(parent_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            parent.add_pending_child(child_resolved);
        }
        {
            let child = state
                .objects
                .get_mut(child_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child.assign_subsurface(subsurface_resolved, parent_resolved);
        }
        (parent_object_id, child_object_id, subsurface_object_id)
    }

    fn send_set_position(
        client: &mut Client,
        state: &mut State,
        subsurface_object_id: ObjectId,
        x: i32,
        y: i32,
    ) {
        let mut dispatcher = Dispatcher::default();
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let subsurface = state
            .objects
            .grab_object::<WlSubsurface>(subsurface_object_id);
        subsurface.on(
            client,
            state,
            set_position_request(WlSubsurfaceId::from(30), x, y),
            &mut client_dispatcher,
        );
    }

    #[test]
    fn set_position_converts_wire_units_for_a_scaled_client() {
        let mut state = state_for_tests();
        let (mut client, _) = test_client_and_dispatcher(5);
        client.set_wire_scale_for_tests(WireScale::new(2));
        let (parent_object_id, child_object_id, subsurface_object_id) =
            parent_child_and_subsurface(&mut client, &mut state);

        send_set_position(&mut client, &mut state, subsurface_object_id, -3, 5);
        {
            let mut parent = state.objects.grab_object::<WlSurface>(parent_object_id);
            assert!(parent.apply_pending_tree_state(&mut state));
        }
        let child = state
            .objects
            .get_mut(child_object_id)
            .downcast_mut::<WlSurface>()
            .unwrap();
        // floor(-3/2) = -2 and floor(5/2) = 2, in logical units.
        assert_eq!(child.current_subsurface_position(), (-2, 2));
    }

    #[test]
    fn set_position_is_the_identity_at_scale_one() {
        let mut state = state_for_tests();
        let (mut client, _) = test_client_and_dispatcher(5);
        let (parent_object_id, child_object_id, subsurface_object_id) =
            parent_child_and_subsurface(&mut client, &mut state);

        send_set_position(&mut client, &mut state, subsurface_object_id, 20, 30);
        {
            let mut parent = state.objects.grab_object::<WlSurface>(parent_object_id);
            assert!(parent.apply_pending_tree_state(&mut state));
        }
        let child = state
            .objects
            .get_mut(child_object_id)
            .downcast_mut::<WlSurface>()
            .unwrap();
        assert_eq!(child.current_subsurface_position(), (20, 30));
    }

    #[test]
    fn place_above_rejects_non_sibling_surface() {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(5);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);

        let (parent_resolved, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(10), client.client_id(), 6),
        );
        let parent_object_id = parent_resolved.object_id();
        state.register_root_surface(parent_resolved);
        let (child_resolved, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(11), client.client_id(), 6),
        );
        let child_object_id = child_resolved.object_id();
        state.register_root_surface(child_resolved);
        let (stranger_resolved, _) = client.insert_object(
            &mut state,
            WlSurface::new(WlSurfaceId::from(12), client.client_id(), 6),
        );
        let stranger_object_id = stranger_resolved.object_id();
        state.register_root_surface(stranger_resolved);
        let (subsurface_resolved, _) = client.insert_object(
            &mut state,
            WlSubsurface::new(
                WlSubsurfaceId::from(30),
                Resolved::new(client.client_id(), WlSurfaceId::from(11), child_object_id),
                Resolved::new(client.client_id(), WlSurfaceId::from(10), parent_object_id),
            ),
        );
        let subsurface_object_id = subsurface_resolved.object_id();
        let subsurface =
            Resolved::new(client.client_id(), WlSubsurfaceId::from(30), subsurface_object_id);
        let child_resolved =
            Resolved::new(client.client_id(), WlSurfaceId::from(11), child_object_id);

        {
            let parent = state
                .objects
                .get_mut(parent_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            parent.add_pending_child(child_resolved);
        }
        {
            let child = state
                .objects
                .get_mut(child_object_id)
                .downcast_mut::<WlSurface>()
                .unwrap();
            child.assign_subsurface(
                subsurface,
                Resolved::new(client.client_id(), WlSurfaceId::from(10), parent_object_id),
            );
        }

        let subsurface = state
            .objects
            .grab_object::<WlSubsurface>(subsurface_object_id);
        subsurface.on(
            &mut client,
            &mut state,
            place_above_request(WlSubsurfaceId::from(30), WlSurfaceId::from(12)),
            &mut client_dispatcher,
        );

        let mut parent = state.objects.grab_object::<WlSurface>(parent_object_id);
        assert!(parent.apply_pending_tree_state(&mut state));
        assert_eq!(
            parent.current_stack(),
            &[StackEntry::Child(child_resolved), StackEntry::Parent]
        );
    }
}
