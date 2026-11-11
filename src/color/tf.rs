use std::hash::{Hash, Hasher};

use super::f32_bits;

/// Encoded electrical signal to linear optical signal, and the inverse.
///
/// Gamma-type curves are sign-preserving. Piecewise curves follow the plan
/// table, including the branches that already cover negative inputs.
///
/// BT.1886 is the ITU curve remapped so signal 0 is linear 0 and signal 1 is
/// linear 1: `(L − Lb) / (Lw − Lb)`. With `Lb = 0` that is `V^2.4`. Linear 0
/// is the black level, which is what luminance mapping assumes.
///
/// PQ returns `Y` on the ST 2084 scale: `1.0` is 10000 cd/m², so cd/m² is
/// `Y * 10000`.
#[derive(Copy, Clone, Debug)]
pub enum TransferFunction {
    Gamma22,
    Gamma28,
    CompoundPower24,
    Bt1886 { lb_over_lw: f32 },
    St240,
    ExtLinear,
    Log100,
    Log316,
    St428,
    St2084Pq,
    Power(f32),
}

impl PartialEq for TransferFunction {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Gamma22, Self::Gamma22)
            | (Self::Gamma28, Self::Gamma28)
            | (Self::CompoundPower24, Self::CompoundPower24)
            | (Self::St240, Self::St240)
            | (Self::ExtLinear, Self::ExtLinear)
            | (Self::Log100, Self::Log100)
            | (Self::Log316, Self::Log316)
            | (Self::St428, Self::St428)
            | (Self::St2084Pq, Self::St2084Pq) => true,
            (Self::Bt1886 { lb_over_lw: left }, Self::Bt1886 { lb_over_lw: right }) => {
                f32_bits(*left) == f32_bits(*right)
            }
            (Self::Power(left), Self::Power(right)) => f32_bits(*left) == f32_bits(*right),
            _ => false,
        }
    }
}

impl Eq for TransferFunction {}

impl Hash for TransferFunction {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Bt1886 { lb_over_lw } => f32_bits(*lb_over_lw).hash(state),
            Self::Power(exponent) => f32_bits(*exponent).hash(state),
            _ => {}
        }
    }
}

impl TransferFunction {
    /// Encoded value to linear optical value.
    pub fn decode(self, encoded: f64) -> f64 {
        match self {
            Self::Gamma22 => signed_pow(encoded, 2.2),
            Self::Gamma28 => signed_pow(encoded, 2.8),
            Self::CompoundPower24 => {
                if encoded <= 0.04045 {
                    encoded / 12.92
                } else {
                    ((encoded + 0.055) / 1.055).powf(2.4)
                }
            }
            Self::Bt1886 { lb_over_lw } => bt1886_decode(encoded, f64::from(lb_over_lw)),
            Self::St240 => {
                if encoded < 0.0913 {
                    encoded / 4.0
                } else {
                    ((encoded + 0.1115) / 1.1115).powf(1.0 / 0.45)
                }
            }
            Self::ExtLinear => encoded,
            Self::Log100 => {
                if encoded <= 0.0 {
                    0.0
                } else {
                    10.0_f64.powf((encoded - 1.0) * 2.0)
                }
            }
            Self::Log316 => {
                if encoded <= 0.0 {
                    0.0
                } else {
                    10.0_f64.powf((encoded - 1.0) * 2.5)
                }
            }
            Self::St428 => signed_pow(encoded, 2.6) * (52.37 / 48.0),
            Self::St2084Pq => pq_decode(encoded),
            Self::Power(exponent) => signed_pow(encoded, f64::from(exponent)),
        }
    }

    /// Linear optical value to encoded value.
    pub fn encode(self, linear: f64) -> f64 {
        match self {
            Self::Gamma22 => signed_pow(linear, 1.0 / 2.2),
            Self::Gamma28 => signed_pow(linear, 1.0 / 2.8),
            Self::CompoundPower24 => {
                if linear <= 0.0031308 {
                    12.92 * linear
                } else {
                    1.055 * linear.powf(1.0 / 2.4) - 0.055
                }
            }
            Self::Bt1886 { lb_over_lw } => bt1886_encode(linear, f64::from(lb_over_lw)),
            Self::St240 => {
                if linear < 0.0913 / 4.0 {
                    4.0 * linear
                } else {
                    1.1115 * linear.powf(0.45) - 0.1115
                }
            }
            Self::ExtLinear => linear,
            Self::Log100 => {
                if linear < 0.01 {
                    0.0
                } else {
                    1.0 + linear.log10() / 2.0
                }
            }
            Self::Log316 => {
                if linear < 10.0_f64.sqrt() / 1000.0 {
                    0.0
                } else {
                    1.0 + linear.log10() / 2.5
                }
            }
            Self::St428 => signed_pow(linear * (48.0 / 52.37), 1.0 / 2.6),
            Self::St2084Pq => pq_encode(linear),
            Self::Power(exponent) => signed_pow(linear, 1.0 / f64::from(exponent)),
        }
    }
}

fn signed_pow(value: f64, exponent: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value.signum() * value.abs().powf(exponent)
    }
}

