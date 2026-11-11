use std::rc::Rc;

use crate::{
    clients::{Client, ClientManagerAction, RawEvent},
    color::{ImageDescription, RenderIntent, TransferFunction},
    drm::ARGB8888,
    globals::{Global, Globals},
    io_uring::model::IoUringAction,
    object::{IdToObject, Object, RequestResult},
    protocol_generated::{
        wp_color_management_surface_feedback_v1, wp_color_management_surface_v1,
        wp_color_manager_v1, wp_image_description_creator_params_v1, wp_image_description_v1,
        Constants, NewId, WaylandId, WlBufferId, WlOutputId, WlSubsurfaceId, WlSurfaceId,
        WpColorManagementOutputV1Id, WpColorManagementSurfaceFeedbackV1Id,
        WpColorManagementSurfaceV1Id, WpColorManagerV1Id, WpImageDescriptionCreatorIccV1Id,
        WpImageDescriptionCreatorParamsV1Id, WpImageDescriptionInfoV1Id,
        WpImageDescriptionReferenceV1Id, WpImageDescriptionV1Id,
    },
    runner::Dispatcher,
    test_support::{
        state_for_tests, test_client_and_dispatcher, test_output_drm1, test_output_drm1_crtc,
    },
    wayland_objects::{
        color_management::{
            wp_color_manager_v1::WpColorManagerV1Global,
            wp_image_description_reference_v1::WpImageDescriptionReferenceV1,
            wp_image_description_v1::WpImageDescriptionV1,
        },
        wl_buffer::{NewWlBufferParams, WlBuffer},
        wl_output::WlOutput,
        wl_shm_pool::shared_memory_from_bytes,
        wl_subsurface::WlSubsurface,
        wl_surface::WlSurface,
    },
};

use super::wp_color_manager_v1::WpColorManagerV1;

struct Wire {
    object: u32,
    opcode: u16,
    args: Vec<u32>,
}

struct Session {
    client: Client,
    state: crate::state::State,
    dispatcher: Dispatcher,
}

impl Session {
    fn new() -> Self {
        let state = state_for_tests();
        let (client, dispatcher) = test_client_and_dispatcher(1);
        Self { client, state, dispatcher }
    }

    fn bind(&mut self, id: u32, version: u32) {
        let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
        WpColorManagerV1Global.bind(
            &mut self.client,
            &mut self.state,
            NewId::from(id),
            version,
            &mut client_dispatcher,
        );
    }

    fn dispatch<K>(&mut self, id: K, opcode: u16, args: &[u32]) -> RequestResult
    where
        K: IdToObject + Copy + Into<WaylandId>,
        K::Object: Object,
    {
        let wire: WaylandId = id.into();
        let mut client_dispatcher = self.client.test_dispatcher(&mut self.dispatcher);
        client_dispatcher.set_requester_object_id_for_tests(wire);
        let object = self.client.grab_object(id, &mut self.state);
        object.on(
            &mut self.client,
            &mut self.state,
            request(wire, opcode, args),
            &mut client_dispatcher,
        )
    }

