use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    object::{Object, Parser, RequestResult},
    protocol_generated::{wp_fifo_v1, WaylandId, WlSurfaceId, WpFifoV1Id},
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, wl_surface::WlSurface},
};

#[derive(Debug)]
pub struct WpFifoV1 {
    id: WpFifoV1Id,
    /// `None` once the `wl_surface` is destroyed.
    surface: Option<Resolved<WlSurfaceId>>,
}

impl WpFifoV1 {
    pub fn new(id: WpFifoV1Id, surface: Resolved<WlSurfaceId>) -> Self {
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
            wp_fifo_v1::Error::SurfaceDestroyed as u32,
            "wl_surface was destroyed",
        );
    }
}

impl Object for WpFifoV1 {
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
        use wp_fifo_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            SetBarrier => {
                let Some(surface) = self.live_surface(state) else {
                    self.surface_destroyed(client, dispatcher);
                    return Ok(());
                };
                surface.ref_mut(state).set_pending_fifo_barrier();
            }
            WaitBarrier => {
                let Some(surface) = self.live_surface(state) else {
                    self.surface_destroyed(client, dispatcher);
                    return Ok(());
                };
                surface.ref_mut(state).set_pending_fifo_wait();
            }
            Destroy => {
                let this = client.resolve(self.id, state).unwrap();
                if let Some(surface) = self.live_surface(state) {
                    surface.ref_mut(state).clear_fifo_object(this);
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
            surface.ref_mut(state).clear_fifo_object(object);
        }
    }
}
