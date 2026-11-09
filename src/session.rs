//! Live xdg-session-management sessions, and when the manager global is advertised.

use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    clients::{Client, ClientAction, ClientId, ClientManagerAction, Resolved},
    protocol_generated::{Constants, XdgSessionV1Id, XdgToplevelSessionV1Id},
    region::RectI,
    root_geometry::{project_configure_geometry, CommittedRootGeometry},
    runner::Dispatcher,
    session_store::{FloatingRecord, ToplevelRecord},
    state::{ManagedWindowMode, OutputId, RememberedFloatingState, State},
    wayland_objects::xdg_session_manager_v1::{XdgSessionManagerV1, XdgSessionManagerV1Global},
    window::{Toplevel, WindowId},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionReason {
    Launch,
    Recover,
    SessionRestore,
}

pub(crate) struct LiveToplevel {
    pub object: Resolved<XdgToplevelSessionV1Id>,
    pub window: WindowId,
}

pub(crate) struct LiveSession {
    pub client: ClientId,
    pub object: Resolved<XdgSessionV1Id>,
    pub reason: SessionReason,
    pub toplevels: BTreeMap<String, LiveToplevel>,
}

/// Layout of one live toplevel. `None` leaves the stored record alone.
pub(crate) fn sample(state: &State, window: WindowId) -> Option<ToplevelRecord> {
    let managed = state.managed_window_state(window)?;
    let workspace_id = managed.workspace_id;
    let mode = managed.mode;
    let remembered = managed.remembered_floating_state;
    let output_id = state.workspace_output_id(workspace_id);
    let origin = output_origin(state, output_id);
    let output =
        output_id.and_then(|id| state.outputs.try_get(id).map(|output| output.name.clone()));
    let workspace = state
        .workspaces
        .try_get(workspace_id)
        .map(|workspace| workspace.name.clone());
    let fullscreen = state.window_is_fullscreen(window);
    let floating = match mode {
        ManagedWindowMode::Tiled => None,
        ManagedWindowMode::Floating if fullscreen => {
            remembered.map(|saved| floating_from_remembered(saved, origin))
        }
        ManagedWindowMode::Floating => {
            let root = state.window_root_surface(window)?;
            let geometry = state.committed_root_geometry(root)?;
            Some(floating_from_geometry(geometry, origin))
        }
    };
    Some(ToplevelRecord {
        last_used: unix_now(),
        output,
        workspace,
        floating,
        fullscreen,
    })
}

pub(crate) fn sample_into_store(state: &mut State, session_id: &str, name: &str, window: WindowId) {
    let Some(record) = sample(state, window) else {
        return;
    };
    if let Some(store) = state.session_store.as_mut() {
        store.put_toplevel(session_id, name, record);
    }
}

/// Re-sample every live toplevel after a layout change.
pub(crate) fn note_window_changed(state: &mut State, dispatcher: &mut Dispatcher) {
    let live = state
        .live_sessions
        .iter()
        .flat_map(|(session_id, live)| {
            live.toplevels
                .iter()
                .map(|(name, toplevel)| (session_id.clone(), name.clone(), toplevel.window))
        })
        .collect::<Vec<_>>();
    for (session_id, name, window) in live {
        sample_into_store(state, &session_id, &name, window);
    }
    crate::session_store::model::note_dirty(state, dispatcher);
}

/// Place a restored toplevel before its first configure. False skips the `restored` event.
pub(crate) fn apply_restore(
    state: &mut State,
    window: WindowId,
    has_parent: bool,
    session_id: &str,
    record: &ToplevelRecord,
) -> bool {
    if has_parent && record.floating.is_none() && !record.fullscreen {
        return false;
    }
    if state.managed_window_state(window).is_none() {
        return false;
    }
    let saved_output = record.output.as_deref().and_then(|name| {
        state
            .outputs
            .iter()
            .find(|(_, output)| output.enabled && output.name == name)
            .map(|(id, _)| id)
    });
    let saved_workspace = saved_output.and_then(|output| {
        record
            .workspace
            .as_deref()
            .and_then(|name| workspace_named(state, output, name))
    });
    let reason = state.live_sessions.get(session_id).map(|live| live.reason);
    let target = match (reason, saved_workspace, saved_output) {
        (Some(SessionReason::Recover), Some(workspace), _) => Some(workspace),
        (Some(SessionReason::Launch | SessionReason::SessionRestore), Some(workspace), _)
            if state.workspace_is_active(workspace) =>
        {
            Some(workspace)
        }
        (_, _, Some(output)) => state.active_workspace_for_output(output),
        _ => None,
    };
    if let Some(workspace) = target {
        let current = state
            .managed_window_state(window)
            .map(|managed| managed.workspace_id);
        if current != Some(workspace) {
            state.assign_window_to_workspace(window, workspace);
        }
    }
    if let Some(floating) = record.floating.as_ref() {
        place_floating(state, window, saved_output, floating);
    }
    if record.fullscreen {
        state.set_window_fullscreen(window, true);
        if let Some(toplevel) = state.toplevel_mut(window) {
            Toplevel::set_requested_fullscreen(toplevel, true);
        }
    }
    true
}

