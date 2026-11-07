//! io_uring driver for one X connection.
//!
//! One `ReadSome` of 64 KiB stays armed. `flush` coalesces the core's outgoing
//! bytes into one `Write`. The ring retries a short write before that `Write`
//! completes, so a second `Write` waits until then. `stop` cancels the read.
//! Each completion carries the generation from when it was armed; a later
//! generation drops it.

use std::os::fd::RawFd;

use log::error;

use crate::{
    io_uring::{
        model::IoUringAction,
        ring::{ReadWriteCompleted, RingId},
    },
    runner::{Dispatcher, Redispatcher},
};

use super::{connection::Read, State, XConnection};

/// Bytes requested by each `ReadSome`.
const READ_LEN: usize = 64 * 1024;

type Completion = std::io::Result<ReadWriteCompleted>;

/// The fd, the armed read, and bytes waiting for the one in-flight write.
pub(crate) struct XIo {
    fd: RawFd,
    generation: u64,
    read_id: Option<RingId>,
    /// Token of the `Write` that has been queued and has not completed.
    write_in_flight: Option<u64>,
    next_write: u64,
    pending: Vec<u8>,
    make_read: Option<Box<dyn Fn(u64, RingId) -> Redispatcher<Completion>>>,
    make_write: Option<Box<dyn Fn(u64, u64) -> Redispatcher<Completion>>>,
}

impl XIo {
    pub(crate) fn new(fd: RawFd) -> Self {
        Self {
            fd,
            generation: 0,
            read_id: None,
            write_in_flight: None,
            next_write: 1,
            pending: Vec::new(),
            make_read: None,
            make_write: None,
        }
    }

    /// Stores the owner's redispatchers and arms a read.
    ///
    /// `make_read` and `make_write` close over the generation and the read id
    /// or write token, and the owner later passes those to [`on_read`] and
    /// [`on_written`]. A second `start` while a read is armed replaces the
    /// redispatchers and leaves that read in place.
    pub(crate) fn start<R, W>(&mut self, dispatcher: &mut Dispatcher, make_read: R, make_write: W)
    where
        R: Fn(u64, RingId) -> Redispatcher<Completion> + 'static,
        W: Fn(u64, u64) -> Redispatcher<Completion> + 'static,
    {
        self.make_read = Some(Box::new(make_read));
        self.make_write = Some(Box::new(make_write));
        self.arm_read(dispatcher);
        self.pump_write(dispatcher);
    }

    /// Queues bytes that are not in the core, such as the setup request from
    /// [`XConnection::new`].
    pub(crate) fn queue_bytes(&mut self, bytes: &[u8], dispatcher: &mut Dispatcher) {
        if bytes.is_empty() {
            return;
        }
        self.pending.extend_from_slice(bytes);
        self.pump_write(dispatcher);
    }

    /// Moves the core's outgoing bytes into the one in-flight write.
    pub(crate) fn flush(&mut self, core: &mut XConnection, dispatcher: &mut Dispatcher) {
        self.queue_bytes(&core.take_outgoing(), dispatcher);
    }

    /// Feeds a finished read to the core and arms the next one.
    ///
    /// An empty buffer is the end of the stream. A read error, an empty read,
    /// or a core that has died leaves the read unarmed.
    pub(crate) fn on_read(
        &mut self,
        generation: u64,
        read_id: RingId,
        result: Completion,
        core: &mut XConnection,
        dispatcher: &mut Dispatcher,
    ) -> Read {
        if generation != self.generation || self.read_id != Some(read_id) {
            return empty_read();
        }
        self.read_id = None;
        let bytes = match result {
            Ok(completed) if !completed.buffer.is_empty() => completed.buffer,
            Ok(_) => {
                error!("X11 read failed: unexpected end of file");
                return self.fail(core, dispatcher);
            }
            Err(error) => {
                error!("X11 read failed: {error}");
                return self.fail(core, dispatcher);
            }
        };
        let read = core.on_read(&bytes, dispatcher);
        if core.state() == State::Dead {
            self.halt(dispatcher);
            return read;
        }
        self.arm_read(dispatcher);
        read
    }

