//! ICCCM, EWMH, selection, and Xdnd atoms.
//!
//! [`NAMES`] is interned in one batch. Predefined atoms are copied from the
//! generated [`Atom`] constants. [`AtomCache`] looks a name up in either
//! direction and can intern further names later.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt::{self, Display, Formatter},
};

use crate::runner::{AnyAction, Redispatcher};

use super::{
    generated::{Atom, InternAtom, InternAtomReply},
    ConnectionError, XConnection, XReplyError,
};

macro_rules! atoms {
    (
        intern { $($intern:ident),* $(,)? }
        predefined { $($pre:ident = $value:expr),* $(,)? }
    ) => {
        #[allow(non_snake_case)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) struct Atoms {
            $(pub(crate) $intern: Atom,)*
            $(pub(crate) $pre: Atom,)*
        }

        /// Names sent to `InternAtom`, in field order.
        pub(crate) const NAMES: &[&str] = &[$(stringify!($intern)),*];

        /// Names filled from the predefined [`Atom`] constants.
        pub(crate) const PREDEFINED: &[&str] = &[$(stringify!($pre)),*];

        impl Atoms {
            pub(crate) fn from_interned(values: &[Atom]) -> Self {
                let mut values = values.iter().copied();
                let atoms = Self {
                    $($intern: values.next().expect("one interned atom per name"),)*
                    $($pre: $value,)*
                };
                debug_assert!(values.next().is_none());
                atoms
            }

            pub(crate) fn each(&self, mut visit: impl FnMut(&'static str, Atom)) {
                $(visit(stringify!($intern), self.$intern);)*
                $(visit(stringify!($pre), self.$pre);)*
            }
        }
    };
}

atoms! {
    intern {
        CLIPBOARD,
        CLIPBOARD_MANAGER,
        COMPOUND_TEXT,
        DELETE,
        INCR,
        _MOTIF_WM_HINTS,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST,
        _NET_CLIENT_LIST_STACKING,
        _NET_STARTUP_ID,
        _NET_STARTUP_INFO,
        _NET_STARTUP_INFO_BEGIN,
        _NET_SUPPORTED,
        _NET_SUPPORTING_WM_CHECK,
        _NET_WM_CM_S0,
        _NET_WM_MOVERESIZE,
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_WM_PING,
        _NET_WM_STATE,
        _NET_WM_STATE_FOCUSED,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_HIDDEN,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MODAL,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_COMBO,
        _NET_WM_WINDOW_TYPE_DIALOG,
        _NET_WM_WINDOW_TYPE_DND,
        _NET_WM_WINDOW_TYPE_DROPDOWN_MENU,
        _NET_WM_WINDOW_TYPE_MENU,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _NET_WM_WINDOW_TYPE_NOTIFICATION,
        _NET_WM_WINDOW_TYPE_POPUP_MENU,
        _NET_WM_WINDOW_TYPE_SPLASH,
        _NET_WM_WINDOW_TYPE_TOOLBAR,
        _NET_WM_WINDOW_TYPE_TOOLTIP,
        _NET_WM_WINDOW_TYPE_UTILITY,
        TARGETS,
        TEXT,
        TIMESTAMP,
        UTF8_STRING,
        _WL_SELECTION,
        WL_SURFACE_ID,
        WL_SURFACE_SERIAL,
        WM_CHANGE_STATE,
        WM_DELETE_WINDOW,
        WM_PROTOCOLS,
        WM_S0,
        WM_STATE,
        WM_TAKE_FOCUS,
        WM_WINDOW_ROLE,
        XdndActionAsk,
        XdndActionCopy,
        XdndActionMove,
        XdndActionPrivate,
        XdndAware,
        XdndDrop,
        XdndEnter,
        XdndFinished,
        XdndLeave,
        XdndPosition,
        XdndProxy,
        XdndSelection,
        XdndStatus,
        XdndTypeList,
    }
    predefined {
        PRIMARY = Atom::PRIMARY,
        WINDOW = Atom::WINDOW,
        WM_HINTS = Atom::WM_HINTS,
        WM_NORMAL_HINTS = Atom::WM_NORMAL_HINTS,
        WM_SIZE_HINTS = Atom::WM_SIZE_HINTS,
        WM_NAME = Atom::WM_NAME,
        WM_CLASS = Atom::WM_CLASS,
        WM_TRANSIENT_FOR = Atom::WM_TRANSIENT_FOR,
        STRING = Atom::STRING,
        CARDINAL = Atom::CARDINAL,
        ATOM = Atom::ATOM,
    }
}

/// Why an intern batch stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AtomError {
    /// One `InternAtom` in the batch failed. The batch produces no [`Atoms`].
    Reply { name: &'static str, error: XReplyError },
    /// An intern reply did not belong to this batch.
    OutOfOrder,
    /// `send` failed.
    Connection(ConnectionError),
}

