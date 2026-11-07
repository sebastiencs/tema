use crate::{
    clients::Resolved,
    input::{pick::build_window_target, types::WindowRoleSnapshot},
    protocol_generated::WlSurfaceId,
    region::RectI,
    runner::Dispatcher,
    scene::SceneRole,
    window::{Toplevel, ToplevelDecorationMode},
    x11::{
        change_property_atoms, change_property_u32s, event_bytes, testing, Atom, Atoms,
        ClientMessage, ClientMessageData, ConfigWindow, ConfigureNotify, ConfigureWindow,
        ConfigureWindowAux, DestroyNotify, Event, EventMask, KillClient, MapNotify, PropertyError,
        PropertyNotify, PropertyState, SendEvent, StackMode, UnmapNotify, Window, XReplyError,
    },
    xwayland::SurfaceEvent,
};

use super::{
    props,
    tests::{
        buffered_x_root, configure_request, created, deliver, deliver_property, expect_requests,
        handle_event, map_request, note_surface, open_wm, reconfigure, serial_message, take_writes,
    },
    Wm, WmEvent, WmPhase,
};

fn atoms(wm: &Wm) -> Atoms {
    let WmPhase::Ready(ready) = &wm.phase else { panic!("ready") };
    ready.atoms
}

fn blank() -> crate::x11::Property {
    crate::x11::Property {
        r#type: Atom(0),
        format: 0,
        data: Vec::new(),
        truncated: false,
    }
}

fn text(ty: Atom, bytes: &[u8]) -> crate::x11::Property {
    crate::x11::Property {
        r#type: ty,
        format: 8,
        data: bytes.to_vec(),
        truncated: false,
    }
}

fn transient(parent: Window) -> crate::x11::Property {
    crate::x11::Property {
        r#type: Atom(0),
        format: 32,
        data: parent.0.to_ne_bytes().to_vec(),
        truncated: false,
    }
}

fn pair_mapped(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &mut crate::xwayland::surfaces::SurfaceSerials,
    window: Window,
    raw: u32,
    serial: u64,
    override_redirect: u8,
) -> Resolved<WlSurfaceId> {
    deliver(wm, state, dispatcher, surfaces, created(window, 30, 40, 10, 12, override_redirect));
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
            override_redirect,
        }),
    );
    let surface = buffered_x_root(state, raw, serial, 10, 12);
    let assigned = SurfaceEvent::SerialAssigned { serial, surface };
    surfaces.apply(assigned);
    let atom = atoms(wm).WL_SURFACE_SERIAL;
    deliver(wm, state, dispatcher, surfaces, serial_message(window, atom, serial));
    surface
}

fn reply(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
    window: Window,
    property: Atom,
    result: Result<crate::x11::Property, PropertyError>,
) {
    deliver_property(wm, state, dispatcher, surfaces, window, property, result);
}

struct ManagedWindow {
    surface: Resolved<WlSurfaceId>,
    window: crate::window::WindowId,
    object: crate::state::ObjectId,
}

fn manage(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &mut crate::xwayland::surfaces::SurfaceSerials,
    window: Window,
    raw: u32,
    serial: u64,
    title: &str,
    class: &str,
) -> ManagedWindow {
    manage_with(
        wm,
        state,
        dispatcher,
        surfaces,
        window,
        raw,
        serial,
        10,
        12,
        title,
        class,
        |_, _| None,
    )
}

fn manage_with(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &mut crate::xwayland::surfaces::SurfaceSerials,
    window: Window,
    raw: u32,
    serial: u64,
    width: u16,
    height: u16,
    title: &str,
    class: &str,
    extra: impl Fn(&Atoms, Atom) -> Option<crate::x11::Property>,
) -> ManagedWindow {
    let surface = pair_box(wm, state, dispatcher, surfaces, window, raw, serial, width, height);
    let ready = atoms(wm);
    let class_bytes = format!("inst\0{class}\0");
    for (property, _) in props::loads(&ready) {
        let value = if let Some(property) = extra(&ready, property) {
            Ok(property)
        } else if property == ready._NET_WM_NAME {
            Ok(text(ready.UTF8_STRING, title.as_bytes()))
        } else if property == ready.WM_CLASS {
            Ok(text(ready.UTF8_STRING, class_bytes.as_bytes()))
        } else {
            Ok(blank())
        };
        reply(wm, state, dispatcher, surfaces, window, property, value);
    }
    let id = state.window_for_root(surface).expect("managed window");
    let object = state.window_role_object(id).expect("role object");
    ManagedWindow { surface, window: id, object }
}

fn pair_box(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &mut crate::xwayland::surfaces::SurfaceSerials,
    window: Window,
    raw: u32,
    serial: u64,
    width: u16,
    height: u16,
) -> Resolved<WlSurfaceId> {
    deliver(wm, state, dispatcher, surfaces, created(window, 30, 40, width, height, 0));
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
    let surface = buffered_x_root(state, raw, serial, i32::from(width), i32::from(height));
    let assigned = SurfaceEvent::SerialAssigned { serial, surface };
    surfaces.apply(assigned);
    let atom = atoms(wm).WL_SURFACE_SERIAL;
    deliver(wm, state, dispatcher, surfaces, serial_message(window, atom, serial));
    surface
}

