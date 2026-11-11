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
bool pointInRect(vec2 point, vec4 rect) {
    return point.x >= rect.x
        && point.x < (rect.x + rect.z)
        && point.y >= rect.y
        && point.y < (rect.y + rect.w);
}
float roundedCornerDistance(vec2 delta, float roundingPower) {
    vec2 absoluteDelta = abs(delta);
    return pow(pow(absoluteDelta.x, roundingPower) + pow(absoluteDelta.y, roundingPower), 1.0 / roundingPower);
}
void maybeDiscardRoundedCorner(vec2 point, vec2 center, float radius, float roundingPower) {
    vec2 delta = point - center;
    if (roundedCornerDistance(delta, roundingPower) > radius) {
        discard;
    }
}
bool pointInRoundedRect(vec2 point, vec4 rect, float radius, float roundingPower) {
    if (!pointInRect(point, rect)) {
        return false;
    }
    if (radius <= 0.0) {
        return true;
    }
    float left = rect.x + radius;
    float top = rect.y + radius;
    float right = rect.x + rect.z - radius;
    float bottom = rect.y + rect.w - radius;
    if (point.x < left && point.y < top) {
        return roundedCornerDistance(point - vec2(left, top), roundingPower) <= radius;
    }
    if (point.x >= right && point.y < top) {
        return roundedCornerDistance(point - vec2(right, top), roundingPower) <= radius;
    }
    if (point.x < left && point.y >= bottom) {
        return roundedCornerDistance(point - vec2(left, bottom), roundingPower) <= radius;
    }
    if (point.x >= right && point.y >= bottom) {
        return roundedCornerDistance(point - vec2(right, bottom), roundingPower) <= radius;
    }
    return true;
}
void discardOutsideRoundedRect(vec2 point, vec4 rect, float radius, float roundingPower) {
    if (!pointInRoundedRect(point, rect, radius, roundingPower)) {
        discard;
    }
}
void main() {
    vec2 point = v_local_position;
    if (u_clip_mode == 3) {
        // Corner-only rounding: keep pixels outside the window box so that
        // client-drawn CSD shadows survive; discard only pixels inside the
        // box that fall outside a rounded corner.
        float outerRadius = min(
            u_clip_outer_corner_radius,
            min(u_clip_outer_rect.z, u_clip_outer_rect.w) * 0.5
        );
        float outerRoundingPower = max(u_clip_outer_rounding_power, 0.0001);
        if (pointInRect(point, u_clip_outer_rect)
            && !pointInRoundedRect(point, u_clip_outer_rect, outerRadius, outerRoundingPower)) {
            discard;
        }
    } else if (u_clip_mode != 0) {
        float outerRadius = min(
            u_clip_outer_corner_radius,
            min(u_clip_outer_rect.z, u_clip_outer_rect.w) * 0.5
        );
        float outerRoundingPower = max(u_clip_outer_rounding_power, 0.0001);
        discardOutsideRoundedRect(point, u_clip_outer_rect, outerRadius, outerRoundingPower);
        if (u_clip_mode == 2) {
            float innerRadius = min(
                u_clip_inner_corner_radius,
                min(u_clip_inner_rect.z, u_clip_inner_rect.w) * 0.5
            );
            float innerRoundingPower = max(u_clip_inner_rounding_power, 0.0001);
            if (pointInRoundedRect(point, u_clip_inner_rect, innerRadius, innerRoundingPower)) {
                discard;
            }
        }
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
    color *= u_alpha;
    gl_FragColor = color;
}
