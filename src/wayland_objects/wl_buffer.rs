use std::rc::Rc;

use crate::{
    buffer_content::BufferContent,
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    dmabuf::DmaBuf,
    drm::{Format, ARGB8888},
    object::{Object, Parser, RequestResult},
    protocol_generated::{wl_buffer, WaylandId, WlBufferId},
    state::{GrabbedObject, State},
};

pub use crate::buffer_content::{
    CaptureBufferError, CaptureShmWrite, CursorShmBuffer, IconBufferError, WlBufferError,
};

use super::wl_shm_pool::SharedMemory;

#[derive(Debug)]
pub struct WlBuffer {
    id: WlBufferId,
    content: Rc<BufferContent>,
}

pub struct NewWlBufferParams {
    pub id: WlBufferId,
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: &'static Format,
    pub mem: Rc<SharedMemory>,
}

pub struct NewDmabufWlBufferParams {
    pub id: WlBufferId,
    pub dmabuf: DmaBuf,
    pub y_inverted: bool,
    pub import_rejected: bool,
}

impl WlBuffer {
    pub fn new(params: NewWlBufferParams) -> Result<Self, WlBufferError> {
        let NewWlBufferParams { id, offset, width, height, stride, format, mem } = params;

        Ok(Self {
            id,
            content: BufferContent::new_shm(offset, width, height, stride, format, mem)?,
        })
    }

    pub fn new_dmabuf(params: NewDmabufWlBufferParams) -> Result<Self, WlBufferError> {
        let NewDmabufWlBufferParams { id, dmabuf, y_inverted, import_rejected } = params;
        Ok(Self {
            id,
            content: BufferContent::new_dmabuf(dmabuf, y_inverted, import_rejected),
        })
    }

    pub fn content(&self) -> &Rc<BufferContent> {
        &self.content
    }

    pub fn send_release(&mut self, dispatcher: &mut ClientDispatcher) {
        self.send_event(wl_buffer::Event::Release, dispatcher);
    }

    #[cfg(test)]
    pub(crate) fn test_argb8888_shm(
        id: WlBufferId,
        width: i32,
        height: i32,
        pixels: Vec<u8>,
    ) -> Self {
        let stride = width.saturating_mul(ARGB8888.bpp as i32);
        let mem = super::wl_shm_pool::shared_memory_from_bytes(&pixels);
        Self {
            id,
            content: BufferContent::new_shm(0, width, height, stride, ARGB8888, mem)
                .expect("test shm buffer"),
        }
    }
}

impl Object for WlBuffer {
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
        use {crate::wayland_objects::wl_display::WlDisplay, wl_buffer::Request::*};

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                // Surfaces still showing this buffer hold its BufferContent.
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
        }
        Ok(())
    }
}
