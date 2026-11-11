use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    color::{ImageDescriptionParams, Luminances, NamedPrimaries, Primaries, TransferFunction},
    object::{Object, RequestResult},
    protocol_generated::{
        wp_image_description_creator_params_v1, wp_image_description_v1, WaylandId,
        WpImageDescriptionCreatorParamsV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::{
        color_management::{
            chromaticities, primaries_from_protocol, transfer_function_from_power,
            transfer_function_from_protocol,
        },
        wl_display::WlDisplay,
        wp_image_description_v1::WpImageDescriptionV1,
    },
};

#[derive(Debug)]
pub struct WpImageDescriptionCreatorParamsV1 {
    id: WpImageDescriptionCreatorParamsV1Id,
    version: u32,
    tf: Option<TransferFunction>,
    primaries: Option<(Option<NamedPrimaries>, Primaries)>,
    luminances: Option<Luminances>,
    mastering_primaries: Option<Primaries>,
    mastering_luminance: Option<(f64, f64)>,
    max_cll_set: bool,
    max_cll: Option<f64>,
    max_fall_set: bool,
    max_fall: Option<f64>,
}

impl WpImageDescriptionCreatorParamsV1 {
    pub(crate) fn new(id: WpImageDescriptionCreatorParamsV1Id, version: u32) -> Self {
        Self {
            id,
            version,
            tf: None,
            primaries: None,
            luminances: None,
            mastering_primaries: None,
            mastering_luminance: None,
            max_cll_set: false,
            max_cll: None,
            max_fall_set: false,
            max_fall: None,
        }
    }

    fn protocol_error(
        &self,
        client: &Client,
        dispatcher: &mut ClientDispatcher,
        code: u32,
        message: &str,
    ) {
        client.send_protocol_error(dispatcher, self.id.into(), code, message);
    }
}

