use std::{
    fs::File,
    io::ErrorKind::UnexpectedEof,
    os::fd::{AsRawFd, OwnedFd, RawFd},
    rc::Rc,
    time::{Duration, Instant},
};

use crate::{
    capture,
    clients::{Client, ClientAction, ClientId, DataSourceClientEvent, Resolved},
    io_uring::{
        model::IoUringAction,
        ring::{ReadWriteCompleted, RingId},
    },
    protocol_generated::{WlDataDeviceId, WlDataSourceId, WlSeatId, WlSurfaceId},
    runner::{Model, Redispatcher},
    seat::{set_keyboard_focus_from_surface, SurfaceFocus},
    state::SelectionSourceState,
    wayland_objects::{
        wl_data_device::WlDataDevice, wl_data_source::WlDataSource, wl_surface::WlSurface,
    },
    x11::{
        change_property_atoms, change_property_u32s, change_property_u8s, testing, Atom,
        ChangeWindowAttributes, ChangeWindowAttributesAux, ConvertSelection, CreateNotify,
        DestroyNotify, Event, EventMask, GetAtomName, GetProperty, InternAtom, Property,
        PropertyNotify, PropertyState, SelectionClear, SelectionEvent, SelectionNotify,
        SelectionRequest, SendEvent, SetSelectionOwner, Window, XFixesSelectionNotify,
    },
    xwayland::XwaylandAction,
};

use super::{
    focus_tests::{focus_root, focus_surface},
    manage_tests::{add_output, atoms, manage},
    selection::SelectionKind,
    tests::{
        claim_requests, deliver, drive_prepared, expect_requests, handle_event, open_wm,
        take_writes,
    },
    Wm, WmCtx, WmEvent, WmPhase,
};

#[test]
fn selection_cancelled_removes_the_source_object() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let (_, object) = state.insert_object(crate::xwayland::XSelectionSourceObject::new(
        1,
        crate::xwayland::SelectionKind::Clipboard,
        vec!["text/plain".to_string()],
    ));
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionCancelled { source: object },
    );
    assert!(state.objects.try_get(object).is_none());

    let fd = Rc::new(OwnedFd::from(File::open("/dev/null").unwrap()));
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionSend { source: object, mime_type: "text/plain".to_string(), fd },
    );
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionChanged(crate::xwayland::SelectionKind::Primary),
    );
}

#[test]
fn a_create_notify_for_a_selection_window_is_ignored() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let (clipboard, primary) = {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        (ready.selections.clipboard.window, ready.selections.primary.window)
    };
    let before = wm.windows.len();
    for window in [clipboard, primary] {
        let sent = deliver(
            &mut wm,
            &mut state,
            &mut dispatcher,
            &surfaces,
            Event::CreateNotify(CreateNotify {
                send_event: false,
                sequence: 0,
                parent: Window(0x100),
                window,
                x: 0,
                y: 0,
                width: 10,
                height: 10,
                border_width: 0,
                override_redirect: 0,
            }),
        );
        assert!(sent.is_empty());
    }
    assert_eq!(wm.windows.len(), before);
    assert!(!wm.windows.contains_key(&clipboard));
    assert!(!wm.windows.contains_key(&primary));
}

#[test]
fn our_selection_owner_stores_the_timestamp() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let (window, selection) = {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        (ready.selections.clipboard.window, ready.selections.clipboard.atom)
    };
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::XFixesSelectionNotify(XFixesSelectionNotify {
            send_event: false,
            code: 85,
            sequence: 0,
            subtype: SelectionEvent::SetSelectionOwner,
            window,
            owner: window,
            selection,
            timestamp: 5,
            selection_timestamp: 9,
        }),
    );
    assert!(sent.is_empty());
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    assert_eq!(ready.selections.clipboard.own_timestamp, 9);
    assert_eq!(ready.selections.primary.own_timestamp, 0);

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::XFixesSelectionNotify(XFixesSelectionNotify {
            send_event: false,
            code: 85,
            sequence: 0,
            subtype: SelectionEvent::SetSelectionOwner,
            window,
            owner: Window(0x50),
            selection,
            timestamp: 6,
            selection_timestamp: 11,
        }),
    );
    let ready = atoms(&wm);
    expect_requests(
        sent,
        vec![testing::encode(
            &ConvertSelection {
                requestor: window,
                selection,
                target: ready.TARGETS,
                property: ready._WL_SELECTION,
                time: 6,
            },
            0,
        )],
    );
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    assert_eq!(ready.selections.clipboard.own_timestamp, 9);
    assert_eq!(ready.selections.primary.own_timestamp, 0);
}

fn xwm_client() -> ClientId {
    ClientId::from_raw(9)
}

fn focused_x_window(
) -> (Wm, crate::State, crate::runner::Dispatcher, crate::xwayland::surfaces::SurfaceSerials) {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let managed = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "Term",
        "XTerm",
    );
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, managed.surface);
    (wm, state, dispatcher, surfaces)
}

fn handle_client(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut crate::runner::Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    event: WmEvent,
) -> Vec<Vec<u8>> {
    {
        let mut ctx = WmCtx {
            run: 1,
            state,
            dispatcher,
            surfaces,
            client: Some(xwm_client()),
        };
        wm.handle(event, &mut ctx).unwrap();
    }
    take_writes(wm, state, dispatcher, surfaces)
}

fn owner_notify(
    window: Window,
    owner: Window,
    selection: crate::x11::Atom,
    timestamp: u32,
) -> Event {
    Event::XFixesSelectionNotify(XFixesSelectionNotify {
        send_event: false,
        code: 85,
        sequence: 0,
        subtype: SelectionEvent::SetSelectionOwner,
        window,
        owner,
        selection,
        timestamp,
        selection_timestamp: timestamp,
    })
}

fn atom_list(atoms: &[Atom]) -> Property {
    let mut data = Vec::new();
    for atom in atoms {
        data.extend(atom.0.to_ne_bytes());
    }
    Property { r#type: Atom::ATOM, format: 32, data, truncated: false }
}

fn clipboard_window(wm: &Wm) -> (Window, crate::x11::Atom) {
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    (ready.selections.clipboard.window, ready.selections.clipboard.atom)
}

#[test]
fn a_foreign_owner_asks_for_targets() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (window, selection) = clipboard_window(&wm);
    let ready = atoms(&wm);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, Window(0x70), selection, 9),
    );
    expect_requests(
        sent,
        vec![testing::encode(
            &ConvertSelection {
                requestor: window,
                selection,
                target: ready.TARGETS,
                property: ready._WL_SELECTION,
                time: 9,
            },
            0,
        )],
    );
}

