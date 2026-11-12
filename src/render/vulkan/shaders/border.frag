#version 450
layout(location = 0) in vec2 v_local_position;
layout(location = 0) out vec4 out_color;
layout(std140, set = 0, binding = 0) uniform Border {
    vec4 transform_c0;
    vec4 transform_c1;
    vec4 transform_c2;
    vec2 item_size;
    float outer_radius;
    float outer_power;
    float inner_radius;
    float inner_power;
    float angle;
    float alpha;
    vec4 outer_rect;
    vec4 inner_rect;
    vec4 gradient_start;
    vec4 gradient_end;
} draw;
#include "rounded_rect.glsl"
void main() {
    float outerCoverage = roundedRectCoverage(
        v_local_position,
        draw.outer_rect,
        draw.outer_radius,
        draw.outer_power
    );
    float innerCoverage = roundedRectCoverage(
        v_local_position,
        draw.inner_rect,
        draw.inner_radius,
        draw.inner_power
    );
    float coverage = outerCoverage * (1.0 - innerCoverage);
    if (coverage <= 0.0) {
        discard;
    }
    vec2 normalized = (v_local_position / max(draw.item_size, vec2(1.0, 1.0))) - vec2(0.5, 0.5);
    float angle = radians(draw.angle);
    vec2 direction = vec2(cos(angle), sin(angle));
    float progress = clamp(dot(normalized, direction) + 0.5, 0.0, 1.0);
    out_color = mix(draw.gradient_start, draw.gradient_end, progress) * (draw.alpha * coverage);
}