/// One `InternAtom` reply, tagged with its place in [`NAMES`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AtomBatchEvent {
    pub(crate) index: usize,
    pub(crate) result: Result<InternAtomReply, XReplyError>,
}

/// The in-flight `InternAtom` batch. Replies are stored by index.
pub(crate) struct AtomBatch {
    left: usize,
    values: Vec<Option<Atom>>,
    failed: Option<AtomError>,
}

impl AtomBatch {
    /// Sends every [`NAMES`] entry back to back. `only_if_exists` is 0.
    pub(crate) fn send(
        connection: &mut XConnection,
        redispatch: Redispatcher<AtomBatchEvent>,
    ) -> Result<Self, ConnectionError> {
        for (index, name) in NAMES.iter().copied().enumerate() {
            let redispatch = redispatch.clone();
            let slot = Redispatcher::to_any(move |result| {
                redispatch
                    .clone()
                    .make(AtomBatchEvent { index, result })
                    .unwrap_or_else(AnyAction::empty)
            });
            connection.send(&InternAtom { only_if_exists: 0, name: name.as_bytes() }, slot)?;
        }
        Ok(Self {
            left: NAMES.len(),
            values: vec![None; NAMES.len()],
            failed: None,
        })
    }

    /// Stores one reply. `Ok(None)` means the batch is still short one or more atoms.
    pub(crate) fn handle(&mut self, event: AtomBatchEvent) -> Result<Option<Atoms>, AtomError> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if event.index >= self.values.len() {
            return Err(AtomError::OutOfOrder);
        }
        if self.values[event.index].is_some() {
            return Ok(None);
        }
        let atom = match event.result {
            Ok(reply) => reply.atom,
            Err(error) => {
                let error = AtomError::Reply { name: NAMES[event.index], error };
                self.failed = Some(error.clone());
                return Err(error);
            }
        };
        self.values[event.index] = Some(atom);
        self.left -= 1;
        if self.left == 0 {
            let values = self.values.iter().copied().flatten().collect::<Vec<_>>();
            Ok(Some(Atoms::from_interned(&values)))
        } else {
            Ok(None)
        }
    }
}

/// A name-to-atom map and the reverse. Seeded from [`Atoms`], then extended.
#[derive(Clone, Debug, Default)]
pub(crate) struct AtomCache {
    to_atom: BTreeMap<Vec<u8>, Atom>,
    to_name: BTreeMap<u32, Vec<u8>>,
}

/// A later `InternAtom` reply. `name` is owned because dynamic names are not all `'static`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AtomInternEvent {
    pub(crate) name: Vec<u8>,
    pub(crate) result: Result<InternAtomReply, XReplyError>,
}

impl AtomCache {
    pub(crate) fn seed(&mut self, atoms: &Atoms) {
        atoms.each(|name, atom| self.insert(name.as_bytes(), atom));
    }

    pub(crate) fn insert(&mut self, name: &[u8], atom: Atom) {
        self.to_atom.insert(name.to_vec(), atom);
        self.to_name.insert(atom.0, name.to_vec());
    }

    pub(crate) fn atom(&self, name: &[u8]) -> Option<Atom> {
        self.to_atom.get(name).copied()
    }

    pub(crate) fn name(&self, atom: Atom) -> Option<&[u8]> {
        self.to_name.get(&atom.0).map(Vec::as_slice)
    }

    /// Interns names that are not cached yet. Returns how many requests were sent.
    pub(crate) fn intern_more(
        &self,
        names: &[&[u8]],
        connection: &mut XConnection,
        redispatch: Redispatcher<AtomInternEvent>,
    ) -> Result<usize, ConnectionError> {
        let mut sent = 0;
        for name in names {
            if self.atom(name).is_some() {
                continue;
            }
            let owned = name.to_vec();
            let redispatch = redispatch.clone();
            let slot = Redispatcher::to_any(move |result| {
                redispatch
                    .clone()
                    .make(AtomInternEvent { name: owned.clone(), result })
                    .unwrap_or_else(AnyAction::empty)
            });
            connection.send(&InternAtom { only_if_exists: 0, name }, slot)?;
            sent += 1;
        }
        Ok(sent)
    }

    /// Stores a successful intern. An error leaves the cache unchanged.
    pub(crate) fn complete(&mut self, event: AtomInternEvent) -> Result<Atom, XReplyError> {
        match event.result {
            Ok(reply) => {
                self.insert(&event.name, reply.atom);
                Ok(reply.atom)
            }
            Err(error) => Err(error),
        }
    }
}

