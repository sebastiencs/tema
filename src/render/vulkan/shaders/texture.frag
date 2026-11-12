#version 450
// Clip tests follow src/shaders/texture_clip.frag so the mask lands on the same pixels.
layout(location = 0) in vec2 v_texcoord;
layout(location = 1) in vec2 v_local_position;
layout(location = 0) out vec4 out_color;
layout(set = 0, binding = 0) uniform sampler2D u_texture;
layout(set = 0, binding = 2) uniform sampler2D u_texture_uv;
layout(std140, set = 0, binding = 1) uniform Draw {
    vec4 transform_c0;
    vec4 transform_c1;
    vec4 transform_c2;
    vec4 tex_c0;
    vec4 tex_c1;
    vec4 tex_c2;
    vec2 item_size;
    float alpha;
    int clip_mode;
    vec4 outer_rect;
    vec4 inner_rect;
    float outer_radius;
    float outer_power;
    float inner_radius;
    float inner_power;
    vec4 color_matrix_0;
    vec4 color_matrix_1;
    vec4 color_matrix_2;
    float src_bt1886_lb_over_lw;
    float dst_bt1886_lb_over_lw;
    float src_power_exponent;
    float dst_power_exponent;
    vec2 yuv_size;
    vec2 yuv_site;
    vec4 yuv_row0;
    vec4 yuv_row1;
    vec4 yuv_row2;
    float tonemap_max;
    float tonemap_dst_max;
    float tonemap_dst_min;
    float tonemap_dst_ref;
    vec4 tonemap_to_lms_0;
    vec4 tonemap_to_lms_1;
    vec4 tonemap_to_lms_2;
    vec4 tonemap_from_lms_0;
    vec4 tonemap_from_lms_1;
    vec4 tonemap_from_lms_2;
} draw;

// Defaults match TexturePipelineKey::identity. An empty specialization list
// keeps this path, which does not convert.
layout(constant_id = 0) const int SRC_TF = 2;
layout(constant_id = 1) const int DST_TF = 2;
layout(constant_id = 2) const int HAS_MATRIX = 0;
layout(constant_id = 3) const int ALPHA_MODE = 0;
layout(constant_id = 4) const int YUV_MODE = 0;
layout(constant_id = 5) const int TONEMAP = 0;
const int NEEDS_COLOR_PATH =
    (SRC_TF != DST_TF || HAS_MATRIX != 0 || ALPHA_MODE != 0 || TONEMAP != 0) ? 1 : 0;

#define u_color_matrix_0 draw.color_matrix_0
#define u_color_matrix_1 draw.color_matrix_1
#define u_color_matrix_2 draw.color_matrix_2
#define u_src_bt1886_lb_over_lw draw.src_bt1886_lb_over_lw
#define u_dst_bt1886_lb_over_lw draw.dst_bt1886_lb_over_lw
#define u_src_power_exponent draw.src_power_exponent
#define u_dst_power_exponent draw.dst_power_exponent
#define u_tonemap_max draw.tonemap_max
#define u_tonemap_dst_max draw.tonemap_dst_max
#define u_tonemap_dst_min draw.tonemap_dst_min
#define u_tonemap_dst_ref draw.tonemap_dst_ref
#define u_tonemap_to_lms_0 draw.tonemap_to_lms_0
#define u_tonemap_to_lms_1 draw.tonemap_to_lms_1
#define u_tonemap_to_lms_2 draw.tonemap_to_lms_2
#define u_tonemap_from_lms_0 draw.tonemap_from_lms_0
#define u_tonemap_from_lms_1 draw.tonemap_from_lms_1
#define u_tonemap_from_lms_2 draw.tonemap_from_lms_2
#include "color.glsl"
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
    if (draw.clip_mode == 3) {
        // Corner-only rounding: keep pixels outside the window box so that
        // client-drawn CSD shadows survive; fade only pixels inside the
        // box near a rounded corner.
        if (pointInRect(point, draw.outer_rect)) {
            coverage = roundedRectCoverage(
                point,
                draw.outer_rect,
                draw.outer_radius,
                draw.outer_power
            );
        }
    } else if (draw.clip_mode != 0) {
        coverage = roundedRectCoverage(
            point,
            draw.outer_rect,
            draw.outer_radius,
            draw.outer_power
        );
        if (draw.clip_mode == 2) {
            coverage *= 1.0 - roundedRectCoverage(
                point,
                draw.inner_rect,
                draw.inner_radius,
                draw.inner_power
            );
        }
    }
    if (coverage <= 0.0) {
        discard;
    }
    vec4 color;
    if (YUV_MODE != 0) {
        vec2 cbcr = texture(u_texture_uv, yuv_chroma_coord(v_texcoord, draw.yuv_size, draw.yuv_site)).rg;
        color = vec4(
            yuv_to_rgb(
                texture(u_texture, v_texcoord).r,
                cbcr.r,
                cbcr.g,
                draw.yuv_row0,
                draw.yuv_row1,
                draw.yuv_row2
            ),
            1.0
        );
    } else {
        color = texture(u_texture, v_texcoord);
    }
    color = apply_color(color, NEEDS_COLOR_PATH, SRC_TF, DST_TF, HAS_MATRIX, ALPHA_MODE, 1, TONEMAP);
    color *= draw.alpha * coverage;
    out_color = color;
}
