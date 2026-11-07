use std::{
    os::fd::{AsRawFd, OwnedFd},
    rc::Rc,
};

use crate::{
    clients::{ClientId, Resolved},
    cursor::CursorImage,
    io_uring::{
        model::IoUringAction,
        ring::{ReadWriteCompleted, RingId},
    },
    process::ProcessEvent,
    protocol_generated::WlSurfaceId,
    runner::{Dispatcher, Model, Redispatcher},
    test_support::state_for_tests,
    wayland_objects::wl_surface::WlSurface,
    x11::{
        change_property_atoms, change_property_u32s, change_property_utf8,
        testing::{self, ATOM_BASE, COMPOSITE_MAJOR_OPCODE, RENDER_MAJOR_OPCODE},
        Atom, ChangeWindowAttributes, ChangeWindowAttributesAux, ClientMessage, ClientMessageData,
        Colormap, CompositeRedirectSubwindows, ConfigWindow, ConfigureNotify, ConfigureRequest,
        ConfigureWindow, ConfigureWindowAux, CreateGc, CreateGcAux, CreateNotify, CreatePixmap,
        CreateWindow, CreateWindowAux, Cursor, DestroyNotify, Directformat, Drawable, Event,
        EventMask, FreeGc, FreePixmap, Gcontext, GetProperty, ImageFormat, MapNotify, MapRequest,
        MapWindow, PictType, Pictformat, Pictforminfo, Picture, Pixmap, PropertyNotify,
        PropertyState, PutImage, Redirect, RenderCreateCursor, RenderCreatePicture,
        RenderCreatePictureAux, RenderFreePicture, RenderQueryPictFormats, SetSelectionOwner,
        StackMode, UnmapNotify, Window, WindowClass, XConnection, NAMES,
    },
};

use super::super::{Phase, SurfaceEvent, SurfaceSerials, Xwayland, XwaylandAction};

use super::{Wm, WmCtx, WmEvent, WmPhase};

const X11_READ: usize = 64 * 1024;

#[test]
fn setup_bytes_are_queued_at_start() {
    let fd = std::fs::File::open("/dev/null").unwrap();
    let mut dispatcher = Dispatcher::default();
    let _wm = Wm::start(fd.as_raw_fd(), 1, 1, &mut dispatcher);
    let (_, expected) = XConnection::new();
    let io = drain_io(&mut dispatcher);
    assert_eq!(io.writes.len(), 1);
    assert_eq!(&*io.writes[0].0, expected.as_slice());
    assert_eq!(io.reads.len(), 1);
}

#[test]
fn a_setup_reply_then_startup_reaches_ready() {
    let (wm, _state, _dispatcher, _surfaces) = drive_to_ready(None);
    let WmPhase::Ready(ready) = &wm.phase else { panic!("startup did not become ready") };
    assert_eq!(ready.root, Window(0x100));
    let index = |name: &str| NAMES.iter().position(|entry| *entry == name).unwrap() as u32;
    assert_eq!(ready.atoms.WM_S0, Atom(ATOM_BASE + index("WM_S0")));
    assert_eq!(ready.extensions.xfixes.major_version, 6);
}

#[test]
fn a_setup_failure_is_a_wm_failure() {
    let fd = std::fs::File::open("/dev/null").unwrap();
    let mut state = state_for_tests();
    let mut dispatcher = Dispatcher::default();
    let mut wm = Wm::start(fd.as_raw_fd(), 1, 1, &mut dispatcher);
    let surfaces = SurfaceSerials::default();
    let io = drain_io(&mut dispatcher);
    let read = io.reads.into_iter().next().unwrap();
    let action = complete(&mut dispatcher, read, Ok(completed(testing::setup_failed("no").into())));
    let error = apply(&mut wm, &mut state, &mut dispatcher, &surfaces, action).unwrap_err();
    assert!(error.to_string().contains("no"), "{error}");
    assert!(matches!(wm.phase, WmPhase::Failed));
}

#[test]
fn the_claim_matches_the_encoded_requests_and_uses_the_resource_base() {
    let (wm, _state, mut dispatcher, _surfaces) = drive_to_ready(None);
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    let base = Window(0x20);
    assert_eq!(ready.check_window, base);
    let io = drain_io(&mut dispatcher);
    assert_eq!(io.writes.len(), 1);
    let sent = testing::split_requests(&io.writes[0].0);
    let expected = claim_requests(ready.root, ready.check_window, &ready.atoms);
    assert_eq!(&sent[..expected.len()], expected.as_slice());
    assert_eq!(sent.len(), expected.len() + 1);
    assert_eq!(sent.last().unwrap()[0], 43);
}

#[test]
fn a_protocol_error_on_the_root_mask_is_logged_and_the_wm_stays_up() {
    let (mut wm, mut state, mut dispatcher, surfaces) = drive_to_ready(None);
    let io = drain_io(&mut dispatcher);
    let read = io.reads.into_iter().next().expect("x11 read");
    let sequence = u16::try_from(8 + NAMES.len() + 1).unwrap();
    feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        read,
        testing::error_bytes(sequence, 10),
    );
    assert_eq!(wm.conn.logged_errors(), &["ChangeWindowAttributes Access".to_string()]);
    assert!(matches!(wm.phase, WmPhase::Ready(_)));
}

#[test]
fn no_cursor_request_is_sent_without_an_image() {
    let (_wm, _state, mut dispatcher, _surfaces) = drive_to_ready(None);
    let io = drain_io(&mut dispatcher);
    let sent = testing::split_requests(&io.writes[0].0);
    assert!(sent
        .iter()
        .all(|request| request.first().copied() != Some(RENDER_MAJOR_OPCODE)));
}

