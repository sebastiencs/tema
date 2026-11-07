//! A fake X server for tests.
//!
//! [`setup_bytes`] and [`ready_connection`] finish the handshake. [`answer_startup`]
//! replies to the extension, version, and `InternAtom` requests [`Startup::start`]
//! has already queued, then feeds each [`StartupEvent`] back into [`Startup::handle`].
//! That redispatcher must queue `StartupEvent` values, as `Redispatcher::to_any(AnyAction::from)` does.
//! Atom `i` in [`NAMES`] is `ATOM_BASE + i`.

use crate::runner::{AnyAction, Dispatcher, Redispatcher};

use super::{
    atoms::NAMES,
    generated::{
        Atom, BackingStore, Colormap, CompositeQueryVersionReply, DestroyWindow, Event, EventMask,
        GetInputFocus, GetInputFocusReply, GetPropertyReply, ImageOrder, InputFocus, InternAtom,
        InternAtomReply, MapWindow, Pictforminfo, QueryExtension, QueryExtensionReply,
        RenderQueryPictFormatsReply, RenderQueryVersionReply, Request, ResQueryVersionReply,
        Screen, Setup, SetupFailed, Window, XError, XFixesQueryVersionReply,
    },
    wire::Writer,
    Bringup, Startup, StartupEvent, State, Status, XConnection,
};

/// First atom [`answer_startup`] assigns. Name `i` in [`NAMES`] becomes this plus `i`.
pub(crate) const ATOM_BASE: u32 = 100;

pub(crate) const COMPOSITE_MAJOR_OPCODE: u8 = 142;
pub(crate) const COMPOSITE_FIRST_EVENT: u8 = 80;
pub(crate) const COMPOSITE_FIRST_ERROR: u8 = 140;
pub(crate) const XFIXES_MAJOR_OPCODE: u8 = 143;
pub(crate) const XFIXES_FIRST_EVENT: u8 = 85;
pub(crate) const XFIXES_FIRST_ERROR: u8 = 160;
pub(crate) const RENDER_MAJOR_OPCODE: u8 = 144;
pub(crate) const RENDER_FIRST_EVENT: u8 = 90;
pub(crate) const RENDER_FIRST_ERROR: u8 = 170;
pub(crate) const RES_MAJOR_OPCODE: u8 = 145;

struct FixedExtension {
    name: &'static [u8],
    major_opcode: u8,
    first_event: u8,
    first_error: u8,
}

const EXTENSIONS: [FixedExtension; 4] = [
    FixedExtension {
        name: b"Composite",
        major_opcode: COMPOSITE_MAJOR_OPCODE,
        first_event: COMPOSITE_FIRST_EVENT,
        first_error: COMPOSITE_FIRST_ERROR,
    },
    FixedExtension {
        name: b"XFIXES",
        major_opcode: XFIXES_MAJOR_OPCODE,
        first_event: XFIXES_FIRST_EVENT,
        first_error: XFIXES_FIRST_ERROR,
    },
    FixedExtension {
        name: b"RENDER",
        major_opcode: RENDER_MAJOR_OPCODE,
        first_event: RENDER_FIRST_EVENT,
        first_error: RENDER_FIRST_ERROR,
    },
    FixedExtension {
        name: b"X-Resource",
        major_opcode: RES_MAJOR_OPCODE,
        first_event: 0,
        first_error: 0,
    },
];

