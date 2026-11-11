//! `wp_color_manager_v1` and the image-description objects it creates.

pub mod wp_color_management_output_v1;
pub mod wp_color_management_surface_feedback_v1;
pub mod wp_color_management_surface_v1;
pub mod wp_color_manager_v1;
pub mod wp_image_description_creator_params_v1;
pub mod wp_image_description_info_v1;
pub mod wp_image_description_reference_v1;
pub mod wp_image_description_v1;

#[cfg(test)]
mod tests;

use crate::{
    color::{NamedPrimaries, Primaries, TransferFunction},
    protocol_generated::wp_color_manager_v1 as color_protocol,
};

const PRIMARIES_SCALE: f64 = 1_000_000.0;
const MIN_LUM_SCALE: f64 = 10_000.0;

pub(super) fn chromaticity(value: i32) -> f64 {
    f64::from(value) / PRIMARIES_SCALE
}

pub(super) fn encode_chromaticity(value: f64) -> i32 {
    (value * PRIMARIES_SCALE).round() as i32
}

pub(super) fn encode_min_luminance(value: f64) -> u32 {
    (value * MIN_LUM_SCALE)
        .round()
        .clamp(0.0, f64::from(u32::MAX)) as u32
}

pub(super) fn encode_luminance(value: f64) -> u32 {
    value.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// Named transfer functions this bind version advertised.
pub(super) fn transfer_function_from_protocol(
    value: u32,
    version: u32,
) -> Option<TransferFunction> {
    use color_protocol::TransferFunction::*;
    let named = |function: color_protocol::TransferFunction| value == function as u32;
    if named(Bt1886) {
        Some(TransferFunction::Bt1886 { lb_over_lw: 0.0 })
    } else if named(Gamma22) {
        Some(TransferFunction::Gamma22)
    } else if named(Gamma28) {
        Some(TransferFunction::Gamma28)
    } else if named(St240) {
        Some(TransferFunction::St240)
    } else if named(ExtLinear) {
        Some(TransferFunction::ExtLinear)
    } else if named(Log100) {
        Some(TransferFunction::Log100)
    } else if named(Log316) {
        Some(TransferFunction::Log316)
    } else if named(St2084Pq) {
        Some(TransferFunction::St2084Pq)
    } else if named(St428) {
        Some(TransferFunction::St428)
    } else if version < 2 && (named(Srgb) || named(ExtSrgb)) {
        // v1 names for the piecewise IEC curve. The curve is sign-preserving.
        Some(TransferFunction::CompoundPower24)
    } else if version >= 2 && named(CompoundPower24) {
        Some(TransferFunction::CompoundPower24)
    } else {
        None
    }
}

pub(super) fn protocol_transfer_function(function: TransferFunction, version: u32) -> Option<u32> {
    use color_protocol::TransferFunction as Protocol;
    let code = match function {
        TransferFunction::Bt1886 { .. } => Protocol::Bt1886,
        TransferFunction::Gamma22 => Protocol::Gamma22,
        TransferFunction::Gamma28 => Protocol::Gamma28,
        TransferFunction::St240 => Protocol::St240,
        TransferFunction::ExtLinear => Protocol::ExtLinear,
        TransferFunction::Log100 => Protocol::Log100,
        TransferFunction::Log316 => Protocol::Log316,
        TransferFunction::St2084Pq => Protocol::St2084Pq,
        TransferFunction::St428 => Protocol::St428,
        TransferFunction::CompoundPower24 if version < 2 => Protocol::Srgb,
        TransferFunction::CompoundPower24 => Protocol::CompoundPower24,
        TransferFunction::Power(_) => return None,
    };
    Some(code as u32)
}

pub(super) fn primaries_from_protocol(value: u32) -> Option<NamedPrimaries> {
    use color_protocol::Primaries::*;
    let named = |primaries: color_protocol::Primaries| value == primaries as u32;
    if named(Srgb) {
        Some(NamedPrimaries::Srgb)
    } else if named(PalM) {
        Some(NamedPrimaries::PalM)
    } else if named(Pal) {
        Some(NamedPrimaries::Pal)
    } else if named(Ntsc) {
        Some(NamedPrimaries::Ntsc)
    } else if named(GenericFilm) {
        Some(NamedPrimaries::GenericFilm)
    } else if named(Bt2020) {
        Some(NamedPrimaries::Bt2020)
    } else if named(Cie1931Xyz) {
        Some(NamedPrimaries::Cie1931Xyz)
    } else if named(DciP3) {
        Some(NamedPrimaries::DciP3)
    } else if named(DisplayP3) {
        Some(NamedPrimaries::DisplayP3)
    } else if named(AdobeRgb) {
        Some(NamedPrimaries::AdobeRgb)
    } else {
        None
    }
}

pub(super) fn protocol_primaries(primaries: NamedPrimaries) -> u32 {
    use color_protocol::Primaries::*;
    let code = match primaries {
        NamedPrimaries::Srgb => Srgb,
        NamedPrimaries::PalM => PalM,
        NamedPrimaries::Pal => Pal,
        NamedPrimaries::Ntsc => Ntsc,
        NamedPrimaries::GenericFilm => GenericFilm,
        NamedPrimaries::Bt2020 => Bt2020,
        NamedPrimaries::Cie1931Xyz => Cie1931Xyz,
        NamedPrimaries::DciP3 => DciP3,
        NamedPrimaries::DisplayP3 => DisplayP3,
        NamedPrimaries::AdobeRgb => AdobeRgb,
    };
    code as u32
}

/// Exponent ×10000 in `[10000, 100000]`. 1.0, 2.2, and 2.8 become named functions.
pub(super) fn transfer_function_from_power(eexp: u32) -> Option<TransferFunction> {
    if !(10_000..=100_000).contains(&eexp) {
        return None;
    }
    Some(match eexp {
        10_000 => TransferFunction::ExtLinear,
        22_000 => TransferFunction::Gamma22,
        28_000 => TransferFunction::Gamma28,
        exponent => TransferFunction::Power(exponent as f32 / 10_000.0),
    })
}

pub(super) fn encode_power(exponent: f32) -> u32 {
    (f64::from(exponent) * 10_000.0).round() as u32
}

pub(super) fn publish_ready_description(
    client: &mut crate::clients::Client,
    state: &mut crate::state::State,
    dispatcher: &mut crate::clients::ClientDispatcher,
    id: crate::protocol_generated::WpImageDescriptionV1Id,
    version: u32,
    description: std::rc::Rc<crate::color::ImageDescription>,
    information_allowed: bool,
) {
    let (_, image) = client.insert_object(
        state,
        wp_image_description_v1::WpImageDescriptionV1::ready(
            id,
            version,
            description,
            information_allowed,
        ),
    );
    image.publish(dispatcher);
}

pub(super) fn publish_failed_description(
    client: &mut crate::clients::Client,
    state: &mut crate::state::State,
    dispatcher: &mut crate::clients::ClientDispatcher,
    id: crate::protocol_generated::WpImageDescriptionV1Id,
    version: u32,
    cause: u32,
    message: &str,
) {
    let (_, image) = client.insert_object(
        state,
        wp_image_description_v1::WpImageDescriptionV1::failed(id, version, cause, message),
    );
    image.publish(dispatcher);
}

pub(super) fn chromaticities(
    r_x: i32,
    r_y: i32,
    g_x: i32,
    g_y: i32,
    b_x: i32,
    b_y: i32,
    w_x: i32,
    w_y: i32,
) -> Primaries {
    Primaries {
        r: (chromaticity(r_x), chromaticity(r_y)),
        g: (chromaticity(g_x), chromaticity(g_y)),
        b: (chromaticity(b_x), chromaticity(b_y)),
        w: (chromaticity(w_x), chromaticity(w_y)),
    }
}
