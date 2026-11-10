//! `xdg_toplevel_icon_manager_v1`. `set_icon` is applied on the toplevel's next commit.

use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    globals::Global,
    object::{Object, RequestResult},
    protocol_generated::{
        xdg_toplevel_icon_manager_v1, Constants, NewId, WaylandId, XdgToplevelIconManagerV1Id,
        XdgToplevelIconV1Id, XdgToplevelId,
    },
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, xdg_toplevel_icon_v1::XdgToplevelIconV1},
};

const ICON_SIZES: [i32; 6] = [16, 24, 32, 48, 64, 128];

/// Advertised at version 1.
#[derive(Debug)]
pub struct XdgToplevelIconManagerV1Global;

impl XdgToplevelIconManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for XdgToplevelIconManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (XdgToplevelIconManagerV1::INTERFACE_NAME, XdgToplevelIconManagerV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        let manager = XdgToplevelIconManagerV1 { id: XdgToplevelIconManagerV1Id::from(id) };
        for size in ICON_SIZES {
            manager.send_event(xdg_toplevel_icon_manager_v1::Event::IconSize { size }, dispatcher);
        }
        manager.send_event(xdg_toplevel_icon_manager_v1::Event::Done, dispatcher);
        client.insert_object(state, manager);
    }
}

#[derive(Debug)]
pub struct XdgToplevelIconManagerV1 {
    id: XdgToplevelIconManagerV1Id,
}

impl Object for XdgToplevelIconManagerV1 {
    fn id(&self) -> WaylandId {
        self.id.into()
    }

    fn on(
        self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        use xdg_toplevel_icon_manager_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            CreateIcon { id } => {
                client.insert_object(state, XdgToplevelIconV1::new(id));
            }
            SetIcon { toplevel, icon } => {
                let toplevel = client.resolve_or_disconnect(toplevel, state, dispatcher)?;
                let pending = match icon.to_opt() {
                    None => None,
                    Some(icon) => {
                        let icon = client.resolve_or_disconnect(icon, state, dispatcher)?;
                        let frozen = icon.try_ref_mut(state).unwrap().freeze();
                        (!frozen.is_empty()).then_some(frozen)
                    }
                };
                toplevel
                    .try_ref_mut(state)
                    .unwrap()
                    .set_pending_icon(pending);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::OwnedFd;

    use crate::{
        capture,
        clients::{Client, ClientId, ClientManagerAction, RawEvent, RawRequest, Resolved},
        dmabuf::{DmaBuf, DmaBufId, DmaBufPlane},
        drm::{Modifier, ARGB8888, XRGB8888},
        globals::{Global, Globals},
        object::{Object, RequestResult},
        protocol_generated::{
            Constants, NewId, WaylandId, WlBufferId, XdgSurfaceId, XdgToplevelIconManagerV1Id,
            XdgToplevelIconV1Id, XdgToplevelId, XdgWmBaseId,
        },
        state::ObjectId,
        test_support::{
            get_toplevel_request, raw_request_from_event, state_for_tests,
            test_client_and_dispatcher,
        },
        wayland_objects::{
            wl_buffer::{NewDmabufWlBufferParams, NewWlBufferParams, WlBuffer},
            wl_shm_pool::shared_memory_from_bytes,
            wl_surface::{Role, WlSurface},
            xdg_surface::{CommitRoleChanges, XdgSurface},
            xdg_toplevel_icon_v1::XdgToplevelIconV1,
        },
    };

    use super::{XdgToplevelIconManagerV1, XdgToplevelIconManagerV1Global};

    const MANAGER: u32 = 6;
    const ICON: u32 = 7;
    const TOPLEVEL: u32 = 22;

    struct Fixture {
        state: crate::state::State,
        client: Client,
        dispatcher: crate::runner::Dispatcher,
        manager: Resolved<XdgToplevelIconManagerV1Id>,
        xdg_surface: Resolved<XdgSurfaceId>,
        window: crate::window::WindowId,
    }

    fn new_fixture() -> Fixture {
        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let (window, xdg_surface) = {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            let (root, _) = client.insert_object(
                &mut state,
                WlSurface::new(
                    crate::protocol_generated::WlSurfaceId::from(20),
                    client.client_id(),
                    6,
                ),
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
                    get_toplevel_request(xdg_surface_id, XdgToplevelId::from(TOPLEVEL)),
                    &mut client_dispatcher,
                )
                .unwrap();
            (state.window_for_root(root).unwrap(), xdg_surface)
        };
        let (manager, _) = client.insert_object(
            &mut state,
            XdgToplevelIconManagerV1 { id: XdgToplevelIconManagerV1Id::from(MANAGER) },
        );
        Fixture { state, client, dispatcher, manager, xdg_surface, window }
    }

    fn request(
        opcode: u16,
        nargs: usize,
        write: impl FnOnce(&mut RawEvent),
    ) -> RawRequest<'static> {
        let mut event = RawEvent::new(WaylandId::from(MANAGER), opcode, nargs);
        write(&mut event);
        raw_request_from_event(event)
    }

    fn icon_request(
        icon: u32,
        opcode: u16,
        nargs: usize,
        write: impl FnOnce(&mut RawEvent),
    ) -> RawRequest<'static> {
        let mut event = RawEvent::new(WaylandId::from(icon), opcode, nargs);
        write(&mut event);
        raw_request_from_event(event)
    }