    fn events(&self) -> Vec<Wire> {
        let mut events = Vec::new();
        self.dispatcher
            .test_for_each_queued_action::<IoUringAction>(|action| {
                let IoUringAction::Sendmsg { buffer, .. } = action else {
                    return;
                };
                let mut offset = 0;
                while offset + 8 <= buffer.len() {
                    let object = u32::from_ne_bytes(buffer[offset..offset + 4].try_into().unwrap());
                    let header =
                        u32::from_ne_bytes(buffer[offset + 4..offset + 8].try_into().unwrap());
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

    fn errors(&self) -> Vec<(u32, u32)> {
        self.events()
            .into_iter()
            .filter(|event| event.object == 1 && event.opcode == 0 && event.args.len() >= 2)
            .map(|event| (event.args[0], event.args[1]))
            .collect()
    }

    fn identity(&self, object: u32) -> u64 {
        self.events()
            .into_iter()
            .rev()
            .find_map(|event| {
                if event.object != object {
                    return None;
                }
                match event.opcode {
                    1 => Some(u64::from(event.args[0])),
                    2 => Some((u64::from(event.args[0]) << 32) | u64::from(event.args[1])),
                    _ => None,
                }
            })
            .expect("image description sent ready")
    }

    fn opcodes(&self, object: u32) -> Vec<u16> {
        self.events()
            .into_iter()
            .filter(|event| event.object == object)
            .map(|event| event.opcode)
            .collect()
    }

    fn closed(&self) -> bool {
        self.dispatcher.test_has_queued_action::<crate::clients::ClientManagerAction>(|action| {
            matches!(action, crate::clients::ClientManagerAction::Close { id } if *id == self.client.client_id())
        })
    }

    fn description(&self, id: WpImageDescriptionV1Id) -> Rc<ImageDescription> {
        Rc::clone(
            self.client
                .resolve(id, &self.state)
                .unwrap()
                .rref(&self.state)
                .description()
                .expect("ready image description"),
        )
    }
}

fn request(object: WaylandId, opcode: u16, args: &[u32]) -> crate::clients::RawRequest<'static> {
    let mut event = RawEvent::new(object, opcode, args.len());
    for arg in args {
        event.uint(*arg);
    }
    (&*event.into_bytes()).try_into().unwrap()
}

fn codes(session: &Session, object: u32, opcode: u16) -> Vec<u32> {
    session
        .events()
        .into_iter()
        .filter(|event| event.object == object && event.opcode == opcode)
        .map(|event| event.args[0])
        .collect()
}

fn parametric(
    session: &mut Session,
    manager: WpColorManagerV1Id,
    image: WpImageDescriptionV1Id,
    tf: u32,
    primaries: u32,
) {
    let image_id = u32::from(image);
    let creator = WpImageDescriptionCreatorParamsV1Id::from(image_id + 1);
    session.dispatch(manager, 5, &[u32::from(creator)]).unwrap();
    session.dispatch(creator, 1, &[tf]).unwrap();
    session.dispatch(creator, 3, &[primaries]).unwrap();
    session.dispatch(creator, 0, &[image_id]).unwrap();
}

const SRGB_TF: u32 = 9;
const COMPOUND_TF: u32 = 14;
const GAMMA22: u32 = 2;
const SRGB_PRIMARIES: u32 = 1;

#[test]
fn the_global_is_advertised_at_version_three_and_can_be_removed() {
    let mut globals = Globals::new();
    globals.initialize();
    assert!(globals.contains_interface(WpColorManagerV1::INTERFACE_NAME));
    assert_eq!(WpColorManagerV1Global.interface(), ("wp_color_manager_v1", 3));
    assert!(globals.remove_interface("wp_color_manager_v1"));
    assert!(!globals.contains_interface(WpColorManagerV1::INTERFACE_NAME));
    assert!(!globals.remove_interface("wp_color_manager_v1"));
}

#[test]
fn bind_advertises_version_three_capabilities() {
    let mut session = Session::new();
    session.bind(8, 3);
    let manager = 8;
    assert_eq!(codes(&session, manager, 0), vec![0, 1, 4, 5]);
    assert_eq!(codes(&session, manager, 1), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(codes(&session, manager, 2), vec![1, 2, 3, 4, 5, 6, 7, 11, 12, 14]);
    assert_eq!(codes(&session, manager, 3), (1..=10).collect::<Vec<_>>());
    assert_eq!(*session.opcodes(manager).last().unwrap(), 4);
    assert!(!codes(&session, manager, 2).contains(&8));
    assert!(!codes(&session, manager, 2).contains(&13));
}

#[test]
fn bind_version_one_advertises_srgb_names_and_not_bt2100() {
    let mut session = Session::new();
    session.bind(8, 1);
    assert_eq!(codes(&session, 8, 0), vec![0, 1, 4]);
    assert_eq!(codes(&session, 8, 1), vec![1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(codes(&session, 8, 2), vec![1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12]);
    assert_eq!(*session.opcodes(8).last().unwrap(), 4);
}

#[test]
fn icc_creator_is_unsupported_and_creates_nothing() {
    let mut session = Session::new();
    session.bind(8, 3);
    let manager = WpColorManagerV1Id::from(8);
    let before = session.errors().len();
    session.dispatch(manager, 4, &[23]).unwrap();
    assert_eq!(session.errors().len(), before + 1);
    assert_eq!(
        session.errors().last().copied(),
        Some((8, wp_color_manager_v1::Error::UnsupportedFeature as u32))
    );
    assert!(!session
        .client
        .contains_object(WpImageDescriptionCreatorIccV1Id::from(23), &session.state));
    assert!(!session.closed());
}

#[test]
fn requests_from_a_lower_bind_version_disconnect() {
    let mut session = Session::new();
    session.bind(8, 1);
    let manager = WpColorManagerV1Id::from(8);
    session.dispatch(manager, 7, &[20, 21]).unwrap();
    assert!(session.closed());
    assert_eq!(
        session.errors().last().copied(),
        Some((8, crate::protocol_generated::wl_display::Error::InvalidMethod as u32))
    );
    assert!(!session
        .client
        .contains_object(WpImageDescriptionV1Id::from(20), &session.state));

    let mut session = Session::new();
    session.bind(8, 2);
    session
        .dispatch(WpColorManagerV1Id::from(8), 8, &[20])
        .unwrap();
    assert!(session.closed());
    assert!(!session
        .client
        .contains_object(WpImageDescriptionV1Id::from(20), &session.state));
}

#[test]
fn equal_parameters_and_named_power_curves_share_an_identity() {
    let mut session = Session::new();
    session.bind(8, 3);
    let manager = WpColorManagerV1Id::from(8);
    parametric(&mut session, manager, WpImageDescriptionV1Id::from(20), GAMMA22, SRGB_PRIMARIES);
    parametric(&mut session, manager, WpImageDescriptionV1Id::from(22), GAMMA22, SRGB_PRIMARIES);
    let gamma = session.identity(20);
    assert_eq!(gamma, session.identity(22));
    assert_eq!(gamma, session.state.color_manager.srgb_gamma22().id());
    assert_eq!(session.opcodes(20), vec![2]);
    assert_eq!(session.opcodes(22), vec![2]);

    let power = WpImageDescriptionV1Id::from(30);
    let creator = WpImageDescriptionCreatorParamsV1Id::from(31);
    session.dispatch(manager, 5, &[u32::from(creator)]).unwrap();
    session.dispatch(creator, 2, &[22_000]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 0, &[u32::from(power)]).unwrap();
    assert_eq!(session.identity(30), gamma);

    let linear = WpImageDescriptionV1Id::from(40);
    let creator = WpImageDescriptionCreatorParamsV1Id::from(41);
    session.dispatch(manager, 5, &[u32::from(creator)]).unwrap();
    session.dispatch(creator, 2, &[10_000]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 0, &[u32::from(linear)]).unwrap();
    let named_linear = WpImageDescriptionV1Id::from(42);
    parametric(&mut session, manager, named_linear, 5, SRGB_PRIMARIES);
    assert_eq!(session.identity(40), session.identity(42));
    assert_eq!(session.description(linear).transfer_function(), TransferFunction::ExtLinear);

    let gamma28 = WpImageDescriptionV1Id::from(50);
    let creator = WpImageDescriptionCreatorParamsV1Id::from(51);
    session.dispatch(manager, 5, &[u32::from(creator)]).unwrap();
    session.dispatch(creator, 2, &[28_000]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 0, &[u32::from(gamma28)]).unwrap();
    let named_gamma28 = WpImageDescriptionV1Id::from(52);
    parametric(&mut session, manager, named_gamma28, 3, SRGB_PRIMARIES);
    assert_eq!(session.identity(50), session.identity(52));
}

#[test]
fn version_one_srgb_and_version_two_compound_power_intern_together() {
    let mut session = Session::new();
    session.bind(8, 1);
    session.bind(9, 3);
    parametric(
        &mut session,
        WpColorManagerV1Id::from(8),
        WpImageDescriptionV1Id::from(20),
        SRGB_TF,
        SRGB_PRIMARIES,
    );
    parametric(
        &mut session,
        WpColorManagerV1Id::from(9),
        WpImageDescriptionV1Id::from(22),
        COMPOUND_TF,
        SRGB_PRIMARIES,
    );
    assert_eq!(session.identity(20), session.identity(22));
    assert_eq!(session.opcodes(20), vec![1]);
    assert_eq!(session.opcodes(22), vec![2]);
    assert_eq!(
        session
            .description(WpImageDescriptionV1Id::from(20))
            .transfer_function(),
        TransferFunction::CompoundPower24
    );

    let creator = WpImageDescriptionCreatorParamsV1Id::from(30);
    session
        .dispatch(WpColorManagerV1Id::from(9), 5, &[u32::from(creator)])
        .unwrap();
    session.dispatch(creator, 1, &[SRGB_TF]).unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((30, wp_image_description_creator_params_v1::Error::InvalidTf as u32))
    );
    assert!(session.client.contains_object(creator, &session.state));

    let creator = WpImageDescriptionCreatorParamsV1Id::from(31);
    session
        .dispatch(WpColorManagerV1Id::from(8), 5, &[u32::from(creator)])
        .unwrap();
    session.dispatch(creator, 1, &[COMPOUND_TF]).unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((31, wp_image_description_creator_params_v1::Error::InvalidTf as u32))
    );
}

#[test]
fn windows_descriptions_are_ready_and_keep_their_identity() {
    let mut session = Session::new();
    session.bind(8, 3);
    let manager = WpColorManagerV1Id::from(8);
    session.dispatch(manager, 6, &[20]).unwrap();
    session.dispatch(manager, 6, &[21]).unwrap();
    assert_eq!(session.identity(20), session.identity(21));
    assert_eq!(session.identity(20), session.state.color_manager.windows_scrgb().id());
    session.dispatch(manager, 8, &[22]).unwrap();
    session.dispatch(manager, 8, &[23]).unwrap();
    assert_eq!(session.identity(22), session.identity(23));
    assert_eq!(session.identity(22), session.state.color_manager.windows_bt2100().id());
    assert_eq!(session.opcodes(20), vec![2]);
    assert_eq!(session.opcodes(22), vec![2]);
}

#[test]
fn a_second_set_is_already_set_and_an_invalid_luminance_does_not_stick() {
    let mut session = Session::new();
    session.bind(8, 3);
    let manager = WpColorManagerV1Id::from(8);
    let creator = WpImageDescriptionCreatorParamsV1Id::from(20);
    session.dispatch(manager, 5, &[20]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session.dispatch(creator, 1, &[3]).unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((20, wp_image_description_creator_params_v1::Error::AlreadySet as u32))
    );
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 3, &[6]).unwrap();
    session
        .dispatch(
            creator,
            4,
            &[640_000, 330_000, 300_000, 600_000, 150_000, 60_000, 312_700, 329_000],
        )
        .unwrap();
    session.dispatch(creator, 5, &[2_000, 80, 80]).unwrap();
    session.dispatch(creator, 5, &[1_000, 100, 100]).unwrap();
    session
        .dispatch(
            creator,
            6,
            &[640_000, 330_000, 300_000, 600_000, 150_000, 60_000, 312_700, 329_000],
        )
        .unwrap();
    session.dispatch(creator, 6, &[0; 8]).unwrap();
    session.dispatch(creator, 7, &[2_000, 80]).unwrap();
    session.dispatch(creator, 7, &[1_000, 90]).unwrap();
    session.dispatch(creator, 8, &[0]).unwrap();
    session.dispatch(creator, 8, &[0]).unwrap();
    session.dispatch(creator, 9, &[0]).unwrap();
    session.dispatch(creator, 9, &[40]).unwrap();
    assert!(session.errors().iter().all(|(_, code)| {
        *code == wp_image_description_creator_params_v1::Error::AlreadySet as u32
    }));
    assert!(session.client.contains_object(creator, &session.state));

    let fresh = WpImageDescriptionCreatorParamsV1Id::from(30);
    session.dispatch(manager, 5, &[30]).unwrap();
    let before = session.errors().len();
    session.dispatch(fresh, 5, &[20_000, 1, 80]).unwrap();
    assert_eq!(session.errors().len(), before + 1);
    assert_eq!(
        session.errors().last().copied(),
        Some((30, wp_image_description_creator_params_v1::Error::InvalidLuminance as u32))
    );
    session.dispatch(fresh, 5, &[2_000, 90, 80]).unwrap();
    assert_eq!(session.errors().len(), before + 1);
    session.dispatch(fresh, 1, &[GAMMA22]).unwrap();
    session.dispatch(fresh, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(fresh, 0, &[31]).unwrap();
    assert!(session
        .client
        .contains_object(WpImageDescriptionV1Id::from(31), &session.state));
    assert!(!session.client.contains_object(fresh, &session.state));
}

#[test]
fn create_checks_completeness_content_light_and_unusable_primaries() {
    let mut session = Session::new();
    session.bind(8, 1);
    let manager = WpColorManagerV1Id::from(8);
    let creator = WpImageDescriptionCreatorParamsV1Id::from(20);
    session.dispatch(manager, 5, &[20]).unwrap();
    session.dispatch(creator, 0, &[21]).unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((20, wp_image_description_creator_params_v1::Error::IncompleteSet as u32))
    );
    assert!(!session
        .client
        .contains_object(WpImageDescriptionV1Id::from(21), &session.state));
    assert!(session.client.contains_object(creator, &session.state));

    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 8, &[100]).unwrap();
    session.dispatch(creator, 0, &[21]).unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((20, wp_image_description_creator_params_v1::Error::InvalidLuminance as u32))
    );
    assert!(session.client.contains_object(creator, &session.state));

    let mut session = Session::new();
    session.bind(8, 2);
    let manager = WpColorManagerV1Id::from(8);
    let creator = WpImageDescriptionCreatorParamsV1Id::from(20);
    session.dispatch(manager, 5, &[20]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 8, &[100]).unwrap();
    session.dispatch(creator, 0, &[21]).unwrap();
    assert!(session.errors().is_empty());
    assert_eq!(session.opcodes(21), vec![2]);
    assert!(session
        .description(WpImageDescriptionV1Id::from(21))
        .max_cll()
        .is_some());

    let unset = WpImageDescriptionV1Id::from(30);
    parametric(&mut session, manager, unset, GAMMA22, SRGB_PRIMARIES);
    let zero = WpImageDescriptionV1Id::from(32);
    let creator = WpImageDescriptionCreatorParamsV1Id::from(33);
    session.dispatch(manager, 5, &[33]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 8, &[0]).unwrap();
    session.dispatch(creator, 0, &[32]).unwrap();
    assert_eq!(session.identity(30), session.identity(32));
    assert!(session.description(zero).max_cll().is_none());

    let creator = WpImageDescriptionCreatorParamsV1Id::from(40);
    session.dispatch(manager, 5, &[40]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 8, &[40]).unwrap();
    session.dispatch(creator, 9, &[90]).unwrap();
    session.dispatch(creator, 0, &[41]).unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((40, wp_image_description_creator_params_v1::Error::InvalidLuminance as u32))
    );
    assert!(!session
        .client
        .contains_object(WpImageDescriptionV1Id::from(41), &session.state));

