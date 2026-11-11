// Shared color conversion. Valid as Vulkan GLSL 450 and as GLSL ES 1.00.
// GLES contexts are OpenGL ES 2, so this file has no #version, uint, switch,
// bit operations, or arrays of structs. The caller samples the texture.
//
// Transfer-function ids match wp_color_manager_v1.transfer_function.
// TF_POWER is the parametric set_tf_power curve; it has no protocol enum value.

#ifndef COLOR_GLSL
#define COLOR_GLSL

#define TF_BT1886 1
#define TF_GAMMA22 2
#define TF_GAMMA28 3
#define TF_ST240 4
#define TF_EXT_LINEAR 5
#define TF_LOG100 6
#define TF_LOG316 7
#define TF_ST2084_PQ 11
#define TF_ST428 12
#define TF_COMPOUND_POWER_2_4 14
#define TF_POWER 15

#define ALPHA_PREMUL_ELECTRICAL 0
#define ALPHA_PREMUL_OPTICAL 1
#define ALPHA_STRAIGHT 2
#define YUV_NONE 0
// Client said nothing: BT.709 limited. Chroma sits on the left of the 2×2
// block and halfway between its luma rows (color-representation type 0).
// Mesa's EGL default is BT.601. `10` expands a P010 sample stored in the
// high 10 bits of a 16-bit UNORM.
#define YUV_BT709_LIMITED_8 1
#define YUV_BT709_LIMITED_10 2

#define COLOR_ALPHA_EPS 0.0001

// GLES sets these directly. Vulkan maps the same names onto the Draw block
// before including this file. Decode and encode each have their own BT.1886
// ratio and power exponent, because the two curves do not share coefficients
// and the source and destination transfer functions are independent.
#ifdef COLOR_LOOSE_UNIFORMS
uniform vec4 u_color_matrix_0;
uniform vec4 u_color_matrix_1;
uniform vec4 u_color_matrix_2;
uniform float u_src_bt1886_lb_over_lw;
uniform float u_dst_bt1886_lb_over_lw;
uniform float u_src_power_exponent;
uniform float u_dst_power_exponent;
uniform float u_tonemap_max;
uniform float u_tonemap_dst_max;
uniform float u_tonemap_dst_min;
uniform float u_tonemap_dst_ref;
uniform vec4 u_tonemap_to_lms_0;
uniform vec4 u_tonemap_to_lms_1;
uniform vec4 u_tonemap_to_lms_2;
uniform vec4 u_tonemap_from_lms_0;
uniform vec4 u_tonemap_from_lms_1;
uniform vec4 u_tonemap_from_lms_2;
#endif

float color_log10(float value) {
    return log(value) / log(10.0);
}

vec3 color_signed_pow(vec3 value, float exponent) {
    return sign(value) * pow(abs(value), vec3(exponent));
}

float color_channel_below(float value, float threshold, float below, float above) {
    if (value < threshold) {
        return below;
    }
    return above;
}

vec3 decode_bt1886(vec3 value) {
    // (L - Lb) / (Lw - Lb), matching src/color/tf.rs. Lb = 0 is signed V^2.4.
    if (u_src_bt1886_lb_over_lw <= 0.0) {
        return color_signed_pow(value, 2.4);
    }
    float lb_over_lw = u_src_bt1886_lb_over_lw;
    float gamma = 1.0 / 2.4;
    float a1 = 1.0 / (1.0 - lb_over_lw);
    float a2 = 1.0 - pow(lb_over_lw, gamma);
    float a3 = pow(lb_over_lw, gamma);
    vec3 lifted = a2 * value + vec3(a3);
    return vec3(a1) * (pow(max(lifted, vec3(0.0)), vec3(2.4)) - vec3(lb_over_lw));
}

