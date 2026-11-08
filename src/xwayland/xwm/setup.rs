//! The WM role, taken once startup is ready.
//!
//! The requests are void. A protocol error is logged and the connection stays up.

use crate::x11::{
    change_property_atoms, change_property_u32s, change_property_utf8, Atom,
    ChangeWindowAttributes, ChangeWindowAttributesAux, CompositeRedirectSubwindows,
    ConnectionError, CreateWindow, CreateWindowAux, EventMask, Redirect, SelectionEventMask,
    SetSelectionOwner, Window, WindowClass, XConnection, XFixesSelectSelectionInput,
};

use super::{
    selection::{SelectionKind, SelectionSide, Selections},
    Ready,
};

const SELECTION_EVENTS: SelectionEventMask = SelectionEventMask::SetSelectionOwner
    .union(SelectionEventMask::SelectionWindowDestroy)
    .union(SelectionEventMask::SelectionClientClose);

const ROOT_EVENTS: EventMask = EventMask::SubstructureNotify
    .union(EventMask::SubstructureRedirect)
    .union(EventMask::PropertyChange);

/// Claims the root and returns the check window.
pub(super) fn claim(
    connection: &mut XConnection,
    ready: &mut Ready,
) -> Result<Window, ConnectionError> {
    let root = ready.root;
    let atoms = ready.atoms;
    connection.send_void(&ChangeWindowAttributes {
        window: root,
        value_list: ChangeWindowAttributesAux::default().event_mask(ROOT_EVENTS),
    })?;
    connection
        .send_void(&CompositeRedirectSubwindows { window: root, update: Redirect::Manual })?;
    let check = Window(connection.generate_id()?);
    connection.send_void(&CreateWindow {
        depth: 0,
        wid: check,
        parent: root,
        x: 0,
        y: 0,
        width: 10,
        height: 10,
        border_width: 0,
        class: WindowClass::InputOutput,
        visual: 0,
        value_list: CreateWindowAux::default(),
    })?;
    let name = change_property_utf8(check, atoms._NET_WM_NAME, atoms.UTF8_STRING, "tema");
    connection.send_void(&name.request())?;
    let supporting = check.0;
    let on_root =
        change_property_u32s(root, atoms._NET_SUPPORTING_WM_CHECK, atoms.WINDOW, &[supporting]);
    connection.send_void(&on_root.request())?;
    let on_check =
        change_property_u32s(check, atoms._NET_SUPPORTING_WM_CHECK, atoms.WINDOW, &[supporting]);
    connection.send_void(&on_check.request())?;
    connection.send_void(&SetSelectionOwner { owner: check, selection: atoms.WM_S0, time: 0 })?;
    connection.send_void(&SetSelectionOwner {
        owner: check,
        selection: atoms._NET_WM_CM_S0,
        time: 0,
    })?;
    let clipboard = selection_window(connection, root, atoms.CLIPBOARD)?;
    let primary = selection_window(connection, root, atoms.PRIMARY)?;
    ready.selections = Selections {
        clipboard: SelectionSide::new(SelectionKind::Clipboard, atoms.CLIPBOARD, clipboard),
        primary: SelectionSide::new(SelectionKind::Primary, atoms.PRIMARY, primary),
    };
    let supported =
        change_property_atoms(root, atoms._NET_SUPPORTED, atoms.ATOM, &supported_atoms(&atoms));
    connection.send_void(&supported.request())?;
    let active = change_property_u32s(root, atoms._NET_ACTIVE_WINDOW, atoms.WINDOW, &[0]);
    connection.send_void(&active.request())?;
    ready.check_window = check;
    Ok(check)
}

fn selection_window(
    connection: &mut XConnection,
    parent: Window,
    selection: Atom,
) -> Result<Window, ConnectionError> {
    let window = Window(connection.generate_id()?);
    connection.send_void(&CreateWindow {
        depth: 0,
        wid: window,
        parent,
        x: 0,
        y: 0,
        width: 10,
        height: 10,
        border_width: 0,
        class: WindowClass::InputOnly,
        visual: 0,
        value_list: CreateWindowAux::default().event_mask(EventMask::PropertyChange),
    })?;
    connection.send_void(&XFixesSelectSelectionInput {
        window,
        selection,
        event_mask: SELECTION_EVENTS,
    })?;
    Ok(window)
}

fn supported_atoms(atoms: &crate::x11::Atoms) -> [Atom; 11] {
    [
        atoms._NET_WM_STATE,
        atoms._NET_ACTIVE_WINDOW,
        atoms._NET_WM_MOVERESIZE,
        atoms._NET_WM_STATE_FOCUSED,
        atoms._NET_WM_STATE_MODAL,
        atoms._NET_WM_STATE_FULLSCREEN,
        atoms._NET_WM_STATE_MAXIMIZED_VERT,
        atoms._NET_WM_STATE_MAXIMIZED_HORZ,
        atoms._NET_WM_STATE_HIDDEN,
        atoms._NET_CLIENT_LIST,
        atoms._NET_CLIENT_LIST_STACKING,
    ]
}
