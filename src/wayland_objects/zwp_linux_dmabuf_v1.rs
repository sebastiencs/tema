use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        wl_display, zwp_linux_dmabuf_v1, Constants, NewId, WaylandId, ZwpLinuxDmabufV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::{
        zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        zwp_linux_dmabuf_feedback_v1::{self, FeedbackTarget, ZwpLinuxDmabufFeedbackV1},
    },
};

#[derive(Debug)]
pub struct ZwpLinuxDmabufV1Global;

impl ZwpLinuxDmabufV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for ZwpLinuxDmabufV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (ZwpLinuxDmabufV1::INTERFACE_NAME, 6)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        version: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        let id = ZwpLinuxDmabufV1Id::from(id);
        // Versions before 4 get one flat list: the first DRM device's sampleable formats.
        let supported_formats = (version < 4).then(|| {
            state
                .drm
                .iter()
                .next()
                .map(|(_, drm)| drm.supported_dmabuf_read_formats())
        });
        let (_, dmabuf) = client.insert_object(state, ZwpLinuxDmabufV1 { id, version });
        if version >= 4 {
            log::debug!("advertised linux-dmabuf feedback to client {:?}", client.client_id());
            return;
        }
        let Some(Some(supported_formats)) = supported_formats else {
            log::warn!("linux-dmabuf bound before DRM backend became available");
            return;
        };
        for (format, modifiers) in supported_formats {
            dmabuf.send_event(
                zwp_linux_dmabuf_v1::Event::Format { format: format.drm as u32 },
                dispatcher,
            );
            if version < 3 {
                continue;
            }
            for modifier in modifiers {
                dmabuf.send_event(
                    zwp_linux_dmabuf_v1::Event::Modifier {
                        format: format.drm as u32,
                        modifier_hi: modifier.high(),
                        modifier_lo: modifier.low(),
                    },
                    dispatcher,
                );
            }
        }
        log::debug!("advertised linux-dmabuf global to client {:?}", client.client_id());
    }
}

#[derive(Debug)]
pub struct ZwpLinuxDmabufV1 {
    id: ZwpLinuxDmabufV1Id,
    version: u32,
}

impl ZwpLinuxDmabufV1 {
    pub(crate) fn new(id: ZwpLinuxDmabufV1Id, version: u32) -> Self {
        Self { id, version }
    }

    pub(crate) fn version(&self) -> u32 {
        self.version
    }

    fn create_feedback(
        &self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        id: crate::protocol_generated::ZwpLinuxDmabufFeedbackV1Id,
        target: FeedbackTarget,
    ) {
        let (resolved, _) =
            client.insert_object(state, ZwpLinuxDmabufFeedbackV1::new(id, self.version, target));
        zwp_linux_dmabuf_feedback_v1::register(state, resolved, target);
        if let Some(mut feedback) = resolved.try_grab(state) {
            zwp_linux_dmabuf_feedback_v1::send_current(&mut feedback, state, dispatcher);
        }
    }
}

impl Object for ZwpLinuxDmabufV1 {
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
        use zwp_linux_dmabuf_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                use crate::wayland_objects::wl_display::WlDisplay;
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            CreateParams { params_id } => {
                let params = ZwpLinuxBufferParamsV1::new(params_id, self.version);
                client.insert_object(state, params);
            }
            GetDefaultFeedback { id } => {
                if self.version < zwp_linux_dmabuf_v1::GET_DEFAULT_FEEDBACK_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "linux-dmabuf feedback is not advertised by this compositor version",
                    );
                    return Ok(());
                }
                self.create_feedback(client, state, dispatcher, id, FeedbackTarget::Default);
            }
            GetSurfaceFeedback { id, surface } => {
                if self.version < zwp_linux_dmabuf_v1::GET_SURFACE_FEEDBACK_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "linux-dmabuf feedback is not advertised by this compositor version",
                    );
                    return Ok(());
                }
                let Ok(surface) = client.resolve_or_disconnect(surface, state, dispatcher) else {
                    return Ok(());
                };
                self.create_feedback(
                    client,
                    state,
                    dispatcher,
                    id,
                    FeedbackTarget::Surface(surface),
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            clients::{Client, ClientId, RawEvent},
            io_uring::model::IoUringAction,
            protocol_generated::{WlDisplayId, WlRegistryId},
            runner::Dispatcher,
            test_support::state_for_tests,
            wayland_objects::wl_display::WlDisplay,
        },
    };

    fn request(object: u32, opcode: u16) -> RawEvent {
        RawEvent::new(WaylandId::from(object), opcode, 8)
    }

    fn dispatch<K>(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        object: K,
        event: RawEvent,
    ) where
        K: crate::object::IdToObject + Into<WaylandId> + Copy,
    {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(object.into());
        client.grab_object(object, state).on(
            client,
            state,
            (&*event.into_bytes()).try_into().unwrap(),
            &mut client_dispatcher,
        );
    }

    fn advertised_version(dispatcher: &Dispatcher) -> Option<u32> {
        let name = b"zwp_linux_dmabuf_v1";
        let mut found = None;
        dispatcher.test_for_each_queued_action(|action: &IoUringAction| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            let Some(at) = buffer.windows(name.len()).position(|window| window == name) else {
                return;
            };
            let version_at = (at + name.len() + 1).next_multiple_of(4);
            if buffer.len() < version_at + 4 {
                return;
            }
            found =
                Some(u32::from_ne_bytes(buffer[version_at..version_at + 4].try_into().unwrap()));
        });
        found
    }

    fn messages_for(dispatcher: &Dispatcher, object_id: u32) -> usize {
        let mut count = 0;
        dispatcher.test_for_each_queued_action(|action: &IoUringAction| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() >= 4
                && u32::from_ne_bytes(buffer[0..4].try_into().unwrap()) == object_id
            {
                count += 1;
            }
        });
        count
    }

    #[test]
    fn the_global_advertises_version_6_and_bind_clamps_to_it() {
        let mut state = state_for_tests();
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut dispatcher = Dispatcher::default();
        client.insert_object(&mut state, WlDisplay::default());
        let mut get_registry = request(1, 1);
        get_registry.id(WlRegistryId::from(2));
        dispatch(&mut client, &mut state, &mut dispatcher, WlDisplayId::from(1u32), get_registry);
        assert_eq!(advertised_version(&dispatcher), Some(6));

        let name = state
            .globals
            .iter()
            .find_map(|(name, global)| {
                (global.interface().0 == ZwpLinuxDmabufV1::INTERFACE_NAME).then_some(name)
            })
            .unwrap();
        let mut bind = request(2, 0);
        bind.uint(name.as_u32());
        bind.string("zwp_linux_dmabuf_v1");
        bind.uint(99);
        bind.id(9u32);
        dispatch(&mut client, &mut state, &mut dispatcher, WlRegistryId::from(2), bind);
        let bound = client.resolve(ZwpLinuxDmabufV1Id::from(9), &state).unwrap();
        assert_eq!(bound.rref(&state).version(), 6);
        assert_eq!(messages_for(&dispatcher, 9), 0);

        let mut bind = request(2, 0);
        bind.uint(name.as_u32());
        bind.string("zwp_linux_dmabuf_v1");
        bind.uint(3);
        bind.id(10u32);
        dispatch(&mut client, &mut state, &mut dispatcher, WlRegistryId::from(2), bind);
        let bound = client
            .resolve(ZwpLinuxDmabufV1Id::from(10), &state)
            .unwrap();
        assert_eq!(bound.rref(&state).version(), 3);
    }
}