vec3 encode_bt1886(vec3 value) {
    if (u_dst_bt1886_lb_over_lw <= 0.0) {
        return color_signed_pow(value, 1.0 / 2.4);
    }
    float lb_over_lw = u_dst_bt1886_lb_over_lw;
    float gamma = 1.0 / 2.4;
    float a1 = 1.0 / (1.0 - pow(lb_over_lw, gamma));
    float a2 = 1.0 - lb_over_lw;
    float a4 = pow(lb_over_lw, gamma);
    vec3 lifted = a2 * value + vec3(lb_over_lw);
    return vec3(a1) * (pow(max(lifted, vec3(0.0)), vec3(gamma)) - vec3(a4));
}

vec3 decode_compound(vec3 value) {
    return vec3(
        value.r <= 0.04045 ? value.r / 12.92 : pow((value.r + 0.055) / 1.055, 2.4),
        value.g <= 0.04045 ? value.g / 12.92 : pow((value.g + 0.055) / 1.055, 2.4),
        value.b <= 0.04045 ? value.b / 12.92 : pow((value.b + 0.055) / 1.055, 2.4)
    );
}

vec3 encode_compound(vec3 value) {
    return vec3(
        value.r <= 0.0031308 ? 12.92 * value.r : 1.055 * pow(value.r, 1.0 / 2.4) - 0.055,
        value.g <= 0.0031308 ? 12.92 * value.g : 1.055 * pow(value.g, 1.0 / 2.4) - 0.055,
        value.b <= 0.0031308 ? 12.92 * value.b : 1.055 * pow(value.b, 1.0 / 2.4) - 0.055
    );
}

vec3 decode_st240(vec3 value) {
    return vec3(
        color_channel_below(value.r, 0.0913, value.r / 4.0, pow((value.r + 0.1115) / 1.1115, 1.0 / 0.45)),
        color_channel_below(value.g, 0.0913, value.g / 4.0, pow((value.g + 0.1115) / 1.1115, 1.0 / 0.45)),
        color_channel_below(value.b, 0.0913, value.b / 4.0, pow((value.b + 0.1115) / 1.1115, 1.0 / 0.45))
    );
}

vec3 encode_st240(vec3 value) {
    return vec3(
        color_channel_below(value.r, 0.0913 / 4.0, 4.0 * value.r, 1.1115 * pow(value.r, 0.45) - 0.1115),
        color_channel_below(value.g, 0.0913 / 4.0, 4.0 * value.g, 1.1115 * pow(value.g, 0.45) - 0.1115),
        color_channel_below(value.b, 0.0913 / 4.0, 4.0 * value.b, 1.1115 * pow(value.b, 0.45) - 0.1115)
    );
}

float decode_log100_channel(float value) {
    if (value <= 0.0) {
        return 0.0;
    }
    return pow(10.0, (value - 1.0) * 2.0);
}

float encode_log100_channel(float value) {
    if (value < 0.01) {
        return 0.0;
    }
    return 1.0 + color_log10(value) / 2.0;
}

float decode_log316_channel(float value) {
    if (value <= 0.0) {
        return 0.0;
    }
    return pow(10.0, (value - 1.0) * 2.5);
}

float encode_log316_channel(float value) {
    if (value < sqrt(10.0) / 1000.0) {
        return 0.0;
    }
    return 1.0 + color_log10(value) / 2.5;
}

vec3 decode_log100(vec3 value) {
    return vec3(
        decode_log100_channel(value.r),
        decode_log100_channel(value.g),
        decode_log100_channel(value.b)
    );
}

vec3 encode_log100(vec3 value) {
    return vec3(
        encode_log100_channel(value.r),
        encode_log100_channel(value.g),
        encode_log100_channel(value.b)
    );
}

vec3 decode_log316(vec3 value) {
    return vec3(
        decode_log316_channel(value.r),
        decode_log316_channel(value.g),
        decode_log316_channel(value.b)
    );
}

vec3 encode_log316(vec3 value) {
    return vec3(
        encode_log316_channel(value.r),
        encode_log316_channel(value.g),
        encode_log316_channel(value.b)
    );
}

float decode_pq_channel(float value) {
    if (value <= 0.0) {
        return 0.0;
    }
    float p = pow(value, 1.0 / 78.84375);
    float numerator = max(p - 0.8359375, 0.0);
    float denominator = 18.8515625 - 18.6875 * p;
    return pow(numerator / denominator, 1.0 / 0.1593017578125);
}

