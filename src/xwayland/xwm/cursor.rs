//! The root cursor, uploaded once the WM role is claimed.
//!
//! A missing image, a missing ARGB format, a request that does not fit, or a
//! reply error skips the cursor. The connection stays up.

use crate::{
    cursor::CursorImage,
    runner::Redispatcher,
    x11::{
        ChangeWindowAttributes, ChangeWindowAttributesAux, ConnectionError, CreateGc, CreateGcAux,
        CreatePixmap, Cursor, Drawable, FreeGc, FreePixmap, Gcontext, ImageFormat, PictType,
        Pictformat, Picture, Pixmap, PutImage, RenderCreateCursor, RenderCreatePicture,
        RenderCreatePictureAux, RenderFreePicture, RenderQueryPictFormats,
        RenderQueryPictFormatsReply, Window, XReplyError,
    },
};

use super::{Wm, WmCtx, WmEvent, WmFailure, WmPhase, Xwayland, XwaylandAction};

impl Wm {
    pub(super) fn begin_cursor(&mut self, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let Some(image) = ctx.state.seat.default_cursor_image() else {
            return Ok(());
        };
        let run = ctx.run;
        let sent = self.conn.send(
            &RenderQueryPictFormats,
            Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
                run,
                event: WmEvent::PictFormats(result),
            }),
        );
        match sent {
            Ok(_) => {
                let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
                ready.cursor = Some(image);
                Ok(())
            }
            Err(ConnectionError::RequestTooLong) => {
                log::debug!("skipping the X root cursor: the request is too long");
                Ok(())
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    pub(super) fn on_pict_formats(
        &mut self,
        result: Result<RenderQueryPictFormatsReply, XReplyError>,
    ) -> Result<(), WmFailure> {
        let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
        let Some(image) = ready.cursor.take() else { return Ok(()) };
        let root = ready.root;
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                log::debug!("skipping the X root cursor: {error}");
                return Ok(());
            }
        };
        let Some(format) = pick_argb32(&reply) else {
            log::debug!("skipping the X root cursor: no ARGB32 picture format");
            return Ok(());
        };
        self.upload(root, &image, format)
    }

    fn upload(
        &mut self,
        root: Window,
        image: &CursorImage,
        format: Pictformat,
    ) -> Result<(), WmFailure> {
        let Some((width, height)) = pixel_size(image) else {
            log::debug!("skipping the X root cursor: the image has no pixels");
            return Ok(());
        };
        let pixmap = Pixmap(self.fresh_id()?);
        let gc = Gcontext(self.fresh_id()?);
        let picture = Picture(self.fresh_id()?);
        let cursor = Cursor(self.fresh_id()?);
        let pixels = argb_words(image);
        let drawable = Drawable(root.0);
        if !self.send(&CreatePixmap { depth: 32, pid: pixmap, drawable, width, height })? {
            return Ok(());
        }
        if !self.send(&CreateGc {
            cid: gc,
            drawable: Drawable(pixmap.0),
            value_list: CreateGcAux::default(),
        })? {
            return Ok(());
        }
        if !self.send(&PutImage {
            format: ImageFormat::ZPixmap,
            drawable: Drawable(pixmap.0),
            gc,
            width,
            height,
            dst_x: 0,
            dst_y: 0,
            left_pad: 0,
            depth: 32,
            data: &pixels,
        })? {
            return Ok(());
        }
        if !self.send(&RenderCreatePicture {
            pid: picture,
            drawable: Drawable(pixmap.0),
            format,
            value_list: RenderCreatePictureAux::default(),
        })? {
            return Ok(());
        }
        if !self.send(&RenderCreateCursor {
            cid: cursor,
            source: picture,
            x: hotspot(image.hotspot_x),
            y: hotspot(image.hotspot_y),
        })? {
            return Ok(());
        }
        if !self.send(&ChangeWindowAttributes {
            window: root,
            value_list: ChangeWindowAttributesAux::default().cursor(cursor),
        })? {
            return Ok(());
        }
        if !self.send(&RenderFreePicture { picture })? {
            return Ok(());
        }
        if !self.send(&FreeGc { gc })? {
            return Ok(());
        }
        self.send(&FreePixmap { pixmap })?;
        Ok(())
    }

    fn fresh_id(&mut self) -> Result<u32, WmFailure> {
        match self.conn.generate_id() {
            Ok(id) => Ok(id),
            Err(error) => self.fail(error.to_string()),
        }
    }

    /// `Ok(false)` means the cursor upload stops here and the WM stays up.
    fn send(
        &mut self,
        request: &impl crate::x11::Request<Reply = crate::x11::NoReply>,
    ) -> Result<bool, WmFailure> {
        match self.conn.send_void(request) {
            Ok(_) => Ok(true),
            Err(ConnectionError::RequestTooLong) => {
                log::debug!("skipping the X root cursor: the request is too long");
                Ok(false)
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    fn fail<T>(&mut self, reason: String) -> Result<T, WmFailure> {
        self.phase = WmPhase::Failed;
        Err(WmFailure { reason })
    }
}

/// Native-endian `a<<24 | r<<16 | g<<8 | b` words. The source is already premultiplied.
pub(super) fn argb_words(image: &CursorImage) -> Vec<u8> {
    let mut words = Vec::with_capacity(image.pixels_rgba.len());
    for pixel in image.pixels_rgba.chunks_exact(4) {
        let word = (u32::from(pixel[3]) << 24)
            | (u32::from(pixel[0]) << 16)
            | (u32::from(pixel[1]) << 8)
            | u32::from(pixel[2]);
        words.extend(word.to_ne_bytes());
    }
    words
}

/// Depth 32, direct, alpha at 24, red at 16, green at 8, blue at 0, each mask `0xff`.
pub(super) fn pick_argb32(reply: &RenderQueryPictFormatsReply) -> Option<Pictformat> {
    reply
        .formats
        .iter()
        .find(|format| {
            format.depth == 32
                && format.r#type == PictType::Direct
                && channel(&format.direct, 24, 16, 8, 0)
        })
        .map(|format| format.id)
}

fn channel(direct: &crate::x11::Directformat, alpha: u16, red: u16, green: u16, blue: u16) -> bool {
    direct.alpha_shift == alpha
        && direct.alpha_mask == 0xff
        && direct.red_shift == red
        && direct.red_mask == 0xff
        && direct.green_shift == green
        && direct.green_mask == 0xff
        && direct.blue_shift == blue
        && direct.blue_mask == 0xff
}

fn pixel_size(image: &CursorImage) -> Option<(u16, u16)> {
    let width = u16::try_from(image.width).ok()?;
    let height = u16::try_from(image.height).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let pixels = usize::from(width) * usize::from(height) * 4;
    (image.pixels_rgba.len() == pixels).then_some((width, height))
}

fn hotspot(value: i32) -> u16 {
    u16::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use {
        super::{argb_words, pick_argb32},
        crate::{
            cursor::CursorImage,
            x11::{
                Colormap, Directformat, PictType, Pictformat, Pictforminfo,
                RenderQueryPictFormatsReply,
            },
        },
    };

    #[test]
    fn rgba_bytes_become_native_argb_words() {
        let image = CursorImage::new(
            "default".to_string(),
            1,
            2,
            0,
            0,
            1,
            vec![0x11, 0x22, 0x33, 0x44, 0xaa, 0xbb, 0xcc, 0xdd],
        );
        let mut expected = Vec::new();
        expected.extend(u32::to_ne_bytes(0x4411_2233));
        expected.extend(u32::to_ne_bytes(0xddaa_bbcc));
        assert_eq!(argb_words(&image), expected);
    }

    #[test]
    fn the_argb32_format_is_the_direct_depth_32_match() {
        let decoy = form(Pictformat(1), 24, 0, 0, 0, 0);
        let matched = form(Pictformat(0x51), 32, 24, 16, 8, 0);
        let formats = reply(vec![decoy, matched]);
        assert_eq!(pick_argb32(&formats), Some(Pictformat(0x51)));
        assert_eq!(pick_argb32(&reply(vec![decoy])), None);
    }

    fn form(
        id: Pictformat,
        depth: u8,
        alpha: u16,
        red: u16,
        green: u16,
        blue: u16,
    ) -> Pictforminfo {
        Pictforminfo {
            id,
            r#type: PictType::Direct,
            depth,
            direct: Directformat {
                red_shift: red,
                red_mask: 0xff,
                green_shift: green,
                green_mask: 0xff,
                blue_shift: blue,
                blue_mask: 0xff,
                alpha_shift: alpha,
                alpha_mask: 0xff,
            },
            colormap: Colormap(0),
        }
    }

    fn reply(formats: Vec<Pictforminfo>) -> RenderQueryPictFormatsReply {
        RenderQueryPictFormatsReply {
            sequence: 1,
            num_depths: 0,
            num_visuals: 0,
            formats,
            screens: Vec::new(),
            subpixels: Vec::new(),
        }
    }
}
