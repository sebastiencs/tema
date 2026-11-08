use crate::{
    clients::{Client, ClientId, Resolved},
    input::SurfaceFocus,
    protocol_generated::WlSurfaceId,
    runner::Dispatcher,
    test_support::state_for_tests,
    wayland_objects::wl_surface::{test_commit_new_buffer, WlSurface},
    window::Toplevel,
    x11::{
        testing, Atom, Atoms, ConfigureWindow, ConfigureWindowAux, DestroyNotify, Event, FocusIn,
        InputFocus, MapNotify, NotifyDetail, NotifyMode, Property, SetInputFocus, StackMode,
        UnmapNotify, Window,
    },
    xwayland::{SurfaceEvent, XwaylandAction},
};

use super::{
    manage_tests::{
        add_output, atoms, cardinals, delete_window_request, hidden_workspace, manage, manage_with,
        net_state_request, state_message, window_list, wm_state_request, ManagedWindow,
    },
    props,
    tests::{
        buffered_x_root, created, deliver, deliver_property, expect_requests, handle_event,
        map_request, root_position, serial_message, set_pid,
    },
    Wm, WmEvent,
};

pub(super) fn focus_root(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    surface: Resolved<WlSurfaceId>,
) -> Vec<Vec<u8>> {
    state.seat.keyboard_focus = Some(SurfaceFocus::new(surface));
    handle_event(wm, state, dispatcher, surfaces, WmEvent::KeyboardFocus)
}

fn active_window(atoms: &Atoms, window: u32) -> Vec<u8> {
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

fn input_focus(window: Window) -> Vec<u8> {
    testing::encode(
        &SetInputFocus { revert_to: InputFocus::PointerRoot, focus: window, time: 0 },
        0,
    )
}

fn restack(window: Window, below: Option<Window>) -> Vec<u8> {
    let value_list = if let Some(sibling) = below {
        ConfigureWindowAux::default()
            .sibling(sibling)
            .stack_mode(StackMode::Above)
    } else {
        ConfigureWindowAux::default().stack_mode(StackMode::Above)
    };
    testing::encode(&ConfigureWindow { window, value_list }, 0)
}

fn managed(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &mut crate::xwayland::surfaces::SurfaceSerials,
    window: Window,
    raw: u32,
    serial: u64,
    hints: impl Fn(&Atoms, Atom) -> Option<crate::x11::Property>,
) -> ManagedWindow {
    manage_with(
        wm, state, dispatcher, surfaces, window, raw, serial, 10, 12, "Term", "XTerm", hints,
    )
}

fn stack(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    window: Window,
) {
    deliver(wm, state, dispatcher, surfaces, map_request(window));
}

#[test]
fn a_passive_window_is_focused_and_raised() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let below = Window(0x50);
    let window = Window(0x51);
    managed(&mut wm, &mut state, &mut dispatcher, &mut surfaces, below, 80, 1, |_, _| None);
    let focused =
        managed(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 81, 2, |_, _| None);
    stack(&mut wm, &mut state, &mut dispatcher, &surfaces, below);
    stack(&mut wm, &mut state, &mut dispatcher, &surfaces, window);
    let ready = atoms(&wm);

    let sent = focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, focused.surface);

    assert_eq!(wm.focus, Some(window));
    expect_requests(
        sent,
        vec![
            active_window(&ready, window.0),
            input_focus(window),
            net_state_request(window, &ready, &[ready._NET_WM_STATE_FOCUSED]),
            restack(window, Some(below)),
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[below, window]),
        ],
    );
}

#[test]
fn a_locally_active_window_gets_set_input_focus_then_wm_take_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let focused = managed(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        window,
        80,
        1,
        |ready, property| {
            (property == ready.WM_PROTOCOLS).then(|| cardinals(&[ready.WM_TAKE_FOCUS.0]))
        },
    );
    let ready = atoms(&wm);

    let sent = focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, focused.surface);

    expect_requests(
        sent,
        vec![
            active_window(&ready, window.0),
            input_focus(window),
            delete_window_request(window, ready.WM_PROTOCOLS, ready.WM_TAKE_FOCUS),
            net_state_request(window, &ready, &[ready._NET_WM_STATE_FOCUSED]),
            restack(window, None),
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[window]),
        ],
    );
}

