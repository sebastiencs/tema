use crate::{
    capture::{
        submit_toplevel_capture, sync_toplevel_outputs, test_capture_job, test_closed,
        test_deliver, test_dispatch, test_request, test_sent, test_sent_arg, test_sent_opcodes,
        test_toplevel_for_window, test_toplevel_is_open, toplevel_rendered_bounds, CaptureCursor,
    },
    clients::{Client, ClientId, Resolved},
    protocol_generated::{
        zwlr_foreign_toplevel_handle_v1::State as WlrToplevelState, ExtForeignToplevelHandleV1Id,
        ExtForeignToplevelImageCaptureSourceManagerV1Id, ExtForeignToplevelListV1Id,
        ExtImageCaptureSourceV1Id, ExtImageCopyCaptureFrameV1Id, ExtImageCopyCaptureManagerV1Id,
        ExtImageCopyCaptureSessionV1Id, WlBufferId, WlSeatId, WlSurfaceId,
        ZwlrForeignToplevelHandleV1Id, ZwlrForeignToplevelManagerV1Id,
    },
    region::RectI,
    runner::Dispatcher,
    wayland_objects::{
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
        ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
        ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
        wl_buffer::{NewWlBufferParams, WlBuffer},
        wl_seat::WlSeat,
        wl_surface::{test_commit_new_buffer, WlSurface},
        zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1,
        zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1,
    },
    x11::{
        testing, Atom, Atoms, DestroyNotify, Event, InputFocus, KillClient, MapNotify, Property,
        PropertyNotify, PropertyState, SetInputFocus, UnmapNotify, Window,
    },
    xwayland::{SurfaceEvent, XwaylandAction},
};

use super::{
    focus_tests::{focus_surface, show_menu},
    manage_tests::{
        add_output, atoms, cardinals, delete_window_request, hidden_workspace, manage, manage_with,
        net_state_request, pair_mapped, state_message, text, transient, wm_state_request,
    },
    props,
    tests::{
        created, deliver, deliver_property, expect_requests, handle_event, map_request,
        note_surface, reconfigure, serial_message, take_writes,
    },
    WmEvent,
};

#[test]
fn a_managed_window_is_announced_with_its_title_and_class() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
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

    let record = test_toplevel_for_window(&state, managed.window).expect("announced");
    assert_eq!(record.title, "Term");
    assert_eq!(record.app_id, "XTerm");
    assert!(test_toplevel_is_open(&state, record.key));
}

#[test]
fn a_window_paired_before_its_buffer_is_announced_on_commit() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut client = Client::new_xwayland_for_tests(ClientId::from_raw(1));
    let (surface, wl_surface) = client
        .insert_object(&mut state, WlSurface::new(WlSurfaceId::from(80), client.client_id(), 1));
    assert!(wl_surface.assign_xwayland_role());
    wl_surface.test_set_xwayland_serial(1);
    state.register_root_surface(surface);
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 30, 40, 10, 12, 0));
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::MapNotify(MapNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
            override_redirect: 0,
        }),
    );
    surfaces.apply(SurfaceEvent::SerialAssigned { serial: 1, surface });
    let serial = atoms(&wm).WL_SURFACE_SERIAL;
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(window, serial, 1));
    let ready = atoms(&wm);
    for (property, _) in props::loads(&ready) {
        deliver_property(
            &mut wm,
            &mut state,
            &mut dispatcher,
            &surfaces,
            window,
            property,
            Ok(blank()),
        );
    }
    let window_id = state.window_for_root(surface).expect("managed");
    assert!(test_toplevel_for_window(&state, window_id).is_none());

    test_commit_new_buffer(&mut client, &mut state, &mut dispatcher, surface, 10, 12);

    let record = test_toplevel_for_window(&state, window_id).expect("announced");
    assert_eq!(record.title, "");
    assert_eq!(record.app_id, "");
    assert!(test_toplevel_is_open(&state, record.key));
}

#[test]
fn an_override_redirect_window_is_absent_from_the_toplevel_list() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = Client::new_for_tests(ClientId::from_raw(9));
    ExtForeignToplevelListV1::attach(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ExtForeignToplevelListV1Id::from(50),
    );
    let surface =
        pair_mapped(&mut wm, &mut state, &mut dispatcher, &mut surfaces, Window(0x70), 90, 1, 1);

    test_deliver(&mut taskbar, &mut state, &mut dispatcher);

    assert!(state.window_for_root(surface).is_none());
    assert!(!test_sent(&dispatcher, 50, 0));
}

