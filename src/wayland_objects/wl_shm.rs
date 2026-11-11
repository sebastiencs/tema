use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    drm::{
        Format, ABGR16161616F, ABGR2101010, ARGB2101010, ARGB8888, XBGR16161616F, XBGR2101010,
        XRGB2101010, XRGB8888,
    },
    globals::{Global, GlobalName},
    object::{Object, Parser, RequestResult},
    protocol_generated::{wl_shm, Constants, NewId, WaylandId, WlShmId},
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, wl_shm_pool::WlShmPool},
};

#[derive(Debug)]
pub struct WlShmGlobal;

impl WlShmGlobal {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self {})
    }
}

/// Client SHM formats, in the order `wl_shm.format` is sent.
///
/// XRGB8888 comes first, then ARGB8888. Wider formats follow in the same
/// opaque-then-alpha order: 10-bit, then FP16.
pub static SHM_FORMATS: [&'static Format; 8] = [
    XRGB8888,
    ARGB8888,
    XRGB2101010,
    ARGB2101010,
    XBGR2101010,
    ABGR2101010,
    XBGR16161616F,
    ABGR16161616F,
];

/// A format in [`SHM_FORMATS`] that every current renderer can upload.
///
/// With no DRM device yet, every listed format is accepted. A GLES 2 device
/// drops the 10-bit and FP16 formats.
pub(crate) fn shm_format_supported(state: &crate::state::State, format: &Format) -> bool {
    state
        .drm
        .iter()
        .all(|(_, drm)| drm.accepts_shm_format(format.drm))
}

impl Global for WlShmGlobal {
    fn interface(&self) -> (&'static str, u32) {
        (WlShm::INTERFACE_NAME, WlShm::INTERFACE_VERSION)
    }
    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        version: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        let id = WlShmId::from(id);
        let codes: Vec<u32> = SHM_FORMATS
            .iter()
            .filter(|format| shm_format_supported(state, format))
            .filter_map(|format| format.wayland_shm_id)
            .collect();
        let (_, wl_shm) = client.insert_object(state, WlShm { id, version });

        for code in codes {
            wl_shm.send_event(wl_shm::Event::Format { format: code }, dispatcher);
        }
    }
}

#[derive(Debug)]
pub struct WlShm {
    id: WlShmId,
    version: u32,
}

impl WlShm {
    pub fn new(id: WlShmId, version: u32) -> Self {
        Self { id, version }
    }
}

impl Object for WlShm {
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
        use crate::protocol_generated::wl_shm::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            CreatePool { id, fd, size } => {
                let pool = WlShmPool::new(id, fd, size as usize, self.version);
                client.insert_object(state, pool);
            }
            Release => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
        }
        Ok(())
    }
}