fn output_origin(state: &State, output: Option<OutputId>) -> (i32, i32) {
    output
        .and_then(|id| state.output_bounds(id))
        .map(|bounds| (bounds.x, bounds.y))
        .unwrap_or((0, 0))
}

fn workspace_named(
    state: &State,
    output: OutputId,
    name: &str,
) -> Option<crate::state::WorkspaceId> {
    let group_id = state.workspace_group_for_output(output)?;
    let group = state.workspace_groups.try_get(group_id)?;
    group.workspaces.iter().copied().find(|id| {
        state
            .workspaces
            .try_get(*id)
            .is_some_and(|workspace| workspace.name == name)
    })
}

fn floating_from_geometry(geometry: CommittedRootGeometry, origin: (i32, i32)) -> FloatingRecord {
    let size = project_configure_geometry(geometry).size;
    let frame = geometry.layout_rect_global;
    FloatingRecord {
        x: geometry.root_pos.0 - origin.0,
        y: geometry.root_pos.1 - origin.1,
        width: size.0,
        height: size.1,
        frame_x: frame.x - origin.0,
        frame_y: frame.y - origin.1,
        frame_width: frame.width,
        frame_height: frame.height,
    }
}

fn floating_from_remembered(saved: RememberedFloatingState, origin: (i32, i32)) -> FloatingRecord {
    FloatingRecord {
        x: saved.root_pos.0 - origin.0,
        y: saved.root_pos.1 - origin.1,
        width: saved.configure_size.0,
        height: saved.configure_size.1,
        frame_x: saved.layout_rect.x - origin.0,
        frame_y: saved.layout_rect.y - origin.1,
        frame_width: saved.layout_rect.width,
        frame_height: saved.layout_rect.height,
    }
}

fn place_floating(
    state: &mut State,
    window: WindowId,
    saved_output: Option<OutputId>,
    floating: &FloatingRecord,
) {
    let Some(workspace_id) = state
        .managed_window_state(window)
        .map(|managed| managed.workspace_id)
    else {
        return;
    };
    let Some(target_out) = state.workspace_output_id(workspace_id) else {
        return;
    };
    let Some(bounds) =
        crate::wayland_objects::zwlr_layer_surface_v1::output_usable_bounds(state, target_out)
            .or_else(|| state.output_bounds(target_out))
    else {
        return;
    };
    let same_output = saved_output == Some(target_out);
    let origin = if same_output {
        output_origin(state, Some(target_out))
    } else {
        output_origin(state, saved_output)
    };
    let mut root_pos = (origin.0 + floating.x, origin.1 + floating.y);
    let mut layout = RectI {
        x: origin.0 + floating.frame_x,
        y: origin.1 + floating.frame_y,
        width: floating.frame_width,
        height: floating.frame_height,
    };
    if !same_output {
        let centered_x = bounds.x + (bounds.width - floating.frame_width) / 2;
        let centered_y = bounds.y + (bounds.height - floating.frame_height) / 2;
        let dx = centered_x - layout.x;
        let dy = centered_y - layout.y;
        root_pos.0 += dx;
        root_pos.1 += dy;
        layout.x += dx;
        layout.y += dy;
    }
    let dx = shift_into_view(layout.x, layout.width, bounds.x, bounds.width);
    let dy = shift_into_view(layout.y, layout.height, bounds.y, bounds.height);
    root_pos.0 += dx;
    root_pos.1 += dy;
    layout.x += dx;
    layout.y += dy;
    if let Some(managed) = state.managed_window_state_mut(window) {
        managed.remembered_floating_state = Some(RememberedFloatingState {
            root_pos,
            configure_size: (floating.width, floating.height),
            layout_rect: layout,
        });
    }
    state.set_window_floating(window, true);
}

