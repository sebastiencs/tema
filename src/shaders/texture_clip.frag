precision highp float;
#ifndef YUV_MODE
#define YUV_MODE 0
#endif
#ifndef NEEDS_COLOR_PATH
#define NEEDS_COLOR_PATH 0
#endif
varying vec2 v_texcoord;
varying vec2 v_local_position;
uniform highp sampler2D u_texture;
uniform int u_clip_mode;
uniform vec4 u_clip_outer_rect;
uniform vec4 u_clip_inner_rect;
uniform float u_clip_outer_corner_radius;
uniform float u_clip_outer_rounding_power;
uniform float u_clip_inner_corner_radius;
uniform float u_clip_inner_rounding_power;
uniform vec2 u_item_size;
uniform float u_alpha;
uniform int u_opaque_sample;
#if YUV_MODE
uniform highp sampler2D u_texture_uv;
uniform vec2 u_yuv_size;
uniform vec2 u_yuv_site;
uniform vec4 u_yuv_row0;
uniform vec4 u_yuv_row1;
uniform vec4 u_yuv_row2;
#endif
#include "rounded_rect.glsl"
bool pointInRect(vec2 point, vec4 rect) {
    return point.x >= rect.x
        && point.x < (rect.x + rect.z)
        && point.y >= rect.y
        && point.y < (rect.y + rect.w);
}
void main() {
    vec2 point = v_local_position;
    float coverage = 1.0;
    if (u_clip_mode == 3) {
        // Corner-only rounding: keep pixels outside the window box so that
        // client-drawn CSD shadows survive; fade only pixels inside the
        // box near a rounded corner.
        if (pointInRect(point, u_clip_outer_rect)) {
            coverage = roundedRectCoverage(
                point,
                u_clip_outer_rect,
                u_clip_outer_corner_radius,
                u_clip_outer_rounding_power
            );
        }
    } else if (u_clip_mode == 4) {
        // Frame clip under an opaque border: fade 1px further out, so the
        // border's inner fade always sits over fully covered window pixels.
        // The exact box still bounds it.
        if (pointInRect(point, u_clip_outer_rect)) {
            coverage = roundedRectCoverageGrown(
                point,
                u_clip_outer_rect,
                u_clip_outer_corner_radius,
                u_clip_outer_rounding_power,
                1.0
            );
        } else {
            coverage = 0.0;
        }
    } else if (u_clip_mode != 0) {
        coverage = roundedRectCoverage(
            point,
            u_clip_outer_rect,
            u_clip_outer_corner_radius,
            u_clip_outer_rounding_power
        );
        if (u_clip_mode == 2) {
            coverage *= 1.0 - roundedRectCoverage(
                point,
                u_clip_inner_rect,
                u_clip_inner_corner_radius,
                u_clip_inner_rounding_power
            );
        }
    }
    if (coverage <= 0.0) {
        discard;
    }
#if YUV_MODE
    // GR88 and GR1616 store Cb in R and Cr in G. YUV is opaque.
    vec2 cbcr = texture2D(u_texture_uv, yuv_chroma_coord(v_texcoord, u_yuv_size, u_yuv_site)).rg;
    vec4 color = vec4(
        yuv_to_rgb(
            texture2D(u_texture, v_texcoord).r,
            cbcr.r,
            cbcr.g,
            u_yuv_row0,
            u_yuv_row1,
            u_yuv_row2
        ),
        1.0
    );
#else
    vec4 color = texture2D(u_texture, v_texcoord);
    // X formats have no alpha. Vulkan swizzles that sample to 1.
    // `highp` keeps FP16 values above 1.0. The framebuffer encode clamps.
    color.a = mix(color.a, 1.0, float(u_opaque_sample));
#endif
#if NEEDS_COLOR_PATH
    color = apply_color(color, 1, SRC_TF, DST_TF, HAS_MATRIX, ALPHA_MODE, 1, TONEMAP);
#endif
    color *= u_alpha * coverage;
    gl_FragColor = color;
}