#[test]
fn unmapping_closes_the_record_and_the_next_map_gets_a_new_key() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let first = test_toplevel_for_window(&state, managed.window)
        .expect("announced")
        .key;

    deliver(
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
    assert!(!test_toplevel_is_open(&state, first));

    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_request(window));
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::MapNotify(MapNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window,
            override_redirect: 0,
        }),
    );
    let again = state
        .window_for_root(managed.surface)
        .expect("mapped again");
    let second = test_toplevel_for_window(&state, again)
        .expect("announced")
        .key;
    assert_ne!(second, first);
    assert!(test_toplevel_is_open(&state, second));
}

#[test]
fn destroy_stop_and_surface_destruction_close_the_record() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let destroyed = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "App",
    );
    let destroyed_key = test_toplevel_for_window(&state, destroyed.window)
        .expect("announced")
        .key;
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::DestroyNotify(DestroyNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window: Window(0x50),
        }),
    );
    assert!(!test_toplevel_is_open(&state, destroyed_key));

    let unpaired = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        "Two",
        "App",
    );
    let unpaired_key = test_toplevel_for_window(&state, unpaired.window)
        .expect("announced")
        .key;
    let event = SurfaceEvent::Destroyed { serial: 2, surface: unpaired.surface };
    surfaces.apply(event);
    note_surface(&mut wm, &mut state, &mut dispatcher, &surfaces, &event);
    assert!(!test_toplevel_is_open(&state, unpaired_key));

    let stopped = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x52),
        82,
        3,
        "Three",
        "App",
    );
    let stopped_key = test_toplevel_for_window(&state, stopped.window)
        .expect("announced")
        .key;
    wm.stop(&mut state, &mut dispatcher);
    assert!(!test_toplevel_is_open(&state, stopped_key));
}

#[test]
fn an_ext_list_receives_the_window_and_its_close() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = Client::new_for_tests(ClientId::from_raw(9));
    ExtForeignToplevelListV1::attach(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ExtForeignToplevelListV1Id::from(50),
    );
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);

    let record = test_toplevel_for_window(&state, managed.window).expect("announced");
    assert_eq!(record.ext_handles.len(), 1);
    let handle = record.ext_handles[0];
    let handle_id: u32 = handle.wayland_id().into();
    assert_eq!(handle.rref(&state).identifier(), record.key.identifier());
    assert_eq!(handle.rref(&state).title(), "Term");
    assert_eq!(test_sent_opcodes(&dispatcher, handle_id), vec![4, 2, 3, 1]);

    ExtForeignToplevelListV1::attach(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ExtForeignToplevelListV1Id::from(51),
    );
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);
    let again = test_toplevel_for_window(&state, managed.window).expect("still announced");
    assert_eq!(again.ext_handles.len(), 2);
    assert_eq!(again.ext_handles[1].rref(&state).identifier(), record.key.identifier());
    assert_eq!(again.ext_handles[1].rref(&state).key(), record.key);
    assert!(test_sent(&dispatcher, 51, 0));

    deliver(
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
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);
    assert!(handle.rref(&state).is_closed());
    assert!(again.ext_handles[1].rref(&state).is_closed());
    assert!(test_sent(&dispatcher, handle_id, 0));
    assert!(!test_toplevel_is_open(&state, record.key));
}

#[test]
fn a_title_change_reaches_both_toplevel_lists() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = taskbar(&mut state, &mut dispatcher);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);
    let record = test_toplevel_for_window(&state, managed.window).expect("announced");
    let ext_id: u32 = record.ext_handles[0].wayland_id().into();
    let wlr_id: u32 = record.wlr_handles[0].wayland_id().into();
    let ext_before = test_sent_opcodes(&dispatcher, ext_id);
    let wlr_before = test_sent_opcodes(&dispatcher, wlr_id);

    let ready = atoms(&wm);
    notify_property(&mut wm, &mut state, &mut dispatcher, &surfaces, window, ready._NET_WM_NAME);
    deliver_property(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        window,
        ready._NET_WM_NAME,
        Ok(text(ready.UTF8_STRING, b"New")),
    );
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);

    let record = test_toplevel_for_window(&state, managed.window).expect("announced");
    assert_eq!(record.title, "New");
    assert_eq!(opcodes_after(&dispatcher, ext_id, ext_before.len()), [2, 1]);
    assert_eq!(opcodes_after(&dispatcher, wlr_id, wlr_before.len()), [0, 5]);
}

#[test]
fn a_class_change_reaches_both_toplevel_lists() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = taskbar(&mut state, &mut dispatcher);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);
    let record = test_toplevel_for_window(&state, managed.window).expect("announced");
    let ext_id: u32 = record.ext_handles[0].wayland_id().into();
    let wlr_id: u32 = record.wlr_handles[0].wayland_id().into();
    let ext_before = test_sent_opcodes(&dispatcher, ext_id);
    let wlr_before = test_sent_opcodes(&dispatcher, wlr_id);

    let ready = atoms(&wm);
    notify_property(&mut wm, &mut state, &mut dispatcher, &surfaces, window, ready.WM_CLASS);
    deliver_property(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        window,
        ready.WM_CLASS,
        Ok(text(ready.UTF8_STRING, b"inst\0NewClass\0")),
    );
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);

    let record = test_toplevel_for_window(&state, managed.window).expect("announced");
    assert_eq!(record.app_id, "NewClass");
    assert_eq!(opcodes_after(&dispatcher, ext_id, ext_before.len()), [3, 1]);
    assert_eq!(opcodes_after(&dispatcher, wlr_id, wlr_before.len()), [1, 5]);
}

