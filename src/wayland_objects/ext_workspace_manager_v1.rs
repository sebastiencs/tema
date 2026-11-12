use std::collections::BTreeMap;

use crate::{
    clients::{Client, ClientDispatcher, ClientManagerAction, EventWriter, RawRequest, Resolved},
    drm::{kick_visual_animation_pump, request_outputs},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        ext_workspace_manager_v1, Constants, ExtWorkspaceGroupHandleV1Id, ExtWorkspaceHandleV1Id,
        ExtWorkspaceManagerV1Id, NewId, WaylandId,
    },
    runner::Dispatcher,
    state::{GrabbedObject, OutputId, State, WorkspaceGroupId, WorkspaceId},
    wayland_objects::{
        ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
        ext_workspace_handle_v1::ExtWorkspaceHandleV1, wl_display::WlDisplay,
    },
};

#[derive(Debug)]
pub struct ExtWorkspaceManagerV1Global;

impl ExtWorkspaceManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self {})
    }
}

pub fn publish_workspace_state_changed(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    affected_outputs: impl IntoIterator<Item = OutputId>,
) {
    let opened = state.relayout_dirty_workspaces();
    crate::idle_inhibit::refresh(state, dispatcher, std::time::Instant::now());
    if !opened.is_empty() {
        kick_visual_animation_pump(dispatcher);
    }
    let mut affected_outputs = affected_outputs.into_iter().collect::<Vec<_>>();
    affected_outputs.sort_unstable();
    affected_outputs.dedup();

    if !affected_outputs.is_empty() {
        request_outputs(state, affected_outputs.iter().copied(), dispatcher);
    }
    crate::input::focus::refresh_pointer_focus_only(state, dispatcher);
    crate::input::focus::keep_or_restore_keyboard_focus(state, dispatcher);
    dispatcher.dispatch(ClientManagerAction::WorkspaceStateChanged);
}

impl Global for ExtWorkspaceManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (ExtWorkspaceManagerV1::INTERFACE_NAME, ExtWorkspaceManagerV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        version: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        let id = ExtWorkspaceManagerV1Id::from(id);
        let (manager, _) = client.insert_object(state, ExtWorkspaceManagerV1::new(id, version));
        let mut manager = manager.grab(state);
        manager.sync(client, state, dispatcher, true);
    }
}

#[derive(Clone, Debug)]
pub enum PendingWorkspaceRequest {
    CreateWorkspace { group_id: WorkspaceGroupId, name: String },
    Activate { workspace_id: WorkspaceId },
    Assign { workspace_id: WorkspaceId, group_id: WorkspaceGroupId },
    Remove { workspace_id: WorkspaceId },
}

#[derive(Debug)]
pub struct ExtWorkspaceManagerV1 {
    id: ExtWorkspaceManagerV1Id,
    version: u32,
    group_handles: BTreeMap<WorkspaceGroupId, Resolved<ExtWorkspaceGroupHandleV1Id>>,
    workspace_handles: BTreeMap<WorkspaceId, Resolved<ExtWorkspaceHandleV1Id>>,
    pending_requests: Vec<PendingWorkspaceRequest>,
}

impl ExtWorkspaceManagerV1 {
    pub fn new(id: ExtWorkspaceManagerV1Id, version: u32) -> Self {
        Self {
            id,
            version,
            group_handles: BTreeMap::new(),
            workspace_handles: BTreeMap::new(),
            pending_requests: Vec::new(),
        }
    }

    pub fn queue_request(&mut self, request: PendingWorkspaceRequest) {
        self.pending_requests.push(request);
    }

    pub fn group_handle_id(
        &self,
        group_id: WorkspaceGroupId,
    ) -> Option<Resolved<ExtWorkspaceGroupHandleV1Id>> {
        self.group_handles.get(&group_id).copied()
    }

    pub fn workspace_handle_id(
        &self,
        workspace_id: WorkspaceId,
    ) -> Option<Resolved<ExtWorkspaceHandleV1Id>> {
        self.workspace_handles.get(&workspace_id).copied()
    }

    /// Clears the manager's mapping so the next `sync` can emit a new `workspace_group` and
    /// `new_id` for this logical group (client destroyed their `ext_workspace_group_handle_v1`).
    pub(crate) fn release_group_handle(&mut self, group_id: WorkspaceGroupId) {
        self.group_handles.remove(&group_id);
    }