#[test]
fn a_globally_active_window_gets_only_wm_take_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let focused = managed(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        window,
        80,
        1,
        |ready, property| {
            if property == ready.WM_PROTOCOLS {
                Some(cardinals(&[ready.WM_TAKE_FOCUS.0]))
            } else if property == ready.WM_HINTS {
                Some(cardinals(&[1, 0]))
            } else {
                None
            }
        },
    );
    let ready = atoms(&wm);

    let sent = focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, focused.surface);

    expect_requests(
        sent,
        vec![
            active_window(&ready, window.0),
            delete_window_request(window, ready.WM_PROTOCOLS, ready.WM_TAKE_FOCUS),
            net_state_request(window, &ready, &[ready._NET_WM_STATE_FOCUSED]),
            restack(window, None),
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[window]),
        ],
    );
}

#[test]
fn a_window_with_no_input_is_activated_without_focus_requests() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let focused = managed(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        window,
        80,
        1,
        |ready, property| (property == ready.WM_HINTS).then(|| cardinals(&[1, 0])),
    );
    let ready = atoms(&wm);

    let sent = focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, focused.surface);

    expect_requests(
        sent,
        vec![
            active_window(&ready, window.0),
            net_state_request(window, &ready, &[ready._NET_WM_STATE_FOCUSED]),
            restack(window, None),
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[window]),
        ],
    );
}

#[test]
fn moving_focus_drops_focused_from_the_previous_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = Window(0x50);
    let second = Window(0x51);
    let older =
        managed(&mut wm, &mut state, &mut dispatcher, &mut surfaces, first, 80, 1, |_, _| None);
    let newer =
        managed(&mut wm, &mut state, &mut dispatcher, &mut surfaces, second, 81, 2, |_, _| None);
    stack(&mut wm, &mut state, &mut dispatcher, &surfaces, first);
    stack(&mut wm, &mut state, &mut dispatcher, &surfaces, second);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, older.surface);
    let ready = atoms(&wm);

    let sent = focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, newer.surface);

    assert_eq!(wm.focus, Some(second));
    expect_requests(
        sent,
        vec![
            active_window(&ready, second.0),
            net_state_request(first, &ready, &[]),
            input_focus(second),
            net_state_request(second, &ready, &[ready._NET_WM_STATE_FOCUSED]),
            restack(second, Some(first)),
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[first, second]),
        ],
    );
}

#[test]
fn leaving_x_clears_the_active_window_and_sends_focus_none() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let focused =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, focused.surface);
    let ready = atoms(&wm);

    state.seat.keyboard_focus = None;
    let sent =
        handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::KeyboardFocus);
    expect_requests(
        sent,
        vec![
            active_window(&ready, 0),
            net_state_request(window, &ready, &[]),
            input_focus(Window::None),
        ],
    );
    assert!(wm.focus.is_none());

    let sent =
        handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::KeyboardFocus);
    assert!(sent.is_empty());

    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, focused.surface);
    let plain = WlSurface::new(WlSurfaceId::from(90), ClientId::from_raw(1), 1);
    let (wayland_id, object_id) = state.insert_object(plain);
    let plain = Resolved::new(ClientId::from_raw(1), WlSurfaceId::from(wayland_id), object_id);
    state.seat.keyboard_focus = Some(SurfaceFocus::new(plain));
    let sent =
        handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::KeyboardFocus);
    expect_requests(
        sent,
        vec![
            active_window(&ready, 0),
            net_state_request(window, &ready, &[]),
            input_focus(Window::None),
        ],
    );
}

#[test]
fn an_override_redirect_root_does_not_change_x_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let focused =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, focused.surface);

    let menu = Window(0x51);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(menu, 1, 2, 10, 12, 0));
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::MapNotify(MapNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window: menu,
            override_redirect: 1,
        }),
    );
    let menu_surface = buffered_x_root(&mut state, 81, 2, 10, 12);
    surfaces.apply(SurfaceEvent::SerialAssigned { serial: 2, surface: menu_surface });
    let serial = atoms(&wm).WL_SURFACE_SERIAL;
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, serial_message(menu, serial, 2));

    let sent = focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, menu_surface);
    assert!(sent.is_empty());
    assert_eq!(wm.focus, Some(window));
}

