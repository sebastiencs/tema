//! A read-only copy of the compositor state the IPC socket publishes.
//!
//! Built from [`State`] on demand. Nothing here talks to a client or the dispatcher.

use crate::{
    capture,
    state::{ManagedWindowMode, OutputId, State, WorkspaceGroupId, WorkspaceId},
};

use super::proto;

#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct IpcSnapshot {
    pub focused_window: Option<proto::Window>,
    pub workspaces: Vec<proto::Workspace>,
    pub outputs: Vec<proto::Output>,
}

pub(crate) fn build(state: &State) -> IpcSnapshot {
    IpcSnapshot {
        focused_window: focused_window(state),
        workspaces: workspaces(state),
        outputs: outputs(state),
    }
}

pub(crate) fn topic(snapshot: &IpcSnapshot, topic: proto::Topic) -> Option<proto::Event> {
    match topic {
        proto::Topic::FocusedWindow => {
            Some(proto::Event::FocusedWindow { window: snapshot.focused_window.clone() })
        }
        proto::Topic::Workspaces => {
            Some(proto::Event::Workspaces { workspaces: snapshot.workspaces.clone() })
        }
        proto::Topic::Outputs => Some(proto::Event::Outputs { outputs: snapshot.outputs.clone() }),
        proto::Topic::Bell => None,
    }
}

/// `None` when nothing has keyboard focus, or the focused window has no open toplevel record.
fn focused_window(state: &State) -> Option<proto::Window> {
    let window = state.focused_window()?;
    let info = capture::toplevel_info(state, window)?;
    let managed = state.managed_window_state(window);
    let workspace_id = managed.map(|managed| managed.workspace_id);
    Some(proto::Window {
        id: window.as_u64(),
        title: info.title.to_string(),
        app_id: info.app_id.to_string(),
        workspace: workspace_id.and_then(|id| stable_id(state, id)),
        output: workspace_id.and_then(|id| first_output_name(state, id)),
        floating: managed.is_some_and(|managed| managed.mode == ManagedWindowMode::Floating),
        fullscreen: info.fullscreen,
        tag: info.tag.map(str::to_owned),
        description: info.description.map(str::to_owned),
        icon: info.icon.map(|icon| proto::WindowIconInfo {
            name: icon.name.clone(),
            sizes: icon
                .sizes
                .iter()
                .map(|&(size, scale)| proto::IconSize { size, scale })
                .collect(),
        }),
        modal: state
            .toplevel(window)
            .and_then(|toplevel| toplevel.modal_parent())
            .is_some_and(|parent| state.windows.contains_key(&parent)),
    })
}

pub(crate) fn window_icon(
    state: &State,
    id: u64,
    size: u32,
    scale: u32,
) -> Result<proto::WindowIcon, String> {
    let window = state
        .window_from_ipc_id(id)
        .ok_or_else(|| "no such window".to_string())?;
    let toplevel = state
        .toplevel(window)
        .ok_or_else(|| "no such window".to_string())?;
    let icon = toplevel.icon();
    let name = icon.and_then(|icon| icon.name.clone());
    let pixmap = icon.and_then(|icon| icon.best(size, scale));
    let (pixmap_size, pixmap_scale, data) = match pixmap {
        Some(pixmap) => {
            (Some(pixmap.size), Some(pixmap.scale), Some(proto::base64_encode(&pixmap.argb8888)))
        }
        None => (None, None, None),
    };
    Ok(proto::WindowIcon {
        id,
        name,
        size: pixmap_size,
        scale: pixmap_scale,
        format: "argb8888_premultiplied".to_string(),
        data,
    })
}