#[test]
fn targets_install_a_clipboard_source_while_an_x_window_is_focused() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (window, selection) = clipboard_window(&wm);
    let ready = atoms(&wm);
    let png = Atom(900);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, Window(0x70), selection, 9),
    );
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionNotify(SelectionNotify {
            send_event: false,
            sequence: 0,
            time: 9,
            requestor: window,
            selection,
            target: ready.TARGETS,
            property: ready._WL_SELECTION,
        }),
    );
    assert_eq!(
        sent,
        vec![testing::encode(
            &GetProperty {
                delete: 1,
                window,
                property: ready._WL_SELECTION,
                r#type: Atom::Any,
                long_offset: 0,
                long_length: u32::MAX / 4,
            },
            0,
        )]
    );
    let sent = handle_client(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionTargets {
            kind: SelectionKind::Clipboard,
            result: Ok(atom_list(&[ready.TARGETS, ready.UTF8_STRING, ready.STRING, png])),
        },
    );
    assert_eq!(sent, vec![testing::encode(&GetAtomName { atom: png }, 0)]);
    let sent = handle_client(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::AtomName {
            kind: SelectionKind::Clipboard,
            generation: 2,
            atom: png,
            result: Ok(crate::x11::GetAtomNameReply { sequence: 1, name: b"image/png".to_vec() }),
        },
    );
    assert!(sent.is_empty());
    let source = state.seat.selection_source().expect("clipboard source");
    assert_eq!(
        source.mime_types(&state),
        Some(vec![
            "text/plain;charset=utf-8".to_string(),
            "text/plain".to_string(),
            "image/png".to_string(),
        ])
    );

    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionChanged(SelectionKind::Clipboard),
    );
    assert!(sent.is_empty());
    assert!(state.seat.selection_source().is_some());
}

#[test]
fn targets_leave_the_slot_alone_when_a_wayland_surface_is_focused() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    focus_surface(&mut state, &mut dispatcher, 90);
    let (window, selection) = clipboard_window(&wm);
    let ready = atoms(&wm);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, Window(0x70), selection, 4),
    );
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionNotify(SelectionNotify {
            send_event: false,
            sequence: 0,
            time: 4,
            requestor: window,
            selection,
            target: ready.TARGETS,
            property: ready._WL_SELECTION,
        }),
    );
    handle_client(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionTargets {
            kind: SelectionKind::Clipboard,
            result: Ok(atom_list(&[ready.UTF8_STRING])),
        },
    );
    assert!(state.seat.selection_source().is_none());
}

#[test]
fn an_empty_owner_clears_an_x_source_and_leaves_a_wayland_source() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (window, selection) = clipboard_window(&wm);
    let ready = atoms(&wm);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, Window(0x70), selection, 3),
    );
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionNotify(SelectionNotify {
            send_event: false,
            sequence: 0,
            time: 3,
            requestor: window,
            selection,
            target: ready.TARGETS,
            property: ready._WL_SELECTION,
        }),
    );
    handle_client(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionTargets {
            kind: SelectionKind::Clipboard,
            result: Ok(atom_list(&[ready.UTF8_STRING])),
        },
    );
    let SelectionSourceState::Xwayland(installed) = state.seat.selection_source().unwrap() else {
        panic!("x source");
    };
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, Window::None, selection, 4),
    );
    assert!(sent.is_empty());
    assert!(state.seat.selection_source().is_none());
    assert!(state.objects.try_get(installed.object).is_none());

    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, Window(0x70), selection, 5),
    );
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionNotify(SelectionNotify {
            send_event: false,
            sequence: 0,
            time: 5,
            requestor: window,
            selection,
            target: ready.TARGETS,
            property: ready._WL_SELECTION,
        }),
    );
    handle_client(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionTargets {
            kind: SelectionKind::Clipboard,
            result: Ok(atom_list(&[ready.STRING])),
        },
    );
    let mut app = Client::new_for_tests(ClientId::from_raw(8));
    let source_id = WlDataSourceId::from(70);
    app.insert_object(&mut state, WlDataSource::new(source_id, app.client_id(), 3));
    let resolved = app.resolve(source_id, &state).unwrap();
    let serial = state.seat.next_serial();
    state
        .seat
        .replace_selection_source(Some(SelectionSourceState::WlData(resolved)), serial);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, Window::None, selection, 6),
    );
    assert!(matches!(state.seat.selection_source(), Some(SelectionSourceState::WlData(_))));
}

#[test]
fn primary_targets_install_the_primary_slot() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (window, selection) = {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        (ready.selections.primary.window, ready.selections.primary.atom)
    };
    let ready = atoms(&wm);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, Window(0x70), selection, 2),
    );
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionNotify(SelectionNotify {
            send_event: false,
            sequence: 0,
            time: 2,
            requestor: window,
            selection,
            target: ready.TARGETS,
            property: ready._WL_SELECTION,
        }),
    );
    let sent = handle_client(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionTargets {
            kind: SelectionKind::Primary,
            result: Ok(atom_list(&[ready.UTF8_STRING, ready.STRING])),
        },
    );
    assert!(sent.is_empty());
    let source = state
        .seat
        .primary_selection_source()
        .expect("primary source");
    assert_eq!(
        source.mime_types(&state),
        Some(vec!["text/plain;charset=utf-8".to_string(), "text/plain".to_string()])
    );
    assert!(state.seat.selection_source().is_none());
}

#[test]
fn a_new_owner_discards_outstanding_target_names() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (window, selection) = clipboard_window(&wm);
    let ready = atoms(&wm);
    let png = Atom(900);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, Window(0x70), selection, 1),
    );
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionNotify(SelectionNotify {
            send_event: false,
            sequence: 0,
            time: 1,
            requestor: window,
            selection,
            target: ready.TARGETS,
            property: ready._WL_SELECTION,
        }),
    );
    handle_client(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionTargets {
            kind: SelectionKind::Clipboard,
            result: Ok(atom_list(&[png])),
        },
    );
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, Window(0x71), selection, 2),
    );
    handle_client(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::AtomName {
            kind: SelectionKind::Clipboard,
            generation: 2,
            atom: png,
            result: Ok(crate::x11::GetAtomNameReply { sequence: 1, name: b"image/png".to_vec() }),
        },
    );
    assert!(state.seat.selection_source().is_none());
}

const UTF8_MIME: &str = "text/plain;charset=utf-8";

/// The test X connection's `File` is dropped, so the next open reuses its fd
/// number. The returned file occupies that number. The `Rc` is a later fd.
fn held_fd() -> (Rc<OwnedFd>, RawFd, File) {
    let occupy = File::open("/dev/null").unwrap();
    let fd = Rc::new(OwnedFd::from(File::open("/dev/null").unwrap()));
    let raw = AsRawFd::as_raw_fd(&*fd);
    (fd, raw, occupy)
}

