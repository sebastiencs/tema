//! Compositor-owned Wayland source for one X selection.
//!
//! The object is not in any client's id map. Its protocol id is unused.
//! `receive` asks the window manager for the bytes.

use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    object::{Object, RequestResult},
    protocol_generated::WaylandId,
    state::{GrabbedObject, State},
    xwayland::SelectionKind,
};

/// MIME types of one X selection, keyed by [`crate::state::XSelectionSource`].
#[derive(Debug)]
pub struct XSelectionSourceObject {
    run: u64,
    kind: SelectionKind,
    mime_types: Vec<String>,
}

impl XSelectionSourceObject {
    pub(crate) fn new(run: u64, kind: SelectionKind, mime_types: Vec<String>) -> Self {
        Self { run, kind, mime_types }
    }

    pub(crate) fn mime_types(&self) -> &[String] {
        &self.mime_types
    }

    pub(crate) fn kind(&self) -> SelectionKind {
        self.kind
    }

    pub(crate) fn run(&self) -> u64 {
        self.run
    }
}

impl Object for XSelectionSourceObject {
    fn id(&self) -> WaylandId {
        WaylandId::from(0)
    }

    fn on(
        self: GrabbedObject<Self>,
        _client: &mut Client,
        _state: &mut State,
        _request: RawRequest,
        _dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs::File,
        os::fd::{FromRawFd, IntoRawFd, OwnedFd},
        rc::Rc,
    };

    use crate::{
        capture,
        clients::{Client, ClientAction, ClientId, DataSourceClientEvent, RawEvent, Resolved},
        input::{set_keyboard_focus_from_surface, SurfaceFocus},
        io_uring::model::IoUringAction,
        object::Object,
        protocol_generated::{
            WlDataDeviceId, WlDataSourceId, WlSeatId, WlSurfaceId, ZwlrDataControlDeviceV1Id,
        },
        runner::{Dispatcher, Model},
        seat::install_x_selection,
        state::{ObjectId, SelectionSourceState, XSelectionSource},
        test_support::state_for_tests,
        wayland_objects::{
            wl_data_device::WlDataDevice, wl_data_source::WlDataSource, wl_surface::WlSurface,
            zwlr_data_control_device_v1::ZwlrDataControlDeviceV1,
        },
        xwayland::{SelectionKind, Xwayland, XwaylandAction},
    };

    use super::XSelectionSourceObject;

    fn source(
        state: &mut crate::State,
        client: ClientId,
        kind: SelectionKind,
        mime_types: &[&str],
    ) -> XSelectionSource {
        let mime_types = mime_types.iter().map(|mime| (*mime).to_string()).collect();
        let (_, object) = state.insert_object(XSelectionSourceObject::new(1, kind, mime_types));
        XSelectionSource { client, object }
    }

    fn focus_with_data_device(client: &mut Client, state: &mut crate::State) {
        client.insert_object(state, WlSurface::new(WlSurfaceId::from(40), client.client_id(), 6));
        client.insert_object(
            state,
            WlDataDevice::new(WlDataDeviceId::from(50), WlSeatId::from(10), 3),
        );
        state.seat.register_data_device(Resolved::new(
            client.client_id(),
            WlDataDeviceId::from(50),
            client.object_id(WlDataDeviceId::from(50)),
        ));
        let mut setup = Dispatcher::default();
        let mut setup_dispatcher = client.test_dispatcher(&mut setup);
        set_keyboard_focus_from_surface(
            state,
            &mut setup_dispatcher,
            Some(SurfaceFocus::new(Resolved::new(
                client.client_id(),
                WlSurfaceId::from(40),
                client.object_id(WlSurfaceId::from(40)),
            ))),
        );
    }

