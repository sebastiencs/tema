//! Native-endian cursor over X11 bytes.
//!
//! A [`Writer`] appends to a buffer the caller owns. Alignment is measured from
//! the start of the open request or reply, or from where this writer began when
//! none is open, so earlier bytes in the same buffer do not shift padding.
//! [`Writer::finish_request`] pads a request to a multiple of 4 bytes and patches
//! its 16-bit length. [`Writer::finish_reply`] pads a reply out to 32 bytes, or
//! to a multiple of 4 when it is already longer, and patches its 32-bit length.
//! [`Writer::finish_event`] pads an event or error to exactly 32 bytes.

use std::{
    error::Error,
    fmt::{self, Display, Formatter},
};

/// Failure while reading or finishing an X11 value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WireError {
    /// The buffer ended before the value was complete.
    Short,
    /// The request is longer than the 16-bit length field can express.
    RequestTooLong,
}

impl Display for WireError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Short => formatter.write_str("X11 message is shorter than its fields"),
            Self::RequestTooLong => {
                formatter.write_str("X11 request is longer than 65535 units of 4 bytes")
            }
        }
    }
}

impl Error for WireError {}

/// Appends X11 values in the machine's native byte order.
pub(crate) struct Writer<'a> {
    buf: &'a mut Vec<u8>,
    /// Buffer length when this writer was created.
    origin: usize,
    /// Start of the request or reply that is open.
    request: Option<usize>,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(buf: &'a mut Vec<u8>) -> Self {
        let origin = buf.len();
        Self { buf, origin, request: None }
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.buf.extend(value.to_ne_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.buf.extend(value.to_ne_bytes());
    }

    pub(crate) fn i16(&mut self, value: i16) {
        self.buf.extend(value.to_ne_bytes());
    }

    pub(crate) fn i32(&mut self, value: i32) {
        self.buf.extend(value.to_ne_bytes());
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) {
        self.buf.extend_from_slice(value);
    }

    /// Writes `count` zero bytes.
    pub(crate) fn pad(&mut self, count: usize) {
        self.buf.resize(self.buf.len() + count, 0);
    }

    /// Writes zeros until the offset from the current base is a multiple of `alignment`.
    ///
    /// The base is the open request, or the writer's origin when none is open.
    pub(crate) fn align(&mut self, alignment: usize) {
        debug_assert!(alignment > 0);
        let offset = self.buf.len() - self.base();
        let remainder = offset % alignment;
        if remainder != 0 {
            self.pad(alignment - remainder);
        }
    }

    /// Starts a request. `opcode` is the major opcode and `data` is the second byte
    /// (a field, an extension minor opcode, or zero).
    pub(crate) fn begin_request(&mut self, opcode: u8, data: u8) {
        debug_assert!(self.request.is_none(), "a message is already open");
        self.request = Some(self.buf.len());
        self.u8(opcode);
        self.u8(data);
        self.u16(0);
    }

    /// Starts a reply. Byte 0 is the response type `1`, then `data`, the sequence,
    /// and a length that [`Writer::finish_reply`] fills in.
    pub(crate) fn begin_reply(&mut self, data: u8, sequence: u16) {
        debug_assert!(self.request.is_none(), "a message is already open");
        self.request = Some(self.buf.len());
        self.u8(1);
        self.u8(data);
        self.u16(sequence);
        self.u32(0);
    }

    /// Pads the open request to 4 bytes and writes its length. A request that does
    /// not fit the 16-bit length is removed from the buffer.
    pub(crate) fn finish_request(&mut self) -> Result<(), WireError> {
        let start = self.request.expect("finish_request without begin_request");
        self.align(4);
        let units = (self.buf.len() - start) / 4;
        let Ok(units) = u16::try_from(units) else {
            self.buf.truncate(start);
            self.request = None;
            return Err(WireError::RequestTooLong);
        };
        self.buf[start + 2..start + 4].copy_from_slice(&units.to_ne_bytes());
        self.request = None;
        Ok(())
    }

    /// Pads the open reply and writes its length. The length counts 4-byte units
    /// after the first 32 bytes. A reply that does not fit the 32-bit length is
    /// removed from the buffer.
    pub(crate) fn finish_reply(&mut self) -> Result<(), WireError> {
        let start = self.request.expect("finish_reply without begin_reply");
        let len = self.buf.len() - start;
        if len < 32 {
            self.pad(32 - len);
        } else {
            self.align(4);
        }
        let extra = (self.buf.len() - start - 32) / 4;
        let Ok(extra) = u32::try_from(extra) else {
            self.buf.truncate(start);
            self.request = None;
            return Err(WireError::RequestTooLong);
        };
        self.buf[start + 4..start + 8].copy_from_slice(&extra.to_ne_bytes());
        self.request = None;
        Ok(())
    }

    /// Records the start of an event or error so alignment is measured from it.
    pub(crate) fn begin_event(&mut self) {
        debug_assert!(self.request.is_none(), "a message is already open");
        self.request = Some(self.buf.len());
    }

    /// Pads the open event or error to 32 bytes. A longer message is removed.
    pub(crate) fn finish_event(&mut self) -> Result<(), WireError> {
        let start = self.request.expect("finish_event without begin_event");
        let len = self.buf.len() - start;
        if len > 32 {
            self.buf.truncate(start);
            self.request = None;
            return Err(WireError::RequestTooLong);
        }
        if len < 32 {
            self.pad(32 - len);
        }
        self.request = None;
        Ok(())
    }

    fn base(&self) -> usize {
        self.request.unwrap_or(self.origin)
    }

    #[cfg(test)]
    fn as_buf(&self) -> &[u8] {
        self.buf
    }
}