fn offer_clipboard(state: &mut crate::State, mime: &str) -> crate::state::ObjectId {
    let (_, object) = state.insert_object(crate::xwayland::XSelectionSourceObject::new(
        1,
        SelectionKind::Clipboard,
        vec![mime.to_string()],
    ));
    object
}

fn pull_fd_writes(
    dispatcher: &mut crate::runner::Dispatcher,
    fd: RawFd,
) -> Vec<(Box<[u8]>, Redispatcher<std::io::Result<ReadWriteCompleted>>)> {
    let mut data = Vec::new();
    let mut restore = Vec::new();
    for action in dispatcher.test_take_queued_actions::<IoUringAction>() {
        match action {
            IoUringAction::Write { fd: write_fd, buffer, dispatch_to, .. } if write_fd == fd => {
                data.push((buffer, dispatch_to));
            }
            other => restore.push(other),
        }
    }
    for action in restore {
        dispatcher.dispatch(action);
    }
    data
}

fn handle_keeping(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut crate::runner::Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    event: WmEvent,
    data_fd: RawFd,
) -> (Vec<Vec<u8>>, Vec<(Box<[u8]>, Redispatcher<std::io::Result<ReadWriteCompleted>>)>) {
    {
        let mut ctx = WmCtx { run: 1, state, dispatcher, surfaces, client: None };
        wm.handle(event, &mut ctx).unwrap();
    }
    let data = pull_fd_writes(dispatcher, data_fd);
    (take_writes(wm, state, dispatcher, surfaces), data)
}

fn deliver_keeping(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut crate::runner::Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    event: Event,
    data_fd: RawFd,
) -> (Vec<Vec<u8>>, Vec<(Box<[u8]>, Redispatcher<std::io::Result<ReadWriteCompleted>>)>) {
    {
        let mut ctx = WmCtx { run: 1, state, dispatcher, surfaces, client: None };
        wm.on_core_event(event, &mut ctx).unwrap();
    }
    let data = pull_fd_writes(dispatcher, data_fd);
    (take_writes(wm, state, dispatcher, surfaces), data)
}

fn finish_write(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut crate::runner::Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    dispatch_to: Redispatcher<std::io::Result<ReadWriteCompleted>>,
    data_fd: RawFd,
) -> (Vec<Vec<u8>>, Vec<(Box<[u8]>, Redispatcher<std::io::Result<ReadWriteCompleted>>)>) {
    dispatcher.redispatch_to(
        dispatch_to,
        Ok(ReadWriteCompleted { fd: 0, offset: 0, buffer: Box::new([]) }),
    );
    let mut actions = dispatcher.test_take_queued_actions::<XwaylandAction>();
    actions.retain(|action| {
        !matches!(
            action,
            XwaylandAction::KeyboardFocus
                | XwaylandAction::SelectionChanged(_)
                | XwaylandAction::SelectionCancelled { .. }
        )
    });
    assert_eq!(actions.len(), 1);
    let XwaylandAction::Wm { event, .. } = actions.pop().unwrap() else {
        panic!("write completion");
    };
    handle_keeping(wm, state, dispatcher, surfaces, event, data_fd)
}

fn deleting_get(window: Window, property: Atom) -> Vec<u8> {
    testing::encode(
        &GetProperty {
            delete: 1,
            window,
            property,
            r#type: Atom::Any,
            long_offset: 0,
            long_length: u32::MAX / 4,
        },
        0,
    )
}

fn data_notify(window: Window, selection: Atom, target: Atom, property: Atom) -> Event {
    Event::SelectionNotify(SelectionNotify {
        send_event: false,
        sequence: 0,
        time: 0,
        requestor: window,
        selection,
        target,
        property,
    })
}

fn new_value(window: Window, atom: Atom) -> Event {
    Event::PropertyNotify(PropertyNotify {
        send_event: false,
        sequence: 0,
        window,
        atom,
        time: 0,
        state: PropertyState::NewValue,
    })
}

fn bytes_property(ty: Atom, data: &[u8]) -> Property {
    Property { r#type: ty, format: 8, data: data.to_vec(), truncated: false }
}

#[test]
fn utf8_receive_writes_hello_and_drops_the_fd() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let source = offer_clipboard(&mut state, UTF8_MIME);
    let (fd, raw, _occupy) = held_fd();
    let watched = Rc::clone(&fd);
    let (window, selection) = clipboard_window(&wm);
    let ready = atoms(&wm);
    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionSend { source, mime_type: UTF8_MIME.to_string(), fd },
    );
    expect_requests(
        sent,
        vec![testing::encode(
            &ConvertSelection {
                requestor: window,
                selection,
                target: ready.UTF8_STRING,
                property: ready._WL_SELECTION,
                time: 0,
            },
            0,
        )],
    );
    let (sent, writes) = deliver_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        data_notify(window, selection, ready.UTF8_STRING, ready._WL_SELECTION),
        raw,
    );
    assert!(writes.is_empty());
    assert_eq!(sent, vec![deleting_get(window, ready._WL_SELECTION)]);
    let (sent, writes) = handle_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::IncomingProperty {
            kind: SelectionKind::Clipboard,
            result: Ok(bytes_property(ready.UTF8_STRING, b"hello")),
        },
        raw,
    );
    assert!(sent.is_empty());
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0.as_ref(), b"hello");
    assert_eq!(Rc::strong_count(&watched), 2);
    let dispatch_to = writes.into_iter().next().unwrap().1;
    let (sent, writes) =
        finish_write(&mut wm, &mut state, &mut dispatcher, &surfaces, dispatch_to, raw);
    assert!(sent.is_empty());
    assert!(writes.is_empty());
    assert_eq!(Rc::strong_count(&watched), 1);
}

#[test]
fn a_refused_conversion_drops_the_fd() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let source = offer_clipboard(&mut state, UTF8_MIME);
    let (fd, raw, _occupy) = held_fd();
    let watched = Rc::clone(&fd);
    let (window, selection) = clipboard_window(&wm);
    let ready = atoms(&wm);
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionSend { source, mime_type: UTF8_MIME.to_string(), fd },
    );
    let (sent, writes) = deliver_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        data_notify(window, selection, ready.UTF8_STRING, Atom(0)),
        raw,
    );
    assert!(sent.is_empty());
    assert!(writes.is_empty());
    assert_eq!(Rc::strong_count(&watched), 1);
}