#[test]
fn a_pict_formats_reply_uploads_the_root_cursor() {
    let image =
        CursorImage::new("default".to_string(), 1, 1, 2, 5, 1, vec![0x11, 0x22, 0x33, 0x44]);
    let (mut wm, mut state, mut dispatcher, surfaces) = drive_to_ready(Some(image.clone()));
    let io = drain_io(&mut dispatcher);
    let (buffer, dispatch_to) = io.writes.into_iter().next().unwrap();
    let sent = testing::split_requests(&buffer);
    let query = testing::encode(&RenderQueryPictFormats, RENDER_MAJOR_OPCODE);
    assert_eq!(sent.last().unwrap(), &query);
    ack_write(&mut wm, &mut state, &mut dispatcher, &surfaces, dispatch_to);
    let sequence = u16::try_from(8 + NAMES.len() + sent.len()).unwrap();
    let read = io.reads.into_iter().next().unwrap();
    feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        read,
        testing::pict_formats_reply(sequence, vec![argb_format()]),
    );
    pump_events(&mut wm, &mut state, &mut dispatcher, &surfaces);
    let io = drain_io(&mut dispatcher);
    assert_eq!(io.writes.len(), 1);
    let uploaded = testing::split_requests(&io.writes[0].0);
    let expected = cursor_requests(&image);
    assert_eq!(&uploaded[..expected.len()], expected.as_slice());
    assert_eq!(uploaded.len(), expected.len() + 1);
    assert_eq!(uploaded.last().unwrap()[0], 43);
    assert!(matches!(wm.phase, WmPhase::Ready(_)));
}

#[test]
fn a_pict_formats_error_skips_the_cursor() {
    let image = CursorImage::new("default".to_string(), 1, 1, 0, 0, 1, vec![1, 2, 3, 4]);
    let (mut wm, mut state, mut dispatcher, surfaces) = drive_to_ready(Some(image));
    let io = drain_io(&mut dispatcher);
    let sent = testing::split_requests(&io.writes[0].0);
    let sequence = u16::try_from(8 + NAMES.len() + sent.len()).unwrap();
    ack_write(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        io.writes.into_iter().next().unwrap().1,
    );
    feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        io.reads.into_iter().next().unwrap(),
        testing::error_bytes(sequence, 10),
    );
    pump_events(&mut wm, &mut state, &mut dispatcher, &surfaces);
    assert!(drain_io(&mut dispatcher).writes.is_empty());
    assert!(matches!(wm.phase, WmPhase::Ready(_)));
}

#[test]
fn display_ready_starts_the_wm_and_a_failure_kills_the_run() {
    let _lock = crate::process::lock_forks();
    let (pidfd, _reap) = sleeping_child();
    let (mut xwayland, mut state, mut dispatcher) = running_with(Rc::clone(&pidfd));
    xwayland
        .process(
            &mut state,
            XwaylandAction::DisplayReady {
                run: 1,
                read: RingId::fresh(),
                result: Ok(completed(b"1\n".to_vec().into_boxed_slice())),
            },
            &mut dispatcher,
        )
        .unwrap();
    assert!(wm_is_some(&xwayland));

    xwayland
        .process(
            &mut state,
            XwaylandAction::Wm {
                run: 0,
                event: WmEvent::Written {
                    generation: 99,
                    token: 99,
                    result: Ok(completed(Box::new([]))),
                },
            },
            &mut dispatcher,
        )
        .unwrap();
    assert!(wm_is_some(&xwayland));

    let mut x11_read = None;
    for action in dispatcher.test_take_queued_actions::<IoUringAction>() {
        match action {
            IoUringAction::ReadSome { length, dispatch_to, .. } if length == X11_READ => {
                x11_read = Some(dispatch_to);
            }
            other => dispatcher.dispatch(other),
        }
    }
    let action = complete(
        &mut dispatcher,
        x11_read.expect("x11 read"),
        Ok(completed(testing::setup_failed("no").into())),
    );
    xwayland
        .process(&mut state, action, &mut dispatcher)
        .unwrap();
    assert!(!wm_is_some(&xwayland));
    assert!(pidfd_readable(pidfd.as_ref(), 5_000));
}

#[test]
fn create_notify_tracks_the_window_and_selects_its_events() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
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
            x: -4,
            y: 8,
            width: 40,
            height: 20,
            border_width: 0,
            override_redirect: 1,
        }),
    );
    let tracked = wm.windows.get(&window).unwrap();
    let rect = super::window::Rect { x: -4, y: 8, width: 40, height: 20 };
    assert_eq!(tracked.geometry, rect);
    assert_eq!(tracked.pending, rect);
    assert!(tracked.override_redirect);
    assert!(!tracked.mapped);
    assert!(!tracked.shown);
    assert!(tracked.serial.is_none());
    assert!(tracked.surface.is_none());
    expect_requests(
        sent,
        vec![testing::encode(
            &ChangeWindowAttributes {
                window,
                value_list: ChangeWindowAttributesAux::default()
                    .event_mask(EventMask::PropertyChange.union(EventMask::FocusChange)),
            },
            0,
        )],
    );
    let check = {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        ready.check_window
    };
    let before = wm.windows.len();
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::CreateNotify(CreateNotify {
            send_event: false,
            sequence: 0,
            parent: Window(0x100),
            window: check,
            x: 0,
            y: 0,
            width: 10,
            height: 10,
            border_width: 0,
            override_redirect: 0,
        }),
    );
    assert_eq!(wm.windows.len(), before);
    assert!(sent.is_empty());
}

