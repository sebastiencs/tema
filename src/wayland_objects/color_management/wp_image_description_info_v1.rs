use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    color::ImageDescription,
    object::{Object, RequestResult},
    protocol_generated::{
        wl_display, wp_image_description_info_v1, WaylandId, WpImageDescriptionInfoV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::color_management::{
        encode_chromaticity, encode_luminance, encode_min_luminance, encode_power,
        protocol_primaries, protocol_transfer_function,
    },
};

/// Sends the description once and is destroyed after `done`.
#[derive(Debug)]
pub struct WpImageDescriptionInfoV1 {
    id: WpImageDescriptionInfoV1Id,
    version: u32,
}

impl WpImageDescriptionInfoV1 {
    pub(crate) fn new(id: WpImageDescriptionInfoV1Id, version: u32) -> Self {
        Self { id, version }
    }

    pub(crate) fn send_description(
        &self,
        description: &ImageDescription,
        dispatcher: &mut ClientDispatcher,
    ) {
        use wp_image_description_info_v1::Event::*;
        let primaries = description.primaries();
        self.send_event(
            Primaries {
                r_x: encode_chromaticity(primaries.r.0),
                r_y: encode_chromaticity(primaries.r.1),
                g_x: encode_chromaticity(primaries.g.0),
                g_y: encode_chromaticity(primaries.g.1),
                b_x: encode_chromaticity(primaries.b.0),
                b_y: encode_chromaticity(primaries.b.1),
                w_x: encode_chromaticity(primaries.w.0),
                w_y: encode_chromaticity(primaries.w.1),
            },
            dispatcher,
        );
        if let Some(named) = description.named_primaries() {
            self.send_event(PrimariesNamed { primaries: protocol_primaries(named) }, dispatcher);
        }
        match description.transfer_function() {
            crate::color::TransferFunction::Power(exponent) => {
                self.send_event(TfPower { eexp: encode_power(exponent) }, dispatcher);
            }
            function => {
                let tf = protocol_transfer_function(function, self.version)
                    .expect("image descriptions use an advertised transfer function");
                self.send_event(TfNamed { tf }, dispatcher);
            }
        }
        let luminances = description.luminances();
        self.send_event(
            Luminances {
                min_lum: encode_min_luminance(luminances.min),
                max_lum: encode_luminance(luminances.max),
                reference_lum: encode_luminance(luminances.white),
            },
            dispatcher,
        );
        let target = description.target_primaries();
        self.send_event(
            TargetPrimaries {
                r_x: encode_chromaticity(target.r.0),
                r_y: encode_chromaticity(target.r.1),
                g_x: encode_chromaticity(target.g.0),
                g_y: encode_chromaticity(target.g.1),
                b_x: encode_chromaticity(target.b.0),
                b_y: encode_chromaticity(target.b.1),
                w_x: encode_chromaticity(target.w.0),
                w_y: encode_chromaticity(target.w.1),
            },
            dispatcher,
        );
        let (target_min, target_max) = description.target_luminances();
        self.send_event(
            TargetLuminance {
                min_lum: encode_min_luminance(target_min),
                max_lum: encode_luminance(target_max),
            },
            dispatcher,
        );
        if let Some(max_cll) = description.max_cll() {
            self.send_event(TargetMaxCll { max_cll: encode_luminance(max_cll) }, dispatcher);
        }
        if let Some(max_fall) = description.max_fall() {
            self.send_event(TargetMaxFall { max_fall: encode_luminance(max_fall) }, dispatcher);
        }
        self.send_event(Done, dispatcher);
    }
}

impl Object for WpImageDescriptionInfoV1 {
    fn id(&self) -> WaylandId {
        self.id.into()
    }

    fn on(
        self: GrabbedObject<Self>,
        client: &mut Client,
        _state: &mut State,
        _request: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        client.protocol_violation_disconnect(
            dispatcher,
            self.id.into(),
            wl_display::Error::InvalidMethod as u32,
            "wp_image_description_info_v1 has no requests",
        );
        Ok(())
    }
}