#[test]
fn incr_writes_one_chunk_then_ends() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let source = offer_clipboard(&mut state, UTF8_MIME);
    let (fd, raw, _occupy) = held_fd();
    let watched = Rc::clone(&fd);
    let (window, selection) = clipboard_window(&wm);
    let ready = atoms(&wm);
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionSend { source, mime_type: UTF8_MIME.to_string(), fd },
    );
    let (sent, writes) = deliver_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        data_notify(window, selection, ready.UTF8_STRING, ready._WL_SELECTION),
        raw,
    );
    assert!(writes.is_empty());
    assert_eq!(sent, vec![deleting_get(window, ready._WL_SELECTION)]);
    let (sent, writes) = handle_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::IncomingProperty {
            kind: SelectionKind::Clipboard,
            result: Ok(Property {
                r#type: ready.INCR,
                format: 32,
                data: 0u32.to_ne_bytes().to_vec(),
                truncated: false,
            }),
        },
        raw,
    );
    assert!(sent.is_empty());
    assert!(writes.is_empty());
    let (sent, writes) = deliver_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        new_value(window, ready._WL_SELECTION),
        raw,
    );
    assert!(writes.is_empty());
    assert_eq!(sent, vec![deleting_get(window, ready._WL_SELECTION)]);
    let (sent, writes) = handle_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::IncomingProperty {
            kind: SelectionKind::Clipboard,
            result: Ok(bytes_property(ready.UTF8_STRING, b"abc")),
        },
        raw,
    );
    assert!(sent.is_empty());
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0.as_ref(), b"abc");
    let dispatch_to = writes.into_iter().next().unwrap().1;
    let (sent, writes) = deliver_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        new_value(window, ready._WL_SELECTION),
        raw,
    );
    assert!(sent.is_empty());
    assert!(writes.is_empty());
    let (sent, writes) =
        finish_write(&mut wm, &mut state, &mut dispatcher, &surfaces, dispatch_to, raw);
    assert!(writes.is_empty());
    assert_eq!(sent, vec![deleting_get(window, ready._WL_SELECTION)]);
    let (sent, writes) = handle_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::IncomingProperty {
            kind: SelectionKind::Clipboard,
            result: Ok(Property {
                r#type: Atom(0),
                format: 0,
                data: Vec::new(),
                truncated: false,
            }),
        },
        raw,
    );
    assert!(sent.is_empty());
    assert!(writes.is_empty());
    assert_eq!(Rc::strong_count(&watched), 1);
}

#[test]
fn a_second_receive_waits_for_the_first() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let source = offer_clipboard(&mut state, UTF8_MIME);
    let (first, first_raw, _first_occupy) = held_fd();
    let first_watched = Rc::clone(&first);
    let (second, _second_raw, _second_occupy) = held_fd();
    let second_watched = Rc::clone(&second);
    let (window, selection) = clipboard_window(&wm);
    let ready = atoms(&wm);
    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionSend { source, mime_type: UTF8_MIME.to_string(), fd: first },
    );
    expect_requests(
        sent,
        vec![testing::encode(
            &ConvertSelection {
                requestor: window,
                selection,
                target: ready.UTF8_STRING,
                property: ready._WL_SELECTION,
                time: 0,
            },
            0,
        )],
    );
    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionSend { source, mime_type: UTF8_MIME.to_string(), fd: second },
    );
    assert!(sent.is_empty());
    assert_eq!(Rc::strong_count(&second_watched), 2);
    deliver_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        data_notify(window, selection, ready.UTF8_STRING, ready._WL_SELECTION),
        first_raw,
    );
    let (_, writes) = handle_keeping(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::IncomingProperty {
            kind: SelectionKind::Clipboard,
            result: Ok(bytes_property(ready.UTF8_STRING, b"hello")),
        },
        first_raw,
    );
    let dispatch_to = writes.into_iter().next().unwrap().1;
    let (sent, writes) =
        finish_write(&mut wm, &mut state, &mut dispatcher, &surfaces, dispatch_to, first_raw);
    assert!(writes.is_empty());
    expect_requests(
        sent,
        vec![testing::encode(
            &ConvertSelection {
                requestor: window,
                selection,
                target: ready.UTF8_STRING,
                property: ready._WL_SELECTION,
                time: 0,
            },
            0,
        )],
    );
    assert_eq!(Rc::strong_count(&first_watched), 1);
    assert_eq!(Rc::strong_count(&second_watched), 2);
}

#[test]
fn an_idle_transfer_expires_and_starts_the_next() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let source = offer_clipboard(&mut state, UTF8_MIME);
    let (first, _first_raw, _first_occupy) = held_fd();
    let first_watched = Rc::clone(&first);
    let (second, _second_raw, _second_occupy) = held_fd();
    let second_watched = Rc::clone(&second);
    let (window, selection) = clipboard_window(&wm);
    let ready = atoms(&wm);
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionSend { source, mime_type: UTF8_MIME.to_string(), fd: first },
    );
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionSend { source, mime_type: UTF8_MIME.to_string(), fd: second },
    );
    {
        let mut ctx = WmCtx {
            run: 1,
            state: &mut state,
            dispatcher: &mut dispatcher,
            surfaces: &surfaces,
            client: None,
        };
        wm.test_expire_transfers(Instant::now() + Duration::from_secs(6), &mut ctx);
    }
    let sent = take_writes(&mut wm, &mut state, &mut dispatcher, &surfaces);
    expect_requests(
        sent,
        vec![testing::encode(
            &ConvertSelection {
                requestor: window,
                selection,
                target: ready.UTF8_STRING,
                property: ready._WL_SELECTION,
                time: 0,
            },
            0,
        )],
    );
    assert_eq!(Rc::strong_count(&first_watched), 1);
    assert_eq!(Rc::strong_count(&second_watched), 2);
}

fn data_source(state: &mut crate::State, mime: &str) -> crate::state::SelectionSourceState {
    offered_source(state, &[mime])
}

fn offered_source(state: &mut crate::State, mimes: &[&str]) -> crate::state::SelectionSourceState {
    let mut app = Client::new_for_tests(ClientId::from_raw(8));
    let source_id = WlDataSourceId::from(70);
    let (resolved, source) =
        app.insert_object(state, WlDataSource::new(source_id, app.client_id(), 3));
    for mime in mimes {
        source.offer((*mime).to_string());
    }
    SelectionSourceState::WlData(resolved)
}

fn owner_request(owner: Window, selection: Atom) -> Vec<u8> {
    testing::encode(&SetSelectionOwner { owner, selection, time: 0 }, 0)
}

#[test]
fn a_wayland_source_owns_the_clipboard_and_interns_its_mime_type() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let (window, selection) = clipboard_window(&wm);
    let source = data_source(&mut state, "image/png");
    state.seat.replace_selection_source(Some(source), 1);
    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionChanged(SelectionKind::Clipboard),
    );
    expect_requests(
        sent,
        vec![
            testing::encode(&InternAtom { only_if_exists: 0, name: b"image/png" }, 0),
            owner_request(window, selection),
        ],
    );
    {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        assert!(ready.selections.clipboard.owned);
        let offer = ready.selections.clipboard.offer.as_ref().unwrap();
        assert_eq!(offer.interns_outstanding, 1);
        assert_eq!(offer.targets, vec![(Atom(0), "image/png".to_string())]);
    }
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::Interned {
            name: b"image/png".to_vec(),
            result: Ok(crate::x11::InternAtomReply { sequence: 1, atom: Atom(900) }),
        },
    );
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    let offer = ready.selections.clipboard.offer.as_ref().unwrap();
    assert_eq!(offer.interns_outstanding, 0);
    assert_eq!(offer.targets, vec![(Atom(900), "image/png".to_string())]);
    assert_eq!(ready.atom_cache.atom(b"image/png"), Some(Atom(900)));
}