    let creator = WpImageDescriptionCreatorParamsV1Id::from(50);
    session.dispatch(manager, 5, &[50]).unwrap();
    session.dispatch(creator, 1, &[11]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 5, &[0, 100, 203]).unwrap();
    session.dispatch(creator, 0, &[51]).unwrap();
    let creator = WpImageDescriptionCreatorParamsV1Id::from(52);
    session.dispatch(manager, 5, &[52]).unwrap();
    session.dispatch(creator, 1, &[11]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 5, &[0, 500, 203]).unwrap();
    session.dispatch(creator, 0, &[53]).unwrap();
    assert_eq!(session.identity(51), session.identity(53));

    let mut session = Session::new();
    session.bind(8, 1);
    let manager = WpColorManagerV1Id::from(8);
    let creator = WpImageDescriptionCreatorParamsV1Id::from(20);
    session.dispatch(manager, 5, &[20]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 7, &[2_000, 200]).unwrap();
    session.dispatch(creator, 8, &[150]).unwrap();
    session.dispatch(creator, 0, &[21]).unwrap();
    assert!(session.errors().is_empty());
    assert_eq!(session.opcodes(21), vec![1]);

    let creator = WpImageDescriptionCreatorParamsV1Id::from(30);
    session.dispatch(manager, 5, &[30]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session.dispatch(creator, 4, &[0; 8]).unwrap();
    session.dispatch(creator, 0, &[31]).unwrap();
    assert_eq!(
        session
            .events()
            .into_iter()
            .find(|event| event.object == 31 && event.opcode == 0)
            .map(|event| event.args[0]),
        Some(wp_image_description_v1::Cause::Unsupported as u32)
    );
    assert!(!session.client.contains_object(creator, &session.state));
    assert!(session
        .client
        .contains_object(WpImageDescriptionV1Id::from(31), &session.state));
    session
        .dispatch(WpImageDescriptionV1Id::from(31), 1, &[32])
        .unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((31, wp_image_description_v1::Error::NotReady as u32))
    );
    assert!(!session
        .client
        .contains_object(WpImageDescriptionInfoV1Id::from(32), &session.state));