/// A successful setup reply. The screen's root visual is `0x21`.
pub(crate) fn setup_bytes(root: Window, base: u32, mask: u32) -> Vec<u8> {
    let mut setup = Setup {
        status: 1,
        protocol_major_version: 11,
        protocol_minor_version: 0,
        length: 0,
        release_number: 1,
        resource_id_base: base,
        resource_id_mask: mask,
        motion_buffer_size: 0,
        vendor_len: 0,
        maximum_request_length: 256,
        roots_len: 1,
        pixmap_formats_len: 0,
        image_byte_order: ImageOrder::LSBFirst,
        bitmap_format_bit_order: ImageOrder::LSBFirst,
        bitmap_format_scanline_unit: 32,
        bitmap_format_scanline_pad: 32,
        min_keycode: 8,
        max_keycode: 255,
        vendor: Vec::new(),
        pixmap_formats: Vec::new(),
        roots: vec![Screen {
            root,
            default_colormap: Colormap(0),
            white_pixel: 0,
            black_pixel: 0,
            current_input_masks: EventMask::empty(),
            width_in_pixels: 1,
            height_in_pixels: 1,
            width_in_millimeters: 1,
            height_in_millimeters: 1,
            min_installed_maps: 1,
            max_installed_maps: 1,
            root_visual: 0x21,
            backing_stores: BackingStore::NotUseful,
            save_unders: 0,
            root_depth: 24,
            allowed_depths_len: 0,
            allowed_depths: Vec::new(),
        }],
    };
    let mut bytes = Vec::new();
    setup.serialize(&mut Writer::new(&mut bytes));
    let extra = bytes.len() - 8;
    setup.length = u16::try_from(extra / 4).expect("setup length fits in u16");
    bytes.clear();
    setup.serialize(&mut Writer::new(&mut bytes));
    bytes
}

/// A setup failure. `reason` is the text the connection reports.
pub(crate) fn setup_failed(reason: &str) -> Vec<u8> {
    let reason = reason.as_bytes();
    let units = reason.len().div_ceil(4);
    let failed = SetupFailed {
        status: 0,
        reason_len: u8::try_from(reason.len()).expect("setup failure reason fits in one byte"),
        protocol_major_version: 11,
        protocol_minor_version: 0,
        length: u16::try_from(units).expect("setup failure length fits"),
        reason: reason.to_vec(),
    };
    let mut bytes = Vec::new();
    failed.serialize(&mut Writer::new(&mut bytes));
    bytes.resize(8 + units * 4, 0);
    bytes
}

/// A connection past the handshake. Root `0x100`, resource base `0x20`, mask `0b1100`.
pub(crate) fn ready_connection() -> (XConnection, Dispatcher) {
    let bytes = setup_bytes(Window(0x100), 0x20, 0b1100);
    let (mut connection, _) = XConnection::new();
    let mut dispatcher = Dispatcher::default();
    let read = connection.on_read(&bytes, &mut dispatcher);
    assert!(read.dead.is_none(), "setup failed: {:?}", read.dead);
    assert_eq!(connection.state(), State::Ready);
    (connection, dispatcher)
}

/// Bytes a closure appends. Used to build one reply.
pub(crate) fn reply_bytes(write: impl FnOnce(&mut Writer<'_>)) -> Vec<u8> {
    let mut bytes = Vec::new();
    write(&mut Writer::new(&mut bytes));
    bytes
}

/// One serialized event.
pub(crate) fn event_bytes(event: &Event) -> Vec<u8> {
    let mut bytes = Vec::new();
    event
        .serialize(&mut Writer::new(&mut bytes))
        .expect("event encodes");
    bytes
}

/// Cuts a request stream on the 16-bit length at bytes 2..4.
pub(crate) fn split_requests(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        assert!(offset + 4 <= bytes.len(), "X11 request is shorter than its header");
        let units = u16::from_ne_bytes(bytes[offset + 2..offset + 4].try_into().unwrap());
        assert!(units > 0, "X11 request length is zero");
        let len = usize::from(units) * 4;
        assert!(offset + len <= bytes.len(), "X11 request extends past the buffer");
        found.push(bytes[offset..offset + len].to_vec());
        offset += len;
    }
    found
}

/// One request, serialized with `major_opcode` (0 for a core request).
pub(crate) fn encode<R: Request>(request: &R, major_opcode: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    request
        .serialize(&mut Writer::new(&mut bytes), major_opcode)
        .expect("request encodes");
    bytes
}