    /// A finished `Write` frees the slot, so any bytes queued behind it go out.
    pub(crate) fn on_written(
        &mut self,
        generation: u64,
        token: u64,
        result: Completion,
        core: &mut XConnection,
        dispatcher: &mut Dispatcher,
    ) -> Read {
        if generation != self.generation || self.write_in_flight != Some(token) {
            return empty_read();
        }
        self.write_in_flight = None;
        if let Err(error) = result {
            error!("X11 write failed: {error}");
            return self.fail(core, dispatcher);
        }
        self.pump_write(dispatcher);
        empty_read()
    }

    /// Cancels the armed read and drops a completion armed before this call.
    ///
    /// The core stays as it is. Bytes still in `pending` are discarded.
    pub(crate) fn stop(&mut self, dispatcher: &mut Dispatcher) {
        self.halt(dispatcher);
    }

    fn arm_read(&mut self, dispatcher: &mut Dispatcher) {
        if self.read_id.is_some() {
            return;
        }
        let generation = self.generation;
        let read_id = RingId::fresh();
        let dispatch_to = match self.make_read.as_ref() {
            Some(make_read) => make_read(generation, read_id),
            None => return,
        };
        self.read_id = Some(read_id);
        dispatcher.dispatch(IoUringAction::ReadSome {
            id: Some(read_id),
            fd: self.fd,
            offset: 0,
            length: READ_LEN,
            dispatch_to,
        });
    }

    fn pump_write(&mut self, dispatcher: &mut Dispatcher) {
        if self.write_in_flight.is_some() || self.pending.is_empty() {
            return;
        }
        let generation = self.generation;
        let token = self.next_write;
        let dispatch_to = match self.make_write.as_ref() {
            Some(make_write) => make_write(generation, token),
            None => return,
        };
        self.next_write = self.next_write.wrapping_add(1);
        let buffer = std::mem::take(&mut self.pending).into_boxed_slice();
        self.write_in_flight = Some(token);
        dispatcher.dispatch(IoUringAction::Write { fd: self.fd, offset: 0, buffer, dispatch_to });
    }

    fn fail(&mut self, core: &mut XConnection, dispatcher: &mut Dispatcher) -> Read {
        let read =
            if core.state() == State::Dead { empty_read() } else { core.disconnect(dispatcher) };
        self.halt(dispatcher);
        read
    }

    fn halt(&mut self, dispatcher: &mut Dispatcher) {
        if let Some(read_id) = self.read_id.take() {
            dispatcher.dispatch(IoUringAction::Cancel { target: read_id });
        }
        self.write_in_flight = None;
        self.pending.clear();
        self.generation = self.generation.wrapping_add(1);
    }
}