    let creator = WpImageDescriptionCreatorParamsV1Id::from(40);
    session.dispatch(manager, 5, &[40]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session.dispatch(creator, 3, &[7]).unwrap();
    session.dispatch(creator, 0, &[41]).unwrap();
    assert_eq!(session.opcodes(41), vec![1]);
}

#[test]
fn get_information_follows_the_image_and_reports_compound_power_as_srgb_on_v1() {
    let mut session = Session::new();
    session.bind(8, 1);
    parametric(
        &mut session,
        WpColorManagerV1Id::from(8),
        WpImageDescriptionV1Id::from(20),
        SRGB_TF,
        SRGB_PRIMARIES,
    );
    session
        .dispatch(WpImageDescriptionV1Id::from(20), 1, &[21])
        .unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((20, wp_image_description_v1::Error::NoInformation as u32))
    );
    assert!(!session.closed());

    let description = session.description(WpImageDescriptionV1Id::from(20));
    session.client.insert_object(
        &mut session.state,
        WpImageDescriptionV1::ready(
            WpImageDescriptionV1Id::from(30),
            1,
            Rc::clone(&description),
            true,
        ),
    );
    session
        .client
        .resolve(WpImageDescriptionV1Id::from(30), &session.state)
        .unwrap()
        .ref_mut(&mut session.state)
        .publish(&mut session.client.test_dispatcher(&mut session.dispatcher));
    session
        .dispatch(WpImageDescriptionV1Id::from(30), 1, &[31])
        .unwrap();
    assert_eq!(session.opcodes(31), vec![2, 3, 5, 6, 7, 8, 0]);
    let tf = session
        .events()
        .into_iter()
        .find(|event| event.object == 31 && event.opcode == 5)
        .unwrap();
    assert_eq!(tf.args, vec![SRGB_TF]);
    assert!(!session
        .client
        .contains_object(WpImageDescriptionInfoV1Id::from(31), &session.state));

    session.client.insert_object(
        &mut session.state,
        WpImageDescriptionV1::ready(WpImageDescriptionV1Id::from(32), 2, description, true),
    );
    session
        .client
        .resolve(WpImageDescriptionV1Id::from(32), &session.state)
        .unwrap()
        .ref_mut(&mut session.state)
        .publish(&mut session.client.test_dispatcher(&mut session.dispatcher));
    session
        .dispatch(WpImageDescriptionV1Id::from(32), 1, &[33])
        .unwrap();
    let tf = session
        .events()
        .into_iter()
        .find(|event| event.object == 33 && event.opcode == 5)
        .unwrap();
    assert_eq!(tf.args, vec![COMPOUND_TF]);