/// Reads X11 values in the machine's native byte order.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub(crate) fn get_u8(&mut self) -> Result<u8, WireError> {
        let bytes = self.bytes(1)?;
        Ok(bytes[0])
    }

    pub(crate) fn get_u16(&mut self) -> Result<u16, WireError> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_ne_bytes(bytes.try_into().unwrap()))
    }

    pub(crate) fn get_u32(&mut self) -> Result<u32, WireError> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_ne_bytes(bytes.try_into().unwrap()))
    }

    pub(crate) fn get_i16(&mut self) -> Result<i16, WireError> {
        let bytes = self.bytes(2)?;
        Ok(i16::from_ne_bytes(bytes.try_into().unwrap()))
    }

    pub(crate) fn get_i32(&mut self) -> Result<i32, WireError> {
        let bytes = self.bytes(4)?;
        Ok(i32::from_ne_bytes(bytes.try_into().unwrap()))
    }

    pub(crate) fn bytes(&mut self, count: usize) -> Result<&'a [u8], WireError> {
        let end = self.pos.checked_add(count).ok_or(WireError::Short)?;
        if end > self.data.len() {
            return Err(WireError::Short);
        }
        let bytes = &self.data[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }

    pub(crate) fn skip(&mut self, count: usize) -> Result<(), WireError> {
        self.bytes(count).map(|_| ())
    }

    /// Skips bytes until the position is a multiple of `alignment`.
    pub(crate) fn align(&mut self, alignment: usize) -> Result<(), WireError> {
        debug_assert!(alignment > 0);
        let remainder = self.pos % alignment;
        if remainder != 0 {
            self.skip(alignment - remainder)?;
        }
        Ok(())
    }

    /// Skips the padding of a reply that started at the beginning of this reader.
    ///
    /// A reply is at least 32 bytes. A longer reply is padded to a multiple of 4.
    pub(crate) fn finish_reply(&mut self) -> Result<(), WireError> {
        let remainder = self.pos % 4;
        let target = if self.pos < 32 {
            32
        } else if remainder == 0 {
            self.pos
        } else {
            self.pos + 4 - remainder
        };
        self.skip(target - self.pos)
    }

    /// Skips the padding of an event or error that started at the beginning of this reader.
    ///
    /// Events and errors are 32 bytes. A reader that has already passed byte 32 fails.
    pub(crate) fn finish_event(&mut self) -> Result<(), WireError> {
        if self.pos > 32 {
            return Err(WireError::RequestTooLong);
        }
        self.skip(32 - self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::{Reader, WireError, Writer};

    #[test]
    fn values_round_trip_in_native_byte_order() {
        let mut buf = Vec::new();
        let mut writer = Writer::new(&mut buf);
        writer.u8(0xff);
        writer.u16(0x0201);
        writer.u32(0x89ab_cdef);
        writer.i16(-2);
        writer.i32(-3);
        writer.bytes(b"atom");

        assert_eq!(&buf[1..3], &0x0201u16.to_ne_bytes());

        let mut reader = Reader::new(&buf);
        assert_eq!(reader.get_u8().unwrap(), 0xff);
        assert_eq!(reader.get_u16().unwrap(), 0x0201);
        assert_eq!(reader.get_u32().unwrap(), 0x89ab_cdef);
        assert_eq!(reader.get_i16().unwrap(), -2);
        assert_eq!(reader.get_i32().unwrap(), -3);
        assert_eq!(reader.bytes(4).unwrap(), b"atom");
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn a_request_is_padded_to_four_bytes_and_its_length_is_patched() {
        let mut buf = Vec::new();
        let mut writer = Writer::new(&mut buf);
        writer.begin_request(18, 0);
        writer.u8(1);
        writer.u8(2);
        writer.finish_request().unwrap();
        writer.begin_request(2, 3);
        writer.u32(9);
        writer.finish_request().unwrap();

        let mut expected = vec![18, 0];
        expected.extend(2u16.to_ne_bytes());
        expected.extend([1, 2, 0, 0, 2, 3]);
        expected.extend(2u16.to_ne_bytes());
        expected.extend(9u32.to_ne_bytes());
        assert_eq!(buf, expected);

        let mut reader = Reader::new(&buf);
        assert_eq!(reader.get_u8().unwrap(), 18);
        assert_eq!(reader.get_u8().unwrap(), 0);
        assert_eq!(reader.get_u16().unwrap(), 2);
        assert_eq!(reader.bytes(4).unwrap(), &[1, 2, 0, 0]);
        assert_eq!(reader.get_u8().unwrap(), 2);
        assert_eq!(reader.get_u8().unwrap(), 3);
        assert_eq!(reader.get_u16().unwrap(), 2);
        assert_eq!(reader.get_u32().unwrap(), 9);
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn alignment_is_measured_from_the_request_start() {
        let mut buf = vec![0xab];
        let mut writer = Writer::new(&mut buf);
        writer.begin_request(1, 0);
        writer.u8(7);
        writer.align(4);
        assert_eq!(writer.as_buf().len(), 1 + 8);
        assert_eq!(&writer.as_buf()[1 + 5..], &[0, 0, 0]);
        writer.finish_request().unwrap();
        assert_eq!(buf.len(), 9);

        let mut reader = Reader::new(&buf[1..]);
        assert_eq!(reader.get_u8().unwrap(), 1);
        assert_eq!(reader.get_u8().unwrap(), 0);
        assert_eq!(reader.get_u16().unwrap(), 2);
        assert_eq!(reader.get_u8().unwrap(), 7);
        reader.align(4).unwrap();
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn alignment_outside_a_request_is_measured_from_where_the_writer_started() {
        let mut buf = vec![0xab];
        {
            let mut writer = Writer::new(&mut buf);
            writer.u8(1);
            writer.align(4);
        }
        assert_eq!(buf, [0xab, 1, 0, 0, 0]);
    }

    #[test]
    fn a_request_longer_than_the_protocol_allows_is_rolled_back() {
        let mut buf = vec![0xab];
        let mut writer = Writer::new(&mut buf);
        writer.begin_request(1, 0);
        writer.pad((u16::MAX as usize + 1) * 4 - 4);
        assert_eq!(writer.finish_request(), Err(WireError::RequestTooLong));
        assert_eq!(buf, [0xab]);
    }

    #[test]
    fn the_longest_request_length_still_fits() {
        let mut buf = Vec::new();
        let mut writer = Writer::new(&mut buf);
        writer.begin_request(1, 0);
        writer.pad(u16::MAX as usize * 4 - 4);
        writer.finish_request().unwrap();
        assert_eq!(buf.len(), u16::MAX as usize * 4);
        assert_eq!(&buf[2..4], &u16::MAX.to_ne_bytes());
    }

    #[test]
    fn a_short_read_returns_short_and_keeps_the_cursor() {
        let mut reader = Reader::new(&[0x11, 0x22]);
        assert_eq!(reader.get_u32(), Err(WireError::Short));
        assert_eq!(reader.remaining(), 2);
        assert_eq!(reader.get_u8().unwrap(), 0x11);
        assert_eq!(reader.bytes(2), Err(WireError::Short));
        assert_eq!(reader.skip(2), Err(WireError::Short));
        assert_eq!(reader.align(4), Err(WireError::Short));
        assert_eq!(reader.remaining(), 1);
        assert_eq!(reader.get_u8().unwrap(), 0x22);
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn a_short_reply_is_padded_to_32_bytes_and_its_length_is_zero() {
        let mut buf = Vec::new();
        let mut writer = Writer::new(&mut buf);
        writer.begin_reply(0, 7);
        writer.u32(1);
        writer.finish_reply().unwrap();
        assert_eq!(buf.len(), 32);
        assert_eq!(buf[0], 1);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..4], &7u16.to_ne_bytes());
        assert_eq!(&buf[4..8], &0u32.to_ne_bytes());
        assert_eq!(&buf[8..12], &1u32.to_ne_bytes());
        assert!(buf[12..].iter().all(|byte| *byte == 0));

        let mut reader = Reader::new(&buf);
        assert_eq!(reader.get_u8().unwrap(), 1);
        assert_eq!(reader.get_u8().unwrap(), 0);
        assert_eq!(reader.get_u16().unwrap(), 7);
        assert_eq!(reader.get_u32().unwrap(), 0);
        assert_eq!(reader.get_u32().unwrap(), 1);
        reader.finish_reply().unwrap();
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn a_reply_past_32_bytes_aligns_to_four_and_patches_the_length() {
        let mut buf = Vec::new();
        let mut writer = Writer::new(&mut buf);
        writer.begin_reply(8, 4);
        writer.pad(24);
        writer.bytes(b"abc");
        writer.finish_reply().unwrap();
        assert_eq!(buf.len(), 36);
        assert_eq!(buf[1], 8);
        assert_eq!(&buf[2..4], &4u16.to_ne_bytes());
        assert_eq!(&buf[4..8], &1u32.to_ne_bytes());
        assert_eq!(&buf[32..35], b"abc");
        assert_eq!(buf[35], 0);

        let mut reader = Reader::new(&buf);
        reader.skip(32).unwrap();
        assert_eq!(reader.bytes(3).unwrap(), b"abc");
        reader.finish_reply().unwrap();
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn finish_reply_rejects_a_buffer_shorter_than_32_bytes() {
        let mut reader = Reader::new(&[0; 8]);
        reader.skip(8).unwrap();
        assert_eq!(reader.finish_reply(), Err(WireError::Short));
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn an_event_is_padded_to_32_bytes() {
        let mut buf = Vec::new();
        let mut writer = Writer::new(&mut buf);
        writer.begin_event();
        writer.u8(33);
        writer.u8(32);
        writer.u16(4);
        writer.finish_event().unwrap();
        assert_eq!(buf.len(), 32);
        assert_eq!(buf[0], 33);
        assert_eq!(buf[1], 32);
        assert_eq!(&buf[2..4], &4u16.to_ne_bytes());
        assert!(buf[4..].iter().all(|byte| *byte == 0));

        let mut reader = Reader::new(&buf);
        assert_eq!(reader.get_u8().unwrap(), 33);
        reader.finish_event().unwrap();
        assert_eq!(reader.remaining(), 0);
    }

    #[test]
    fn an_event_longer_than_32_bytes_is_rolled_back() {
        let mut buf = vec![0xab];
        let mut writer = Writer::new(&mut buf);
        writer.begin_event();
        writer.pad(33);
        assert_eq!(writer.finish_event(), Err(WireError::RequestTooLong));
        assert_eq!(buf, [0xab]);
    }
}
