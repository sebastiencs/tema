use std::rc::Rc;

use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    globals::Global,
    object::{Object, RequestResult},
    protocol_generated::{
        wl_display, wp_color_manager_v1, Constants, NewId, WaylandId, WpColorManagerV1Id,
        WpImageDescriptionV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::{
        color_management::{
            wp_color_management_output_v1::WpColorManagementOutputV1,
            wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
            wp_color_management_surface_v1::WpColorManagementSurfaceV1,
            wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
        },
        wl_display::WlDisplay,
        wp_image_description_reference_v1::WpImageDescriptionReferenceV1,
        wp_image_description_v1::WpImageDescriptionV1,
    },
};

#[derive(Debug)]
pub struct WpColorManagerV1Global;

impl WpColorManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for WpColorManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (WpColorManagerV1::INTERFACE_NAME, WpColorManagerV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        version: u32,
        dispatcher: &mut ClientDispatcher,
    ) {
        let manager = WpColorManagerV1 { id: id.into(), version: version.max(1) };
        manager.send_capabilities(dispatcher);
        client.insert_object(state, manager);
    }
}

#[derive(Debug)]
pub struct WpColorManagerV1 {
    id: WpColorManagerV1Id,
    version: u32,
}

impl WpColorManagerV1 {
    fn send_capabilities(&self, dispatcher: &mut ClientDispatcher) {
        use wp_color_manager_v1::{
            Event::*, Feature::*, Primaries, RenderIntent::*, TransferFunction,
        };
        for render_intent in [Perceptual, Relative, RelativeBpc] {
            self.send_event(SupportedIntent { render_intent: render_intent as u32 }, dispatcher);
        }
        if self.version >= 2 {
            self.send_event(
                SupportedIntent { render_intent: AbsoluteNoAdaptation as u32 },
                dispatcher,
            );
        }
        for feature in [
            Parametric,
            SetPrimaries,
            SetTfPower,
            SetLuminances,
            SetMasteringDisplayPrimaries,
            ExtendedTargetVolume,
            WindowsScrgb,
        ] {
            self.send_event(SupportedFeature { feature: feature as u32 }, dispatcher);
        }
        if self.version >= wp_color_manager_v1::CREATE_WINDOWS_BT2100_SINCE {
            self.send_event(SupportedFeature { feature: WindowsBt2100 as u32 }, dispatcher);
        }
        for tf in [
            TransferFunction::Bt1886,
            TransferFunction::Gamma22,
            TransferFunction::Gamma28,
            TransferFunction::St240,
            TransferFunction::ExtLinear,
            TransferFunction::Log100,
            TransferFunction::Log316,
        ] {
            self.send_event(SupportedTfNamed { tf: tf as u32 }, dispatcher);
        }
        if self.version < 2 {
            self.send_event(SupportedTfNamed { tf: TransferFunction::Srgb as u32 }, dispatcher);
            self.send_event(SupportedTfNamed { tf: TransferFunction::ExtSrgb as u32 }, dispatcher);
        }
        self.send_event(SupportedTfNamed { tf: TransferFunction::St2084Pq as u32 }, dispatcher);
        self.send_event(SupportedTfNamed { tf: TransferFunction::St428 as u32 }, dispatcher);
        if self.version >= 2 {
            self.send_event(
                SupportedTfNamed { tf: TransferFunction::CompoundPower24 as u32 },
                dispatcher,
            );
        }
        for primaries in [
            Primaries::Srgb,
            Primaries::PalM,
            Primaries::Pal,
            Primaries::Ntsc,
            Primaries::GenericFilm,
            Primaries::Bt2020,
            Primaries::Cie1931Xyz,
            Primaries::DciP3,
            Primaries::DisplayP3,
            Primaries::AdobeRgb,
        ] {
            self.send_event(SupportedPrimariesNamed { primaries: primaries as u32 }, dispatcher);
        }
        self.send_event(Done, dispatcher);
    }

    fn unsupported(&self, client: &Client, dispatcher: &mut ClientDispatcher, message: &str) {
        client.send_protocol_error(
            dispatcher,
            self.id.into(),
            wp_color_manager_v1::Error::UnsupportedFeature as u32,
            message,
        );
    }

    fn publish_prebuilt(
        &self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        id: WpImageDescriptionV1Id,
        description: Rc<crate::color::ImageDescription>,
    ) {
        let (_, image) = client.insert_object(
            state,
            WpImageDescriptionV1::ready(id, self.version, description, false),
        );
        image.publish(dispatcher);
    }
}

impl Object for WpColorManagerV1 {
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
        use wp_color_manager_v1::Request::*;
        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetOutput { id, output } => {
                let output = client.resolve_or_disconnect(output, state, dispatcher)?;
                let output_id = output.rref(state).output_id();
                client.insert_object(
                    state,
                    WpColorManagementOutputV1::new(id, self.version, output_id),
                );
            }
            GetSurface { id, surface } => {
                let surface = client.resolve_or_disconnect(surface, state, dispatcher)?;
                if surface.rref(state).has_color_management_surface() {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        wp_color_manager_v1::Error::SurfaceExists as u32,
                        "the surface already has a wp_color_management_surface_v1",
                    );
                    return Ok(());
                }
                client.insert_object(
                    state,
                    WpColorManagementSurfaceV1::new(id, self.version, surface),
                );
                let object = client.resolve(id, state).unwrap();
                surface.grab(state).set_color_management_surface(object);
            }
            GetSurfaceFeedback { id, surface } => {
                let surface = client.resolve_or_disconnect(surface, state, dispatcher)?;
                let preferred = surface.rref(state).preferred_image_description(state).id();
                client.insert_object(
                    state,
                    WpColorManagementSurfaceFeedbackV1::new(id, self.version, surface),
                );
                let object = client.resolve(id, state).unwrap();
                surface.grab(state).add_color_feedback(object, preferred);
            }
            CreateIccCreator { .. } => {
                self.unsupported(client, dispatcher, "ICC image descriptions are not supported");
            }
            CreateParametricCreator { obj } => {
                client.insert_object(
                    state,
                    WpImageDescriptionCreatorParamsV1::new(obj, self.version),
                );
            }
            CreateWindowsScrgb { image_description } => {
                let description = state.color_manager.windows_scrgb();
                self.publish_prebuilt(client, state, dispatcher, image_description, description);
            }
            GetImageDescription { image_description, reference } => {
                if self.version < wp_color_manager_v1::GET_IMAGE_DESCRIPTION_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "get_image_description requires version 2",
                    );
                    return Ok(());
                }
                let reference = client.resolve_or_disconnect(reference, state, dispatcher)?;
                let (description, information_allowed) = {
                    let reference = reference.rref(state);
                    (Rc::clone(reference.description()), reference.information_allowed())
                };
                let (_, image) = client.insert_object(
                    state,
                    WpImageDescriptionV1::ready(
                        image_description,
                        self.version,
                        description,
                        information_allowed,
                    ),
                );
                image.publish(dispatcher);
            }
            CreateWindowsBt2100 { image_description } => {
                if self.version < wp_color_manager_v1::CREATE_WINDOWS_BT2100_SINCE {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wl_display::Error::InvalidMethod as u32,
                        "create_windows_bt2100 requires version 3",
                    );
                    return Ok(());
                }
                let description = state.color_manager.windows_bt2100();
                self.publish_prebuilt(client, state, dispatcher, image_description, description);
            }
        }
        Ok(())
    }
}
