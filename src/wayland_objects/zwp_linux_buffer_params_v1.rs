use std::{
    collections::BTreeMap,
    mem::size_of,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    rc::Rc,
};

use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    dmabuf::{DmaBuf, DmaBufId, DmaBufPlane},
    dmabuf_feedback::render_dev_t,
    drm::{Modifier, FORMATS_MAP},
    object::{Object, Parser, RequestResult},
    protocol_generated::{zwp_linux_buffer_params_v1, WaylandId, ZwpLinuxBufferParamsV1Id},
    render::Renderer,
    state::{DrmId, GrabbedObject, State},
    wayland_objects::wl_buffer::{NewDmabufWlBufferParams, WlBuffer},
};

#[derive(Debug)]
struct PendingPlane {
    fd: Rc<OwnedFd>,
    offset: u32,
    stride: u32,
    modifier: Modifier,
}

#[derive(Debug)]
pub struct ZwpLinuxBufferParamsV1 {
    id: ZwpLinuxBufferParamsV1Id,
    /// Version of the `zwp_linux_dmabuf_v1` that created these params.
    version: u32,
    used: bool,
    planes: BTreeMap<u32, PendingPlane>,
    /// Last `set_sampling_device` value. A later call replaces it.
    sampling_device: Option<libc::dev_t>,
}

impl ZwpLinuxBufferParamsV1 {
    pub fn new(id: ZwpLinuxBufferParamsV1Id, version: u32) -> Self {
        Self {
            id,
            version,
            used: false,
            planes: BTreeMap::new(),
            sampling_device: None,
        }
    }

    fn already_used(
        &self,
        dispatcher: &mut ClientDispatcher,
    ) -> Result<(), zwp_linux_buffer_params_v1::Error> {
        if self.used {
            self.send_error(
                dispatcher,
                zwp_linux_buffer_params_v1::Error::AlreadyUsed as u32,
                "buffer params already used",
            );
            Err(zwp_linux_buffer_params_v1::Error::AlreadyUsed)
        } else {
            Ok(())
        }
    }