    /// Clears the manager's mapping so the next `sync` can emit a new `workspace` and `new_id`
    /// for this logical workspace (client destroyed their `ext_workspace_handle_v1`).
    pub(crate) fn release_workspace_handle(&mut self, workspace_id: WorkspaceId) {
        self.workspace_handles.remove(&workspace_id);
    }

    fn create_missing_group_resources(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        for group_id in state.workspace_group_ids() {
            if self.group_handles.contains_key(&group_id) {
                continue;
            }
            let resource_id = client.allocate_server_object_id::<ExtWorkspaceGroupHandleV1Id>();
            client.insert_object(
                state,
                ExtWorkspaceGroupHandleV1::new(
                    resource_id,
                    Resolved::new(client.client_id(), self.id, client.object_id(self.id)),
                    group_id,
                    self.version,
                ),
            );
            self.group_handles.insert(
                group_id,
                Resolved::new(client.client_id(), resource_id, client.object_id(resource_id)),
            );
            self.send_event(
                ext_workspace_manager_v1::Event::WorkspaceGroup { workspace_group: resource_id },
                dispatcher,
            );
        }
    }

    fn create_missing_workspace_resources(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
    ) {
        let mut new_workspace_object_ids = Vec::new();
        for workspace_id in state.workspace_ids() {
            if self.workspace_handles.contains_key(&workspace_id) {
                continue;
            }
            let resource_id = client.allocate_server_object_id::<ExtWorkspaceHandleV1Id>();
            let this = client.resolve(self.id, state).unwrap();
            let (workspace_handle, _) = client.insert_object(
                state,
                ExtWorkspaceHandleV1::new(resource_id, this, workspace_id, self.version),
            );
            self.workspace_handles
                .insert(workspace_id, workspace_handle);
            self.send_event(
                ext_workspace_manager_v1::Event::Workspace { workspace: resource_id },
                dispatcher,
            );
            new_workspace_object_ids.push(workspace_handle);
        }

        for object_id in new_workspace_object_ids {
            let mut workspace = object_id.grab(state);
            workspace.sync(state, dispatcher);
        }
    }

    pub fn sync(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        send_done: bool,
    ) {
        self.create_missing_group_resources(client, state, dispatcher);
        self.create_missing_workspace_resources(client, state, dispatcher);

        let workspace_handles = self.workspace_handles.clone();
        let group_object_ids = self.group_handles.values().copied().collect::<Vec<_>>();
        for resource_id in group_object_ids {
            let Some(mut group) = resource_id.try_grab(state) else {
                continue;
            };
            group.sync(client, state, dispatcher, &workspace_handles);
        }

        let workspace_object_ids = self.workspace_handles.values().copied().collect::<Vec<_>>();
        for resource_id in workspace_object_ids {
            let Some(mut workspace) = resource_id.try_grab(state) else {
                continue;
            };
            workspace.sync(state, dispatcher);
        }

        if send_done {
            self.send_event(ext_workspace_manager_v1::Event::Done, dispatcher);
        }
    }

    fn apply_pending_requests(&mut self, state: &mut State) -> (Vec<crate::state::OutputId>, bool) {
        let requests = std::mem::take(&mut self.pending_requests);
        let had_requests = !requests.is_empty();
        let mut affected_outputs = Vec::new();
        for request in requests {
            match request {
                PendingWorkspaceRequest::CreateWorkspace { group_id, name } => {
                    if state.workspace_groups.try_get(group_id).is_none() {
                        continue;
                    }
                    let workspace_id = state.create_workspace(Some(group_id), Some(name));
                    let _ = state.activate_workspace(workspace_id);
                    affected_outputs.extend(state.workspace_output_ids(workspace_id));
                }
                PendingWorkspaceRequest::Activate { workspace_id } => {
                    if state.workspaces.try_get(workspace_id).is_none() {
                        continue;
                    }
                    affected_outputs.extend(state.workspace_output_ids(workspace_id));
                    let _ = state.activate_workspace(workspace_id);
                }
                PendingWorkspaceRequest::Assign { workspace_id, group_id } => {
                    if state.workspaces.try_get(workspace_id).is_none()
                        || state.workspace_groups.try_get(group_id).is_none()
                    {
                        continue;
                    }
                    affected_outputs.extend(state.workspace_output_ids(workspace_id));
                    state.move_workspace_to_group_output(workspace_id, group_id);
                    affected_outputs.extend(state.workspace_output_ids(workspace_id));
                }
                PendingWorkspaceRequest::Remove { workspace_id } => {
                    if state.workspaces.try_get(workspace_id).is_none() {
                        continue;
                    }
                    affected_outputs.extend(state.workspace_output_ids(workspace_id));
                    let _ = state.remove_workspace(workspace_id);
                }
            }
        }
        affected_outputs.sort_unstable();
        affected_outputs.dedup();
        (affected_outputs, had_requests)
    }

