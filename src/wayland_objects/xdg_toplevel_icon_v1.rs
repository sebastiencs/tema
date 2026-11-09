//! `xdg_toplevel_icon_v1`. Buffers are copied on `add_buffer`. The object can be destroyed after.

use std::rc::Rc;

use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    object::{Object, RequestResult},
    protocol_generated::{xdg_toplevel_icon_v1, WaylandId, XdgToplevelIconV1Id},
    state::{GrabbedObject, State},
    toplevel_icon::{IconPixmap, ToplevelIcon},
    wayland_objects::{wl_buffer::IconBufferError, wl_display::WlDisplay},
};

const MAX_PIXMAPS: usize = 16;
const MAX_EDGE: u32 = 256;

#[derive(Debug)]
pub struct XdgToplevelIconV1 {
    id: XdgToplevelIconV1Id,
    icon: ToplevelIcon,
    frozen: Option<Rc<ToplevelIcon>>,
}

impl XdgToplevelIconV1 {
    pub(crate) fn new(id: XdgToplevelIconV1Id) -> Self {
        Self { id, icon: ToplevelIcon::default(), frozen: None }
    }

    /// First call moves the icon into an `Rc`. Later calls clone that `Rc`.
    pub(crate) fn freeze(&mut self) -> Rc<ToplevelIcon> {
        if let Some(frozen) = &self.frozen {
            return Rc::clone(frozen);
        }
        let icon = std::mem::take(&mut self.icon);
        let frozen = Rc::new(icon);
        self.frozen = Some(Rc::clone(&frozen));
        frozen
    }

    fn protocol_error(
        &self,
        client: &mut Client,
        dispatcher: &mut ClientDispatcher,
        code: xdg_toplevel_icon_v1::Error,
        message: &str,
    ) {
        client.protocol_violation_disconnect(dispatcher, self.id.into(), code as u32, message);
    }

    #[cfg(test)]
    pub(crate) fn pixmap_count(&self) -> usize {
        self.icon.pixmaps.len()
    }

    #[cfg(test)]
    pub(crate) fn pixmap_bytes(&self, size: u32, scale: u32) -> Option<&[u8]> {
        self.icon
            .pixmaps
            .iter()
            .find(|pixmap| pixmap.size == size && pixmap.scale == scale)
            .map(|pixmap| pixmap.argb8888.as_ref())
    }
}

impl Object for XdgToplevelIconV1 {
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
        use xdg_toplevel_icon_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            SetName { icon_name } => {
                if self.frozen.is_some() {
                    self.protocol_error(
                        client,
                        dispatcher,
                        xdg_toplevel_icon_v1::Error::Immutable,
                        "xdg_toplevel_icon_v1 is immutable",
                    );
                    return Ok(());
                }
                self.icon.name = (!icon_name.is_empty()).then_some(icon_name);
            }
            AddBuffer { buffer, scale } => {
                if self.frozen.is_some() {
                    self.protocol_error(
                        client,
                        dispatcher,
                        xdg_toplevel_icon_v1::Error::Immutable,
                        "xdg_toplevel_icon_v1 is immutable",
                    );
                    return Ok(());
                }
                let buffer = client.resolve_or_disconnect(buffer, state, dispatcher)?;
                if scale < 1 {
                    self.protocol_error(
                        client,
                        dispatcher,
                        xdg_toplevel_icon_v1::Error::InvalidBuffer,
                        "xdg_toplevel_icon_v1 buffer scale must be at least 1",
                    );
                    return Ok(());
                }
                let copied = buffer.rref(state).copy_shm_square_argb();
                let (size, pixels) = match copied {
                    Err(IconBufferError::NotShm | IconBufferError::NotSquare) => {
                        self.protocol_error(
                            client,
                            dispatcher,
                            xdg_toplevel_icon_v1::Error::InvalidBuffer,
                            "xdg_toplevel_icon_v1 buffer must be a square shm image",
                        );
                        return Ok(());
                    }
                    Err(IconBufferError::Format) => {
                        log::debug!(
                            "xdg_toplevel_icon_v1 buffer format is not argb8888 or xrgb8888"
                        );
                        return Ok(());
                    }
                    Err(
                        IconBufferError::Destroyed
                        | IconBufferError::Stride
                        | IconBufferError::Overflow,
                    ) => {
                        self.protocol_error(
                            client,
                            dispatcher,
                            xdg_toplevel_icon_v1::Error::InvalidBuffer,
                            "xdg_toplevel_icon_v1 buffer cannot be copied",
                        );
                        return Ok(());
                    }
                    Ok((size, _)) if size > MAX_EDGE => {
                        log::debug!(
                            "xdg_toplevel_icon_v1 buffer edge {size} is larger than {MAX_EDGE}"
                        );
                        return Ok(());
                    }
                    Ok(copied) => copied,
                };
                let scale = u32::try_from(scale).expect("scale was checked to be at least 1");
                if let Some(existing) = self
                    .icon
                    .pixmaps
                    .iter_mut()
                    .find(|pixmap| pixmap.size == size && pixmap.scale == scale)
                {
                    existing.argb8888 = pixels.into_boxed_slice();
                } else if self.icon.pixmaps.len() < MAX_PIXMAPS {
                    self.icon.pixmaps.push(IconPixmap {
                        size,
                        scale,
                        argb8888: pixels.into_boxed_slice(),
                    });
                }
                self.icon.pixmaps.sort_by_key(|pixmap| {
                    (u64::from(pixmap.size).saturating_mul(u64::from(pixmap.scale)), pixmap.scale)
                });
            }
        }
        Ok(())
    }
}