#[test]
fn activating_an_iconic_window_writes_normal_and_clears_hidden() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let focused =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    wm.windows.get_mut(&window).unwrap().props.hidden = true;
    let ready = atoms(&wm);

    let sent = focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, focused.surface);

    assert!(!wm.windows.get(&window).unwrap().props.hidden);
    expect_requests(
        sent,
        vec![
            wm_state_request(window, ready.WM_STATE, 1),
            active_window(&ready, window.0),
            input_focus(window),
            net_state_request(window, &ready, &[ready._NET_WM_STATE_FOCUSED]),
            restack(window, None),
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[window]),
        ],
    );
}

#[test]
fn destroying_the_focused_window_clears_x_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let focused =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, focused.surface);
    let ready = atoms(&wm);

    let sent = deliver(
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

    assert!(wm.focus.is_none());
    expect_requests(
        sent,
        vec![
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[]),
            active_window(&ready, 0),
            input_focus(Window::None),
        ],
    );
}

#[test]
fn set_keyboard_focus_queues_keyboard_focus() {
    let mut state = state_for_tests();
    let mut dispatcher = Dispatcher::default();
    let (wayland_id, object_id) =
        state.insert_object(WlSurface::new(WlSurfaceId::from(4), ClientId::from_raw(1), 1));
    let surface = Resolved::new(ClientId::from_raw(1), WlSurfaceId::from(wayland_id), object_id);
    crate::input::set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(surface)));
    assert!(matches!(
        dispatcher
            .test_take_queued_actions::<XwaylandAction>()
            .as_slice(),
        [XwaylandAction::KeyboardFocus]
    ));

    crate::input::set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(surface)));
    assert!(dispatcher
        .test_take_queued_actions::<XwaylandAction>()
        .is_empty());
}

#[test]
fn a_mapped_x_window_takes_focus_when_the_buffer_is_already_committed() {
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
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(managed.surface));
}

#[test]
fn a_mapped_x_window_takes_focus_when_the_buffer_arrives_after_pairing() {
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
            Ok(Property {
                r#type: Atom(0),
                format: 0,
                data: Vec::new(),
                truncated: false,
            }),
        );
    }
    assert!(state.seat.keyboard_focus.is_none());

    test_commit_new_buffer(&mut client, &mut state, &mut dispatcher, surface, 10, 12);
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(surface));
}

#[test]
fn a_new_x_window_does_not_take_focus_from_another_workspace() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    let output = add_output(&mut state);
    let hidden = hidden_workspace(&state, output);
    let (wayland_id, object_id) =
        state.insert_object(WlSurface::new(WlSurfaceId::from(70), ClientId::from_raw(1), 1));
    let other = Resolved::new(ClientId::from_raw(1), WlSurfaceId::from(wayland_id), object_id);
    state.register_root_surface(other);
    state.assign_root_surface_to_workspace(other, hidden);
    crate::input::set_keyboard_focus(&mut state, &mut dispatcher, Some(SurfaceFocus::new(other)));

    manage(
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
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(other));
}

#[test]
fn unmapping_the_focused_x_window_focuses_the_remaining_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    let second = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        "Two",
        "XTerm",
    );
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(second.surface));
    handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::KeyboardFocus);
    let ready = atoms(&wm);

    deliver(
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
    );
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(first.surface));

    let sent =
        handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::KeyboardFocus);
    expect_requests(
        sent,
        vec![
            active_window(&ready, Window(0x50).0),
            net_state_request(Window(0x51), &ready, &[]),
            input_focus(Window(0x50)),
            net_state_request(Window(0x50), &ready, &[ready._NET_WM_STATE_FOCUSED]),
            restack(Window(0x50), Some(Window(0x51))),
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[Window(0x51), Window(0x50)]),
        ],
    );
}

#[test]
fn unlock_restores_focus_to_a_managed_x_window() {
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
    let saved = state.seat.keyboard_focus;
    state.seat.keyboard_focus = None;

    crate::input::focus::restore_keyboard_focus_after_unlock(&mut state, &mut dispatcher, saved);
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(managed.surface));
}