float encode_pq_channel(float value) {
    float y = pow(max(value, 0.0), 0.1593017578125);
    return pow((0.8359375 + 18.8515625 * y) / (1.0 + 18.6875 * y), 78.84375);
}

vec3 decode_pq(vec3 value) {
    return vec3(decode_pq_channel(value.r), decode_pq_channel(value.g), decode_pq_channel(value.b));
}

vec3 encode_pq(vec3 value) {
    return vec3(encode_pq_channel(value.r), encode_pq_channel(value.g), encode_pq_channel(value.b));
}

vec3 decode_tf(int tf, vec3 value) {
    if (tf == TF_GAMMA22) {
        return color_signed_pow(value, 2.2);
    }
    if (tf == TF_GAMMA28) {
        return color_signed_pow(value, 2.8);
    }
    if (tf == TF_COMPOUND_POWER_2_4) {
        return decode_compound(value);
    }
    if (tf == TF_BT1886) {
        return decode_bt1886(value);
    }
    if (tf == TF_ST240) {
        return decode_st240(value);
    }
    if (tf == TF_EXT_LINEAR) {
        return value;
    }
    if (tf == TF_LOG100) {
        return decode_log100(value);
    }
    if (tf == TF_LOG316) {
        return decode_log316(value);
    }
    if (tf == TF_ST428) {
        return color_signed_pow(value, 2.6) * vec3(52.37 / 48.0);
    }
    if (tf == TF_ST2084_PQ) {
        return decode_pq(value);
    }
    if (tf == TF_POWER) {
        return color_signed_pow(value, u_src_power_exponent);
    }
    return value;
}

vec3 encode_tf(int tf, vec3 value) {
    if (tf == TF_GAMMA22) {
        return color_signed_pow(value, 1.0 / 2.2);
    }
    if (tf == TF_GAMMA28) {
        return color_signed_pow(value, 1.0 / 2.8);
    }
    if (tf == TF_COMPOUND_POWER_2_4) {
        return encode_compound(value);
    }
    if (tf == TF_BT1886) {
        return encode_bt1886(value);
    }
    if (tf == TF_ST240) {
        return encode_st240(value);
    }
    if (tf == TF_EXT_LINEAR) {
        return value;
    }
    if (tf == TF_LOG100) {
        return encode_log100(value);
    }
    if (tf == TF_LOG316) {
        return encode_log316(value);
    }
    if (tf == TF_ST428) {
        return color_signed_pow(value * vec3(48.0 / 52.37), 1.0 / 2.6);
    }
    if (tf == TF_ST2084_PQ) {
        return encode_pq(value);
    }
    if (tf == TF_POWER) {
        return color_signed_pow(value, 1.0 / u_dst_power_exponent);
    }
    return value;
}

vec3 apply_color_matrix(vec3 rgb) {
    vec4 value = vec4(rgb, 1.0);
    return vec3(
        dot(u_color_matrix_0, value),
        dot(u_color_matrix_1, value),
        dot(u_color_matrix_2, value)
    );
}

vec3 tonemap_mul(vec4 row0, vec4 row1, vec4 row2, vec3 value) {
    return vec3(dot(row0.xyz, value), dot(row1.xyz, value), dot(row2.xyz, value));
}