    fn build_dmabuf(
        &self,
        width: i32,
        height: i32,
        format: u32,
    ) -> Result<DmaBuf, zwp_linux_buffer_params_v1::Error> {
        if width <= 0 || height <= 0 {
            return Err(zwp_linux_buffer_params_v1::Error::InvalidDimensions);
        }
        let drm_format = drm::buffer::DrmFourcc::try_from(format)
            .map_err(|_| zwp_linux_buffer_params_v1::Error::InvalidFormat)?;
        let format = *FORMATS_MAP
            .get(&drm_format)
            .ok_or(zwp_linux_buffer_params_v1::Error::InvalidFormat)?;
        if self.planes.is_empty() {
            return Err(zwp_linux_buffer_params_v1::Error::Incomplete);
        }
        let max_plane = *self.planes.keys().last().unwrap();
        if self.planes.len() != (max_plane as usize + 1) {
            return Err(zwp_linux_buffer_params_v1::Error::Incomplete);
        }

        let mut modifier = None;
        let planes = self
            .planes
            .iter()
            .map(|(index, plane)| {
                if *index > 3 {
                    return Err(zwp_linux_buffer_params_v1::Error::PlaneIdx);
                }
                if let Some(existing) = modifier {
                    if existing != plane.modifier {
                        return Err(zwp_linux_buffer_params_v1::Error::InvalidFormat);
                    }
                } else {
                    modifier = Some(plane.modifier);
                }
                let fd = plane.fd.as_raw_fd();
                let fd = unsafe { OwnedFd::from_raw_fd(libc::dup(fd)) };
                Ok(DmaBufPlane { offset: plane.offset, stride: plane.stride, fd })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(DmaBuf {
            id: DmaBufId::next(),
            width,
            height,
            format,
            modifier: modifier.unwrap_or(Modifier::INVALID),
            planes: planes.into_boxed_slice(),
            sampling_device: self.sampling_device,
        })
    }
}

struct NamedDev {
    render_dev: Option<libc::dev_t>,
    primary_dev: Option<libc::dev_t>,
}

/// `None` means there is no device, so the buffer is accepted.
/// A sampling device that was never advertised falls back to every device.
fn indexes_to_test(devices: &[NamedDev], sampling: Option<libc::dev_t>) -> Option<Vec<usize>> {
    if devices.is_empty() {
        return None;
    }
    if let Some(sampling) = sampling {
        let matched: Vec<_> = devices
            .iter()
            .enumerate()
            .filter(|(_, device)| {
                device.render_dev == Some(sampling) || device.primary_dev == Some(sampling)
            })
            .map(|(index, _)| index)
            .collect();
        if !matched.is_empty() {
            return Some(matched);
        }
        log::debug!("linux-dmabuf sampling device {sampling:#x} was not advertised");
    }
    Some((0..devices.len()).collect())
}

fn named_import_ok(
    state: &mut State,
    dmabuf: &DmaBuf,
    named: &[NamedDev],
    drm_ids: &[DrmId],
) -> bool {
    let Some(indexes) = indexes_to_test(named, dmabuf.sampling_device) else {
        log::debug!("linux-dmabuf create accepted with no DRM device");
        return true;
    };
    indexes.into_iter().any(|index| {
        state
            .drm
            .try_get_mut(drm_ids[index])
            .is_some_and(|drm| drm.test_import_client_dmabuf(dmabuf).is_ok())
    })
}

/// True when some selected device can sample `dmabuf`, or when no device exists yet.
fn client_dmabuf_importable(state: &mut State, dmabuf: &DmaBuf) -> bool {
    #[cfg(test)]
    if let Some(devices) = state.dmabuf_importers.as_mut() {
        let named: Vec<_> = devices
            .iter()
            .map(|device| NamedDev {
                render_dev: device.render_dev,
                primary_dev: device.primary_dev,
            })
            .collect();
        let Some(indexes) = indexes_to_test(&named, dmabuf.sampling_device) else {
            log::debug!("linux-dmabuf create accepted with no DRM device");
            return true;
        };
        return indexes
            .into_iter()
            .any(|index| devices[index].renderer.test_import_dmabuf(dmabuf).is_ok());
    }
    let listed: Vec<_> = state
        .drm
        .iter()
        .map(|(id, drm)| {
            (id, NamedDev { render_dev: render_dev_t(drm), primary_dev: drm.devnum() })
        })
        .collect();
    let (drm_ids, named): (Vec<_>, Vec<_>) = listed.into_iter().unzip();
    named_import_ok(state, dmabuf, &named, &drm_ids)
}

impl Object for ZwpLinuxBufferParamsV1 {
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
        use zwp_linux_buffer_params_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                use crate::wayland_objects::wl_display::WlDisplay;
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            Add { fd, plane_idx, offset, stride, modifier_hi, modifier_lo } => {
                if self.already_used(dispatcher).is_err() {
                    return Ok(());
                }
                if plane_idx > 3 {
                    self.send_error(
                        dispatcher,
                        zwp_linux_buffer_params_v1::Error::PlaneIdx as u32,
                        "plane index out of bounds",
                    );
                    return Ok(());
                }
                if self.planes.contains_key(&plane_idx) {
                    self.send_error(
                        dispatcher,
                        zwp_linux_buffer_params_v1::Error::PlaneSet as u32,
                        "plane index already set",
                    );
                    return Ok(());
                }
                let modifier = Modifier(((modifier_hi as u64) << 32) | modifier_lo as u64);
                self.planes
                    .insert(plane_idx, PendingPlane { fd, offset, stride, modifier });
            }
            Create { width, height, format, flags } => {
                if self.already_used(dispatcher).is_err() {
                    return Ok(());
                }
                self.used = true;
                let y_inverted = (flags & zwp_linux_buffer_params_v1::Flags::YInvert as u32) != 0;
                match self.build_dmabuf(width, height, format) {
                    Ok(dmabuf) => {
                        if !client_dmabuf_importable(state, &dmabuf) {
                            self.send_event(zwp_linux_buffer_params_v1::Event::Failed, dispatcher);
                            return Ok(());
                        }
                        match WlBuffer::new_dmabuf(NewDmabufWlBufferParams {
                            id: client.allocate_server_object_id(),
                            owner_client_id: client.client_id(),
                            dmabuf,
                            y_inverted,
                            import_rejected: false,
                        }) {
                            Ok(buffer) => {
                                let (buffer, _) = client.insert_object(state, buffer);
                                let buffer_wayland_id: WaylandId = buffer.wayland_id().into();
                                self.send_event(
                                    zwp_linux_buffer_params_v1::Event::Created {
                                        buffer: buffer_wayland_id.into(),
                                    },
                                    dispatcher,
                                );
                                log::debug!(
                                    "created client dma-buf wl_buffer {:?}",
                                    buffer_wayland_id
                                );
                            }
                            Err(_) => self
                                .send_event(zwp_linux_buffer_params_v1::Event::Failed, dispatcher),
                        }
                    }
                    Err(error) => {
                        self.send_error(dispatcher, error as u32, "invalid dma-buf buffer params");
                    }
                }
            }
            CreateImmed { buffer_id, width, height, format, flags } => {
                if self.already_used(dispatcher).is_err() {
                    return Ok(());
                }
                self.used = true;
                let y_inverted = (flags & zwp_linux_buffer_params_v1::Flags::YInvert as u32) != 0;
                match self.build_dmabuf(width, height, format) {
                    Ok(dmabuf) => {
                        let rejected = !client_dmabuf_importable(state, &dmabuf);
                        match WlBuffer::new_dmabuf(NewDmabufWlBufferParams {
                            id: buffer_id,
                            owner_client_id: client.client_id(),
                            dmabuf,
                            y_inverted,
                            import_rejected: rejected,
                        }) {
                            Ok(buffer) => {
                                if rejected {
                                    client.insert_object(state, buffer);
                                    self.send_event(
                                        zwp_linux_buffer_params_v1::Event::Failed,
                                        dispatcher,
                                    );
                                } else {
                                    client.insert_object(state, buffer);
                                    log::debug!(
                                        "created immediate dma-buf wl_buffer {:?}",
                                        buffer_id
                                    );
                                }
                            }
                            Err(_) => self.send_error(
                                dispatcher,
                                zwp_linux_buffer_params_v1::Error::InvalidWlBuffer as u32,
                                "failed to create wl_buffer from dma-buf params",
                            ),
                        }
                    }
                    Err(error) => {
                        self.send_error(dispatcher, error as u32, "invalid dma-buf params")
                    }
                }
            }
            SetSamplingDevice { device } => {
                if self.version < zwp_linux_buffer_params_v1::SET_SAMPLING_DEVICE_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        crate::protocol_generated::wl_display::Error::InvalidMethod as u32,
                        "set_sampling_device is not advertised by this compositor version",
                    );
                    return Ok(());
                }
                if self.already_used(dispatcher).is_err() {
                    return Ok(());
                }
                let Some(dev) = dev_t_from_array(device.as_bytes()) else {
                    self.send_error(
                        dispatcher,
                        zwp_linux_buffer_params_v1::Error::InvalidDevTSize as u32,
                        "sampling device array is not the size of dev_t",
                    );
                    return Ok(());
                };
                self.sampling_device = Some(dev);
            }
        }
        Ok(())
    }
}