    let reference = WpImageDescriptionReferenceV1Id::from(40);
    let srgb = session.state.color_manager.srgb_gamma22();
    session.client.insert_object(
        &mut session.state,
        WpImageDescriptionReferenceV1::new(reference, srgb, true),
    );
    session.bind(9, 2);
    session
        .dispatch(WpColorManagerV1Id::from(9), 7, &[41, u32::from(reference)])
        .unwrap();
    assert_eq!(session.identity(41), session.state.color_manager.srgb_gamma22().id());
    session
        .dispatch(WpImageDescriptionV1Id::from(41), 1, &[42])
        .unwrap();
    assert_eq!(session.opcodes(42), vec![2, 3, 5, 6, 7, 8, 0]);
    let luminances = session
        .events()
        .into_iter()
        .find(|event| event.object == 42 && event.opcode == 6)
        .unwrap();
    assert_eq!(luminances.args, vec![2_000, 80, 80]);
    let named = session
        .events()
        .into_iter()
        .find(|event| event.object == 42 && event.opcode == 3)
        .unwrap();
    assert_eq!(named.args, vec![SRGB_PRIMARIES]);
}

#[test]
fn destroying_the_manager_removes_only_that_object() {
    let mut session = Session::new();
    session.bind(8, 3);
    session
        .dispatch(WpColorManagerV1Id::from(8), 6, &[20])
        .unwrap();
    session
        .dispatch(WpColorManagerV1Id::from(8), 0, &[])
        .unwrap();
    assert!(!session
        .client
        .contains_object(WpColorManagerV1Id::from(8), &session.state));
    assert!(session
        .client
        .contains_object(WpImageDescriptionV1Id::from(20), &session.state));
    session
        .dispatch(WpImageDescriptionV1Id::from(20), 0, &[])
        .unwrap();
    assert!(!session
        .client
        .contains_object(WpImageDescriptionV1Id::from(20), &session.state));
}

fn insert_buffer(session: &mut Session, id: u32, size: i32) {
    let bytes = vec![0u8; (size * size * 4) as usize];
    session.client.insert_object(
        &mut session.state,
        WlBuffer::new(NewWlBufferParams {
            id: WlBufferId::from(id),
            offset: 0,
            width: size,
            height: size,
            stride: size.saturating_mul(4),
            format: &ARGB8888,
            mem: shared_memory_from_bytes(&bytes),
        })
        .unwrap(),
    );
}

fn map_surface(session: &mut Session, surface: WlSurfaceId, buffer: u32, size: i32) {
    insert_buffer(session, buffer, size);
    session.dispatch(surface, 1, &[buffer, 0, 0]).unwrap();
    session.dispatch(surface, 6, &[]).unwrap();
}

fn publish_description(session: &mut Session, id: u32, description: Rc<ImageDescription>) {
    let mut dispatcher = session.client.test_dispatcher(&mut session.dispatcher);
    let (_, image) = session.client.insert_object(
        &mut session.state,
        WpImageDescriptionV1::ready(WpImageDescriptionV1Id::from(id), 3, description, true),
    );
    image.publish(&mut dispatcher);
}

fn committed(session: &Session, surface: WlSurfaceId) -> Option<(u64, RenderIntent)> {
    session
        .client
        .resolve(surface, &session.state)
        .unwrap()
        .rref(&session.state)
        .committed_color()
}

#[test]
fn output_image_description_tracks_the_output_and_fails_when_it_is_gone() {
    let mut session = Session::new();
    session.bind(8, 3);
    let output_id = session.state.insert_output(test_output_drm1(0, 0, 64, 64));
    let expected = session
        .state
        .outputs
        .try_get(output_id)
        .unwrap()
        .color_description
        .id();
    session
        .client
        .insert_object(&mut session.state, WlOutput::new(WlOutputId::from(30), output_id, 4));
    session
        .dispatch(WpColorManagerV1Id::from(8), 1, &[31, 30])
        .unwrap();
    session
        .dispatch(WpColorManagementOutputV1Id::from(31), 1, &[32])
        .unwrap();
    assert_eq!(session.identity(32), expected);
    session
        .dispatch(WpImageDescriptionV1Id::from(32), 1, &[33])
        .unwrap();
    assert!(session.events().iter().any(|event| event.object == 33));
    assert!(session.errors().iter().all(|error| error.0 != 32));
    assert!(!session.closed());

    session
        .state
        .remove_output(output_id, &mut session.dispatcher);
    session
        .dispatch(WpColorManagementOutputV1Id::from(31), 1, &[34])
        .unwrap();
    let failed = session
        .events()
        .into_iter()
        .rev()
        .find(|event| event.object == 34);
    assert_eq!(failed.map(|event| (event.opcode, event.args[0])), Some((0, 3)));
}

#[test]
fn image_description_changed_is_followed_by_wl_output_done() {
    let mut session = Session::new();
    session.bind(8, 3);
    let output_id = session.state.insert_output(test_output_drm1(0, 0, 64, 64));
    session
        .client
        .insert_object(&mut session.state, WlOutput::new(WlOutputId::from(30), output_id, 4));
    session
        .dispatch(WpColorManagerV1Id::from(8), 1, &[31, 30])
        .unwrap();
    let replacement = session.state.color_manager.windows_scrgb();
    session
        .state
        .outputs
        .try_get_mut(output_id)
        .unwrap()
        .color_description = Rc::clone(&replacement);
    session.state.note_output_description_changed(output_id);
    crate::drm::request_output_color_redraws(&mut session.state, &mut session.dispatcher);
    assert!(session.dispatcher.test_has_queued_action::<ClientManagerAction>(|action| {
        matches!(
            action,
            ClientManagerAction::OutputColorDescriptionChanged { output_id: id } if *id == output_id
        )
    }));
    let mut dispatcher = session.client.test_dispatcher(&mut session.dispatcher);
    session
        .client
        .notify_output_color_description(&mut session.state, &mut dispatcher, output_id);
    drop(dispatcher);
    let events = session.events();
    assert!(events
        .iter()
        .any(|event| event.object == 31 && event.opcode == 0));
    assert!(events
        .iter()
        .any(|event| event.object == 30 && event.opcode == 2));
    session
        .dispatch(WpColorManagementOutputV1Id::from(31), 1, &[32])
        .unwrap();
    assert_eq!(session.identity(32), replacement.id());
}