impl Object for WpImageDescriptionCreatorParamsV1 {
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
        use wp_image_description_creator_params_v1::{Error, Request::*};
        match client.parse::<Self>(req, dispatcher)? {
            Create { image_description } => {
                let Some(mut tf) = self.tf else {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::IncompleteSet as u32,
                        "transfer function is not set",
                    );
                    return Ok(());
                };
                let Some((named_primaries, primaries)) = self.primaries else {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::IncompleteSet as u32,
                        "primaries are not set",
                    );
                    return Ok(());
                };
                let mut luminances = self
                    .luminances
                    .unwrap_or_else(|| Luminances::for_transfer_function(tf));
                if tf == TransferFunction::St2084Pq {
                    luminances.max = luminances.min + 10_000.0;
                }
                if luminances.max <= luminances.min || luminances.white <= luminances.min {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::InvalidLuminance as u32,
                        "luminance maximum and reference white must exceed the minimum",
                    );
                    return Ok(());
                }
                let target_luminances = self
                    .mastering_luminance
                    .unwrap_or((luminances.min, luminances.max));
                if self.version < 2 {
                    let inside =
                        |value: f64| value > target_luminances.0 && value <= target_luminances.1;
                    if self.max_cll.is_some_and(|value| !inside(value))
                        || self.max_fall.is_some_and(|value| !inside(value))
                    {
                        self.protocol_error(
                            client,
                            dispatcher,
                            Error::InvalidLuminance as u32,
                            "max_cll and max_fall must be inside the mastering luminance range",
                        );
                        return Ok(());
                    }
                }
                if self
                    .max_cll
                    .is_some_and(|cll| self.max_fall.is_some_and(|fall| fall > cll))
                {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::InvalidLuminance as u32,
                        "max_fall must be less than or equal to max_cll",
                    );
                    return Ok(());
                }
                if let TransferFunction::Bt1886 { .. } = tf {
                    let ratio =
                        if luminances.max > 0.0 { luminances.min / luminances.max } else { 0.0 };
                    tf = TransferFunction::Bt1886 { lb_over_lw: ratio as f32 };
                }
                let (primaries, named_primaries) = match named_primaries {
                    Some(named) => (named.chromaticities(), Some(named)),
                    None => match Primaries::from_chromaticities(
                        primaries.r,
                        primaries.g,
                        primaries.b,
                        primaries.w,
                    ) {
                        Some(primaries) => (primaries, None),
                        None => {
                            publish_failed(
                                client,
                                state,
                                dispatcher,
                                self.id.into(),
                                image_description,
                                self.version,
                                "primaries are unusable",
                            );
                            return Ok(());
                        }
                    },
                };
                let target_primaries = match self.mastering_primaries {
                    Some(primaries) => match Primaries::from_chromaticities(
                        primaries.r,
                        primaries.g,
                        primaries.b,
                        primaries.w,
                    ) {
                        Some(primaries) => primaries,
                        None => {
                            publish_failed(
                                client,
                                state,
                                dispatcher,
                                self.id.into(),
                                image_description,
                                self.version,
                                "mastering display primaries are unusable",
                            );
                            return Ok(());
                        }
                    },
                    None => primaries,
                };
                let description = state.color_manager.intern(ImageDescriptionParams {
                    tf,
                    primaries,
                    named_primaries,
                    luminances,
                    target_primaries,
                    target_luminances,
                    max_cll: self.max_cll,
                    max_fall: self.max_fall,
                });
                publish_ready(
                    client,
                    state,
                    dispatcher,
                    self.id.into(),
                    image_description,
                    self.version,
                    description,
                    false,
                );
            }
            SetTfNamed { tf } => {
                if self.tf.is_some() {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::AlreadySet as u32,
                        "transfer function is already set",
                    );
                    return Ok(());
                }
                let Some(tf) = transfer_function_from_protocol(tf, self.version) else {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::InvalidTf as u32,
                        "transfer function is unknown or not advertised",
                    );
                    return Ok(());
                };
                self.tf = Some(tf);
            }
            SetTfPower { eexp } => {
                if self.tf.is_some() {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::AlreadySet as u32,
                        "transfer function is already set",
                    );
                    return Ok(());
                }
                let Some(tf) = transfer_function_from_power(eexp) else {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::InvalidTf as u32,
                        "power exponent must be between 1.0 and 10.0",
                    );
                    return Ok(());
                };
                self.tf = Some(tf);
            }
            SetPrimariesNamed { primaries } => {
                if self.primaries.is_some() {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::AlreadySet as u32,
                        "primaries are already set",
                    );
                    return Ok(());
                }
                let Some(named) = primaries_from_protocol(primaries) else {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::InvalidPrimariesNamed as u32,
                        "named primaries are unknown or not advertised",
                    );
                    return Ok(());
                };
                self.primaries = Some((Some(named), named.chromaticities()));
            }
            SetPrimaries { r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y } => {
                if self.primaries.is_some() {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::AlreadySet as u32,
                        "primaries are already set",
                    );
                    return Ok(());
                }
                self.primaries =
                    Some((None, chromaticities(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y)));
            }
            SetLuminances { min_lum, max_lum, reference_lum } => {
                if self.luminances.is_some() {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::AlreadySet as u32,
                        "luminances are already set",
                    );
                    return Ok(());
                }
                let luminances = Luminances {
                    min: f64::from(min_lum) / 10_000.0,
                    max: f64::from(max_lum),
                    white: f64::from(reference_lum),
                };
                if luminances.max <= luminances.min || luminances.white <= luminances.min {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::InvalidLuminance as u32,
                        "luminance maximum and reference white must exceed the minimum",
                    );
                    return Ok(());
                }
                self.luminances = Some(luminances);
            }
            SetMasteringDisplayPrimaries { r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y } => {
                if self.mastering_primaries.is_some() {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::AlreadySet as u32,
                        "mastering display primaries are already set",
                    );
                    return Ok(());
                }
                self.mastering_primaries =
                    Some(chromaticities(r_x, r_y, g_x, g_y, b_x, b_y, w_x, w_y));
            }
            SetMasteringLuminance { min_lum, max_lum } => {
                if self.mastering_luminance.is_some() {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::AlreadySet as u32,
                        "mastering luminance is already set",
                    );
                    return Ok(());
                }
                let min = f64::from(min_lum) / 10_000.0;
                let max = f64::from(max_lum);
                if max <= min {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::InvalidLuminance as u32,
                        "mastering maximum luminance must exceed the minimum",
                    );
                    return Ok(());
                }
                self.mastering_luminance = Some((min, max));
            }
            SetMaxCll { max_cll } => {
                if self.max_cll_set {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::AlreadySet as u32,
                        "max_cll is already set",
                    );
                    return Ok(());
                }
                self.max_cll_set = true;
                self.max_cll = (max_cll != 0).then_some(f64::from(max_cll));
            }
            SetMaxFall { max_fall } => {
                if self.max_fall_set {
                    self.protocol_error(
                        client,
                        dispatcher,
                        Error::AlreadySet as u32,
                        "max_fall is already set",
                    );
                    return Ok(());
                }
                self.max_fall_set = true;
                self.max_fall = (max_fall != 0).then_some(f64::from(max_fall));
            }
        }
        Ok(())
    }
}

fn publish_ready(
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    creator: WaylandId,
    image_description: crate::protocol_generated::WpImageDescriptionV1Id,
    version: u32,
    description: std::rc::Rc<crate::color::ImageDescription>,
    information_allowed: bool,
) {
    {
        let (_, image) = client.insert_object(
            state,
            WpImageDescriptionV1::ready(
                image_description,
                version,
                description,
                information_allowed,
            ),
        );
        image.publish(dispatcher);
    }
    client.delete_id(&WlDisplay::default(), state, dispatcher, creator);
}

fn publish_failed(
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    creator: WaylandId,
    image_description: crate::protocol_generated::WpImageDescriptionV1Id,
    version: u32,
    message: &str,
) {
    {
        let (_, image) = client.insert_object(
            state,
            WpImageDescriptionV1::failed(
                image_description,
                version,
                wp_image_description_v1::Cause::Unsupported as u32,
                message,
            ),
        );
        image.publish(dispatcher);
    }
    client.delete_id(&WlDisplay::default(), state, dispatcher, creator);
}