fn press(state: &mut crate::State, dispatcher: &mut Dispatcher, surface: Resolved<WlSurfaceId>) {
    let (x, y) = root_position(state, surface);
    state.seat.pointer_pos = (f64::from(x) + 1.0, f64::from(y) + 1.0);
    crate::seat::handle_pointer_button(state, dispatcher, 0x110, 1, 100);
}

pub(super) fn show_menu(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &mut crate::xwayland::surfaces::SurfaceSerials,
) -> Resolved<WlSurfaceId> {
    let menu = Window(0x70);
    deliver(wm, state, dispatcher, surfaces, created(menu, 200, 40, 10, 12, 0));
    deliver(
        wm,
        state,
        dispatcher,
        surfaces,
        Event::MapNotify(MapNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window: menu,
            override_redirect: 1,
        }),
    );
    let surface = buffered_x_root(state, 90, 4, 10, 12);
    surfaces.apply(SurfaceEvent::SerialAssigned { serial: 4, surface });
    let serial = atoms(wm).WL_SURFACE_SERIAL;
    deliver(wm, state, dispatcher, surfaces, serial_message(menu, serial, 4));
    surface
}

pub(super) fn focus_surface(
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    raw: u32,
) -> Resolved<WlSurfaceId> {
    let (wayland_id, object_id) =
        state.insert_object(WlSurface::new(WlSurfaceId::from(raw), ClientId::from_raw(1), 1));
    let surface = Resolved::new(ClientId::from_raw(1), WlSurfaceId::from(wayland_id), object_id);
    crate::input::set_keyboard_focus(state, dispatcher, Some(SurfaceFocus::new(surface)));
    surface
}

#[test]
fn a_press_on_an_override_redirect_root_keeps_x_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let focused = manage(
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
    let menu = show_menu(&mut wm, &mut state, &mut dispatcher, &mut surfaces);

    press(&mut state, &mut dispatcher, menu);
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(focused.surface));
}

#[test]
fn a_press_on_an_override_redirect_root_focuses_it_from_a_wayland_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    manage(
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
    let menu = show_menu(&mut wm, &mut state, &mut dispatcher, &mut surfaces);
    focus_surface(&mut state, &mut dispatcher, 70);

    press(&mut state, &mut dispatcher, menu);
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(menu));
}

#[test]
fn a_press_on_a_tooltip_or_utility_window_keeps_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let focused = manage(
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
    let tooltip = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        10,
        12,
        "Tip",
        "App",
        |ready, property| {
            (property == ready._NET_WM_WINDOW_TYPE)
                .then(|| cardinals(&[ready._NET_WM_WINDOW_TYPE_TOOLTIP.0]))
        },
    );
    let utility = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x52),
        82,
        3,
        10,
        12,
        "Tool",
        "App",
        |ready, property| {
            (property == ready._NET_WM_WINDOW_TYPE)
                .then(|| cardinals(&[ready._NET_WM_WINDOW_TYPE_UTILITY.0]))
        },
    );
    assert!(!state
        .toplevel(tooltip.window)
        .unwrap()
        .accepts_keyboard_focus());
    assert!(!state
        .toplevel(utility.window)
        .unwrap()
        .accepts_keyboard_focus());
    crate::input::set_keyboard_focus(
        &mut state,
        &mut dispatcher,
        Some(SurfaceFocus::new(focused.surface)),
    );

    press(&mut state, &mut dispatcher, tooltip.surface);
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(focused.surface));
    press(&mut state, &mut dispatcher, utility.surface);
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(focused.surface));
}

#[test]
fn a_press_on_a_window_with_no_input_keeps_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let focused = manage(
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
    let quiet = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        10,
        12,
        "Quiet",
        "App",
        |ready, property| (property == ready.WM_HINTS).then(|| cardinals(&[1, 0])),
    );
    assert!(!state
        .toplevel(quiet.window)
        .unwrap()
        .accepts_keyboard_focus());
    crate::input::set_keyboard_focus(
        &mut state,
        &mut dispatcher,
        Some(SurfaceFocus::new(focused.surface)),
    );

    press(&mut state, &mut dispatcher, quiet.surface);
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(focused.surface));
}