fn cardinals(words: &[u32]) -> crate::x11::Property {
    let mut data = Vec::with_capacity(words.len() * 4);
    for word in words {
        data.extend(word.to_ne_bytes());
    }
    crate::x11::Property { r#type: Atom(0), format: 32, data, truncated: false }
}

#[test]
fn a_paired_mapped_window_is_managed_after_its_properties() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    let surface =
        pair_mapped(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, 0);
    assert!(state.window_for_root(surface).is_none());
    assert!(!wm.windows.get(&window).unwrap().shown);

    let ready = atoms(&wm);
    let loaded = props::loads(&ready);
    let count = loaded.len();
    for (index, (property, _)) in loaded.into_iter().enumerate() {
        let result = if property == ready._NET_WM_NAME {
            Ok(text(ready.UTF8_STRING, b"Term"))
        } else if property == ready.WM_CLASS {
            Ok(text(ready.UTF8_STRING, b"inst\0XTerm\0"))
        } else if index + 1 == loaded.len() {
            Err(PropertyError::Reply(XReplyError::MissingReply))
        } else {
            Ok(blank())
        };
        reply(&mut wm, &mut state, &mut dispatcher, &surfaces, window, property, result);
        if index + 1 < count {
            assert!(state.window_for_root(surface).is_none());
        }
    }

    let id = state.window_for_root(surface).expect("window");
    let toplevel = state.toplevel(id).expect("toplevel");
    assert_eq!(toplevel.title(), "Term");
    assert_eq!(toplevel.app_id(), "XTerm");
    assert!(wm.windows.get(&window).unwrap().shown);
    assert_eq!(super::tests::root_position(&state, surface), (0, 0));
}

#[test]
fn an_override_redirect_window_is_not_managed() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    let surface =
        pair_mapped(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, 1);
    let ready = atoms(&wm);
    for (property, _) in props::loads(&ready) {
        reply(&mut wm, &mut state, &mut dispatcher, &surfaces, window, property, Ok(blank()));
    }

    assert!(state.window_for_root(surface).is_none());
    assert!(wm.windows.get(&window).unwrap().shown);
    assert!(!super::tests::hidden(&state, surface));
}

#[test]
fn unmap_destroy_unpair_and_override_redirect_each_release_the_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();

    let unmapped = manage(
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
    assert!(state.window_for_root(unmapped.surface).is_none());
    assert!(state.objects.try_get(unmapped.object).is_none());
    assert!(super::tests::hidden(&state, unmapped.surface));

    let destroyed = manage(
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
    assert!(state.objects.try_get(destroyed.object).is_none());
    assert!(super::tests::hidden(&state, destroyed.surface));

    let unpaired = manage(
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
    let event = SurfaceEvent::Destroyed { serial: 3, surface: unpaired.surface };
    surfaces.apply(event);
    note_surface(&mut wm, &mut state, &mut dispatcher, &surfaces, &event);
    assert!(state.window_for_root(unpaired.surface).is_none());
    assert!(state.objects.try_get(unpaired.object).is_none());
    assert!(super::tests::hidden(&state, unpaired.surface));

    let redirected = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x53),
        83,
        4,
        "Four",
        "App",
    );
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::ConfigureNotify(ConfigureNotify {
            send_event: false,
            sequence: 0,
            event: Window(0x100),
            window: Window(0x53),
            above_sibling: Window(0),
            x: 30,
            y: 40,
            width: 10,
            height: 12,
            border_width: 0,
            override_redirect: 1,
        }),
    );
    assert!(state.window_for_root(redirected.surface).is_none());
    assert!(state.objects.try_get(redirected.object).is_none());
    assert!(wm.windows.get(&Window(0x53)).unwrap().shown);
    assert!(!super::tests::hidden(&state, redirected.surface));
}

#[test]
fn a_title_change_reaches_the_toplevel() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Old", "App");
    let ready = atoms(&wm);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        Event::PropertyNotify(PropertyNotify {
            send_event: false,
            sequence: 0,
            window,
            atom: ready._NET_WM_NAME,
            time: 0,
            state: PropertyState::NewValue,
        }),
    );
    reply(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        window,
        ready._NET_WM_NAME,
        Ok(text(ready.UTF8_STRING, b"New")),
    );

    assert_eq!(state.toplevel(managed.window).unwrap().title(), "New");
}

#[test]
fn a_transient_child_uses_its_parents_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let parent = Window(0x50);
    let parent_managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, parent, 80, 1, "Parent", "App");
    let child = Window(0x51);
    let surface = pair_mapped(&mut wm, &mut state, &mut dispatcher, &mut surfaces, child, 81, 2, 0);
    let ready = atoms(&wm);
    for (property, _) in props::loads(&ready) {
        let value =
            if property == ready.WM_TRANSIENT_FOR { Ok(transient(parent)) } else { Ok(blank()) };
        reply(&mut wm, &mut state, &mut dispatcher, &surfaces, child, property, value);
    }

    let child_id = state.window_for_root(surface).expect("child");
    assert_eq!(state.toplevel(child_id).unwrap().parent_window(), Some(parent_managed.window));
}