    /// `finished` destroys this manager. The group and workspace handles are
    /// server-allocated, so the client never receives `delete_id` for them and
    /// still owns the proxies. They stay in the client map until it sends
    /// `destroy`. Clearing the maps keeps a later sync from touching them
    /// through this manager.
    fn stop(&mut self, client: &mut Client, state: &mut State, dispatcher: &mut ClientDispatcher) {
        self.send_event(ext_workspace_manager_v1::Event::Finished, dispatcher);
        self.group_handles.clear();
        self.workspace_handles.clear();
        client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
    }
}

impl Object for ExtWorkspaceManagerV1 {
    fn id(&self) -> WaylandId {
        self.id.into()
    }

    fn on(
        mut self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        use ext_workspace_manager_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Commit => {
                let (affected_outputs, had_requests) = self.apply_pending_requests(state);
                if !had_requests {
                    self.send_event(ext_workspace_manager_v1::Event::Done, dispatcher);
                    return Ok(());
                }
                publish_workspace_state_changed(state, dispatcher, affected_outputs);
            }
            Stop => self.stop(client, state, dispatcher),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use {
        super::ExtWorkspaceManagerV1,
        crate::{
            clients::{Arguments, Client, ClientId, ClientManagerAction, RawEvent, Resolved},
            io_uring::model::IoUringAction,
            object::Object,
            protocol_generated::{
                ExtWorkspaceGroupHandleV1Id, ExtWorkspaceHandleV1Id, ExtWorkspaceManagerV1Id,
                WaylandId,
            },
            runner::Dispatcher,
            state::{WorkspaceGroupId, WorkspaceId},
            test_support::state_for_tests,
            wayland_objects::{
                ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
                ext_workspace_handle_v1::ExtWorkspaceHandleV1,
            },
        },
    };

    struct Handles {
        state: crate::state::State,
        client: Client,
        manager_id: ExtWorkspaceManagerV1Id,
        group_id: ExtWorkspaceGroupHandleV1Id,
        workspace_id: ExtWorkspaceHandleV1Id,
        manager: Resolved<ExtWorkspaceManagerV1Id>,
        group: Resolved<ExtWorkspaceGroupHandleV1Id>,
        workspace: Resolved<ExtWorkspaceHandleV1Id>,
    }

    fn handles() -> Handles {
        let mut state = state_for_tests();
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let manager_id = ExtWorkspaceManagerV1Id::from(4);
        let group_id = ExtWorkspaceGroupHandleV1Id::from(0xff00_0000);
        let workspace_id = ExtWorkspaceHandleV1Id::from(0xff00_0001);
        let (manager, _) =
            client.insert_object(&mut state, ExtWorkspaceManagerV1::new(manager_id, 1));
        let (group, _) = client.insert_object(
            &mut state,
            ExtWorkspaceGroupHandleV1::new(
                group_id,
                manager,
                WorkspaceGroupId::from_parts(1, 1),
                1,
            ),
        );
        let (workspace, _) = client.insert_object(
            &mut state,
            ExtWorkspaceHandleV1::new(workspace_id, manager, WorkspaceId::from_parts(1, 1), 1),
        );
        {
            let mut manager_obj = manager.ref_mut(&mut state);
            manager_obj
                .group_handles
                .insert(WorkspaceGroupId::from_parts(1, 1), group);
            manager_obj
                .workspace_handles
                .insert(WorkspaceId::from_parts(1, 1), workspace);
        }
        Handles {
            state,
            client,
            manager_id,
            group_id,
            workspace_id,
            manager,
            group,
            workspace,
        }
    }

    fn empty_request(id: impl Into<WaylandId>, opcode: u16) -> crate::clients::RawRequest<'static> {
        let event = RawEvent::new(id.into(), opcode, 0);
        (&*event.into_bytes()).try_into().unwrap()
    }