#[test]
fn a_press_on_a_normal_x_window_focuses_it() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let window = manage(
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
    assert!(state
        .toplevel(window.window)
        .unwrap()
        .accepts_keyboard_focus());
    focus_surface(&mut state, &mut dispatcher, 70);

    press(&mut state, &mut dispatcher, window.surface);
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(window.surface));
}

fn focus_in(window: Window, sequence: u16, mode: NotifyMode, detail: NotifyDetail) -> Event {
    Event::FocusIn(FocusIn { send_event: false, sequence, detail, event: window, mode })
}

/// The newest sequence an event can carry. The next request number is still unused, and an
/// event is never newer than the last request sent.
fn latest_sequence(wm: &Wm) -> u16 {
    u16::try_from(wm.conn.test_next_sequence().saturating_sub(1)).unwrap()
}

#[test]
fn a_same_pid_focus_in_moves_tema_focus_and_focused() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    let second = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        "Two",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x51), 7);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, first.surface);
    let ready = atoms(&wm);
    let sequence = latest_sequence(&wm);

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        focus_in(Window(0x51), sequence, NotifyMode::Normal, NotifyDetail::Ancestor),
    );

    assert_eq!(wm.focus, Some(Window(0x51)));
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(second.surface));
    expect_requests(
        sent,
        vec![
            net_state_request(Window(0x50), &ready, &[]),
            net_state_request(Window(0x51), &ready, &[ready._NET_WM_STATE_FOCUSED]),
        ],
    );
}

#[test]
fn a_different_pid_focus_in_returns_focus_to_the_wm_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        "Two",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x51), 8);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, first.surface);
    let sequence = latest_sequence(&wm);

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        focus_in(Window(0x51), sequence, NotifyMode::Normal, NotifyDetail::Ancestor),
    );

    assert_eq!(wm.focus, Some(Window(0x50)));
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(first.surface));
    expect_requests(sent, vec![input_focus(Window(0x50))]);
}

#[test]
fn a_focus_in_without_a_pid_returns_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        "Two",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, first.surface);
    let sequence = latest_sequence(&wm);

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        focus_in(Window(0x51), sequence, NotifyMode::Normal, NotifyDetail::Ancestor),
    );

    assert_eq!(wm.focus, Some(Window(0x50)));
    expect_requests(sent, vec![input_focus(Window(0x50))]);
}

#[test]
fn an_older_focus_in_returns_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        "Two",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x51), 7);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, first.surface);

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        focus_in(Window(0x51), 1, NotifyMode::Normal, NotifyDetail::Ancestor),
    );

    assert_eq!(wm.focus, Some(Window(0x50)));
    expect_requests(sent, vec![input_focus(Window(0x50))]);
}

#[test]
fn grab_ungrab_and_pointer_focus_in_are_ignored() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, first.surface);
    let sequence = latest_sequence(&wm);

    for event in [
        focus_in(Window(0x50), sequence, NotifyMode::Grab, NotifyDetail::Ancestor),
        focus_in(Window(0x50), sequence, NotifyMode::Ungrab, NotifyDetail::Ancestor),
        focus_in(Window(0x50), sequence, NotifyMode::Normal, NotifyDetail::Pointer),
    ] {
        let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, event);
        assert!(sent.is_empty());
    }
    assert_eq!(wm.focus, Some(Window(0x50)));
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(first.surface));
}

#[test]
fn a_same_pid_focus_in_on_an_override_redirect_window_keeps_tema_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, first.surface);
    let menu = show_menu(&mut wm, &mut state, &mut dispatcher, &mut surfaces);
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x70), 7);
    let ready = atoms(&wm);
    let sequence = latest_sequence(&wm);

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        focus_in(Window(0x70), sequence, NotifyMode::Normal, NotifyDetail::Ancestor),
    );

    assert_eq!(wm.focus, Some(Window(0x70)));
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(first.surface));
    assert_ne!(menu, first.surface);
    expect_requests(sent, vec![net_state_request(Window(0x50), &ready, &[])]);
}

#[test]
fn a_focus_in_with_no_wm_focus_sends_focus_none() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    assert!(wm.focus.is_none());
    let sequence = latest_sequence(&wm);

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        focus_in(Window(0x50), sequence, NotifyMode::Normal, NotifyDetail::Ancestor),
    );

    assert!(wm.focus.is_none());
    expect_requests(sent, vec![input_focus(Window::None)]);
}

