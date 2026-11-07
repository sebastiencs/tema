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
#[cfg(test)]
pub(crate) mod testing;
mod wire;

// Later phases send and parse through these types.
#[allow(unused_imports)]
pub(crate) use atoms::{AtomCache, Atoms, NAMES};
#[allow(unused_imports)]
pub(crate) use connection::{ConnectionError, Read, State, XConnection, XReplyError};
#[allow(unused_imports)]
pub(crate) use generated::{
    Atom, ChangeWindowAttributes, ChangeWindowAttributesAux, ClientIdMask, ClientIdSpec,
    ClientIdValue, ClientMessage, ClientMessageData, Colormap, CompositeRedirectSubwindows,
    ConfigWindow, ConfigureNotify, ConfigureRequest, ConfigureWindow, ConfigureWindowAux, CreateGc,
    CreateGcAux, CreateNotify, CreatePixmap, CreateWindow, CreateWindowAux, Cursor, DestroyNotify,
    Directformat, Drawable, Event, EventMask, FocusIn, FreeGc, FreePixmap, Gcontext, GetProperty,
    ImageFormat, InputFocus, KillClient, MapNotify, MapRequest, MapWindow, NoReply, NotifyDetail,
    NotifyMode, PictType, Pictformat, Pictforminfo, Picture, Pixmap, Property as PropertyState,
    PropertyNotify, PutImage, Redirect, RenderCreateCursor, RenderCreatePicture,
    RenderCreatePictureAux, RenderFreePicture, RenderQueryPictFormats, RenderQueryPictFormatsReply,
    Request, ResQueryClientIds, ResQueryClientIdsReply, SendEvent, SetInputFocus,
    SetSelectionOwner, StackMode, UnmapNotify, Window, WindowClass,
};
#[allow(unused_imports)]
pub(crate) use io::XIo;
#[allow(unused_imports)]
pub(crate) use property::{
    change_property_atoms, change_property_u32s, change_property_u8s, change_property_utf8,
    delete_property, get_property, Property, PropertyChange, PropertyError,
};
#[allow(unused_imports)]
pub(crate) use startup::{
    Bringup, ExtensionInfo, Extensions, Startup, StartupError, StartupEvent, Status,
};
#[allow(unused_imports)]
pub(crate) use wire::{event_bytes, Reader, WireError, Writer};