#[test]
fn surface_color_is_double_buffered_and_damages_the_whole_surface() {
    let mut session = Session::new();
    session.bind(8, 3);
    let surface = WlSurfaceId::from(10);
    let (resolved, _) = session
        .client
        .insert_object(&mut session.state, WlSurface::new(surface, session.client.client_id(), 6));
    map_surface(&mut session, surface, 11, 4);
    resolved
        .grab(&mut session.state)
        .take_render_state_at(resolved, 0, 0);
    let description = session.state.color_manager.windows_scrgb();
    publish_description(&mut session, 12, Rc::clone(&description));
    session
        .dispatch(WpColorManagerV1Id::from(8), 2, &[13, 10])
        .unwrap();
    session
        .dispatch(WpColorManagementSurfaceV1Id::from(13), 1, &[12, 1])
        .unwrap();
    assert_eq!(committed(&session, surface), None);
    let before = resolved.rref(&session.state).render_state().unwrap();
    assert_eq!(before.render_intent, RenderIntent::Perceptual);
    assert!(before.damage.rects().is_empty());

    session
        .dispatch(WpImageDescriptionV1Id::from(12), 0, &[])
        .unwrap();
    session.dispatch(surface, 6, &[]).unwrap();
    assert_eq!(committed(&session, surface), Some((description.id(), RenderIntent::Relative)));
    let after = resolved.rref(&session.state).render_state().unwrap();
    assert_eq!(after.color_description.id(), description.id());
    assert_eq!(after.render_intent, RenderIntent::Relative);
    assert_eq!(after.damage.rects(), &[crate::region::RectI::new(0, 0, 4, 4).unwrap()]);
}

#[test]
fn unset_waits_for_commit_and_a_missing_color_does_not_clobber_the_cache() {
    let mut session = Session::new();
    session.bind(8, 3);
    let surface = WlSurfaceId::from(10);
    session
        .client
        .insert_object(&mut session.state, WlSurface::new(surface, session.client.client_id(), 6));
    let description = session.state.color_manager.windows_scrgb();
    publish_description(&mut session, 12, Rc::clone(&description));
    session
        .dispatch(WpColorManagerV1Id::from(8), 2, &[13, 10])
        .unwrap();
    session
        .dispatch(WpColorManagementSurfaceV1Id::from(13), 1, &[12, 0])
        .unwrap();
    session.dispatch(surface, 6, &[]).unwrap();
    session
        .dispatch(WpColorManagementSurfaceV1Id::from(13), 2, &[])
        .unwrap();
    assert_eq!(committed(&session, surface), Some((description.id(), RenderIntent::Perceptual)));
    session.dispatch(surface, 6, &[]).unwrap();
    assert_eq!(committed(&session, surface), None);
}

#[test]
fn a_synchronized_subsurface_applies_color_when_the_parent_commits() {
    let mut session = Session::new();
    session.bind(8, 3);
    let parent = WlSurfaceId::from(10);
    let child = WlSurfaceId::from(11);
    let (parent_resolved, _) = session
        .client
        .insert_object(&mut session.state, WlSurface::new(parent, session.client.client_id(), 6));
    session.state.register_root_surface(parent_resolved);
    let (child_resolved, _) = session
        .client
        .insert_object(&mut session.state, WlSurface::new(child, session.client.client_id(), 6));
    let (subsurface, _) = session.client.insert_object(
        &mut session.state,
        WlSubsurface::new(WlSubsurfaceId::from(14), child_resolved, parent_resolved),
    );
    child_resolved
        .grab(&mut session.state)
        .assign_subsurface(subsurface, parent_resolved);
    parent_resolved
        .grab(&mut session.state)
        .add_pending_child(child_resolved);

    map_surface(&mut session, child, 15, 4);
    session.dispatch(parent, 6, &[]).unwrap();
    child_resolved
        .grab(&mut session.state)
        .take_render_state_at(child_resolved, 0, 0);

    let description = session.state.color_manager.windows_scrgb();
    publish_description(&mut session, 16, Rc::clone(&description));
    session
        .dispatch(WpColorManagerV1Id::from(8), 2, &[17, 11])
        .unwrap();
    session
        .dispatch(WpColorManagementSurfaceV1Id::from(17), 1, &[16, 4])
        .unwrap();
    session.dispatch(child, 6, &[]).unwrap();
    assert_eq!(committed(&session, child), None);
    assert!(child_resolved
        .rref(&session.state)
        .render_state()
        .unwrap()
        .damage
        .rects()
        .is_empty());

    session.dispatch(parent, 6, &[]).unwrap();
    assert_eq!(committed(&session, child), Some((description.id(), RenderIntent::RelativeBpc)));
    assert_eq!(
        child_resolved
            .rref(&session.state)
            .render_state()
            .unwrap()
            .damage
            .rects(),
        &[crate::region::RectI::new(0, 0, 4, 4).unwrap()]
    );
}