#[test]
fn map_request_places_a_sized_window_and_ignores_an_unknown_one() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 3, 4, 30, 12, 0));
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    let atoms = ready.atoms;
    let state_property = change_property_u32s(window, atoms.WM_STATE, atoms.WM_STATE, &[1, 0]);
    let net_wm_state = change_property_atoms(window, atoms._NET_WM_STATE, atoms.ATOM, &[]);
    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_request(window));
    expect_requests(
        sent,
        vec![
            testing::encode(&state_property.request(), 0),
            testing::encode(
                &ConfigureWindow {
                    window,
                    value_list: ConfigureWindowAux::default().x(0).y(0).width(1).height(1),
                },
                0,
            ),
            testing::encode(
                &ConfigureWindow {
                    window,
                    value_list: ConfigureWindowAux::default().x(3).y(4).width(30).height(12),
                },
                0,
            ),
            testing::encode(&net_wm_state.request(), 0),
            testing::encode(&MapWindow { window }, 0),
        ],
    );
    assert!(deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_request(Window(0x51)))
        .is_empty());
}

#[test]
fn map_request_skips_configure_when_the_pending_size_is_empty() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 0, 0, 0, 10, 0));
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    let atoms = ready.atoms;
    let state_property = change_property_u32s(window, atoms.WM_STATE, atoms.WM_STATE, &[1, 0]);
    let net_wm_state = change_property_atoms(window, atoms._NET_WM_STATE, atoms.ATOM, &[]);
    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_request(window));
    expect_requests(
        sent,
        vec![
            testing::encode(&state_property.request(), 0),
            testing::encode(&net_wm_state.request(), 0),
            testing::encode(&MapWindow { window }, 0),
        ],
    );
}

#[test]
fn map_and_unmap_update_mapped_and_wm_state() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 8, 9, 0));
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::MapNotify(MapNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
            override_redirect: 1,
        }),
    )
    .is_empty());
    let tracked = wm.windows.get(&window).unwrap();
    assert!(tracked.mapped);
    assert!(tracked.override_redirect);
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    let wm_state = ready.atoms.WM_STATE;
    let withdrawn = change_property_u32s(window, wm_state, wm_state, &[0, 0]);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::UnmapNotify(UnmapNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
            from_configure: 0,
        }),
    );
    expect_requests(sent, vec![testing::encode(&withdrawn.request(), 0)]);
    assert!(!wm.windows.get(&window).unwrap().mapped);
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::UnmapNotify(UnmapNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window: Window(0x51),
            from_configure: 0,
        }),
    )
    .is_empty());
}

#[test]
fn configure_request_merges_until_the_window_is_shown() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 3, 4, 30, 12, 0));
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        configure_request(window, ConfigWindow::Width | ConfigWindow::Height, 9, 10, 11, 13),
    )
    .is_empty());
    let tracked = wm.windows.get(&window).unwrap();
    assert_eq!(tracked.pending, super::window::Rect { x: 3, y: 4, width: 11, height: 13 });
    assert_eq!(tracked.geometry, super::window::Rect { x: 3, y: 4, width: 30, height: 12 });
    wm.windows.get_mut(&window).unwrap().shown = true;
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        configure_request(window, ConfigWindow::X | ConfigWindow::Y, 80, 90, 1, 1),
    )
    .is_empty());
    assert_eq!(
        wm.windows.get(&window).unwrap().pending,
        super::window::Rect { x: 3, y: 4, width: 11, height: 13 }
    );
}

#[test]
fn configure_notify_updates_geometry_and_pending_while_hidden() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 3, 4, 30, 12, 0));
    wm.windows.get_mut(&window).unwrap().pending.x = 7;
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::ConfigureNotify(ConfigureNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
            above_sibling: Window(0),
            x: 5,
            y: 6,
            width: 15,
            height: 16,
            border_width: 0,
            override_redirect: 1,
        }),
    )
    .is_empty());
    let tracked = wm.windows.get(&window).unwrap();
    let next = super::window::Rect { x: 5, y: 6, width: 15, height: 16 };
    assert_eq!(tracked.geometry, next);
    assert_eq!(tracked.pending, next);
    assert!(tracked.override_redirect);
    wm.windows.get_mut(&window).unwrap().shown = true;
    wm.windows.get_mut(&window).unwrap().pending.x = 7;
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::ConfigureNotify(ConfigureNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
            above_sibling: Window(0),
            x: 1,
            y: 2,
            width: 3,
            height: 4,
            border_width: 0,
            override_redirect: 0,
        }),
    )
    .is_empty());
    let tracked = wm.windows.get(&window).unwrap();
    assert_eq!(tracked.geometry, super::window::Rect { x: 1, y: 2, width: 3, height: 4 });
    assert_eq!(tracked.pending.x, 7);
    assert!(!tracked.override_redirect);
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::ConfigureNotify(ConfigureNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window: Window(0x51),
            above_sibling: Window(0),
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            border_width: 0,
            override_redirect: 0,
        }),
    )
    .is_empty());
}

#[test]
fn destroy_notify_removes_the_window() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 1, 2, 2, 0));
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::DestroyNotify(DestroyNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
        }),
    )
    .is_empty());
    assert!(wm.windows.get(&window).is_none());
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::DestroyNotify(DestroyNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
        }),
    )
    .is_empty());
}

#[test]
fn the_client_message_then_the_surface_pairs() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 3, 4, 0));
    let atom = serial_atom(&wm);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(window, atom, 1));
    assert_eq!(wm.windows.get(&window).unwrap().serial, Some(1));
    assert!(wm.windows.get(&window).unwrap().surface.is_none());
    let surface = x_surface(&mut state, 4, 1);
    let event = SurfaceEvent::SerialAssigned { serial: 1, surface };
    surfaces.apply(event);
    note_surface(&mut wm, &mut state, &mut dispatcher, &surfaces, &event);
    assert_eq!(wm.windows.get(&window).unwrap().surface, Some(surface));
}

#[test]
fn the_surface_then_the_client_message_pairs() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 3, 4, 0));
    let surface = x_surface(&mut state, 4, 1);
    let event = SurfaceEvent::SerialAssigned { serial: 1, surface };
    surfaces.apply(event);
    note_surface(&mut wm, &mut state, &mut dispatcher, &surfaces, &event);
    assert!(wm.windows.get(&window).unwrap().surface.is_none());
    let atom = serial_atom(&wm);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(window, atom, 1));
    assert_eq!(wm.windows.get(&window).unwrap().surface, Some(surface));
}

