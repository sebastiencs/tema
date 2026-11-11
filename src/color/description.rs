use std::{fmt, rc::Rc};

use super::{
    luminance::Luminances, manager::Shared, primaries::Primaries, tf::TransferFunction,
    NamedPrimaries,
};

/// Parameters that identify an image description.
///
/// `target_luminances` is `(min, max)` of the target color volume. The target
/// volume is part of the description identity. It is not the destination of a
/// [`super::ColorTransform`].
#[derive(Copy, Clone, Debug)]
pub struct ImageDescriptionParams {
    pub tf: TransferFunction,
    pub primaries: Primaries,
    pub named_primaries: Option<NamedPrimaries>,
    pub luminances: Luminances,
    pub target_primaries: Primaries,
    pub target_luminances: (f64, f64),
    pub max_cll: Option<f64>,
    pub max_fall: Option<f64>,
}

impl ImageDescriptionParams {
    pub fn srgb_gamma22() -> Self {
        Self {
            tf: TransferFunction::Gamma22,
            primaries: Primaries::SRGB,
            named_primaries: Some(NamedPrimaries::Srgb),
            luminances: Luminances::SRGB,
            target_primaries: Primaries::SRGB,
            target_luminances: (Luminances::SRGB.min, Luminances::SRGB.max),
            max_cll: None,
            max_fall: None,
        }
    }

    pub fn windows_scrgb() -> Self {
        Self {
            tf: TransferFunction::ExtLinear,
            primaries: Primaries::SRGB,
            named_primaries: Some(NamedPrimaries::Srgb),
            luminances: Luminances::WINDOWS_SCRGB,
            target_primaries: Primaries::BT2020,
            target_luminances: (Luminances::ST2084_PQ.min, Luminances::ST2084_PQ.max),
            max_cll: None,
            max_fall: None,
        }
    }

    pub fn windows_bt2100() -> Self {
        Self {
            tf: TransferFunction::St2084Pq,
            primaries: Primaries::BT2020,
            named_primaries: Some(NamedPrimaries::Bt2020),
            luminances: Luminances::ST2084_PQ,
            target_primaries: Primaries::BT2020,
            target_luminances: (Luminances::ST2084_PQ.min, Luminances::ST2084_PQ.max),
            max_cll: None,
            max_fall: None,
        }
    }
}

pub(super) struct LinearPart {
    pub(super) id: u64,
    pub(super) primaries: Primaries,
    pub(super) luminances: Luminances,
    pub(super) target_primaries: Primaries,
    pub(super) target_luminances: (f64, f64),
    pub(super) max_cll: Option<f64>,
    pub(super) max_fall: Option<f64>,
    pub(super) xyz_from_rgb: super::Matrix3x4,
    pub(super) rgb_from_xyz: super::Matrix3x4,
    pub(super) shared: Rc<Shared>,
}

impl Drop for LinearPart {
    fn drop(&mut self) {
        let dead = self.shared.dead_linear.get();
        self.shared.dead_linear.set(dead + 1);
    }
}

pub struct ImageDescription {
    pub(super) id: u64,
    pub(super) linear: Rc<LinearPart>,
    pub(super) tf: TransferFunction,
    pub(super) named_primaries: Option<NamedPrimaries>,
    pub(super) shared: Rc<Shared>,
}

impl fmt::Debug for ImageDescription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageDescription")
            .field("id", &self.id)
            .field("linear_id", &self.linear.id)
            .field("tf", &self.tf)
            .field("named_primaries", &self.named_primaries)
            .finish()
    }
}

impl ImageDescription {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn linear_id(&self) -> u64 {
        self.linear.id
    }

    pub fn transfer_function(&self) -> TransferFunction {
        self.tf
    }

    pub fn named_primaries(&self) -> Option<NamedPrimaries> {
        self.named_primaries
    }

    pub fn primaries(&self) -> Primaries {
        self.linear.primaries
    }

    pub fn luminances(&self) -> Luminances {
        self.linear.luminances
    }

    pub fn target_primaries(&self) -> Primaries {
        self.linear.target_primaries
    }

    pub fn target_luminances(&self) -> (f64, f64) {
        self.linear.target_luminances
    }

    pub fn max_cll(&self) -> Option<f64> {
        self.linear.max_cll
    }

    pub fn max_fall(&self) -> Option<f64> {
        self.linear.max_fall
    }
}

impl Drop for ImageDescription {
    fn drop(&mut self) {
        let dead = self.shared.dead_complete.get();
        self.shared.dead_complete.set(dead + 1);
    }
}