#[test]
fn an_unchanged_title_queues_nothing() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = taskbar(&mut state, &mut dispatcher);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);
    let ext_id: u32 = test_toplevel_for_window(&state, managed.window)
        .expect("announced")
        .ext_handles[0]
        .wayland_id()
        .into();
    let before = test_sent_opcodes(&dispatcher, ext_id);

    let ready = atoms(&wm);
    notify_property(&mut wm, &mut state, &mut dispatcher, &surfaces, window, ready._NET_WM_NAME);
    deliver_property(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        window,
        ready._NET_WM_NAME,
        Ok(text(ready.UTF8_STRING, b"Term")),
    );
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);

    assert_eq!(
        test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .title,
        "Term"
    );
    assert_eq!(test_sent_opcodes(&dispatcher, ext_id), before);
}

#[test]
fn a_dialogs_parent_follows_its_managed_parent() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = taskbar(&mut state, &mut dispatcher);
    let parent = Window(0x50);
    let parent_managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, parent, 80, 1, "Parent", "App");
    let child = Window(0x51);
    let child_managed = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        child,
        81,
        2,
        10,
        12,
        "Ask",
        "Dialog",
        |ready, property| (property == ready.WM_TRANSIENT_FOR).then(|| transient(parent)),
    );
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);

    let parent_record = test_toplevel_for_window(&state, parent_managed.window).expect("parent");
    let child_record = test_toplevel_for_window(&state, child_managed.window).expect("dialog");
    assert_eq!(child_record.parent, Some(parent_record.key));
    assert_eq!(
        state
            .toplevel(child_managed.window)
            .unwrap()
            .parent_window(),
        Some(parent_managed.window)
    );
    let child_wlr: u32 = child_record.wlr_handles[0].wayland_id().into();
    let parent_wlr: u32 = parent_record.wlr_handles[0].wayland_id().into();
    assert!(test_sent(&dispatcher, child_wlr, 7));
    assert!(test_sent_arg(&dispatcher, child_wlr, 7, parent_wlr));

    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::UnmapNotify(UnmapNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window: parent,
            from_configure: 0,
        }),
    );
    let child_record = test_toplevel_for_window(&state, child_managed.window).expect("dialog");
    assert_eq!(child_record.parent, None);
    assert_eq!(
        state
            .toplevel(child_managed.window)
            .unwrap()
            .parent_window(),
        None
    );
}

#[test]
fn a_dialog_managed_first_receives_its_parent_on_the_parents_map() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = taskbar(&mut state, &mut dispatcher);
    let parent = Window(0x60);
    let child = Window(0x61);
    let child_managed = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        child,
        81,
        1,
        10,
        12,
        "Ask",
        "Dialog",
        |ready, property| (property == ready.WM_TRANSIENT_FOR).then(|| transient(parent)),
    );
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);
    assert_eq!(
        test_toplevel_for_window(&state, child_managed.window)
            .expect("dialog")
            .parent,
        None
    );

    let parent_managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, parent, 80, 2, "Parent", "App");
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);

    let parent_key = test_toplevel_for_window(&state, parent_managed.window)
        .expect("parent")
        .key;
    let child_record = test_toplevel_for_window(&state, child_managed.window).expect("dialog");
    assert_eq!(child_record.parent, Some(parent_key));
    assert_eq!(
        state
            .toplevel(child_managed.window)
            .unwrap()
            .parent_window(),
        Some(parent_managed.window)
    );
    let child_wlr: u32 = child_record.wlr_handles[0].wayland_id().into();
    let parent_wlr: u32 = test_toplevel_for_window(&state, parent_managed.window)
        .expect("parent")
        .wlr_handles[0]
        .wayland_id()
        .into();
    assert!(test_sent(&dispatcher, child_wlr, 7));
    assert!(test_sent_arg(&dispatcher, child_wlr, 7, parent_wlr));
}

#[test]
fn fullscreen_at_map_is_published() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let managed = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        10,
        12,
        "Term",
        "XTerm",
        |ready, property| {
            (property == ready._NET_WM_STATE)
                .then(|| cardinals(&[ready._NET_WM_STATE_FULLSCREEN.0]))
        },
    );

    let record = test_toplevel_for_window(&state, managed.window).expect("announced");
    assert!(record.fullscreen);
    assert!(!record.maximized);
}