#[test]
fn a_non_monotonic_serial_is_ignored() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 3, 4, 0));
    let atom = serial_atom(&wm);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(window, atom, 2));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(window, atom, 2));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(window, atom, 1));
    assert_eq!(wm.last_serial, 2);
    assert_eq!(wm.windows.get(&window).unwrap().serial, Some(2));
    assert_eq!(wm.by_serial.get(&2), Some(&window));
    assert!(wm.by_serial.get(&1).is_none());
}

#[test]
fn a_new_serial_unpairs_the_old_surface() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 3, 4, 0));
    let atom = serial_atom(&wm);
    let first = x_surface(&mut state, 4, 1);
    let assigned = SurfaceEvent::SerialAssigned { serial: 1, surface: first };
    surfaces.apply(assigned);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(window, atom, 1));
    assert_eq!(wm.windows.get(&window).unwrap().surface, Some(first));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(window, atom, 2));
    let tracked = wm.windows.get(&window).unwrap();
    assert_eq!(tracked.serial, Some(2));
    assert!(tracked.surface.is_none());
    assert!(wm.by_serial.get(&1).is_none());
    assert_eq!(wm.by_serial.get(&2), Some(&window));
}

#[test]
fn destroyed_unpairs_the_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 3, 4, 0));
    let surface = x_surface(&mut state, 4, 1);
    let assigned = SurfaceEvent::SerialAssigned { serial: 1, surface };
    surfaces.apply(assigned);
    let atom = serial_atom(&wm);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(window, atom, 1));
    let destroyed = SurfaceEvent::Destroyed { serial: 1, surface };
    surfaces.apply(destroyed);
    note_surface(&mut wm, &mut state, &mut dispatcher, &surfaces, &destroyed);
    assert!(wm.windows.get(&window).unwrap().surface.is_none());
    assert_eq!(wm.by_serial.get(&1), Some(&window));
}

#[test]
fn destroy_notify_drops_the_serial() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 3, 4, 0));
    let atom = serial_atom(&wm);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(window, atom, 1));
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::DestroyNotify(DestroyNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
        }),
    );
    assert!(wm.windows.get(&window).is_none());
    assert!(wm.by_serial.get(&1).is_none());
}

#[test]
fn pairing_requests_every_window_property() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 3, 4, 0));
    let atoms = {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        ready.atoms
    };
    let surface = x_surface(&mut state, 4, 1);
    let assigned = SurfaceEvent::SerialAssigned { serial: 1, surface };
    surfaces.apply(assigned);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        serial_message(window, atoms.WL_SURFACE_SERIAL, 1),
    );
    let loaded = super::props::loads(&atoms);
    assert_eq!(
        loaded.map(|(property, _)| property),
        [
            atoms.WM_CLASS,
            atoms.WM_NAME,
            atoms._NET_WM_NAME,
            atoms.WM_TRANSIENT_FOR,
            atoms.WM_PROTOCOLS,
            atoms.WM_HINTS,
            atoms.WM_NORMAL_HINTS,
            atoms._MOTIF_WM_HINTS,
            atoms._NET_STARTUP_ID,
            atoms._NET_WM_STATE,
            atoms._NET_WM_WINDOW_TYPE,
            atoms.WM_WINDOW_ROLE,
        ]
    );
    assert_eq!(
        loaded.map(|(_, ty)| ty),
        [
            Atom::Any,
            Atom::Any,
            Atom::Any,
            atoms.WINDOW,
            atoms.ATOM,
            Atom::Any,
            atoms.WM_SIZE_HINTS,
            Atom::Any,
            Atom::Any,
            Atom::Any,
            atoms.ATOM,
            Atom::Any,
        ]
    );
    let expected: Vec<Vec<u8>> = loaded
        .into_iter()
        .map(|(property, ty)| property_request(window, property, ty))
        .collect();
    assert_eq!(sent, expected);
}

#[test]
fn property_notify_reloads_that_atom() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 3, 4, 0));
    let atoms = {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        ready.atoms
    };
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::PropertyNotify(PropertyNotify {
            send_event: false,
            sequence: 0,
            window,
            atom: atoms.WM_NAME,
            time: 0,
            state: PropertyState::NewValue,
        }),
    );
    assert_eq!(sent, vec![property_request(window, atoms.WM_NAME, Atom::Any)]);
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::PropertyNotify(PropertyNotify {
            send_event: false,
            sequence: 0,
            window,
            atom: atoms.WM_DELETE_WINDOW,
            time: 0,
            state: PropertyState::NewValue,
        }),
    )
    .is_empty());
    assert!(deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::PropertyNotify(PropertyNotify {
            send_event: false,
            sequence: 0,
            window: Window(0x51),
            atom: atoms.WM_NAME,
            time: 0,
            state: PropertyState::NewValue,
        }),
    )
    .is_empty());
}

