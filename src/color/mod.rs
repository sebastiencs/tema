//! Color math for image descriptions.

mod description;
pub(crate) mod hdr;
mod intent;
mod luminance;
mod manager;
mod primaries;
mod tf;
mod tonemap;
mod transform;
pub mod yuv;

#[allow(unused_imports)]
pub use {
    description::{ImageDescription, ImageDescriptionParams},
    intent::RenderIntent,
    luminance::Luminances,
    manager::ColorManager,
    primaries::{NamedPrimaries, Primaries},
    tf::TransferFunction,
    transform::{ColorTransform, Matrix3x4},
    yuv::{
        alpha_from_wire, chroma_from_wire, chroma_matches_format, coefficients_from_wire,
        coefficients_match_format, AlphaMode, ChromaSite, Coefficients, ColorRepresentation,
        RepresentationUpdate, YuvConversion,
    },
};

pub(super) fn f64_bits(value: f64) -> u64 {
    if value == 0.0 {
        0.0_f64.to_bits()
    } else {
        value.to_bits()
    }
}

pub(super) fn f32_bits(value: f32) -> u32 {
    if value == 0.0 {
        0.0_f32.to_bits()
    } else {
        value.to_bits()
    }
}

pub(super) fn mat_mul(left: Matrix3x4, right: Matrix3x4) -> Matrix3x4 {
    let mut out = [[0.0; 4]; 3];
    for row in 0..3 {
        for col in 0..3 {
            out[row][col] = left[row][0] * right[0][col]
                + left[row][1] * right[1][col]
                + left[row][2] * right[2][col];
        }
        out[row][3] = left[row][0] * right[0][3]
            + left[row][1] * right[1][3]
            + left[row][2] * right[2][3]
            + left[row][3];
    }
    out
}

#[cfg(test)]
fn assert_matrix_close(actual: Matrix3x4, expected: [[f64; 4]; 3], tolerance: f64) {
    for row in 0..3 {
        for col in 0..4 {
            let delta = (actual[row][col] - expected[row][col]).abs();
            assert!(
                delta < tolerance,
                "matrix[{row}][{col}] = {} , expected {}, delta {delta}",
                actual[row][col],
                expected[row][col]
            );
        }
    }
}