#[test]
fn stopping_the_wm_releases_every_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let first = manage(
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
    let second = manage(
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

    wm.stop(&mut state, &mut dispatcher);

    assert!(state.window_for_root(first.surface).is_none());
    assert!(state.window_for_root(second.surface).is_none());
    assert!(state.objects.try_get(first.object).is_none());
    assert!(state.objects.try_get(second.object).is_none());
    assert!(super::tests::hidden(&state, first.surface));
    assert!(super::tests::hidden(&state, second.surface));
}

#[test]
fn a_managed_window_is_tiled_at_the_content_origin() {
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

    assert!(state.window_is_tiled(managed.window));
    let geometry = state
        .committed_root_geometry(managed.surface)
        .expect("geometry");
    let content = geometry.content_rect_global();
    assert_eq!(super::tests::root_position(&state, managed.surface), (content.x, content.y));
    assert_ne!((content.x, content.y), (30, 40));
    assert_eq!(scene_role(&state, managed.surface), Some(SceneRole::WorkspaceRoot));

    let target = build_window_target(&state, managed.surface).expect("target");
    match target.role {
        WindowRoleSnapshot::Toplevel { window, decoration_mode, .. } => {
            assert_eq!(window, managed.window);
            assert_eq!(decoration_mode, ToplevelDecorationMode::ServerSide);
        }
        WindowRoleSnapshot::Unsupported => panic!("managed x window is a toplevel"),
    }
}

#[test]
fn two_managed_windows_split_the_workspace() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
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
        "App",
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
        "App",
    );

    let first_rect = state
        .committed_root_geometry(first.surface)
        .expect("first")
        .layout_rect_global;
    let second_rect = state
        .committed_root_geometry(second.surface)
        .expect("second")
        .layout_rect_global;
    let (left, right) = if first_rect.x <= second_rect.x {
        (first_rect, second_rect)
    } else {
        (second_rect, first_rect)
    };
    assert_eq!(left.y, right.y);
    assert_eq!(left.height, right.height);
    assert_eq!(left.x + left.width, right.x);
    assert!(left.width > 0 && right.width > 0);
    assert_ne!(
        super::tests::root_position(&state, first.surface),
        super::tests::root_position(&state, second.surface)
    );
    assert_eq!(scene_role(&state, first.surface), Some(SceneRole::WorkspaceRoot));
    assert_eq!(scene_role(&state, second.surface), Some(SceneRole::WorkspaceRoot));
}

#[test]
fn unmap_removes_the_window_from_the_layout() {
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
    assert!(state.window_is_tiled(managed.window));

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

    assert!(state.window_for_root(managed.surface).is_none());
    assert!(scene_role(&state, managed.surface).is_none());
}

#[test]
fn an_override_redirect_window_stays_at_its_x_geometry() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let surface =
        pair_mapped(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, 1);
    let ready = atoms(&wm);
    for (property, _) in props::loads(&ready) {
        reply(&mut wm, &mut state, &mut dispatcher, &surfaces, window, property, Ok(blank()));
    }

    assert!(state.window_for_root(surface).is_none());
    assert_eq!(super::tests::root_position(&state, surface), (30, 40));
    assert_eq!(scene_role(&state, surface), Some(SceneRole::UnmanagedRoot));
}

#[test]
fn a_tiled_window_is_configured_to_its_content_rect() {
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
    let content = state
        .tiled_window_geometry(managed.window)
        .expect("tile")
        .content_rect_global();

    let sent = reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces);
    expect_requests(sent, vec![encoded_configure(Window(0x50), content)]);

    let again = reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces);
    assert!(again.is_empty());
}

#[test]
fn a_second_window_shrinks_the_first_and_both_are_configured() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
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
        "App",
    );
    let alone = state
        .tiled_window_geometry(first.window)
        .expect("first tile")
        .content_rect_global();
    expect_requests(
        reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces),
        vec![encoded_configure(Window(0x50), alone)],
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
        "App",
    );
    let first_now = state
        .tiled_window_geometry(first.window)
        .expect("shrunk tile")
        .content_rect_global();
    let second_rect = state
        .tiled_window_geometry(second.window)
        .expect("second tile")
        .content_rect_global();
    assert!(first_now.width < alone.width);

    expect_requests(
        reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces),
        vec![
            encoded_configure(Window(0x50), first_now),
            encoded_configure(Window(0x51), second_rect),
        ],
    );
}

#[test]
fn a_fullscreen_window_is_configured_to_the_output_rect() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
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
    assert!(state.set_window_fullscreen(managed.window, true));

    let bounds = state.output_bounds(output).expect("output");
    let geometry = state
        .tiled_window_geometry(managed.window)
        .expect("fullscreen");
    let content = geometry.content_rect_global();
    assert!(geometry.fullscreen);
    assert_eq!(content, bounds);

    expect_requests(
        reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces),
        vec![encoded_configure(Window(0x50), content)],
    );
}

#[test]
fn a_configure_request_on_a_managed_window_is_ignored() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let pending = wm.windows.get(&window).unwrap().pending;

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        configure_request(
            window,
            ConfigWindow::X | ConfigWindow::Y | ConfigWindow::Width | ConfigWindow::Height,
            80,
            90,
            40,
            50,
        ),
    );

    assert!(sent.is_empty());
    assert_eq!(wm.windows.get(&window).unwrap().pending, pending);
}

#[test]
fn a_window_on_a_hidden_workspace_is_not_configured() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
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
    let group = state.workspace_group_for_output(output).expect("group");
    let active = state.active_workspace_for_output(output);
    let hidden = state
        .workspace_groups
        .try_get(group)
        .expect("group")
        .workspaces
        .iter()
        .copied()
        .find(|workspace| Some(*workspace) != active)
        .expect("hidden workspace");
    assert!(state.assign_window_to_workspace(managed.window, hidden));
    state.relayout_dirty_workspaces();

    assert!(reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces).is_empty());
}