fn dev_t_from_array(bytes: &[u8]) -> Option<libc::dev_t> {
    let bytes: [u8; size_of::<libc::dev_t>()] = bytes.try_into().ok()?;
    Some(libc::dev_t::from_ne_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, fs::File, os::fd::OwnedFd};

    use {
        super::*,
        crate::{
            clients::{Arguments, Client, ClientId, RawRequest},
            io_uring::model::IoUringAction,
            render::mock::{MockDmabufImporter, MockRenderer},
            runner::Dispatcher,
            test_support::state_for_tests,
            wayland_objects::wl_buffer::WlBuffer,
        },
    };

    fn array_words(bytes: &[u8]) -> Vec<u32> {
        let mut padded = bytes.to_vec();
        while !padded.len().is_multiple_of(4) {
            padded.push(0);
        }
        let mut words = vec![bytes.len() as u32];
        for chunk in padded.chunks(4) {
            words.push(u32::from_ne_bytes(chunk.try_into().unwrap()));
        }
        words
    }

    fn dispatch(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        object: crate::state::ObjectId,
        opcode: u16,
        words: &[u32],
        mut fds: VecDeque<OwnedFd>,
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(4.into());
        let request = RawRequest {
            object_id: 4,
            request_id: opcode,
            arguments: Arguments::from_words(VecDeque::from(words.to_vec())),
            fds: Some(&mut fds),
        };
        state
            .objects
            .grab(object)
            .on(client, state, request, &mut client_dispatcher)
            .unwrap();
    }

    fn error_codes(dispatcher: &Dispatcher) -> Vec<u32> {
        let mut codes = Vec::new();
        dispatcher.test_for_each_queued_action(|action: &IoUringAction| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 16 {
                return;
            }
            let object_id = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            let opcode = u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff;
            if object_id != 1 || opcode != 0 {
                return;
            }
            codes.push(u32::from_ne_bytes(buffer[12..16].try_into().unwrap()));
        });
        codes
    }

    #[test]
    fn set_sampling_device_rejects_the_wrong_size_and_reaches_the_buffer() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (params, _) = client.insert_object(
            &mut state,
            ZwpLinuxBufferParamsV1::new(ZwpLinuxBufferParamsV1Id::from(4), 6),
        );

        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            4,
            &array_words(&[1, 2, 3, 4]),
            VecDeque::new(),
        );
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            4,
            &array_words(&[1, 2, 3, 4, 5, 6, 7, 8, 9]),
            VecDeque::new(),
        );
        assert_eq!(
            error_codes(&dispatcher),
            vec![
                zwp_linux_buffer_params_v1::Error::InvalidDevTSize as u32,
                zwp_linux_buffer_params_v1::Error::InvalidDevTSize as u32,
            ]
        );

        let dev: libc::dev_t = 0x1122_3344_5566_7788;
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            4,
            &array_words(&dev.to_ne_bytes()),
            VecDeque::new(),
        );
        let fd = OwnedFd::from(File::open("/dev/null").unwrap());
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            1,
            &[0, 0, 4, 0, 0],
            VecDeque::from([fd]),
        );
        let format = drm::buffer::DrmFourcc::Argb8888 as u32;
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            2,
            &[1, 1, format, 0],
            VecDeque::new(),
        );
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            4,
            &array_words(&dev.to_ne_bytes()),
            VecDeque::new(),
        );

        let ids: Vec<_> = state.objects.iter().map(|(id, _)| id).collect();
        let sampling = ids.into_iter().find_map(|id| {
            state
                .objects
                .try_ref_object::<WlBuffer>(id)?
                .content()
                .dmabuf()
                .and_then(|dmabuf| dmabuf.sampling_device)
        });
        assert_eq!(sampling, Some(dev));
        assert!(error_codes(&dispatcher)
            .contains(&(zwp_linux_buffer_params_v1::Error::AlreadyUsed as u32)));
    }

    #[test]
    fn set_sampling_device_before_version_6_disconnects() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (params, _) = client.insert_object(
            &mut state,
            ZwpLinuxBufferParamsV1::new(ZwpLinuxBufferParamsV1Id::from(4), 5),
        );
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            4,
            &array_words(&1u64.to_ne_bytes()),
            VecDeque::new(),
        );
        assert!(dispatcher.test_has_queued_action(
            |action: &crate::clients::ClientManagerAction| {
                matches!(action, crate::clients::ClientManagerAction::Close { .. })
            }
        ));
        assert_eq!(
            error_codes(&dispatcher),
            vec![crate::protocol_generated::wl_display::Error::InvalidMethod as u32]
        );
    }

    fn params_opcodes(dispatcher: &Dispatcher) -> Vec<u16> {
        let mut opcodes = Vec::new();
        dispatcher.test_for_each_queued_action(|action: &IoUringAction| {
            let IoUringAction::Sendmsg { buffer, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let object_id = u32::from_ne_bytes(buffer[0..4].try_into().unwrap());
            if object_id != 4 {
                return;
            }
            opcodes.push((u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff) as u16);
        });
        opcodes
    }

    fn importer(
        render_dev: libc::dev_t,
        primary_dev: libc::dev_t,
        fail: bool,
    ) -> MockDmabufImporter {
        let mut renderer = MockRenderer::default();
        if fail {
            renderer.fail_dmabuf_import();
        }
        MockDmabufImporter {
            render_dev: Some(render_dev),
            primary_dev: Some(primary_dev),
            renderer,
        }
    }

    fn add_plane(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        params: crate::state::ObjectId,
    ) {
        let fd = OwnedFd::from(File::open("/dev/null").unwrap());
        dispatch(client, state, dispatcher, params, 1, &[0, 0, 4, 0, 0], VecDeque::from([fd]));
    }

    fn create_buffer(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        params: crate::state::ObjectId,
    ) {
        let format = drm::buffer::DrmFourcc::Argb8888 as u32;
        dispatch(client, state, dispatcher, params, 2, &[1, 1, format, 0], VecDeque::new());
    }

    fn closed(dispatcher: &Dispatcher) -> bool {
        dispatcher.test_has_queued_action(|action: &crate::clients::ClientManagerAction| {
            matches!(action, crate::clients::ClientManagerAction::Close { .. })
        })
    }

    #[test]
    fn create_accepts_a_buffer_when_no_device_can_test_it() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        state.dmabuf_importers = Some(Vec::new());
        let mut dispatcher = Dispatcher::default();
        let (params, _) = client.insert_object(
            &mut state,
            ZwpLinuxBufferParamsV1::new(ZwpLinuxBufferParamsV1Id::from(4), 6),
        );
        add_plane(&mut client, &mut state, &mut dispatcher, params.object_id());
        create_buffer(&mut client, &mut state, &mut dispatcher, params.object_id());
        assert_eq!(params_opcodes(&dispatcher), vec![0]);
        assert!(!closed(&dispatcher));
    }

    #[test]
    fn create_sends_failed_when_the_mock_renderer_rejects_the_import() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        state.dmabuf_importers = Some(vec![importer(10, 1, true)]);
        let mut dispatcher = Dispatcher::default();
        let (params, _) = client.insert_object(
            &mut state,
            ZwpLinuxBufferParamsV1::new(ZwpLinuxBufferParamsV1Id::from(4), 6),
        );
        add_plane(&mut client, &mut state, &mut dispatcher, params.object_id());
        create_buffer(&mut client, &mut state, &mut dispatcher, params.object_id());
        assert_eq!(params_opcodes(&dispatcher), vec![1]);
        assert!(!closed(&dispatcher));
        let ids: Vec<_> = state.objects.iter().map(|(id, _)| id).collect();
        assert!(ids
            .into_iter()
            .all(|id| state.objects.try_ref_object::<WlBuffer>(id).is_none()));
    }

    #[test]
    fn sampling_device_tests_only_the_matching_renderer() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        state.dmabuf_importers = Some(vec![importer(10, 1, true), importer(11, 2, false)]);
        let mut dispatcher = Dispatcher::default();
        let (params, _) = client.insert_object(
            &mut state,
            ZwpLinuxBufferParamsV1::new(ZwpLinuxBufferParamsV1Id::from(4), 6),
        );
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            4,
            &array_words(&10u64.to_ne_bytes()),
            VecDeque::new(),
        );
        add_plane(&mut client, &mut state, &mut dispatcher, params.object_id());
        create_buffer(&mut client, &mut state, &mut dispatcher, params.object_id());
        assert_eq!(params_opcodes(&dispatcher), vec![1]);
        let devices = state.dmabuf_importers.as_ref().unwrap();
        assert!(devices[0].renderer.made_current());
        assert!(!devices[1].renderer.made_current());

        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        state.dmabuf_importers = Some(vec![importer(10, 1, true), importer(11, 2, false)]);
        let mut dispatcher = Dispatcher::default();
        let (params, _) = client.insert_object(
            &mut state,
            ZwpLinuxBufferParamsV1::new(ZwpLinuxBufferParamsV1Id::from(4), 6),
        );
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            4,
            &array_words(&2u64.to_ne_bytes()),
            VecDeque::new(),
        );
        add_plane(&mut client, &mut state, &mut dispatcher, params.object_id());
        create_buffer(&mut client, &mut state, &mut dispatcher, params.object_id());
        assert_eq!(params_opcodes(&dispatcher), vec![0]);
        let devices = state.dmabuf_importers.as_ref().unwrap();
        assert!(!devices[0].renderer.made_current());
        assert!(devices[1].renderer.made_current());
    }

    #[test]
    fn an_unknown_sampling_device_tries_every_renderer() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        state.dmabuf_importers = Some(vec![importer(10, 1, true), importer(11, 2, false)]);
        let mut dispatcher = Dispatcher::default();
        let (params, _) = client.insert_object(
            &mut state,
            ZwpLinuxBufferParamsV1::new(ZwpLinuxBufferParamsV1Id::from(4), 6),
        );
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            4,
            &array_words(&99u64.to_ne_bytes()),
            VecDeque::new(),
        );
        add_plane(&mut client, &mut state, &mut dispatcher, params.object_id());
        create_buffer(&mut client, &mut state, &mut dispatcher, params.object_id());
        assert_eq!(params_opcodes(&dispatcher), vec![0]);
        let devices = state.dmabuf_importers.as_ref().unwrap();
        assert!(devices[0].renderer.made_current());
        assert!(devices[1].renderer.made_current());
    }

    #[test]
    fn create_immed_sends_failed_and_keeps_an_unusable_buffer() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        state.dmabuf_importers = Some(vec![importer(10, 1, true)]);
        let mut dispatcher = Dispatcher::default();
        let (params, _) = client.insert_object(
            &mut state,
            ZwpLinuxBufferParamsV1::new(ZwpLinuxBufferParamsV1Id::from(4), 6),
        );
        add_plane(&mut client, &mut state, &mut dispatcher, params.object_id());
        let format = drm::buffer::DrmFourcc::Argb8888 as u32;
        dispatch(
            &mut client,
            &mut state,
            &mut dispatcher,
            params.object_id(),
            3,
            &[7, 1, 1, format, 0],
            VecDeque::new(),
        );
        assert_eq!(params_opcodes(&dispatcher), vec![1]);
        assert!(!closed(&dispatcher));
        let ids: Vec<_> = state.objects.iter().map(|(id, _)| id).collect();
        let rejected = ids.into_iter().any(|id| {
            state
                .objects
                .try_ref_object::<WlBuffer>(id)
                .is_some_and(|buffer| buffer.content().import_rejected())
        });
        assert!(rejected);
    }
}
