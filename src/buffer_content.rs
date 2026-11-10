use std::rc::Rc;

use crate::{
    clients::Resolved,
    dmabuf::DmaBuf,
    drm::{Format, ARGB8888},
    protocol_generated::WlBufferId,
    state::State,
    wayland_objects::wl_shm_pool::SharedMemory,
};

/// A buffer as a surface uses it. `object` names the `wl_buffer` (identity and
/// release events) and goes stale when the client destroys it. `content` keeps
/// the pixels alive for as long as this attachment exists.
#[derive(Clone, Debug)]
pub struct AttachedBuffer {
    pub object: Resolved<WlBufferId>,
    pub content: Rc<BufferContent>,
}

impl PartialEq for AttachedBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.object == other.object
    }
}

impl Eq for AttachedBuffer {}

impl AttachedBuffer {
    /// Clone the pixels from a live `wl_buffer`. `None` when that object is gone.
    pub fn resolve(state: &State, object: Resolved<WlBufferId>) -> Option<Self> {
        let content = object.try_ref(state)?.content().clone();
        Some(Self { object, content })
    }

    /// Geometry-only attachment for tests whose buffer id is not a real `wl_buffer`.
    #[cfg(test)]
    pub(crate) fn placeholder(object: Resolved<WlBufferId>, width: i32, height: i32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let stride = width.saturating_mul(4);
        let bytes = vec![0u8; (stride as usize).saturating_mul(height as usize)];
        let mem = crate::wayland_objects::wl_shm_pool::shared_memory_from_bytes(&bytes);
        let content = BufferContent::new_shm(0, width, height, stride, ARGB8888, mem)
            .expect("placeholder buffer");
        Self { object, content }
    }
}

/// Pixels behind a `wl_buffer`, shared by the protocol object and every
/// surface state or render snapshot that shows it. Dropped, and its dma-buf
/// fds or SHM mapping released, when the last holder goes away; the
/// `wl_buffer` id itself may be long gone by then.
#[derive(Debug)]
pub struct BufferContent {
    backing: BufferBacking,
    /// `create_immed` import failure: the buffer exists, its pixels are unusable.
    import_rejected: bool,
}

#[derive(Debug)]
enum BufferBacking {
    Shm {
        offset: usize,
        width: usize,
        height: usize,
        stride: usize,
        format: &'static Format,
        mem: Rc<SharedMemory>,
    },
    Dmabuf {
        dmabuf: DmaBuf,
        y_inverted: bool,
    },
}

/// One shared-memory sample. The bytes are borrowed from the buffer's pool.
#[derive(Debug)]
pub(crate) struct ShmSample<'a> {
    pub bytes: &'a [u8],
    pub width: i32,
    pub height: i32,
    pub stride: usize,
    pub fourcc: drm::buffer::DrmFourcc,
}

#[derive(Clone, Debug)]
pub struct CursorShmBuffer {
    mem: Rc<SharedMemory>,
    offset: usize,
    width: usize,
    height: usize,
    stride: usize,
}

impl CursorShmBuffer {
    pub(crate) fn stride(&self) -> usize {
        self.stride
    }

    pub(crate) fn argb_bytes(&self) -> Vec<u8> {
        let len = self.stride.saturating_mul(self.height);
        if self.offset.saturating_add(len) > self.mem.len() {
            return Vec::new();
        }
        self.mem.bytes_at(self.offset, len).to_vec()
    }

    pub fn width(&self) -> i32 {
        self.width as i32
    }

    pub fn height(&self) -> i32 {
        self.height as i32
    }

    pub fn copy_to_drm_argb8888_cursor_bytes(
        &self,
        output_pitch: usize,
        output_height: usize,
    ) -> Vec<u8> {
        let mut dst = vec![0; output_pitch.saturating_mul(output_height)];
        let row_bytes = self.width.saturating_mul(4);
        let src = self
            .mem
            .clone()
            .slice_at(self.offset, self.stride.saturating_mul(self.height));
        let src = src.as_slice();
        for row in 0..self.height {
            let src_start = row.saturating_mul(self.stride);
            let dst_start = row.saturating_mul(output_pitch);
            let src_row = &src[src_start..src_start + row_bytes];
            let dst_row = &mut dst[dst_start..dst_start + row_bytes];
            dst_row.copy_from_slice(src_row);
        }
        dst
    }
}

