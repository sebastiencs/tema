use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    color::RenderIntent,
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        wp_color_management_surface_v1, WaylandId, WlSurfaceId, WpColorManagementSurfaceV1Id,
        WpImageDescriptionV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

/// One `wp_color_management_surface_v1`. The surface stores the pending color;
/// this object only checks the protocol and writes that slot.
#[derive(Debug)]
pub struct WpColorManagementSurfaceV1 {
    id: WpColorManagementSurfaceV1Id,
    version: u32,
    surface: Resolved<WlSurfaceId>,
}

impl WpColorManagementSurfaceV1 {
    pub(crate) fn new(
        id: WpColorManagementSurfaceV1Id,
        version: u32,
        surface: Resolved<WlSurfaceId>,
    ) -> Self {
        Self { id, version, surface }
    }

    fn inert(&self, client: &Client, dispatcher: &mut ClientDispatcher) {
        client.send_protocol_error(
            dispatcher,
            self.id.into(),
            wp_color_management_surface_v1::Error::Inert as u32,
            "wl_surface was destroyed",
        );
    }
}

impl Object for WpColorManagementSurfaceV1 {
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
        use wp_color_management_surface_v1::Request::*;
        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    self.inert(client, dispatcher);
                    return Ok(());
                };
                let this = client.resolve(self.id, state).unwrap();
                surface.clear_color_management_surface(this);
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            SetImageDescription { image_description, render_intent } => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    self.inert(client, dispatcher);
                    return Ok(());
                };
                let Some(intent) = RenderIntent::from_wayland(render_intent, self.version) else {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        wp_color_management_surface_v1::Error::RenderIntent as u32,
                        "rendering intent is not advertised",
                    );
                    return Ok(());
                };
                let image = client.resolve_or_disconnect(image_description, state, dispatcher)?;
                let Some(description) = image.rref(state).ready_description() else {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        wp_color_management_surface_v1::Error::ImageDescription as u32,
                        "image description is not ready",
                    );
                    return Ok(());
                };
                surface.set_pending_color(Some((description, intent)));
            }
            UnsetImageDescription => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    self.inert(client, dispatcher);
                    return Ok(());
                };
                surface.set_pending_color(None);
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
            surface.clear_color_management_surface(object);
        }
    }
}
