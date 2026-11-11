use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        ext_background_effect_surface_v1, ExtBackgroundEffectSurfaceV1Id, WaylandId, WlSurfaceId,
    },
    region::RegionValue,
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, wl_surface::WlSurface},
};

#[derive(Debug)]
pub struct ExtBackgroundEffectSurfaceV1 {
    id: ExtBackgroundEffectSurfaceV1Id,
    surface: Resolved<WlSurfaceId>,
}

impl ExtBackgroundEffectSurfaceV1 {
    pub fn new(id: ExtBackgroundEffectSurfaceV1Id, surface: Resolved<WlSurfaceId>) -> Self {
        Self { id, surface }
    }
}

impl Object for ExtBackgroundEffectSurfaceV1 {
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
        use ext_background_effect_surface_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            SetBlurRegion { region } => {
                if self.surface.try_grab(state).is_none() {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        ext_background_effect_surface_v1::Error::SurfaceDestroyed as u32,
                        "wl_surface was destroyed",
                    );
                    return Ok(());
                }
                // NULL removes the effect. A live region is copied, so the client
                // may destroy the wl_region before the next commit.
                let region = match region.to_opt() {
                    None => None,
                    Some(region_id) => {
                        let Some(region) = WlSurface::snapshot_region_request(
                            client,
                            state,
                            dispatcher,
                            region_id,
                            RegionValue::empty(),
                        ) else {
                            return Ok(());
                        };
                        Some(region)
                    }
                };
                let Some(mut surface) = self.surface.try_grab(state) else {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        ext_background_effect_surface_v1::Error::SurfaceDestroyed as u32,
                        "wl_surface was destroyed",
                    );
                    return Ok(());
                };
                surface.set_pending_blur_region(region);
            }
            Destroy => {
                // The spec makes this object inert once the surface is gone, and
                // still removes the region on the next commit when the surface
                // remains. Destroy itself is not a protocol error.
                if let Some(mut surface) = self.surface.try_grab(state) {
                    let this = client.resolve(self.id, state).unwrap();
                    surface.clear_background_effect_object(this);
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
        if let Some(mut surface) = self.surface.try_grab(state) {
            surface.clear_background_effect_object(object);
        }
    }
}