    fn sent_contains(dispatcher: &Dispatcher, needle: &[u8]) -> bool {
        let mut found = false;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            if let IoUringAction::Sendmsg { buffer, .. } = action {
                if buffer.windows(needle.len()).any(|window| window == needle) {
                    found = true;
                }
            }
        });
        found
    }

    #[test]
    fn a_focused_data_device_is_offered_every_mime_type() {
        let mut state = state_for_tests();
        let mut focused = Client::new_for_tests(ClientId::from_raw(2));
        let xwayland = ClientId::from_raw(4);
        focus_with_data_device(&mut focused, &mut state);
        let installed =
            source(&mut state, xwayland, SelectionKind::Clipboard, &["text/plain", "image/png"]);
        let mut dispatcher = Dispatcher::default();
        {
            let mut client_dispatcher = focused.test_dispatcher(&mut dispatcher);
            install_x_selection(
                &mut state,
                &mut client_dispatcher,
                SelectionKind::Clipboard,
                Some(installed),
            );
        }
        focused
            .process(&mut state, ClientAction::RefreshDataDeviceSelection, &mut dispatcher)
            .unwrap();
        assert!(capture::test_sent(&dispatcher, 50, 0));
        assert!(capture::test_sent(&dispatcher, 50, 5));
        assert!(sent_contains(&dispatcher, b"text/plain"));
        assert!(sent_contains(&dispatcher, b"image/png"));
    }

    #[test]
    fn receive_dispatches_selection_send() {
        let mut state = state_for_tests();
        let mut focused = Client::new_for_tests(ClientId::from_raw(2));
        focus_with_data_device(&mut focused, &mut state);
        let installed =
            source(&mut state, ClientId::from_raw(4), SelectionKind::Clipboard, &["text/plain"]);
        let mut dispatcher = Dispatcher::default();
        {
            let mut client_dispatcher = focused.test_dispatcher(&mut dispatcher);
            install_x_selection(
                &mut state,
                &mut client_dispatcher,
                SelectionKind::Clipboard,
                Some(installed),
            );
        }
        focused
            .process(&mut state, ClientAction::RefreshDataDeviceSelection, &mut dispatcher)
            .unwrap();
        let offer = focused
            .resolve(WlDataDeviceId::from(50), &state)
            .unwrap()
            .rref(&state)
            .selection_offer()
            .expect("offer");
        let file = File::open("/dev/null").unwrap();
        let raw = file.into_raw_fd();
        let mut event = RawEvent::new(offer.wayland_id().into(), 1, 1);
        event.string("text/plain".to_string());
        let bytes = event.into_bytes();
        let mut request: crate::clients::RawRequest<'static> = (&*bytes).try_into().unwrap();
        let fds = VecDeque::from([unsafe { OwnedFd::from_raw_fd(raw) }]);
        request.fds = Some(Box::leak(Box::new(fds)));
        {
            let mut client_dispatcher = focused.test_dispatcher(&mut dispatcher);
            focused
                .grab_object(offer.wayland_id(), &mut state)
                .on(&mut focused, &mut state, request, &mut client_dispatcher)
                .unwrap();
        }
        let sent = dispatcher.test_take_queued_actions::<XwaylandAction>();
        let send = sent.iter().find_map(|action| match action {
            XwaylandAction::SelectionSend { source, mime_type, .. } => Some((*source, mime_type)),
            _ => None,
        });
        assert_eq!(send, Some((installed.object, &"text/plain".to_string())));
    }

    #[test]
    fn replacing_the_x_source_cancels_it_without_a_wayland_event() {
        let mut state = state_for_tests();
        let mut focused = Client::new_for_tests(ClientId::from_raw(2));
        focus_with_data_device(&mut focused, &mut state);
        let installed =
            source(&mut state, ClientId::from_raw(4), SelectionKind::Clipboard, &["text/plain"]);
        let mut dispatcher = Dispatcher::default();
        {
            let mut client_dispatcher = focused.test_dispatcher(&mut dispatcher);
            install_x_selection(
                &mut state,
                &mut client_dispatcher,
                SelectionKind::Clipboard,
                Some(installed),
            );
        }
        let source_id = WlDataSourceId::from(61);
        focused.insert_object(&mut state, WlDataSource::new(source_id, focused.client_id(), 3));
        let serial = state.seat.selection_serial().wrapping_add(1);
        state.seat.record_client_serial(focused.client_id(), serial);
        let mut event = RawEvent::new(WlDataDeviceId::from(50).into(), 1, 2);
        event.uint(source_id.into());
        event.uint(serial);
        let bytes = event.into_bytes();
        let request: crate::clients::RawRequest<'static> = (&*bytes).try_into().unwrap();
        {
            let mut client_dispatcher = focused.test_dispatcher(&mut dispatcher);
            focused
                .grab_object(WlDataDeviceId::from(50), &mut state)
                .on(&mut focused, &mut state, request, &mut client_dispatcher)
                .unwrap();
        }
        assert!(matches!(state.seat.selection_source(), Some(SelectionSourceState::WlData(_))));
        assert!(dispatcher
            .test_take_queued_actions::<XwaylandAction>()
            .into_iter()
            .any(|action| matches!(
                action,
                XwaylandAction::SelectionCancelled { source } if source == installed.object
            )));
        assert!(!dispatcher.test_has_queued_action_for(
            focused.client_id(),
            |action: &ClientAction| matches!(
                action,
                ClientAction::DataSourceEvent { event: DataSourceClientEvent::Cancelled, .. }
            )
        ));
    }

    #[test]
    fn a_data_control_device_sees_both_x_selections() {
        let mut state = state_for_tests();
        let mut clipboard = Client::new_for_tests(ClientId::from_raw(3));
        clipboard.insert_object(
            &mut state,
            ZwlrDataControlDeviceV1::new(
                ZwlrDataControlDeviceV1Id::from(30),
                WlSeatId::from(10),
                2,
            ),
        );
        state.seat.register_data_control_device(Resolved::new(
            clipboard.client_id(),
            ZwlrDataControlDeviceV1Id::from(30),
            clipboard.object_id(ZwlrDataControlDeviceV1Id::from(30)),
        ));
        let xwayland = ClientId::from_raw(4);
        let clip = source(&mut state, xwayland, SelectionKind::Clipboard, &["text/plain"]);
        let primary = source(&mut state, xwayland, SelectionKind::Primary, &["image/png"]);
        let mut dispatcher = Dispatcher::default();
        {
            let mut client_dispatcher = clipboard.test_dispatcher(&mut dispatcher);
            install_x_selection(
                &mut state,
                &mut client_dispatcher,
                SelectionKind::Clipboard,
                Some(clip),
            );
            install_x_selection(
                &mut state,
                &mut client_dispatcher,
                SelectionKind::Primary,
                Some(primary),
            );
        }
        clipboard
            .process(&mut state, ClientAction::RefreshDataControlSelection, &mut dispatcher)
            .unwrap();
        clipboard
            .process(&mut state, ClientAction::RefreshDataControlPrimarySelection, &mut dispatcher)
            .unwrap();
        assert!(capture::test_sent(&dispatcher, 30, 1));
        assert!(!capture::test_sent_arg(&dispatcher, 30, 1, 0));
        assert!(capture::test_sent(&dispatcher, 30, 3));
        assert!(!capture::test_sent_arg(&dispatcher, 30, 3, 0));
        assert!(sent_contains(&dispatcher, b"text/plain"));
        assert!(sent_contains(&dispatcher, b"image/png"));
    }

    #[test]
    fn removing_the_xwayland_client_clears_its_selections() {
        let mut state = state_for_tests();
        let xwayland = ClientId::from_raw(4);
        let mut dispatcher = Dispatcher::default();
        let owner = Client::new_xwayland_for_tests(xwayland);
        let clipboard = source(&mut state, xwayland, SelectionKind::Clipboard, &["text/plain"]);
        let primary = source(&mut state, xwayland, SelectionKind::Primary, &["text/plain"]);
        {
            let mut client_dispatcher = owner.test_dispatcher(&mut dispatcher);
            install_x_selection(
                &mut state,
                &mut client_dispatcher,
                SelectionKind::Clipboard,
                Some(clipboard),
            );
            install_x_selection(
                &mut state,
                &mut client_dispatcher,
                SelectionKind::Primary,
                Some(primary),
            );
        }
        let removal = state.seat.remove_client(xwayland);
        assert!(removal.selection_cleared);
        assert!(removal.primary_selection_cleared);
        assert!(state.seat.selection_source().is_none());
        assert!(state.seat.primary_selection_source().is_none());

        let mut app = Client::new_for_tests(ClientId::from_raw(8));
        let source_id = WlDataSourceId::from(70);
        app.insert_object(&mut state, WlDataSource::new(source_id, app.client_id(), 3));
        let resolved = app.resolve(source_id, &state).unwrap();
        let serial = state.seat.next_serial();
        state
            .seat
            .replace_selection_source(Some(SelectionSourceState::WlData(resolved)), serial);
        let removal = state.seat.remove_client(app.client_id());
        assert!(!removal.selection_cleared);
        assert!(state.seat.selection_source().is_none());
    }

    #[test]
    fn closing_the_xwayland_client_publishes_the_cleared_selection() {
        let mut state = state_for_tests();
        let mut focused = Client::new_for_tests(ClientId::from_raw(2));
        focus_with_data_device(&mut focused, &mut state);
        let xwayland_id = ClientId::from_raw(4);
        let mut xwayland = Client::new_xwayland_for_tests(xwayland_id);
        let installed = source(&mut state, xwayland_id, SelectionKind::Clipboard, &["text/plain"]);
        let mut dispatcher = Dispatcher::default();
        {
            let mut client_dispatcher = xwayland.test_dispatcher(&mut dispatcher);
            install_x_selection(
                &mut state,
                &mut client_dispatcher,
                SelectionKind::Clipboard,
                Some(installed),
            );
        }
        xwayland
            .process(&mut state, ClientAction::Close, &mut dispatcher)
            .unwrap();
        assert!(state.seat.selection_source().is_none());
        assert!(dispatcher.test_has_queued_action_for(
            focused.client_id(),
            |action: &ClientAction| matches!(action, ClientAction::RefreshDataDeviceSelection)
        ));
    }

    #[test]
    fn a_stopped_run_drops_the_receive_fd() {
        let mut xwayland = Xwayland::new();
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let fd = Rc::new(OwnedFd::from(File::open("/dev/null").unwrap()));
        let watched = Rc::clone(&fd);
        xwayland
            .process(
                &mut state,
                XwaylandAction::SelectionSend {
                    source: ObjectId::from(1usize),
                    mime_type: "text/plain".to_string(),
                    fd,
                },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(Rc::strong_count(&watched), 1);
    }
}