#[test]
fn a_property_reply_fills_the_window_and_a_destroyed_window_drops_it() {
    let (mut wm, mut state, mut dispatcher, mut surfaces, read) = open_with_read();
    let window = Window(0x50);
    let other = Window(0x51);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 3, 4, 0));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(other, 1, 2, 3, 4, 0));
    let atoms = {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        ready.atoms
    };
    let surface = x_surface(&mut state, 4, 1);
    let assigned = SurfaceEvent::SerialAssigned { serial: 1, surface };
    surfaces.apply(assigned);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        serial_message(window, atoms.WL_SURFACE_SERIAL, 1),
    );
    let sequence = u16::try_from(wm.conn.test_next_sequence() - 12).unwrap();
    feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        read,
        testing::property_reply(sequence, 8, atoms.STRING, 4, b"inst\0App"),
    );
    pump_events(&mut wm, &mut state, &mut dispatcher, &surfaces);
    let props = &wm.windows.get(&window).unwrap().props;
    assert_eq!(props.instance.as_deref(), Some("inst"));
    assert_eq!(props.class.as_deref(), Some("App"));
    assert!(props.title.is_none());

    {
        let mut ctx = WmCtx {
            run: 1,
            state: &mut state,
            dispatcher: &mut dispatcher,
            surfaces: &surfaces,
        };
        wm.on_core_event(
            Event::DestroyNotify(DestroyNotify {
                send_event: false,
                sequence: 0,
                event: Window(0x100),
                window,
            }),
            &mut ctx,
        )
        .unwrap();
    }
    wm.io.flush(&mut wm.conn, &mut dispatcher);
    let io = drain_io(&mut dispatcher);
    let read = io.reads.into_iter().next().expect("rearmed read");
    feed_read(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        read,
        testing::property_reply(sequence + 1, 8, atoms.STRING, 0, b"gone"),
    );
    pump_events(&mut wm, &mut state, &mut dispatcher, &surfaces);
    assert!(wm.windows.get(&window).is_none());
    assert!(wm.windows.get(&other).unwrap().props.title.is_none());
    assert!(matches!(wm.phase, WmPhase::Ready(_)));
}

#[test]
fn map_request_writes_net_wm_state_from_the_flags() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 3, 4, 30, 12, 0));
    {
        let props = &mut wm.windows.get_mut(&window).unwrap().props;
        props.modal = true;
        props.fullscreen = true;
        props.hidden = true;
    }
    let atoms = {
        let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
        ready.atoms
    };
    let state_atoms =
        [atoms._NET_WM_STATE_MODAL, atoms._NET_WM_STATE_FULLSCREEN, atoms._NET_WM_STATE_HIDDEN];
    let net_wm_state = change_property_atoms(window, atoms._NET_WM_STATE, atoms.ATOM, &state_atoms);
    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_request(window));
    assert!(sent
        .iter()
        .any(|request| request == &testing::encode(&net_wm_state.request(), 0)));
}

#[test]
fn a_paired_mapped_window_is_shown_at_its_x_geometry() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let surface = show_at(&mut wm, &mut state, &mut dispatcher, &mut surfaces, Window(0x50), 80, 1);
    assert!(wm.windows.get(&Window(0x50)).unwrap().shown);
    assert!(!hidden(&state, surface));
    assert_eq!(root_position(&state, surface), (30, 40));
    assert!(scene_has(&state, surface));
    assert_eq!(picked(&state, (30.5, 40.5)), Some(surface));
    assert!(picked(&state, (0.5, 0.5)).is_none());

    state
        .objects
        .get_mut(surface.object_id())
        .downcast_mut::<WlSurface>()
        .unwrap()
        .test_clear_committed_buffer();
    assert!(picked(&state, (30.5, 40.5)).is_none());
}

#[test]
fn unmap_destroy_and_unpair_each_hide() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();

    let unmapped =
        show_at(&mut wm, &mut state, &mut dispatcher, &mut surfaces, Window(0x50), 80, 1);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::UnmapNotify(UnmapNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window: Window(0x50),
            from_configure: 0,
        }),
    );
    assert!(!wm.windows.get(&Window(0x50)).unwrap().shown);
    assert!(hidden(&state, unmapped));
    assert!(!scene_has(&state, unmapped));

    let destroyed =
        show_at(&mut wm, &mut state, &mut dispatcher, &mut surfaces, Window(0x51), 81, 2);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::DestroyNotify(DestroyNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window: Window(0x51),
        }),
    );
    assert!(wm.windows.get(&Window(0x51)).is_none());
    assert!(hidden(&state, destroyed));
    assert!(!scene_has(&state, destroyed));

    let unpaired =
        show_at(&mut wm, &mut state, &mut dispatcher, &mut surfaces, Window(0x52), 82, 3);
    let event = SurfaceEvent::Destroyed { serial: 3, surface: unpaired };
    surfaces.apply(event);
    note_surface(&mut wm, &mut state, &mut dispatcher, &surfaces, &event);
    assert!(!wm.windows.get(&Window(0x52)).unwrap().shown);
    assert!(hidden(&state, unpaired));
    assert!(!scene_has(&state, unpaired));
}

#[test]
fn configure_notify_while_shown_moves_the_root() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    let surface = show_at(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::ConfigureNotify(ConfigureNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
            above_sibling: Window(0),
            x: 80,
            y: 90,
            width: 10,
            height: 12,
            border_width: 0,
            override_redirect: 0,
        }),
    );
    let tracked = wm.windows.get(&window).unwrap();
    assert!(tracked.shown);
    assert_eq!(tracked.pending, super::window::Rect { x: 30, y: 40, width: 10, height: 12 });
    assert_eq!(root_position(&state, surface), (80, 90));
    assert_eq!(picked(&state, (80.5, 90.5)), Some(surface));
    assert!(picked(&state, (30.5, 40.5)).is_none());
}

fn open_wm() -> (Wm, crate::State, Dispatcher, SurfaceSerials) {
    let (mut wm, mut state, mut dispatcher, surfaces) = drive_to_ready(None);
    let io = drain_io(&mut dispatcher);
    let (_, dispatch_to) = io.writes.into_iter().next().expect("claim write");
    ack_write(&mut wm, &mut state, &mut dispatcher, &surfaces, dispatch_to);
    (wm, state, dispatcher, surfaces)
}

