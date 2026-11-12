precision highp float;
varying vec2 v_local_position;
uniform vec4 u_outer_rect;
uniform vec4 u_inner_rect;
uniform float u_outer_corner_radius;
uniform float u_outer_rounding_power;
uniform float u_inner_corner_radius;
uniform float u_inner_rounding_power;
uniform vec2 u_item_size;
uniform vec4 u_gradient_start;
uniform vec4 u_gradient_end;
uniform float u_gradient_angle_degrees;
uniform float u_alpha;
#include "rounded_rect.glsl"
void main() {
    float outerCoverage = roundedRectCoverage(
        v_local_position,
        u_outer_rect,
        u_outer_corner_radius,
        u_outer_rounding_power
    );
    float innerCoverage = roundedRectCoverage(
        v_local_position,
        u_inner_rect,
        u_inner_corner_radius,
        u_inner_rounding_power
    );
    float coverage = outerCoverage * (1.0 - innerCoverage);
    if (coverage <= 0.0) {
        discard;
    }
    vec2 normalized = (v_local_position / max(u_item_size, vec2(1.0, 1.0))) - vec2(0.5, 0.5);
    float angle = radians(u_gradient_angle_degrees);
    vec2 direction = vec2(cos(angle), sin(angle));
    float progress = clamp(dot(normalized, direction) + 0.5, 0.0, 1.0);
    gl_FragColor = mix(u_gradient_start, u_gradient_end, progress) * (u_alpha * coverage);
}