#[test]
fn a_dialog_floats_centered_on_the_output() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let output = add_output(&mut state);
    let dialog = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        10,
        12,
        "Ask",
        "App",
        |ready, property| {
            (property == ready._NET_WM_WINDOW_TYPE)
                .then(|| cardinals(&[ready._NET_WM_WINDOW_TYPE_DIALOG.0]))
        },
    );

    assert!(state.window_is_floating(dialog.window));
    let bounds = state.output_bounds(output).expect("output");
    let placed = placed_content(bounds, bounds, state.frame_insets(), 10, 12);
    assert_eq!(super::tests::root_position(&state, dialog.surface), (placed.x, placed.y));
    expect_requests(
        reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces),
        vec![encoded_configure(Window(0x50), placed)],
    );
}

#[test]
fn a_transient_child_floats_centered_over_its_tiled_parent() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let output = add_output(&mut state);
    let parent = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "Parent",
        "App",
    );
    manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x52),
        82,
        2,
        "Other",
        "App",
    );
    let parent_frame = state
        .committed_root_geometry(parent.surface)
        .expect("parent")
        .layout_rect_global;
    let transient_for = atoms(&wm).WM_TRANSIENT_FOR;
    let child = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        3,
        10,
        12,
        "Child",
        "App",
        move |_, property| (property == transient_for).then(|| transient(Window(0x50))),
    );

    assert!(state.window_is_floating(child.window));
    let bounds = state.output_bounds(output).expect("output");
    let placed = placed_content(parent_frame, bounds, state.frame_insets(), 10, 12);
    let on_output = placed_content(bounds, bounds, state.frame_insets(), 10, 12);
    assert_ne!((placed.x, placed.y), (on_output.x, on_output.y));
    assert_eq!(super::tests::root_position(&state, child.surface), (placed.x, placed.y));
}

#[test]
fn a_transient_child_whose_parent_is_on_another_workspace_centers_on_the_output() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let output = add_output(&mut state);
    let parent = manage(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        "Parent",
        "App",
    );
    assert!(state.assign_window_to_workspace(parent.window, hidden_workspace(&state, output)));
    state.relayout_dirty_workspaces();
    let transient_for = atoms(&wm).WM_TRANSIENT_FOR;
    let child = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x51),
        81,
        2,
        10,
        12,
        "Child",
        "App",
        move |_, property| (property == transient_for).then(|| transient(Window(0x50))),
    );

    assert!(state.window_is_floating(child.window));
    let bounds = state.output_bounds(output).expect("output");
    let placed = placed_content(bounds, bounds, state.frame_insets(), 10, 12);
    assert_eq!(super::tests::root_position(&state, child.surface), (placed.x, placed.y));
}

#[test]
fn a_fixed_size_window_floats() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
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
        "Fixed",
        "App",
        |ready, property| {
            if property != ready.WM_NORMAL_HINTS {
                return None;
            }
            let mut hints = vec![0; 9];
            hints[0] = (1 << 4) | (1 << 5);
            hints[5] = 40;
            hints[6] = 20;
            hints[7] = 40;
            hints[8] = 20;
            Some(cardinals(&hints))
        },
    );

    assert!(state.window_is_floating(managed.window));
}

#[test]
fn a_window_larger_than_the_output_is_shrunk_and_clamped() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let output = add_output(&mut state);
    let managed = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        900,
        700,
        "Big",
        "App",
        |ready, property| {
            (property == ready._NET_WM_WINDOW_TYPE)
                .then(|| cardinals(&[ready._NET_WM_WINDOW_TYPE_DIALOG.0]))
        },
    );

    assert!(state.window_is_floating(managed.window));
    let bounds = state.output_bounds(output).expect("output");
    let insets = state.frame_insets();
    let placed = placed_content(bounds, bounds, insets, 900, 700);
    assert!(placed.width < 900);
    assert!(placed.height < 700);
    assert_eq!(super::tests::root_position(&state, managed.surface), (placed.x, placed.y));
    assert!(placed.x >= bounds.x + insets.left);
    assert!(placed.y >= bounds.y + insets.top);
    assert!(placed.x + placed.width <= bounds.x + bounds.width - insets.right);
    assert!(placed.y + placed.height <= bounds.y + bounds.height - insets.bottom);
    assert_eq!(
        wm.windows
            .get(&Window(0x50))
            .unwrap()
            .managed
            .unwrap()
            .floating_size,
        Some((u32::try_from(placed.width).unwrap(), u32::try_from(placed.height).unwrap()))
    );
}

#[test]
fn a_normal_window_tiles() {
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

    assert!(state.window_is_tiled(managed.window));
    assert!(!state.window_is_floating(managed.window));
}

#[test]
fn fullscreen_at_map_makes_a_tiled_window_fullscreen() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let output = add_output(&mut state);
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

    assert!(state.window_is_fullscreen(managed.window));
    assert!(!state.window_is_floating(managed.window));
    let bounds = state.output_bounds(output).expect("output");
    let content = state
        .tiled_window_geometry(managed.window)
        .expect("fullscreen")
        .content_rect_global();
    assert_eq!(content, bounds);
}

#[test]
fn a_window_with_delete_protocol_gets_wm_delete_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        window,
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
    let ready = atoms(&wm);
    let sent =
        handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::Close { window });

    expect_requests(
        sent,
        vec![delete_window_request(window, ready.WM_PROTOCOLS, ready.WM_DELETE_WINDOW)],
    );
}