/// `(L − Lb) / (Lw − Lb)` of the ITU BT.1886 EOTF, with `Lw = 1` and `Lb = c`.
fn bt1886_decode(encoded: f64, lb_over_lw: f64) -> f64 {
    if lb_over_lw <= 0.0 {
        return signed_pow(encoded, 2.4);
    }
    let gamma = 1.0 / 2.4;
    let a1 = 1.0 / (1.0 - lb_over_lw);
    let a2 = 1.0 - lb_over_lw.powf(gamma);
    let a3 = lb_over_lw.powf(gamma);
    let lifted = a2 * encoded + a3;
    if lifted <= 0.0 {
        a1 * (0.0 - lb_over_lw)
    } else {
        a1 * (lifted.powf(2.4) - lb_over_lw)
    }
}

fn bt1886_encode(linear: f64, lb_over_lw: f64) -> f64 {
    if lb_over_lw <= 0.0 {
        return signed_pow(linear, 1.0 / 2.4);
    }
    let gamma = 1.0 / 2.4;
    let a1 = 1.0 / (1.0 - lb_over_lw.powf(gamma));
    let a2 = 1.0 - lb_over_lw;
    let a4 = lb_over_lw.powf(gamma);
    let lifted = a2 * linear + lb_over_lw;
    if lifted <= 0.0 {
        a1 * (0.0 - a4)
    } else {
        a1 * (lifted.powf(gamma) - a4)
    }
}

fn pq_decode(encoded: f64) -> f64 {
    const M1: f64 = 0.1593017578125;
    const M2: f64 = 78.84375;
    const C1: f64 = 0.8359375;
    const C2: f64 = 18.8515625;
    const C3: f64 = 18.6875;
    if encoded <= 0.0 {
        return 0.0;
    }
    let p = encoded.powf(1.0 / M2);
    let numerator = (p - C1).max(0.0);
    let denominator = C2 - C3 * p;
    (numerator / denominator).powf(1.0 / M1)
}

fn pq_encode(linear: f64) -> f64 {
    const M1: f64 = 0.1593017578125;
    const M2: f64 = 78.84375;
    const C1: f64 = 0.8359375;
    const C2: f64 = 18.8515625;
    const C3: f64 = 18.6875;
    let y = linear.max(0.0).powf(M1);
    ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2)
}

#[cfg(test)]
mod tests {
    use super::TransferFunction;

    fn round_trips(tf: TransferFunction, samples: &[f64]) {
        for &sample in samples {
            let decoded = tf.decode(tf.encode(sample));
            let encoded = tf.encode(tf.decode(sample));
            assert!((decoded - sample).abs() < 1e-6, "{tf:?} decode(encode({sample})) = {decoded}");
            assert!((encoded - sample).abs() < 1e-6, "{tf:?} encode(decode({sample})) = {encoded}");
        }
    }

    #[test]
    fn gamma_power_and_linear_round_trip_including_sign() {
        let samples = [-0.5, 0.0, 0.25, 0.5, 1.0];
        round_trips(TransferFunction::Gamma22, &samples);
        round_trips(TransferFunction::Gamma28, &samples);
        round_trips(TransferFunction::Power(2.2), &samples);
        round_trips(TransferFunction::ExtLinear, &samples);
        round_trips(TransferFunction::St428, &samples);
        round_trips(TransferFunction::Bt1886 { lb_over_lw: 0.0 }, &samples);
    }

    #[test]
    fn piecewise_curves_round_trip_on_their_domain() {
        round_trips(TransferFunction::CompoundPower24, &[-0.001, 0.0, 0.02, 0.5, 1.0]);
        round_trips(TransferFunction::St240, &[0.0, 0.01, 0.2, 0.5, 1.0]);
        // The log encodes clamp everything at or below the knee to 0, and
        // decode maps that 0 back to 0, so the knee itself does not round trip.
        round_trips(TransferFunction::Log100, &[0.02, 0.1, 0.5, 1.0]);
        round_trips(TransferFunction::Log316, &[0.01, 0.1, 0.5, 1.0]);
        round_trips(TransferFunction::St2084Pq, &[0.05, 0.2, 0.5, 1.0]);
        round_trips(TransferFunction::Bt1886 { lb_over_lw: 0.01 }, &[0.0, 0.25, 0.5, 1.0]);
    }

    #[test]
    fn bt1886_with_zero_black_is_gamma_2_4() {
        let tf = TransferFunction::Bt1886 { lb_over_lw: 0.0 };
        for sample in [0.0_f64, 0.2, 0.5, 1.0] {
            let expected = sample.powf(2.4);
            assert!((tf.decode(sample) - expected).abs() < 1e-12);
        }
    }

    #[test]
    fn bt1886_black_and_white_map_to_zero_and_one() {
        let tf = TransferFunction::Bt1886 { lb_over_lw: 0.01 };
        assert!(tf.decode(0.0).abs() < 1e-12);
        assert!((tf.decode(1.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn pq_endpoints_are_the_st2084_scale() {
        let tf = TransferFunction::St2084Pq;
        assert_eq!(tf.decode(0.0), 0.0);
        assert!((tf.decode(1.0) - 1.0).abs() < 1e-6);
        assert!((tf.encode(1.0) - 1.0).abs() < 1e-6);
        assert!(tf.decode(tf.encode(0.0)).abs() < 1e-6);
    }

    #[test]
    fn gamma22_preserves_sign() {
        let tf = TransferFunction::Gamma22;
        let decoded = tf.decode(-0.5);
        assert!(decoded < 0.0);
        assert!((decoded + 0.5_f64.powf(2.2)).abs() < 1e-12);
    }
}