/// One `GetProperty` reply. `format` 0 is an absent property.
pub(crate) fn property_reply(
    sequence: u16,
    format: u8,
    ty: Atom,
    bytes_after: u32,
    value: &[u8],
) -> Vec<u8> {
    let unit = match format {
        16 => 2,
        32 => 4,
        _ => 1,
    };
    let value_len = u32::try_from(value.len() / unit).expect("property value is whole units");
    let reply = GetPropertyReply {
        sequence,
        format,
        r#type: ty,
        bytes_after,
        value_len,
        value: value.to_vec(),
    };
    let mut bytes = Vec::new();
    reply
        .serialize(&mut Writer::new(&mut bytes))
        .expect("property reply encodes");
    bytes
}

/// A `RenderQueryPictFormats` reply listing `formats`.
pub(crate) fn pict_formats_reply(sequence: u16, formats: Vec<Pictforminfo>) -> Vec<u8> {
    let reply = RenderQueryPictFormatsReply {
        sequence,
        num_depths: 0,
        num_visuals: 0,
        formats,
        screens: Vec::new(),
        subpixels: Vec::new(),
    };
    let mut bytes = Vec::new();
    reply
        .serialize(&mut Writer::new(&mut bytes))
        .expect("formats encode");
    bytes
}

/// One X error frame. `code` 10 is Access.
pub(crate) fn error_bytes(sequence: u16, code: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    XError {
        code,
        sequence,
        bad_value: 0,
        minor_opcode: 0,
        major_opcode: 0,
    }
    .serialize(&mut Writer::new(&mut bytes))
    .expect("error encodes");
    bytes
}

/// Replies for one batch of requests. `sequence` and `next_atom` advance past the batch.
pub(crate) fn reply_batch(bytes: &[u8], sequence: &mut u16, next_atom: &mut u32) -> Vec<u8> {
    let mut replies = Vec::new();
    for request in split_requests(bytes) {
        replies.extend(reply_to(&request, *sequence, next_atom));
        *sequence = sequence
            .checked_add(1)
            .expect("startup sequence fits in u16");
    }
    replies
}

/// Drives [`Startup`] to [`Status::Ready`].
///
/// `start` has already run. Sequences begin at 1, so this connection has sent nothing else.
pub(crate) fn answer_startup(
    connection: &mut XConnection,
    dispatcher: &mut Dispatcher,
    startup: &mut Startup,
) -> Bringup {
    let mut sequence = 1u16;
    let mut next_atom = ATOM_BASE;
    loop {
        let outgoing = connection.take_outgoing();
        if outgoing.is_empty() {
            break;
        }
        let replies = reply_batch(&outgoing, &mut sequence, &mut next_atom);
        let read = connection.on_read(&replies, dispatcher);
        assert!(read.dead.is_none(), "fake server reply killed the connection: {:?}", read.dead);
        for event in dispatcher.test_take_queued_actions::<StartupEvent>() {
            if let Status::Failed(error) = startup.handle(event, connection) {
                panic!("startup failed: {error}");
            }
        }
    }
    match startup.status() {
        Status::Ready(bringup) => bringup,
        other => panic!("startup stopped at {other:?}"),
    }
}

