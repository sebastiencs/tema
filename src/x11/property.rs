//! Property get, change, and delete.
//!
//! [`get_property`] sends one `GetProperty`. `long_length` is `max_bytes / 4`.
//! [`Atom::Any`] keeps the type the server returns. A requested type that
//! differs from the reply is a type error. `truncated` is set when the server
//! kept the tail.

use std::{
    error::Error,
    fmt::{self, Display, Formatter},
};

use crate::runner::{AnyAction, Redispatcher};

use super::{
    generated::{
        Atom, ChangeProperty, DeleteProperty, GetProperty, GetPropertyReply, PropMode, Window,
    },
    ConnectionError, XConnection, XReplyError,
};

/// One property value. `format` 0 means the property is absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Property {
    pub(crate) r#type: Atom,
    pub(crate) format: u8,
    pub(crate) data: Vec<u8>,
    /// The server reported bytes past the end of [`Self::data`].
    pub(crate) truncated: bool,
}

/// Why a property read or decode failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PropertyError {
    /// The connection reported an error for `GetProperty`.
    Reply(XReplyError),
    /// The property's type is not the type that was requested.
    Type { expected: Atom, actual: Atom },
    /// An accessor was used on a property of a different format.
    Format { expected: u8, actual: u8 },
    /// The byte length is not a whole number of values.
    Length,
}

/// Sends one `GetProperty` and decodes the reply into a [`Property`].
///
/// `long_offset` is 0 and `delete` is 0. `long_length` is `max_bytes / 4`.
pub(crate) fn get_property(
    connection: &mut XConnection,
    window: Window,
    property: Atom,
    ty: Atom,
    max_bytes: usize,
    redispatch: Redispatcher<Result<Property, PropertyError>>,
) -> Result<u64, ConnectionError> {
    let long_length = u32::try_from(max_bytes / 4).unwrap_or(u32::MAX);
    let slot = Redispatcher::to_any(move |result| {
        let decoded = decode(ty, result);
        redispatch
            .clone()
            .make(decoded)
            .unwrap_or_else(AnyAction::empty)
    });
    connection.send(
        &GetProperty {
            delete: 0,
            window,
            property,
            r#type: ty,
            long_offset: 0,
            long_length,
        },
        slot,
    )
}

fn decode(
    requested: Atom,
    result: Result<GetPropertyReply, XReplyError>,
) -> Result<Property, PropertyError> {
    let reply = result.map_err(PropertyError::Reply)?;
    if reply.format != 0 && requested.0 != 0 && reply.r#type != requested {
        return Err(PropertyError::Type { expected: requested, actual: reply.r#type });
    }
    Ok(Property {
        r#type: reply.r#type,
        format: reply.format,
        data: reply.value,
        truncated: reply.bytes_after != 0,
    })
}

impl Property {
    /// Format 32 values in the connection's byte order.
    pub(crate) fn as_u32s(&self) -> Result<Vec<u32>, PropertyError> {
        self.units()
    }

    pub(crate) fn as_atoms(&self) -> Result<Vec<Atom>, PropertyError> {
        Ok(self.units()?.into_iter().map(Atom).collect())
    }

    pub(crate) fn as_windows(&self) -> Result<Vec<Window>, PropertyError> {
        Ok(self.units()?.into_iter().map(Window).collect())
    }

    /// Format 8 bytes interpreted as UTF-8. Invalid sequences become U+FFFD.
    pub(crate) fn as_utf8(&self) -> Result<String, PropertyError> {
        self.check_format(8)?;
        Ok(String::from_utf8_lossy(&self.data).into_owned())
    }

    /// Format 8 bytes interpreted as Latin-1.
    pub(crate) fn as_latin1(&self) -> Result<String, PropertyError> {
        self.check_format(8)?;
        Ok(self.data.iter().copied().map(char::from).collect())
    }

    fn units(&self) -> Result<Vec<u32>, PropertyError> {
        self.check_format(32)?;
        if self.data.len() % 4 != 0 {
            return Err(PropertyError::Length);
        }
        Ok(self
            .data
            .chunks_exact(4)
            .map(|chunk| u32::from_ne_bytes(chunk.try_into().expect("chunk is 4 bytes")))
            .collect())
    }

    fn check_format(&self, expected: u8) -> Result<(), PropertyError> {
        if self.format == expected {
            Ok(())
        } else {
            Err(PropertyError::Format { expected, actual: self.format })
        }
    }
}

/// A `ChangeProperty` whose data stays owned until [`PropertyChange::request`].
pub(crate) struct PropertyChange {
    window: Window,
    property: Atom,
    ty: Atom,
    format: u8,
    data: Vec<u8>,
}

impl PropertyChange {
    pub(crate) fn request(&self) -> ChangeProperty<'_> {
        let values = match self.format {
            16 => self.data.len() / 2,
            32 => self.data.len() / 4,
            _ => self.data.len(),
        };
        ChangeProperty {
            mode: PropMode::Replace,
            window: self.window,
            property: self.property,
            r#type: self.ty,
            format: self.format,
            data_len: u32::try_from(values).expect("property data fits in a u32"),
            data: &self.data,
        }
    }
}