#[test]
fn clearing_the_selection_releases_x_ownership() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let (_window, selection) = clipboard_window(&wm);
    let source = data_source(&mut state, "text/plain");
    state.seat.replace_selection_source(Some(source), 1);
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionChanged(SelectionKind::Clipboard),
    );
    state.seat.replace_selection_source(None, 2);
    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionChanged(SelectionKind::Clipboard),
    );
    expect_requests(sent, vec![owner_request(Window(0), selection)]);
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    assert!(!ready.selections.clipboard.owned);
    assert!(ready.selections.clipboard.offer.is_none());
}

#[test]
fn selection_clear_drops_ownership_so_a_later_clear_sends_nothing() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let (window, selection) = clipboard_window(&wm);
    let source = data_source(&mut state, "text/plain");
    state.seat.replace_selection_source(Some(source), 1);
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionChanged(SelectionKind::Clipboard),
    );
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionClear(SelectionClear {
            send_event: false,
            sequence: 0,
            time: 4,
            owner: window,
            selection,
        }),
    );
    assert!(sent.is_empty());
    {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        assert!(!ready.selections.clipboard.owned);
        assert!(ready.selections.clipboard.offer.is_none());
    }
    state.seat.replace_selection_source(None, 2);
    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionChanged(SelectionKind::Clipboard),
    );
    assert!(sent.is_empty());
}

#[test]
fn an_existing_primary_selection_is_owned_at_ready() {
    let (mut wm, mut state, mut dispatcher, surfaces) = drive_prepared(None, |state| {
        let mut app = Client::new_for_tests(ClientId::from_raw(4));
        let source_id = crate::protocol_generated::ZwpPrimarySelectionSourceV1Id::from(11);
        let (resolved, source) = app.insert_object(
            state,
            crate::wayland_objects::zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1::new(
                source_id,
                1,
            ),
        );
        source.offer("text/plain".to_string());
        state.seat.replace_primary_selection_source(
            Some(crate::state::PrimarySelectionSourceState::Zwp(resolved)),
            1,
        );
    });
    let expected = {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        assert!(ready.selections.primary.owned);
        assert!(!ready.selections.clipboard.owned);
        let mut expected = claim_requests(
            ready.root,
            ready.check_window,
            ready.selections.clipboard.window,
            ready.selections.primary.window,
            &ready.atoms,
        );
        expected
            .push(owner_request(ready.selections.primary.window, ready.selections.primary.atom));
        expected
    };
    let sent = take_writes(&mut wm, &mut state, &mut dispatcher, &surfaces);
    expect_requests(sent, expected);
}

fn own_clipboard(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut crate::runner::Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    mimes: &[&str],
) -> (Window, Atom) {
    let (window, selection) = clipboard_window(wm);
    let source = offered_source(state, mimes);
    state.seat.replace_selection_source(Some(source), 1);
    let sent = handle_event(
        wm,
        state,
        dispatcher,
        surfaces,
        WmEvent::SelectionChanged(SelectionKind::Clipboard),
    );
    expect_requests(sent, vec![owner_request(window, selection)]);
    (window, selection)
}

fn selection_request(
    requestor: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
    time: u32,
) -> SelectionRequest {
    SelectionRequest {
        send_event: false,
        sequence: 0,
        time,
        owner: Window(0),
        requestor,
        selection,
        target,
        property,
    }
}

fn notify_request(
    requestor: Window,
    selection: Atom,
    target: Atom,
    property: Atom,
    time: u32,
) -> Vec<u8> {
    let message = SelectionNotify {
        send_event: true,
        sequence: 0,
        time,
        requestor,
        selection,
        target,
        property,
    };
    let event = crate::x11::event_bytes(|writer| message.serialize(writer));
    testing::encode(
        &SendEvent {
            propagate: 0,
            destination: requestor,
            event_mask: EventMask::NoEvent,
            event: &event,
        },
        0,
    )
}

fn pull_reads(
    dispatcher: &mut crate::runner::Dispatcher,
) -> Vec<Redispatcher<std::io::Result<ReadWriteCompleted>>> {
    let mut reads = Vec::new();
    let mut restore = Vec::new();
    for action in dispatcher.test_take_queued_actions::<IoUringAction>() {
        match action {
            IoUringAction::ReadSome { dispatch_to, .. } => reads.push(dispatch_to),
            other => restore.push(other),
        }
    }
    for action in restore {
        dispatcher.dispatch(action);
    }
    reads
}

fn sent_mimes(dispatcher: &mut crate::runner::Dispatcher) -> Vec<String> {
    dispatcher
        .test_take_queued_actions::<ClientAction>()
        .into_iter()
        .filter_map(|action| match action {
            ClientAction::DataSourceEvent {
                event: DataSourceClientEvent::Send { mime_type, .. },
                ..
            } => Some(mime_type),
            _ => None,
        })
        .collect()
}

fn feed_read(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut crate::runner::Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    dispatch_to: Redispatcher<std::io::Result<ReadWriteCompleted>>,
    result: std::io::Result<ReadWriteCompleted>,
) -> (Vec<Vec<u8>>, Vec<Redispatcher<std::io::Result<ReadWriteCompleted>>>) {
    dispatcher.redispatch_to(dispatch_to, result);
    let mut actions = dispatcher.test_take_queued_actions::<XwaylandAction>();
    actions.retain(|action| {
        !matches!(
            action,
            XwaylandAction::KeyboardFocus
                | XwaylandAction::SelectionChanged(_)
                | XwaylandAction::SelectionCancelled { .. }
        )
    });
    assert_eq!(actions.len(), 1);
    let XwaylandAction::Wm { event, .. } = actions.pop().unwrap() else {
        panic!("outgoing read");
    };
    {
        let mut ctx = WmCtx { run: 1, state, dispatcher, surfaces, client: None };
        wm.handle(event, &mut ctx).unwrap();
    }
    let reads = pull_reads(dispatcher);
    let sent = take_writes(wm, state, dispatcher, surfaces);
    (sent, reads)
}