// Hyprland's default PQ ICtCp shoulder. `linear` is destination-normalized.
// The rows are the CPU's column-major LMS matrices.
vec3 tonemap_ictcp(vec3 linear) {
    float span = u_tonemap_dst_max - u_tonemap_dst_min;
    vec3 nits = linear * span + vec3(u_tonemap_dst_min);
    if (u_tonemap_max < u_tonemap_dst_max * 1.01) {
        nits = clamp(nits, vec3(0.0), vec3(u_tonemap_dst_max));
        return (nits - vec3(u_tonemap_dst_min)) / span;
    }
    vec3 lms = encode_pq(tonemap_mul(
        u_tonemap_to_lms_0,
        u_tonemap_to_lms_1,
        u_tonemap_to_lms_2,
        nits / 10000.0
    ));
    float i = dot(vec3(0.5, 0.5, 0.0), lms);
    float ct = dot(vec3(1.61376953125, -3.323486328125, 1.709716796875), lms);
    float cp = dot(vec3(4.378173828125, -4.24560546875, -0.132568359375), lms);
    float luminance = decode_pq_channel(clamp(i, 0.0, 1.0)) * 10000.0;
    float src_scale = u_tonemap_max / u_tonemap_dst_ref;
    float dst_scale = u_tonemap_dst_max / u_tonemap_dst_ref;
    float ratio = max(luminance / u_tonemap_dst_ref, 0.0);
    float v = (dst_scale * (1.0 + src_scale) - src_scale) / (src_scale * src_scale);
    float new_luminance = (ratio * (1.0 + ratio * v) / (1.0 + ratio)) * u_tonemap_dst_ref;
    i = encode_pq_channel(clamp(new_luminance / 10000.0, 0.0, 1.0));
    vec3 restored = decode_pq(vec3(
        dot(vec3(1.0, 0.0086090370379327566, 0.11102962500302595656), vec3(i, ct, cp)),
        dot(vec3(1.0, -0.0086090370379327566, -0.11102962500302595656), vec3(i, ct, cp)),
        dot(vec3(1.0, 0.560031335710679118, -0.32062717498731885185), vec3(i, ct, cp))
    ));
    nits = tonemap_mul(
        u_tonemap_from_lms_0,
        u_tonemap_from_lms_1,
        u_tonemap_from_lms_2,
        restored * 10000.0
    );
    nits = clamp(nits, vec3(0.0), vec3(u_tonemap_dst_max));
    return (nits - vec3(u_tonemap_dst_min)) / span;
}

// `needs_color_path == 0` keeps the sampled color, apart from straight alpha.
// Window opacity is applied by the caller after this returns.
vec4 apply_color(
    vec4 color,
    int needs_color_path,
    int src_tf,
    int dst_tf,
    int has_matrix,
    int alpha_mode,
    int src_has_alpha,
    int tonemap
) {
    if (needs_color_path == 1) {
        vec3 rgb = color.rgb;
        if (src_has_alpha == 1 && alpha_mode == ALPHA_PREMUL_ELECTRICAL) {
            rgb /= max(color.a, COLOR_ALPHA_EPS);
        }
        rgb = decode_tf(src_tf, rgb);
        if (src_has_alpha == 1 && alpha_mode == ALPHA_PREMUL_OPTICAL) {
            rgb /= max(color.a, COLOR_ALPHA_EPS);
        }
        if (has_matrix == 1) {
            rgb = apply_color_matrix(rgb);
        }
        if (tonemap == 1) {
            rgb = tonemap_ictcp(rgb);
        }
        rgb = encode_tf(dst_tf, rgb);
        if (src_has_alpha == 1) {
            rgb *= color.a;
        }
        color.rgb = rgb;
    } else if (alpha_mode == ALPHA_STRAIGHT && src_has_alpha == 1) {
        color.rgb *= color.a;
    }
    return color;
}

// `uv` is the luma texel coordinate. `site` is the chroma offset in luma pixels.
// Type 0 is (0, 0.5). The CPU builds the same offset.
vec2 yuv_chroma_coord(vec2 uv, vec2 luma_size, vec2 site) {
    vec2 luma_index = uv * luma_size - vec2(0.5);
    vec2 chroma_index = (luma_index - site) * vec2(0.5);
    vec2 chroma_size = luma_size * vec2(0.5);
    return (chroma_index + vec2(0.5)) / chroma_size;
}

// `y`, `cb`, and `cr` are UNORM samples. The rows are the CPU-built matrix,
// including range expansion. `vec4(y, cb, cr, 1)`.
vec3 yuv_to_rgb(float y, float cb, float cr, vec4 row0, vec4 row1, vec4 row2) {
    vec4 yuv_sample = vec4(y, cb, cr, 1.0);
    return vec3(dot(row0, yuv_sample), dot(row1, yuv_sample), dot(row2, yuv_sample));
}

#endif
