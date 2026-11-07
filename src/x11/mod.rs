//! X11 protocol types and the connection to Xwayland.
//!
//! The wire module speaks the machine's native byte order, which is the order
//! an X client announces in its connection setup.

mod atoms;
mod connection;
#[cfg(test)]
mod gen;
mod generated;
mod io;
#[cfg(test)]
mod live;
mod property;
mod startup;
mod wire;

// Later phases send and parse through these types.
#[allow(unused_imports)]
pub(crate) use atoms::{AtomCache, Atoms};
#[allow(unused_imports)]
pub(crate) use connection::{ConnectionError, Read, State, XConnection, XReplyError};
#[allow(unused_imports)]
pub(crate) use io::XIo;
#[allow(unused_imports)]
pub(crate) use property::{Property, PropertyError};
#[allow(unused_imports)]
pub(crate) use startup::{
    Bringup, ExtensionInfo, Extensions, Startup, StartupError, StartupEvent, Status,
};
#[allow(unused_imports)]
pub(crate) use wire::{Reader, WireError, Writer};
