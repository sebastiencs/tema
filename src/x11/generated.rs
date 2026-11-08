//! Generated from the installed xcb-proto descriptions. Do not edit.
//!
//! Rebuild with `cargo test regenerate_x11_generated -- --ignored`.
//!
//! `serialize` and `parse` speak the machine's native byte order. Alignment is
//! measured from the start of the reader or writer, which is the start of the
//! value being converted.

#![allow(non_upper_case_globals)]

use super::wire::{Reader, WireError, Writer};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Window(pub u32);

impl Window {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.0);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(reader.get_u32()?))
    }
    pub(crate) const None: Self = Self(0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Pixmap(pub u32);

impl Pixmap {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.0);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(reader.get_u32()?))
    }
    pub(crate) const None: Self = Self(0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Cursor(pub u32);

impl Cursor {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.0);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(reader.get_u32()?))
    }
    pub(crate) const None: Self = Self(0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Font(pub u32);

impl Font {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.0);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(reader.get_u32()?))
    }
    pub(crate) const None: Self = Self(0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Gcontext(pub u32);

impl Gcontext {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.0);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(reader.get_u32()?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Colormap(pub u32);

impl Colormap {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.0);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(reader.get_u32()?))
    }
    pub(crate) const None: Self = Self(0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Atom(pub u32);

impl Atom {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.0);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(reader.get_u32()?))
    }
    pub(crate) const None: Self = Self(0);
    pub(crate) const Any: Self = Self(0);
    pub(crate) const PRIMARY: Self = Self(1);
    pub(crate) const SECONDARY: Self = Self(2);
    pub(crate) const ARC: Self = Self(3);
    pub(crate) const ATOM: Self = Self(4);
    pub(crate) const BITMAP: Self = Self(5);
    pub(crate) const CARDINAL: Self = Self(6);
    pub(crate) const COLORMAP: Self = Self(7);
    pub(crate) const CURSOR: Self = Self(8);
    pub(crate) const CUT_BUFFER0: Self = Self(9);
    pub(crate) const CUT_BUFFER1: Self = Self(10);
    pub(crate) const CUT_BUFFER2: Self = Self(11);
    pub(crate) const CUT_BUFFER3: Self = Self(12);
    pub(crate) const CUT_BUFFER4: Self = Self(13);
    pub(crate) const CUT_BUFFER5: Self = Self(14);
    pub(crate) const CUT_BUFFER6: Self = Self(15);
    pub(crate) const CUT_BUFFER7: Self = Self(16);
    pub(crate) const DRAWABLE: Self = Self(17);
    pub(crate) const FONT: Self = Self(18);
    pub(crate) const INTEGER: Self = Self(19);
    pub(crate) const PIXMAP: Self = Self(20);
    pub(crate) const POINT: Self = Self(21);
    pub(crate) const RECTANGLE: Self = Self(22);
    pub(crate) const RESOURCE_MANAGER: Self = Self(23);
    pub(crate) const RGB_COLOR_MAP: Self = Self(24);
    pub(crate) const RGB_BEST_MAP: Self = Self(25);
    pub(crate) const RGB_BLUE_MAP: Self = Self(26);
    pub(crate) const RGB_DEFAULT_MAP: Self = Self(27);
    pub(crate) const RGB_GRAY_MAP: Self = Self(28);
    pub(crate) const RGB_GREEN_MAP: Self = Self(29);
    pub(crate) const RGB_RED_MAP: Self = Self(30);
    pub(crate) const STRING: Self = Self(31);
    pub(crate) const VISUALID: Self = Self(32);
    pub(crate) const WINDOW: Self = Self(33);
    pub(crate) const WM_COMMAND: Self = Self(34);
    pub(crate) const WM_HINTS: Self = Self(35);
    pub(crate) const WM_CLIENT_MACHINE: Self = Self(36);
    pub(crate) const WM_ICON_NAME: Self = Self(37);
    pub(crate) const WM_ICON_SIZE: Self = Self(38);
    pub(crate) const WM_NAME: Self = Self(39);
    pub(crate) const WM_NORMAL_HINTS: Self = Self(40);
    pub(crate) const WM_SIZE_HINTS: Self = Self(41);
    pub(crate) const WM_ZOOM_HINTS: Self = Self(42);
    pub(crate) const MIN_SPACE: Self = Self(43);
    pub(crate) const NORM_SPACE: Self = Self(44);
    pub(crate) const MAX_SPACE: Self = Self(45);
    pub(crate) const END_SPACE: Self = Self(46);
    pub(crate) const SUPERSCRIPT_X: Self = Self(47);
    pub(crate) const SUPERSCRIPT_Y: Self = Self(48);
    pub(crate) const SUBSCRIPT_X: Self = Self(49);
    pub(crate) const SUBSCRIPT_Y: Self = Self(50);
    pub(crate) const UNDERLINE_POSITION: Self = Self(51);
    pub(crate) const UNDERLINE_THICKNESS: Self = Self(52);
    pub(crate) const STRIKEOUT_ASCENT: Self = Self(53);
    pub(crate) const STRIKEOUT_DESCENT: Self = Self(54);
    pub(crate) const ITALIC_ANGLE: Self = Self(55);
    pub(crate) const X_HEIGHT: Self = Self(56);
    pub(crate) const QUAD_WIDTH: Self = Self(57);
    pub(crate) const WEIGHT: Self = Self(58);
    pub(crate) const POINT_SIZE: Self = Self(59);
    pub(crate) const RESOLUTION: Self = Self(60);
    pub(crate) const COPYRIGHT: Self = Self(61);
    pub(crate) const NOTICE: Self = Self(62);
    pub(crate) const FONT_NAME: Self = Self(63);
    pub(crate) const FAMILY_NAME: Self = Self(64);
    pub(crate) const FULL_NAME: Self = Self(65);
    pub(crate) const CAP_HEIGHT: Self = Self(66);
    pub(crate) const WM_CLASS: Self = Self(67);
    pub(crate) const WM_TRANSIENT_FOR: Self = Self(68);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Drawable(pub u32);

impl Drawable {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.0);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(reader.get_u32()?))
    }
}

impl From<Window> for Drawable {
    fn from(value: Window) -> Self {
        Self(value.0)
    }
}

impl From<Pixmap> for Drawable {
    fn from(value: Pixmap) -> Self {
        Self(value.0)
    }
}

pub(crate) type Bool32 = u32;

pub(crate) type Visualid = u32;

pub(crate) type Timestamp = u32;

pub(crate) type Keycode = u8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Format {
    pub(crate) depth: u8,
    pub(crate) bits_per_pixel: u8,
    pub(crate) scanline_pad: u8,
}