/// Shift `pos` so at least `min(64, size)` pixels of the span stay inside the bounds.
fn shift_into_view(pos: i32, size: i32, bounds_pos: i32, bounds_size: i32) -> i32 {
    let size = size.max(0);
    let visible = 64.min(size);
    if visible == 0 {
        return 0;
    }
    let min_pos = bounds_pos + visible - size;
    let max_pos = bounds_pos + bounds_size - visible;
    if pos < min_pos {
        min_pos - pos
    } else if pos > max_pos {
        max_pos - pos
    } else {
        0
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The toplevel is grabbed by its destroy path. This only updates the session side.
pub(crate) fn toplevel_gone(
    state: &mut State,
    _client: &mut Client,
    session: Resolved<XdgToplevelSessionV1Id>,
    window: WindowId,
) {
    let Some((session_id, name, inert)) = session.try_ref(state).map(|object| {
        (object.session_id().to_string(), object.name().to_string(), object.is_inert())
    }) else {
        return;
    };
    if inert {
        return;
    }
    sample_into_store(state, &session_id, &name, window);
    if let Some(object) = session.try_ref_mut(state) {
        object.make_inert();
    }
    if let Some(live) = state.live_sessions.get_mut(&session_id) {
        live.toplevels.remove(&name);
    }
}

pub(crate) fn detach_toplevel_session(
    state: &mut State,
    session: Resolved<XdgToplevelSessionV1Id>,
) {
    let toplevel = session
        .try_ref_mut(state)
        .and_then(|object| object.make_inert());
    if let Some(toplevel) = toplevel {
        if let Some(toplevel) = toplevel.try_ref_mut(state) {
            toplevel.set_session(None);
        }
    }
}

/// Drop a live session. `sample` writes each toplevel before its session object goes inert.
pub(crate) fn release_live_session(state: &mut State, session_id: &str, sample: bool) {
    let Some(live) = state.live_sessions.remove(session_id) else {
        return;
    };
    for (name, toplevel) in live.toplevels {
        if sample {
            sample_into_store(state, session_id, &name, toplevel.window);
        }
        detach_toplevel_session(state, toplevel.object);
    }
}

/// Another client is taking this id. The old client hears `replaced` from its own queue.
pub(crate) fn take_over_live_session(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    session_id: &str,
) {
    let Some(live) = state.live_sessions.remove(session_id) else {
        return;
    };
    dispatcher.dispatch_to(live.client, ClientAction::SessionReplaced { session: live.object });
    for (name, toplevel) in live.toplevels {
        sample_into_store(state, session_id, &name, toplevel.window);
        detach_toplevel_session(state, toplevel.object);
    }
    if let Some(session) = live.object.try_ref_mut(state) {
        session.set_inert();
    }
}

pub(crate) fn sync_session_manager_global(state: &mut State, dispatcher: &mut Dispatcher) {
    let want = state.config.session_management.enabled && state.session_store.is_some();
    match (want, state.session_manager_global) {
        (true, None) => {
            let global = XdgSessionManagerV1Global::new();
            let (interface, version) = global.interface();
            let name = state.insert_global(global);
            state.session_manager_global = Some(name);
            dispatcher.dispatch(ClientManagerAction::RegistryGlobalAdded {
                name: name.as_u32(),
                interface: interface.to_string(),
                version,
            });
        }
        (false, Some(name)) => {
            let _ = state.remove_global(name);
            state.session_manager_global = None;
            dispatcher.dispatch(ClientManagerAction::RegistryGlobalRemoved { name: name.as_u32() });
        }
        (true, Some(_)) | (false, None) => {}
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            clients::{Client, RawEvent, RawRequest, Resolved},
            input::ActiveGrabState,
            io_uring::model::IoUringAction,
            object::Object,
            protocol_generated::{
                xdg_toplevel, WaylandId, WlSurfaceId, XdgSessionV1Id, XdgSurfaceId, XdgToplevelId,
                XdgToplevelSessionV1Id, XdgWmBaseId,
            },
            region::RectI,
            root_geometry::{CommittedRootGeometry, WindowTargetGeometry},
            runner::Dispatcher,
            session_store::{FloatingRecord, SessionStore, ToplevelRecord},
            state::{ManagedWindowMode, ObjectId, OutputId, RootGeometryCacheEntry, State},
            test_support::{
                get_toplevel_request, raw_request_from_event, state_for_tests, test_output_drm0_wh,
            },
            wayland_objects::{
                wl_surface::{test_commit_new_buffer, Role, WlSurface},
                xdg_session_v1,
                xdg_surface::XdgSurface,
                xdg_toplevel_session_v1::XdgToplevelSessionV1,
            },
            window::WindowId,
        },
        std::{cell::Cell, collections::BTreeMap, path::PathBuf},
    };

    #[test]
    fn sync_session_manager_global_adds_then_withdraws() {
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        sync_session_manager_global(&mut state, &mut dispatcher);
        assert!(state.session_manager_global.is_none());

        state.session_store = Some(SessionStore::load(PathBuf::from("missing-sessions.json")));
        state.config.session_management.enabled = false;
        sync_session_manager_global(&mut state, &mut dispatcher);
        assert!(state.session_manager_global.is_none());

        state.config.session_management.enabled = true;
        sync_session_manager_global(&mut state, &mut dispatcher);
        let name = state.session_manager_global.unwrap();
        assert_eq!(
            state
                .globals
                .name_for_interface(XdgSessionManagerV1::INTERFACE_NAME),
            Some(name)
        );
        assert!(dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(
                action,
                ClientManagerAction::RegistryGlobalAdded {
                    name: added,
                    interface,
                    version: 1,
                } if *added == name.as_u32() && interface == XdgSessionManagerV1::INTERFACE_NAME
            )
        }));
        sync_session_manager_global(&mut state, &mut dispatcher);
        assert_eq!(
            dispatcher.test_count_queued_action(|action: &ClientManagerAction| {
                matches!(action, ClientManagerAction::RegistryGlobalAdded { .. })
            }),
            1
        );

        state.session_store = None;
        sync_session_manager_global(&mut state, &mut dispatcher);
        assert!(state.session_manager_global.is_none());
        assert!(state
            .globals
            .name_for_interface(XdgSessionManagerV1::INTERFACE_NAME)
            .is_none());
        assert!(dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::RegistryGlobalRemoved { name: removed } if *removed == name.as_u32())
        }));
    }

    struct Shell {
        state: State,
        client: Client,
        dispatcher: Dispatcher,
        output: OutputId,
        surface: Resolved<WlSurfaceId>,
        xdg: Resolved<XdgSurfaceId>,
        toplevel: Resolved<XdgToplevelId>,
        window: WindowId,
        session_id: String,
        session: Resolved<XdgSessionV1Id>,
    }

    fn new_shell(reason: SessionReason) -> Shell {
        let mut state = state_for_tests();
        state.session_store = Some(SessionStore::load(PathBuf::from("missing-sessions.json")));
        let output = state.insert_output(test_output_drm0_wh(1280, 720));
        let (mut client, mut dispatcher) = crate::test_support::test_client_and_dispatcher(1);
        let surface = install_toplevel(&mut client, &mut state, &mut dispatcher, 20, 21, 22);
        let session_id = "sess".to_string();
        state
            .session_store
            .as_mut()
            .unwrap()
            .touch_session(&session_id, 1);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let session = xdg_session_v1::open(
            &mut client,
            &mut state,
            &mut client_dispatcher,
            XdgSessionV1Id::from(30),
            session_id.clone(),
            false,
        );
        drop(client_dispatcher);
        state.live_sessions.insert(
            session_id.clone(),
            LiveSession {
                client: client.client_id(),
                object: session,
                reason,
                toplevels: BTreeMap::new(),
            },
        );
        let toplevel = client.resolve(XdgToplevelId::from(22), &state).unwrap();
        let window = state.window_for_root(surface.0).unwrap();
        Shell {
            state,
            client,
            dispatcher,
            output,
            surface: surface.0,
            xdg: surface.1,
            toplevel,
            window,
            session_id,
            session,
        }
    }

    fn install_toplevel(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        surface: u32,
        xdg: u32,
        toplevel: u32,
    ) -> (Resolved<WlSurfaceId>, Resolved<XdgSurfaceId>) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        let wl_id = WlSurfaceId::from(surface);
        let (root, _) = client.insert_object(state, WlSurface::new(wl_id, client.client_id(), 6));
        {
            let mut surface = state.objects.grab_object::<WlSurface>(root.object_id());
            surface
                .set_role(Role::XdgSurface, &mut client_dispatcher)
                .unwrap();
        }
        state.register_root_surface(root);
        let xdg_id = XdgSurfaceId::from(xdg);
        let (xdg_surface, _) = client.insert_object(
            state,
            XdgSurface::new(
                xdg_id,
                Resolved::new(
                    client.client_id(),
                    XdgWmBaseId::from(1),
                    ObjectId::from(1_000_000usize),
                ),
                root,
                7,
            ),
        );
        state
            .objects
            .grab_object::<WlSurface>(root.object_id())
            .set_xdg_surface_impl(xdg_surface);
        state
            .objects
            .grab_object::<XdgSurface>(xdg_surface.object_id())
            .on(
                client,
                state,
                get_toplevel_request(xdg_id, XdgToplevelId::from(toplevel)),
                &mut client_dispatcher,
            )
            .unwrap();
        (root, xdg_surface)
    }

    fn request(shell: &mut Shell, object: ObjectId, wire: u32, request: RawRequest<'static>) {
        let mut client_dispatcher = shell.client.test_dispatcher(&mut shell.dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(WaylandId::from(wire));
        shell
            .state
            .objects
            .grab(object)
            .on(&mut shell.client, &mut shell.state, request, &mut client_dispatcher)
            .unwrap();
    }

    fn restore_main(shell: &mut Shell) {
        let mut event = RawEvent::new(WaylandId::from(30), 3, 3);
        event.id(XdgToplevelSessionV1Id::from(40));
        event.id(XdgToplevelId::from(22));
        event.string("main");
        let session = shell.session.object_id();
        request(shell, session, 30, raw_request_from_event(event));
    }

    fn store_record(shell: &mut Shell, record: ToplevelRecord) {
        shell
            .state
            .session_store
            .as_mut()
            .unwrap()
            .put_toplevel(&shell.session_id, "main", record);
    }

    fn initial_commit(shell: &mut Shell) {
        let mut client_dispatcher = shell.client.test_dispatcher(&mut shell.dispatcher);
        let mut xdg = shell
            .client
            .grab_object(XdgSurfaceId::from(21), &mut shell.state);
        let role_changes = xdg
            .on_commit(
                &mut shell.client,
                &mut shell.state,
                &mut client_dispatcher,
                None,
                false,
                false,
                None,
            )
            .expect("initial commit");
        xdg.apply_commit_role_changes(
            &mut shell.client,
            &mut shell.state,
            &mut client_dispatcher,
            role_changes,
        );
    }

    fn messages(dispatcher: &Dispatcher) -> Vec<(u32, u32, Vec<u8>)> {
        let mut sent = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            sent.push((object, opcode, buffer[8..].to_vec()));
        });
        sent
    }

    fn event_index(dispatcher: &Dispatcher, object: u32, opcode: u32) -> Option<usize> {
        messages(dispatcher)
            .iter()
            .position(|(id, event, _)| *id == object && *event == opcode)
    }

    fn toplevel_configure(dispatcher: &Dispatcher) -> (i32, i32, Vec<u32>) {
        let (_, _, body) = messages(dispatcher)
            .into_iter()
            .find(|(id, opcode, body)| *id == 22 && *opcode == 0 && body.len() >= 12)
            .unwrap();
        let width = i32::from_ne_bytes(body[0..4].try_into().unwrap());
        let height = i32::from_ne_bytes(body[4..8].try_into().unwrap());
        let len = u32::from_ne_bytes(body[8..12].try_into().unwrap()) as usize;
        let mut states = Vec::new();
        let mut offset = 12;
        while offset + 4 <= 12 + len && offset + 4 <= body.len() {
            states.push(u32::from_ne_bytes(body[offset..offset + 4].try_into().unwrap()));
            offset += 4;
        }
        (width, height, states)
    }

    fn ack_initial_configure(shell: &mut Shell) {
        let serial = messages(&shell.dispatcher)
            .into_iter()
            .rev()
            .find_map(|(id, opcode, body)| {
                (id == 21 && opcode == 0 && body.len() >= 4)
                    .then(|| u32::from_ne_bytes(body[0..4].try_into().unwrap()))
            })
            .unwrap();
        let mut event = RawEvent::new(WaylandId::from(21), 4, 1);
        event.uint(serial);
        let xdg = shell.xdg.object_id();
        request(shell, xdg, 21, raw_request_from_event(event));
    }

    fn root_pos(shell: &Shell) -> (i32, i32) {
        shell.surface.try_ref(&shell.state).unwrap().root_position()
    }

    fn workspace_named(state: &State, name: &str) -> crate::state::WorkspaceId {
        state
            .workspaces
            .iter()
            .find(|(_, workspace)| workspace.name == name)
            .map(|(id, _)| id)
            .unwrap()
    }

    fn floating_record() -> FloatingRecord {
        FloatingRecord {
            x: 40,
            y: 50,
            width: 200,
            height: 100,
            frame_x: 30,
            frame_y: 40,
            frame_width: 220,
            frame_height: 120,
        }
    }

    fn track(shell: &mut Shell) {
        let (object, _) = shell.client.insert_object(
            &mut shell.state,
            XdgToplevelSessionV1::new(
                XdgToplevelSessionV1Id::from(40),
                shell.session_id.clone(),
                "main".to_string(),
                Some(shell.toplevel),
                false,
                shell.session,
            ),
        );
        shell
            .toplevel
            .try_ref_mut(&mut shell.state)
            .unwrap()
            .set_session(Some(object));
        shell
            .state
            .live_sessions
            .get_mut(&shell.session_id)
            .unwrap()
            .toplevels
            .insert("main".to_string(), LiveToplevel { object, window: shell.window });
    }

    #[test]
    fn sample_records_a_floating_window_relative_to_its_output() {
        let mut shell = new_shell(SessionReason::Launch);
        shell
            .state
            .managed_window_state_mut(shell.window)
            .unwrap()
            .mode = ManagedWindowMode::Floating;
        shell.state.root_geometry_cache.insert(
            shell.surface,
            Cell::new(RootGeometryCacheEntry {
                geometry: Some(CommittedRootGeometry {
                    layout_rect_global: RectI { x: 40, y: 50, width: 120, height: 90 },
                    root_pos: (40, 50),
                    surface_bounds_local: Some(RectI { x: 0, y: 0, width: 120, height: 90 }),
                    logical_window_local: Some(RectI { x: 0, y: 0, width: 120, height: 90 }),
                    decoration_mode: None,
                    decoration_insets: Default::default(),
                    tiled_edges: 0,
                    fullscreen: false,
                }),
                dirty: false,
                revision: 1,
                built_from_revision: 1,
            }),
        );
        track(&mut shell);
        note_window_changed(&mut shell.state, &mut shell.dispatcher);

        let record = shell
            .state
            .session_store
            .as_ref()
            .unwrap()
            .toplevel(&shell.session_id, "main")
            .unwrap()
            .clone();
        assert_eq!(record.output.as_deref(), Some("test-output"));
        assert_eq!(record.workspace.as_deref(), Some("1"));
        assert!(!record.fullscreen);
        assert_eq!(
            record.floating.unwrap(),
            FloatingRecord {
                x: 40,
                y: 50,
                width: 120,
                height: 90,
                frame_x: 40,
                frame_y: 50,
                frame_width: 120,
                frame_height: 90,
            }
        );

        shell.state.root_geometry_cache.insert(
            shell.surface,
            Cell::new(RootGeometryCacheEntry {
                geometry: None,
                dirty: false,
                revision: 2,
                built_from_revision: 2,
            }),
        );
        note_window_changed(&mut shell.state, &mut shell.dispatcher);
        assert_eq!(
            shell
                .state
                .session_store
                .as_ref()
                .unwrap()
                .toplevel(&shell.session_id, "main")
                .unwrap()
                .floating
                .as_ref()
                .unwrap()
                .x,
            40
        );
    }

    #[test]
    fn a_reopened_floating_window_comes_back_where_it_was() {
        let mut shell = new_shell(SessionReason::Launch);
        store_record(
            &mut shell,
            ToplevelRecord {
                output: Some("test-output".to_string()),
                workspace: Some("1".to_string()),
                floating: Some(floating_record()),
                ..ToplevelRecord::default()
            },
        );
        restore_main(&mut shell);
        initial_commit(&mut shell);

        let restored = event_index(&shell.dispatcher, 40, 0).unwrap();
        let configure = event_index(&shell.dispatcher, 22, 0).unwrap();
        assert!(restored < configure);
        assert_eq!(toplevel_configure(&shell.dispatcher).0, 200);
        assert_eq!(toplevel_configure(&shell.dispatcher).1, 100);
        assert_eq!(root_pos(&shell), (40, 50));

        ack_initial_configure(&mut shell);
        test_commit_new_buffer(
            &mut shell.client,
            &mut shell.state,
            &mut shell.dispatcher,
            shell.surface,
            200,
            100,
        );
        assert_eq!(root_pos(&shell), (40, 50));
        assert!(shell
            .surface
            .try_ref(&shell.state)
            .unwrap()
            .has_committed_buffer());
    }

    #[test]
    fn a_tiled_restore_lands_on_the_saved_active_workspace() {
        let mut shell = new_shell(SessionReason::Launch);
        store_record(
            &mut shell,
            ToplevelRecord {
                output: Some("test-output".to_string()),
                workspace: Some("1".to_string()),
                ..ToplevelRecord::default()
            },
        );
        restore_main(&mut shell);
        initial_commit(&mut shell);

        assert!(
            event_index(&shell.dispatcher, 40, 0).unwrap()
                < event_index(&shell.dispatcher, 22, 0).unwrap()
        );
        let managed = shell.state.managed_window_state(shell.window).unwrap();
        assert_eq!(managed.mode, ManagedWindowMode::Tiled);
        assert!(managed.remembered_floating_state.is_none());
        assert_eq!(managed.workspace_id, workspace_named(&shell.state, "1"));
    }

    #[test]
    fn launch_uses_the_active_workspace_and_recover_uses_the_hidden_one() {
        let mut shell = new_shell(SessionReason::Launch);
        let hidden = workspace_named(&shell.state, "2");
        assert!(shell.state.assign_window_to_workspace(shell.window, hidden));
        let record = ToplevelRecord {
            output: Some("test-output".to_string()),
            workspace: Some("2".to_string()),
            ..ToplevelRecord::default()
        };
        assert!(apply_restore(&mut shell.state, shell.window, false, &shell.session_id, &record));
        assert_eq!(
            shell
                .state
                .managed_window_state(shell.window)
                .unwrap()
                .workspace_id,
            workspace_named(&shell.state, "1")
        );

        let mut shell = new_shell(SessionReason::Recover);
        assert!(apply_restore(&mut shell.state, shell.window, false, &shell.session_id, &record));
        let hidden = workspace_named(&shell.state, "2");
        assert_eq!(
            shell
                .state
                .managed_window_state(shell.window)
                .unwrap()
                .workspace_id,
            hidden
        );
        assert!(!shell.state.workspace_is_active(hidden));
    }

    #[test]
    fn a_missing_output_centers_the_floating_frame() {
        let mut shell = new_shell(SessionReason::Launch);
        let record = ToplevelRecord {
            floating: Some(FloatingRecord {
                x: 10,
                y: 20,
                width: 200,
                height: 100,
                frame_x: 0,
                frame_y: 0,
                frame_width: 200,
                frame_height: 100,
            }),
            ..ToplevelRecord::default()
        };
        assert!(apply_restore(&mut shell.state, shell.window, false, &shell.session_id, &record));
        assert_eq!(root_pos(&shell), (550, 330));
        assert_eq!(
            shell
                .state
                .managed_window_state(shell.window)
                .unwrap()
                .remembered_floating_state
                .unwrap()
                .layout_rect,
            RectI { x: 540, y: 310, width: 200, height: 100 }
        );
    }

    #[test]
    fn a_far_floating_position_keeps_64px_visible() {
        let mut shell = new_shell(SessionReason::Launch);
        let record = ToplevelRecord {
            output: Some("test-output".to_string()),
            workspace: Some("1".to_string()),
            floating: Some(FloatingRecord {
                x: -4990,
                y: 20,
                width: 200,
                height: 100,
                frame_x: -5000,
                frame_y: 10,
                frame_width: 200,
                frame_height: 100,
            }),
            ..ToplevelRecord::default()
        };
        assert!(apply_restore(&mut shell.state, shell.window, false, &shell.session_id, &record));
        let remembered = shell
            .state
            .managed_window_state(shell.window)
            .unwrap()
            .remembered_floating_state
            .unwrap();
        assert_eq!(remembered.layout_rect.x + remembered.layout_rect.width, 64);
        assert_eq!(remembered.layout_rect.y, 10);
        assert_eq!(root_pos(&shell), (-126, 20));
    }

    #[test]
    fn a_child_with_a_tiled_record_is_not_restored() {
        let mut shell = new_shell(SessionReason::Launch);
        let parent = install_toplevel(
            &mut shell.client,
            &mut shell.state,
            &mut shell.dispatcher,
            60,
            61,
            62,
        );
        let _parent_window = shell.state.window_for_root(parent.0).unwrap();
        let mut event = RawEvent::new(WaylandId::from(22), 1, 1);
        event.id(XdgToplevelId::from(62));
        let toplevel = shell.toplevel.object_id();
        request(&mut shell, toplevel, 22, raw_request_from_event(event));
        store_record(
            &mut shell,
            ToplevelRecord {
                output: Some("test-output".to_string()),
                workspace: Some("2".to_string()),
                ..ToplevelRecord::default()
            },
        );
        restore_main(&mut shell);
        initial_commit(&mut shell);

        assert!(event_index(&shell.dispatcher, 40, 0).is_none());
        let managed = shell.state.managed_window_state(shell.window).unwrap();
        assert_eq!(managed.mode, ManagedWindowMode::Tiled);
        assert_eq!(managed.workspace_id, workspace_named(&shell.state, "1"));
    }

    #[test]
    fn an_unknown_restore_name_sends_no_restored_event() {
        let mut shell = new_shell(SessionReason::Launch);
        restore_main(&mut shell);
        initial_commit(&mut shell);
        assert!(event_index(&shell.dispatcher, 40, 0).is_none());
        assert_eq!(
            shell
                .state
                .managed_window_state(shell.window)
                .unwrap()
                .workspace_id,
            workspace_named(&shell.state, "1")
        );
    }

    #[test]
    fn fullscreen_records_configure_the_output_size() {
        let bounds = {
            let mut shell = new_shell(SessionReason::Launch);
            let bounds = crate::wayland_objects::zwlr_layer_surface_v1::output_usable_bounds(
                &shell.state,
                shell.output,
            )
            .unwrap();
            store_record(
                &mut shell,
                ToplevelRecord {
                    output: Some("test-output".to_string()),
                    workspace: Some("1".to_string()),
                    fullscreen: true,
                    ..ToplevelRecord::default()
                },
            );
            restore_main(&mut shell);
            initial_commit(&mut shell);
            let (width, height, states) = toplevel_configure(&shell.dispatcher);
            assert!(
                event_index(&shell.dispatcher, 40, 0).unwrap()
                    < event_index(&shell.dispatcher, 22, 0).unwrap()
            );
            assert_eq!((width, height), (bounds.width, bounds.height));
            assert!(states.contains(&(xdg_toplevel::State::Fullscreen as u32)));
            assert_eq!(
                shell.state.managed_window_state(shell.window).unwrap().mode,
                ManagedWindowMode::Tiled
            );
            bounds
        };

        let mut shell = new_shell(SessionReason::Launch);
        store_record(
            &mut shell,
            ToplevelRecord {
                output: Some("test-output".to_string()),
                workspace: Some("1".to_string()),
                floating: Some(floating_record()),
                fullscreen: true,
                ..ToplevelRecord::default()
            },
        );
        restore_main(&mut shell);
        initial_commit(&mut shell);
        let (width, height, states) = toplevel_configure(&shell.dispatcher);
        assert_eq!((width, height), (bounds.width, bounds.height));
        assert!(states.contains(&(xdg_toplevel::State::Fullscreen as u32)));
        assert_eq!(
            shell.state.managed_window_state(shell.window).unwrap().mode,
            ManagedWindowMode::Floating
        );
    }

    #[test]
    fn finishing_a_floating_move_updates_the_store() {
        let mut shell = new_shell(SessionReason::Launch);
        {
            let mut surface = shell
                .state
                .objects
                .grab_object::<WlSurface>(shell.surface.object_id());
            surface.test_map(ObjectId::from(70usize), 0, 0, 80, 60);
        }
        assert!(shell.state.set_window_floating(shell.window, true));
        track(&mut shell);
        note_window_changed(&mut shell.state, &mut shell.dispatcher);
        let before = shell
            .state
            .session_store
            .as_ref()
            .unwrap()
            .toplevel(&shell.session_id, "main")
            .unwrap()
            .floating
            .clone()
            .unwrap();

        shell.state.seat.pointer_pos = (40.0, 30.0);
        shell.state.seat.pointer_interactive_grab = Some(ActiveGrabState::Move {
            window: shell.window,
            root_surface: shell.surface,
            initial_pointer_pos: (0.0, 0.0),
            initial_geometry: WindowTargetGeometry {
                root_pos: root_pos(&shell),
                resize_size: (80, 60),
                target_rect: RectI { x: 0, y: 0, width: 80, height: 60 },
                binding_rect: RectI { x: 0, y: 0, width: 80, height: 60 },
            },
        });
        assert!(crate::input::apply_pointer_interactive_grab(
            &mut shell.state,
            &mut shell.dispatcher
        ));
        crate::input::finish_pointer_interactive_grab(&mut shell.state, &mut shell.dispatcher, 0);

        let after = shell
            .state
            .session_store
            .as_ref()
            .unwrap()
            .toplevel(&shell.session_id, "main")
            .unwrap()
            .floating
            .clone()
            .unwrap();
        assert_eq!(after.x, before.x + 40);
        assert_eq!(after.y, before.y + 30);
    }

    #[test]
    fn fullscreen_and_destroy_keep_the_sampled_state() {
        let mut shell = new_shell(SessionReason::Launch);
        {
            let mut surface = shell
                .state
                .objects
                .grab_object::<WlSurface>(shell.surface.object_id());
            surface.test_map(ObjectId::from(71usize), 0, 0, 80, 60);
        }
        assert!(shell.state.set_window_floating(shell.window, true));
        track(&mut shell);
        assert!(shell.state.set_window_fullscreen(shell.window, true));
        note_window_changed(&mut shell.state, &mut shell.dispatcher);
        assert!(
            shell
                .state
                .session_store
                .as_ref()
                .unwrap()
                .toplevel(&shell.session_id, "main")
                .unwrap()
                .fullscreen
        );

        let session = shell
            .state
            .live_sessions
            .get(&shell.session_id)
            .unwrap()
            .toplevels
            .get("main")
            .unwrap()
            .object;
        toplevel_gone(&mut shell.state, &mut shell.client, session, shell.window);
        assert!(
            shell
                .state
                .session_store
                .as_ref()
                .unwrap()
                .toplevel(&shell.session_id, "main")
                .unwrap()
                .fullscreen
        );
    }
}
