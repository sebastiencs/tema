//! `zwp_linux_dmabuf_feedback_v1`.
//!
//! The format table is built in [`crate::dmabuf_feedback`]. This object sends it.
//! A surface feedback is removed from the registry when its `wl_surface` dies;
//! the object stays allocated and sends nothing more.

use std::{mem::size_of, rc::Rc};

use crate::{
    clients::{Client, ClientDispatcher, ClientId, EventWriter, RawRequest, Resolved},
    dmabuf_feedback::{
        build, collect_devices, collect_scanout, DeviceTranche, DmabufFeedbackTable,
        FeedbackDevice, ScanoutFormats, ScanoutTranche,
    },
    object::{Object, RequestResult},
    protocol_generated::{
        zwp_linux_dmabuf_feedback_v1::{self, TrancheFlags},
        Array, WaylandId, WlSurfaceId, ZwpLinuxDmabufFeedbackV1Id,
    },
    runner::Dispatcher,
    state::{DrmId, GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

/// Why a feedback object exists. Phase 3 reorders surface feedback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedbackTarget {
    Default,
    Surface(Resolved<WlSurfaceId>),
}

#[derive(Clone, Copy, Debug)]
pub struct LiveDmabufFeedback {
    pub object: Resolved<ZwpLinuxDmabufFeedbackV1Id>,
    pub target: FeedbackTarget,
}

#[derive(Debug)]
pub struct ZwpLinuxDmabufFeedbackV1 {
    id: ZwpLinuxDmabufFeedbackV1Id,
    version: u32,
    target: FeedbackTarget,
    last_table_id: Option<u64>,
    /// Device order of the last round that was actually sent.
    last_order: Option<Vec<DrmId>>,
    /// Scanout tranches of the last round. `None` means nothing has been sent.
    last_scanout: Option<Vec<(libc::dev_t, Vec<u16>)>>,
}

impl ZwpLinuxDmabufFeedbackV1 {
    pub(crate) fn new(
        id: ZwpLinuxDmabufFeedbackV1Id,
        version: u32,
        target: FeedbackTarget,
    ) -> Self {
        Self {
            id,
            version,
            target,
            last_table_id: None,
            last_order: None,
            last_scanout: None,
        }
    }
}

/// Rebuild the shared table from the DRM devices and resend it when it changed.
pub fn refresh_dmabuf_feedback(state: &mut State, dispatcher: &mut Dispatcher) {
    let devices = collect_devices(state);
    let scanout = collect_scanout(state);
    publish_feedback(state, dispatcher, &devices, &scanout);
}

fn publish_dmabuf_feedback(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    devices: &[FeedbackDevice],
) {
    publish_feedback(state, dispatcher, devices, &[]);
}

/// `None` from [`build`] means the table is unchanged, so no client is told.
/// An empty device list still stores a table, so the next real table gets a new id.
fn publish_feedback(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    devices: &[FeedbackDevice],
    scanout: &[ScanoutFormats],
) {
    let next = match build(devices, scanout, state.dmabuf_feedback.as_deref()) {
        Ok(Some(table)) => table,
        Ok(None) => return,
        Err(error) => {
            log::error!("linux-dmabuf feedback update failed: {error}");
            return;
        }
    };
    let send = next.devices.iter().any(|device| !device.indices.is_empty());
    state.dmabuf_feedback = Some(Rc::new(next));
    if !send {
        log::debug!("linux-dmabuf feedback has no sampling tranche yet");
        return;
    }
    let pending: Vec<_> = state
        .dmabuf_feedbacks
        .iter()
        .map(|live| (live.object.client_id(), live.object.wayland_id()))
        .collect();
    for (client_id, id) in pending {
        dispatcher.dispatch_to(client_id, crate::clients::ClientAction::SendDmabufFeedback { id });
    }
}

pub(crate) fn register(
    state: &mut State,
    object: Resolved<ZwpLinuxDmabufFeedbackV1Id>,
    target: FeedbackTarget,
) {
    state
        .dmabuf_feedbacks
        .push(LiveDmabufFeedback { object, target });
}

pub(crate) fn unregister(state: &mut State, client_id: ClientId, id: ZwpLinuxDmabufFeedbackV1Id) {
    state
        .dmabuf_feedbacks
        .retain(|live| live.object.client_id() != client_id || live.object.wayland_id() != id);
}

/// Drop every feedback bound to `surface`. The protocol objects stay inert.
pub fn retire_surface(state: &mut State, surface: Resolved<WlSurfaceId>) {
    state
        .dmabuf_feedbacks
        .retain(|live| live.target != FeedbackTarget::Surface(surface));
}

/// Send one feedback round. `format_table` is skipped when this object already
/// sent `table`'s id. Tranche order follows `device_order`.
pub fn send_feedback(
    feedback: &mut ZwpLinuxDmabufFeedbackV1,
    table: &DmabufFeedbackTable,
    device_order: &[DrmId],
    scanout: &[ScanoutTranche],
    dispatcher: &mut ClientDispatcher,
) -> bool {
    let tranches: Vec<&DeviceTranche> = device_order
        .iter()
        .filter_map(|drm_id| {
            table
                .devices
                .iter()
                .find(|device| device.drm_id == *drm_id && !device.indices.is_empty())
        })
        .collect();
    if tranches.is_empty() {
        return false;
    }
    if feedback.last_table_id != Some(table.id) {
        feedback.send_event(
            zwp_linux_dmabuf_feedback_v1::Event::FormatTable {
                fd: Rc::clone(&table.memfd),
                size: table.size,
            },
            dispatcher,
        );
        feedback.last_table_id = Some(table.id);
    }
    if feedback.version < 6 {
        feedback.send_event(
            zwp_linux_dmabuf_feedback_v1::Event::MainDevice {
                device: dev_t_array(tranches[0].dev),
            },
            dispatcher,
        );
    }
    let sampling_flags = if feedback.version >= 6 { TrancheFlags::Sampling as u32 } else { 0 };
    let scanout_flags = if feedback.version >= 6 {
        TrancheFlags::Scanout as u32 | TrancheFlags::Sampling as u32
    } else {
        TrancheFlags::Scanout as u32
    };
    for tranche in scanout {
        feedback.send_tranche(tranche.dev, scanout_flags, &tranche.indices, dispatcher);
    }
    for tranche in tranches {
        feedback.send_tranche(tranche.dev, sampling_flags, &tranche.indices, dispatcher);
    }
    feedback.send_event(zwp_linux_dmabuf_feedback_v1::Event::Done, dispatcher);
    true
}

impl ZwpLinuxDmabufFeedbackV1 {
    fn send_tranche(
        &self,
        dev: libc::dev_t,
        flags: u32,
        indices: &[u16],
        dispatcher: &mut ClientDispatcher,
    ) {
        self.send_event(
            zwp_linux_dmabuf_feedback_v1::Event::TrancheTargetDevice { device: dev_t_array(dev) },
            dispatcher,
        );
        self.send_event(zwp_linux_dmabuf_feedback_v1::Event::TrancheFlags { flags }, dispatcher);
        self.send_event(
            zwp_linux_dmabuf_feedback_v1::Event::TrancheFormats {
                indices: Array::from_u16s(indices),
            },
            dispatcher,
        );
        self.send_event(zwp_linux_dmabuf_feedback_v1::Event::TrancheDone, dispatcher);
    }
}

pub(crate) fn send_current(
    feedback: &mut ZwpLinuxDmabufFeedbackV1,
    state: &State,
    dispatcher: &mut ClientDispatcher,
) {
    let Some(table) = state.dmabuf_feedback.clone() else {
        log::debug!("linux-dmabuf feedback waiting for a DRM device");
        return;
    };
    let order = device_order(state, &table, feedback.target);
    let scanout = scanout_tranches(state, &table, feedback.target);
    let scanout_key = scanout_key(&scanout);
    let same_table = feedback.last_table_id == Some(table.id);
    let same_order = feedback.last_order.as_deref() == Some(order.as_slice());
    let same_scanout = feedback.last_scanout.as_ref() == Some(&scanout_key);
    if same_table && same_order && same_scanout {
        return;
    }
    if send_feedback(feedback, &table, &order, &scanout, dispatcher) {
        feedback.last_order = Some(order);
        feedback.last_scanout = Some(scanout_key);
    }
}

/// Remember which surface could be scanned out on `output_id`, and resend the
/// old and new surfaces when that changes.
pub fn set_scanout_candidate(
    state: &mut State,
    dispatcher: &mut Dispatcher,
    output_id: crate::state::OutputId,
    surface: Option<Resolved<WlSurfaceId>>,
) {
    let previous = state.scanout_candidates.get(&output_id).copied();
    if previous == surface {
        return;
    }
    match surface {
        Some(surface) => {
            state.scanout_candidates.insert(output_id, surface);
        }
        None => {
            state.scanout_candidates.remove(&output_id);
        }
    }
    if let Some(previous) = previous {
        queue_surface_feedback(state, dispatcher, previous);
    }
    if let Some(surface) = surface {
        queue_surface_feedback(state, dispatcher, surface);
    }
}

fn queue_surface_feedback(
    state: &State,
    dispatcher: &mut Dispatcher,
    surface: Resolved<WlSurfaceId>,
) {
    let pending: Vec<_> = state
        .dmabuf_feedbacks
        .iter()
        .filter(|live| live.target == FeedbackTarget::Surface(surface))
        .map(|live| live.object)
        .collect();
    for object in pending {
        dispatcher.dispatch_to(
            object.client_id(),
            crate::clients::ClientAction::SendDmabufFeedback { id: object.wayland_id() },
        );
    }
}

fn scanout_tranches(
    state: &State,
    table: &DmabufFeedbackTable,
    target: FeedbackTarget,
) -> Vec<ScanoutTranche> {
    let FeedbackTarget::Surface(surface) = target else {
        return Vec::new();
    };
    table
        .scanout
        .iter()
        .filter(|tranche| state.scanout_candidates.get(&tranche.output_id) == Some(&surface))
        .filter(|tranche| !tranche.indices.is_empty())
        .cloned()
        .collect()
}

fn scanout_key(scanout: &[ScanoutTranche]) -> Vec<(libc::dev_t, Vec<u16>)> {
    scanout
        .iter()
        .map(|tranche| (tranche.dev, tranche.indices.clone()))
        .collect()
}

/// Resend surface feedback whose GPU order changed. Default feedback is left alone.
pub fn reorder_surface_feedback(state: &mut State, dispatcher: &mut Dispatcher) {
    let Some(table) = state.dmabuf_feedback.clone() else {
        return;
    };
    let pending: Vec<_> = state
        .dmabuf_feedbacks
        .iter()
        .filter_map(|live| match live.target {
            FeedbackTarget::Surface(_) => Some(live.object),
            FeedbackTarget::Default => None,
        })
        .collect();
    for object in pending {
        let (target, last_table_id, last_order) = {
            let Some(feedback) = object.try_ref(state) else {
                continue;
            };
            (feedback.target, feedback.last_table_id, feedback.last_order.clone())
        };
        let order = device_order(state, &table, target);
        if last_table_id == Some(table.id) && last_order.as_deref() == Some(order.as_slice()) {
            continue;
        }
        dispatcher.dispatch_to(
            object.client_id(),
            crate::clients::ClientAction::SendDmabufFeedback { id: object.wayland_id() },
        );
    }
}

/// One surface changed outputs. Send immediately on `dispatcher`'s client.
pub fn refresh_surface_feedback(
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    surface: Resolved<WlSurfaceId>,
) {
    let pending: Vec<_> = state
        .dmabuf_feedbacks
        .iter()
        .filter_map(|live| (live.target == FeedbackTarget::Surface(surface)).then_some(live.object))
        .collect();
    for object in pending {
        let Some(mut feedback) = object.try_grab(state) else {
            continue;
        };
        send_current(&mut feedback, state, dispatcher);
    }
}

/// Table order, except a surface feedback puts its preferred output's GPU first.
/// A preferred GPU that is not in the table keeps the table order.
fn device_order(state: &State, table: &DmabufFeedbackTable, target: FeedbackTarget) -> Vec<DrmId> {
    let default: Vec<DrmId> = table.devices.iter().map(|device| device.drm_id).collect();
    let FeedbackTarget::Surface(surface) = target else {
        return default;
    };
    let Some(surface_ref) = surface.try_ref(state) else {
        return default;
    };
    let Some(output_id) = surface_ref.preferred_output_id_for_object(state, surface) else {
        return default;
    };
    let Some(drm_id) = state
        .backend_for_output(output_id)
        .map(|backend| backend.drm_id)
    else {
        return default;
    };
    if !default.contains(&drm_id) {
        return default;
    }
    let mut order = Vec::with_capacity(default.len());
    order.push(drm_id);
    order.extend(default.iter().copied().filter(|id| *id != drm_id));
    order
}

fn dev_t_array(dev: libc::dev_t) -> Array {
    Array(dev.to_ne_bytes().to_vec())
}

impl Object for ZwpLinuxDmabufFeedbackV1 {
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
        match client.parse::<Self>(req, dispatcher)? {
            zwp_linux_dmabuf_feedback_v1::Request::Destroy => {
                unregister(state, client.client_id(), self.id);
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
        }
        Ok(())
    }

    fn on_client_disconnect(
        &mut self,
        client: &mut Client,
        state: &mut State,
        _dispatcher: &mut ClientDispatcher,
    ) {
        unregister(state, client.client_id(), self.id);
    }
}

const _: () = assert!(size_of::<libc::dev_t>() == 8);

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use {
        super::*,
        crate::{
            clients::{Arguments, Client, ClientAction, ClientId, RawRequest},
            dmabuf_feedback::{FeedbackDevice, ScanoutFormats},
            io_uring::model::IoUringAction,
            protocol_generated::ZwpLinuxDmabufV1Id,
            runner::{Dispatcher, Model},
            state::{DrmId, OutputId},
            test_support::state_for_tests,
            wayland_objects::{wl_surface::WlSurface, zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1},
        },
    };

    struct Sent {
        opcode: u16,
        words: Vec<u32>,
        fds: usize,
    }

    fn sent(dispatcher: &Dispatcher) -> Vec<Sent> {
        let mut events = Vec::new();
        dispatcher.test_for_each_queued_action(|action: &IoUringAction| {
            let IoUringAction::Sendmsg { buffer, fds, .. } = action else {
                return;
            };
            if buffer.len() < 8 {
                return;
            }
            let opcode = (u32::from_ne_bytes(buffer[4..8].try_into().unwrap()) & 0xffff) as u16;
            let words = buffer[8..]
                .chunks_exact(4)
                .map(|chunk| u32::from_ne_bytes(chunk.try_into().unwrap()))
                .collect();
            events.push(Sent { opcode, words, fds: fds.len() });
        });
        events
    }

    fn device(index: usize, dev: libc::dev_t, formats: Vec<(u32, u64)>) -> FeedbackDevice {
        FeedbackDevice { drm_id: DrmId::from(index), dev, formats }
    }

    fn request(object_id: u32, opcode: u16, words: &[u32]) -> RawRequest<'static> {
        RawRequest {
            object_id,
            request_id: opcode,
            arguments: Arguments::from_words(VecDeque::from(words.to_vec())),
            fds: None,
        }
    }

    fn dispatch_dmabuf(
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut Dispatcher,
        dmabuf: crate::state::ObjectId,
        opcode: u16,
        words: &[u32],
    ) {
        let mut client_dispatcher = client.test_dispatcher(dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(4.into());
        state
            .objects
            .grab(dmabuf)
            .on(client, state, request(4, opcode, words), &mut client_dispatcher)
            .unwrap();
    }

    fn feedback_opcodes(events: &[Sent]) -> Vec<u16> {
        events.iter().map(|event| event.opcode).collect()
    }

    #[test]
    fn version_6_default_feedback_lists_each_device_with_the_sampling_flag() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (resolved, _) =
            client.insert_object(&mut state, ZwpLinuxDmabufV1::new(ZwpLinuxDmabufV1Id::from(4), 6));
        publish_dmabuf_feedback(
            &mut state,
            &mut dispatcher,
            &[device(1, 10, vec![(2, 20), (1, 10)]), device(2, 11, vec![(1, 10), (3, 30)])],
        );
        dispatch_dmabuf(&mut client, &mut state, &mut dispatcher, resolved.object_id(), 2, &[5]);

        let events = sent(&dispatcher);
        assert_eq!(feedback_opcodes(&events), vec![1, 4, 6, 5, 3, 4, 6, 5, 3, 0]);
        assert_eq!(events[0].fds, 1);
        assert_eq!(events[0].words, vec![16 * 3]);
        assert_eq!(events[2].words, vec![TrancheFlags::Sampling as u32]);
        assert_eq!(events[6].words, vec![TrancheFlags::Sampling as u32]);
        assert!(events.iter().all(|event| event.opcode != 2));
        assert_eq!(events[1].words[0], 8);
        assert_eq!(&events[1].words[1..], &dev_t_array(10).to_u32s());
    }

    #[test]
    fn version_4_feedback_sends_main_device_and_zero_flags() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (resolved, _) =
            client.insert_object(&mut state, ZwpLinuxDmabufV1::new(ZwpLinuxDmabufV1Id::from(4), 4));
        publish_dmabuf_feedback(&mut state, &mut dispatcher, &[device(1, 10, vec![(1, 10)])]);
        dispatch_dmabuf(&mut client, &mut state, &mut dispatcher, resolved.object_id(), 2, &[5]);
        let events = sent(&dispatcher);
        assert_eq!(feedback_opcodes(&events), vec![1, 2, 4, 6, 5, 3, 0]);
        assert_eq!(events[3].words, vec![0]);
        assert_eq!(events[1].words[0], 8);
    }

    #[test]
    fn an_unchanged_refresh_sends_nothing_and_a_change_sends_the_table_again() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (resolved, _) =
            client.insert_object(&mut state, ZwpLinuxDmabufV1::new(ZwpLinuxDmabufV1Id::from(4), 6));
        let devices = [device(1, 10, vec![(1, 10)])];
        publish_dmabuf_feedback(&mut state, &mut dispatcher, &devices);
        dispatch_dmabuf(&mut client, &mut state, &mut dispatcher, resolved.object_id(), 2, &[5]);
        let before = sent(&dispatcher).len();
        publish_dmabuf_feedback(&mut state, &mut dispatcher, &devices);
        assert_eq!(sent(&dispatcher).len(), before);
        assert_eq!(
            dispatcher.test_count_queued_action::<ClientAction>(|action| {
                matches!(action, ClientAction::SendDmabufFeedback { .. })
            }),
            0
        );

        publish_dmabuf_feedback(&mut state, &mut dispatcher, &[device(1, 12, vec![(1, 10)])]);
        client
            .process(
                &mut state,
                ClientAction::SendDmabufFeedback { id: ZwpLinuxDmabufFeedbackV1Id::from(5) },
                &mut dispatcher,
            )
            .unwrap();
        let format_tables = sent(&dispatcher)
            .iter()
            .filter(|event| event.opcode == 1)
            .count();
        assert_eq!(format_tables, 2);
    }

    #[test]
    fn the_same_table_is_not_attached_twice() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (resolved, _) =
            client.insert_object(&mut state, ZwpLinuxDmabufV1::new(ZwpLinuxDmabufV1Id::from(4), 6));
        publish_dmabuf_feedback(&mut state, &mut dispatcher, &[device(1, 10, vec![(1, 10)])]);
        dispatch_dmabuf(&mut client, &mut state, &mut dispatcher, resolved.object_id(), 2, &[5]);
        let table = state.dmabuf_feedback.clone().unwrap();
        let order = vec![table.devices[0].drm_id];
        {
            let mut feedback = state.dmabuf_feedbacks[0]
                .object
                .try_grab(&mut state)
                .unwrap();
            send_feedback(
                &mut feedback,
                &table,
                &order,
                &[],
                &mut client.test_dispatcher(&mut dispatcher),
            );
        }
        assert_eq!(
            sent(&dispatcher)
                .iter()
                .filter(|event| event.opcode == 1)
                .count(),
            1
        );
        assert!(sent(&dispatcher).len() > 5);
    }

    #[test]
    fn feedback_for_a_destroyed_surface_stays_silent() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (dmabuf, _) =
            client.insert_object(&mut state, ZwpLinuxDmabufV1::new(ZwpLinuxDmabufV1Id::from(4), 6));
        let (surface, _) = client.insert_object(
            &mut state,
            WlSurface::new(crate::protocol_generated::WlSurfaceId::from(8), client.client_id(), 6),
        );
        publish_dmabuf_feedback(&mut state, &mut dispatcher, &[device(1, 10, vec![(1, 10)])]);
        dispatch_dmabuf(&mut client, &mut state, &mut dispatcher, dmabuf.object_id(), 3, &[5, 8]);
        assert!(!sent(&dispatcher).is_empty());
        retire_surface(&mut state, surface);
        let queued_before = dispatcher.test_count_queued_action::<ClientAction>(|_| true);
        publish_dmabuf_feedback(&mut state, &mut dispatcher, &[device(1, 99, vec![(1, 10)])]);
        assert_eq!(dispatcher.test_count_queued_action::<ClientAction>(|_| true), queued_before);
        client
            .process(
                &mut state,
                ClientAction::SendDmabufFeedback { id: ZwpLinuxDmabufFeedbackV1Id::from(5) },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(
            sent(&dispatcher)
                .iter()
                .filter(|event| event.opcode == 1)
                .count(),
            1
        );
    }

    fn target_devices(events: &[Sent]) -> Vec<libc::dev_t> {
        events
            .iter()
            .filter(|event| event.opcode == 4)
            .map(|event| {
                let mut bytes = [0u8; 8];
                bytes[..4].copy_from_slice(&event.words[1].to_ne_bytes());
                bytes[4..].copy_from_slice(&event.words[2].to_ne_bytes());
                libc::dev_t::from_ne_bytes(bytes)
            })
            .collect()
    }

    #[test]
    fn surface_feedback_puts_the_preferred_gpu_first_and_skips_a_same_gpu_move() {
        use crate::{state::BackendOutputRef, test_support::test_output_with_crtc_name};

        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let gpu_a = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(1)),
            1,
            0,
            0,
            100,
            100,
        ));
        let gpu_b = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(2)),
            2,
            100,
            0,
            100,
            100,
        ));
        let gpu_b_again = state.insert_output(test_output_with_crtc_name(
            BackendOutputRef::new(DrmId::from(2)),
            3,
            200,
            0,
            100,
            100,
        ));
        let (dmabuf, _) =
            client.insert_object(&mut state, ZwpLinuxDmabufV1::new(ZwpLinuxDmabufV1Id::from(4), 6));
        let (surface, _) = client.insert_object(
            &mut state,
            WlSurface::new(crate::protocol_generated::WlSurfaceId::from(8), client.client_id(), 6),
        );
        surface
            .ref_mut(&mut state)
            .update_output_membership(vec![gpu_a]);
        publish_dmabuf_feedback(
            &mut state,
            &mut dispatcher,
            &[device(1, 10, vec![(1, 10)]), device(2, 11, vec![(2, 20)])],
        );
        dispatch_dmabuf(&mut client, &mut state, &mut dispatcher, dmabuf.object_id(), 3, &[5, 8]);
        assert_eq!(target_devices(&sent(&dispatcher)), vec![10, 11]);

        surface
            .ref_mut(&mut state)
            .update_output_membership(vec![gpu_b]);
        let before = sent(&dispatcher).len();
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            refresh_surface_feedback(&mut state, &mut client_dispatcher, surface);
        }
        let moved = &sent(&dispatcher)[before..];
        assert!(moved.iter().all(|event| event.opcode != 1));
        assert_eq!(target_devices(moved), vec![11, 10]);

        surface
            .ref_mut(&mut state)
            .update_output_membership(vec![gpu_b_again]);
        let before = sent(&dispatcher).len();
        {
            let mut client_dispatcher = client.test_dispatcher(&mut dispatcher);
            refresh_surface_feedback(&mut state, &mut client_dispatcher, surface);
        }
        assert_eq!(sent(&dispatcher).len(), before);
    }

    fn flag_words(events: &[Sent]) -> Vec<u32> {
        events
            .iter()
            .filter(|event| event.opcode == 6)
            .map(|event| event.words[0])
            .collect()
    }

    #[test]
    fn a_scanout_candidate_hears_that_tranche_first_and_another_surface_does_not() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (dmabuf, _) =
            client.insert_object(&mut state, ZwpLinuxDmabufV1::new(ZwpLinuxDmabufV1Id::from(4), 6));
        let (surface, _) = client.insert_object(
            &mut state,
            WlSurface::new(crate::protocol_generated::WlSurfaceId::from(8), client.client_id(), 6),
        );
        let (_other, _) = client.insert_object(
            &mut state,
            WlSurface::new(crate::protocol_generated::WlSurfaceId::from(9), client.client_id(), 6),
        );
        let output_id = OutputId::from(3usize);
        publish_feedback(
            &mut state,
            &mut dispatcher,
            &[device(1, 10, vec![(1, 10), (1, 20)])],
            &[ScanoutFormats {
                output_id,
                drm_id: DrmId::from(1usize),
                dev: 10,
                formats: vec![(1, 20)],
            }],
        );
        dispatch_dmabuf(&mut client, &mut state, &mut dispatcher, dmabuf.object_id(), 3, &[5, 8]);
        dispatch_dmabuf(&mut client, &mut state, &mut dispatcher, dmabuf.object_id(), 3, &[6, 9]);
        assert_eq!(
            flag_words(&sent(&dispatcher)),
            vec![TrancheFlags::Sampling as u32, TrancheFlags::Sampling as u32,]
        );

        let before = sent(&dispatcher).len();
        set_scanout_candidate(&mut state, &mut dispatcher, output_id, Some(surface));
        client
            .process(
                &mut state,
                ClientAction::SendDmabufFeedback { id: ZwpLinuxDmabufFeedbackV1Id::from(5) },
                &mut dispatcher,
            )
            .unwrap();
        let added = &sent(&dispatcher)[before..];
        assert!(added.iter().all(|event| event.opcode != 1));
        assert_eq!(
            flag_words(added),
            vec![
                TrancheFlags::Scanout as u32 | TrancheFlags::Sampling as u32,
                TrancheFlags::Sampling as u32,
            ]
        );
        assert_eq!(
            flag_words(&sent(&dispatcher))
                .into_iter()
                .filter(
                    |flags| *flags == TrancheFlags::Scanout as u32 | TrancheFlags::Sampling as u32
                )
                .count(),
            1
        );

        let before = sent(&dispatcher).len();
        set_scanout_candidate(&mut state, &mut dispatcher, output_id, None);
        client
            .process(
                &mut state,
                ClientAction::SendDmabufFeedback { id: ZwpLinuxDmabufFeedbackV1Id::from(5) },
                &mut dispatcher,
            )
            .unwrap();
        assert_eq!(flag_words(&sent(&dispatcher)[before..]), vec![TrancheFlags::Sampling as u32]);
    }

    #[test]
    fn version_3_rejects_feedback_requests() {
        let mut client = Client::new_for_tests(ClientId::from_raw(1));
        let mut state = state_for_tests();
        let mut dispatcher = Dispatcher::default();
        let (resolved, _) =
            client.insert_object(&mut state, ZwpLinuxDmabufV1::new(ZwpLinuxDmabufV1Id::from(4), 3));
        dispatch_dmabuf(&mut client, &mut state, &mut dispatcher, resolved.object_id(), 2, &[5]);
        assert!(dispatcher.test_has_queued_action(
            |action: &crate::clients::ClientManagerAction| {
                matches!(action, crate::clients::ClientManagerAction::Close { .. })
            }
        ));
    }
}