#[test]
fn surface_protocol_errors_do_not_disconnect() {
    let mut session = Session::new();
    session.bind(8, 3);
    let surface = WlSurfaceId::from(10);
    let resolved = session
        .client
        .insert_object(&mut session.state, WlSurface::new(surface, session.client.client_id(), 6))
        .0;
    session
        .dispatch(WpColorManagerV1Id::from(8), 2, &[13, 10])
        .unwrap();
    session
        .dispatch(WpColorManagerV1Id::from(8), 2, &[14, 10])
        .unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((8, wp_color_manager_v1::Error::SurfaceExists as u32))
    );
    assert!(!session
        .client
        .contains_object(WpColorManagementSurfaceV1Id::from(14), &session.state));
    assert!(!session.closed());

    let description = session.state.color_manager.windows_scrgb();
    publish_description(&mut session, 12, description);
    session
        .dispatch(WpColorManagementSurfaceV1Id::from(13), 1, &[12, 2])
        .unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((13, wp_color_management_surface_v1::Error::RenderIntent as u32))
    );

    let mut dispatcher = session.client.test_dispatcher(&mut session.dispatcher);
    let (_, image) = session.client.insert_object(
        &mut session.state,
        WpImageDescriptionV1::failed(
            WpImageDescriptionV1Id::from(18),
            3,
            wp_image_description_v1::Cause::Unsupported as u32,
            "unsupported",
        ),
    );
    image.publish(&mut dispatcher);
    drop(dispatcher);
    session
        .dispatch(WpColorManagementSurfaceV1Id::from(13), 1, &[18, 0])
        .unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((13, wp_color_management_surface_v1::Error::ImageDescription as u32))
    );
    assert_eq!(committed(&session, surface), None);

    {
        let mut surface_object = session.client.grab_object(surface, &mut session.state);
        let mut dispatcher = session.client.test_dispatcher(&mut session.dispatcher);
        surface_object.delete_self(&mut session.client, &mut session.state, &mut dispatcher);
    }
    assert!(!session.client.contains_object(surface, &session.state));
    session
        .dispatch(WpColorManagementSurfaceV1Id::from(13), 1, &[12, 0])
        .unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((13, wp_color_management_surface_v1::Error::Inert as u32))
    );
    session
        .dispatch(WpColorManagementSurfaceV1Id::from(13), 0, &[])
        .unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((13, wp_color_management_surface_v1::Error::Inert as u32))
    );
    assert!(session
        .client
        .contains_object(WpColorManagementSurfaceV1Id::from(13), &session.state));
    assert!(!session.closed());
    let _ = resolved;
}

#[test]
fn preferred_feedback_follows_the_primary_output_without_an_initial_event() {
    let mut session = Session::new();
    session.bind(8, 3);
    let surface = WlSurfaceId::from(10);
    let (resolved, _) = session
        .client
        .insert_object(&mut session.state, WlSurface::new(surface, session.client.client_id(), 6));
    let first = session.state.insert_output(test_output_drm1(0, 0, 64, 64));
    let second = session
        .state
        .insert_output(test_output_drm1_crtc(2, 64, 0, 64, 64));
    let second_description = session.state.color_manager.windows_scrgb();
    session
        .state
        .outputs
        .try_get_mut(second)
        .unwrap()
        .color_description = Rc::clone(&second_description);
    session
        .dispatch(WpColorManagerV1Id::from(8), 3, &[20, 10])
        .unwrap();
    session
        .dispatch(WpColorManagerV1Id::from(8), 3, &[21, 10])
        .unwrap();
    assert!(session
        .events()
        .iter()
        .all(|event| event.object != 20 && event.object != 21));

    session
        .dispatch(WpColorManagementSurfaceFeedbackV1Id::from(20), 1, &[22])
        .unwrap();
    session
        .dispatch(WpColorManagementSurfaceFeedbackV1Id::from(20), 2, &[23])
        .unwrap();
    let fallback = session.state.color_manager.srgb_gamma22().id();
    assert_eq!(session.identity(22), fallback);
    assert_eq!(session.identity(23), fallback);
    session
        .dispatch(WpImageDescriptionV1Id::from(22), 1, &[24])
        .unwrap();
    assert!(session.events().iter().any(|event| event.object == 24));
    assert!(session.errors().iter().all(|error| error.0 != 22));

    {
        let mut surface = resolved.grab(&mut session.state);
        surface.update_output_membership(vec![second]);
        let mut dispatcher = session.client.test_dispatcher(&mut session.dispatcher);
        surface.send_preferred_color_description(&session.state, &mut dispatcher);
    }
    let changed: Vec<_> = session
        .events()
        .into_iter()
        .filter(|event| event.opcode == 1 && (event.object == 20 || event.object == 21))
        .map(|event| (event.object, event.args[0], event.args[1]))
        .collect();
    let lo = second_description.id() as u32;
    assert_eq!(changed, vec![(20, 0, lo), (21, 0, lo)]);

    let third = session.state.color_manager.windows_bt2100();
    session
        .state
        .outputs
        .try_get_mut(second)
        .unwrap()
        .color_description = Rc::clone(&third);
    let mut dispatcher = session.client.test_dispatcher(&mut session.dispatcher);
    session
        .client
        .notify_output_color_description(&mut session.state, &mut dispatcher, second);
    drop(dispatcher);
    let again: Vec<_> = session
        .events()
        .into_iter()
        .filter(|event| event.opcode == 1 && event.object == 20 && event.args.len() == 2)
        .map(|event| event.args[1])
        .collect();
    assert_eq!(again.last().copied(), Some(third.id() as u32));

    resolved
        .grab(&mut session.state)
        .update_output_membership(vec![first]);
    let mut dispatcher = session.client.test_dispatcher(&mut session.dispatcher);
    resolved
        .grab(&mut session.state)
        .send_preferred_color_description(&session.state, &mut dispatcher);
    drop(dispatcher);
    let back = session
        .events()
        .into_iter()
        .filter(|event| event.opcode == 1 && event.object == 20)
        .map(|event| event.args[1])
        .last();
    assert_eq!(
        back,
        Some(
            session
                .state
                .outputs
                .try_get(first)
                .unwrap()
                .color_description
                .id() as u32
        )
    );
}

#[test]
fn a_version_one_feedback_uses_preferred_changed() {
    let mut session = Session::new();
    session.bind(8, 1);
    let surface = WlSurfaceId::from(10);
    let (resolved, _) = session
        .client
        .insert_object(&mut session.state, WlSurface::new(surface, session.client.client_id(), 6));
    let output_id = session.state.insert_output(test_output_drm1(0, 0, 32, 32));
    let description = session.state.color_manager.windows_scrgb();
    session
        .state
        .outputs
        .try_get_mut(output_id)
        .unwrap()
        .color_description = Rc::clone(&description);
    session
        .dispatch(WpColorManagerV1Id::from(8), 3, &[20, 10])
        .unwrap();
    resolved
        .grab(&mut session.state)
        .update_output_membership(vec![output_id]);
    let mut dispatcher = session.client.test_dispatcher(&mut session.dispatcher);
    resolved
        .grab(&mut session.state)
        .send_preferred_color_description(&session.state, &mut dispatcher);
    drop(dispatcher);
    let event = session
        .events()
        .into_iter()
        .rev()
        .find(|event| event.object == 20)
        .unwrap();
    assert_eq!(event.opcode, 0);
    assert_eq!(event.args, vec![description.id() as u32]);
}