fn reply_to(request: &[u8], sequence: u16, next_atom: &mut u32) -> Vec<u8> {
    let opcode = request[0];
    let mut bytes = Vec::new();
    let mut writer = Writer::new(&mut bytes);
    match opcode {
        QueryExtension::OPCODE => {
            let info = extension_named(query_name(request));
            QueryExtensionReply {
                sequence,
                present: 1,
                major_opcode: info.major_opcode,
                first_event: info.first_event,
                first_error: info.first_error,
            }
            .serialize(&mut writer)
            .expect("extension reply encodes");
        }
        InternAtom::OPCODE => {
            let atom = Atom(*next_atom);
            *next_atom += 1;
            InternAtomReply { sequence, atom }
                .serialize(&mut writer)
                .expect("intern reply encodes");
        }
        GetInputFocus::OPCODE => {
            GetInputFocusReply { sequence, revert_to: InputFocus::None, focus: Window::None }
                .serialize(&mut writer)
                .expect("focus reply encodes");
        }
        COMPOSITE_MAJOR_OPCODE => {
            CompositeQueryVersionReply { sequence, major_version: 0, minor_version: 4 }
                .serialize(&mut writer)
                .expect("composite version reply encodes")
        }
        XFIXES_MAJOR_OPCODE => {
            XFixesQueryVersionReply { sequence, major_version: 6, minor_version: 0 }
                .serialize(&mut writer)
                .expect("xfixes version reply encodes");
        }
        RENDER_MAJOR_OPCODE => {
            RenderQueryVersionReply { sequence, major_version: 0, minor_version: 11 }
                .serialize(&mut writer)
                .expect("render version reply encodes");
        }
        RES_MAJOR_OPCODE => {
            ResQueryVersionReply { sequence, server_major: 1, server_minor: 2 }
                .serialize(&mut writer)
                .expect("res version reply encodes");
        }
        other => panic!("fake server has no reply for opcode {other}"),
    }
    bytes
}

fn extension_named(name: &[u8]) -> &'static FixedExtension {
    EXTENSIONS
        .iter()
        .find(|extension| extension.name == name)
        .unwrap_or_else(|| panic!("fake server has no extension {}", String::from_utf8_lossy(name)))
}

fn query_name(request: &[u8]) -> &[u8] {
    let len = u16::from_ne_bytes(request[4..6].try_into().unwrap()) as usize;
    &request[8..8 + len]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_requests_cuts_two_concatenated_requests() {
        let mut bytes = encode(&DestroyWindow { window: Window(1) }, 0);
        bytes.extend(encode(&MapWindow { window: Window(2) }, 0));
        let requests = split_requests(&bytes);
        assert_eq!(
            requests,
            vec![
                encode(&DestroyWindow { window: Window(1) }, 0),
                encode(&MapWindow { window: Window(2) }, 0)
            ]
        );
    }

    #[test]
    fn answer_startup_reaches_ready_with_the_expected_atoms() {
        let (mut connection, mut dispatcher) = ready_connection();
        let mut startup = Startup::new();
        let status = startup.start(&mut connection, Redispatcher::to_any(AnyAction::from));
        assert_eq!(status, Status::Pending);
        let bringup = answer_startup(&mut connection, &mut dispatcher, &mut startup);
        let index = |name: &str| NAMES.iter().position(|entry| *entry == name).unwrap() as u32;
        assert_eq!(bringup.atoms.WM_S0, Atom(ATOM_BASE + index("WM_S0")));
        assert_eq!(bringup.atoms.UTF8_STRING, Atom(ATOM_BASE + index("UTF8_STRING")));
        assert_eq!(bringup.atoms.WM_NAME, Atom::WM_NAME);
        assert_eq!(bringup.extensions.composite.major_opcode, COMPOSITE_MAJOR_OPCODE);
        assert_eq!(bringup.extensions.composite.major_version, 0);
        assert_eq!(bringup.extensions.composite.minor_version, 4);
        assert_eq!(bringup.extensions.xfixes.major_version, 6);
        assert_eq!(bringup.extensions.xfixes.first_event, XFIXES_FIRST_EVENT);
        assert_eq!(bringup.extensions.render.minor_version, 11);
        assert_eq!(bringup.extensions.render.major_opcode, RENDER_MAJOR_OPCODE);
        let res = bringup.extensions.res.expect("X-Resource is present");
        assert_eq!(res.major_version, 1);
        assert_eq!(res.minor_version, 2);
        assert_eq!(res.major_opcode, RES_MAJOR_OPCODE);
        assert_eq!(connection.state(), State::Ready);
    }
}