impl Display for AtomError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reply { name, error } => {
                write!(formatter, "X atom {name} intern failed: {error}")
            }
            Self::OutOfOrder => {
                formatter.write_str("X atom reply does not match this intern batch")
            }
            Self::Connection(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for AtomError {}

#[cfg(test)]
mod tests {
    use {
        super::{AtomBatch, AtomBatchEvent, AtomCache, AtomError, Atoms, NAMES, PREDEFINED},
        crate::{
            runner::{AnyAction, Dispatcher, Redispatcher},
            x11::{
                generated::{
                    Atom, BackingStore, Colormap, EventMask, ImageOrder, InternAtomReply, Screen,
                    Setup, Window, XError,
                },
                wire::Writer,
                State, XConnection, XReplyError,
            },
        },
    };

    const JAY: &[&str] = &[
        "CLIPBOARD",
        "CLIPBOARD_MANAGER",
        "COMPOUND_TEXT",
        "DELETE",
        "INCR",
        "_MOTIF_WM_HINTS",
        "_NET_ACTIVE_WINDOW",
        "_NET_CLIENT_LIST",
        "_NET_CLIENT_LIST_STACKING",
        "_NET_STARTUP_ID",
        "_NET_STARTUP_INFO",
        "_NET_STARTUP_INFO_BEGIN",
        "_NET_SUPPORTED",
        "_NET_SUPPORTING_WM_CHECK",
        "_NET_WM_CM_S0",
        "_NET_WM_MOVERESIZE",
        "_NET_WM_NAME",
        "_NET_WM_PID",
        "_NET_WM_PING",
        "_NET_WM_STATE",
        "_NET_WM_STATE_FOCUSED",
        "_NET_WM_STATE_FULLSCREEN",
        "_NET_WM_STATE_HIDDEN",
        "_NET_WM_STATE_MAXIMIZED_HORZ",
        "_NET_WM_STATE_MAXIMIZED_VERT",
        "_NET_WM_STATE_MODAL",
        "_NET_WM_WINDOW_TYPE",
        "_NET_WM_WINDOW_TYPE_COMBO",
        "_NET_WM_WINDOW_TYPE_DIALOG",
        "_NET_WM_WINDOW_TYPE_DND",
        "_NET_WM_WINDOW_TYPE_DROPDOWN_MENU",
        "_NET_WM_WINDOW_TYPE_MENU",
        "_NET_WM_WINDOW_TYPE_NORMAL",
        "_NET_WM_WINDOW_TYPE_NOTIFICATION",
        "_NET_WM_WINDOW_TYPE_POPUP_MENU",
        "_NET_WM_WINDOW_TYPE_SPLASH",
        "_NET_WM_WINDOW_TYPE_TOOLBAR",
        "_NET_WM_WINDOW_TYPE_TOOLTIP",
        "_NET_WM_WINDOW_TYPE_UTILITY",
        "PRIMARY",
        "TARGETS",
        "TEXT",
        "TIMESTAMP",
        "UTF8_STRING",
        "WINDOW",
        "_WL_SELECTION",
        "WL_SURFACE_ID",
        "WL_SURFACE_SERIAL",
        "WM_CHANGE_STATE",
        "WM_DELETE_WINDOW",
        "WM_HINTS",
        "WM_NORMAL_HINTS",
        "WM_PROTOCOLS",
        "WM_S0",
        "WM_SIZE_HINTS",
        "WM_STATE",
        "WM_TAKE_FOCUS",
        "WM_WINDOW_ROLE",
        "XdndActionAsk",
        "XdndActionCopy",
        "XdndActionMove",
        "XdndActionPrivate",
        "XdndAware",
        "XdndDrop",
        "XdndEnter",
        "XdndFinished",
        "XdndLeave",
        "XdndPosition",
        "XdndProxy",
        "XdndSelection",
        "XdndStatus",
        "XdndTypeList",
    ];

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

    fn query_name(request: &[u8]) -> &[u8] {
        let len = u16::from_ne_bytes(request[4..6].try_into().unwrap()) as usize;
        &request[8..8 + len]
    }

    fn reply(index: usize) -> InternAtomReply {
        InternAtomReply { sequence: 0, atom: Atom(1_000 + index as u32) }
    }

    #[test]
    fn names_cover_jay_and_the_milestone_constants() {
        for name in JAY {
            assert!(
                NAMES.contains(name) || PREDEFINED.contains(name),
                "{name} is neither interned nor predefined"
            );
        }
        for name in ["WM_NAME", "WM_CLASS", "WM_TRANSIENT_FOR", "STRING", "CARDINAL", "ATOM"] {
            assert!(PREDEFINED.contains(&name), "{name}");
            assert!(!NAMES.contains(&name), "{name} would be interned");
        }
        assert_eq!(NAMES.len() + PREDEFINED.len(), 67 + 11);
        assert!(NAMES.contains(&"XdndSelection"));
        assert!(NAMES.contains(&"_NET_WM_NAME"));
    }

    #[test]
    fn the_batch_keeps_its_values_when_replies_arrive_in_reverse() {
        let mut connection = ready();
        let mut batch =
            AtomBatch::send(&mut connection, Redispatcher::to_any(AnyAction::from)).unwrap();
        let outgoing = connection.take_outgoing();
        let sent = requests(&outgoing);
        assert_eq!(sent.len(), NAMES.len());
        assert_eq!(sent[0][1], 0);
        assert_eq!(query_name(sent[0]), b"CLIPBOARD");
        assert_eq!(query_name(sent[sent.len() - 1]), b"XdndTypeList");
        for (index, name) in NAMES.iter().enumerate() {
            assert_eq!(query_name(sent[index]), name.as_bytes());
        }

        let mut done = None;
        for index in (0..NAMES.len()).rev() {
            done = batch
                .handle(AtomBatchEvent { index, result: Ok(reply(index)) })
                .unwrap();
        }
        let atoms = done.expect("the last reply completes the batch");
        assert_eq!(atoms.CLIPBOARD, Atom(1_000));
        assert_eq!(atoms.XdndTypeList, Atom(1_000 + (NAMES.len() - 1) as u32));
        assert_eq!(atoms.PRIMARY, Atom::PRIMARY);
        assert_eq!(atoms.WM_NAME, Atom::WM_NAME);
        assert_eq!(atoms.STRING, Atom::STRING);
    }

    #[test]
    fn one_error_fails_the_batch() {
        let mut connection = ready();
        let mut batch =
            AtomBatch::send(&mut connection, Redispatcher::to_any(AnyAction::from)).unwrap();
        let error = XReplyError::Protocol(XError {
            code: 5,
            sequence: 2,
            bad_value: 0,
            minor_opcode: 0,
            major_opcode: 16,
        });
        let failed = batch.handle(AtomBatchEvent { index: 4, result: Err(error.clone()) });
        assert_eq!(failed, Err(AtomError::Reply { name: "INCR", error: error.clone() }));
        let later = batch.handle(AtomBatchEvent { index: 0, result: Ok(reply(0)) });
        assert_eq!(later, Err(AtomError::Reply { name: "INCR", error }));
    }

    #[test]
    fn the_cache_looks_up_both_ways_and_interns_the_rest() {
        let values: Vec<Atom> = (0..NAMES.len())
            .map(|index| Atom(1_000 + index as u32))
            .collect();
        let atoms = Atoms::from_interned(&values);
        let mut cache = AtomCache::default();
        cache.seed(&atoms);
        assert_eq!(cache.atom(b"CLIPBOARD"), Some(atoms.CLIPBOARD));
        assert_eq!(cache.name(atoms.CLIPBOARD), Some(b"CLIPBOARD".as_slice()));
        assert_eq!(cache.atom(b"WM_NAME"), Some(Atom::WM_NAME));
        assert_eq!(cache.name(Atom::WM_NAME), Some(b"WM_NAME".as_slice()));
        assert_eq!(cache.atom(b"XdndSelection"), Some(atoms.XdndSelection));
        assert_eq!(cache.atom(b"missing"), None);
        assert_eq!(cache.name(Atom(9)), None);

        let mut connection = ready();
        let mut dispatcher = Dispatcher::default();
        let sent = cache
            .intern_more(
                &[b"CLIPBOARD", b"text/plain"],
                &mut connection,
                Redispatcher::to_any(AnyAction::from),
            )
            .unwrap();
        assert_eq!(sent, 1);
        let outgoing = connection.take_outgoing();
        let sent = requests(&outgoing);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0][1], 0);
        assert_eq!(query_name(sent[0]), b"text/plain");

        let mut bytes = Vec::new();
        InternAtomReply { sequence: 1, atom: Atom(77) }
            .serialize(&mut Writer::new(&mut bytes))
            .unwrap();
        let read = connection.on_read(&bytes, &mut dispatcher);
        assert!(read.dead.is_none());
        let mut events = Vec::new();
        dispatcher.test_for_each_queued_action(|event: &super::AtomInternEvent| {
            events.push(event.clone())
        });
        assert_eq!(events.len(), 1);
        assert_eq!(cache.complete(events.remove(0)).unwrap(), Atom(77));
        assert_eq!(cache.atom(b"text/plain"), Some(Atom(77)));
        assert_eq!(cache.name(Atom(77)), Some(b"text/plain".as_slice()));

        let refused = cache.complete(super::AtomInternEvent {
            name: b"text/html".to_vec(),
            result: Err(XReplyError::Dead),
        });
        assert_eq!(refused, Err(XReplyError::Dead));
        assert_eq!(cache.atom(b"text/html"), None);
    }
}
