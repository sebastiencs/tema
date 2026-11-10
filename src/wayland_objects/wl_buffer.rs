use std::rc::Rc;

use crate::{
    buffer_content::BufferContent,
    clients::{Client, ClientDispatcher, ClientId, EventWriter, RawRequest, Resolved},
    dmabuf::DmaBuf,
    drm::{Format, ARGB8888},
    object::{Object, Parser, PostAction, RequestResult},
    protocol_generated::{wl_buffer, WaylandId, WlBufferId},
    state::{GrabbedObject, ObjectId, State},
    sync::PlainRelease,
};

pub use crate::buffer_content::{
    CaptureBufferError, CaptureShmWrite, CursorShmBuffer, IconBufferError, WlBufferError,
};

use super::wl_shm_pool::SharedMemory;

#[derive(Debug)]
pub struct WlBuffer {
    id: WlBufferId,
    owner_client_id: ClientId,
    destroyed: bool,
    content: Rc<BufferContent>,
}

pub struct NewWlBufferParams {
    pub id: WlBufferId,
    pub owner_client_id: ClientId,
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: &'static Format,
    pub mem: Rc<SharedMemory>,
}

pub struct NewDmabufWlBufferParams {
    pub id: WlBufferId,
    pub owner_client_id: ClientId,
    pub dmabuf: DmaBuf,
    pub y_inverted: bool,
    pub import_rejected: bool,
}

impl WlBuffer {
    pub fn new(params: NewWlBufferParams) -> Result<Self, WlBufferError> {
        let NewWlBufferParams {
            id,
            owner_client_id,
            offset,
            width,
            height,
            stride,
            format,
            mem,
        } = params;

        Ok(Self {
            id,
            owner_client_id,
            destroyed: false,
            content: BufferContent::new_shm(offset, width, height, stride, format, mem)?,
        })
    }

    pub fn new_dmabuf(params: NewDmabufWlBufferParams) -> Result<Self, WlBufferError> {
        let NewDmabufWlBufferParams { id, owner_client_id, dmabuf, y_inverted, import_rejected } =
            params;
        Ok(Self {
            id,
            owner_client_id,
            destroyed: false,
            content: BufferContent::new_dmabuf(dmabuf, y_inverted, import_rejected),
        })
    }

    pub fn content(&self) -> &Rc<BufferContent> {
        &self.content
    }

    pub fn send_release(&mut self, dispatcher: &mut ClientDispatcher) {
        self.send_event(wl_buffer::Event::Release, dispatcher);
    }

    pub fn plain_release_target(&self, object_id: ObjectId) -> Option<PlainRelease> {
        if self.destroyed {
            return None;
        }
        Some(Resolved::new(self.owner_client_id, self.id, object_id))
    }

    pub fn mark_destroyed(&mut self) {
        self.destroyed = true;
    }

    pub fn is_destroyed(&self) -> bool {
        self.destroyed
    }

    /// The destroyed check stays on the protocol object. The pixels themselves
    /// do not know whether the client has destroyed the `wl_buffer`.
    pub fn begin_capture_write(
        &self,
        width: i32,
        height: i32,
    ) -> Result<CaptureShmWrite, CaptureBufferError> {
        if self.destroyed {
            return Err(CaptureBufferError::Destroyed);
        }
        self.content.begin_capture_write(width, height)
    }

    pub(crate) fn copy_shm_square_argb(&self) -> Result<(u32, Vec<u8>), IconBufferError> {
        if self.destroyed {
            return Err(IconBufferError::Destroyed);
        }
        self.content.copy_shm_square_argb()
    }

    #[cfg(test)]
    pub(crate) fn test_argb8888_shm(
        id: WlBufferId,
        owner_client_id: ClientId,
        width: i32,
        height: i32,
        pixels: Vec<u8>,
    ) -> Self {
        let stride = width.saturating_mul(ARGB8888.bpp as i32);
        let mem = super::wl_shm_pool::shared_memory_from_bytes(&pixels);
        Self {
            id,
            owner_client_id,
            destroyed: false,
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
        mut self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        use {crate::wayland_objects::wl_display::WlDisplay, wl_buffer::Request::*};

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                // The id is client-allocated, so the client may reuse it as soon
                // as it sees delete_id. The pixels can still be on screen, so the
                // object stays until nothing of this client references it.
                self.destroyed = true;
                let object_id = client.object_id(self.id);
                let resolved = Resolved::new(self.owner_client_id, self.id, object_id);
                client.release_client_id(&WlDisplay::default(), dispatcher, self.id.into());
                state.destroyed_buffers.push(resolved);
                dispatcher.dispatch_post_action(FreeDestroyedBuffers);
            }
        }
        Ok(())
    }
}

pub(crate) struct FreeDestroyedBuffers;

impl PostAction for FreeDestroyedBuffers {
    fn execute(
        self: Box<Self>,
        client: &mut Client,
        state: &mut State,
        _dispatcher: &mut ClientDispatcher,
    ) {
        state.free_unused_destroyed_buffers(client);
    }
}