#[test]
fn net_wm_state_fullscreen_reaches_the_record_on_reconfigure() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let ready = atoms(&wm);
    let fullscreen = ready._NET_WM_STATE_FULLSCREEN;

    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready._NET_WM_STATE, [1, fullscreen.0, 0, 0, 0]),
    );
    assert!(state.window_is_fullscreen(managed.window));
    assert!(
        !test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .fullscreen
    );
    reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces);
    assert!(
        test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .fullscreen
    );

    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready._NET_WM_STATE, [0, fullscreen.0, 0, 0, 0]),
    );
    assert!(!state.window_is_fullscreen(managed.window));
    assert!(
        test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .fullscreen
    );
    reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces);
    assert!(
        !test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .fullscreen
    );
}

#[test]
fn a_compositor_fullscreen_reaches_the_record_on_reconfigure() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
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
    assert!(state.set_window_fullscreen(managed.window, true));
    assert!(
        !test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .fullscreen
    );

    reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces);
    assert!(
        test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .fullscreen
    );
    assert!(
        !test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .maximized
    );
}

#[test]
fn maximized_net_wm_state_leaves_the_record_unset() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let managed = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        10,
        12,
        "Term",
        "XTerm",
        |ready, property| {
            (property == ready._NET_WM_STATE).then(|| {
                cardinals(&[
                    ready._NET_WM_STATE_MAXIMIZED_VERT.0,
                    ready._NET_WM_STATE_MAXIMIZED_HORZ.0,
                ])
            })
        },
    );
    reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces);

    let record = test_toplevel_for_window(&state, managed.window).expect("announced");
    assert!(!record.maximized);
    assert!(!record.fullscreen);
}

#[test]
fn keyboard_focus_activates_the_x_window_and_a_wayland_surface_clears_it() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
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
    assert!(
        test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .activated
    );

    focus_surface(&mut state, &mut dispatcher, 70);
    assert!(
        !test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .activated
    );
}

#[test]
fn focus_on_an_override_redirect_menu_activates_nothing() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
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
    assert!(
        test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .activated
    );
    let menu = show_menu(&mut wm, &mut state, &mut dispatcher, &mut surfaces);

    crate::input::set_keyboard_focus(
        &mut state,
        &mut dispatcher,
        Some(crate::input::SurfaceFocus::new(menu)),
    );

    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(menu));
    assert!(
        !test_toplevel_for_window(&state, managed.window)
            .expect("announced")
            .activated
    );
}

#[test]
fn a_fullscreen_focused_window_sends_activated_and_fullscreen() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = Client::new_for_tests(ClientId::from_raw(9));
    ZwlrForeignToplevelManagerV1::attach(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ZwlrForeignToplevelManagerV1Id::from(90),
        3,
    );
    let managed = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        10,
        12,
        "Term",
        "XTerm",
        |ready, property| {
            (property == ready._NET_WM_STATE)
                .then(|| cardinals(&[ready._NET_WM_STATE_FULLSCREEN.0]))
        },
    );
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);

    let record = test_toplevel_for_window(&state, managed.window).expect("announced");
    assert!(record.activated);
    assert!(record.fullscreen);
    assert!(!record.maximized);
    let handle_id: u32 = record.wlr_handles[0].wayland_id().into();
    let expected = ZwlrForeignToplevelHandleV1::state_values(3, true, false, true);
    assert_eq!(
        expected,
        vec![WlrToplevelState::Activated as u32, WlrToplevelState::Fullscreen as u32]
    );
    assert!(sent_state_arrays(&dispatcher, handle_id).contains(&expected));
}

#[test]
fn output_membership_reaches_the_record() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    let output = add_output(&mut state);
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
    managed
        .surface
        .ref_mut(&mut state)
        .update_output_membership(vec![output]);
    sync_toplevel_outputs(&mut state, &mut dispatcher, managed.window);

    let record = test_toplevel_for_window(&state, managed.window).expect("announced");
    assert!(record.outputs.contains(&output));
}

#[test]
fn activate_focuses_the_x_window_and_sends_active_window_and_input_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = wlr_taskbar(&mut state, &mut dispatcher);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let (handle, handle_id) =
        delivered_handle(&mut taskbar, &mut state, &mut dispatcher, managed.window);
    focus_surface(&mut state, &mut dispatcher, 70);
    let _ = dispatcher.test_take_queued_actions::<XwaylandAction>();

    dispatch_activate(&mut taskbar, &mut state, &mut dispatcher, handle, handle_id);

    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(managed.surface));
    assert!(dispatcher.test_has_queued_action::<XwaylandAction>(|action| {
        matches!(action, XwaylandAction::KeyboardFocus)
    }));
    let sent =
        handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::KeyboardFocus);
    let ready = atoms(&wm);
    assert!(sent.contains(&active_window_request(&ready, window.0)));
    assert!(sent.contains(&input_focus_request(window)));
}