#[test]
fn a_window_without_delete_protocol_is_killed() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let window = Window(0x50);
    manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let sent =
        handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::Close { window });

    expect_requests(sent, vec![testing::encode(&KillClient { resource: window.0 }, 0)]);
}

#[test]
fn close_on_a_focused_x_window_queues_the_wm_close() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    state.seat.keyboard_focus = Some(crate::input::SurfaceFocus::new(managed.surface));

    crate::config::execute_action(
        &mut state,
        &mut dispatcher,
        crate::config::CompositorAction::Close,
    );

    assert!(dispatcher.test_has_queued_action::<crate::xwayland::XwaylandAction>(|action| {
        matches!(
            action,
            crate::xwayland::XwaylandAction::Wm { run: 1, event: WmEvent::Close { window: closed } }
                if *closed == window
        )
    }));
}

fn delete_window_request(window: Window, protocols: Atom, delete: Atom) -> Vec<u8> {
    let mut data = ClientMessageData::default();
    data.set_data32([delete.0, 0, 0, 0, 0]);
    let message = ClientMessage {
        send_event: true,
        sequence: 0,
        format: 32,
        window,
        r#type: protocols,
        data,
    };
    let event = event_bytes(|writer| message.serialize(writer));
    testing::encode(
        &SendEvent {
            propagate: 0,
            destination: window,
            event_mask: EventMask::NoEvent,
            event: &event,
        },
        0,
    )
}

#[test]
fn net_wm_state_add_remove_and_toggle_fullscreen() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let ready = atoms(&wm);
    let fullscreen = ready._NET_WM_STATE_FULLSCREEN;

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready._NET_WM_STATE, [1, fullscreen.0, 0, 0, 0]),
    );
    assert!(state.window_is_fullscreen(managed.window));
    assert!(state
        .toplevel(managed.window)
        .unwrap()
        .is_fullscreen_requested());
    expect_requests(sent, vec![net_state_request(window, &ready, &[fullscreen])]);

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready._NET_WM_STATE, [0, fullscreen.0, 0, 0, 0]),
    );
    assert!(!state.window_is_fullscreen(managed.window));
    assert!(!state
        .toplevel(managed.window)
        .unwrap()
        .is_fullscreen_requested());
    expect_requests(sent, vec![net_state_request(window, &ready, &[])]);

    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready._NET_WM_STATE, [2, fullscreen.0, 0, 0, 0]),
    );
    assert!(state.window_is_fullscreen(managed.window));
    assert!(wm.windows.get(&window).unwrap().props.fullscreen);
    expect_requests(sent, vec![net_state_request(window, &ready, &[fullscreen])]);
}

#[test]
fn an_unknown_net_wm_state_atom_writes_nothing() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let ready = atoms(&wm);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready._NET_WM_STATE, [1, 0x00ff_ffff, 0, 0, 0]),
    );

    assert!(sent.is_empty());
    assert!(!wm.windows.get(&window).unwrap().props.fullscreen);
    assert!(!state.window_is_fullscreen(managed.window));
}

#[test]
fn a_compositor_fullscreen_toggle_writes_nothing_to_x() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    state.seat.keyboard_focus = Some(crate::input::SurfaceFocus::new(managed.surface));

    crate::config::execute_action(
        &mut state,
        &mut dispatcher,
        crate::config::CompositorAction::ToggleFullscreen,
    );

    assert!(state.window_is_fullscreen(managed.window));
    assert!(!wm.windows.get(&window).unwrap().props.fullscreen);
    assert!(take_writes(&mut wm, &mut state, &mut dispatcher, &surfaces).is_empty());
}

#[test]
fn iconic_while_focused_writes_normal() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    state.seat.keyboard_focus = Some(crate::input::SurfaceFocus::new(managed.surface));
    let ready = atoms(&wm);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready.WM_CHANGE_STATE, [3, 0, 0, 0, 0]),
    );

    assert!(!wm.windows.get(&window).unwrap().props.hidden);
    assert!(wm.windows.get(&window).unwrap().shown);
    expect_requests(
        sent,
        vec![wm_state_request(window, ready.WM_STATE, 1), net_state_request(window, &ready, &[])],
    );
}

#[test]
fn iconic_while_unfocused_writes_iconic_and_hidden() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    let managed =
        manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let ready = atoms(&wm);
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready.WM_CHANGE_STATE, [3, 0, 0, 0, 0]),
    );

    assert!(wm.windows.get(&window).unwrap().props.hidden);
    assert!(state.window_is_mapped(managed.window));
    expect_requests(
        sent,
        vec![
            wm_state_request(window, ready.WM_STATE, 3),
            net_state_request(window, &ready, &[ready._NET_WM_STATE_HIDDEN]),
        ],
    );
}

#[test]
fn change_state_normal_clears_hidden() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let window = Window(0x50);
    manage(&mut wm, &mut state, &mut dispatcher, &mut surfaces, window, 80, 1, "Term", "XTerm");
    let ready = atoms(&wm);
    deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready.WM_CHANGE_STATE, [3, 0, 0, 0, 0]),
    );
    let sent = deliver(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        state_message(window, ready.WM_CHANGE_STATE, [1, 0, 0, 0, 0]),
    );

    assert!(!wm.windows.get(&window).unwrap().props.hidden);
    expect_requests(
        sent,
        vec![wm_state_request(window, ready.WM_STATE, 1), net_state_request(window, &ready, &[])],
    );
}