fn workspaces(state: &State) -> Vec<proto::Workspace> {
    let focused = state.focused_window().and_then(|window| {
        state
            .managed_window_state(window)
            .map(|managed| managed.workspace_id)
    });
    let mut rows = Vec::new();
    for (id, workspace) in state.workspaces.iter() {
        let outputs = workspace
            .group
            .map(|group| group_output_names(state, group))
            .unwrap_or_default();
        let index = workspace_index(state, id);
        rows.push((
            outputs.clone(),
            index,
            proto::Workspace {
                id: workspace.stable_id.clone(),
                name: workspace.name.clone(),
                index,
                coordinates: workspace.coordinates.clone(),
                outputs,
                active: state.workspace_is_active(id),
                focused: focused == Some(id),
                windows: u32::try_from(workspace.root_surfaces.len()).unwrap_or(u32::MAX),
            },
        ));
    }
    rows.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.id.cmp(&right.2.id))
    });
    rows.into_iter()
        .map(|(_, _, workspace)| workspace)
        .collect()
}

fn outputs(state: &State) -> Vec<proto::Output> {
    let mut rows: Vec<proto::Output> = state
        .outputs
        .iter()
        .map(|(id, output)| proto::Output {
            name: output.name.clone(),
            make: output.make.clone(),
            model: output.model.clone(),
            description: output.description.clone(),
            enabled: output.enabled,
            x: output.x,
            y: output.y,
            width: output.current_mode.width,
            height: output.current_mode.height,
            refresh_mhz: output.current_mode.refresh_millihz,
            scale: output.scale.as_f64(),
            active_workspace: active_workspace(state, id),
        })
        .collect();
    rows.sort_by(|left, right| left.name.cmp(&right.name));
    rows
}

fn active_workspace(state: &State, output_id: OutputId) -> Option<String> {
    let group_id = state.workspace_group_for_output(output_id)?;
    let workspace_id = state.workspace_groups.try_get(group_id)?.active_workspace?;
    stable_id(state, workspace_id)
}

fn stable_id(state: &State, workspace_id: WorkspaceId) -> Option<String> {
    state
        .workspaces
        .try_get(workspace_id)
        .map(|workspace| workspace.stable_id.clone())
}

fn first_output_name(state: &State, workspace_id: WorkspaceId) -> Option<String> {
    let group = state.workspaces.try_get(workspace_id)?.group?;
    group_output_names(state, group).into_iter().next()
}

/// Names of the group's outputs, sorted by name. Empty when the group is gone.
fn group_output_names(state: &State, group_id: WorkspaceGroupId) -> Vec<String> {
    let Some(group) = state.workspace_groups.try_get(group_id) else {
        return Vec::new();
    };
    let mut named: Vec<(String, OutputId)> = group
        .outputs
        .iter()
        .filter_map(|output_id| {
            let output = state.outputs.try_get(*output_id)?;
            Some((output.name.clone(), *output_id))
        })
        .collect();
    named.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    named.into_iter().map(|(name, _)| name).collect()
}