fn empty_read() -> Read {
    Read { events: Vec::new(), dead: None }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, io::ErrorKind, rc::Rc};

    use {
        super::{XIo, READ_LEN},
        crate::{
            io_uring::{
                model::IoUringAction,
                ring::{ReadWriteCompleted, RingId},
            },
            runner::{Dispatcher, Redispatcher},
            x11::{
                generated::{
                    Atom, BackingStore, Colormap, DestroyWindow, Event, EventMask, GetInputFocus,
                    ImageOrder, Property, PropertyNotify, Request, Screen, Setup, Window,
                },
                wire::Writer,
                ConnectionError, State, XConnection,
            },
        },
    };

    const FD: i32 = 7;

    fn started() -> (XIo, Dispatcher) {
        let mut io = XIo::new(FD);
        let mut dispatcher = Dispatcher::default();
        io.start(&mut dispatcher, |_, _| Redispatcher::None, |_, _| Redispatcher::None);
        (io, dispatcher)
    }

    fn completed(bytes: &[u8]) -> ReadWriteCompleted {
        ReadWriteCompleted { fd: FD, offset: 0, buffer: bytes.to_vec().into_boxed_slice() }
    }

    fn read_ids(dispatcher: &Dispatcher) -> Vec<(RingId, i32, usize, usize)> {
        let mut reads = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::ReadSome { id, fd, offset, length, .. } = action {
                reads.push((id.expect("read id"), *fd, *offset, *length));
            }
        });
        reads
    }

    fn write_buffers(dispatcher: &Dispatcher) -> Vec<Vec<u8>> {
        let mut writes = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::Write { fd, offset, buffer, .. } = action {
                assert_eq!(*fd, FD);
                assert_eq!(*offset, 0);
                writes.push(buffer.to_vec());
            }
        });
        writes
    }

    fn cancels(dispatcher: &Dispatcher) -> Vec<RingId> {
        let mut targets = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::Cancel { target } = action {
                targets.push(*target);
            }
        });
        targets
    }

    fn screen(root: u32, visual: u32) -> Screen {
        Screen {
            root: Window(root),
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
            root_visual: visual,
            backing_stores: BackingStore::NotUseful,
            save_unders: 0,
            root_depth: 24,
            allowed_depths_len: 0,
            allowed_depths: Vec::new(),
        }
    }

    fn setup_bytes() -> Vec<u8> {
        let mut setup = Setup {
            status: 1,
            protocol_major_version: 11,
            protocol_minor_version: 0,
            length: 0,
            release_number: 1,
            resource_id_base: 0x20,
            resource_id_mask: 0b1100,
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
            roots: vec![screen(0x100, 0x21)],
        };
        let mut bytes = Vec::new();
        setup.serialize(&mut Writer::new(&mut bytes));
        let extra = bytes.len() - 8;
        setup.length = u16::try_from(extra / 4).unwrap();
        bytes.clear();
        setup.serialize(&mut Writer::new(&mut bytes));
        bytes
    }

    fn ready_core() -> XConnection {
        let (mut core, _) = XConnection::new();
        let read = core.on_read(&setup_bytes(), &mut Dispatcher::default());
        assert!(read.dead.is_none());
        assert_eq!(core.state(), State::Ready);
        core
    }

    #[test]
    fn start_arms_one_64_kib_read() {
        let calls = Rc::new(Cell::new(0));
        let seen = Rc::clone(&calls);
        let mut io = XIo::new(FD);
        let mut dispatcher = Dispatcher::default();
        io.start(
            &mut dispatcher,
            move |generation, read_id| {
                seen.set(seen.get() + 1);
                assert_eq!(generation, 0);
                assert_ne!(read_id, RingId(0));
                Redispatcher::None
            },
            |_, _| Redispatcher::None,
        );
        io.start(&mut dispatcher, |_, _| Redispatcher::None, |_, _| Redispatcher::None);
        assert_eq!(calls.get(), 1);
        assert_eq!(read_ids(&dispatcher), vec![(io.read_id.unwrap(), FD, 0, READ_LEN)]);
        assert_eq!(READ_LEN, 64 * 1024);
        assert!(write_buffers(&dispatcher).is_empty());
    }

    #[test]
    fn flush_coalesces_voids_into_one_write() {
        let mut core = ready_core();
        let (mut io, mut dispatcher) = started();
        core.send_void(&DestroyWindow { window: Window(1) })
            .unwrap();
        core.send_void(&DestroyWindow { window: Window(2) })
            .unwrap();
        io.flush(&mut core, &mut dispatcher);
        let writes = write_buffers(&dispatcher);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].len(), 20);
        assert_eq!(writes[0][0], DestroyWindow::OPCODE);
        assert_eq!(writes[0][8], DestroyWindow::OPCODE);
        assert_eq!(writes[0][16], GetInputFocus::OPCODE);

        core.send_void(&DestroyWindow { window: Window(3) })
            .unwrap();
        io.flush(&mut core, &mut dispatcher);
        assert_eq!(write_buffers(&dispatcher).len(), 1);

        let generation = io.generation;
        let token = io.write_in_flight.unwrap();
        let read = io.on_written(generation, token, Ok(completed(&[])), &mut core, &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(core.state(), State::Ready);
        let writes = write_buffers(&dispatcher);
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[1].len(), 12);
        assert_eq!(writes[1][0], DestroyWindow::OPCODE);
        assert_eq!(writes[1][8], GetInputFocus::OPCODE);

        let stale =
            io.on_written(generation, token, Ok(completed(&[])), &mut core, &mut dispatcher);
        assert!(stale.dead.is_none());
        assert_eq!(write_buffers(&dispatcher).len(), 2);
    }

    #[test]
    fn queued_bytes_wait_behind_the_write_in_flight() {
        let (mut core, setup) = XConnection::new();
        let (mut io, mut dispatcher) = started();
        io.queue_bytes(&setup, &mut dispatcher);
        io.queue_bytes(b"more", &mut dispatcher);
        assert_eq!(write_buffers(&dispatcher), vec![setup]);

        let generation = io.generation;
        let token = io.write_in_flight.unwrap();
        let read = io.on_written(generation, token, Ok(completed(&[])), &mut core, &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(core.state(), State::Handshake);
        assert_eq!(write_buffers(&dispatcher)[1], b"more");
    }

    #[test]
    fn a_finished_handshake_arms_the_next_read() {
        let bytes = setup_bytes();
        let (mut core, _) = XConnection::new();
        let (mut io, mut dispatcher) = started();
        let first = read_ids(&dispatcher)[0].0;
        let generation = io.generation;
        let read =
            io.on_read(generation, first, Ok(completed(&bytes[..8])), &mut core, &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(core.state(), State::Handshake);

        let second = read_ids(&dispatcher)[1].0;
        let read =
            io.on_read(generation, second, Ok(completed(&bytes[8..])), &mut core, &mut dispatcher);
        assert!(read.dead.is_none());
        assert!(read.events.is_empty());
        assert_eq!(core.state(), State::Ready);
        assert_eq!(core.root(), Some(Window(0x100)));

        let mut event = Vec::new();
        PropertyNotify {
            send_event: false,
            sequence: 0,
            window: Window(0x100),
            atom: Atom(39),
            time: 1,
            state: Property(0),
        }
        .serialize(&mut Writer::new(&mut event))
        .unwrap();
        let third = read_ids(&dispatcher)[2].0;
        let read = io.on_read(generation, third, Ok(completed(&event)), &mut core, &mut dispatcher);
        assert!(read.dead.is_none());
        assert!(matches!(read.events.as_slice(), [Event::PropertyNotify(_)]));
        assert_eq!(read_ids(&dispatcher).len(), 4);
        assert_eq!(read_ids(&dispatcher)[3].3, READ_LEN);

        let late = io.on_read(generation, first, Ok(completed(&event)), &mut core, &mut dispatcher);
        assert!(late.events.is_empty());
        assert_eq!(read_ids(&dispatcher).len(), 4);
    }

    #[test]
    fn a_read_error_or_empty_read_kills_the_connection() {
        let (mut core, _) = XConnection::new();
        let (mut io, mut dispatcher) = started();
        let read_id = read_ids(&dispatcher)[0].0;
        let generation = io.generation;
        let read = io.on_read(
            generation,
            read_id,
            Err(ErrorKind::ConnectionReset.into()),
            &mut core,
            &mut dispatcher,
        );
        assert_eq!(read.dead, Some(ConnectionError::Dead));
        assert_eq!(core.state(), State::Dead);
        assert_eq!(read_ids(&dispatcher).len(), 1);
        assert!(cancels(&dispatcher).is_empty());

        let again = io.on_read(
            generation,
            read_id,
            Err(ErrorKind::ConnectionReset.into()),
            &mut core,
            &mut dispatcher,
        );
        assert!(again.dead.is_none());
        assert_eq!(cancels(&dispatcher).len(), 0);

        let (mut core, _) = XConnection::new();
        let (mut io, mut dispatcher) = started();
        let read_id = read_ids(&dispatcher)[0].0;
        let read =
            io.on_read(io.generation, read_id, Ok(completed(&[])), &mut core, &mut dispatcher);
        assert_eq!(read.dead, Some(ConnectionError::Dead));
        assert_eq!(core.state(), State::Dead);
        assert_eq!(read_ids(&dispatcher).len(), 1);
    }

    #[test]
    fn a_malformed_setup_keeps_its_reason() {
        let (mut core, _) = XConnection::new();
        let (mut io, mut dispatcher) = started();
        let read_id = read_ids(&dispatcher)[0].0;
        let read = io.on_read(
            io.generation,
            read_id,
            Ok(completed(&[3, 0, 0, 0, 0, 0, 0, 0])),
            &mut core,
            &mut dispatcher,
        );
        assert_eq!(read.dead, Some(ConnectionError::Malformed));
        assert_eq!(core.state(), State::Dead);
        assert_eq!(read_ids(&dispatcher).len(), 1);
        assert!(cancels(&dispatcher).is_empty());
    }

    #[test]
    fn a_write_error_cancels_the_read() {
        let (mut core, setup) = XConnection::new();
        let (mut io, mut dispatcher) = started();
        let read_id = read_ids(&dispatcher)[0].0;
        io.queue_bytes(&setup, &mut dispatcher);
        let generation = io.generation;
        let token = io.write_in_flight.unwrap();
        let read = io.on_written(
            generation,
            token,
            Err(ErrorKind::BrokenPipe.into()),
            &mut core,
            &mut dispatcher,
        );
        assert_eq!(read.dead, Some(ConnectionError::Dead));
        assert_eq!(core.state(), State::Dead);
        assert_eq!(cancels(&dispatcher), vec![read_id]);
        assert_eq!(write_buffers(&dispatcher).len(), 1);

        let again = io.on_written(
            generation,
            token,
            Err(ErrorKind::BrokenPipe.into()),
            &mut core,
            &mut dispatcher,
        );
        assert!(again.dead.is_none());
        assert_eq!(cancels(&dispatcher), vec![read_id]);
    }

    #[test]
    fn stop_cancels_the_read_and_drops_a_stale_completion() {
        let bytes = setup_bytes();
        let (mut core, _) = XConnection::new();
        let (mut io, mut dispatcher) = started();
        let read_id = read_ids(&dispatcher)[0].0;
        let generation = io.generation;
        io.queue_bytes(b"first", &mut dispatcher);
        io.queue_bytes(b"second", &mut dispatcher);
        let token = io.write_in_flight.unwrap();
        io.stop(&mut dispatcher);
        assert_eq!(cancels(&dispatcher), vec![read_id]);
        assert!(io.read_id.is_none());
        assert!(io.write_in_flight.is_none());
        assert_eq!(core.state(), State::Handshake);
        assert_eq!(write_buffers(&dispatcher), vec![b"first".to_vec()]);

        let read =
            io.on_read(generation, read_id, Ok(completed(&bytes)), &mut core, &mut dispatcher);
        assert!(read.dead.is_none());
        assert_eq!(core.state(), State::Handshake);
        assert_eq!(read_ids(&dispatcher).len(), 1);

        let written =
            io.on_written(generation, token, Ok(completed(&[])), &mut core, &mut dispatcher);
        assert!(written.dead.is_none());
        assert_eq!(write_buffers(&dispatcher), vec![b"first".to_vec()]);

        io.start(&mut dispatcher, |_, _| Redispatcher::None, |_, _| Redispatcher::None);
        assert_eq!(read_ids(&dispatcher).len(), 2);
        assert_eq!(read_ids(&dispatcher)[1].3, READ_LEN);
        assert_eq!(write_buffers(&dispatcher), vec![b"first".to_vec()]);
    }

    #[test]
    fn flush_with_nothing_outgoing_queues_no_write() {
        let mut core = ready_core();
        let (mut io, mut dispatcher) = started();
        io.flush(&mut core, &mut dispatcher);
        assert!(write_buffers(&dispatcher).is_empty());
    }
}