#[test]
fn targets_lists_the_offered_types() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (window, selection) = own_clipboard(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        &["text/plain;charset=utf-8", "text/plain"],
    );
    let ready = atoms(&wm);
    let requestor = Window(0x90);
    let property = Atom(70);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionRequest(selection_request(
            requestor,
            selection,
            ready.TARGETS,
            property,
            4,
        )),
    );
    let _ = window;
    expect_requests(
        sent,
        vec![
            testing::encode(
                &change_property_atoms(
                    requestor,
                    property,
                    Atom::ATOM,
                    &[ready.TARGETS, ready.TIMESTAMP, ready.UTF8_STRING, ready.TEXT, ready.STRING],
                )
                .request(),
                0,
            ),
            notify_request(requestor, selection, ready.TARGETS, property, 4),
        ],
    );
}

#[test]
fn timestamp_returns_the_recorded_time() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (window, selection) =
        own_clipboard(&mut wm, &mut state, &mut dispatcher, &surfaces, &["text/plain"]);
    let ready = atoms(&wm);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        owner_notify(window, window, selection, 9),
    );
    assert!(sent.is_empty());
    let requestor = Window(0x90);
    let property = Atom(71);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionRequest(selection_request(
            requestor,
            selection,
            ready.TIMESTAMP,
            property,
            4,
        )),
    );
    expect_requests(
        sent,
        vec![
            testing::encode(
                &change_property_u32s(requestor, property, Atom::INTEGER, &[9]).request(),
                0,
            ),
            notify_request(requestor, selection, ready.TIMESTAMP, property, 4),
        ],
    );
}

#[test]
fn utf8_string_is_read_from_the_wayland_source() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (_window, selection) = own_clipboard(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        &["text/plain;charset=utf-8"],
    );
    let ready = atoms(&wm);
    let requestor = Window(0x90);
    let property = Atom(72);
    {
        let mut ctx = WmCtx {
            run: 1,
            state: &mut state,
            dispatcher: &mut dispatcher,
            surfaces: &surfaces,
            client: None,
        };
        wm.on_core_event(
            Event::SelectionRequest(selection_request(
                requestor,
                selection,
                ready.UTF8_STRING,
                property,
                4,
            )),
            &mut ctx,
        )
        .unwrap();
    }
    assert_eq!(sent_mimes(&mut dispatcher), vec!["text/plain;charset=utf-8".to_string()]);
    let mut reads = pull_reads(&mut dispatcher);
    assert_eq!(reads.len(), 1);
    assert!(take_writes(&mut wm, &mut state, &mut dispatcher, &surfaces).is_empty());
    let (sent, reads) = feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        reads.pop().unwrap(),
        Ok(ReadWriteCompleted { fd: 0, offset: 0, buffer: Box::from(&b"hi"[..]) }),
    );
    assert!(sent.is_empty());
    assert_eq!(reads.len(), 1);
    let (sent, reads) = feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        reads.into_iter().next().unwrap(),
        Err(UnexpectedEof.into()),
    );
    assert!(reads.is_empty());
    expect_requests(
        sent,
        vec![
            testing::encode(
                &change_property_u8s(requestor, property, ready.UTF8_STRING, b"hi").request(),
                0,
            ),
            notify_request(requestor, selection, ready.UTF8_STRING, property, 4),
        ],
    );
}

#[test]
fn a_missing_property_uses_the_target() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (_window, selection) = own_clipboard(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        &["text/plain;charset=utf-8"],
    );
    let ready = atoms(&wm);
    let requestor = Window(0x90);
    {
        let mut ctx = WmCtx {
            run: 1,
            state: &mut state,
            dispatcher: &mut dispatcher,
            surfaces: &surfaces,
            client: None,
        };
        wm.on_core_event(
            Event::SelectionRequest(selection_request(
                requestor,
                selection,
                ready.UTF8_STRING,
                Atom(0),
                4,
            )),
            &mut ctx,
        )
        .unwrap();
    }
    let mut reads = pull_reads(&mut dispatcher);
    let _ = take_writes(&mut wm, &mut state, &mut dispatcher, &surfaces);
    let (sent, reads) = feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        reads.pop().unwrap(),
        Ok(ReadWriteCompleted { fd: 0, offset: 0, buffer: Box::from(&b"hi"[..]) }),
    );
    assert!(sent.is_empty());
    let (sent, _) = feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        reads.into_iter().next().unwrap(),
        Err(UnexpectedEof.into()),
    );
    expect_requests(
        sent,
        vec![
            testing::encode(
                &change_property_u8s(requestor, ready.UTF8_STRING, ready.UTF8_STRING, b"hi")
                    .request(),
                0,
            ),
            notify_request(requestor, selection, ready.UTF8_STRING, ready.UTF8_STRING, 4),
        ],
    );
}

#[test]
fn an_unoffered_target_is_refused() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (_window, selection) = own_clipboard(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        &["text/plain;charset=utf-8"],
    );
    let ready = atoms(&wm);
    let requestor = Window(0x90);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionRequest(selection_request(requestor, selection, ready.STRING, Atom(73), 4)),
    );
    expect_requests(sent, vec![notify_request(requestor, selection, ready.STRING, Atom(0), 4)]);
}

#[test]
fn multiple_is_refused() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (_window, selection) =
        own_clipboard(&mut wm, &mut state, &mut dispatcher, &surfaces, &["text/plain"]);
    let ready = atoms(&wm);
    let requestor = Window(0x90);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionRequest(selection_request(
            requestor,
            selection,
            ready.MULTIPLE,
            Atom(74),
            4,
        )),
    );
    expect_requests(sent, vec![notify_request(requestor, selection, ready.MULTIPLE, Atom(0), 4)]);
}

#[test]
fn a_request_without_x_focus_is_refused() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (_window, selection) =
        own_clipboard(&mut wm, &mut state, &mut dispatcher, &surfaces, &["text/plain"]);
    focus_surface(&mut state, &mut dispatcher, 40);
    let ready = atoms(&wm);
    let requestor = Window(0x90);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionRequest(selection_request(
            requestor,
            selection,
            ready.UTF8_STRING,
            Atom(75),
            4,
        )),
    );
    expect_requests(
        sent,
        vec![notify_request(requestor, selection, ready.UTF8_STRING, Atom(0), 4)],
    );
}

#[test]
fn a_request_waits_until_the_mime_atom_is_interned() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (_window, selection) = clipboard_window(&wm);
    let source = data_source(&mut state, "image/png");
    state.seat.replace_selection_source(Some(source), 1);
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionChanged(SelectionKind::Clipboard),
    );
    let ready = atoms(&wm);
    let requestor = Window(0x90);
    let property = Atom(76);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionRequest(selection_request(
            requestor,
            selection,
            ready.TARGETS,
            property,
            4,
        )),
    );
    assert!(sent.is_empty());
    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::Interned {
            name: b"image/png".to_vec(),
            result: Ok(crate::x11::InternAtomReply { sequence: 1, atom: Atom(900) }),
        },
    );
    expect_requests(
        sent,
        vec![
            testing::encode(
                &change_property_atoms(
                    requestor,
                    property,
                    Atom::ATOM,
                    &[ready.TARGETS, ready.TIMESTAMP, Atom(900)],
                )
                .request(),
                0,
            ),
            notify_request(requestor, selection, ready.TARGETS, property, 4),
        ],
    );
}