#[test]
fn activate_on_a_hidden_workspace_shows_that_workspace() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    let output = add_output(&mut state);
    let hidden = hidden_workspace(&state, output);
    let mut taskbar = wlr_taskbar(&mut state, &mut dispatcher);
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
    let (handle, handle_id) =
        delivered_handle(&mut taskbar, &mut state, &mut dispatcher, managed.window);
    assert!(state.assign_window_to_workspace(managed.window, hidden));
    assert_ne!(state.active_workspace_for_output(output), Some(hidden));
    focus_surface(&mut state, &mut dispatcher, 70);

    dispatch_activate(&mut taskbar, &mut state, &mut dispatcher, handle, handle_id);

    assert_eq!(state.active_workspace_for_output(output), Some(hidden));
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(managed.surface));
}

#[test]
fn activate_on_an_iconic_window_writes_normal() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = wlr_taskbar(&mut state, &mut dispatcher);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let (handle, handle_id) =
        delivered_handle(&mut taskbar, &mut state, &mut dispatcher, managed.window);
    focus_surface(&mut state, &mut dispatcher, 70);
    let ready = atoms(&wm);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready.WM_CHANGE_STATE, [3, 0, 0, 0, 0]),
    );
    assert!(wm.windows.get(&window).unwrap().props.hidden);

    dispatch_activate(&mut taskbar, &mut state, &mut dispatcher, handle, handle_id);
    let sent =
        handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::KeyboardFocus);

    assert!(!wm.windows.get(&window).unwrap().props.hidden);
    assert!(sent.contains(&wm_state_request(window, ready.WM_STATE, 1)));
    assert!(sent.contains(&active_window_request(&ready, window.0)));
    assert!(sent.contains(&input_focus_request(window)));
}

#[test]
fn close_sends_wm_delete_window_or_kill_client() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = wlr_taskbar(&mut state, &mut dispatcher);
    let polite = Window(0x50);
    let polite_managed = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        polite,
        80,
        1,
        10,
        12,
        "Term",
        "XTerm",
        |ready, property| {
            (property == ready.WM_PROTOCOLS).then(|| cardinals(&[ready.WM_DELETE_WINDOW.0]))
        },
    );
    let (polite_handle, polite_id) =
        delivered_handle(&mut taskbar, &mut state, &mut dispatcher, polite_managed.window);
    let _ = dispatcher.test_take_queued_actions::<XwaylandAction>();

    dispatch_handle(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        polite_handle,
        test_request(polite_id, 5, 0),
    );
    let queued = dispatcher.test_take_queued_actions::<XwaylandAction>();
    assert!(queued.iter().any(|action| {
        matches!(
            action,
            XwaylandAction::Wm { run: 1, event: WmEvent::Close { window } } if *window == polite
        )
    }));
    let ready = atoms(&wm);
    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::Close { window: polite },
    );
    expect_requests(
        sent,
        vec![delete_window_request(polite, ready.WM_PROTOCOLS, ready.WM_DELETE_WINDOW)],
    );

    let bare = Window(0x51);
    let bare_managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, bare, 81, 2, "Term", "XTerm");
    let (bare_handle, bare_id) =
        delivered_handle(&mut taskbar, &mut state, &mut dispatcher, bare_managed.window);
    dispatch_handle(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        bare_handle,
        test_request(bare_id, 5, 0),
    );
    let queued = dispatcher.test_take_queued_actions::<XwaylandAction>();
    assert!(queued.iter().any(|action| {
        matches!(
            action,
            XwaylandAction::Wm { run: 1, event: WmEvent::Close { window } } if *window == bare
        )
    }));
    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::Close { window: bare },
    );
    expect_requests(sent, vec![testing::encode(&KillClient { resource: bare.0 }, 0)]);
}

#[test]
fn set_fullscreen_changes_the_layout_and_not_net_wm_state() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = wlr_taskbar(&mut state, &mut dispatcher);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let (handle, handle_id) =
        delivered_handle(&mut taskbar, &mut state, &mut dispatcher, managed.window);
    let ready = atoms(&wm);

    let mut fullscreen = test_request(handle_id, 8, 1);
    fullscreen.id(0u32);
    dispatch_handle(&mut taskbar, &mut state, &mut dispatcher, handle, fullscreen);
    assert!(state.window_is_fullscreen(managed.window));
    assert!(take_writes(&mut wm, &mut state, &mut dispatcher, &surfaces).is_empty());
    let sent = reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces);
    assert!(sent.iter().all(|request| {
        *request != net_state_request(window, &ready, &[ready._NET_WM_STATE_FULLSCREEN])
    }));
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);
    let states = sent_state_arrays(&dispatcher, handle_id);
    assert!(states
        .last()
        .is_some_and(|state| state.contains(&(WlrToplevelState::Fullscreen as u32))));

    dispatch_handle(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        handle,
        test_request(handle_id, 9, 0),
    );
    assert!(!state.window_is_fullscreen(managed.window));
    assert!(take_writes(&mut wm, &mut state, &mut dispatcher, &surfaces).is_empty());
    let sent = reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces);
    assert!(sent.iter().all(|request| {
        *request != net_state_request(window, &ready, &[ready._NET_WM_STATE_FULLSCREEN])
    }));
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);
    let states = sent_state_arrays(&dispatcher, handle_id);
    assert!(states
        .last()
        .is_some_and(|state| !state.contains(&(WlrToplevelState::Fullscreen as u32))));
}