/// Replaces a format 8 property with these bytes.
pub(crate) fn change_property_u8s(
    window: Window,
    property: Atom,
    ty: Atom,
    data: &[u8],
) -> PropertyChange {
    PropertyChange { window, property, ty, format: 8, data: data.to_vec() }
}

/// Replaces a format 32 property with these values.
pub(crate) fn change_property_u32s(
    window: Window,
    property: Atom,
    ty: Atom,
    data: &[u32],
) -> PropertyChange {
    PropertyChange { window, property, ty, format: 32, data: u32s_to_bytes(data) }
}

/// Replaces a format 32 property with these atoms.
pub(crate) fn change_property_atoms(
    window: Window,
    property: Atom,
    ty: Atom,
    data: &[Atom],
) -> PropertyChange {
    let values: Vec<u32> = data.iter().map(|atom| atom.0).collect();
    change_property_u32s(window, property, ty, &values)
}

/// Replaces a format 8 property with the UTF-8 bytes of `text`.
pub(crate) fn change_property_utf8(
    window: Window,
    property: Atom,
    ty: Atom,
    text: &str,
) -> PropertyChange {
    change_property_u8s(window, property, ty, text.as_bytes())
}

/// A `DeleteProperty` request.
pub(crate) fn delete_property(window: Window, property: Atom) -> DeleteProperty {
    DeleteProperty { window, property }
}

fn u32s_to_bytes(values: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend(value.to_ne_bytes());
    }
    bytes
}

impl Display for PropertyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reply(error) => write!(formatter, "{error}"),
            Self::Type { expected, actual } => {
                write!(formatter, "X property type {actual:?} differs from {expected:?}")
            }
            Self::Format { expected, actual } => {
                write!(formatter, "X property format {actual} differs from {expected}")
            }
            Self::Length => {
                formatter.write_str("X property length is not a whole number of values")
            }
        }
    }
}

impl Error for PropertyError {}

#[cfg(test)]
mod tests {
    use {
        super::{
            change_property_atoms, change_property_u32s, change_property_u8s, change_property_utf8,
            delete_property, get_property, Property, PropertyError,
        },
        crate::{
            runner::{AnyAction, Dispatcher, Redispatcher},
            x11::{
                generated::{
                    Atom, BackingStore, Colormap, EventMask, GetPropertyReply, ImageOrder, Screen,
                    Setup, Window,
                },
                wire::Writer,
                State, XConnection, XReplyError,
            },
        },
    };