    fn create_workspace_request(
        id: ExtWorkspaceGroupHandleV1Id,
    ) -> crate::clients::RawRequest<'static> {
        let mut event = RawEvent::new(id.into(), 0, 1);
        event.string("spare".to_string());
        (&*event.into_bytes()).try_into().unwrap()
    }

    fn stopped(dispatcher: &Dispatcher, manager_id: ExtWorkspaceManagerV1Id) -> bool {
        let mut found = false;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            if object == u32::from(manager_id) && opcode == 3 {
                found = true;
            }
        });
        found
    }

    fn closed(dispatcher: &Dispatcher, client_id: ClientId) -> bool {
        dispatcher.test_has_queued_action::<ClientManagerAction>(
            |action| matches!(action, ClientManagerAction::Close { id } if *id == client_id),
        )
    }

    fn stop(handles: &mut Handles, dispatcher: &mut Dispatcher) {
        let mut client_dispatcher = handles.client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(handles.manager_id.into());
        let manager = handles
            .state
            .objects
            .grab_object::<ExtWorkspaceManagerV1>(handles.manager.object_id());
        manager
            .on(
                &mut handles.client,
                &mut handles.state,
                empty_request(handles.manager_id, 1),
                &mut client_dispatcher,
            )
            .unwrap();
    }

    #[test]
    fn stop_keeps_handles_until_client_destroys_them() {
        let mut handles = handles();
        let mut dispatcher = Dispatcher::default();
        stop(&mut handles, &mut dispatcher);

        assert!(stopped(&dispatcher, handles.manager_id));
        assert!(!handles
            .client
            .contains_object(handles.manager_id, &handles.state));
        assert!(handles
            .client
            .contains_object(handles.group_id, &handles.state));
        assert!(handles
            .client
            .contains_object(handles.workspace_id, &handles.state));
    }

    #[test]
    fn handle_requests_after_stop_are_ignored() {
        let mut handles = handles();
        let client_id = handles.client.client_id();
        let mut dispatcher = Dispatcher::default();
        stop(&mut handles, &mut dispatcher);
        {
            let mut client_dispatcher = handles.client.test_dispatcher(&mut dispatcher);
            let workspace = handles
                .state
                .objects
                .grab_object::<ExtWorkspaceHandleV1>(handles.workspace.object_id());
            workspace
                .on(
                    &mut handles.client,
                    &mut handles.state,
                    empty_request(handles.workspace_id, 1),
                    &mut client_dispatcher,
                )
                .unwrap();
            let group = handles
                .state
                .objects
                .grab_object::<ExtWorkspaceGroupHandleV1>(handles.group.object_id());
            group
                .on(
                    &mut handles.client,
                    &mut handles.state,
                    create_workspace_request(handles.group_id),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        assert!(!closed(&dispatcher, client_id));
    }

    #[test]
    fn handle_destroy_after_stop_removes_it() {
        let mut handles = handles();
        let mut dispatcher = Dispatcher::default();
        stop(&mut handles, &mut dispatcher);
        {
            let mut client_dispatcher = handles.client.test_dispatcher(&mut dispatcher);
            client_dispatcher.set_requester_object_id_for_tests(handles.group_id.into());
            let group = handles
                .state
                .objects
                .grab_object::<ExtWorkspaceGroupHandleV1>(handles.group.object_id());
            group
                .on(
                    &mut handles.client,
                    &mut handles.state,
                    empty_request(handles.group_id, 1),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        {
            let mut client_dispatcher = handles.client.test_dispatcher(&mut dispatcher);
            client_dispatcher.set_requester_object_id_for_tests(handles.workspace_id.into());
            let workspace = handles
                .state
                .objects
                .grab_object::<ExtWorkspaceHandleV1>(handles.workspace.object_id());
            workspace
                .on(
                    &mut handles.client,
                    &mut handles.state,
                    empty_request(handles.workspace_id, 0),
                    &mut client_dispatcher,
                )
                .unwrap();
        }
        assert!(!handles
            .client
            .contains_object(handles.group_id, &handles.state));
        assert!(!handles
            .client
            .contains_object(handles.workspace_id, &handles.state));
    }

    struct Wire {
        object: u32,
        opcode: u16,
        args: Vec<u32>,
    }

    fn sync_manager(handles: &mut Handles, dispatcher: &mut Dispatcher) {
        let mut client_dispatcher = handles.client.test_dispatcher(dispatcher);
        let mut manager = handles
            .state
            .objects
            .grab_object::<ExtWorkspaceManagerV1>(handles.manager.object_id());
        manager.sync(&mut handles.client, &mut handles.state, &mut client_dispatcher, true);
    }

    fn queued_events(dispatcher: &Dispatcher) -> Vec<Wire> {
        let mut events = Vec::new();
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            let mut offset = 0;
            while offset + 8 <= buffer.len() {
                let object = u32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap());
                let header = u32::from_ne_bytes(buffer[offset + 4..offset + 8].try_into().unwrap());
                let size = (header >> 16) as usize;
                let opcode = header as u16;
                if size < 8 || offset + size > buffer.len() {
                    break;
                }
                let mut args = Vec::new();
                let mut word = offset + 8;
                while word + 4 <= offset + size {
                    args.push(u32::from_ne_bytes(buffer[word..word + 4].try_into().unwrap()));
                    word += 4;
                }
                events.push(Wire { object, opcode, args });
                offset += size;
            }
        });
        events
    }

    fn wayland_string(args: &[u32]) -> String {
        Arguments::from_words(VecDeque::from(args.to_vec()))
            .string()
            .unwrap()
    }

    /// Object ids announced by `ext_workspace_manager_v1.workspace` (opcode 1).
    fn announced_workspaces(events: &[Wire], manager: u32) -> Vec<u32> {
        events
            .iter()
            .filter(|event| event.object == manager && event.opcode == 1)
            .map(|event| event.args[0])
            .collect()
    }

    fn id_strings(events: &[Wire], manager: u32) -> Vec<String> {
        let workspaces = announced_workspaces(events, manager);
        events
            .iter()
            .filter(|event| event.opcode == 0 && workspaces.contains(&event.object))
            .map(|event| wayland_string(&event.args))
            .collect()
    }

    fn id_precedes_name(events: &[Wire], workspace: u32) -> bool {
        let opcodes = events
            .iter()
            .filter(|event| event.object == workspace)
            .map(|event| event.opcode)
            .collect::<Vec<_>>();
        let id = opcodes.iter().position(|opcode| *opcode == 0);
        let name = opcodes.iter().position(|opcode| *opcode == 1);
        matches!((id, name), (Some(id), Some(name)) if id < name)
    }

    #[test]
    fn workspace_id_is_sent_once_per_handle() {
        let mut handles = handles();
        let workspace_id = handles.state.create_workspace(None, None);
        let stable_id = handles.state.workspaces[workspace_id].stable_id.clone();
        let manager = u32::from(handles.manager_id);
        let mut dispatcher = Dispatcher::default();

        sync_manager(&mut handles, &mut dispatcher);
        let events = queued_events(&dispatcher);
        let workspaces = announced_workspaces(&events, manager);
        assert_eq!(workspaces.len(), 1);
        assert_eq!(id_strings(&events, manager), vec![stable_id.clone()]);
        assert!(id_precedes_name(&events, workspaces[0]));

        sync_manager(&mut handles, &mut dispatcher);
        let events = queued_events(&dispatcher);
        assert_eq!(id_strings(&events, manager), vec![stable_id.clone()]);

        handles
            .manager
            .ref_mut(&mut handles.state)
            .release_workspace_handle(workspace_id);
        sync_manager(&mut handles, &mut dispatcher);
        let events = queued_events(&dispatcher);
        assert_eq!(id_strings(&events, manager), vec![stable_id.clone(), stable_id]);
        let workspaces = announced_workspaces(&events, manager);
        assert_eq!(workspaces.len(), 2);
        assert!(id_precedes_name(&events, workspaces[1]));
    }
}