#[test]
fn feedback_is_inert_after_the_surface_is_destroyed() {
    let mut session = Session::new();
    session.bind(8, 3);
    let surface = WlSurfaceId::from(10);
    session
        .client
        .insert_object(&mut session.state, WlSurface::new(surface, session.client.client_id(), 6));
    session
        .dispatch(WpColorManagerV1Id::from(8), 3, &[20, 10])
        .unwrap();
    {
        let mut surface_object = session.client.grab_object(surface, &mut session.state);
        let mut dispatcher = session.client.test_dispatcher(&mut session.dispatcher);
        surface_object.delete_self(&mut session.client, &mut session.state, &mut dispatcher);
    }
    session
        .dispatch(WpColorManagementSurfaceFeedbackV1Id::from(20), 1, &[21])
        .unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((20, wp_color_management_surface_feedback_v1::Error::Inert as u32))
    );
    assert!(!session
        .client
        .contains_object(WpImageDescriptionV1Id::from(21), &session.state));
    assert!(!session.closed());
}

#[test]
fn parametric_creator_rejects_extreme_primaries_and_luminances() {
    let mut session = Session::new();
    session.bind(8, 1);
    let manager = WpColorManagerV1Id::from(8);
    let creator = WpImageDescriptionCreatorParamsV1Id::from(20);
    session.dispatch(manager, 5, &[20]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    for luminances in
        [[0, 0, 0], [2_000, 0, 80], [800_000, 80, 80], [2_000, 80, 0], [1_000_000, 50, 200]]
    {
        let before = session.errors().len();
        session.dispatch(creator, 5, &luminances).unwrap();
        assert_eq!(session.errors().len(), before + 1);
        assert_eq!(
            session.errors().last().copied(),
            Some((20, wp_image_description_creator_params_v1::Error::InvalidLuminance as u32))
        );
        assert!(!session.closed());
    }
    session.dispatch(creator, 7, &[50_000, 5]).unwrap();
    assert_eq!(
        session.errors().last().copied(),
        Some((20, wp_image_description_creator_params_v1::Error::InvalidLuminance as u32))
    );
    session.dispatch(creator, 5, &[2_000, 80, 80]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 0, &[21]).unwrap();
    assert_eq!(session.opcodes(21), vec![1]);
    assert!(!session.closed());

    let bad_primaries: &[&[u32]] = &[
        &[0; 8],
        &[640_000, 0, 300_000, 600_000, 150_000, 60_000, 312_700, 329_000],
        &[640_000, 330_000, 640_000, 330_000, 150_000, 60_000, 312_700, 329_000],
        &[i32::MIN as u32, i32::MIN as u32, 0, 0, 0, 0, 0, 0],
        &[i32::MAX as u32, i32::MAX as u32, 1, 1, 2, 2, 3, 3],
        &[200_000, 200_000, 200_000, 200_000, 200_000, 200_000, 200_000, 200_000],
    ];
    let mut next_id = 40u32;
    for primaries in bad_primaries {
        let creator_id = next_id;
        let image_id = next_id + 1;
        next_id += 2;
        let creator = WpImageDescriptionCreatorParamsV1Id::from(creator_id);
        session.dispatch(manager, 5, &[creator_id]).unwrap();
        session.dispatch(creator, 1, &[GAMMA22]).unwrap();
        session.dispatch(creator, 4, primaries).unwrap();
        let errors_before = session.errors().len();
        session.dispatch(creator, 0, &[image_id]).unwrap();
        assert_eq!(session.errors().len(), errors_before, "{primaries:?}");
        assert_eq!(session.opcodes(image_id), vec![0], "{primaries:?}");
        let cause = session
            .events()
            .into_iter()
            .find(|event| event.object == image_id && event.opcode == 0)
            .map(|event| event.args[0]);
        assert_eq!(cause, Some(wp_image_description_v1::Cause::Unsupported as u32));
        assert!(!session.client.contains_object(creator, &session.state));
        assert!(!session.closed());
    }

    let creator = WpImageDescriptionCreatorParamsV1Id::from(70);
    session.dispatch(manager, 5, &[70]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session
        .dispatch(
            creator,
            4,
            &[100_000, 100_000, 100_001, 100_000, 100_000, 100_001, 312_700, 329_000],
        )
        .unwrap();
    session.dispatch(creator, 0, &[71]).unwrap();
    assert_eq!(session.opcodes(71), vec![1]);
    assert!(session
        .description(WpImageDescriptionV1Id::from(71))
        .primaries()
        .rgb_to_xyz()
        .iter()
        .flatten()
        .all(|value| value.is_finite()));

    let creator = WpImageDescriptionCreatorParamsV1Id::from(80);
    session.dispatch(manager, 5, &[80]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session
        .dispatch(
            creator,
            4,
            &[640_000, 330_000, 300_000, 600_000, 150_000, 60_000, 312_700, 329_000],
        )
        .unwrap();
    session.dispatch(creator, 0, &[81]).unwrap();
    assert_eq!(session.opcodes(81), vec![1]);

    let creator = WpImageDescriptionCreatorParamsV1Id::from(90);
    session.dispatch(manager, 5, &[90]).unwrap();
    session.dispatch(creator, 1, &[GAMMA22]).unwrap();
    session.dispatch(creator, 3, &[SRGB_PRIMARIES]).unwrap();
    session.dispatch(creator, 6, &[0; 8]).unwrap();
    session.dispatch(creator, 0, &[91]).unwrap();
    assert_eq!(session.opcodes(91), vec![0]);
    assert_eq!(
        session
            .events()
            .into_iter()
            .find(|event| event.object == 91 && event.opcode == 0)
            .map(|event| event.args[0]),
        Some(wp_image_description_v1::Cause::Unsupported as u32)
    );
    assert!(!session.closed());
}