impl Format {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u8(self.depth);
        writer.u8(self.bits_per_pixel);
        writer.u8(self.scanline_pad);
        writer.pad(5);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let depth = reader.get_u8()?;
        let bits_per_pixel = reader.get_u8()?;
        let scanline_pad = reader.get_u8()?;
        reader.skip(5)?;
        Ok(Self { depth, bits_per_pixel, scanline_pad })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct VisualClass(pub u32);

impl VisualClass {
    pub(crate) const StaticGray: Self = Self(0);
    pub(crate) const GrayScale: Self = Self(1);
    pub(crate) const StaticColor: Self = Self(2);
    pub(crate) const PseudoColor: Self = Self(3);
    pub(crate) const TrueColor: Self = Self(4);
    pub(crate) const DirectColor: Self = Self(5);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Visualtype {
    pub(crate) visual_id: Visualid,
    pub(crate) class: VisualClass,
    pub(crate) bits_per_rgb_value: u8,
    pub(crate) colormap_entries: u16,
    pub(crate) red_mask: u32,
    pub(crate) green_mask: u32,
    pub(crate) blue_mask: u32,
}

impl Visualtype {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.visual_id);
        writer.u8(self.class.0 as u8);
        writer.u8(self.bits_per_rgb_value);
        writer.u16(self.colormap_entries);
        writer.u32(self.red_mask);
        writer.u32(self.green_mask);
        writer.u32(self.blue_mask);
        writer.pad(4);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let visual_id = reader.get_u32()?;
        let class = VisualClass(u32::from(reader.get_u8()?));
        let bits_per_rgb_value = reader.get_u8()?;
        let colormap_entries = reader.get_u16()?;
        let red_mask = reader.get_u32()?;
        let green_mask = reader.get_u32()?;
        let blue_mask = reader.get_u32()?;
        reader.skip(4)?;
        Ok(Self {
            visual_id,
            class,
            bits_per_rgb_value,
            colormap_entries,
            red_mask,
            green_mask,
            blue_mask,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Depth {
    pub(crate) depth: u8,
    pub(crate) visuals_len: u16,
    pub(crate) visuals: Vec<Visualtype>,
}

impl Depth {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u8(self.depth);
        writer.pad(1);
        writer.u16(self.visuals_len);
        writer.pad(4);
        debug_assert_eq!(self.visuals.len(), (self.visuals_len as usize));
        for item in &self.visuals {
            item.serialize(writer);
        }
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let depth = reader.get_u8()?;
        reader.skip(1)?;
        let visuals_len = reader.get_u16()?;
        reader.skip(4)?;
        let visuals_count = visuals_len as usize;
        let mut visuals = Vec::with_capacity(visuals_count);
        for _ in 0..visuals_count {
            visuals.push(Visualtype::parse(reader)?);
        }
        Ok(Self { depth, visuals_len, visuals })
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub(crate) struct EventMask: u32 {
        const NoEvent = 0u32;
        const KeyPress = 1u32 << 0;
        const KeyRelease = 1u32 << 1;
        const ButtonPress = 1u32 << 2;
        const ButtonRelease = 1u32 << 3;
        const EnterWindow = 1u32 << 4;
        const LeaveWindow = 1u32 << 5;
        const PointerMotion = 1u32 << 6;
        const PointerMotionHint = 1u32 << 7;
        const Button1Motion = 1u32 << 8;
        const Button2Motion = 1u32 << 9;
        const Button3Motion = 1u32 << 10;
        const Button4Motion = 1u32 << 11;
        const Button5Motion = 1u32 << 12;
        const ButtonMotion = 1u32 << 13;
        const KeymapState = 1u32 << 14;
        const Exposure = 1u32 << 15;
        const VisibilityChange = 1u32 << 16;
        const StructureNotify = 1u32 << 17;
        const ResizeRedirect = 1u32 << 18;
        const SubstructureNotify = 1u32 << 19;
        const SubstructureRedirect = 1u32 << 20;
        const FocusChange = 1u32 << 21;
        const PropertyChange = 1u32 << 22;
        const ColorMapChange = 1u32 << 23;
        const OwnerGrabButton = 1u32 << 24;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BackingStore(pub u32);

impl BackingStore {
    pub(crate) const NotUseful: Self = Self(0);
    pub(crate) const WhenMapped: Self = Self(1);
    pub(crate) const Always: Self = Self(2);
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Screen {
    pub(crate) root: Window,
    pub(crate) default_colormap: Colormap,
    pub(crate) white_pixel: u32,
    pub(crate) black_pixel: u32,
    pub(crate) current_input_masks: EventMask,
    pub(crate) width_in_pixels: u16,
    pub(crate) height_in_pixels: u16,
    pub(crate) width_in_millimeters: u16,
    pub(crate) height_in_millimeters: u16,
    pub(crate) min_installed_maps: u16,
    pub(crate) max_installed_maps: u16,
    pub(crate) root_visual: Visualid,
    pub(crate) backing_stores: BackingStore,
    pub(crate) save_unders: u8,
    pub(crate) root_depth: u8,
    pub(crate) allowed_depths_len: u8,
    pub(crate) allowed_depths: Vec<Depth>,
}

impl Screen {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        self.root.serialize(writer);
        self.default_colormap.serialize(writer);
        writer.u32(self.white_pixel);
        writer.u32(self.black_pixel);
        writer.u32(self.current_input_masks.bits());
        writer.u16(self.width_in_pixels);
        writer.u16(self.height_in_pixels);
        writer.u16(self.width_in_millimeters);
        writer.u16(self.height_in_millimeters);
        writer.u16(self.min_installed_maps);
        writer.u16(self.max_installed_maps);
        writer.u32(self.root_visual);
        writer.u8(self.backing_stores.0 as u8);
        writer.u8(self.save_unders);
        writer.u8(self.root_depth);
        writer.u8(self.allowed_depths_len);
        debug_assert_eq!(self.allowed_depths.len(), (self.allowed_depths_len as usize));
        for item in &self.allowed_depths {
            item.serialize(writer);
        }
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let root = Window::parse(reader)?;
        let default_colormap = Colormap::parse(reader)?;
        let white_pixel = reader.get_u32()?;
        let black_pixel = reader.get_u32()?;
        let current_input_masks = EventMask::from_bits_retain(reader.get_u32()?);
        let width_in_pixels = reader.get_u16()?;
        let height_in_pixels = reader.get_u16()?;
        let width_in_millimeters = reader.get_u16()?;
        let height_in_millimeters = reader.get_u16()?;
        let min_installed_maps = reader.get_u16()?;
        let max_installed_maps = reader.get_u16()?;
        let root_visual = reader.get_u32()?;
        let backing_stores = BackingStore(u32::from(reader.get_u8()?));
        let save_unders = reader.get_u8()?;
        let root_depth = reader.get_u8()?;
        let allowed_depths_len = reader.get_u8()?;
        let allowed_depths_count = allowed_depths_len as usize;
        let mut allowed_depths = Vec::with_capacity(allowed_depths_count);
        for _ in 0..allowed_depths_count {
            allowed_depths.push(Depth::parse(reader)?);
        }
        Ok(Self {
            root,
            default_colormap,
            white_pixel,
            black_pixel,
            current_input_masks,
            width_in_pixels,
            height_in_pixels,
            width_in_millimeters,
            height_in_millimeters,
            min_installed_maps,
            max_installed_maps,
            root_visual,
            backing_stores,
            save_unders,
            root_depth,
            allowed_depths_len,
            allowed_depths,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SetupRequest {
    pub(crate) byte_order: u8,
    pub(crate) protocol_major_version: u16,
    pub(crate) protocol_minor_version: u16,
    pub(crate) authorization_protocol_name_len: u16,
    pub(crate) authorization_protocol_data_len: u16,
    pub(crate) authorization_protocol_name: Vec<u8>,
    pub(crate) authorization_protocol_data: Vec<u8>,
}

impl SetupRequest {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u8(self.byte_order);
        writer.pad(1);
        writer.u16(self.protocol_major_version);
        writer.u16(self.protocol_minor_version);
        writer.u16(self.authorization_protocol_name_len);
        writer.u16(self.authorization_protocol_data_len);
        writer.pad(2);
        debug_assert_eq!(
            self.authorization_protocol_name.len(),
            (self.authorization_protocol_name_len as usize)
        );
        writer.bytes(&self.authorization_protocol_name);
        writer.align(4);
        debug_assert_eq!(
            self.authorization_protocol_data.len(),
            (self.authorization_protocol_data_len as usize)
        );
        writer.bytes(&self.authorization_protocol_data);
        writer.align(4);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let byte_order = reader.get_u8()?;
        reader.skip(1)?;
        let protocol_major_version = reader.get_u16()?;
        let protocol_minor_version = reader.get_u16()?;
        let authorization_protocol_name_len = reader.get_u16()?;
        let authorization_protocol_data_len = reader.get_u16()?;
        reader.skip(2)?;
        let authorization_protocol_name_count = authorization_protocol_name_len as usize;
        let authorization_protocol_name = reader.bytes(authorization_protocol_name_count)?.to_vec();
        reader.align(4)?;
        let authorization_protocol_data_count = authorization_protocol_data_len as usize;
        let authorization_protocol_data = reader.bytes(authorization_protocol_data_count)?.to_vec();
        reader.align(4)?;
        Ok(Self {
            byte_order,
            protocol_major_version,
            protocol_minor_version,
            authorization_protocol_name_len,
            authorization_protocol_data_len,
            authorization_protocol_name,
            authorization_protocol_data,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SetupFailed {
    pub(crate) status: u8,
    pub(crate) reason_len: u8,
    pub(crate) protocol_major_version: u16,
    pub(crate) protocol_minor_version: u16,
    pub(crate) length: u16,
    pub(crate) reason: Vec<u8>,
}

impl SetupFailed {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u8(self.status);
        writer.u8(self.reason_len);
        writer.u16(self.protocol_major_version);
        writer.u16(self.protocol_minor_version);
        writer.u16(self.length);
        debug_assert_eq!(self.reason.len(), (self.reason_len as usize));
        writer.bytes(&self.reason);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let status = reader.get_u8()?;
        let reason_len = reader.get_u8()?;
        let protocol_major_version = reader.get_u16()?;
        let protocol_minor_version = reader.get_u16()?;
        let length = reader.get_u16()?;
        let reason_count = reason_len as usize;
        let reason = reader.bytes(reason_count)?.to_vec();
        Ok(Self {
            status,
            reason_len,
            protocol_major_version,
            protocol_minor_version,
            length,
            reason,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SetupAuthenticate {
    pub(crate) status: u8,
    pub(crate) length: u16,
    pub(crate) reason: Vec<u8>,
}

impl SetupAuthenticate {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u8(self.status);
        writer.pad(5);
        writer.u16(self.length);
        debug_assert_eq!(self.reason.len(), ((self.length as usize) * 4usize));
        writer.bytes(&self.reason);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let status = reader.get_u8()?;
        reader.skip(5)?;
        let length = reader.get_u16()?;
        let reason_count = (length as usize) * 4usize;
        let reason = reader.bytes(reason_count)?.to_vec();
        Ok(Self { status, length, reason })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ImageOrder(pub u32);

impl ImageOrder {
    pub(crate) const LSBFirst: Self = Self(0);
    pub(crate) const MSBFirst: Self = Self(1);
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Setup {
    pub(crate) status: u8,
    pub(crate) protocol_major_version: u16,
    pub(crate) protocol_minor_version: u16,
    pub(crate) length: u16,
    pub(crate) release_number: u32,
    pub(crate) resource_id_base: u32,
    pub(crate) resource_id_mask: u32,
    pub(crate) motion_buffer_size: u32,
    pub(crate) vendor_len: u16,
    pub(crate) maximum_request_length: u16,
    pub(crate) roots_len: u8,
    pub(crate) pixmap_formats_len: u8,
    pub(crate) image_byte_order: ImageOrder,
    pub(crate) bitmap_format_bit_order: ImageOrder,
    pub(crate) bitmap_format_scanline_unit: u8,
    pub(crate) bitmap_format_scanline_pad: u8,
    pub(crate) min_keycode: Keycode,
    pub(crate) max_keycode: Keycode,
    pub(crate) vendor: Vec<u8>,
    pub(crate) pixmap_formats: Vec<Format>,
    pub(crate) roots: Vec<Screen>,
}

impl Setup {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u8(self.status);
        writer.pad(1);
        writer.u16(self.protocol_major_version);
        writer.u16(self.protocol_minor_version);
        writer.u16(self.length);
        writer.u32(self.release_number);
        writer.u32(self.resource_id_base);
        writer.u32(self.resource_id_mask);
        writer.u32(self.motion_buffer_size);
        writer.u16(self.vendor_len);
        writer.u16(self.maximum_request_length);
        writer.u8(self.roots_len);
        writer.u8(self.pixmap_formats_len);
        writer.u8(self.image_byte_order.0 as u8);
        writer.u8(self.bitmap_format_bit_order.0 as u8);
        writer.u8(self.bitmap_format_scanline_unit);
        writer.u8(self.bitmap_format_scanline_pad);
        writer.u8(self.min_keycode);
        writer.u8(self.max_keycode);
        writer.pad(4);
        debug_assert_eq!(self.vendor.len(), (self.vendor_len as usize));
        writer.bytes(&self.vendor);
        writer.align(4);
        debug_assert_eq!(self.pixmap_formats.len(), (self.pixmap_formats_len as usize));
        for item in &self.pixmap_formats {
            item.serialize(writer);
        }
        debug_assert_eq!(self.roots.len(), (self.roots_len as usize));
        for item in &self.roots {
            item.serialize(writer);
        }
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let status = reader.get_u8()?;
        reader.skip(1)?;
        let protocol_major_version = reader.get_u16()?;
        let protocol_minor_version = reader.get_u16()?;
        let length = reader.get_u16()?;
        let release_number = reader.get_u32()?;
        let resource_id_base = reader.get_u32()?;
        let resource_id_mask = reader.get_u32()?;
        let motion_buffer_size = reader.get_u32()?;
        let vendor_len = reader.get_u16()?;
        let maximum_request_length = reader.get_u16()?;
        let roots_len = reader.get_u8()?;
        let pixmap_formats_len = reader.get_u8()?;
        let image_byte_order = ImageOrder(u32::from(reader.get_u8()?));
        let bitmap_format_bit_order = ImageOrder(u32::from(reader.get_u8()?));
        let bitmap_format_scanline_unit = reader.get_u8()?;
        let bitmap_format_scanline_pad = reader.get_u8()?;
        let min_keycode = reader.get_u8()?;
        let max_keycode = reader.get_u8()?;
        reader.skip(4)?;
        let vendor_count = vendor_len as usize;
        let vendor = reader.bytes(vendor_count)?.to_vec();
        reader.align(4)?;
        let pixmap_formats_count = pixmap_formats_len as usize;
        let mut pixmap_formats = Vec::with_capacity(pixmap_formats_count);
        for _ in 0..pixmap_formats_count {
            pixmap_formats.push(Format::parse(reader)?);
        }
        let roots_count = roots_len as usize;
        let mut roots = Vec::with_capacity(roots_count);
        for _ in 0..roots_count {
            roots.push(Screen::parse(reader)?);
        }
        Ok(Self {
            status,
            protocol_major_version,
            protocol_minor_version,
            length,
            release_number,
            resource_id_base,
            resource_id_mask,
            motion_buffer_size,
            vendor_len,
            maximum_request_length,
            roots_len,
            pixmap_formats_len,
            image_byte_order,
            bitmap_format_bit_order,
            bitmap_format_scanline_unit,
            bitmap_format_scanline_pad,
            min_keycode,
            max_keycode,
            vendor,
            pixmap_formats,
            roots,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NotifyDetail(pub u32);

impl NotifyDetail {
    pub(crate) const Ancestor: Self = Self(0);
    pub(crate) const Virtual: Self = Self(1);
    pub(crate) const Inferior: Self = Self(2);
    pub(crate) const Nonlinear: Self = Self(3);
    pub(crate) const NonlinearVirtual: Self = Self(4);
    pub(crate) const Pointer: Self = Self(5);
    pub(crate) const PointerRoot: Self = Self(6);
    pub(crate) const None: Self = Self(7);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NotifyMode(pub u32);

impl NotifyMode {
    pub(crate) const Normal: Self = Self(0);
    pub(crate) const Grab: Self = Self(1);
    pub(crate) const Ungrab: Self = Self(2);
    pub(crate) const WhileGrabbed: Self = Self(3);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Property(pub u32);

impl Property {
    pub(crate) const NewValue: Self = Self(0);
    pub(crate) const Delete: Self = Self(1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Time(pub u32);

impl Time {
    pub(crate) const CurrentTime: Self = Self(0);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct ClientMessageData {
    bytes: [u8; 20],
}

impl ClientMessageData {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.bytes(&self.bytes);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let mut bytes = [0; 20];
        bytes.copy_from_slice(reader.bytes(20)?);
        Ok(Self { bytes })
    }

    pub(crate) fn data8(&self) -> [u8; 20] {
        let mut out = [0; 20];
        for (index, slot) in out.iter_mut().enumerate() {
            let start = index * 1;
            *slot = self.bytes[start];
        }
        out
    }

    pub(crate) fn set_data8(&mut self, value: [u8; 20]) {
        self.bytes = [0; 20];
        for (index, slot) in value.into_iter().enumerate() {
            let start = index * 1;
            self.bytes[start..start + 1].copy_from_slice(&slot.to_ne_bytes());
        }
    }

    pub(crate) fn data16(&self) -> [u16; 10] {
        let mut out = [0; 10];
        for (index, slot) in out.iter_mut().enumerate() {
            let start = index * 2;
            *slot = u16::from_ne_bytes(self.bytes[start..start + 2].try_into().unwrap());
        }
        out
    }

    pub(crate) fn set_data16(&mut self, value: [u16; 10]) {
        self.bytes = [0; 20];
        for (index, slot) in value.into_iter().enumerate() {
            let start = index * 2;
            self.bytes[start..start + 2].copy_from_slice(&slot.to_ne_bytes());
        }
    }

    pub(crate) fn data32(&self) -> [u32; 5] {
        let mut out = [0; 5];
        for (index, slot) in out.iter_mut().enumerate() {
            let start = index * 4;
            *slot = u32::from_ne_bytes(self.bytes[start..start + 4].try_into().unwrap());
        }
        out
    }

    pub(crate) fn set_data32(&mut self, value: [u32; 5]) {
        self.bytes = [0; 20];
        for (index, slot) in value.into_iter().enumerate() {
            let start = index * 4;
            self.bytes[start..start + 4].copy_from_slice(&slot.to_ne_bytes());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WindowClass(pub u32);

impl WindowClass {
    pub(crate) const CopyFromParent: Self = Self(0);
    pub(crate) const InputOutput: Self = Self(1);
    pub(crate) const InputOnly: Self = Self(2);
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub(crate) struct Cw: u32 {
        const BackPixmap = 1u32 << 0;
        const BackPixel = 1u32 << 1;
        const BorderPixmap = 1u32 << 2;
        const BorderPixel = 1u32 << 3;
        const BitGravity = 1u32 << 4;
        const WinGravity = 1u32 << 5;
        const BackingStore = 1u32 << 6;
        const BackingPlanes = 1u32 << 7;
        const BackingPixel = 1u32 << 8;
        const OverrideRedirect = 1u32 << 9;
        const SaveUnder = 1u32 << 10;
        const EventMask = 1u32 << 11;
        const DontPropagate = 1u32 << 12;
        const Colormap = 1u32 << 13;
        const Cursor = 1u32 << 14;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BackPixmap(pub u32);

impl BackPixmap {
    pub(crate) const None: Self = Self(0);
    pub(crate) const ParentRelative: Self = Self(1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Gravity(pub u32);

impl Gravity {
    pub(crate) const BitForget: Self = Self(0);
    pub(crate) const WinUnmap: Self = Self(0);
    pub(crate) const NorthWest: Self = Self(1);
    pub(crate) const North: Self = Self(2);
    pub(crate) const NorthEast: Self = Self(3);
    pub(crate) const West: Self = Self(4);
    pub(crate) const Center: Self = Self(5);
    pub(crate) const East: Self = Self(6);
    pub(crate) const SouthWest: Self = Self(7);
    pub(crate) const South: Self = Self(8);
    pub(crate) const SouthEast: Self = Self(9);
    pub(crate) const Static: Self = Self(10);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MapState(pub u32);

impl MapState {
    pub(crate) const Unmapped: Self = Self(0);
    pub(crate) const Unviewable: Self = Self(1);
    pub(crate) const Viewable: Self = Self(2);
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub(crate) struct ConfigWindow: u32 {
        const X = 1u32 << 0;
        const Y = 1u32 << 1;
        const Width = 1u32 << 2;
        const Height = 1u32 << 3;
        const BorderWidth = 1u32 << 4;
        const Sibling = 1u32 << 5;
        const StackMode = 1u32 << 6;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct StackMode(pub u32);

impl StackMode {
    pub(crate) const Above: Self = Self(0);
    pub(crate) const Below: Self = Self(1);
    pub(crate) const TopIf: Self = Self(2);
    pub(crate) const BottomIf: Self = Self(3);
    pub(crate) const Opposite: Self = Self(4);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PropMode(pub u32);

impl PropMode {
    pub(crate) const Replace: Self = Self(0);
    pub(crate) const Prepend: Self = Self(1);
    pub(crate) const Append: Self = Self(2);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetPropertyType(pub u32);

impl GetPropertyType {
    pub(crate) const Any: Self = Self(0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SendEventDest(pub u32);

impl SendEventDest {
    pub(crate) const PointerWindow: Self = Self(0);
    pub(crate) const ItemFocus: Self = Self(1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InputFocus(pub u32);

impl InputFocus {
    pub(crate) const None: Self = Self(0);
    pub(crate) const PointerRoot: Self = Self(1);
    pub(crate) const Parent: Self = Self(2);
    pub(crate) const FollowKeyboard: Self = Self(3);
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub(crate) struct Gc: u32 {
        const Function = 1u32 << 0;
        const PlaneMask = 1u32 << 1;
        const Foreground = 1u32 << 2;
        const Background = 1u32 << 3;
        const LineWidth = 1u32 << 4;
        const LineStyle = 1u32 << 5;
        const CapStyle = 1u32 << 6;
        const JoinStyle = 1u32 << 7;
        const FillStyle = 1u32 << 8;
        const FillRule = 1u32 << 9;
        const Tile = 1u32 << 10;
        const Stipple = 1u32 << 11;
        const TileStippleOriginX = 1u32 << 12;
        const TileStippleOriginY = 1u32 << 13;
        const Font = 1u32 << 14;
        const SubwindowMode = 1u32 << 15;
        const GraphicsExposures = 1u32 << 16;
        const ClipOriginX = 1u32 << 17;
        const ClipOriginY = 1u32 << 18;
        const ClipMask = 1u32 << 19;
        const DashOffset = 1u32 << 20;
        const DashList = 1u32 << 21;
        const ArcMode = 1u32 << 22;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Gx(pub u32);

impl Gx {
    pub(crate) const clear: Self = Self(0);
    pub(crate) const and: Self = Self(1);
    pub(crate) const andReverse: Self = Self(2);
    pub(crate) const copy: Self = Self(3);
    pub(crate) const andInverted: Self = Self(4);
    pub(crate) const noop: Self = Self(5);
    pub(crate) const xor: Self = Self(6);
    pub(crate) const or: Self = Self(7);
    pub(crate) const nor: Self = Self(8);
    pub(crate) const equiv: Self = Self(9);
    pub(crate) const invert: Self = Self(10);
    pub(crate) const orReverse: Self = Self(11);
    pub(crate) const copyInverted: Self = Self(12);
    pub(crate) const orInverted: Self = Self(13);
    pub(crate) const nand: Self = Self(14);
    pub(crate) const set: Self = Self(15);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LineStyle(pub u32);

impl LineStyle {
    pub(crate) const Solid: Self = Self(0);
    pub(crate) const OnOffDash: Self = Self(1);
    pub(crate) const DoubleDash: Self = Self(2);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CapStyle(pub u32);

impl CapStyle {
    pub(crate) const NotLast: Self = Self(0);
    pub(crate) const Butt: Self = Self(1);
    pub(crate) const Round: Self = Self(2);
    pub(crate) const Projecting: Self = Self(3);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct JoinStyle(pub u32);

impl JoinStyle {
    pub(crate) const Miter: Self = Self(0);
    pub(crate) const Round: Self = Self(1);
    pub(crate) const Bevel: Self = Self(2);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FillStyle(pub u32);

impl FillStyle {
    pub(crate) const Solid: Self = Self(0);
    pub(crate) const Tiled: Self = Self(1);
    pub(crate) const Stippled: Self = Self(2);
    pub(crate) const OpaqueStippled: Self = Self(3);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FillRule(pub u32);

impl FillRule {
    pub(crate) const EvenOdd: Self = Self(0);
    pub(crate) const Winding: Self = Self(1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SubwindowMode(pub u32);

impl SubwindowMode {
    pub(crate) const ClipByChildren: Self = Self(0);
    pub(crate) const IncludeInferiors: Self = Self(1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ArcMode(pub u32);

impl ArcMode {
    pub(crate) const Chord: Self = Self(0);
    pub(crate) const PieSlice: Self = Self(1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ImageFormat(pub u32);

impl ImageFormat {
    pub(crate) const XYBitmap: Self = Self(0);
    pub(crate) const XYPixmap: Self = Self(1);
    pub(crate) const ZPixmap: Self = Self(2);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Kill(pub u32);

impl Kill {
    pub(crate) const AllTemporary: Self = Self(0);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Redirect(pub u32);

impl Redirect {
    pub(crate) const Automatic: Self = Self(0);
    pub(crate) const Manual: Self = Self(1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SelectionEvent(pub u32);

impl SelectionEvent {
    pub(crate) const SetSelectionOwner: Self = Self(0);
    pub(crate) const SelectionWindowDestroy: Self = Self(1);
    pub(crate) const SelectionClientClose: Self = Self(2);
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub(crate) struct SelectionEventMask: u32 {
        const SetSelectionOwner = 1u32 << 0;
        const SelectionWindowDestroy = 1u32 << 1;
        const SelectionClientClose = 1u32 << 2;
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub(crate) struct ClientIdMask: u32 {
        const ClientXID = 1u32 << 0;
        const LocalClientPID = 1u32 << 1;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ClientIdSpec {
    pub(crate) client: u32,
    pub(crate) mask: ClientIdMask,
}

impl ClientIdSpec {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.client);
        writer.u32(self.mask.bits());
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let client = reader.get_u32()?;
        let mask = ClientIdMask::from_bits_retain(reader.get_u32()?);
        Ok(Self { client, mask })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ClientIdValue {
    pub(crate) spec: ClientIdSpec,
    pub(crate) length: u32,
    pub(crate) value: Vec<u32>,
}

impl ClientIdValue {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        self.spec.serialize(writer);
        writer.u32(self.length);
        debug_assert_eq!(self.value.len(), ((self.length as usize) / 4usize));
        for item in &self.value {
            writer.u32(*item);
        }
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let spec = ClientIdSpec::parse(reader)?;
        let length = reader.get_u32()?;
        let value_count = (length as usize) / 4usize;
        let mut value = Vec::with_capacity(value_count);
        for _ in 0..value_count {
            value.push(reader.get_u32()?);
        }
        Ok(Self { spec, length, value })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PictType(pub u32);

impl PictType {
    pub(crate) const Indexed: Self = Self(0);
    pub(crate) const Direct: Self = Self(1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PolyEdge(pub u32);

impl PolyEdge {
    pub(crate) const Sharp: Self = Self(0);
    pub(crate) const Smooth: Self = Self(1);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PolyMode(pub u32);

impl PolyMode {
    pub(crate) const Precise: Self = Self(0);
    pub(crate) const Imprecise: Self = Self(1);
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub(crate) struct Cp: u32 {
        const Repeat = 1u32 << 0;
        const AlphaMap = 1u32 << 1;
        const AlphaXOrigin = 1u32 << 2;
        const AlphaYOrigin = 1u32 << 3;
        const ClipXOrigin = 1u32 << 4;
        const ClipYOrigin = 1u32 << 5;
        const ClipMask = 1u32 << 6;
        const GraphicsExposure = 1u32 << 7;
        const SubwindowMode = 1u32 << 8;
        const PolyEdge = 1u32 << 9;
        const PolyMode = 1u32 << 10;
        const Dither = 1u32 << 11;
        const ComponentAlpha = 1u32 << 12;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SubPixel(pub u32);

impl SubPixel {
    pub(crate) const Unknown: Self = Self(0);
    pub(crate) const HorizontalRGB: Self = Self(1);
    pub(crate) const HorizontalBGR: Self = Self(2);
    pub(crate) const VerticalRGB: Self = Self(3);
    pub(crate) const VerticalBGR: Self = Self(4);
    pub(crate) const None: Self = Self(5);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Repeat(pub u32);

impl Repeat {
    pub(crate) const None: Self = Self(0);
    pub(crate) const Normal: Self = Self(1);
    pub(crate) const Pad: Self = Self(2);
    pub(crate) const Reflect: Self = Self(3);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Picture(pub u32);

impl Picture {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.0);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(reader.get_u32()?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Pictformat(pub u32);

impl Pictformat {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.0);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self(reader.get_u32()?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Directformat {
    pub(crate) red_shift: u16,
    pub(crate) red_mask: u16,
    pub(crate) green_shift: u16,
    pub(crate) green_mask: u16,
    pub(crate) blue_shift: u16,
    pub(crate) blue_mask: u16,
    pub(crate) alpha_shift: u16,
    pub(crate) alpha_mask: u16,
}

impl Directformat {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u16(self.red_shift);
        writer.u16(self.red_mask);
        writer.u16(self.green_shift);
        writer.u16(self.green_mask);
        writer.u16(self.blue_shift);
        writer.u16(self.blue_mask);
        writer.u16(self.alpha_shift);
        writer.u16(self.alpha_mask);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let red_shift = reader.get_u16()?;
        let red_mask = reader.get_u16()?;
        let green_shift = reader.get_u16()?;
        let green_mask = reader.get_u16()?;
        let blue_shift = reader.get_u16()?;
        let blue_mask = reader.get_u16()?;
        let alpha_shift = reader.get_u16()?;
        let alpha_mask = reader.get_u16()?;
        Ok(Self {
            red_shift,
            red_mask,
            green_shift,
            green_mask,
            blue_shift,
            blue_mask,
            alpha_shift,
            alpha_mask,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Pictforminfo {
    pub(crate) id: Pictformat,
    pub(crate) r#type: PictType,
    pub(crate) depth: u8,
    pub(crate) direct: Directformat,
    pub(crate) colormap: Colormap,
}

impl Pictforminfo {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        self.id.serialize(writer);
        writer.u8(self.r#type.0 as u8);
        writer.u8(self.depth);
        writer.pad(2);
        self.direct.serialize(writer);
        self.colormap.serialize(writer);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let id = Pictformat::parse(reader)?;
        let r#type = PictType(u32::from(reader.get_u8()?));
        let depth = reader.get_u8()?;
        reader.skip(2)?;
        let direct = Directformat::parse(reader)?;
        let colormap = Colormap::parse(reader)?;
        Ok(Self { id, r#type, depth, direct, colormap })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Pictvisual {
    pub(crate) visual: Visualid,
    pub(crate) format: Pictformat,
}

impl Pictvisual {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.visual);
        self.format.serialize(writer);
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let visual = reader.get_u32()?;
        let format = Pictformat::parse(reader)?;
        Ok(Self { visual, format })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Pictdepth {
    pub(crate) depth: u8,
    pub(crate) num_visuals: u16,
    pub(crate) visuals: Vec<Pictvisual>,
}

impl Pictdepth {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u8(self.depth);
        writer.pad(1);
        writer.u16(self.num_visuals);
        writer.pad(4);
        debug_assert_eq!(self.visuals.len(), (self.num_visuals as usize));
        for item in &self.visuals {
            item.serialize(writer);
        }
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let depth = reader.get_u8()?;
        reader.skip(1)?;
        let num_visuals = reader.get_u16()?;
        reader.skip(4)?;
        let visuals_count = num_visuals as usize;
        let mut visuals = Vec::with_capacity(visuals_count);
        for _ in 0..visuals_count {
            visuals.push(Pictvisual::parse(reader)?);
        }
        Ok(Self { depth, num_visuals, visuals })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Pictscreen {
    pub(crate) num_depths: u32,
    pub(crate) fallback: Pictformat,
    pub(crate) depths: Vec<Pictdepth>,
}

impl Pictscreen {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {
        writer.u32(self.num_depths);
        self.fallback.serialize(writer);
        debug_assert_eq!(self.depths.len(), (self.num_depths as usize));
        for item in &self.depths {
            item.serialize(writer);
        }
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let num_depths = reader.get_u32()?;
        let fallback = Pictformat::parse(reader)?;
        let depths_count = num_depths as usize;
        let mut depths = Vec::with_capacity(depths_count);
        for _ in 0..depths_count {
            depths.push(Pictdepth::parse(reader)?);
        }
        Ok(Self { num_depths, fallback, depths })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ExtensionName {
    Composite,
    XFixes,
    Res,
    Render,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct NoReply;

pub(crate) trait Request {
    const NAME: &'static str;
    const EXTENSION: Option<ExtensionName>;
    /// Core opcode, or the extension's minor opcode.
    const OPCODE: u8;
    type Reply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError>;
}

pub(crate) trait XReply: Sized {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct CreateWindowAux {
    pub(crate) background_pixmap: Option<Pixmap>,
    pub(crate) background_pixel: Option<u32>,
    pub(crate) border_pixmap: Option<Pixmap>,
    pub(crate) border_pixel: Option<u32>,
    pub(crate) bit_gravity: Option<Gravity>,
    pub(crate) win_gravity: Option<Gravity>,
    pub(crate) backing_store: Option<BackingStore>,
    pub(crate) backing_planes: Option<u32>,
    pub(crate) backing_pixel: Option<u32>,
    pub(crate) override_redirect: Option<Bool32>,
    pub(crate) save_under: Option<Bool32>,
    pub(crate) event_mask: Option<EventMask>,
    pub(crate) do_not_propogate_mask: Option<EventMask>,
    pub(crate) colormap: Option<Colormap>,
    pub(crate) cursor: Option<Cursor>,
}

impl CreateWindowAux {
    pub(crate) fn background_pixmap(mut self, value: Pixmap) -> Self {
        self.background_pixmap = Some(value);
        self
    }

    pub(crate) fn background_pixel(mut self, value: u32) -> Self {
        self.background_pixel = Some(value);
        self
    }

    pub(crate) fn border_pixmap(mut self, value: Pixmap) -> Self {
        self.border_pixmap = Some(value);
        self
    }

    pub(crate) fn border_pixel(mut self, value: u32) -> Self {
        self.border_pixel = Some(value);
        self
    }

    pub(crate) fn bit_gravity(mut self, value: Gravity) -> Self {
        self.bit_gravity = Some(value);
        self
    }

    pub(crate) fn win_gravity(mut self, value: Gravity) -> Self {
        self.win_gravity = Some(value);
        self
    }

    pub(crate) fn backing_store(mut self, value: BackingStore) -> Self {
        self.backing_store = Some(value);
        self
    }

    pub(crate) fn backing_planes(mut self, value: u32) -> Self {
        self.backing_planes = Some(value);
        self
    }

    pub(crate) fn backing_pixel(mut self, value: u32) -> Self {
        self.backing_pixel = Some(value);
        self
    }

    pub(crate) fn override_redirect(mut self, value: Bool32) -> Self {
        self.override_redirect = Some(value);
        self
    }

    pub(crate) fn save_under(mut self, value: Bool32) -> Self {
        self.save_under = Some(value);
        self
    }

    pub(crate) fn event_mask(mut self, value: EventMask) -> Self {
        self.event_mask = Some(value);
        self
    }

    pub(crate) fn do_not_propogate_mask(mut self, value: EventMask) -> Self {
        self.do_not_propogate_mask = Some(value);
        self
    }

    pub(crate) fn colormap(mut self, value: Colormap) -> Self {
        self.colormap = Some(value);
        self
    }

    pub(crate) fn cursor(mut self, value: Cursor) -> Self {
        self.cursor = Some(value);
        self
    }

    pub(crate) fn mask(&self) -> Cw {
        let mut mask = Cw::empty();
        if self.background_pixmap.is_some() {
            mask |= Cw::BackPixmap;
        }
        if self.background_pixel.is_some() {
            mask |= Cw::BackPixel;
        }
        if self.border_pixmap.is_some() {
            mask |= Cw::BorderPixmap;
        }
        if self.border_pixel.is_some() {
            mask |= Cw::BorderPixel;
        }
        if self.bit_gravity.is_some() {
            mask |= Cw::BitGravity;
        }
        if self.win_gravity.is_some() {
            mask |= Cw::WinGravity;
        }
        if self.backing_store.is_some() {
            mask |= Cw::BackingStore;
        }
        if self.backing_planes.is_some() {
            mask |= Cw::BackingPlanes;
        }
        if self.backing_pixel.is_some() {
            mask |= Cw::BackingPixel;
        }
        if self.override_redirect.is_some() {
            mask |= Cw::OverrideRedirect;
        }
        if self.save_under.is_some() {
            mask |= Cw::SaveUnder;
        }
        if self.event_mask.is_some() {
            mask |= Cw::EventMask;
        }
        if self.do_not_propogate_mask.is_some() {
            mask |= Cw::DontPropagate;
        }
        if self.colormap.is_some() {
            mask |= Cw::Colormap;
        }
        if self.cursor.is_some() {
            mask |= Cw::Cursor;
        }
        mask
    }

    pub(crate) fn write(&self, writer: &mut Writer<'_>) {
        if let Some(value) = &self.background_pixmap {
            value.serialize(writer);
        }
        if let Some(value) = &self.background_pixel {
            writer.u32(*value);
        }
        if let Some(value) = &self.border_pixmap {
            value.serialize(writer);
        }
        if let Some(value) = &self.border_pixel {
            writer.u32(*value);
        }
        if let Some(value) = &self.bit_gravity {
            writer.u32(value.0);
        }
        if let Some(value) = &self.win_gravity {
            writer.u32(value.0);
        }
        if let Some(value) = &self.backing_store {
            writer.u32(value.0);
        }
        if let Some(value) = &self.backing_planes {
            writer.u32(*value);
        }
        if let Some(value) = &self.backing_pixel {
            writer.u32(*value);
        }
        if let Some(value) = &self.override_redirect {
            writer.u32(*value);
        }
        if let Some(value) = &self.save_under {
            writer.u32(*value);
        }
        if let Some(value) = &self.event_mask {
            writer.u32(value.bits());
        }
        if let Some(value) = &self.do_not_propogate_mask {
            writer.u32(value.bits());
        }
        if let Some(value) = &self.colormap {
            value.serialize(writer);
        }
        if let Some(value) = &self.cursor {
            value.serialize(writer);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CreateWindow {
    pub(crate) depth: u8,
    pub(crate) wid: Window,
    pub(crate) parent: Window,
    pub(crate) x: i16,
    pub(crate) y: i16,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) border_width: u16,
    pub(crate) class: WindowClass,
    pub(crate) visual: Visualid,
    pub(crate) value_list: CreateWindowAux,
}

impl Request for CreateWindow {
    const NAME: &'static str = "CreateWindow";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 1;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, self.depth);
        self.wid.serialize(writer);
        self.parent.serialize(writer);
        writer.i16(self.x);
        writer.i16(self.y);
        writer.u16(self.width);
        writer.u16(self.height);
        writer.u16(self.border_width);
        writer.u16(self.class.0 as u16);
        writer.u32(self.visual);
        writer.u32(self.value_list.mask().bits());
        self.value_list.write(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct ChangeWindowAttributesAux {
    pub(crate) background_pixmap: Option<Pixmap>,
    pub(crate) background_pixel: Option<u32>,
    pub(crate) border_pixmap: Option<Pixmap>,
    pub(crate) border_pixel: Option<u32>,
    pub(crate) bit_gravity: Option<Gravity>,
    pub(crate) win_gravity: Option<Gravity>,
    pub(crate) backing_store: Option<BackingStore>,
    pub(crate) backing_planes: Option<u32>,
    pub(crate) backing_pixel: Option<u32>,
    pub(crate) override_redirect: Option<Bool32>,
    pub(crate) save_under: Option<Bool32>,
    pub(crate) event_mask: Option<EventMask>,
    pub(crate) do_not_propogate_mask: Option<EventMask>,
    pub(crate) colormap: Option<Colormap>,
    pub(crate) cursor: Option<Cursor>,
}

impl ChangeWindowAttributesAux {
    pub(crate) fn background_pixmap(mut self, value: Pixmap) -> Self {
        self.background_pixmap = Some(value);
        self
    }

    pub(crate) fn background_pixel(mut self, value: u32) -> Self {
        self.background_pixel = Some(value);
        self
    }

    pub(crate) fn border_pixmap(mut self, value: Pixmap) -> Self {
        self.border_pixmap = Some(value);
        self
    }

    pub(crate) fn border_pixel(mut self, value: u32) -> Self {
        self.border_pixel = Some(value);
        self
    }

    pub(crate) fn bit_gravity(mut self, value: Gravity) -> Self {
        self.bit_gravity = Some(value);
        self
    }

    pub(crate) fn win_gravity(mut self, value: Gravity) -> Self {
        self.win_gravity = Some(value);
        self
    }

    pub(crate) fn backing_store(mut self, value: BackingStore) -> Self {
        self.backing_store = Some(value);
        self
    }

    pub(crate) fn backing_planes(mut self, value: u32) -> Self {
        self.backing_planes = Some(value);
        self
    }

    pub(crate) fn backing_pixel(mut self, value: u32) -> Self {
        self.backing_pixel = Some(value);
        self
    }

    pub(crate) fn override_redirect(mut self, value: Bool32) -> Self {
        self.override_redirect = Some(value);
        self
    }

    pub(crate) fn save_under(mut self, value: Bool32) -> Self {
        self.save_under = Some(value);
        self
    }

    pub(crate) fn event_mask(mut self, value: EventMask) -> Self {
        self.event_mask = Some(value);
        self
    }

    pub(crate) fn do_not_propogate_mask(mut self, value: EventMask) -> Self {
        self.do_not_propogate_mask = Some(value);
        self
    }

    pub(crate) fn colormap(mut self, value: Colormap) -> Self {
        self.colormap = Some(value);
        self
    }

    pub(crate) fn cursor(mut self, value: Cursor) -> Self {
        self.cursor = Some(value);
        self
    }

    pub(crate) fn mask(&self) -> Cw {
        let mut mask = Cw::empty();
        if self.background_pixmap.is_some() {
            mask |= Cw::BackPixmap;
        }
        if self.background_pixel.is_some() {
            mask |= Cw::BackPixel;
        }
        if self.border_pixmap.is_some() {
            mask |= Cw::BorderPixmap;
        }
        if self.border_pixel.is_some() {
            mask |= Cw::BorderPixel;
        }
        if self.bit_gravity.is_some() {
            mask |= Cw::BitGravity;
        }
        if self.win_gravity.is_some() {
            mask |= Cw::WinGravity;
        }
        if self.backing_store.is_some() {
            mask |= Cw::BackingStore;
        }
        if self.backing_planes.is_some() {
            mask |= Cw::BackingPlanes;
        }
        if self.backing_pixel.is_some() {
            mask |= Cw::BackingPixel;
        }
        if self.override_redirect.is_some() {
            mask |= Cw::OverrideRedirect;
        }
        if self.save_under.is_some() {
            mask |= Cw::SaveUnder;
        }
        if self.event_mask.is_some() {
            mask |= Cw::EventMask;
        }
        if self.do_not_propogate_mask.is_some() {
            mask |= Cw::DontPropagate;
        }
        if self.colormap.is_some() {
            mask |= Cw::Colormap;
        }
        if self.cursor.is_some() {
            mask |= Cw::Cursor;
        }
        mask
    }

    pub(crate) fn write(&self, writer: &mut Writer<'_>) {
        if let Some(value) = &self.background_pixmap {
            value.serialize(writer);
        }
        if let Some(value) = &self.background_pixel {
            writer.u32(*value);
        }
        if let Some(value) = &self.border_pixmap {
            value.serialize(writer);
        }
        if let Some(value) = &self.border_pixel {
            writer.u32(*value);
        }
        if let Some(value) = &self.bit_gravity {
            writer.u32(value.0);
        }
        if let Some(value) = &self.win_gravity {
            writer.u32(value.0);
        }
        if let Some(value) = &self.backing_store {
            writer.u32(value.0);
        }
        if let Some(value) = &self.backing_planes {
            writer.u32(*value);
        }
        if let Some(value) = &self.backing_pixel {
            writer.u32(*value);
        }
        if let Some(value) = &self.override_redirect {
            writer.u32(*value);
        }
        if let Some(value) = &self.save_under {
            writer.u32(*value);
        }
        if let Some(value) = &self.event_mask {
            writer.u32(value.bits());
        }
        if let Some(value) = &self.do_not_propogate_mask {
            writer.u32(value.bits());
        }
        if let Some(value) = &self.colormap {
            value.serialize(writer);
        }
        if let Some(value) = &self.cursor {
            value.serialize(writer);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ChangeWindowAttributes {
    pub(crate) window: Window,
    pub(crate) value_list: ChangeWindowAttributesAux,
}

impl Request for ChangeWindowAttributes {
    const NAME: &'static str = "ChangeWindowAttributes";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 2;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.window.serialize(writer);
        writer.u32(self.value_list.mask().bits());
        self.value_list.write(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetWindowAttributes {
    pub(crate) window: Window,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetWindowAttributesReply {
    pub(crate) sequence: u16,
    pub(crate) backing_store: BackingStore,
    pub(crate) visual: Visualid,
    pub(crate) class: WindowClass,
    pub(crate) bit_gravity: Gravity,
    pub(crate) win_gravity: Gravity,
    pub(crate) backing_planes: u32,
    pub(crate) backing_pixel: u32,
    pub(crate) save_under: u8,
    pub(crate) map_is_installed: u8,
    pub(crate) map_state: MapState,
    pub(crate) override_redirect: u8,
    pub(crate) colormap: Colormap,
    pub(crate) all_event_masks: EventMask,
    pub(crate) your_event_mask: EventMask,
    pub(crate) do_not_propagate_mask: EventMask,
}

impl GetWindowAttributesReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_reply(self.backing_store.0 as u8, self.sequence);
        writer.u32(self.visual);
        writer.u16(self.class.0 as u16);
        writer.u8(self.bit_gravity.0 as u8);
        writer.u8(self.win_gravity.0 as u8);
        writer.u32(self.backing_planes);
        writer.u32(self.backing_pixel);
        writer.u8(self.save_under);
        writer.u8(self.map_is_installed);
        writer.u8(self.map_state.0 as u8);
        writer.u8(self.override_redirect);
        self.colormap.serialize(writer);
        writer.u32(self.all_event_masks.bits());
        writer.u32(self.your_event_mask.bits());
        writer.u16(self.do_not_propagate_mask.bits() as u16);
        writer.pad(2);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let backing_store = BackingStore(u32::from(reader.get_u8()?));
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let visual = reader.get_u32()?;
        let class = WindowClass(u32::from(reader.get_u16()?));
        let bit_gravity = Gravity(u32::from(reader.get_u8()?));
        let win_gravity = Gravity(u32::from(reader.get_u8()?));
        let backing_planes = reader.get_u32()?;
        let backing_pixel = reader.get_u32()?;
        let save_under = reader.get_u8()?;
        let map_is_installed = reader.get_u8()?;
        let map_state = MapState(u32::from(reader.get_u8()?));
        let override_redirect = reader.get_u8()?;
        let colormap = Colormap::parse(reader)?;
        let all_event_masks = EventMask::from_bits_retain(reader.get_u32()?);
        let your_event_mask = EventMask::from_bits_retain(reader.get_u32()?);
        let do_not_propagate_mask = EventMask::from_bits_retain(u32::from(reader.get_u16()?));
        reader.skip(2)?;
        reader.finish_reply()?;
        Ok(Self {
            sequence,
            backing_store,
            visual,
            class,
            bit_gravity,
            win_gravity,
            backing_planes,
            backing_pixel,
            save_under,
            map_is_installed,
            map_state,
            override_redirect,
            colormap,
            all_event_masks,
            your_event_mask,
            do_not_propagate_mask,
        })
    }
}

impl XReply for GetWindowAttributesReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        GetWindowAttributesReply::parse(reader)
    }
}

impl Request for GetWindowAttributes {
    const NAME: &'static str = "GetWindowAttributes";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 3;
    type Reply = GetWindowAttributesReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.window.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DestroyWindow {
    pub(crate) window: Window,
}

impl Request for DestroyWindow {
    const NAME: &'static str = "DestroyWindow";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 4;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.window.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MapWindow {
    pub(crate) window: Window,
}

impl Request for MapWindow {
    const NAME: &'static str = "MapWindow";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 8;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.window.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct UnmapWindow {
    pub(crate) window: Window,
}

impl Request for UnmapWindow {
    const NAME: &'static str = "UnmapWindow";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 10;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.window.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct ConfigureWindowAux {
    pub(crate) x: Option<i32>,
    pub(crate) y: Option<i32>,
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
    pub(crate) border_width: Option<u32>,
    pub(crate) sibling: Option<Window>,
    pub(crate) stack_mode: Option<StackMode>,
}

impl ConfigureWindowAux {
    pub(crate) fn x(mut self, value: i32) -> Self {
        self.x = Some(value);
        self
    }

    pub(crate) fn y(mut self, value: i32) -> Self {
        self.y = Some(value);
        self
    }

    pub(crate) fn width(mut self, value: u32) -> Self {
        self.width = Some(value);
        self
    }

    pub(crate) fn height(mut self, value: u32) -> Self {
        self.height = Some(value);
        self
    }

    pub(crate) fn border_width(mut self, value: u32) -> Self {
        self.border_width = Some(value);
        self
    }

    pub(crate) fn sibling(mut self, value: Window) -> Self {
        self.sibling = Some(value);
        self
    }

    pub(crate) fn stack_mode(mut self, value: StackMode) -> Self {
        self.stack_mode = Some(value);
        self
    }

    pub(crate) fn mask(&self) -> ConfigWindow {
        let mut mask = ConfigWindow::empty();
        if self.x.is_some() {
            mask |= ConfigWindow::X;
        }
        if self.y.is_some() {
            mask |= ConfigWindow::Y;
        }
        if self.width.is_some() {
            mask |= ConfigWindow::Width;
        }
        if self.height.is_some() {
            mask |= ConfigWindow::Height;
        }
        if self.border_width.is_some() {
            mask |= ConfigWindow::BorderWidth;
        }
        if self.sibling.is_some() {
            mask |= ConfigWindow::Sibling;
        }
        if self.stack_mode.is_some() {
            mask |= ConfigWindow::StackMode;
        }
        mask
    }

    pub(crate) fn write(&self, writer: &mut Writer<'_>) {
        if let Some(value) = &self.x {
            writer.i32(*value);
        }
        if let Some(value) = &self.y {
            writer.i32(*value);
        }
        if let Some(value) = &self.width {
            writer.u32(*value);
        }
        if let Some(value) = &self.height {
            writer.u32(*value);
        }
        if let Some(value) = &self.border_width {
            writer.u32(*value);
        }
        if let Some(value) = &self.sibling {
            value.serialize(writer);
        }
        if let Some(value) = &self.stack_mode {
            writer.u32(value.0);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConfigureWindow {
    pub(crate) window: Window,
    pub(crate) value_list: ConfigureWindowAux,
}

impl Request for ConfigureWindow {
    const NAME: &'static str = "ConfigureWindow";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 12;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.window.serialize(writer);
        writer.u16(self.value_list.mask().bits() as u16);
        writer.pad(2);
        self.value_list.write(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetGeometry {
    pub(crate) drawable: Drawable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetGeometryReply {
    pub(crate) sequence: u16,
    pub(crate) depth: u8,
    pub(crate) root: Window,
    pub(crate) x: i16,
    pub(crate) y: i16,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) border_width: u16,
}

impl GetGeometryReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_reply(self.depth, self.sequence);
        self.root.serialize(writer);
        writer.i16(self.x);
        writer.i16(self.y);
        writer.u16(self.width);
        writer.u16(self.height);
        writer.u16(self.border_width);
        writer.pad(2);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let depth = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let root = Window::parse(reader)?;
        let x = reader.get_i16()?;
        let y = reader.get_i16()?;
        let width = reader.get_u16()?;
        let height = reader.get_u16()?;
        let border_width = reader.get_u16()?;
        reader.skip(2)?;
        reader.finish_reply()?;
        Ok(Self { sequence, depth, root, x, y, width, height, border_width })
    }
}

impl XReply for GetGeometryReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        GetGeometryReply::parse(reader)
    }
}

impl Request for GetGeometry {
    const NAME: &'static str = "GetGeometry";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 14;
    type Reply = GetGeometryReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.drawable.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct QueryTree {
    pub(crate) window: Window,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct QueryTreeReply {
    pub(crate) sequence: u16,
    pub(crate) root: Window,
    pub(crate) parent: Window,
    pub(crate) children: Vec<Window>,
}

impl QueryTreeReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        let children_len =
            u16::try_from(self.children.len()).map_err(|_| WireError::RequestTooLong)?;
        writer.begin_reply(0, self.sequence);
        self.root.serialize(writer);
        self.parent.serialize(writer);
        writer.u16(children_len);
        writer.pad(14);
        for item in &self.children {
            item.serialize(writer);
        }
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let root = Window::parse(reader)?;
        let parent = Window::parse(reader)?;
        let children_len = reader.get_u16()?;
        reader.skip(14)?;
        let children_count = children_len as usize;
        let mut children = Vec::with_capacity(children_count);
        for _ in 0..children_count {
            children.push(Window::parse(reader)?);
        }
        reader.finish_reply()?;
        Ok(Self { sequence, root, parent, children })
    }
}

impl XReply for QueryTreeReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        QueryTreeReply::parse(reader)
    }
}

impl Request for QueryTree {
    const NAME: &'static str = "QueryTree";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 15;
    type Reply = QueryTreeReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.window.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InternAtom<'a> {
    pub(crate) only_if_exists: u8,
    pub(crate) name: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InternAtomReply {
    pub(crate) sequence: u16,
    pub(crate) atom: Atom,
}

impl InternAtomReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_reply(0, self.sequence);
        self.atom.serialize(writer);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let atom = Atom::parse(reader)?;
        reader.finish_reply()?;
        Ok(Self { sequence, atom })
    }
}

impl XReply for InternAtomReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        InternAtomReply::parse(reader)
    }
}

impl<'a> Request for InternAtom<'a> {
    const NAME: &'static str = "InternAtom";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 16;
    type Reply = InternAtomReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        let name_len = u16::try_from(self.name.len()).map_err(|_| WireError::RequestTooLong)?;
        writer.begin_request(Self::OPCODE, self.only_if_exists);
        writer.u16(name_len);
        writer.pad(2);
        writer.bytes(self.name);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetAtomName {
    pub(crate) atom: Atom,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetAtomNameReply {
    pub(crate) sequence: u16,
    pub(crate) name: Vec<u8>,
}

impl GetAtomNameReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        let name_len = u16::try_from(self.name.len()).map_err(|_| WireError::RequestTooLong)?;
        writer.begin_reply(0, self.sequence);
        writer.u16(name_len);
        writer.pad(22);
        writer.bytes(&self.name);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let name_len = reader.get_u16()?;
        reader.skip(22)?;
        let name_count = name_len as usize;
        let name = reader.bytes(name_count)?.to_vec();
        reader.finish_reply()?;
        Ok(Self { sequence, name })
    }
}

impl XReply for GetAtomNameReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        GetAtomNameReply::parse(reader)
    }
}

impl Request for GetAtomName {
    const NAME: &'static str = "GetAtomName";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 17;
    type Reply = GetAtomNameReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.atom.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ChangeProperty<'a> {
    pub(crate) mode: PropMode,
    pub(crate) window: Window,
    pub(crate) property: Atom,
    pub(crate) r#type: Atom,
    pub(crate) format: u8,
    pub(crate) data_len: u32,
    pub(crate) data: &'a [u8],
}

impl<'a> Request for ChangeProperty<'a> {
    const NAME: &'static str = "ChangeProperty";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 18;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        debug_assert_eq!(
            self.data.len(),
            (((self.data_len as usize) * (self.format as usize)) / 8usize)
        );
        writer.begin_request(Self::OPCODE, self.mode.0 as u8);
        self.window.serialize(writer);
        self.property.serialize(writer);
        self.r#type.serialize(writer);
        writer.u8(self.format);
        writer.pad(3);
        writer.u32(self.data_len);
        writer.bytes(self.data);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DeleteProperty {
    pub(crate) window: Window,
    pub(crate) property: Atom,
}

impl Request for DeleteProperty {
    const NAME: &'static str = "DeleteProperty";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 19;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.window.serialize(writer);
        self.property.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetProperty {
    pub(crate) delete: u8,
    pub(crate) window: Window,
    pub(crate) property: Atom,
    pub(crate) r#type: Atom,
    pub(crate) long_offset: u32,
    pub(crate) long_length: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetPropertyReply {
    pub(crate) sequence: u16,
    pub(crate) format: u8,
    pub(crate) r#type: Atom,
    pub(crate) bytes_after: u32,
    pub(crate) value_len: u32,
    pub(crate) value: Vec<u8>,
}

impl GetPropertyReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        debug_assert_eq!(
            self.value.len(),
            ((self.value_len as usize) * ((self.format as usize) / 8usize))
        );
        writer.begin_reply(self.format, self.sequence);
        self.r#type.serialize(writer);
        writer.u32(self.bytes_after);
        writer.u32(self.value_len);
        writer.pad(12);
        writer.bytes(&self.value);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let format = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let r#type = Atom::parse(reader)?;
        let bytes_after = reader.get_u32()?;
        let value_len = reader.get_u32()?;
        reader.skip(12)?;
        let value_count = (value_len as usize) * ((format as usize) / 8usize);
        let value = reader.bytes(value_count)?.to_vec();
        reader.finish_reply()?;
        Ok(Self { sequence, format, r#type, bytes_after, value_len, value })
    }
}

impl XReply for GetPropertyReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        GetPropertyReply::parse(reader)
    }
}

impl Request for GetProperty {
    const NAME: &'static str = "GetProperty";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 20;
    type Reply = GetPropertyReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, self.delete);
        self.window.serialize(writer);
        self.property.serialize(writer);
        self.r#type.serialize(writer);
        writer.u32(self.long_offset);
        writer.u32(self.long_length);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SetSelectionOwner {
    pub(crate) owner: Window,
    pub(crate) selection: Atom,
    pub(crate) time: Timestamp,
}

impl Request for SetSelectionOwner {
    const NAME: &'static str = "SetSelectionOwner";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 22;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.owner.serialize(writer);
        self.selection.serialize(writer);
        writer.u32(self.time);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetSelectionOwner {
    pub(crate) selection: Atom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetSelectionOwnerReply {
    pub(crate) sequence: u16,
    pub(crate) owner: Window,
}

impl GetSelectionOwnerReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_reply(0, self.sequence);
        self.owner.serialize(writer);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let owner = Window::parse(reader)?;
        reader.finish_reply()?;
        Ok(Self { sequence, owner })
    }
}

impl XReply for GetSelectionOwnerReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        GetSelectionOwnerReply::parse(reader)
    }
}

impl Request for GetSelectionOwner {
    const NAME: &'static str = "GetSelectionOwner";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 23;
    type Reply = GetSelectionOwnerReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.selection.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConvertSelection {
    pub(crate) requestor: Window,
    pub(crate) selection: Atom,
    pub(crate) target: Atom,
    pub(crate) property: Atom,
    pub(crate) time: Timestamp,
}

impl Request for ConvertSelection {
    const NAME: &'static str = "ConvertSelection";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 24;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.requestor.serialize(writer);
        self.selection.serialize(writer);
        self.target.serialize(writer);
        self.property.serialize(writer);
        writer.u32(self.time);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SendEvent<'a> {
    pub(crate) propagate: u8,
    pub(crate) destination: Window,
    pub(crate) event_mask: EventMask,
    pub(crate) event: &'a [u8; 32],
}

impl<'a> Request for SendEvent<'a> {
    const NAME: &'static str = "SendEvent";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 25;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, self.propagate);
        self.destination.serialize(writer);
        writer.u32(self.event_mask.bits());
        writer.bytes(self.event);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SetInputFocus {
    pub(crate) revert_to: InputFocus,
    pub(crate) focus: Window,
    pub(crate) time: Timestamp,
}

impl Request for SetInputFocus {
    const NAME: &'static str = "SetInputFocus";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 42;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, self.revert_to.0 as u8);
        self.focus.serialize(writer);
        writer.u32(self.time);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetInputFocus;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct GetInputFocusReply {
    pub(crate) sequence: u16,
    pub(crate) revert_to: InputFocus,
    pub(crate) focus: Window,
}

impl GetInputFocusReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_reply(self.revert_to.0 as u8, self.sequence);
        self.focus.serialize(writer);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let revert_to = InputFocus(u32::from(reader.get_u8()?));
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let focus = Window::parse(reader)?;
        reader.finish_reply()?;
        Ok(Self { sequence, revert_to, focus })
    }
}

impl XReply for GetInputFocusReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        GetInputFocusReply::parse(reader)
    }
}

impl Request for GetInputFocus {
    const NAME: &'static str = "GetInputFocus";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 43;
    type Reply = GetInputFocusReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        let _ = self;
        writer.begin_request(Self::OPCODE, 0);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct QueryExtension<'a> {
    pub(crate) name: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct QueryExtensionReply {
    pub(crate) sequence: u16,
    pub(crate) present: u8,
    pub(crate) major_opcode: u8,
    pub(crate) first_event: u8,
    pub(crate) first_error: u8,
}

impl QueryExtensionReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_reply(0, self.sequence);
        writer.u8(self.present);
        writer.u8(self.major_opcode);
        writer.u8(self.first_event);
        writer.u8(self.first_error);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let present = reader.get_u8()?;
        let major_opcode = reader.get_u8()?;
        let first_event = reader.get_u8()?;
        let first_error = reader.get_u8()?;
        reader.finish_reply()?;
        Ok(Self { sequence, present, major_opcode, first_event, first_error })
    }
}

impl XReply for QueryExtensionReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        QueryExtensionReply::parse(reader)
    }
}

impl<'a> Request for QueryExtension<'a> {
    const NAME: &'static str = "QueryExtension";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 98;
    type Reply = QueryExtensionReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        let name_len = u16::try_from(self.name.len()).map_err(|_| WireError::RequestTooLong)?;
        writer.begin_request(Self::OPCODE, 0);
        writer.u16(name_len);
        writer.pad(2);
        writer.bytes(self.name);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct KillClient {
    pub(crate) resource: u32,
}

impl Request for KillClient {
    const NAME: &'static str = "KillClient";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 113;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        writer.u32(self.resource);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CreatePixmap {
    pub(crate) depth: u8,
    pub(crate) pid: Pixmap,
    pub(crate) drawable: Drawable,
    pub(crate) width: u16,
    pub(crate) height: u16,
}

impl Request for CreatePixmap {
    const NAME: &'static str = "CreatePixmap";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 53;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, self.depth);
        self.pid.serialize(writer);
        self.drawable.serialize(writer);
        writer.u16(self.width);
        writer.u16(self.height);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FreePixmap {
    pub(crate) pixmap: Pixmap,
}

impl Request for FreePixmap {
    const NAME: &'static str = "FreePixmap";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 54;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.pixmap.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct CreateGcAux {
    pub(crate) function: Option<Gx>,
    pub(crate) plane_mask: Option<u32>,
    pub(crate) foreground: Option<u32>,
    pub(crate) background: Option<u32>,
    pub(crate) line_width: Option<u32>,
    pub(crate) line_style: Option<LineStyle>,
    pub(crate) cap_style: Option<CapStyle>,
    pub(crate) join_style: Option<JoinStyle>,
    pub(crate) fill_style: Option<FillStyle>,
    pub(crate) fill_rule: Option<FillRule>,
    pub(crate) tile: Option<Pixmap>,
    pub(crate) stipple: Option<Pixmap>,
    pub(crate) tile_stipple_x_origin: Option<i32>,
    pub(crate) tile_stipple_y_origin: Option<i32>,
    pub(crate) font: Option<Font>,
    pub(crate) subwindow_mode: Option<SubwindowMode>,
    pub(crate) graphics_exposures: Option<Bool32>,
    pub(crate) clip_x_origin: Option<i32>,
    pub(crate) clip_y_origin: Option<i32>,
    pub(crate) clip_mask: Option<Pixmap>,
    pub(crate) dash_offset: Option<u32>,
    pub(crate) dashes: Option<u32>,
    pub(crate) arc_mode: Option<ArcMode>,
}

impl CreateGcAux {
    pub(crate) fn function(mut self, value: Gx) -> Self {
        self.function = Some(value);
        self
    }

    pub(crate) fn plane_mask(mut self, value: u32) -> Self {
        self.plane_mask = Some(value);
        self
    }

    pub(crate) fn foreground(mut self, value: u32) -> Self {
        self.foreground = Some(value);
        self
    }

    pub(crate) fn background(mut self, value: u32) -> Self {
        self.background = Some(value);
        self
    }

    pub(crate) fn line_width(mut self, value: u32) -> Self {
        self.line_width = Some(value);
        self
    }

    pub(crate) fn line_style(mut self, value: LineStyle) -> Self {
        self.line_style = Some(value);
        self
    }

    pub(crate) fn cap_style(mut self, value: CapStyle) -> Self {
        self.cap_style = Some(value);
        self
    }

    pub(crate) fn join_style(mut self, value: JoinStyle) -> Self {
        self.join_style = Some(value);
        self
    }

    pub(crate) fn fill_style(mut self, value: FillStyle) -> Self {
        self.fill_style = Some(value);
        self
    }

    pub(crate) fn fill_rule(mut self, value: FillRule) -> Self {
        self.fill_rule = Some(value);
        self
    }

    pub(crate) fn tile(mut self, value: Pixmap) -> Self {
        self.tile = Some(value);
        self
    }

    pub(crate) fn stipple(mut self, value: Pixmap) -> Self {
        self.stipple = Some(value);
        self
    }

    pub(crate) fn tile_stipple_x_origin(mut self, value: i32) -> Self {
        self.tile_stipple_x_origin = Some(value);
        self
    }

    pub(crate) fn tile_stipple_y_origin(mut self, value: i32) -> Self {
        self.tile_stipple_y_origin = Some(value);
        self
    }

    pub(crate) fn font(mut self, value: Font) -> Self {
        self.font = Some(value);
        self
    }

    pub(crate) fn subwindow_mode(mut self, value: SubwindowMode) -> Self {
        self.subwindow_mode = Some(value);
        self
    }

    pub(crate) fn graphics_exposures(mut self, value: Bool32) -> Self {
        self.graphics_exposures = Some(value);
        self
    }

    pub(crate) fn clip_x_origin(mut self, value: i32) -> Self {
        self.clip_x_origin = Some(value);
        self
    }

    pub(crate) fn clip_y_origin(mut self, value: i32) -> Self {
        self.clip_y_origin = Some(value);
        self
    }

    pub(crate) fn clip_mask(mut self, value: Pixmap) -> Self {
        self.clip_mask = Some(value);
        self
    }

    pub(crate) fn dash_offset(mut self, value: u32) -> Self {
        self.dash_offset = Some(value);
        self
    }

    pub(crate) fn dashes(mut self, value: u32) -> Self {
        self.dashes = Some(value);
        self
    }

    pub(crate) fn arc_mode(mut self, value: ArcMode) -> Self {
        self.arc_mode = Some(value);
        self
    }

    pub(crate) fn mask(&self) -> Gc {
        let mut mask = Gc::empty();
        if self.function.is_some() {
            mask |= Gc::Function;
        }
        if self.plane_mask.is_some() {
            mask |= Gc::PlaneMask;
        }
        if self.foreground.is_some() {
            mask |= Gc::Foreground;
        }
        if self.background.is_some() {
            mask |= Gc::Background;
        }
        if self.line_width.is_some() {
            mask |= Gc::LineWidth;
        }
        if self.line_style.is_some() {
            mask |= Gc::LineStyle;
        }
        if self.cap_style.is_some() {
            mask |= Gc::CapStyle;
        }
        if self.join_style.is_some() {
            mask |= Gc::JoinStyle;
        }
        if self.fill_style.is_some() {
            mask |= Gc::FillStyle;
        }
        if self.fill_rule.is_some() {
            mask |= Gc::FillRule;
        }
        if self.tile.is_some() {
            mask |= Gc::Tile;
        }
        if self.stipple.is_some() {
            mask |= Gc::Stipple;
        }
        if self.tile_stipple_x_origin.is_some() {
            mask |= Gc::TileStippleOriginX;
        }
        if self.tile_stipple_y_origin.is_some() {
            mask |= Gc::TileStippleOriginY;
        }
        if self.font.is_some() {
            mask |= Gc::Font;
        }
        if self.subwindow_mode.is_some() {
            mask |= Gc::SubwindowMode;
        }
        if self.graphics_exposures.is_some() {
            mask |= Gc::GraphicsExposures;
        }
        if self.clip_x_origin.is_some() {
            mask |= Gc::ClipOriginX;
        }
        if self.clip_y_origin.is_some() {
            mask |= Gc::ClipOriginY;
        }
        if self.clip_mask.is_some() {
            mask |= Gc::ClipMask;
        }
        if self.dash_offset.is_some() {
            mask |= Gc::DashOffset;
        }
        if self.dashes.is_some() {
            mask |= Gc::DashList;
        }
        if self.arc_mode.is_some() {
            mask |= Gc::ArcMode;
        }
        mask
    }

    pub(crate) fn write(&self, writer: &mut Writer<'_>) {
        if let Some(value) = &self.function {
            writer.u32(value.0);
        }
        if let Some(value) = &self.plane_mask {
            writer.u32(*value);
        }
        if let Some(value) = &self.foreground {
            writer.u32(*value);
        }
        if let Some(value) = &self.background {
            writer.u32(*value);
        }
        if let Some(value) = &self.line_width {
            writer.u32(*value);
        }
        if let Some(value) = &self.line_style {
            writer.u32(value.0);
        }
        if let Some(value) = &self.cap_style {
            writer.u32(value.0);
        }
        if let Some(value) = &self.join_style {
            writer.u32(value.0);
        }
        if let Some(value) = &self.fill_style {
            writer.u32(value.0);
        }
        if let Some(value) = &self.fill_rule {
            writer.u32(value.0);
        }
        if let Some(value) = &self.tile {
            value.serialize(writer);
        }
        if let Some(value) = &self.stipple {
            value.serialize(writer);
        }
        if let Some(value) = &self.tile_stipple_x_origin {
            writer.i32(*value);
        }
        if let Some(value) = &self.tile_stipple_y_origin {
            writer.i32(*value);
        }
        if let Some(value) = &self.font {
            value.serialize(writer);
        }
        if let Some(value) = &self.subwindow_mode {
            writer.u32(value.0);
        }
        if let Some(value) = &self.graphics_exposures {
            writer.u32(*value);
        }
        if let Some(value) = &self.clip_x_origin {
            writer.i32(*value);
        }
        if let Some(value) = &self.clip_y_origin {
            writer.i32(*value);
        }
        if let Some(value) = &self.clip_mask {
            value.serialize(writer);
        }
        if let Some(value) = &self.dash_offset {
            writer.u32(*value);
        }
        if let Some(value) = &self.dashes {
            writer.u32(*value);
        }
        if let Some(value) = &self.arc_mode {
            writer.u32(value.0);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CreateGc {
    pub(crate) cid: Gcontext,
    pub(crate) drawable: Drawable,
    pub(crate) value_list: CreateGcAux,
}

impl Request for CreateGc {
    const NAME: &'static str = "CreateGc";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 55;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.cid.serialize(writer);
        self.drawable.serialize(writer);
        writer.u32(self.value_list.mask().bits());
        self.value_list.write(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FreeGc {
    pub(crate) gc: Gcontext,
}

impl Request for FreeGc {
    const NAME: &'static str = "FreeGc";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 60;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, 0);
        self.gc.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PutImage<'a> {
    pub(crate) format: ImageFormat,
    pub(crate) drawable: Drawable,
    pub(crate) gc: Gcontext,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) dst_x: i16,
    pub(crate) dst_y: i16,
    pub(crate) left_pad: u8,
    pub(crate) depth: u8,
    pub(crate) data: &'a [u8],
}

impl<'a> Request for PutImage<'a> {
    const NAME: &'static str = "PutImage";
    const EXTENSION: Option<ExtensionName> = None;
    const OPCODE: u8 = 72;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, _major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(Self::OPCODE, self.format.0 as u8);
        self.drawable.serialize(writer);
        self.gc.serialize(writer);
        writer.u16(self.width);
        writer.u16(self.height);
        writer.i16(self.dst_x);
        writer.i16(self.dst_y);
        writer.u8(self.left_pad);
        writer.u8(self.depth);
        writer.pad(2);
        writer.bytes(self.data);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CompositeQueryVersion {
    pub(crate) client_major_version: u32,
    pub(crate) client_minor_version: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CompositeQueryVersionReply {
    pub(crate) sequence: u16,
    pub(crate) major_version: u32,
    pub(crate) minor_version: u32,
}

impl CompositeQueryVersionReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_reply(0, self.sequence);
        writer.u32(self.major_version);
        writer.u32(self.minor_version);
        writer.pad(16);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let major_version = reader.get_u32()?;
        let minor_version = reader.get_u32()?;
        reader.skip(16)?;
        reader.finish_reply()?;
        Ok(Self { sequence, major_version, minor_version })
    }
}

impl XReply for CompositeQueryVersionReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        CompositeQueryVersionReply::parse(reader)
    }
}

impl Request for CompositeQueryVersion {
    const NAME: &'static str = "CompositeQueryVersion";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::Composite);
    const OPCODE: u8 = 0;
    type Reply = CompositeQueryVersionReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(major_opcode, Self::OPCODE);
        writer.u32(self.client_major_version);
        writer.u32(self.client_minor_version);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CompositeRedirectSubwindows {
    pub(crate) window: Window,
    pub(crate) update: Redirect,
}

impl Request for CompositeRedirectSubwindows {
    const NAME: &'static str = "CompositeRedirectSubwindows";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::Composite);
    const OPCODE: u8 = 2;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(major_opcode, Self::OPCODE);
        self.window.serialize(writer);
        writer.u8(self.update.0 as u8);
        writer.pad(3);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct XFixesQueryVersion {
    pub(crate) client_major_version: u32,
    pub(crate) client_minor_version: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct XFixesQueryVersionReply {
    pub(crate) sequence: u16,
    pub(crate) major_version: u32,
    pub(crate) minor_version: u32,
}

impl XFixesQueryVersionReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_reply(0, self.sequence);
        writer.u32(self.major_version);
        writer.u32(self.minor_version);
        writer.pad(16);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let major_version = reader.get_u32()?;
        let minor_version = reader.get_u32()?;
        reader.skip(16)?;
        reader.finish_reply()?;
        Ok(Self { sequence, major_version, minor_version })
    }
}

impl XReply for XFixesQueryVersionReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        XFixesQueryVersionReply::parse(reader)
    }
}

impl Request for XFixesQueryVersion {
    const NAME: &'static str = "XFixesQueryVersion";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::XFixes);
    const OPCODE: u8 = 0;
    type Reply = XFixesQueryVersionReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(major_opcode, Self::OPCODE);
        writer.u32(self.client_major_version);
        writer.u32(self.client_minor_version);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct XFixesSelectSelectionInput {
    pub(crate) window: Window,
    pub(crate) selection: Atom,
    pub(crate) event_mask: SelectionEventMask,
}

impl Request for XFixesSelectSelectionInput {
    const NAME: &'static str = "XFixesSelectSelectionInput";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::XFixes);
    const OPCODE: u8 = 2;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(major_opcode, Self::OPCODE);
        self.window.serialize(writer);
        self.selection.serialize(writer);
        writer.u32(self.event_mask.bits());
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ResQueryVersion {
    pub(crate) client_major: u8,
    pub(crate) client_minor: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ResQueryVersionReply {
    pub(crate) sequence: u16,
    pub(crate) server_major: u16,
    pub(crate) server_minor: u16,
}

impl ResQueryVersionReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_reply(0, self.sequence);
        writer.u16(self.server_major);
        writer.u16(self.server_minor);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let server_major = reader.get_u16()?;
        let server_minor = reader.get_u16()?;
        reader.finish_reply()?;
        Ok(Self { sequence, server_major, server_minor })
    }
}

impl XReply for ResQueryVersionReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        ResQueryVersionReply::parse(reader)
    }
}

impl Request for ResQueryVersion {
    const NAME: &'static str = "ResQueryVersion";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::Res);
    const OPCODE: u8 = 0;
    type Reply = ResQueryVersionReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(major_opcode, Self::OPCODE);
        writer.u8(self.client_major);
        writer.u8(self.client_minor);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ResQueryClientIds<'a> {
    pub(crate) specs: &'a [ClientIdSpec],
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ResQueryClientIdsReply {
    pub(crate) sequence: u16,
    pub(crate) ids: Vec<ClientIdValue>,
}

impl ResQueryClientIdsReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        let num_ids = u32::try_from(self.ids.len()).map_err(|_| WireError::RequestTooLong)?;
        writer.begin_reply(0, self.sequence);
        writer.u32(num_ids);
        writer.pad(20);
        for item in &self.ids {
            item.serialize(writer);
        }
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let num_ids = reader.get_u32()?;
        reader.skip(20)?;
        let ids_count = num_ids as usize;
        let mut ids = Vec::with_capacity(ids_count);
        for _ in 0..ids_count {
            ids.push(ClientIdValue::parse(reader)?);
        }
        reader.finish_reply()?;
        Ok(Self { sequence, ids })
    }
}

impl XReply for ResQueryClientIdsReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        ResQueryClientIdsReply::parse(reader)
    }
}

impl<'a> Request for ResQueryClientIds<'a> {
    const NAME: &'static str = "ResQueryClientIds";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::Res);
    const OPCODE: u8 = 4;
    type Reply = ResQueryClientIdsReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        let num_specs = u32::try_from(self.specs.len()).map_err(|_| WireError::RequestTooLong)?;
        writer.begin_request(major_opcode, Self::OPCODE);
        writer.u32(num_specs);
        for item in self.specs {
            item.serialize(writer);
        }
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RenderQueryVersion {
    pub(crate) client_major_version: u32,
    pub(crate) client_minor_version: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RenderQueryVersionReply {
    pub(crate) sequence: u16,
    pub(crate) major_version: u32,
    pub(crate) minor_version: u32,
}

impl RenderQueryVersionReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_reply(0, self.sequence);
        writer.u32(self.major_version);
        writer.u32(self.minor_version);
        writer.pad(16);
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let major_version = reader.get_u32()?;
        let minor_version = reader.get_u32()?;
        reader.skip(16)?;
        reader.finish_reply()?;
        Ok(Self { sequence, major_version, minor_version })
    }
}

impl XReply for RenderQueryVersionReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        RenderQueryVersionReply::parse(reader)
    }
}

impl Request for RenderQueryVersion {
    const NAME: &'static str = "RenderQueryVersion";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::Render);
    const OPCODE: u8 = 0;
    type Reply = RenderQueryVersionReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(major_opcode, Self::OPCODE);
        writer.u32(self.client_major_version);
        writer.u32(self.client_minor_version);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RenderQueryPictFormats;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RenderQueryPictFormatsReply {
    pub(crate) sequence: u16,
    pub(crate) num_depths: u32,
    pub(crate) num_visuals: u32,
    pub(crate) formats: Vec<Pictforminfo>,
    pub(crate) screens: Vec<Pictscreen>,
    pub(crate) subpixels: Vec<SubPixel>,
}

impl RenderQueryPictFormatsReply {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        let num_formats =
            u32::try_from(self.formats.len()).map_err(|_| WireError::RequestTooLong)?;
        let num_screens =
            u32::try_from(self.screens.len()).map_err(|_| WireError::RequestTooLong)?;
        let num_subpixel =
            u32::try_from(self.subpixels.len()).map_err(|_| WireError::RequestTooLong)?;
        writer.begin_reply(0, self.sequence);
        writer.u32(num_formats);
        writer.u32(num_screens);
        writer.u32(self.num_depths);
        writer.u32(self.num_visuals);
        writer.u32(num_subpixel);
        writer.pad(4);
        for item in &self.formats {
            item.serialize(writer);
        }
        for item in &self.screens {
            item.serialize(writer);
        }
        for item in &self.subpixels {
            writer.u32(item.0);
        }
        writer.finish_reply()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let _data = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let _length = reader.get_u32()?;
        let num_formats = reader.get_u32()?;
        let num_screens = reader.get_u32()?;
        let num_depths = reader.get_u32()?;
        let num_visuals = reader.get_u32()?;
        let num_subpixel = reader.get_u32()?;
        reader.skip(4)?;
        let formats_count = num_formats as usize;
        let mut formats = Vec::with_capacity(formats_count);
        for _ in 0..formats_count {
            formats.push(Pictforminfo::parse(reader)?);
        }
        let screens_count = num_screens as usize;
        let mut screens = Vec::with_capacity(screens_count);
        for _ in 0..screens_count {
            screens.push(Pictscreen::parse(reader)?);
        }
        let subpixels_count = num_subpixel as usize;
        let mut subpixels = Vec::with_capacity(subpixels_count);
        for _ in 0..subpixels_count {
            subpixels.push(SubPixel(reader.get_u32()?));
        }
        reader.finish_reply()?;
        Ok(Self {
            sequence,
            num_depths,
            num_visuals,
            formats,
            screens,
            subpixels,
        })
    }
}

impl XReply for RenderQueryPictFormatsReply {
    fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        RenderQueryPictFormatsReply::parse(reader)
    }
}

impl Request for RenderQueryPictFormats {
    const NAME: &'static str = "RenderQueryPictFormats";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::Render);
    const OPCODE: u8 = 1;
    type Reply = RenderQueryPictFormatsReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        let _ = self;
        writer.begin_request(major_opcode, Self::OPCODE);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct RenderCreatePictureAux {
    pub(crate) repeat: Option<Repeat>,
    pub(crate) alphamap: Option<Picture>,
    pub(crate) alphaxorigin: Option<i32>,
    pub(crate) alphayorigin: Option<i32>,
    pub(crate) clipxorigin: Option<i32>,
    pub(crate) clipyorigin: Option<i32>,
    pub(crate) clipmask: Option<Pixmap>,
    pub(crate) graphicsexposure: Option<u32>,
    pub(crate) subwindowmode: Option<SubwindowMode>,
    pub(crate) polyedge: Option<PolyEdge>,
    pub(crate) polymode: Option<PolyMode>,
    pub(crate) dither: Option<Atom>,
    pub(crate) componentalpha: Option<u32>,
}

impl RenderCreatePictureAux {
    pub(crate) fn repeat(mut self, value: Repeat) -> Self {
        self.repeat = Some(value);
        self
    }

    pub(crate) fn alphamap(mut self, value: Picture) -> Self {
        self.alphamap = Some(value);
        self
    }

    pub(crate) fn alphaxorigin(mut self, value: i32) -> Self {
        self.alphaxorigin = Some(value);
        self
    }

    pub(crate) fn alphayorigin(mut self, value: i32) -> Self {
        self.alphayorigin = Some(value);
        self
    }

    pub(crate) fn clipxorigin(mut self, value: i32) -> Self {
        self.clipxorigin = Some(value);
        self
    }

    pub(crate) fn clipyorigin(mut self, value: i32) -> Self {
        self.clipyorigin = Some(value);
        self
    }

    pub(crate) fn clipmask(mut self, value: Pixmap) -> Self {
        self.clipmask = Some(value);
        self
    }

    pub(crate) fn graphicsexposure(mut self, value: u32) -> Self {
        self.graphicsexposure = Some(value);
        self
    }

    pub(crate) fn subwindowmode(mut self, value: SubwindowMode) -> Self {
        self.subwindowmode = Some(value);
        self
    }

    pub(crate) fn polyedge(mut self, value: PolyEdge) -> Self {
        self.polyedge = Some(value);
        self
    }

    pub(crate) fn polymode(mut self, value: PolyMode) -> Self {
        self.polymode = Some(value);
        self
    }

    pub(crate) fn dither(mut self, value: Atom) -> Self {
        self.dither = Some(value);
        self
    }

    pub(crate) fn componentalpha(mut self, value: u32) -> Self {
        self.componentalpha = Some(value);
        self
    }

    pub(crate) fn mask(&self) -> Cp {
        let mut mask = Cp::empty();
        if self.repeat.is_some() {
            mask |= Cp::Repeat;
        }
        if self.alphamap.is_some() {
            mask |= Cp::AlphaMap;
        }
        if self.alphaxorigin.is_some() {
            mask |= Cp::AlphaXOrigin;
        }
        if self.alphayorigin.is_some() {
            mask |= Cp::AlphaYOrigin;
        }
        if self.clipxorigin.is_some() {
            mask |= Cp::ClipXOrigin;
        }
        if self.clipyorigin.is_some() {
            mask |= Cp::ClipYOrigin;
        }
        if self.clipmask.is_some() {
            mask |= Cp::ClipMask;
        }
        if self.graphicsexposure.is_some() {
            mask |= Cp::GraphicsExposure;
        }
        if self.subwindowmode.is_some() {
            mask |= Cp::SubwindowMode;
        }
        if self.polyedge.is_some() {
            mask |= Cp::PolyEdge;
        }
        if self.polymode.is_some() {
            mask |= Cp::PolyMode;
        }
        if self.dither.is_some() {
            mask |= Cp::Dither;
        }
        if self.componentalpha.is_some() {
            mask |= Cp::ComponentAlpha;
        }
        mask
    }

    pub(crate) fn write(&self, writer: &mut Writer<'_>) {
        if let Some(value) = &self.repeat {
            writer.u32(value.0);
        }
        if let Some(value) = &self.alphamap {
            value.serialize(writer);
        }
        if let Some(value) = &self.alphaxorigin {
            writer.i32(*value);
        }
        if let Some(value) = &self.alphayorigin {
            writer.i32(*value);
        }
        if let Some(value) = &self.clipxorigin {
            writer.i32(*value);
        }
        if let Some(value) = &self.clipyorigin {
            writer.i32(*value);
        }
        if let Some(value) = &self.clipmask {
            value.serialize(writer);
        }
        if let Some(value) = &self.graphicsexposure {
            writer.u32(*value);
        }
        if let Some(value) = &self.subwindowmode {
            writer.u32(value.0);
        }
        if let Some(value) = &self.polyedge {
            writer.u32(value.0);
        }
        if let Some(value) = &self.polymode {
            writer.u32(value.0);
        }
        if let Some(value) = &self.dither {
            value.serialize(writer);
        }
        if let Some(value) = &self.componentalpha {
            writer.u32(*value);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RenderCreatePicture {
    pub(crate) pid: Picture,
    pub(crate) drawable: Drawable,
    pub(crate) format: Pictformat,
    pub(crate) value_list: RenderCreatePictureAux,
}

impl Request for RenderCreatePicture {
    const NAME: &'static str = "RenderCreatePicture";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::Render);
    const OPCODE: u8 = 4;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(major_opcode, Self::OPCODE);
        self.pid.serialize(writer);
        self.drawable.serialize(writer);
        self.format.serialize(writer);
        writer.u32(self.value_list.mask().bits());
        self.value_list.write(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RenderFreePicture {
    pub(crate) picture: Picture,
}

impl Request for RenderFreePicture {
    const NAME: &'static str = "RenderFreePicture";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::Render);
    const OPCODE: u8 = 7;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(major_opcode, Self::OPCODE);
        self.picture.serialize(writer);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RenderCreateCursor {
    pub(crate) cid: Cursor,
    pub(crate) source: Picture,
    pub(crate) x: u16,
    pub(crate) y: u16,
}

impl Request for RenderCreateCursor {
    const NAME: &'static str = "RenderCreateCursor";
    const EXTENSION: Option<ExtensionName> = Some(ExtensionName::Render);
    const OPCODE: u8 = 27;
    type Reply = NoReply;
    fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError> {
        writer.begin_request(major_opcode, Self::OPCODE);
        self.cid.serialize(writer);
        self.source.serialize(writer);
        writer.u16(self.x);
        writer.u16(self.y);
        writer.finish_request()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CreateNotify {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) parent: Window,
    pub(crate) window: Window,
    pub(crate) x: i16,
    pub(crate) y: i16,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) border_width: u16,
    pub(crate) override_redirect: u8,
}

impl CreateNotify {
    pub(crate) const CODE: u8 = 16;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        self.parent.serialize(writer);
        self.window.serialize(writer);
        writer.i16(self.x);
        writer.i16(self.y);
        writer.u16(self.width);
        writer.u16(self.height);
        writer.u16(self.border_width);
        writer.u8(self.override_redirect);
        writer.pad(1);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let parent = Window::parse(reader)?;
        let window = Window::parse(reader)?;
        let x = reader.get_i16()?;
        let y = reader.get_i16()?;
        let width = reader.get_u16()?;
        let height = reader.get_u16()?;
        let border_width = reader.get_u16()?;
        let override_redirect = reader.get_u8()?;
        reader.skip(1)?;
        reader.finish_event()?;
        Ok(Self {
            send_event,
            sequence,
            parent,
            window,
            x,
            y,
            width,
            height,
            border_width,
            override_redirect,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DestroyNotify {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) event: Window,
    pub(crate) window: Window,
}

impl DestroyNotify {
    pub(crate) const CODE: u8 = 17;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        self.event.serialize(writer);
        self.window.serialize(writer);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let event = Window::parse(reader)?;
        let window = Window::parse(reader)?;
        reader.finish_event()?;
        Ok(Self { send_event, sequence, event, window })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct UnmapNotify {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) event: Window,
    pub(crate) window: Window,
    pub(crate) from_configure: u8,
}

impl UnmapNotify {
    pub(crate) const CODE: u8 = 18;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        self.event.serialize(writer);
        self.window.serialize(writer);
        writer.u8(self.from_configure);
        writer.pad(3);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let event = Window::parse(reader)?;
        let window = Window::parse(reader)?;
        let from_configure = reader.get_u8()?;
        reader.skip(3)?;
        reader.finish_event()?;
        Ok(Self { send_event, sequence, event, window, from_configure })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MapNotify {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) event: Window,
    pub(crate) window: Window,
    pub(crate) override_redirect: u8,
}

impl MapNotify {
    pub(crate) const CODE: u8 = 19;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        self.event.serialize(writer);
        self.window.serialize(writer);
        writer.u8(self.override_redirect);
        writer.pad(3);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let event = Window::parse(reader)?;
        let window = Window::parse(reader)?;
        let override_redirect = reader.get_u8()?;
        reader.skip(3)?;
        reader.finish_event()?;
        Ok(Self { send_event, sequence, event, window, override_redirect })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct MapRequest {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) parent: Window,
    pub(crate) window: Window,
}

impl MapRequest {
    pub(crate) const CODE: u8 = 20;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        self.parent.serialize(writer);
        self.window.serialize(writer);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let parent = Window::parse(reader)?;
        let window = Window::parse(reader)?;
        reader.finish_event()?;
        Ok(Self { send_event, sequence, parent, window })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ReparentNotify {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) event: Window,
    pub(crate) window: Window,
    pub(crate) parent: Window,
    pub(crate) x: i16,
    pub(crate) y: i16,
    pub(crate) override_redirect: u8,
}

impl ReparentNotify {
    pub(crate) const CODE: u8 = 21;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        self.event.serialize(writer);
        self.window.serialize(writer);
        self.parent.serialize(writer);
        writer.i16(self.x);
        writer.i16(self.y);
        writer.u8(self.override_redirect);
        writer.pad(3);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let event = Window::parse(reader)?;
        let window = Window::parse(reader)?;
        let parent = Window::parse(reader)?;
        let x = reader.get_i16()?;
        let y = reader.get_i16()?;
        let override_redirect = reader.get_u8()?;
        reader.skip(3)?;
        reader.finish_event()?;
        Ok(Self {
            send_event,
            sequence,
            event,
            window,
            parent,
            x,
            y,
            override_redirect,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConfigureNotify {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) event: Window,
    pub(crate) window: Window,
    pub(crate) above_sibling: Window,
    pub(crate) x: i16,
    pub(crate) y: i16,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) border_width: u16,
    pub(crate) override_redirect: u8,
}

impl ConfigureNotify {
    pub(crate) const CODE: u8 = 22;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        self.event.serialize(writer);
        self.window.serialize(writer);
        self.above_sibling.serialize(writer);
        writer.i16(self.x);
        writer.i16(self.y);
        writer.u16(self.width);
        writer.u16(self.height);
        writer.u16(self.border_width);
        writer.u8(self.override_redirect);
        writer.pad(1);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let event = Window::parse(reader)?;
        let window = Window::parse(reader)?;
        let above_sibling = Window::parse(reader)?;
        let x = reader.get_i16()?;
        let y = reader.get_i16()?;
        let width = reader.get_u16()?;
        let height = reader.get_u16()?;
        let border_width = reader.get_u16()?;
        let override_redirect = reader.get_u8()?;
        reader.skip(1)?;
        reader.finish_event()?;
        Ok(Self {
            send_event,
            sequence,
            event,
            window,
            above_sibling,
            x,
            y,
            width,
            height,
            border_width,
            override_redirect,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConfigureRequest {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) stack_mode: StackMode,
    pub(crate) parent: Window,
    pub(crate) window: Window,
    pub(crate) sibling: Window,
    pub(crate) x: i16,
    pub(crate) y: i16,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) border_width: u16,
    pub(crate) value_mask: ConfigWindow,
}

impl ConfigureRequest {
    pub(crate) const CODE: u8 = 23;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(self.stack_mode.0 as u8);
        writer.u16(self.sequence);
        self.parent.serialize(writer);
        self.window.serialize(writer);
        self.sibling.serialize(writer);
        writer.i16(self.x);
        writer.i16(self.y);
        writer.u16(self.width);
        writer.u16(self.height);
        writer.u16(self.border_width);
        writer.u16(self.value_mask.bits() as u16);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let stack_mode = StackMode(u32::from(reader.get_u8()?));
        let sequence = reader.get_u16()?;
        let parent = Window::parse(reader)?;
        let window = Window::parse(reader)?;
        let sibling = Window::parse(reader)?;
        let x = reader.get_i16()?;
        let y = reader.get_i16()?;
        let width = reader.get_u16()?;
        let height = reader.get_u16()?;
        let border_width = reader.get_u16()?;
        let value_mask = ConfigWindow::from_bits_retain(u32::from(reader.get_u16()?));
        reader.finish_event()?;
        Ok(Self {
            send_event,
            sequence,
            stack_mode,
            parent,
            window,
            sibling,
            x,
            y,
            width,
            height,
            border_width,
            value_mask,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PropertyNotify {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) window: Window,
    pub(crate) atom: Atom,
    pub(crate) time: Timestamp,
    pub(crate) state: Property,
}

impl PropertyNotify {
    pub(crate) const CODE: u8 = 28;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        self.window.serialize(writer);
        self.atom.serialize(writer);
        writer.u32(self.time);
        writer.u8(self.state.0 as u8);
        writer.pad(3);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let window = Window::parse(reader)?;
        let atom = Atom::parse(reader)?;
        let time = reader.get_u32()?;
        let state = Property(u32::from(reader.get_u8()?));
        reader.skip(3)?;
        reader.finish_event()?;
        Ok(Self { send_event, sequence, window, atom, time, state })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SelectionClear {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) time: Timestamp,
    pub(crate) owner: Window,
    pub(crate) selection: Atom,
}

impl SelectionClear {
    pub(crate) const CODE: u8 = 29;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        writer.u32(self.time);
        self.owner.serialize(writer);
        self.selection.serialize(writer);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let time = reader.get_u32()?;
        let owner = Window::parse(reader)?;
        let selection = Atom::parse(reader)?;
        reader.finish_event()?;
        Ok(Self { send_event, sequence, time, owner, selection })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SelectionRequest {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) time: Timestamp,
    pub(crate) owner: Window,
    pub(crate) requestor: Window,
    pub(crate) selection: Atom,
    pub(crate) target: Atom,
    pub(crate) property: Atom,
}

impl SelectionRequest {
    pub(crate) const CODE: u8 = 30;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        writer.u32(self.time);
        self.owner.serialize(writer);
        self.requestor.serialize(writer);
        self.selection.serialize(writer);
        self.target.serialize(writer);
        self.property.serialize(writer);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let time = reader.get_u32()?;
        let owner = Window::parse(reader)?;
        let requestor = Window::parse(reader)?;
        let selection = Atom::parse(reader)?;
        let target = Atom::parse(reader)?;
        let property = Atom::parse(reader)?;
        reader.finish_event()?;
        Ok(Self {
            send_event,
            sequence,
            time,
            owner,
            requestor,
            selection,
            target,
            property,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SelectionNotify {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) time: Timestamp,
    pub(crate) requestor: Window,
    pub(crate) selection: Atom,
    pub(crate) target: Atom,
    pub(crate) property: Atom,
}

impl SelectionNotify {
    pub(crate) const CODE: u8 = 31;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(0);
        writer.u16(self.sequence);
        writer.u32(self.time);
        self.requestor.serialize(writer);
        self.selection.serialize(writer);
        self.target.serialize(writer);
        self.property.serialize(writer);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let _detail = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let time = reader.get_u32()?;
        let requestor = Window::parse(reader)?;
        let selection = Atom::parse(reader)?;
        let target = Atom::parse(reader)?;
        let property = Atom::parse(reader)?;
        reader.finish_event()?;
        Ok(Self {
            send_event,
            sequence,
            time,
            requestor,
            selection,
            target,
            property,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ClientMessage {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) format: u8,
    pub(crate) window: Window,
    pub(crate) r#type: Atom,
    pub(crate) data: ClientMessageData,
}

impl ClientMessage {
    pub(crate) const CODE: u8 = 33;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(self.format);
        writer.u16(self.sequence);
        self.window.serialize(writer);
        self.r#type.serialize(writer);
        self.data.serialize(writer);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let format = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let window = Window::parse(reader)?;
        let r#type = Atom::parse(reader)?;
        let data = ClientMessageData::parse(reader)?;
        reader.finish_event()?;
        Ok(Self { send_event, sequence, format, window, r#type, data })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FocusIn {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) detail: NotifyDetail,
    pub(crate) event: Window,
    pub(crate) mode: NotifyMode,
}

impl FocusIn {
    pub(crate) const CODE: u8 = 9;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(self.detail.0 as u8);
        writer.u16(self.sequence);
        self.event.serialize(writer);
        writer.u8(self.mode.0 as u8);
        writer.pad(3);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let detail = NotifyDetail(u32::from(reader.get_u8()?));
        let sequence = reader.get_u16()?;
        let event = Window::parse(reader)?;
        let mode = NotifyMode(u32::from(reader.get_u8()?));
        reader.skip(3)?;
        reader.finish_event()?;
        Ok(Self { send_event, sequence, detail, event, mode })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FocusOut {
    pub(crate) send_event: bool,
    pub(crate) sequence: u16,
    pub(crate) detail: NotifyDetail,
    pub(crate) event: Window,
    pub(crate) mode: NotifyMode,
}

impl FocusOut {
    pub(crate) const CODE: u8 = 10;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });
        writer.u8(self.detail.0 as u8);
        writer.u16(self.sequence);
        self.event.serialize(writer);
        writer.u8(self.mode.0 as u8);
        writer.pad(3);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        debug_assert_eq!(raw & 0x7f, Self::CODE);
        let detail = NotifyDetail(u32::from(reader.get_u8()?));
        let sequence = reader.get_u16()?;
        let event = Window::parse(reader)?;
        let mode = NotifyMode(u32::from(reader.get_u8()?));
        reader.skip(3)?;
        reader.finish_event()?;
        Ok(Self { send_event, sequence, detail, event, mode })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct XFixesSelectionNotify {
    pub(crate) send_event: bool,
    pub(crate) code: u8,
    pub(crate) sequence: u16,
    pub(crate) subtype: SelectionEvent,
    pub(crate) window: Window,
    pub(crate) owner: Window,
    pub(crate) selection: Atom,
    pub(crate) timestamp: Timestamp,
    pub(crate) selection_timestamp: Timestamp,
}

impl XFixesSelectionNotify {
    /// Protocol number added to the extension's first event code.
    pub(crate) const NUMBER: u8 = 0;
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(if self.send_event { self.code | 0x80 } else { self.code & 0x7f });
        writer.u8(self.subtype.0 as u8);
        writer.u16(self.sequence);
        self.window.serialize(writer);
        self.owner.serialize(writer);
        self.selection.serialize(writer);
        writer.u32(self.timestamp);
        writer.u32(self.selection_timestamp);
        writer.pad(8);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let raw = reader.get_u8()?;
        let send_event = raw & 0x80 != 0;
        let code = raw & 0x7f;
        let subtype = SelectionEvent(u32::from(reader.get_u8()?));
        let sequence = reader.get_u16()?;
        let window = Window::parse(reader)?;
        let owner = Window::parse(reader)?;
        let selection = Atom::parse(reader)?;
        let timestamp = reader.get_u32()?;
        let selection_timestamp = reader.get_u32()?;
        reader.skip(8)?;
        reader.finish_event()?;
        Ok(Self {
            send_event,
            code,
            sequence,
            subtype,
            window,
            owner,
            selection,
            timestamp,
            selection_timestamp,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ExtensionEvent {
    pub(crate) extension: ExtensionName,
    pub(crate) first_event: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Event {
    CreateNotify(CreateNotify),
    DestroyNotify(DestroyNotify),
    UnmapNotify(UnmapNotify),
    MapNotify(MapNotify),
    MapRequest(MapRequest),
    ReparentNotify(ReparentNotify),
    ConfigureNotify(ConfigureNotify),
    ConfigureRequest(ConfigureRequest),
    PropertyNotify(PropertyNotify),
    SelectionClear(SelectionClear),
    SelectionRequest(SelectionRequest),
    SelectionNotify(SelectionNotify),
    ClientMessage(ClientMessage),
    FocusIn(FocusIn),
    FocusOut(FocusOut),
    XFixesSelectionNotify(XFixesSelectionNotify),
    Unknown { send_event: bool, code: u8, bytes: Vec<u8> },
}

impl Event {
    pub(crate) fn send_event(&self) -> bool {
        match self {
            Self::CreateNotify(event) => event.send_event,
            Self::DestroyNotify(event) => event.send_event,
            Self::UnmapNotify(event) => event.send_event,
            Self::MapNotify(event) => event.send_event,
            Self::MapRequest(event) => event.send_event,
            Self::ReparentNotify(event) => event.send_event,
            Self::ConfigureNotify(event) => event.send_event,
            Self::ConfigureRequest(event) => event.send_event,
            Self::PropertyNotify(event) => event.send_event,
            Self::SelectionClear(event) => event.send_event,
            Self::SelectionRequest(event) => event.send_event,
            Self::SelectionNotify(event) => event.send_event,
            Self::ClientMessage(event) => event.send_event,
            Self::FocusIn(event) => event.send_event,
            Self::FocusOut(event) => event.send_event,
            Self::XFixesSelectionNotify(event) => event.send_event,
            Self::Unknown { send_event, .. } => *send_event,
        }
    }

    pub(crate) fn parse(data: &[u8], extensions: &[ExtensionEvent]) -> Result<Self, WireError> {
        if data.is_empty() {
            return Err(WireError::Short);
        }
        let code = data[0] & 0x7f;
        let mut reader = Reader::new(data);
        let event = match code {
            CreateNotify::CODE => Self::CreateNotify(CreateNotify::parse(&mut reader)?),
            DestroyNotify::CODE => Self::DestroyNotify(DestroyNotify::parse(&mut reader)?),
            UnmapNotify::CODE => Self::UnmapNotify(UnmapNotify::parse(&mut reader)?),
            MapNotify::CODE => Self::MapNotify(MapNotify::parse(&mut reader)?),
            MapRequest::CODE => Self::MapRequest(MapRequest::parse(&mut reader)?),
            ReparentNotify::CODE => Self::ReparentNotify(ReparentNotify::parse(&mut reader)?),
            ConfigureNotify::CODE => Self::ConfigureNotify(ConfigureNotify::parse(&mut reader)?),
            ConfigureRequest::CODE => Self::ConfigureRequest(ConfigureRequest::parse(&mut reader)?),
            PropertyNotify::CODE => Self::PropertyNotify(PropertyNotify::parse(&mut reader)?),
            SelectionClear::CODE => Self::SelectionClear(SelectionClear::parse(&mut reader)?),
            SelectionRequest::CODE => Self::SelectionRequest(SelectionRequest::parse(&mut reader)?),
            SelectionNotify::CODE => Self::SelectionNotify(SelectionNotify::parse(&mut reader)?),
            ClientMessage::CODE => Self::ClientMessage(ClientMessage::parse(&mut reader)?),
            FocusIn::CODE => Self::FocusIn(FocusIn::parse(&mut reader)?),
            FocusOut::CODE => Self::FocusOut(FocusOut::parse(&mut reader)?),
            35 => return Self::unknown(data),
            code => {
                for extension in extensions {
                    let Some(minor) = code.checked_sub(extension.first_event) else {
                        continue;
                    };
                    match (extension.extension, minor) {
                        (ExtensionName::XFixes, XFixesSelectionNotify::NUMBER) => {
                            return Ok(Self::XFixesSelectionNotify(XFixesSelectionNotify::parse(
                                &mut reader,
                            )?));
                        }
                        _ => continue,
                    }
                }
                return Self::unknown(data);
            }
        };
        if reader.remaining() != 0 {
            return Err(WireError::RequestTooLong);
        }
        Ok(event)
    }

    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        match self {
            Self::CreateNotify(event) => event.serialize(writer),
            Self::DestroyNotify(event) => event.serialize(writer),
            Self::UnmapNotify(event) => event.serialize(writer),
            Self::MapNotify(event) => event.serialize(writer),
            Self::MapRequest(event) => event.serialize(writer),
            Self::ReparentNotify(event) => event.serialize(writer),
            Self::ConfigureNotify(event) => event.serialize(writer),
            Self::ConfigureRequest(event) => event.serialize(writer),
            Self::PropertyNotify(event) => event.serialize(writer),
            Self::SelectionClear(event) => event.serialize(writer),
            Self::SelectionRequest(event) => event.serialize(writer),
            Self::SelectionNotify(event) => event.serialize(writer),
            Self::ClientMessage(event) => event.serialize(writer),
            Self::FocusIn(event) => event.serialize(writer),
            Self::FocusOut(event) => event.serialize(writer),
            Self::XFixesSelectionNotify(event) => event.serialize(writer),
            Self::Unknown { send_event, code, bytes } => {
                if bytes.is_empty() {
                    return Err(WireError::Short);
                }
                let mut bytes = bytes.clone();
                bytes[0] = code & 0x7f;
                if *send_event {
                    bytes[0] |= 0x80;
                }
                writer.bytes(&bytes);
                Ok(())
            }
        }
    }

    fn unknown(data: &[u8]) -> Result<Self, WireError> {
        if data.is_empty() {
            return Err(WireError::Short);
        }
        let mut bytes = data.to_vec();
        let send_event = bytes[0] & 0x80 != 0;
        bytes[0] &= 0x7f;
        let code = bytes[0];
        Ok(Self::Unknown { send_event, code, bytes })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct XError {
    pub(crate) code: u8,
    pub(crate) sequence: u16,
    pub(crate) bad_value: u32,
    pub(crate) minor_opcode: u16,
    pub(crate) major_opcode: u8,
}

impl XError {
    pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {
        writer.begin_event();
        writer.u8(0);
        writer.u8(self.code);
        writer.u16(self.sequence);
        writer.u32(self.bad_value);
        writer.u16(self.minor_opcode);
        writer.u8(self.major_opcode);
        writer.finish_event()
    }

    pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {
        let _response = reader.get_u8()?;
        let code = reader.get_u8()?;
        let sequence = reader.get_u16()?;
        let bad_value = reader.get_u32()?;
        let minor_opcode = reader.get_u16()?;
        let major_opcode = reader.get_u8()?;
        reader.finish_event()?;
        Ok(Self { code, sequence, bad_value, minor_opcode, major_opcode })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ExtensionError {
    pub(crate) extension: ExtensionName,
    pub(crate) first_error: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ErrorName {
    pub(crate) extension: Option<ExtensionName>,
    pub(crate) number: u8,
    pub(crate) name: &'static str,
}

pub(crate) const ERROR_NAMES: &[ErrorName] = &[
    ErrorName { extension: None, number: 1, name: "Request" },
    ErrorName { extension: None, number: 2, name: "Value" },
    ErrorName { extension: None, number: 3, name: "Window" },
    ErrorName { extension: None, number: 4, name: "Pixmap" },
    ErrorName { extension: None, number: 5, name: "Atom" },
    ErrorName { extension: None, number: 6, name: "Cursor" },
    ErrorName { extension: None, number: 7, name: "Font" },
    ErrorName { extension: None, number: 8, name: "Match" },
    ErrorName { extension: None, number: 9, name: "Drawable" },
    ErrorName { extension: None, number: 10, name: "Access" },
    ErrorName { extension: None, number: 11, name: "Alloc" },
    ErrorName { extension: None, number: 12, name: "Colormap" },
    ErrorName { extension: None, number: 13, name: "GContext" },
    ErrorName { extension: None, number: 14, name: "IDChoice" },
    ErrorName { extension: None, number: 15, name: "Name" },
    ErrorName { extension: None, number: 16, name: "Length" },
    ErrorName { extension: None, number: 17, name: "Implementation" },
    ErrorName {
        extension: Some(ExtensionName::XFixes),
        number: 0,
        name: "BadRegion",
    },
    ErrorName {
        extension: Some(ExtensionName::Render),
        number: 0,
        name: "PictFormat",
    },
    ErrorName {
        extension: Some(ExtensionName::Render),
        number: 1,
        name: "Picture",
    },
    ErrorName {
        extension: Some(ExtensionName::Render),
        number: 2,
        name: "PictOp",
    },
    ErrorName {
        extension: Some(ExtensionName::Render),
        number: 3,
        name: "GlyphSet",
    },
    ErrorName {
        extension: Some(ExtensionName::Render),
        number: 4,
        name: "Glyph",
    },
];

pub(crate) fn error_name(code: u8, extensions: &[ExtensionError]) -> Option<&'static str> {
    if code < 128 {
        return ERROR_NAMES
            .iter()
            .find(|error| error.extension.is_none() && error.number == code)
            .map(|error| error.name);
    }
    for extension in extensions {
        let Some(minor) = code.checked_sub(extension.first_error) else {
            continue;
        };
        if let Some(error) = ERROR_NAMES
            .iter()
            .find(|error| error.extension == Some(extension.extension) && error.number == minor)
        {
            return Some(error.name);
        }
    }
    None
}