fn state_message(window: Window, kind: Atom, words: [u32; 5]) -> Event {
    let mut data = ClientMessageData::default();
    data.set_data32(words);
    Event::ClientMessage(ClientMessage {
        send_event: false,
        sequence: 0,
        format: 32,
        window,
        r#type: kind,
        data,
    })
}

fn net_state_request(window: Window, atoms: &Atoms, state: &[Atom]) -> Vec<u8> {
    testing::encode(
        &change_property_atoms(window, atoms._NET_WM_STATE, atoms.ATOM, state).request(),
        0,
    )
}

fn wm_state_request(window: Window, wm_state: Atom, state: u32) -> Vec<u8> {
    testing::encode(&change_property_u32s(window, wm_state, wm_state, &[state, 0]).request(), 0)
}

#[test]
fn two_map_requests_list_clients_in_map_order() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let first = Window(0x50);
    let second = Window(0x51);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(first, 1, 2, 10, 12, 0));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(second, 3, 4, 10, 12, 0));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_request(first));
    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_request(second));
    let ready = atoms(&wm);

    assert!(sent.iter().any(|request| {
        request == &window_list(&ready, ready._NET_CLIENT_LIST, &[first, second])
    }));
    assert_eq!(wm.map_order, vec![first, second]);
    assert_eq!(wm.stack, vec![first, second]);
}

#[test]
fn the_second_map_notify_restacks_above_its_sibling() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let first = Window(0x50);
    let second = Window(0x51);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(first, 1, 2, 10, 12, 0));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(second, 3, 4, 10, 12, 0));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_request(first));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_notify(first, 0));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_request(second));
    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_notify(second, 0));
    let ready = atoms(&wm);

    expect_requests(
        sent,
        vec![
            testing::encode(
                &ConfigureWindow {
                    window: second,
                    value_list: ConfigureWindowAux::default()
                        .sibling(first)
                        .stack_mode(StackMode::Above),
                },
                0,
            ),
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[first, second]),
        ],
    );
}

#[test]
fn unmap_removes_the_window_from_the_client_list_only() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let (first, second) = mapped_pair(&mut wm, &mut state, &mut dispatcher, &surfaces);
    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, unmapped(second));
    let ready = atoms(&wm);

    assert_eq!(wm.map_order, vec![first]);
    assert_eq!(wm.stack, vec![first, second]);
    expect_requests(
        sent,
        vec![
            wm_state_request(second, ready.WM_STATE, 0),
            window_list(&ready, ready._NET_CLIENT_LIST, &[first]),
        ],
    );
}

#[test]
fn destroy_removes_the_window_from_both_lists() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let (first, second) = mapped_pair(&mut wm, &mut state, &mut dispatcher, &surfaces);
    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, destroyed(second));
    let ready = atoms(&wm);

    assert_eq!(wm.map_order, vec![first]);
    assert_eq!(wm.stack, vec![first]);
    expect_requests(
        sent,
        vec![
            window_list(&ready, ready._NET_CLIENT_LIST, &[first]),
            window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[first]),
        ],
    );
}

#[test]
fn an_override_redirect_window_is_never_listed() {
    let (mut wm, mut state, mut dispatcher, surfaces) = open_wm();
    let window = Window(0x50);
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, created(window, 1, 2, 10, 12, 1));
    let sent = deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_request(window));
    let ready = atoms(&wm);
    let client = window_list(&ready, ready._NET_CLIENT_LIST, &[]);
    let stacking = window_list(&ready, ready._NET_CLIENT_LIST_STACKING, &[]);

    assert!(wm.map_order.is_empty());
    assert!(wm.stack.is_empty());
    assert!(sent
        .iter()
        .all(|request| request != &client && request != &stacking));
    assert!(
        deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_notify(window, 1)).is_empty()
    );
    assert!(wm.map_order.is_empty());
    assert!(wm.stack.is_empty());
}

fn mapped_pair(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &crate::xwayland::surfaces::SurfaceSerials,
) -> (Window, Window) {
    let first = Window(0x50);
    let second = Window(0x51);
    deliver(wm, state, dispatcher, surfaces, created(first, 1, 2, 10, 12, 0));
    deliver(wm, state, dispatcher, surfaces, created(second, 3, 4, 10, 12, 0));
    deliver(wm, state, dispatcher, surfaces, map_request(first));
    deliver(wm, state, dispatcher, surfaces, map_notify(first, 0));
    deliver(wm, state, dispatcher, surfaces, map_request(second));
    deliver(wm, state, dispatcher, surfaces, map_notify(second, 0));
    (first, second)
}

fn map_notify(window: Window, override_redirect: u8) -> Event {
    Event::MapNotify(MapNotify {
        send_event: false,
        sequence: 0,
        event: Window(0x100),
        window,
        override_redirect,
    })
}

fn unmapped(window: Window) -> Event {
    Event::UnmapNotify(UnmapNotify {
        send_event: false,
        sequence: 0,
        event: Window(0x100),
        window,
        from_configure: 0,
    })
}

fn destroyed(window: Window) -> Event {
    Event::DestroyNotify(DestroyNotify {
        send_event: false,
        sequence: 0,
        event: Window(0x100),
        window,
    })
}