fn deliver(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &SurfaceSerials,
    event: Event,
) -> Vec<Vec<u8>> {
    {
        let mut ctx = WmCtx { run: 1, state, dispatcher, surfaces };
        wm.on_core_event(event, &mut ctx).unwrap();
    }
    wm.io.flush(&mut wm.conn, dispatcher);
    let io = drain_io(dispatcher);
    let sent = io
        .writes
        .iter()
        .flat_map(|(buffer, _)| testing::split_requests(buffer))
        .collect();
    for (_, dispatch_to) in io.writes {
        ack_write(wm, state, dispatcher, surfaces, dispatch_to);
    }
    sent
}

fn expect_requests(sent: Vec<Vec<u8>>, expected: Vec<Vec<u8>>) {
    assert_eq!(&sent[..expected.len()], expected.as_slice());
    assert_eq!(sent.len(), expected.len() + 1);
    assert_eq!(sent.last().unwrap()[0], 43);
}

fn created(
    window: Window,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    override_redirect: u8,
) -> Event {
    Event::CreateNotify(CreateNotify {
        send_event: false,
        sequence: 0,
        parent: Window(0x100),
        window,
        x,
        y,
        width,
        height,
        border_width: 0,
        override_redirect,
    })
}

fn map_request(window: Window) -> Event {
    Event::MapRequest(MapRequest {
        send_event: false,
        sequence: 0,
        parent: Window(0x100),
        window,
    })
}

fn serial_atom(wm: &Wm) -> Atom {
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    ready.atoms.WL_SURFACE_SERIAL
}

fn serial_message(window: Window, atom: Atom, serial: u64) -> Event {
    let mut data = ClientMessageData::default();
    data.set_data32([serial as u32, (serial >> 32) as u32, 0, 0, 0]);
    Event::ClientMessage(ClientMessage {
        send_event: false,
        sequence: 0,
        format: 32,
        window,
        r#type: atom,
        data,
    })
}

fn show_at(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &mut SurfaceSerials,
    window: Window,
    raw: u32,
    serial: u64,
) -> Resolved<WlSurfaceId> {
    deliver(wm, state, dispatcher, surfaces, created(window, 30, 40, 10, 12, 0));
    deliver(
        wm,
        state,
        dispatcher,
        surfaces,
        Event::MapNotify(MapNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
            override_redirect: 0,
        }),
    );
    let surface = buffered_x_root(state, raw, serial, 10, 12);
    let assigned = SurfaceEvent::SerialAssigned { serial, surface };
    surfaces.apply(assigned);
    let atom = serial_atom(wm);
    deliver(wm, state, dispatcher, surfaces, serial_message(window, atom, serial));
    surface
}

fn buffered_x_root(
    state: &mut crate::State,
    raw: u32,
    serial: u64,
    width: i32,
    height: i32,
) -> Resolved<WlSurfaceId> {
    let surface = x_surface(state, raw, serial);
    state
        .objects
        .get_mut(surface.object_id())
        .downcast_mut::<WlSurface>()
        .unwrap()
        .test_map(crate::state::ObjectId::from(900 + raw as usize), 0, 0, width, height);
    state.register_root_surface(surface);
    surface
}

fn hidden(state: &crate::State, surface: Resolved<WlSurfaceId>) -> bool {
    surface.try_ref(state).unwrap().is_hidden_xwayland_surface()
}

fn root_position(state: &crate::State, surface: Resolved<WlSurfaceId>) -> (i32, i32) {
    surface.try_ref(state).unwrap().root_position()
}

fn scene_has(state: &crate::State, surface: Resolved<WlSurfaceId>) -> bool {
    state.build_scene_model().entries.iter().any(|entry| {
        entry.root_surface == surface && entry.role == crate::scene::SceneRole::UnmanagedRoot
    })
}

fn picked(state: &crate::State, at: (f64, f64)) -> Option<Resolved<WlSurfaceId>> {
    crate::input::pick::scene_pick_at(state, at)
        .surface_focus
        .map(|focus| focus.target.surface)
}

fn property_request(window: Window, property: Atom, ty: Atom) -> Vec<u8> {
    testing::encode(
        &GetProperty {
            delete: 0,
            window,
            property,
            r#type: ty,
            long_offset: 0,
            long_length: 1024,
        },
        0,
    )
}

fn open_with_read(
) -> (Wm, crate::State, Dispatcher, SurfaceSerials, Redispatcher<std::io::Result<ReadWriteCompleted>>)
{
    let (mut wm, mut state, mut dispatcher, surfaces) = drive_to_ready(None);
    let io = drain_io(&mut dispatcher);
    let read = io.reads.into_iter().next().expect("x11 read");
    let (_, dispatch_to) = io.writes.into_iter().next().expect("claim write");
    ack_write(&mut wm, &mut state, &mut dispatcher, &surfaces, dispatch_to);
    (wm, state, dispatcher, surfaces, read)
}

fn x_surface(state: &mut crate::State, raw: u32, serial: u64) -> Resolved<WlSurfaceId> {
    let mut surface = WlSurface::new(WlSurfaceId::from(raw), ClientId::from_raw(1), 1);
    assert!(surface.assign_xwayland_role());
    surface.test_set_xwayland_serial(serial);
    let (wayland_id, object_id) = state.insert_object(surface);
    Resolved::new(ClientId::from_raw(1), WlSurfaceId::from(wayland_id), object_id)
}

fn note_surface(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &SurfaceSerials,
    event: &SurfaceEvent,
) {
    let mut ctx = WmCtx { run: 1, state, dispatcher, surfaces };
    wm.on_surface_event(event, &mut ctx).unwrap();
}

fn configure_request(
    window: Window,
    mask: ConfigWindow,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
) -> Event {
    Event::ConfigureRequest(ConfigureRequest {
        send_event: false,
        sequence: 0,
        stack_mode: StackMode::Above,
        parent: Window(0x100),
        window,
        sibling: Window(0),
        x,
        y,
        width,
        height,
        border_width: 0,
        value_mask: mask,
    })
}