#[derive(Debug)]
pub enum WlBufferError {
    InvalidParams,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IconBufferError {
    NotShm,
    Format,
    NotSquare,
    Stride,
    Overflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureBufferError {
    NotShm,
    Format,
    Size,
    Stride,
    Overflow,
}

/// A checked writable view of one SHM buffer. Padding past each row's pixels stays untouched.
#[derive(Debug)]
pub struct CaptureShmWrite {
    mem: Rc<SharedMemory>,
    offset: usize,
    width: usize,
    height: usize,
    stride: usize,
}

impl CaptureShmWrite {
    pub(crate) fn write_top_left(
        &self,
        src: &[u8],
        src_stride: usize,
    ) -> Result<(), CaptureBufferError> {
        let row_bytes = self.width.saturating_mul(4);
        if src_stride < row_bytes {
            return Err(CaptureBufferError::Stride);
        }
        let src_bytes = src_stride
            .checked_mul(self.height)
            .ok_or(CaptureBufferError::Overflow)?;
        if src.len() < src_bytes {
            return Err(CaptureBufferError::Overflow);
        }
        let length = self.stride.saturating_mul(self.height);
        let dst = self.mem.bytes_at_mut(self.offset, length);
        for row in 0..self.height {
            let src_start = row.saturating_mul(src_stride);
            let dst_start = row.saturating_mul(self.stride);
            dst[dst_start..dst_start + row_bytes]
                .copy_from_slice(&src[src_start..src_start + row_bytes]);
        }
        Ok(())
    }

    pub(crate) fn stride_bytes(&self) -> usize {
        self.stride
    }

    #[cfg(test)]
    pub(crate) fn as_slice(&self) -> &[u8] {
        let length = self.stride.saturating_mul(self.height);
        self.mem.bytes_at(self.offset, length)
    }
}

impl BufferContent {
    pub fn new_shm(
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: &'static Format,
        mem: Rc<SharedMemory>,
    ) -> Result<Rc<Self>, WlBufferError> {
        Ok(Rc::new(Self {
            import_rejected: false,
            backing: BufferBacking::Shm {
                offset: offset
                    .try_into()
                    .map_err(|_| WlBufferError::InvalidParams)?,
                width: width.try_into().map_err(|_| WlBufferError::InvalidParams)?,
                height: height
                    .try_into()
                    .map_err(|_| WlBufferError::InvalidParams)?,
                stride: stride
                    .try_into()
                    .map_err(|_| WlBufferError::InvalidParams)?,
                format,
                mem,
            },
        }))
    }

    pub fn new_dmabuf(dmabuf: DmaBuf, y_inverted: bool, import_rejected: bool) -> Rc<Self> {
        Rc::new(Self {
            import_rejected,
            backing: BufferBacking::Dmabuf { dmabuf, y_inverted },
        })
    }

    pub(crate) fn import_rejected(&self) -> bool {
        self.import_rejected
    }

    pub fn dimensions(&self) -> (i32, i32) {
        match &self.backing {
            BufferBacking::Shm { width, height, .. } => (*width as i32, *height as i32),
            BufferBacking::Dmabuf { dmabuf, .. } => (dmabuf.width, dmabuf.height),
        }
    }

    pub fn format(&self) -> &'static Format {
        match &self.backing {
            BufferBacking::Shm { format, .. } => format,
            BufferBacking::Dmabuf { dmabuf, .. } => dmabuf.format,
        }
    }

    pub fn dmabuf(&self) -> Option<&DmaBuf> {
        match &self.backing {
            BufferBacking::Shm { .. } => None,
            BufferBacking::Dmabuf { dmabuf, .. } => Some(dmabuf),
        }
    }

    pub fn y_inverted(&self) -> bool {
        match &self.backing {
            BufferBacking::Shm { .. } => true,
            BufferBacking::Dmabuf { y_inverted, .. } => *y_inverted,
        }
    }

    pub fn supports_explicit_sync(&self) -> bool {
        matches!(self.backing, BufferBacking::Dmabuf { .. })
    }

    pub fn base_tex_transform(&self) -> crate::transform::Mat3 {
        match &self.backing {
            BufferBacking::Shm { .. } => crate::transform::Mat3::identity(),
            BufferBacking::Dmabuf { y_inverted, .. } => {
                if *y_inverted {
                    crate::transform::normalized_texcoord_flip_y()
                } else {
                    crate::transform::Mat3::identity()
                }
            }
        }
    }

    /// Shared-memory pixels for one sample. The client can rewrite them between commits.
    pub(crate) fn shm_sample(&self) -> Option<ShmSample<'_>> {
        let BufferBacking::Shm { offset, width, height, stride, format, mem } = &self.backing
        else {
            return None;
        };
        let bytes = stride.saturating_mul(*height);
        let end = offset.checked_add(bytes)?;
        if end > mem.len() {
            return None;
        }
        Some(ShmSample {
            bytes: mem.bytes_at(*offset, bytes),
            width: *width as i32,
            height: *height as i32,
            stride: *stride,
            fourcc: format.drm,
        })
    }

