use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        wp_alpha_modifier_surface_v1, WaylandId, WlSurfaceId, WpAlphaModifierSurfaceV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

#[derive(Debug)]
pub struct WpAlphaModifierSurfaceV1 {
    id: WpAlphaModifierSurfaceV1Id,
    surface: Resolved<WlSurfaceId>,
}

impl WpAlphaModifierSurfaceV1 {
    pub fn new(id: WpAlphaModifierSurfaceV1Id, surface: Resolved<WlSurfaceId>) -> Self {
        Self { id, surface }
    }
}

impl Object for WpAlphaModifierSurfaceV1 {
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
        use wp_alpha_modifier_surface_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            SetMultiplier { factor } => {
                let Some(mut surface) = self.surface.try_grab(state) else {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        wp_alpha_modifier_surface_v1::Error::NoSurface as u32,
                        "wl_surface was destroyed",
                    );
                    return Ok(());
                };
                surface.set_pending_alpha_multiplier(factor);
            }
            Destroy => {
                // The XML raises `no_surface` for every request once `wl_surface` is gone,
                // including this destructor. The object stays so a later request still
                // reports `no_surface` rather than an unknown object.
                let Some(mut surface) = self.surface.try_grab(state) else {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        wp_alpha_modifier_surface_v1::Error::NoSurface as u32,
                        "wl_surface was destroyed",
                    );
                    return Ok(());
                };
                let this = client.resolve(self.id, state).unwrap();
                surface.clear_alpha_modifier_object(this);
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
        if let Some(mut surface) = self.surface.try_grab(state) {
            surface.clear_alpha_modifier_object(object);
        }
    }
}