fn drive_to_ready(
    cursor: Option<crate::cursor::CursorImage>,
) -> (Wm, crate::State, Dispatcher, SurfaceSerials) {
    let fd = std::fs::File::open("/dev/null").unwrap();
    let mut state = state_for_tests();
    state.seat.test_block_cursor_icons(&["default", "left_ptr"]);
    if let Some(image) = cursor {
        state.seat.test_cache_cursor_icon("default", image);
    }
    let mut dispatcher = Dispatcher::default();
    let mut wm = Wm::start(fd.as_raw_fd(), 1, 1, &mut dispatcher);
    let surfaces = SurfaceSerials::default();
    let mut sequence = 1u16;
    let mut next_atom = ATOM_BASE;
    let mut setup_sent = false;
    for _ in 0..6 {
        if matches!(wm.phase, WmPhase::Ready(_)) {
            return (wm, state, dispatcher, surfaces);
        }
        let io = drain_io(&mut dispatcher);
        if !setup_sent {
            assert_eq!(io.writes.len(), 1);
            let (_, expected) = XConnection::new();
            assert_eq!(&*io.writes[0].0, expected.as_slice());
            setup_sent = true;
            ack_write(
                &mut wm,
                &mut state,
                &mut dispatcher,
                &surfaces,
                io.writes.into_iter().next().unwrap().1,
            );
            let read = io.reads.into_iter().next().expect("setup read");
            feed_read(
                &mut wm,
                &mut state,
                &mut dispatcher,
                &surfaces,
                read,
                testing::setup_bytes(Window(0x100), 0x20, 0x1f),
            );
            continue;
        }
        pump_events(&mut wm, &mut state, &mut dispatcher, &surfaces);
        if matches!(wm.phase, WmPhase::Ready(_)) {
            return (wm, state, dispatcher, surfaces);
        }
        assert_eq!(io.writes.len(), 1, "startup batch");
        let (buffer, dispatch_to) = io.writes.into_iter().next().unwrap();
        ack_write(&mut wm, &mut state, &mut dispatcher, &surfaces, dispatch_to);
        let replies = testing::reply_batch(&buffer, &mut sequence, &mut next_atom);
        let read = io.reads.into_iter().next().expect("startup read");
        feed_read(&mut wm, &mut state, &mut dispatcher, &surfaces, read, replies);
        pump_events(&mut wm, &mut state, &mut dispatcher, &surfaces);
    }
    panic!("startup did not become ready");
}

fn claim_requests(root: Window, check: Window, atoms: &crate::x11::Atoms) -> Vec<Vec<u8>> {
    let mask = EventMask::SubstructureNotify
        .union(EventMask::SubstructureRedirect)
        .union(EventMask::PropertyChange);
    let supported = [
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
    ];
    let name = change_property_utf8(check, atoms._NET_WM_NAME, atoms.UTF8_STRING, "tema");
    let on_root =
        change_property_u32s(root, atoms._NET_SUPPORTING_WM_CHECK, atoms.WINDOW, &[check.0]);
    let on_check =
        change_property_u32s(check, atoms._NET_SUPPORTING_WM_CHECK, atoms.WINDOW, &[check.0]);
    let advertised = change_property_atoms(root, atoms._NET_SUPPORTED, atoms.ATOM, &supported);
    let active = change_property_u32s(root, atoms._NET_ACTIVE_WINDOW, atoms.WINDOW, &[0]);
    vec![
        testing::encode(
            &ChangeWindowAttributes {
                window: root,
                value_list: ChangeWindowAttributesAux::default().event_mask(mask),
            },
            0,
        ),
        testing::encode(
            &CompositeRedirectSubwindows { window: root, update: Redirect::Manual },
            COMPOSITE_MAJOR_OPCODE,
        ),
        testing::encode(
            &CreateWindow {
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
            },
            0,
        ),
        testing::encode(&name.request(), 0),
        testing::encode(&on_root.request(), 0),
        testing::encode(&on_check.request(), 0),
        testing::encode(&SetSelectionOwner { owner: check, selection: atoms.WM_S0, time: 0 }, 0),
        testing::encode(
            &SetSelectionOwner { owner: check, selection: atoms._NET_WM_CM_S0, time: 0 },
            0,
        ),
        testing::encode(&advertised.request(), 0),
        testing::encode(&active.request(), 0),
    ]
}

fn cursor_requests(image: &CursorImage) -> Vec<Vec<u8>> {
    let pixmap = Pixmap(0x21);
    let gc = Gcontext(0x22);
    let picture = Picture(0x23);
    let cursor = Cursor(0x24);
    let root = Drawable(0x100);
    let pixels = super::cursor::argb_words(image);
    let width = u16::try_from(image.width).unwrap();
    let height = u16::try_from(image.height).unwrap();
    vec![
        testing::encode(&CreatePixmap { depth: 32, pid: pixmap, drawable: root, width, height }, 0),
        testing::encode(
            &CreateGc {
                cid: gc,
                drawable: Drawable(pixmap.0),
                value_list: CreateGcAux::default(),
            },
            0,
        ),
        testing::encode(
            &PutImage {
                format: ImageFormat::ZPixmap,
                drawable: Drawable(pixmap.0),
                gc,
                width,
                height,
                dst_x: 0,
                dst_y: 0,
                left_pad: 0,
                depth: 32,
                data: &pixels,
            },
            0,
        ),
        testing::encode(
            &RenderCreatePicture {
                pid: picture,
                drawable: Drawable(pixmap.0),
                format: Pictformat(0x51),
                value_list: RenderCreatePictureAux::default(),
            },
            RENDER_MAJOR_OPCODE,
        ),
        testing::encode(
            &RenderCreateCursor {
                cid: cursor,
                source: picture,
                x: u16::try_from(image.hotspot_x).unwrap(),
                y: u16::try_from(image.hotspot_y).unwrap(),
            },
            RENDER_MAJOR_OPCODE,
        ),
        testing::encode(
            &ChangeWindowAttributes {
                window: Window(0x100),
                value_list: ChangeWindowAttributesAux::default().cursor(cursor),
            },
            0,
        ),
        testing::encode(&RenderFreePicture { picture }, RENDER_MAJOR_OPCODE),
        testing::encode(&FreeGc { gc }, 0),
        testing::encode(&FreePixmap { pixmap }, 0),
    ]
}