    fn dispatch_at(
        fixture: &mut Fixture,
        object: ObjectId,
        wire_id: u32,
        request: RawRequest<'static>,
    ) -> RequestResult {
        let mut client_dispatcher = fixture.client.test_dispatcher(&mut fixture.dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(WaylandId::from(wire_id));
        fixture.state.objects.grab(object).on(
            &mut fixture.client,
            &mut fixture.state,
            request,
            &mut client_dispatcher,
        )
    }

    fn dispatch_manager(fixture: &mut Fixture, request: RawRequest<'static>) -> RequestResult {
        let object = fixture.manager.object_id();
        dispatch_at(fixture, object, MANAGER, request)
    }

    fn create_icon(fixture: &mut Fixture, id: u32) -> Resolved<XdgToplevelIconV1Id> {
        dispatch_manager(fixture, request(1, 1, |event| event.id(XdgToplevelIconV1Id::from(id))))
            .unwrap();
        fixture
            .client
            .resolve(XdgToplevelIconV1Id::from(id), &fixture.state)
            .unwrap()
    }

    fn dispatch_icon(
        fixture: &mut Fixture,
        icon: Resolved<XdgToplevelIconV1Id>,
        request: RawRequest<'static>,
    ) -> RequestResult {
        dispatch_at(fixture, icon.object_id(), icon.wayland_id().into(), request)
    }

    fn set_icon(fixture: &mut Fixture, icon: u32) -> RequestResult {
        dispatch_manager(
            fixture,
            request(2, 2, |event| {
                event.id(XdgToplevelId::from(TOPLEVEL));
                event.id(XdgToplevelIconV1Id::from(icon));
            }),
        )
    }

    fn set_name(
        fixture: &mut Fixture,
        icon: Resolved<XdgToplevelIconV1Id>,
        name: &str,
    ) -> RequestResult {
        dispatch_icon(
            fixture,
            icon,
            icon_request(icon.wayland_id().into(), 1, 1, |event| {
                event.string(name);
            }),
        )
    }

    fn add_buffer(
        fixture: &mut Fixture,
        icon: Resolved<XdgToplevelIconV1Id>,
        buffer: u32,
        scale: i32,
    ) -> RequestResult {
        dispatch_icon(
            fixture,
            icon,
            icon_request(icon.wayland_id().into(), 2, 2, |event| {
                event.id(WlBufferId::from(buffer));
                event.int(scale);
            }),
        )
    }

    fn apply(fixture: &mut Fixture) {
        let mut xdg_surface = fixture
            .state
            .objects
            .grab_object::<XdgSurface>(fixture.xdg_surface.object_id());
        let mut client_dispatcher = fixture.client.test_dispatcher(&mut fixture.dispatcher);
        xdg_surface.apply_commit_role_changes(
            &mut fixture.client,
            &mut fixture.state,
            &mut client_dispatcher,
            CommitRoleChanges::default(),
        );
    }

    fn icon_name(fixture: &Fixture) -> Option<String> {
        capture::toplevel_info(&fixture.state, fixture.window)
            .and_then(|info| info.icon)
            .and_then(|icon| icon.name.clone())
    }

    fn stored_pixels(fixture: &Fixture, size: u32, scale: u32) -> Option<Vec<u8>> {
        fixture
            .state
            .toplevel(fixture.window)
            .unwrap()
            .icon()
            .and_then(|icon| {
                icon.best(size, scale)
                    .map(|pixmap| pixmap.argb8888.to_vec())
            })
    }

    fn sent_events(dispatcher: &crate::runner::Dispatcher) -> Vec<(u32, u32, u32, u32)> {
        let mut sent = Vec::new();
        dispatcher.test_for_each_queued_action::<crate::io_uring::model::IoUringAction>(|action| {
            let crate::io_uring::model::IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let object = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            let arg = if buffer.len() >= 12 {
                u32::from_ne_bytes(buffer[8..12].try_into().unwrap())
            } else {
                0
            };
            let second = if buffer.len() >= 16 {
                u32::from_ne_bytes(buffer[12..16].try_into().unwrap())
            } else {
                0
            };
            sent.push((object, opcode, arg, second));
        });
        sent
    }

    fn error_codes(dispatcher: &crate::runner::Dispatcher) -> Vec<u32> {
        sent_events(dispatcher)
            .into_iter()
            .filter_map(|(object, opcode, _object_id, code)| {
                (object == 1 && opcode == 0).then_some(code)
            })
            .collect()
    }

    fn closed(fixture: &Fixture) -> bool {
        fixture.dispatcher.test_has_queued_action(|action: &ClientManagerAction| {
            matches!(action, ClientManagerAction::Close { id } if *id == ClientId::from_raw(1))
        })
    }

    fn insert_shm(
        fixture: &mut Fixture,
        id: u32,
        width: i32,
        height: i32,
        format: &'static crate::drm::Format,
        pixels: &[u8],
    ) {
        fixture.client.insert_object(
            &mut fixture.state,
            WlBuffer::new(NewWlBufferParams {
                id: WlBufferId::from(id),
                offset: 0,
                width,
                height,
                stride: width.saturating_mul(4),
                format,
                mem: shared_memory_from_bytes(pixels),
            })
            .unwrap(),
        );
    }

    fn insert_dmabuf(fixture: &mut Fixture, id: u32) {
        let fd = OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
        fixture.client.insert_object(
            &mut fixture.state,
            WlBuffer::new_dmabuf(NewDmabufWlBufferParams {
                id: WlBufferId::from(id),
                dmabuf: DmaBuf {
                    id: DmaBufId::next(),
                    width: 1,
                    height: 1,
                    format: ARGB8888,
                    modifier: Modifier::LINEAR,
                    planes: Box::new([DmaBufPlane { offset: 0, stride: 4, fd }]),
                    sampling_device: None,
                },
                y_inverted: false,
                import_rejected: false,
            })
            .unwrap(),
        );
    }

    #[test]
    fn bind_sends_six_icon_sizes_then_done() {
        let mut globals = Globals::new();
        globals.initialize();
        assert!(globals.contains_interface(XdgToplevelIconManagerV1::INTERFACE_NAME));

        let mut state = state_for_tests();
        let (mut client, mut dispatcher) = test_client_and_dispatcher(1);
        let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
        let mut global = XdgToplevelIconManagerV1Global;
        global.bind(&mut client, &mut state, NewId::from(MANAGER), 1, &mut client_dispatcher);
        assert_eq!(
            sent_events(&dispatcher),
            vec![
                (MANAGER, 0, 16, 0),
                (MANAGER, 0, 24, 0),
                (MANAGER, 0, 32, 0),
                (MANAGER, 0, 48, 0),
                (MANAGER, 0, 64, 0),
                (MANAGER, 0, 128, 0),
                (MANAGER, 1, 0, 0),
            ]
        );
        assert_eq!(XdgToplevelIconManagerV1Global.interface(), ("xdg_toplevel_icon_manager_v1", 1));
    }

    #[test]
    fn add_buffer_rejects_a_bad_buffer_and_ignores_an_oversized_one() {
        let mut fixture = new_fixture();
        let icon = create_icon(&mut fixture, ICON);
        insert_shm(&mut fixture, 30, 2, 1, ARGB8888, &[0; 8]);
        assert!(add_buffer(&mut fixture, icon, 30, 1).is_ok());
        assert_eq!(error_codes(&fixture.dispatcher), vec![1]);
        assert!(closed(&fixture));

        let mut fixture = new_fixture();
        let icon = create_icon(&mut fixture, ICON);
        insert_dmabuf(&mut fixture, 30);
        assert!(add_buffer(&mut fixture, icon, 30, 1).is_ok());
        assert_eq!(error_codes(&fixture.dispatcher), vec![1]);
        assert!(closed(&fixture));

        let mut fixture = new_fixture();
        let icon = create_icon(&mut fixture, ICON);
        insert_shm(&mut fixture, 30, 1, 1, ARGB8888, &[1, 2, 3, 4]);
        assert!(add_buffer(&mut fixture, icon, 30, 0).is_ok());
        assert_eq!(error_codes(&fixture.dispatcher), vec![1]);
        assert!(closed(&fixture));

        let mut fixture = new_fixture();
        let icon = create_icon(&mut fixture, ICON);
        insert_shm(&mut fixture, 30, 512, 512, ARGB8888, &vec![0; 512 * 512 * 4]);
        assert!(add_buffer(&mut fixture, icon, 30, 1).is_ok());
        assert!(error_codes(&fixture.dispatcher).is_empty());
        assert!(!closed(&fixture));
        assert_eq!(icon.rref(&fixture.state).pixmap_count(), 0);
    }

    #[test]
    fn the_seventeenth_pixmap_is_ignored_and_the_same_size_replaces() {
        let mut fixture = new_fixture();
        let icon = create_icon(&mut fixture, ICON);
        for (index, size) in (1..=16).enumerate() {
            let id = 30 + index as u32;
            insert_shm(
                &mut fixture,
                id,
                size,
                size,
                ARGB8888,
                &vec![index as u8; (size * size * 4) as usize],
            );
            assert!(add_buffer(&mut fixture, icon, id, 1).is_ok());
        }
        insert_shm(&mut fixture, 50, 17, 17, ARGB8888, &vec![9; 17 * 17 * 4]);
        assert!(add_buffer(&mut fixture, icon, 50, 1).is_ok());
        assert_eq!(icon.rref(&fixture.state).pixmap_count(), 16);
        assert!(icon.rref(&fixture.state).pixmap_bytes(17, 1).is_none());
        assert!(!closed(&fixture));

        insert_shm(&mut fixture, 51, 1, 1, ARGB8888, &[9, 8, 7, 6]);
        assert!(add_buffer(&mut fixture, icon, 51, 1).is_ok());
        assert_eq!(icon.rref(&fixture.state).pixmap_bytes(1, 1), Some(&[9, 8, 7, 6][..]));
        assert_eq!(icon.rref(&fixture.state).pixmap_count(), 16);
    }

    #[test]
    fn set_name_and_add_buffer_after_set_icon_are_immutable() {
        let mut named = new_fixture();
        let icon = create_icon(&mut named, ICON);
        set_name(&mut named, icon, "firefox").unwrap();
        set_icon(&mut named, ICON).unwrap();
        assert!(set_name(&mut named, icon, "other").is_ok());
        assert_eq!(error_codes(&named.dispatcher), vec![2]);
        assert!(closed(&named));

        let mut buffered = new_fixture();
        let icon = create_icon(&mut buffered, ICON);
        insert_shm(&mut buffered, 30, 1, 1, ARGB8888, &[1, 2, 3, 4]);
        add_buffer(&mut buffered, icon, 30, 1).unwrap();
        set_icon(&mut buffered, ICON).unwrap();
        assert!(add_buffer(&mut buffered, icon, 30, 1).is_ok());
        assert_eq!(error_codes(&buffered.dispatcher), vec![2]);
        assert!(closed(&buffered));
    }

    #[test]
    fn set_icon_appears_on_commit_and_null_or_empty_clears_it() {
        let mut fixture = new_fixture();
        capture::map_window(&mut fixture.state, &mut fixture.dispatcher, fixture.window);
        let icon = create_icon(&mut fixture, ICON);
        set_name(&mut fixture, icon, "firefox").unwrap();
        set_icon(&mut fixture, ICON).unwrap();
        assert!(fixture
            .state
            .toplevel(fixture.window)
            .unwrap()
            .icon()
            .is_none());
        assert_eq!(icon_name(&fixture), None);

        apply(&mut fixture);
        assert_eq!(
            fixture
                .state
                .toplevel(fixture.window)
                .unwrap()
                .icon()
                .unwrap()
                .summary()
                .name
                .as_deref(),
            Some("firefox")
        );
        assert_eq!(icon_name(&fixture).as_deref(), Some("firefox"));

        set_icon(&mut fixture, 0).unwrap();
        assert_eq!(icon_name(&fixture).as_deref(), Some("firefox"));
        apply(&mut fixture);
        assert!(fixture
            .state
            .toplevel(fixture.window)
            .unwrap()
            .icon()
            .is_none());
        assert_eq!(icon_name(&fixture), None);

        let empty = create_icon(&mut fixture, 8);
        set_icon(&mut fixture, empty.wayland_id().into()).unwrap();
        apply(&mut fixture);
        assert!(fixture
            .state
            .toplevel(fixture.window)
            .unwrap()
            .icon()
            .is_none());
        assert_eq!(icon_name(&fixture), None);
    }

    #[test]
    fn a_mapped_window_keeps_an_icon_that_was_committed_before_map() {
        let mut fixture = new_fixture();
        let icon = create_icon(&mut fixture, ICON);
        set_name(&mut fixture, icon, "firefox").unwrap();
        set_icon(&mut fixture, ICON).unwrap();
        apply(&mut fixture);
        assert!(capture::toplevel_info(&fixture.state, fixture.window).is_none());
        capture::map_window(&mut fixture.state, &mut fixture.dispatcher, fixture.window);
        assert_eq!(icon_name(&fixture).as_deref(), Some("firefox"));
    }

    #[test]
    fn destroying_the_icon_and_buffer_keeps_the_copied_pixels() {
        let mut fixture = new_fixture();
        let icon = create_icon(&mut fixture, ICON);
        let pixels = [4, 5, 6, 7];
        insert_shm(&mut fixture, 30, 1, 1, ARGB8888, &pixels);
        let buffer = fixture
            .client
            .resolve(WlBufferId::from(30), &fixture.state)
            .unwrap();
        add_buffer(&mut fixture, icon, 30, 1).unwrap();
        set_icon(&mut fixture, ICON).unwrap();
        dispatch_icon(&mut fixture, icon, icon_request(ICON, 0, 0, |_| {})).unwrap();
        {
            let mut client_dispatcher = fixture.client.test_dispatcher(&mut fixture.dispatcher);
            fixture.client.delete_id(
                &crate::wayland_objects::wl_display::WlDisplay::default(),
                &mut fixture.state,
                &mut client_dispatcher,
                buffer.wayland_id().into(),
            );
        }
        apply(&mut fixture);
        assert_eq!(stored_pixels(&fixture, 1, 1).as_deref(), Some(&pixels[..]));
        assert!(!fixture
            .client
            .contains_object(XdgToplevelIconV1Id::from(ICON), &fixture.state));
    }

    #[test]
    fn xrgb_alpha_is_forced_opaque() {
        let mut fixture = new_fixture();
        let icon = create_icon(&mut fixture, ICON);
        insert_shm(&mut fixture, 30, 1, 1, XRGB8888, &[1, 2, 3, 0]);
        add_buffer(&mut fixture, icon, 30, 1).unwrap();
        assert_eq!(icon.rref(&fixture.state).pixmap_bytes(1, 1), Some(&[1, 2, 3, 255][..]));
    }
}