    fn ready() -> XConnection {
        let mut setup = Setup {
            status: 1,
            protocol_major_version: 11,
            protocol_minor_version: 0,
            length: 0,
            release_number: 1,
            resource_id_base: 0x20,
            resource_id_mask: 1,
            motion_buffer_size: 0,
            vendor_len: 0,
            maximum_request_length: 65535,
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
                root: Window(1),
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
                root_visual: 2,
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
        setup.length = u16::try_from(extra / 4).unwrap();
        bytes.clear();
        setup.serialize(&mut Writer::new(&mut bytes));
        let (mut connection, _) = XConnection::new();
        assert!(connection
            .on_read(&bytes, &mut Dispatcher::default())
            .dead
            .is_none());
        assert_eq!(connection.state(), State::Ready);
        connection
    }

    fn property(format: u8, data: &[u8]) -> Property {
        Property {
            r#type: Atom::STRING,
            format,
            data: data.to_vec(),
            truncated: false,
        }
    }

    fn reply(sequence: u16, format: u8, ty: Atom, bytes_after: u32, value: &[u8]) -> Vec<u8> {
        let value_len = match format {
            32 => u32::try_from(value.len() / 4).unwrap(),
            0 => 0,
            _ => u32::try_from(value.len()).unwrap(),
        };
        let mut bytes = Vec::new();
        GetPropertyReply {
            sequence,
            format,
            r#type: ty,
            bytes_after,
            value_len,
            value: value.to_vec(),
        }
        .serialize(&mut Writer::new(&mut bytes))
        .unwrap();
        bytes
    }

    fn requests(bytes: &[u8]) -> Vec<&[u8]> {
        let mut found = Vec::new();
        let mut offset = 0;
        while offset + 4 <= bytes.len() {
            let units = u16::from_ne_bytes(bytes[offset + 2..offset + 4].try_into().unwrap());
            let len = usize::from(units) * 4;
            found.push(&bytes[offset..offset + len]);
            offset += len;
        }
        assert_eq!(offset, bytes.len());
        found
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn results(dispatcher: &Dispatcher) -> Vec<Result<Property, PropertyError>> {
        let mut found = Vec::new();
        dispatcher.test_for_each_queued_action(|result: &Result<Property, PropertyError>| {
            found.push(result.clone())
        });
        found
    }

    #[test]
    fn decoders_honor_the_format() {
        let text = property(8, b"caf\xff");
        assert_eq!(text.as_utf8().unwrap(), "caf\u{FFFD}");
        assert_eq!(text.as_latin1().unwrap(), "caf\u{00FF}");
        assert_eq!(text.as_u32s(), Err(PropertyError::Format { expected: 32, actual: 8 }));
        assert_eq!(
            property(32, b"abcd").as_utf8(),
            Err(PropertyError::Format { expected: 8, actual: 32 })
        );

        let mut words = Vec::new();
        words.extend(0x11u32.to_ne_bytes());
        words.extend(0x22u32.to_ne_bytes());
        let numbers = property(32, &words);
        assert_eq!(numbers.as_u32s().unwrap(), vec![0x11, 0x22]);
        assert_eq!(numbers.as_atoms().unwrap(), vec![Atom(0x11), Atom(0x22)]);
        assert_eq!(numbers.as_windows().unwrap(), vec![Window(0x11), Window(0x22)]);
        assert_eq!(property(32, &[1, 2, 3]).as_u32s(), Err(PropertyError::Length));
        assert_eq!(
            property(0, &[]).as_utf8(),
            Err(PropertyError::Format { expected: 8, actual: 0 })
        );
    }

    #[test]
    fn get_property_sends_one_request_and_decodes_the_reply() {
        let mut connection = ready();
        let mut dispatcher = Dispatcher::default();
        let slot = Redispatcher::to_any(AnyAction::from);
        let window = Window(7);
        let name = Atom::WM_NAME;
        assert_eq!(
            get_property(&mut connection, window, name, Atom::Any, 10, slot.clone()).unwrap(),
            1
        );
        assert_eq!(
            get_property(&mut connection, window, name, Atom::STRING, 8, slot.clone()).unwrap(),
            2
        );
        assert_eq!(get_property(&mut connection, window, name, Atom::STRING, 4, slot).unwrap(), 3);

        let outgoing = connection.take_outgoing();
        let sent = requests(&outgoing);
        assert_eq!(sent.len(), 3);
        for request in &sent {
            assert_eq!(request[0], 20);
            assert_eq!(request[1], 0);
            assert_eq!(u32_at(request, 4), 7);
            assert_eq!(u32_at(request, 8), Atom::WM_NAME.0);
            assert_eq!(u32_at(request, 16), 0);
        }
        assert_eq!(u32_at(sent[0], 12), 0);
        assert_eq!(u32_at(sent[0], 20), 2);
        assert_eq!(u32_at(sent[1], 12), Atom::STRING.0);
        assert_eq!(u32_at(sent[1], 20), 2);
        assert_eq!(u32_at(sent[2], 20), 1);

        let mut wire = reply(1, 8, Atom::STRING, 0, b"ab\xff");
        wire.extend(reply(2, 8, Atom::WM_NAME, 6, b""));
        wire.extend(reply(3, 8, Atom::STRING, 9, b"xy"));
        assert!(connection.on_read(&wire, &mut dispatcher).dead.is_none());
        let decoded = results(&dispatcher);
        assert_eq!(decoded[0].as_ref().unwrap().as_utf8().unwrap(), "ab\u{FFFD}");
        assert!(!decoded[0].as_ref().unwrap().truncated);
        assert_eq!(decoded[0].as_ref().unwrap().r#type, Atom::STRING);
        assert_eq!(
            decoded[1],
            Err(PropertyError::Type { expected: Atom::STRING, actual: Atom::WM_NAME })
        );
        let partial = decoded[2].as_ref().unwrap();
        assert!(partial.truncated);
        assert_eq!(partial.as_latin1().unwrap(), "xy");
        assert_eq!(partial.data, b"xy");
    }

    #[test]
    fn an_absent_property_has_format_zero() {
        let mut connection = ready();
        let mut dispatcher = Dispatcher::default();
        get_property(
            &mut connection,
            Window(1),
            Atom::WM_NAME,
            Atom::STRING,
            16,
            Redispatcher::to_any(AnyAction::from),
        )
        .unwrap();
        connection.take_outgoing();
        let wire = reply(1, 0, Atom::None, 0, b"");
        assert!(connection.on_read(&wire, &mut dispatcher).dead.is_none());
        let property = results(&dispatcher).remove(0).unwrap();
        assert_eq!(property.format, 0);
        assert!(!property.truncated);
        assert_eq!(property.as_utf8(), Err(PropertyError::Format { expected: 8, actual: 0 }));
    }

    #[test]
    fn a_dead_reply_is_returned_to_the_waiter() {
        let mut connection = ready();
        let mut dispatcher = Dispatcher::default();
        get_property(
            &mut connection,
            Window(1),
            Atom::WM_NAME,
            Atom::Any,
            4,
            Redispatcher::to_any(AnyAction::from),
        )
        .unwrap();
        connection.disconnect(&mut dispatcher);
        assert_eq!(results(&dispatcher), vec![Err(PropertyError::Reply(XReplyError::Dead))]);
    }

    #[test]
    fn change_and_delete_build_replace_requests() {
        let mut connection = ready();
        let window = Window(4);
        let property = Atom::WM_NAME;
        let utf8 = change_property_utf8(window, property, Atom(42), "é");
        connection.send_void(&utf8.request()).unwrap();
        let bytes = change_property_u8s(window, property, Atom::STRING, b"ab");
        connection.send_void(&bytes.request()).unwrap();
        let numbers = change_property_u32s(window, property, Atom::CARDINAL, &[7, 8]);
        connection.send_void(&numbers.request()).unwrap();
        let atoms = change_property_atoms(window, property, Atom::ATOM, &[Atom(3), Atom(4)]);
        connection.send_void(&atoms.request()).unwrap();
        connection
            .send_void(&delete_property(window, property))
            .unwrap();

        let outgoing = connection.take_outgoing();
        let sent = requests(&outgoing);
        assert_eq!(sent.len(), 6);
        assert_eq!(sent[0][0], 18);
        assert_eq!(sent[0][1], 0);
        assert_eq!(u32_at(sent[0], 4), 4);
        assert_eq!(u32_at(sent[0], 8), Atom::WM_NAME.0);
        assert_eq!(u32_at(sent[0], 12), 42);
        assert_eq!(sent[0][16], 8);
        assert_eq!(u32_at(sent[0], 20), 2);
        assert_eq!(&sent[0][24..26], "é".as_bytes());

        assert_eq!(sent[1][16], 8);
        assert_eq!(&sent[1][24..26], b"ab");

        assert_eq!(sent[2][16], 32);
        assert_eq!(u32_at(sent[2], 20), 2);
        assert_eq!(u32_at(sent[2], 24), 7);
        assert_eq!(u32_at(sent[2], 28), 8);

        assert_eq!(sent[3][16], 32);
        assert_eq!(u32_at(sent[3], 12), Atom::ATOM.0);
        assert_eq!(u32_at(sent[3], 24), 3);
        assert_eq!(u32_at(sent[3], 28), 4);

        assert_eq!(sent[4][0], 19);
        assert_eq!(u32_at(sent[4], 4), 4);
        assert_eq!(u32_at(sent[4], 8), Atom::WM_NAME.0);
        assert_eq!(sent[5][0], 43);
    }
}