#[test]
fn a_request_on_an_unmapped_window_does_nothing() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = wlr_taskbar(&mut state, &mut dispatcher);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let (handle, handle_id) =
        delivered_handle(&mut taskbar, &mut state, &mut dispatcher, managed.window);
    deliver(
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
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);
    let focus = state.seat.keyboard_focus;
    let _ = dispatcher.test_take_queued_actions::<XwaylandAction>();

    dispatch_activate(&mut taskbar, &mut state, &mut dispatcher, handle, handle_id);
    dispatch_handle(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        handle,
        test_request(handle_id, 5, 0),
    );
    let mut fullscreen = test_request(handle_id, 8, 1);
    fullscreen.id(0u32);
    dispatch_handle(&mut taskbar, &mut state, &mut dispatcher, handle, fullscreen);

    assert_eq!(state.seat.keyboard_focus, focus);
    assert!(dispatcher
        .test_take_queued_actions::<XwaylandAction>()
        .is_empty());
    assert!(take_writes(&mut wm, &mut state, &mut dispatcher, &surfaces).is_empty());
    assert!(!test_closed(&dispatcher, &taskbar));
}

#[test]
fn create_source_on_an_x_handle_stays_connected() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = ext_taskbar(&mut state, &mut dispatcher);
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
    let (handle, _) =
        delivered_ext_handle(&mut taskbar, &mut state, &mut dispatcher, managed.window);
    taskbar.insert_object(
        &mut state,
        ExtForeignToplevelImageCaptureSourceManagerV1::new(
            ExtForeignToplevelImageCaptureSourceManagerV1Id::from(51),
        ),
    );

    let mut event = test_request(51, 0, 2);
    event.id(ExtImageCaptureSourceV1Id::from(52));
    event.id(handle.wayland_id());
    test_dispatch(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ExtForeignToplevelImageCaptureSourceManagerV1Id::from(51),
        event,
    );

    assert!(taskbar
        .resolve(ExtImageCaptureSourceV1Id::from(52), &state)
        .is_some());
    assert!(!test_closed(&dispatcher, &taskbar));
    assert!(!handle.rref(&state).is_closed());
}

#[test]
fn rendered_bounds_cover_the_scene_and_the_server_side_decoration() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
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
    let key = test_toplevel_for_window(&state, managed.window)
        .expect("announced")
        .key;
    state.config.decorations.shadow.radius = 12;
    let scene = state.surface_scene_bounds(managed.surface).expect("scene");
    let influence = state
        .root_surface_visual_state(managed.surface)
        .expect("decoration")
        .influence_bounds();
    let rendered = toplevel_rendered_bounds(&state, key).expect("rendered");

    assert_eq!(rendered, union_rect(scene, influence));
    assert!(rect_covers(rendered, scene));
    assert!(rect_covers(rendered, influence));
    assert!(rendered.width > scene.width || rendered.height > scene.height);
}

#[test]
fn submit_toplevel_capture_queues_the_rendered_size() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = Client::new_for_tests(ClientId::from_raw(9));
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
    let key = test_toplevel_for_window(&state, managed.window)
        .expect("announced")
        .key;
    let bounds = toplevel_rendered_bounds(&state, key).expect("rendered");
    let buffer = shm_buffer(&mut taskbar, &mut state, 71, bounds.width, bounds.height);

    let job = submit_toplevel_capture(
        &mut state,
        &mut dispatcher,
        key,
        buffer,
        CaptureCursor::Hidden,
        false,
    )
    .expect("queued");
    let job = test_capture_job(&state, job).expect("job");
    assert!(job.queued);
    assert_eq!((job.width, job.height), (bounds.width, bounds.height));
}

