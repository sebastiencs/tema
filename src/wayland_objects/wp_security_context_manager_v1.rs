use std::os::fd::AsRawFd;

use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        wp_security_context_manager_v1, Constants, NewId, WaylandId, WpSecurityContextManagerV1Id,
        WpSecurityContextV1Id,
    },
    security_context,
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, wp_security_context_v1::WpSecurityContextV1},
};

/// Advertised at version 1. Sandboxed clients never see it (not on the allow list).
#[derive(Debug)]
pub struct WpSecurityContextManagerV1Global;

impl WpSecurityContextManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for WpSecurityContextManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (WpSecurityContextManagerV1::INTERFACE_NAME, WpSecurityContextManagerV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(
            state,
            WpSecurityContextManagerV1 { id: WpSecurityContextManagerV1Id::from(id) },
        );
    }
}

#[derive(Debug)]
pub struct WpSecurityContextManagerV1 {
    id: WpSecurityContextManagerV1Id,
}

impl Object for WpSecurityContextManagerV1 {
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
        use wp_security_context_manager_v1::{Error, Request::*};

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            CreateListener { id, listen_fd, close_fd } => {
                if client.security_context().is_some() {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        Error::Nested as u32,
                        "nested security contexts are forbidden",
                    );
                    return Ok(());
                }
                if let Err(message) = security_context::validate_listen_fd(listen_fd.as_raw_fd()) {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        Error::InvalidListenFd as u32,
                        message,
                    );
                    return Ok(());
                }
                client.insert_object(
                    state,
                    WpSecurityContextV1::new(WpSecurityContextV1Id::from(id), listen_fd, close_fd),
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs::File,
        net::TcpListener,
        os::{
            fd::OwnedFd,
            linux::net::SocketAddrExt,
            unix::net::{SocketAddr, UnixListener, UnixStream},
        },
    };

    use crate::{
        clients::{Arguments, Client, ClientId, ClientManagerAction, RawRequest},
        globals::{Global, Globals},
        io_uring::model::IoUringAction,
        object::{Object, RequestResult},
        protocol_generated::{Constants, WpSecurityContextManagerV1Id, WpSecurityContextV1Id},
        runner::Dispatcher,
        security_context::SecurityContextMetadata,
        state::State,
        test_support::{state_for_tests, test_client_and_dispatcher},
    };

    use super::{WpSecurityContextManagerV1, WpSecurityContextManagerV1Global};

    struct Fixture {
        state: State,
        client: Client,
        dispatcher: Dispatcher,
    }

    fn fixture(sandboxed: bool) -> Fixture {
        let mut state = state_for_tests();
        let (mut client, dispatcher) = if sandboxed {
            (
                Client::new_sandboxed_for_tests(
                    ClientId::from_raw(1),
                    SecurityContextMetadata::default(),
                ),
                Dispatcher::default(),
            )
        } else {
            test_client_and_dispatcher(1)
        };
        client.insert_object(
            &mut state,
            WpSecurityContextManagerV1 { id: WpSecurityContextManagerV1Id::from(6) },
        );
        Fixture { state, client, dispatcher }
    }

    /// Sends `create_listener(new_id, listen_fd, close_fd)` on the manager at wire id 6.
    fn create_listener(
        fixture: &mut Fixture,
        new_id: u32,
        listen_fd: OwnedFd,
        close_fd: OwnedFd,
    ) -> RequestResult {
        let manager = WpSecurityContextManagerV1Id::from(6);
        let mut fds = VecDeque::from([listen_fd, close_fd]);
        let mut client_dispatcher = fixture.client.test_dispatcher(&mut fixture.dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(manager.into());
        let request = RawRequest {
            object_id: 6,
            request_id: 1,
            arguments: Arguments::from_words(VecDeque::from([new_id])),
            fds: Some(&mut fds),
        };
        let mut object = fixture.client.grab_object(manager, &mut fixture.state);
        object.on(&mut fixture.client, &mut fixture.state, request, &mut client_dispatcher)
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

    fn closed(fixture: &Fixture) -> bool {
        fixture.dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == fixture.client.client_id())
        })
    }

    fn stream_fd() -> OwnedFd {
        OwnedFd::from(UnixStream::pair().unwrap().0)
    }

    fn listening_socket(name: &str) -> OwnedFd {
        let address =
            SocketAddr::from_abstract_name(format!("tema-security-manager-{name}")).unwrap();
        OwnedFd::from(UnixListener::bind_addr(&address).unwrap())
    }

    #[test]
    fn the_global_is_advertised_at_version_one() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(WpSecurityContextManagerV1::INTERFACE_NAME));
        assert_eq!(
            WpSecurityContextManagerV1Global.interface(),
            ("wp_security_context_manager_v1", 1)
        );
    }

    #[test]
    fn a_listening_unix_socket_creates_a_context() {
        let mut fixture = fixture(false);
        create_listener(&mut fixture, 7, listening_socket("creates"), stream_fd()).unwrap();
        assert!(fixture
            .client
            .contains_object(WpSecurityContextV1Id::from(7), &fixture.state));
        assert_eq!(display_error(&fixture.dispatcher), None);
        assert!(!closed(&fixture));
    }

    #[test]
    fn a_connected_unix_socket_is_invalid() {
        let mut fixture = fixture(false);
        create_listener(&mut fixture, 7, stream_fd(), stream_fd()).unwrap();
        assert_eq!(display_error(&fixture.dispatcher), Some((6, 1)));
        assert!(closed(&fixture));
        assert!(!fixture
            .client
            .contains_object(WpSecurityContextV1Id::from(7), &fixture.state));
    }

    #[test]
    fn a_non_socket_is_invalid() {
        let mut fixture = fixture(false);
        let null = OwnedFd::from(File::open("/dev/null").unwrap());
        create_listener(&mut fixture, 7, null, stream_fd()).unwrap();
        assert_eq!(display_error(&fixture.dispatcher), Some((6, 1)));
        assert!(closed(&fixture));
    }

    #[test]
    fn a_tcp_listener_is_invalid() {
        let mut fixture = fixture(false);
        let tcp = OwnedFd::from(TcpListener::bind("127.0.0.1:0").unwrap());
        create_listener(&mut fixture, 7, tcp, stream_fd()).unwrap();
        assert_eq!(display_error(&fixture.dispatcher), Some((6, 1)));
        assert!(closed(&fixture));
    }

    #[test]
    fn a_sandboxed_client_gets_nested() {
        let mut fixture = fixture(true);
        create_listener(&mut fixture, 7, listening_socket("nested"), stream_fd()).unwrap();
        assert_eq!(display_error(&fixture.dispatcher), Some((6, 2)));
        assert!(closed(&fixture));
    }
}