    /// Accept an SHM `XRGB8888` or `ARGB8888` destination for one capture.
    ///
    /// The buffer size must match the capture, the stride must hold each row, and the mapped
    /// pool must contain `stride * height` bytes from the buffer offset.
    pub fn begin_capture_write(
        &self,
        width: i32,
        height: i32,
    ) -> Result<CaptureShmWrite, CaptureBufferError> {
        let BufferBacking::Shm {
            offset,
            width: buffer_width,
            height: buffer_height,
            stride,
            format,
            mem,
            ..
        } = &self.backing
        else {
            return Err(CaptureBufferError::NotShm);
        };
        if format.drm != ARGB8888.drm && format.drm != crate::drm::XRGB8888.drm {
            return Err(CaptureBufferError::Format);
        }
        if width <= 0
            || height <= 0
            || *buffer_width != width as usize
            || *buffer_height != height as usize
        {
            return Err(CaptureBufferError::Size);
        }
        let row_bytes = buffer_width.saturating_mul(4);
        if *stride < row_bytes || stride % 4 != 0 {
            return Err(CaptureBufferError::Stride);
        }
        let bytes = stride
            .checked_mul(*buffer_height)
            .ok_or(CaptureBufferError::Overflow)?;
        let end = offset
            .checked_add(bytes)
            .ok_or(CaptureBufferError::Overflow)?;
        if end > mem.len() {
            return Err(CaptureBufferError::Overflow);
        }
        Ok(CaptureShmWrite {
            mem: mem.clone(),
            offset: *offset,
            width: *buffer_width,
            height: *buffer_height,
            stride: *stride,
        })
    }

    /// Square SHM icon pixels, tightly packed. `XRGB8888` alpha is forced to opaque.
    pub(crate) fn copy_shm_square_argb(&self) -> Result<(u32, Vec<u8>), IconBufferError> {
        let BufferBacking::Shm { offset, width, height, stride, format, mem } = &self.backing
        else {
            return Err(IconBufferError::NotShm);
        };
        if format.drm != crate::drm::ARGB8888.drm && format.drm != crate::drm::XRGB8888.drm {
            return Err(IconBufferError::Format);
        }
        if *width == 0 || width != height {
            return Err(IconBufferError::NotSquare);
        }
        let row = width.checked_mul(4).ok_or(IconBufferError::Overflow)?;
        if *stride < row {
            return Err(IconBufferError::Stride);
        }
        let len = stride
            .checked_mul(*height)
            .ok_or(IconBufferError::Overflow)?;
        let end = offset.checked_add(len).ok_or(IconBufferError::Overflow)?;
        if end > mem.len() {
            return Err(IconBufferError::Overflow);
        }
        let tight = row.checked_mul(*height).ok_or(IconBufferError::Overflow)?;
        let src = mem.bytes_at(*offset, len);
        let mut out = Vec::with_capacity(tight);
        for y in 0..*height {
            let start = y * *stride;
            out.extend_from_slice(&src[start..start + row]);
        }
        if format.drm == crate::drm::XRGB8888.drm {
            for px in out.chunks_exact_mut(4) {
                px[3] = 0xff;
            }
        }
        Ok((u32::try_from(*width).map_err(|_| IconBufferError::Overflow)?, out))
    }

    pub fn shm_cursor_argb8888(&self) -> Option<CursorShmBuffer> {
        let BufferBacking::Shm { offset, width, height, stride, format, mem, .. } = &self.backing
        else {
            return None;
        };
        if format.drm != ARGB8888.drm {
            return None;
        }
        Some(CursorShmBuffer {
            mem: mem.clone(),
            offset: *offset,
            width: *width,
            height: *height,
            stride: *stride,
        })
    }
}