#[test]
fn unmap_fails_the_queued_capture_and_rejects_a_new_source() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let mut taskbar = ext_taskbar(&mut state, &mut dispatcher);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let (handle, _) =
        delivered_ext_handle(&mut taskbar, &mut state, &mut dispatcher, managed.window);
    let key = test_toplevel_for_window(&state, managed.window)
        .expect("announced")
        .key;
    let bounds = toplevel_rendered_bounds(&state, key).expect("rendered");
    taskbar.insert_object(
        &mut state,
        ExtForeignToplevelImageCaptureSourceManagerV1::new(
            ExtForeignToplevelImageCaptureSourceManagerV1Id::from(51),
        ),
    );
    let mut event = test_request(51, 0, 2);
    event.id(ExtImageCaptureSourceV1Id::from(52));
    event.id(handle.wayland_id());
    test_dispatch(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ExtForeignToplevelImageCaptureSourceManagerV1Id::from(51),
        event,
    );
    taskbar.insert_object(
        &mut state,
        ExtImageCopyCaptureManagerV1::new(ExtImageCopyCaptureManagerV1Id::from(53)),
    );
    let mut event = test_request(53, 0, 3);
    event.id(ExtImageCopyCaptureSessionV1Id::from(54));
    event.id(ExtImageCaptureSourceV1Id::from(52));
    event.uint(0);
    test_dispatch(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ExtImageCopyCaptureManagerV1Id::from(53),
        event,
    );
    assert_eq!(
        taskbar
            .resolve(ExtImageCopyCaptureSessionV1Id::from(54), &state)
            .expect("session")
            .rref(&state)
            .advertised_size(),
        Some((bounds.width, bounds.height))
    );
    let buffer = shm_buffer(&mut taskbar, &mut state, 71, bounds.width, bounds.height);
    let mut event = test_request(54, 0, 1);
    event.id(ExtImageCopyCaptureFrameV1Id::from(55));
    test_dispatch(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ExtImageCopyCaptureSessionV1Id::from(54),
        event,
    );
    let mut attach = test_request(55, 1, 1);
    attach.id(buffer.wayland_id());
    test_dispatch(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ExtImageCopyCaptureFrameV1Id::from(55),
        attach,
    );
    test_dispatch(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ExtImageCopyCaptureFrameV1Id::from(55),
        test_request(55, 3, 0),
    );
    assert!(!test_sent(&dispatcher, 55, 4));
    let second = shm_buffer(&mut taskbar, &mut state, 72, bounds.width, bounds.height);
    let job = submit_toplevel_capture(
        &mut state,
        &mut dispatcher,
        key,
        second,
        CaptureCursor::Hidden,
        false,
    )
    .expect("queued");
    assert!(test_capture_job(&state, job).expect("job").queued);

    deliver(
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
    assert!(test_capture_job(&state, job).expect("job").failed);
    test_deliver(&mut taskbar, &mut state, &mut dispatcher);
    assert!(handle.rref(&state).is_closed());
    assert!(test_sent(&dispatcher, 54, 5));
    assert!(test_sent_arg(&dispatcher, 55, 4, 2));

    let mut event = test_request(51, 0, 2);
    event.id(ExtImageCaptureSourceV1Id::from(56));
    event.id(handle.wayland_id());
    test_dispatch(
        &mut taskbar,
        &mut state,
        &mut dispatcher,
        ExtForeignToplevelImageCaptureSourceManagerV1Id::from(51),
        event,
    );
    assert!(test_closed(&dispatcher, &taskbar));
}

#[test]
fn an_override_redirect_menu_leaves_capture_bounds_unchanged() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
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
    let key = test_toplevel_for_window(&state, managed.window)
        .expect("announced")
        .key;
    let before = toplevel_rendered_bounds(&state, key).expect("rendered");

    show_menu(&mut wm, &mut state, &mut dispatcher, &mut surfaces);

    assert_eq!(toplevel_rendered_bounds(&state, key), Some(before));
}

fn opcodes_after(dispatcher: &Dispatcher, object: u32, already_sent: usize) -> Vec<u32> {
    test_sent_opcodes(dispatcher, object).split_off(already_sent)
}

fn sent_state_arrays(dispatcher: &Dispatcher, object: u32) -> Vec<Vec<u32>> {
    use crate::io_uring::model::IoUringAction;
    let mut arrays = Vec::new();
    dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
        let IoUringAction::Sendmsg { buffer, .. } = action else {
            return;
        };
        if buffer.len() < 12
            || u32::from_ne_bytes(buffer[0..4].try_into().unwrap()) != object
            || u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff != 4
        {
            return;
        }
        let len = u32::from_ne_bytes(buffer[8..12].try_into().unwrap()) as usize;
        let mut values = Vec::new();
        let mut offset = 12;
        while offset + 4 <= 12 + len && offset + 4 <= buffer.len() {
            values.push(u32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap()));
            offset += 4;
        }
        arrays.push(values);
    });
    arrays
}

fn ext_taskbar(state: &mut crate::State, dispatcher: &mut Dispatcher) -> Client {
    let mut taskbar = Client::new_for_tests(ClientId::from_raw(9));
    ExtForeignToplevelListV1::attach(
        &mut taskbar,
        state,
        dispatcher,
        ExtForeignToplevelListV1Id::from(50),
    );
    taskbar
}