fn begin_utf8(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut crate::runner::Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    requestor: Window,
    property: Atom,
) -> (Atom, Option<RingId>, Redispatcher<std::io::Result<ReadWriteCompleted>>) {
    let (_window, selection) =
        own_clipboard(wm, state, dispatcher, surfaces, &["text/plain;charset=utf-8"]);
    let ready = atoms(wm);
    {
        let mut ctx = WmCtx { run: 1, state, dispatcher, surfaces, client: None };
        wm.on_core_event(
            Event::SelectionRequest(selection_request(
                requestor,
                selection,
                ready.UTF8_STRING,
                property,
                4,
            )),
            &mut ctx,
        )
        .unwrap();
    }
    assert_eq!(sent_mimes(dispatcher), vec!["text/plain;charset=utf-8".to_string()]);
    let mut reads = pull_read_slots(dispatcher);
    assert_eq!(reads.len(), 1);
    assert!(take_writes(wm, state, dispatcher, surfaces).is_empty());
    let (id, read) = reads.pop().unwrap();
    (selection, id, read)
}

fn pull_read_slots(
    dispatcher: &mut crate::runner::Dispatcher,
) -> Vec<(Option<RingId>, Redispatcher<std::io::Result<ReadWriteCompleted>>)> {
    let mut reads = Vec::new();
    let mut restore = Vec::new();
    for action in dispatcher.test_take_queued_actions::<IoUringAction>() {
        match action {
            IoUringAction::ReadSome { id, dispatch_to, .. } => reads.push((id, dispatch_to)),
            other => restore.push(other),
        }
    }
    for action in restore {
        dispatcher.dispatch(action);
    }
    reads
}

fn pull_cancels(dispatcher: &mut crate::runner::Dispatcher) -> Vec<RingId> {
    let mut cancels = Vec::new();
    let mut restore = Vec::new();
    for action in dispatcher.test_take_queued_actions::<IoUringAction>() {
        match action {
            IoUringAction::Cancel { target } => cancels.push(target),
            other => restore.push(other),
        }
    }
    for action in restore {
        dispatcher.dispatch(action);
    }
    cancels
}

fn handle_x(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut crate::runner::Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    event: Event,
) -> (Vec<Vec<u8>>, Vec<(Option<RingId>, Redispatcher<std::io::Result<ReadWriteCompleted>>)>) {
    {
        let mut ctx = WmCtx { run: 1, state, dispatcher, surfaces, client: None };
        wm.on_core_event(event, &mut ctx).unwrap();
    }
    let reads = pull_read_slots(dispatcher);
    let sent = take_writes(wm, state, dispatcher, surfaces);
    (sent, reads)
}

fn deleted(window: Window, atom: Atom) -> Event {
    Event::PropertyNotify(PropertyNotify {
        send_event: false,
        sequence: 0,
        window,
        atom,
        time: 0,
        state: PropertyState::Delete,
    })
}

#[test]
fn a_long_value_is_sent_incrementally() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let requestor = Window(0x90);
    let property = Atom(72);
    let (selection, _read_id, read) =
        begin_utf8(&mut wm, &mut state, &mut dispatcher, &surfaces, requestor, property);
    let ready = atoms(&wm);
    let mut data = vec![1u8; 1000];
    data.extend(std::iter::repeat_n(2u8, 1000));
    data.extend(std::iter::repeat_n(3u8, 500));
    let (sent, reads) = feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        read,
        Ok(ReadWriteCompleted { fd: 0, offset: 0, buffer: data.clone().into_boxed_slice() }),
    );
    assert!(reads.is_empty());
    expect_requests(
        sent,
        vec![
            testing::encode(
                &ChangeWindowAttributes {
                    window: requestor,
                    value_list: ChangeWindowAttributesAux::default()
                        .event_mask(EventMask::PropertyChange),
                },
                0,
            ),
            testing::encode(
                &change_property_u32s(requestor, property, ready.INCR, &[1000]).request(),
                0,
            ),
            notify_request(requestor, selection, ready.UTF8_STRING, property, 4),
        ],
    );

    let (sent, reads) =
        handle_x(&mut wm, &mut state, &mut dispatcher, &surfaces, deleted(requestor, property));
    assert!(reads.is_empty());
    expect_requests(
        sent,
        vec![testing::encode(
            &change_property_u8s(requestor, property, ready.UTF8_STRING, &data[..1000]).request(),
            0,
        )],
    );

    let (sent, mut reads) =
        handle_x(&mut wm, &mut state, &mut dispatcher, &surfaces, deleted(requestor, property));
    assert_eq!(reads.len(), 1);
    expect_requests(
        sent,
        vec![testing::encode(
            &change_property_u8s(requestor, property, ready.UTF8_STRING, &data[1000..2000])
                .request(),
            0,
        )],
    );

    let (sent, extra) =
        handle_x(&mut wm, &mut state, &mut dispatcher, &surfaces, deleted(requestor, property));
    assert!(extra.is_empty());
    expect_requests(
        sent,
        vec![testing::encode(
            &change_property_u8s(requestor, property, ready.UTF8_STRING, &data[2000..]).request(),
            0,
        )],
    );

    let (sent, drained) = feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        reads.pop().unwrap().1,
        Err(UnexpectedEof.into()),
    );
    assert!(sent.is_empty());
    assert!(drained.is_empty());
    let (sent, leftover) =
        handle_x(&mut wm, &mut state, &mut dispatcher, &surfaces, deleted(requestor, property));
    assert!(leftover.is_empty());
    expect_requests(
        sent,
        vec![testing::encode(
            &change_property_u8s(requestor, property, ready.UTF8_STRING, &[]).request(),
            0,
        )],
    );
}

#[test]
fn a_managed_requestor_already_reports_property_changes() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let requestor = Window(0x50);
    let property = Atom(72);
    let (selection, _read_id, read) =
        begin_utf8(&mut wm, &mut state, &mut dispatcher, &surfaces, requestor, property);
    let ready = atoms(&wm);
    let (sent, reads) = feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        read,
        Ok(ReadWriteCompleted { fd: 0, offset: 0, buffer: vec![7u8; 1000].into_boxed_slice() }),
    );
    assert!(reads.is_empty());
    expect_requests(
        sent,
        vec![
            testing::encode(
                &change_property_u32s(requestor, property, ready.INCR, &[1000]).request(),
                0,
            ),
            notify_request(requestor, selection, ready.UTF8_STRING, property, 4),
        ],
    );
}