/// 1-based position in the group's workspace list. `0` when the workspace has no group.
fn workspace_index(state: &State, workspace_id: WorkspaceId) -> u32 {
    let Some(group_id) = state
        .workspaces
        .try_get(workspace_id)
        .and_then(|workspace| workspace.group)
    else {
        return 0;
    };
    let Some(position) = state
        .workspace_groups
        .try_get(group_id)
        .and_then(|group| group.workspaces.iter().position(|id| *id == workspace_id))
    else {
        return 0;
    };
    u32::try_from(position + 1).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use crate::{
        capture::{self, toplevel_info},
        clients::{Client, Resolved},
        input::SurfaceFocus,
        object::Object,
        protocol_generated::{WlSurfaceId, XdgSurfaceId, XdgToplevelId, XdgWmBaseId},
        runner::Dispatcher,
        state::{BackendOutputRef, DrmId, ManagedWindowMode, ObjectId, State},
        test_support::{
            get_toplevel_request, state_for_tests, test_client_and_dispatcher, test_output,
        },
        wayland_objects::{
            wl_surface::{Role, WlSurface},
            xdg_surface::{xdg_toplevel::XdgToplevel, XdgSurface},
        },
        window::WindowId,
    };

    use super::*;

    #[test]
    fn an_empty_state_has_no_focus_and_no_lists() {
        let state = state_for_tests();
        let snapshot = build(&state);
        assert_eq!(snapshot, IpcSnapshot::default());
        assert_eq!(
            topic(&snapshot, proto::Topic::FocusedWindow),
            Some(proto::Event::FocusedWindow { window: None })
        );
        assert_eq!(
            topic(&snapshot, proto::Topic::Workspaces),
            Some(proto::Event::Workspaces { workspaces: Vec::new() })
        );
        assert_eq!(
            topic(&snapshot, proto::Topic::Outputs),
            Some(proto::Event::Outputs { outputs: Vec::new() })
        );
        assert_eq!(topic(&snapshot, proto::Topic::Bell), None);
        assert!(toplevel_info(&state, WindowId::for_tests(1)).is_none());
    }

    #[test]
    fn one_output_publishes_its_four_workspaces() {
        let mut state = state_for_tests();
        let output_id = insert_named_output(&mut state, "DP-1");
        let snapshot = build(&state);
        let stored = state.outputs.try_get(output_id).unwrap();

        assert!(snapshot.focused_window.is_none());
        assert_eq!(snapshot.outputs.len(), 1);
        let output = &snapshot.outputs[0];
        assert_eq!(output.name, "DP-1");
        assert_eq!(output.make, "Dell");
        assert_eq!(output.model, "U2723QE");
        assert_eq!(output.description, "Dell Inc. U2723QE");
        assert!(output.enabled);
        assert_eq!((output.x, output.y), (10, 20));
        assert_eq!((output.width, output.height), (2560, 1440));
        assert_eq!(output.refresh_mhz, stored.current_mode.refresh_millihz);
        assert_eq!(output.scale, stored.scale.as_f64());
        assert_eq!(output.active_workspace.as_deref(), Some("workspace-1"));

        assert_eq!(snapshot.workspaces.len(), 4);
        assert_eq!(
            snapshot
                .workspaces
                .iter()
                .filter(|workspace| workspace.active)
                .count(),
            1
        );
        for (offset, workspace) in snapshot.workspaces.iter().enumerate() {
            let position = u32::try_from(offset + 1).unwrap();
            assert_eq!(workspace.index, position);
            assert_eq!(workspace.name, position.to_string());
            assert_eq!(workspace.id, format!("workspace-{position}"));
            assert_eq!(workspace.coordinates, vec![position]);
            assert_eq!(workspace.outputs, vec!["DP-1".to_string()]);
            assert_eq!(workspace.windows, 0);
            assert!(!workspace.focused);
            assert_eq!(workspace.active, position == 1);
        }
    }

    #[test]
    fn workspaces_follow_output_name_and_disabled_outputs_stay_listed() {
        let mut state = state_for_tests();
        let later = insert_named_output(&mut state, "B");
        let _earlier = insert_named_output(&mut state, "A");
        state.outputs.try_get_mut(later).unwrap().enabled = false;

        let snapshot = build(&state);
        assert_eq!(
            snapshot
                .outputs
                .iter()
                .map(|output| output.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B"]
        );
        assert!(snapshot.outputs[0].enabled);
        assert!(!snapshot.outputs[1].enabled);
        assert_eq!(snapshot.outputs[0].active_workspace.as_deref(), Some("workspace-5"));
        assert_eq!(snapshot.outputs[1].active_workspace.as_deref(), Some("workspace-1"));
        assert_eq!(
            snapshot
                .workspaces
                .iter()
                .map(|workspace| {
                    (workspace.outputs[0].as_str(), workspace.index, workspace.id.as_str())
                })
                .collect::<Vec<_>>(),
            vec![
                ("A", 1, "workspace-5"),
                ("A", 2, "workspace-6"),
                ("A", 3, "workspace-7"),
                ("A", 4, "workspace-8"),
                ("B", 1, "workspace-1"),
                ("B", 2, "workspace-2"),
                ("B", 3, "workspace-3"),
                ("B", 4, "workspace-4"),
            ]
        );
        assert_eq!(
            snapshot
                .workspaces
                .iter()
                .filter(|workspace| workspace.active)
                .count(),
            2
        );
    }

    #[test]
    fn a_mapped_focused_window_fills_the_snapshot() {
        let mut fixture = mapped_window("nvim — ~/src", "Alacritty");
        fixture
            .state
            .managed_window_state_mut(fixture.window)
            .unwrap()
            .mode = ManagedWindowMode::Floating;
        capture::note_window_state(
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.window,
            false,
            true,
        );

        let snapshot = build(&fixture.state);
        assert_eq!(build(&fixture.state), snapshot);
        let window = snapshot.focused_window.clone().expect("focused window");
        assert_eq!(window.id, fixture.window.as_u64());
        assert_eq!(window.title, "nvim — ~/src");
        assert_eq!(window.app_id, "Alacritty");
        assert_eq!(window.workspace.as_deref(), Some("workspace-1"));
        assert_eq!(window.output.as_deref(), Some("DP-1"));
        assert!(window.floating);
        assert!(window.fullscreen);
        assert!(window.icon.is_none());
        assert_eq!(
            topic(&snapshot, proto::Topic::FocusedWindow),
            Some(proto::Event::FocusedWindow { window: Some(window) })
        );

        let focused: Vec<_> = snapshot
            .workspaces
            .iter()
            .filter(|workspace| workspace.focused)
            .map(|workspace| (workspace.id.as_str(), workspace.windows))
            .collect();
        assert_eq!(focused, vec![("workspace-1", 1)]);
        assert!(snapshot
            .workspaces
            .iter()
            .filter(|workspace| workspace.id != "workspace-1")
            .all(|workspace| !workspace.focused && workspace.windows == 0));

        capture::unmap_window(&mut fixture.state, &mut fixture.dispatcher, fixture.window);
        assert!(toplevel_info(&fixture.state, fixture.window).is_none());
        let after_close = build(&fixture.state);
        assert!(after_close.focused_window.is_none());
        assert!(after_close
            .workspaces
            .iter()
            .any(|workspace| workspace.focused));
    }

    struct Fixture {
        state: State,
        dispatcher: Dispatcher,
        window: WindowId,
        _client: Client,
    }

    fn insert_named_output(state: &mut State, name: &str) -> crate::state::OutputId {
        let backend = BackendOutputRef::new(DrmId::from(0usize));
        let mut output = test_output(backend, 1, 10, 20, 2560, 1440, name);
        output.make = "Dell".to_string();
        output.model = "U2723QE".to_string();
        output.description = "Dell Inc. U2723QE".to_string();
        state.insert_output(output)
    }

    fn mapped_window(title: &str, app_id: &str) -> Fixture {
        let mut state = state_for_tests();
        insert_named_output(&mut state, "DP-1");
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let (window, root) = {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let (root, _) = client.insert_object(
                &mut state,
                WlSurface::new(WlSurfaceId::from(20), client.client_id(), 6),
            );
            {
                let mut surface = state.objects.grab_object::<WlSurface>(root.object_id());
                assert!(surface
                    .set_role(Role::XdgSurface, &mut client_dispatcher)
                    .is_ok());
            }
            state.register_root_surface(root);
            let xdg_surface_id = XdgSurfaceId::from(21);
            let (xdg_surface, _) = client.insert_object(
                &mut state,
                XdgSurface::new(
                    xdg_surface_id,
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
                    &mut client,
                    &mut state,
                    get_toplevel_request(xdg_surface_id, XdgToplevelId::from(22)),
                    &mut client_dispatcher,
                )
                .unwrap();
            let window = state.window_for_root(root).unwrap();
            let role = state.window_role_object(window).unwrap();
            state
                .objects
                .try_ref_object_mut::<XdgToplevel>(role)
                .unwrap()
                .test_set_identity(title, app_id);
            (window, root)
        };
        capture::map_window(&mut state, &mut dispatcher, window);
        state.seat.keyboard_focus = Some(SurfaceFocus::new(root));
        assert_eq!(state.focused_window(), Some(window));
        Fixture { state, dispatcher, window, _client: client }
    }

    #[test]
    fn a_workspace_outside_a_group_has_no_outputs_and_index_zero() {
        let mut state = state_for_tests();
        state.create_workspace(None, Some("loose".to_string()));
        let snapshot = build(&state);
        assert_eq!(snapshot.workspaces.len(), 1);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(workspace.name, "loose");
        assert_eq!(workspace.index, 0);
        assert!(workspace.outputs.is_empty());
        assert!(workspace.coordinates.is_empty());
        assert!(!workspace.active);
    }

    fn pixmap(size: u32, scale: u32, byte: u8) -> crate::toplevel_icon::IconPixmap {
        let len = (size as usize)
            .saturating_mul(size as usize)
            .saturating_mul(4);
        crate::toplevel_icon::IconPixmap {
            size,
            scale,
            argb8888: vec![byte; len].into_boxed_slice(),
        }
    }

    fn install_icon(state: &mut State, window: WindowId, icon: crate::toplevel_icon::ToplevelIcon) {
        let role = state.window_role_object(window).unwrap();
        let toplevel = state
            .objects
            .try_ref_object_mut::<XdgToplevel>(role)
            .unwrap();
        toplevel.set_pending_icon(Some(std::rc::Rc::new(icon)));
        assert!(toplevel
            .take_pending_icon()
            .map(|icon| toplevel.set_icon(icon))
            .is_some());
    }

    #[test]
    fn window_icon_reports_an_unknown_window_and_selects_a_pixmap() {
        let mut fixture = mapped_window("nvim — ~/src", "Alacritty");
        let id = fixture.window.as_u64();
        assert_eq!(window_icon(&fixture.state, id + 1, 32, 1), Err("no such window".to_string()));

        let empty = window_icon(&fixture.state, id, 32, 1).unwrap();
        assert_eq!(empty.id, id);
        assert_eq!(empty.name, None);
        assert_eq!(empty.size, None);
        assert_eq!(empty.scale, None);
        assert_eq!(empty.data, None);
        assert_eq!(empty.format, "argb8888_premultiplied");

        install_icon(
            &mut fixture.state,
            fixture.window,
            crate::toplevel_icon::ToplevelIcon {
                name: Some("firefox".to_string()),
                pixmaps: vec![],
            },
        );
        let named = window_icon(&fixture.state, id, 32, 1).unwrap();
        assert_eq!(named.name.as_deref(), Some("firefox"));
        assert_eq!(named.size, None);
        assert_eq!(named.scale, None);
        assert_eq!(named.data, None);

        install_icon(
            &mut fixture.state,
            fixture.window,
            crate::toplevel_icon::ToplevelIcon {
                name: Some("firefox".to_string()),
                pixmaps: vec![pixmap(1, 1, 1), pixmap(4, 1, 4)],
            },
        );
        let exact = window_icon(&fixture.state, id, 1, 1).unwrap();
        assert_eq!(exact.size, Some(1));
        assert_eq!(exact.scale, Some(1));
        assert_eq!(proto::base64_decode(exact.data.as_deref().unwrap()), Ok(vec![1, 1, 1, 1]));
        let larger = window_icon(&fixture.state, id, 2, 1).unwrap();
        assert_eq!(larger.size, Some(4));
        assert_eq!(larger.scale, Some(1));
        assert_eq!(
            proto::base64_decode(larger.data.as_deref().unwrap())
                .unwrap()
                .len(),
            4 * 4 * 4
        );
        assert!(proto::base64_decode(larger.data.as_deref().unwrap())
            .unwrap()
            .iter()
            .all(|byte| *byte == 4));

        let summary = fixture
            .state
            .toplevel(fixture.window)
            .unwrap()
            .icon()
            .unwrap()
            .summary();
        capture::note_window_metadata(
            &mut fixture.state,
            &mut fixture.dispatcher,
            fixture.window,
            None,
            None,
            Some(summary),
        );
        let listed = build(&fixture.state).focused_window.unwrap().icon.unwrap();
        assert_eq!(listed.name.as_deref(), Some("firefox"));
        assert_eq!(
            listed.sizes,
            vec![proto::IconSize { size: 1, scale: 1 }, proto::IconSize { size: 4, scale: 1 }]
        );
    }
}
