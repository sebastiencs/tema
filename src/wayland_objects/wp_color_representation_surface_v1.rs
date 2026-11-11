use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    color::{alpha_from_wire, chroma_from_wire, coefficients_from_wire},
    object::{Object, RequestResult},
    protocol_generated::{
        wp_color_representation_surface_v1, WaylandId, WlSurfaceId,
        WpColorRepresentationSurfaceV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

#[derive(Debug)]
pub struct WpColorRepresentationSurfaceV1 {
    id: WpColorRepresentationSurfaceV1Id,
    surface: Resolved<WlSurfaceId>,
}

impl WpColorRepresentationSurfaceV1 {
    pub fn new(id: WpColorRepresentationSurfaceV1Id, surface: Resolved<WlSurfaceId>) -> Self {
        Self { id, surface }
    }

    fn inert(&self, client: &mut Client, dispatcher: &mut ClientDispatcher) -> RequestResult {
        client.send_protocol_error(
            dispatcher,
            self.id.into(),
            wp_color_representation_surface_v1::Error::Inert as u32,
            "wl_surface was destroyed",
        );
        Ok(())
    }
}

impl Object for WpColorRepresentationSurfaceV1 {
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
        use wp_color_representation_surface_v1::Request::*;
        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                if let Some(mut surface) = self.surface.try_grab(state) {
                    let this = client.resolve(self.id, state).unwrap();
                    surface.clear_color_representation_object(this);
                    surface.unset_pending_representation();
                }
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            SetAlphaMode { alpha_mode } => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    return self.inert(client, dispatcher);
                };
                let Some(mode) = alpha_from_wire(alpha_mode) else {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        wp_color_representation_surface_v1::Error::AlphaMode as u32,
                        "unsupported alpha mode",
                    );
                    return Ok(());
                };
                surface.set_pending_alpha_mode(Some(mode));
            }
            SetCoefficientsAndRange { coefficients, range } => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    return self.inert(client, dispatcher);
                };
                let Some(coefficients) = coefficients_from_wire(coefficients, range) else {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        wp_color_representation_surface_v1::Error::Coefficients as u32,
                        "unsupported coefficients and range",
                    );
                    return Ok(());
                };
                surface.set_pending_coefficients(Some(coefficients));
            }
            SetChromaLocation { chroma_location } => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    return self.inert(client, dispatcher);
                };
                let Some(site) = chroma_from_wire(chroma_location) else {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        wp_color_representation_surface_v1::Error::ChromaLocation as u32,
                        "invalid chroma location",
                    );
                    return Ok(());
                };
                surface.set_pending_chroma(Some(site));
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
            surface.clear_color_representation_object(object);
        }
    }
}