fn argb_format() -> Pictforminfo {
    Pictforminfo {
        id: Pictformat(0x51),
        r#type: PictType::Direct,
        depth: 32,
        direct: Directformat {
            red_shift: 16,
            red_mask: 0xff,
            green_shift: 8,
            green_mask: 0xff,
            blue_shift: 0,
            blue_mask: 0xff,
            alpha_shift: 24,
            alpha_mask: 0xff,
        },
        colormap: Colormap(0),
    }
}

fn pump_events(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &SurfaceSerials,
) {
    for action in dispatcher.test_take_queued_actions::<XwaylandAction>() {
        apply(wm, state, dispatcher, surfaces, action).unwrap();
    }
}

fn ack_write(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &SurfaceSerials,
    dispatch_to: Redispatcher<std::io::Result<ReadWriteCompleted>>,
) {
    let action = complete(dispatcher, dispatch_to, Ok(completed(Box::new([]))));
    apply(wm, state, dispatcher, surfaces, action).unwrap();
}

fn feed_read(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &SurfaceSerials,
    dispatch_to: Redispatcher<std::io::Result<ReadWriteCompleted>>,
    bytes: Vec<u8>,
) {
    let action = complete(dispatcher, dispatch_to, Ok(completed(bytes.into())));
    apply(wm, state, dispatcher, surfaces, action).unwrap();
}

fn apply(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &SurfaceSerials,
    action: XwaylandAction,
) -> Result<(), super::WmFailure> {
    let XwaylandAction::Wm { event, .. } = action else { panic!("x11 completion is a wm event") };
    let mut ctx = WmCtx { run: 1, state, dispatcher, surfaces };
    wm.handle(event, &mut ctx)
}

fn complete(
    dispatcher: &mut Dispatcher,
    dispatch_to: Redispatcher<std::io::Result<ReadWriteCompleted>>,
    result: std::io::Result<ReadWriteCompleted>,
) -> XwaylandAction {
    dispatcher.redispatch_to(dispatch_to, result);
    let mut actions = dispatcher.test_take_queued_actions::<XwaylandAction>();
    assert_eq!(actions.len(), 1);
    actions.pop().unwrap()
}

struct PendingIo {
    reads: Vec<Redispatcher<std::io::Result<ReadWriteCompleted>>>,
    writes: Vec<(Box<[u8]>, Redispatcher<std::io::Result<ReadWriteCompleted>>)>,
}

fn drain_io(dispatcher: &mut Dispatcher) -> PendingIo {
    let mut reads = Vec::new();
    let mut writes = Vec::new();
    for action in dispatcher.test_take_queued_actions::<IoUringAction>() {
        match action {
            IoUringAction::ReadSome { dispatch_to, .. } => reads.push(dispatch_to),
            IoUringAction::Write { buffer, dispatch_to, .. } => writes.push((buffer, dispatch_to)),
            other => dispatcher.dispatch(other),
        }
    }
    PendingIo { reads, writes }
}

fn completed(buffer: Box<[u8]>) -> ReadWriteCompleted {
    ReadWriteCompleted { fd: 0, offset: 0, buffer }
}

fn wm_is_some(xwayland: &Xwayland) -> bool {
    matches!(xwayland.phase, Phase::Running(ref run) if run.wm.is_some())
}

fn running_with(pidfd: Rc<OwnedFd>) -> (Xwayland, crate::State, Dispatcher) {
    let mut xwayland = Xwayland::new();
    let mut state = state_for_tests();
    let mut dispatcher = Dispatcher::default();
    xwayland
        .process(&mut state, XwaylandAction::Start, &mut dispatcher)
        .unwrap();
    xwayland
        .process(
            &mut state,
            XwaylandAction::ListenReadable { result: Ok(crate::io_uring::ring::PollCompleted {}) },
            &mut dispatcher,
        )
        .unwrap();
    xwayland
        .process(
            &mut state,
            XwaylandAction::Process { run: 1, event: ProcessEvent::Spawned { pid: 42, pidfd } },
            &mut dispatcher,
        )
        .unwrap();
    (xwayland, state, dispatcher)
}

fn sleeping_child() -> (Rc<OwnedFd>, Reap) {
    let pid = match unsafe { nix::unistd::fork() }.expect("fork") {
        nix::unistd::ForkResult::Parent { child } => child,
        nix::unistd::ForkResult::Child => {
            nix::unistd::sleep(30);
            unsafe { libc::_exit(0) }
        }
    };
    let pidfd = crate::process::pidfd_open(pid.as_raw()).expect("pidfd_open");
    let reap = Reap(pidfd.try_clone().expect("dup pidfd"));
    (Rc::new(pidfd), reap)
}

fn pidfd_readable(pidfd: &OwnedFd, timeout_ms: u16) -> bool {
    use std::os::fd::AsFd;
    let mut polls = [nix::poll::PollFd::new(pidfd.as_fd(), nix::poll::PollFlags::POLLIN)];
    nix::poll::poll(&mut polls, timeout_ms).expect("poll") > 0
}

struct Reap(OwnedFd);

impl Drop for Reap {
    fn drop(&mut self) {
        let _ = crate::process::pidfd_send_signal(&self.0, libc::SIGKILL);
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.0.as_raw_fd()) };
        let _ = nix::sys::wait::waitid(
            nix::sys::wait::Id::PIDFd(borrowed),
            nix::sys::wait::WaitPidFlag::WEXITED,
        );
    }
}