fn active_window_message(kind: Atom, window: Window) -> Event {
    state_message(window, kind, [1, 0, 0, 0, 0])
}

fn drain_x_actions(dispatcher: &mut Dispatcher) {
    dispatcher.test_take_queued_actions::<XwaylandAction>();
}

fn queued_keyboard_focus(dispatcher: &mut Dispatcher) -> bool {
    dispatcher
        .test_take_queued_actions::<XwaylandAction>()
        .into_iter()
        .any(|action| matches!(action, XwaylandAction::KeyboardFocus))
}

#[test]
fn a_same_pid_active_window_request_queues_focus() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    let second = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        "Two",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x51), 7);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, first.surface);
    drain_x_actions(&mut dispatcher);
    let ready = atoms(&wm);

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        active_window_message(ready._NET_ACTIVE_WINDOW, Window(0x51)),
    );

    assert!(sent.is_empty());
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(second.surface));
    assert!(queued_keyboard_focus(&mut dispatcher));
    let sent =
        handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::KeyboardFocus);
    assert_eq!(wm.focus, Some(Window(0x51)));
    expect_requests(
        sent,
        vec![
            active_window(&ready, Window(0x51).0),
            net_state_request(Window(0x50), &ready, &[]),
            input_focus(Window(0x51)),
            net_state_request(Window(0x51), &ready, &[ready._NET_WM_STATE_FOCUSED]),
            restack(Window(0x51), Some(Window(0x50))),
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[Window(0x50), Window(0x51)]),
        ],
    );
}

#[test]
fn a_different_pid_active_window_request_changes_nothing() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        "Two",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x51), 8);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, first.surface);
    drain_x_actions(&mut dispatcher);
    let message = active_window_message(atoms(&wm)._NET_ACTIVE_WINDOW, Window(0x51));

    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, message);

    assert!(sent.is_empty());
    assert_eq!(wm.focus, Some(Window(0x50)));
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(first.surface));
    assert!(!queued_keyboard_focus(&mut dispatcher));
}

#[test]
fn an_active_window_request_for_a_hidden_workspace_changes_nothing() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    let output = add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    let second = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        "Two",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x51), 7);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, first.surface);
    assert!(state.assign_window_to_workspace(second.window, hidden_workspace(&state, output)));
    state.relayout_dirty_workspaces();
    drain_x_actions(&mut dispatcher);
    let message = active_window_message(atoms(&wm)._NET_ACTIVE_WINDOW, Window(0x51));

    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, message);

    assert!(sent.is_empty());
    assert_eq!(wm.focus, Some(Window(0x50)));
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(first.surface));
    assert!(!queued_keyboard_focus(&mut dispatcher));
}

#[test]
fn an_active_window_request_with_no_x_focus_changes_nothing() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    let second = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        "Two",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x51), 7);
    drain_x_actions(&mut dispatcher);
    assert!(wm.focus.is_none());
    let message = active_window_message(atoms(&wm)._NET_ACTIVE_WINDOW, Window(0x51));

    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, message);

    assert!(sent.is_empty());
    assert!(wm.focus.is_none());
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(second.surface));
    assert!(!queued_keyboard_focus(&mut dispatcher));
}

#[test]
fn an_active_window_request_for_an_override_redirect_window_changes_nothing() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = super::tests::open_wm();
    add_output(&mut state);
    let first = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "One",
        "XTerm",
    );
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x50), 7);
    focus_root(&mut wm, &mut state, &mut dispatcher, &surfaces, first.surface);
    show_menu(&mut wm, &mut state, &mut dispatcher, &mut surfaces);
    set_pid(&mut wm, &mut state, &mut dispatcher, &surfaces, Window(0x70), 7);
    drain_x_actions(&mut dispatcher);
    let message = active_window_message(atoms(&wm)._NET_ACTIVE_WINDOW, Window(0x70));

    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, message);

    assert!(sent.is_empty());
    assert_eq!(wm.focus, Some(Window(0x50)));
    assert_eq!(state.seat.keyboard_focus.map(|focus| focus.surface), Some(first.surface));
    assert!(!queued_keyboard_focus(&mut dispatcher));
}