fn window_list(atoms: &Atoms, property: Atom, windows: &[Window]) -> Vec<u8> {
    let ids: Vec<u32> = windows.iter().map(|window| window.0).collect();
    testing::encode(&change_property_u32s(Window(0x100), property, atoms.WINDOW, &ids).request(), 0)
}

fn placed_content(
    anchor: RectI,
    bounds: RectI,
    insets: crate::config::DecorationInsets,
    content_width: i32,
    content_height: i32,
) -> RectI {
    let mut frame_width = content_width + insets.left + insets.right;
    let mut frame_height = content_height + insets.top + insets.bottom;
    if frame_width > bounds.width {
        frame_width = bounds.width;
    }
    if frame_height > bounds.height {
        frame_height = bounds.height;
    }
    let frame_x = clamp_placed(
        anchor.x + (anchor.width - frame_width) / 2,
        frame_width,
        bounds.x,
        bounds.width,
    );
    let frame_y = clamp_placed(
        anchor.y + (anchor.height - frame_height) / 2,
        frame_height,
        bounds.y,
        bounds.height,
    );
    RectI {
        x: frame_x + insets.left,
        y: frame_y + insets.top,
        width: frame_width - insets.left - insets.right,
        height: frame_height - insets.top - insets.bottom,
    }
}

fn clamp_placed(origin: i32, size: i32, bounds_origin: i32, bounds_size: i32) -> i32 {
    if bounds_size <= size {
        return bounds_origin;
    }
    origin.clamp(bounds_origin, bounds_origin + bounds_size - size)
}

fn hidden_workspace(
    state: &crate::State,
    output: crate::state::OutputId,
) -> crate::state::WorkspaceId {
    let group = state.workspace_group_for_output(output).expect("group");
    let active = state.active_workspace_for_output(output);
    state
        .workspace_groups
        .try_get(group)
        .expect("group")
        .workspaces
        .iter()
        .copied()
        .find(|workspace| Some(*workspace) != active)
        .expect("hidden workspace")
}

fn add_output(state: &mut crate::State) -> crate::state::OutputId {
    state.insert_output(crate::test_support::test_output_drm0_wh(800, 600))
}

fn encoded_configure(window: Window, rect: RectI) -> Vec<u8> {
    testing::encode(
        &ConfigureWindow {
            window,
            value_list: ConfigureWindowAux::default()
                .x(rect.x)
                .y(rect.y)
                .width(u32::try_from(rect.width).unwrap())
                .height(u32::try_from(rect.height).unwrap())
                .border_width(0),
        },
        0,
    )
}

#[test]
fn override_redirect_windows_draw_in_map_order() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    let first = Window(0x50);
    let second = Window(0x51);
    let earlier = hold_override_redirect(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        first,
        80,
        1,
        10,
        20,
        30,
        16,
    );
    let later = hold_override_redirect(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        second,
        81,
        2,
        10,
        20,
        30,
        16,
    );

    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_notify(second, 1));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_notify(first, 1));

    let scene = scene_surfaces(&state);
    assert_eq!(scene.iter().position(|surface| *surface == later), Some(0));
    assert_eq!(scene.iter().position(|surface| *surface == earlier), Some(1));
}

#[test]
fn an_override_redirect_window_draws_above_a_floating_x_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let dialog = manage_with(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        Window(0x50),
        80,
        1,
        10,
        12,
        "Ask",
        "App",
        |ready, property| {
            (property == ready._NET_WM_WINDOW_TYPE)
                .then(|| cardinals(&[ready._NET_WM_WINDOW_TYPE_DIALOG.0]))
        },
    );
    let menu =
        pair_mapped(&mut wm, &mut state, &mut dispatcher, &mut surfaces, Window(0x51), 81, 2, 1);

    let scene = scene_surfaces(&state);
    let dialog_at = scene
        .iter()
        .position(|surface| *surface == dialog.surface)
        .expect("dialog");
    let menu_at = scene
        .iter()
        .position(|surface| *surface == menu)
        .expect("menu");
    assert!(menu_at > dialog_at);
    assert_eq!(scene_role(&state, menu), Some(SceneRole::UnmanagedRoot));
}

#[test]
fn pick_at_an_overlap_returns_the_later_override_redirect_window() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let first = Window(0x50);
    let second = Window(0x51);
    let earlier = hold_override_redirect(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        first,
        80,
        1,
        10,
        20,
        30,
        16,
    );
    let later = hold_override_redirect(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &mut surfaces,
        second,
        81,
        2,
        12,
        22,
        30,
        16,
    );
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_notify(second, 1));
    deliver(&mut wm, &mut state, &mut dispatcher, &surfaces, map_notify(first, 1));

    assert_eq!(super::tests::picked(&state, (20.0, 28.0)), Some(earlier));
    assert_ne!(earlier, later);
}

