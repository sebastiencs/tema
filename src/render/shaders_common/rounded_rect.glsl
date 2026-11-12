// Rounded-rectangle coverage shared by the GLES and Vulkan shaders.
// Valid as GLSL ES 1.00 and as GLSL 450: no qualifiers, no version-specific
// built-ins. GLES sources get it pasted in by `expand_gles_includes`
// (src/render/shader_source.rs). Vulkan sources get it through shaderc's
// include callback in build.rs.
// Never write the hash-include directive text in this file's comments:
// tests assert that the expanded GLES source contains no such text.
//
// Units are framebuffer pixels: callers pass v_local_position, which is
// a_position * item_size, and every item is drawn 1:1 onto its target. The
// fade is therefore one pixel wide without screen-space derivatives.

// Signed distance from `point` to the edge of the rounded rectangle `rect`
// (x, y, width, height). Negative inside, positive outside. The corner is
// |dx|^p + |dy|^p = radius^p with p = roundingPower, the same curve as
// RootSurfaceClip::contains_point. The radius is clamped to half the shorter
// side, as RootSurfaceClip::new does.
float roundedRectDistance(vec2 point, vec4 rect, float radius, float roundingPower) {
    vec2 halfSize = rect.zw * 0.5;
    float r = clamp(radius, 0.0, min(halfSize.x, halfSize.y));
    vec2 q = abs(point - (rect.xy + halfSize)) - (halfSize - vec2(r));
    if (r > 0.0 && q.x > 0.0 && q.y > 0.0) {
        float p = max(roundingPower, 0.0001);
        float n = pow(pow(q.x, p) + pow(q.y, p), 1.0 / p);
        // Length of the p-norm gradient. It is 1 for a circle (p = 2).
        // Dividing by it turns the norm excess into a Euclidean distance.
        vec2 g = pow(q / n, vec2(p - 1.0));
        return (n - r) / max(length(g), 0.0001);
    }
    return max(q.x, q.y) - r;
}

// Fraction of the pixel centred on `point` that lies inside the rounded
// rectangle grown outward by `grow` pixels. Straight edges are pixel-aligned,
// so their distance is exactly +-0.5; the snap removes interpolation noise
// there and keeps those edges identical to a hard cutoff.
float roundedRectCoverageGrown(
    vec2 point,
    vec4 rect,
    float radius,
    float roundingPower,
    float grow
) {
    float distance = roundedRectDistance(point, rect, radius, roundingPower) - grow;
    float coverage = clamp(0.5 - distance, 0.0, 1.0);
    if (coverage < 0.001) {
        return 0.0;
    }
    if (coverage > 0.999) {
        return 1.0;
    }
    return coverage;
}

float roundedRectCoverage(vec2 point, vec4 rect, float radius, float roundingPower) {
    return roundedRectCoverageGrown(point, rect, radius, roundingPower, 0.0);
}