#[test]
fn destroying_the_requestor_ends_the_transfer() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let requestor = Window(0x90);
    let (_selection, read_id, read) =
        begin_utf8(&mut wm, &mut state, &mut dispatcher, &surfaces, requestor, Atom(72));
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::DestroyNotify(DestroyNotify {
            send_event: false,
            sequence: 0,
            event: requestor,
            window: requestor,
        }),
    );
    assert!(sent.is_empty());
    assert!(pull_cancels(&mut dispatcher).contains(&read_id.unwrap()));
    let (sent, reads) = feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        read,
        Ok(ReadWriteCompleted { fd: 0, offset: 0, buffer: vec![1u8; 1000].into_boxed_slice() }),
    );
    assert!(sent.is_empty());
    assert!(reads.is_empty());
}

#[test]
fn an_incremental_read_is_cancelled_when_it_times_out() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let requestor = Window(0x90);
    let property = Atom(72);
    let (selection, _read_id, read) =
        begin_utf8(&mut wm, &mut state, &mut dispatcher, &surfaces, requestor, property);
    let ready = atoms(&wm);
    let (sent, reads) = feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        read,
        Ok(ReadWriteCompleted { fd: 0, offset: 0, buffer: vec![4u8; 1000].into_boxed_slice() }),
    );
    assert!(reads.is_empty());
    expect_requests(
        sent,
        vec![
            testing::encode(
                &ChangeWindowAttributes {
                    window: requestor,
                    value_list: ChangeWindowAttributesAux::default()
                        .event_mask(EventMask::PropertyChange),
                },
                0,
            ),
            testing::encode(
                &change_property_u32s(requestor, property, ready.INCR, &[1000]).request(),
                0,
            ),
            notify_request(requestor, selection, ready.UTF8_STRING, property, 4),
        ],
    );
    let (sent, mut reads) =
        handle_x(&mut wm, &mut state, &mut dispatcher, &surfaces, deleted(requestor, property));
    expect_requests(
        sent,
        vec![testing::encode(
            &change_property_u8s(requestor, property, ready.UTF8_STRING, &[4u8; 1000]).request(),
            0,
        )],
    );
    assert_eq!(reads.len(), 1);
    let read_id = reads.pop().unwrap().0.unwrap();
    {
        let mut ctx = WmCtx {
            run: 1,
            state: &mut state,
            dispatcher: &mut dispatcher,
            surfaces: &surfaces,
            client: None,
        };
        wm.test_expire_transfers(Instant::now() + Duration::from_secs(6), &mut ctx);
    }
    assert!(pull_cancels(&mut dispatcher).contains(&read_id));
    assert!(take_writes(&mut wm, &mut state, &mut dispatcher, &surfaces).is_empty());
    let (sent, leftover) =
        handle_x(&mut wm, &mut state, &mut dispatcher, &surfaces, deleted(requestor, property));
    assert!(sent.is_empty());
    assert!(leftover.is_empty());
}

fn publish_utf8_clipboard(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut crate::runner::Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
) -> crate::state::ObjectId {
    let (window, selection) = clipboard_window(wm);
    let ready = atoms(wm);
    deliver(wm, state, dispatcher, surfaces, owner_notify(window, Window(0x70), selection, 3));
    deliver(
        wm,
        state,
        dispatcher,
        surfaces,
        Event::SelectionNotify(SelectionNotify {
            send_event: false,
            sequence: 0,
            time: 3,
            requestor: window,
            selection,
            target: ready.TARGETS,
            property: ready._WL_SELECTION,
        }),
    );
    handle_client(
        wm,
        state,
        dispatcher,
        surfaces,
        WmEvent::SelectionTargets {
            kind: SelectionKind::Clipboard,
            result: Ok(atom_list(&[ready.UTF8_STRING])),
        },
    );
    let SelectionSourceState::Xwayland(installed) = state.seat.selection_source().unwrap() else {
        panic!("x source");
    };
    installed.object
}

fn focus_data_device(
    state: &mut crate::State,
    dispatcher: &mut crate::runner::Dispatcher,
) -> Client {
    let mut client = Client::new_for_tests(ClientId::from_raw(8));
    client.insert_object(state, WlSurface::new(WlSurfaceId::from(40), client.client_id(), 6));
    client.insert_object(state, WlDataDevice::new(WlDataDeviceId::from(50), WlSeatId::from(10), 3));
    state.seat.register_data_device(Resolved::new(
        client.client_id(),
        WlDataDeviceId::from(50),
        client.object_id(WlDataDeviceId::from(50)),
    ));
    set_keyboard_focus_from_surface(
        state,
        dispatcher,
        Some(SurfaceFocus::new(Resolved::new(
            client.client_id(),
            WlSurfaceId::from(40),
            client.object_id(WlSurfaceId::from(40)),
        ))),
    );
    client
}

#[test]
#[test]
fn an_unknown_selection_request_is_left_unanswered() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let ready = atoms(&wm);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::SelectionRequest(selection_request(
            Window(0x90),
            ready.XdndSelection,
            ready.UTF8_STRING,
            Atom(70),
            4,
        )),
    );
    assert!(sent.is_empty());
}

fn stopping_clears_the_x_selection_and_drops_a_pending_receive() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let object = publish_utf8_clipboard(&mut wm, &mut state, &mut dispatcher, &surfaces);
    let mut client = focus_data_device(&mut state, &mut dispatcher);
    let (fd, _raw, _occupy) = held_fd();
    let watched = Rc::clone(&fd);
    handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::SelectionSend {
            source: object,
            mime_type: "text/plain;charset=utf-8".to_string(),
            fd,
        },
    );
    assert_eq!(Rc::strong_count(&watched), 2);

    wm.stop(&mut state, &mut dispatcher);

    assert!(state.seat.selection_source().is_none());
    assert!(state.objects.try_get(object).is_none());
    assert_eq!(Rc::strong_count(&watched), 1);
    client
        .process(&mut state, ClientAction::RefreshDataDeviceSelection, &mut dispatcher)
        .unwrap();
    assert!(capture::test_sent_arg(&dispatcher, 50, 5, 0));
}

#[test]
fn stopping_cancels_an_outgoing_read() {
    let (mut wm, mut state, mut dispatcher, surfaces) = focused_x_window();
    let (_selection, read_id, _read) =
        begin_utf8(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x90), Atom(72));
    wm.stop(&mut state, &mut dispatcher);
    assert!(pull_cancels(&mut dispatcher).contains(&read_id.unwrap()));
    assert!(matches!(state.seat.selection_source(), Some(SelectionSourceState::WlData(_))));
}
