use std::{os::fd::OwnedFd, rc::Rc};

use crate::{
    clients::{Client, ClientDispatcher, ClientManagerAction, RawRequest},
    object::{Object, Parser, RequestResult},
    protocol_generated::{wp_security_context_v1, WaylandId, WpSecurityContextV1Id},
    security_context::SecurityContextMetadata,
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

#[derive(Debug)]
pub struct WpSecurityContextV1 {
    id: WpSecurityContextV1Id,
    /// `None` after `commit`: the fds went to the `ClientManager`.
    fds: Option<(Rc<OwnedFd>, Rc<OwnedFd>)>, // (listen_fd, close_fd)
    metadata: SecurityContextMetadata,
}

impl WpSecurityContextV1 {
    pub fn new(id: WpSecurityContextV1Id, listen_fd: Rc<OwnedFd>, close_fd: Rc<OwnedFd>) -> Self {
        Self {
            id,
            fds: Some((listen_fd, close_fd)),
            metadata: SecurityContextMetadata::default(),
        }
    }
}

impl Object for WpSecurityContextV1 {
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
        use wp_security_context_v1::{Error, Request::*};

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            request => {
                if self.fds.is_none() {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        Error::AlreadyUsed as u32,
                        "security context already committed",
                    );
                    return Ok(());
                }
                match request {
                    SetSandboxEngine { name } => {
                        if self.metadata.sandbox_engine.is_some() {
                            client.protocol_violation_disconnect(
                                dispatcher,
                                self.id.into(),
                                Error::AlreadySet as u32,
                                "sandbox engine already set",
                            );
                            return Ok(());
                        }
                        self.metadata.sandbox_engine = Some(name);
                    }
                    SetAppId { app_id } => {
                        if self.metadata.app_id.is_some() {
                            client.protocol_violation_disconnect(
                                dispatcher,
                                self.id.into(),
                                Error::AlreadySet as u32,
                                "app id already set",
                            );
                            return Ok(());
                        }
                        self.metadata.app_id = Some(app_id);
                    }
                    SetInstanceId { instance_id } => {
                        if self.metadata.instance_id.is_some() {
                            client.protocol_violation_disconnect(
                                dispatcher,
                                self.id.into(),
                                Error::AlreadySet as u32,
                                "instance id already set",
                            );
                            return Ok(());
                        }
                        self.metadata.instance_id = Some(instance_id);
                    }
                    Commit => {
                        let (listen_fd, close_fd) = self
                            .fds
                            .take()
                            .expect("commit checked the context is not used yet");
                        dispatcher.dispatch(ClientManagerAction::AddSecurityListener {
                            listen_fd,
                            close_fd,
                            metadata: Rc::new(std::mem::take(&mut self.metadata)),
                        });
                    }
                    Destroy => unreachable!("destroy is handled above"),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        os::{
            fd::{AsRawFd, OwnedFd},
            unix::net::UnixStream,
        },
        rc::Rc,
    };

    use crate::{
        clients::{Arguments, Client, ClientManagerAction, RawRequest},
        io_uring::model::IoUringAction,
        object::{Object, RequestResult},
        protocol_generated::{WaylandId, WpSecurityContextV1Id},
        runner::Dispatcher,
        security_context::SecurityContextMetadata,
        state::State,
        test_support::{state_for_tests, test_client_and_dispatcher},
    };

    use super::WpSecurityContextV1;

    struct Fixture {
        state: State,
        client: Client,
        dispatcher: Dispatcher,
    }

    fn fixture_with_fds(listen_fd: OwnedFd, close_fd: OwnedFd) -> Fixture {
        let mut state = state_for_tests();
        let (mut client, dispatcher) = test_client_and_dispatcher(1);
        client.insert_object(
            &mut state,
            WpSecurityContextV1::new(
                WpSecurityContextV1Id::from(7),
                Rc::new(listen_fd),
                Rc::new(close_fd),
            ),
        );
        Fixture { state, client, dispatcher }
    }

    fn fixture() -> Fixture {
        fixture_with_fds(
            OwnedFd::from(UnixStream::pair().unwrap().0),
            OwnedFd::from(UnixStream::pair().unwrap().0),
        )
    }

    /// Wire encoding of a string argument: the length including the NUL, then the
    /// bytes and the NUL padded to a multiple of four, as native-endian words.
    fn string_words(value: &str) -> Vec<u32> {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        let mut words = vec![(value.len() + 1) as u32];
        words.extend(
            bytes
                .chunks(4)
                .map(|chunk| u32::from_ne_bytes(chunk.try_into().unwrap())),
        );
        words
    }

    /// Sends request `opcode` with `words` to the context at wire id 7.
    fn dispatch(fixture: &mut Fixture, opcode: u16, words: Vec<u32>) -> RequestResult {
        let context = WpSecurityContextV1Id::from(7);
        let wire: WaylandId = context.into();
        let mut client_dispatcher = fixture.client.test_dispatcher(&mut fixture.dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(wire);
        let request = RawRequest {
            object_id: 7,
            request_id: opcode,
            arguments: Arguments::from_words(VecDeque::from(words)),
            fds: None,
        };
        let mut object = fixture.client.grab_object(context, &mut fixture.state);
        object.on(&mut fixture.client, &mut fixture.state, request, &mut client_dispatcher)
    }

    fn set_metadata(fixture: &mut Fixture) {
        dispatch(fixture, 1, string_words("org.flatpak")).unwrap();
        dispatch(fixture, 2, string_words("org.example.App")).unwrap();
        dispatch(fixture, 3, string_words("42")).unwrap();
    }

    fn display_error(dispatcher: &Dispatcher) -> Option<(u32, u32)> {
        let mut found = None;
        dispatcher.test_for_each_queued_action::<IoUringAction>(|action| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 16 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            if object != 1 {
                return;
            }
            let target = u32::from_ne_bytes(buffer[8..12].try_into().unwrap());
            let code = u32::from_ne_bytes(buffer[12..16].try_into().unwrap());
            found = Some((target, code));
        });
        found
    }

    /// The single `AddSecurityListener` queued by the commit.
    fn take_listener(dispatcher: &mut Dispatcher) -> Vec<ClientManagerAction> {
        dispatcher.test_take_queued_actions::<ClientManagerAction>()
    }

    fn hung_up(fd: std::os::fd::RawFd) -> bool {
        let mut pollfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut pollfd, 1, 0) };
        ready == 1 && pollfd.revents & libc::POLLHUP != 0
    }

    #[test]
    fn metadata_then_commit_hands_the_listener_to_the_client_manager() {
        let mut fixture = fixture();
        set_metadata(&mut fixture);
        dispatch(&mut fixture, 4, Vec::new()).unwrap();

        let actions = take_listener(&mut fixture.dispatcher);
        let [ClientManagerAction::AddSecurityListener { metadata, .. }] = actions.as_slice() else {
            panic!("expected one AddSecurityListener, got {actions:?}");
        };
        assert_eq!(
            **metadata,
            SecurityContextMetadata {
                sandbox_engine: Some("org.flatpak".into()),
                app_id: Some("org.example.App".into()),
                instance_id: Some("42".into()),
            }
        );
        assert_eq!(display_error(&fixture.dispatcher), None);
    }

    #[test]
    fn commit_without_metadata_is_allowed() {
        let mut fixture = fixture();
        dispatch(&mut fixture, 4, Vec::new()).unwrap();

        let actions = take_listener(&mut fixture.dispatcher);
        let [ClientManagerAction::AddSecurityListener { metadata, .. }] = actions.as_slice() else {
            panic!("expected one AddSecurityListener, got {actions:?}");
        };
        assert_eq!(**metadata, SecurityContextMetadata::default());
        assert_eq!(display_error(&fixture.dispatcher), None);
    }

    #[test]
    fn setting_a_field_twice_is_already_set() {
        let mut fixture = fixture();
        dispatch(&mut fixture, 2, string_words("org.example.App")).unwrap();
        dispatch(&mut fixture, 2, string_words("org.example.Other")).unwrap();
        assert_eq!(display_error(&fixture.dispatcher), Some((7, 2)));
    }

    #[test]
    fn any_request_after_commit_is_already_used() {
        let mut fixture = fixture();
        dispatch(&mut fixture, 4, Vec::new()).unwrap();
        dispatch(&mut fixture, 2, string_words("org.example.App")).unwrap();
        assert_eq!(display_error(&fixture.dispatcher), Some((7, 1)));
    }

    #[test]
    fn committing_twice_is_already_used() {
        let mut fixture = fixture();
        dispatch(&mut fixture, 4, Vec::new()).unwrap();
        dispatch(&mut fixture, 4, Vec::new()).unwrap();
        assert_eq!(display_error(&fixture.dispatcher), Some((7, 1)));
    }

    #[test]
    fn destroy_after_commit_keeps_the_listener() {
        let mut fixture = fixture();
        dispatch(&mut fixture, 4, Vec::new()).unwrap();

        let actions = take_listener(&mut fixture.dispatcher);
        let [ClientManagerAction::AddSecurityListener { listen_fd, .. }] = actions.as_slice()
        else {
            panic!("expected one AddSecurityListener, got {actions:?}");
        };
        let listen_raw = listen_fd.as_raw_fd();

        dispatch(&mut fixture, 0, Vec::new()).unwrap();
        assert!(!fixture
            .client
            .contains_object(WpSecurityContextV1Id::from(7), &fixture.state));
        assert_eq!(display_error(&fixture.dispatcher), None);
        assert_ne!(unsafe { libc::fcntl(listen_raw, libc::F_GETFD) }, -1);
    }

    #[test]
    fn destroy_before_commit_closes_the_fds() {
        let (close, peer) = UnixStream::pair().unwrap();
        let mut fixture =
            fixture_with_fds(OwnedFd::from(UnixStream::pair().unwrap().0), OwnedFd::from(close));
        dispatch(&mut fixture, 0, Vec::new()).unwrap();
        assert!(hung_up(peer.as_raw_fd()));
    }
}
