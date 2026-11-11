use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest, Resolved},
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        wp_color_management_surface_feedback_v1, WaylandId, WlSurfaceId,
        WpColorManagementSurfaceFeedbackV1Id, WpImageDescriptionV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

/// One `wp_color_management_surface_feedback_v1`. Preferred descriptions are
/// parametric, so `get_preferred` and `get_preferred_parametric` publish the
/// same description.
#[derive(Debug)]
pub struct WpColorManagementSurfaceFeedbackV1 {
    id: WpColorManagementSurfaceFeedbackV1Id,
    version: u32,
    surface: Resolved<WlSurfaceId>,
}

impl WpColorManagementSurfaceFeedbackV1 {
    pub(crate) fn new(
        id: WpColorManagementSurfaceFeedbackV1Id,
        version: u32,
        surface: Resolved<WlSurfaceId>,
    ) -> Self {
        Self { id, version, surface }
    }

    pub(crate) fn send_preferred_changed(&self, identity: u64, dispatcher: &mut ClientDispatcher) {
        use wp_color_management_surface_feedback_v1::{Event, PREFERRED_CHANGED2_SINCE};
        if self.version >= PREFERRED_CHANGED2_SINCE {
            self.send_event(
                Event::PreferredChanged2 {
                    identity_hi: (identity >> 32) as u32,
                    identity_lo: identity as u32,
                },
                dispatcher,
            );
        } else if identity <= u64::from(u32::MAX) {
            self.send_event(Event::PreferredChanged { identity: identity as u32 }, dispatcher);
        }
    }

    fn inert(&self, client: &Client, dispatcher: &mut ClientDispatcher) {
        client.send_protocol_error(
            dispatcher,
            self.id.into(),
            wp_color_management_surface_feedback_v1::Error::Inert as u32,
            "wl_surface was destroyed",
        );
    }

    fn publish_preferred(
        &self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        image_description: WpImageDescriptionV1Id,
    ) {
        let Some(description) = self
            .surface
            .try_ref(state)
            .map(|surface| surface.preferred_image_description(state))
        else {
            self.inert(client, dispatcher);
            return;
        };
        super::publish_ready_description(
            client,
            state,
            dispatcher,
            image_description,
            self.version,
            description,
            true,
        );
    }
}

impl Object for WpColorManagementSurfaceFeedbackV1 {
    fn id(&self) -> WaylandId {
        self.id.into()
    }

    fn on(
        self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        use wp_color_management_surface_feedback_v1::Request::*;
        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    self.inert(client, dispatcher);
                    return Ok(());
                };
                let this = client.resolve(self.id, state).unwrap();
                surface.remove_color_feedback(this);
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetPreferred { image_description } | GetPreferredParametric { image_description } => {
                if self.surface.try_ref(state).is_none() {
                    self.inert(client, dispatcher);
                    return Ok(());
                }
                self.publish_preferred(client, state, dispatcher, image_description);
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
        if let Some(mut surface) = self.surface.try_grab(state) {
            surface.remove_color_feedback(object);
        }
    }
}