#[test]
fn a_resize_grab_on_a_floating_x_window_configures_the_new_size() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let dialog = floating_dialog(&mut wm, &mut state, &mut dispatcher, &mut surfaces);
    let root = super::tests::root_position(&state, dialog.surface);
    state.seat.pointer_pos = (100.0, 100.0);
    state.seat.start_toplevel_resize(
        crate::clients::ClientId::from_raw(1),
        dialog.window,
        dialog.surface,
        crate::window::InteractiveSizeConstraints::new(None, None),
        crate::tiling::EDGE_RIGHT,
        grab_geometry(root, 10, 12),
    );
    state.seat.pointer_pos = (140.0, 100.0);
    assert!(crate::input::apply_pointer_interactive_grab(&mut state, &mut dispatcher));
    let queued = dispatcher.test_take_queued_actions::<crate::xwayland::XwaylandAction>();
    assert!(matches!(
        queued.as_slice(),
        [crate::xwayland::XwaylandAction::Wm {
            run: 1,
            event: WmEvent::Resize { window, width: 50, height: 12 },
        }] if *window == Window(0x50)
    ));
    let sent = handle_event(
        &mut wm,
        &mut state,
        &mut dispatcher,
        &surfaces,
        WmEvent::Resize { window: Window(0x50), width: 50, height: 12 },
    );
    expect_requests(
        sent,
        vec![encoded_configure(
            Window(0x50),
            RectI { x: root.0, y: root.1, width: 50, height: 12 },
        )],
    );
}

#[test]
fn a_floating_move_sends_the_new_position() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
    add_output(&mut state);
    let dialog = floating_dialog(&mut wm, &mut state, &mut dispatcher, &mut surfaces);
    let root = super::tests::root_position(&state, dialog.surface);
    state.seat.pointer_pos = (0.0, 0.0);
    state.seat.start_toplevel_move(
        crate::clients::ClientId::from_raw(1),
        dialog.window,
        dialog.surface,
        grab_geometry(root, 10, 12),
    );
    state.seat.pointer_pos = (25.0, -10.0);
    assert!(crate::input::apply_pointer_interactive_grab(&mut state, &mut dispatcher));
    let queued = dispatcher.test_take_queued_actions::<crate::xwayland::XwaylandAction>();
    assert!(matches!(
        queued.as_slice(),
        [crate::xwayland::XwaylandAction::Wm { run: 1, event: WmEvent::Reconfigure }]
    ));

    let sent = handle_event(&mut wm, &mut state, &mut dispatcher, &surfaces, WmEvent::Reconfigure);
    expect_requests(
        sent,
        vec![encoded_configure(
            Window(0x50),
            RectI { x: root.0 + 25, y: root.1 - 10, width: 10, height: 12 },
        )],
    );
}

#[test]
fn a_tiled_resize_reconfigures_both_neighbors() {
    let (mut wm, mut state, mut dispatcher, mut surfaces) = open_wm();
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
        "App",
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
        "App",
    );
    reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces);
    let before_first = state
        .tiled_window_geometry(first.window)
        .expect("first")
        .content_rect_global();
    let before_second = state
        .tiled_window_geometry(second.window)
        .expect("second")
        .content_rect_global();

    assert!(state.resize_tiled_window(first.window, crate::tiling::EDGE_RIGHT, 40, 0));

    let after_first = state
        .tiled_window_geometry(first.window)
        .expect("resized first")
        .content_rect_global();
    let after_second = state
        .tiled_window_geometry(second.window)
        .expect("resized second")
        .content_rect_global();
    assert_ne!(after_first, before_first);
    assert_ne!(after_second, before_second);
    expect_requests(
        reconfigure(&mut wm, &mut state, &mut dispatcher, &surfaces),
        vec![
            encoded_configure(Window(0x50), after_first),
            encoded_configure(Window(0x51), after_second),
        ],
    );
}

fn floating_dialog(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &mut crate::xwayland::surfaces::SurfaceSerials,
) -> ManagedWindow {
    manage_with(
        wm,
        state,
        dispatcher,
        surfaces,
        Window(0x50),
        80,
        1,
        10,
        12,
        "Ask",
        "App",
        |ready, property| {
            (property == ready._NET_WM_WINDOW_TYPE)
                .then(|| cardinals(&[ready._NET_WM_WINDOW_TYPE_DIALOG.0]))
        },
    )
}

fn grab_geometry(
    root: (i32, i32),
    width: i32,
    height: i32,
) -> crate::root_geometry::WindowTargetGeometry {
    let rect = RectI { x: root.0, y: root.1, width, height };
    crate::root_geometry::WindowTargetGeometry {
        root_pos: root,
        resize_size: (width, height),
        target_rect: rect,
        binding_rect: rect,
    }
}

fn hold_override_redirect(
    wm: &mut Wm,
    state: &mut crate::State,
    dispatcher: &mut Dispatcher,
    surfaces: &mut crate::xwayland::surfaces::SurfaceSerials,
    window: Window,
    raw: u32,
    serial: u64,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
) -> Resolved<WlSurfaceId> {
    deliver(wm, state, dispatcher, surfaces, created(window, x, y, width, height, 1));
    let surface = buffered_x_root(state, raw, serial, i32::from(width), i32::from(height));
    let assigned = SurfaceEvent::SerialAssigned { serial, surface };
    surfaces.apply(assigned);
    let atom = atoms(wm).WL_SURFACE_SERIAL;
    deliver(wm, state, dispatcher, surfaces, serial_message(window, atom, serial));
    surface
}

fn scene_surfaces(state: &crate::State) -> Vec<Resolved<WlSurfaceId>> {
    state
        .build_scene_model()
        .entries
        .into_iter()
        .map(|entry| entry.root_surface)
        .collect()
}

fn scene_role(state: &crate::State, surface: Resolved<WlSurfaceId>) -> Option<SceneRole> {
    state
        .build_scene_model()
        .entries
        .into_iter()
        .find(|entry| entry.root_surface == surface)
        .map(|entry| entry.role)
}