fn delivered_ext_handle(
    taskbar: &mut Client,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    window: crate::window::WindowId,
) -> (Resolved<ExtForeignToplevelHandleV1Id>, u32) {
    test_deliver(taskbar, state, dispatcher);
    let handle = test_toplevel_for_window(state, window)
        .expect("announced")
        .ext_handles[0];
    let handle_id: u32 = handle.wayland_id().into();
    (handle, handle_id)
}

fn shm_buffer(
    client: &mut Client,
    state: &mut crate::State,
    id: u32,
    width: i32,
    height: i32,
) -> Resolved<WlBufferId> {
    let stride = width.saturating_mul(4);
    let bytes = vec![0u8; stride.saturating_mul(height) as usize];
    client
        .insert_object(
            state,
            WlBuffer::new(NewWlBufferParams {
                id: WlBufferId::from(id),
                offset: 0,
                width,
                height,
                stride,
                format: &crate::drm::ARGB8888,
                mem: crate::wayland_objects::wl_shm_pool::shared_memory_from_bytes(&bytes),
            })
            .expect("buffer"),
        )
        .0
}

fn union_rect(left: RectI, right: RectI) -> RectI {
    RectI::from_extents(
        left.x.min(right.x),
        left.y.min(right.y),
        left.x2().max(right.x2()),
        left.y2().max(right.y2()),
    )
    .expect("union")
}

fn rect_covers(outer: RectI, inner: RectI) -> bool {
    outer.x <= inner.x && outer.y <= inner.y && outer.x2() >= inner.x2() && outer.y2() >= inner.y2()
}

fn wlr_taskbar(state: &mut crate::State, dispatcher: &mut Dispatcher) -> Client {
    let mut taskbar = Client::new_for_tests(ClientId::from_raw(9));
    ZwlrForeignToplevelManagerV1::attach(
        &mut taskbar,
        state,
        dispatcher,
        ZwlrForeignToplevelManagerV1Id::from(90),
        3,
    );
    taskbar.insert_object(state, WlSeat::new(WlSeatId::from(91)));
    taskbar
}

fn delivered_handle(
    taskbar: &mut Client,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    window: crate::window::WindowId,
) -> (Resolved<ZwlrForeignToplevelHandleV1Id>, u32) {
    test_deliver(taskbar, state, dispatcher);
    let handle = test_toplevel_for_window(state, window)
        .expect("announced")
        .wlr_handles[0];
    let handle_id: u32 = handle.wayland_id().into();
    (handle, handle_id)
}

fn dispatch_handle(
    client: &mut Client,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    handle: Resolved<ZwlrForeignToplevelHandleV1Id>,
    event: crate::clients::RawEvent,
) {
    test_dispatch(client, state, dispatcher, handle.wayland_id(), event);
}

fn dispatch_activate(
    client: &mut Client,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    handle: Resolved<ZwlrForeignToplevelHandleV1Id>,
    handle_id: u32,
) {
    let mut activate = test_request(handle_id, 4, 1);
    activate.id(WlSeatId::from(91));
    dispatch_handle(client, state, dispatcher, handle, activate);
}

fn active_window_request(atoms: &Atoms, window: u32) -> Vec<u8> {
    testing::encode(
        &crate::x11::change_property_u32s(
            Window(0x100),
            atoms._NET_ACTIVE_WINDOW,
            atoms.WINDOW,
            &[window],
        )
        .request(),
        0,
    )
}

fn input_focus_request(window: Window) -> Vec<u8> {
    testing::encode(
        &SetInputFocus { revert_to: InputFocus::PointerRoot, focus: window, time: 0 },
        0,
    )
}

fn taskbar(state: &mut crate::State, dispatcher: &mut Dispatcher) -> Client {
    let mut taskbar = Client::new_for_tests(ClientId::from_raw(9));
    ExtForeignToplevelListV1::attach(
        &mut taskbar,
        state,
        dispatcher,
        ExtForeignToplevelListV1Id::from(50),
    );
    ZwlrForeignToplevelManagerV1::attach(
        &mut taskbar,
        state,
        dispatcher,
        ZwlrForeignToplevelManagerV1Id::from(90),
        3,
    );
    taskbar
}

fn notify_property(
    wm: &mut super::Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    window: Window,
    atom: Atom,
) {
    deliver(
        wm,
        state,
        dispatcher,
        surfaces,
        Event::PropertyNotify(PropertyNotify {
            send_event: false,
            sequence: 0,
            window,
            atom,
            time: 0,
            state: PropertyState::NewValue,
        }),
    );
}

fn blank() -> Property {
    Property {
        r#type: Atom(0),
        format: 0,
        data: Vec::new(),
        truncated: false,
    }
}
