#version 450

layout(push_constant, std140) uniform PushConstants {
    vec2 screenSize;
};

struct CompositeInstanceData {
    /// scale_x(width), scale_y(height), translate_x(left), translate_y(top)
    vec4 pos_st;
    vec4 uv_st;
    mat4 position_modifier_matrix;
    /// left, top, right, bottom (pixels from edge)
    vec4 slice_borders;
    /// tex_size_w_px, tex_size_h_px, composite_mode, opacity
    vec4 tex_size_pixels_composite_mode_opacity;
    vec4 color_tint;
    /// start_sec, end_sec, to_value(fromはpos_stに設定されている値), reserved
    vec4 pos_x_animation_data;
    /// x_p1x, x_p1y, x_p2x, x_p2y
    vec4 pos_x_curve_control_points;
    /// start_sec, end_sec, to_value(fromはpos_stに設定されている値), reserved
    vec4 pos_y_animation_data;
    /// y_p1x, y_p1y, y_p2x, y_p2y
    vec4 pos_y_curve_control_points;
    /// start_sec, end_sec, to_value(fromはpos_stに設定されている値), reserved
    vec4 pos_width_animation_data;
    /// w_p1x, w_p1y, w_p2x, w_p2y
    vec4 pos_width_curve_control_points;
    /// start_sec, end_sec, to_value(fromはpos_stに設定されている値), reserved
    vec4 pos_height_animation_data;
    /// h_p1x, h_p1y, h_p2x, h_p2y
    vec4 pos_height_curve_control_points;
};

layout(set = 0, binding = 0, std140) readonly buffer InstanceDataArray {
    CompositeInstanceData instanceDataArray[];
};
layout(set = 0, binding = 1, std140) uniform StreamingData {
    float current_sec;
};

layout(location = 0) out vec4 uv_compositeMode_opacity;
layout(location = 1) out vec4 uvOffset_texSizePixels;
layout(location = 2) out vec4 relativePixelCoord_renderSizePixels;
layout(location = 3) out vec4 sliceBordersLTRB;
layout(location = 4) out vec4 colorTintOut;
layout(location = 5) out vec4 texSlicedSizePixels;

float interpolate_curve(in vec2 p1, in vec2 p2, in float x) {
    // p01 = mix(vec2(0.0), p1, t) = p1 * t
    // p12 = mix(p1, p2, t) = p1 * (1.0 - t) + p2 * t
    // p23 = mix(p2, vec2(1.0), t) = p2 * (1.0 - t) + vec2(t)
    // p012 = mix(p01, p12, t) = p01 * (1.0 - t) + p12 * t = p1 * t * (1.0 - t) + (p1 * (1.0 -t ) + p2 * t) * t =
    // p1 * t * (1.0 - t) + p1 * t * (1.0 - t) + p2 * t * t = p1 * 2.0 * t * (1.0 - t) + p2 * t * t =
    // p1 * (2.0 * t - 2.0 * t * t) + p2 * t * t
    // p123 = mix(p12, p23, t) = p12 * (1.0 - t) + p23 * t = (p1 * (1.0 - t) + p2 * t) * (1.0 - t) + (p2 * (1.0 - t) + vec2(t)) * t =
    // p1 * (1.0 - t) * (1.0 - t) + p2 * t * (1.0 - t) + p2 * (1.0 - t) * t  + vec2(t * t) =
    // p1 * (1.0 - t) * (1.0 - t) + p2 * 2.0 * t * (1.0 - t) + vec2(t * t) =
    // p1 * (1.0 - t) * (1.0 - t) + p2 * (2.0 * t - 2.0 * t * t) + vec2(t * t)
    // p = mix(p012, p123, t) = p012 * (1.0 - t) + p123 * t =
    // (p1 * (2.0 * t - 2.0 * t * t) + p2 * t * t) * (1.0 - t) + (p1 * (1.0 - t) * (1.0 - t) + p2 * (2.0 * t - 2.0 * t * t) + vec2(t * t)) * t =
    // p1 * (2.0 * t - 2.0 * t * t) * (1.0 - t) + p2 * t * t * (1.0 - t) + p1 * t * (1.0 - t) * (1.0 - t) + p2 * t * (2.0 * t - 2.0 * t * t) + vec2(t * t * t) =
    // p1 * 2.0 * t * (1.0 - t) * (1.0 - t) + p2 * t * t * (1.0 - t) + p1 * t * (1.0 - t) * (1.0 - t) + p2 * 2.0 * t * t * (1.0 - t) + vec2(t * t * t) =
    // p1 * 3.0 * t * (1.0 - t) * (1.0 - t) + p2 * 3.0 * t * t * (1.0 - t) + vec2(t * t * t)
    //
    // (1.0 - t)^2 = 1.0^2 - 2.0 * t + t^2
    //
    // x = (p1.x * 3.0 * t * (1.0 - t) * (1.0 - t) + p2.x * 3.0 * t * t * (1.0 - t) + t * t * t), t = ?
    // x = p1.x * (3.0 * t - 6.0 * t^2 + 3.0 * t^3) + p2.x * (3.0 * t^2 - 3.0 * t^3) + t^3
    // x = (p1.x * 3.0 - p2.x * 3.0 + 1.0) * t^3 + (-p1.x * 6.0 + p2.x * 3.0) * t^2 + p1.x * 3.0 * t
    // 0 = (p1.x * 3.0 - p2.x * 3.0 + 1.0) * t^3 + (-p1.x * 6.0 + p2.x * 3.0) * t^2 + p1.x * 3.0 * t - x

    // x = (p1.x * 3.0 - p2.x * 3.0 + 1.0) * t^3 + (p2.x * 3.0 - p1.x * 6.0) * t^2 + p1.x * 3.0 * t
    // t = ?
    const float a = p1.x * 3.0f - p2.x * 3.0f + 1.0f;
    const float b = p2.x * 3.0f - p1.x * 6.0f;
    const float c = p1.x * 3.0f;
    const float d = -x;

    float t;
    if (a == 0) {
        // solve quadratic: (p2.x * 3.0 - p1.x * 6.0) * t^2 + p1.x * 3.0 * t - x = 0
        const float dq = c * c - 4.0f * b * d;
        if (dq < 0.0f) {
            // no value
            return 0.0f;
        } else if (dq == 0.0f) {
            // exactly one
            t = -c / 2.0f * b;
        } else {
            // select correct value
            const float t1 = -c + sqrt(dq) / 2.0f * b;
            const float t2 = -c - sqrt(dq) / 2.0f * b;

            t = 0.0f <= t1 && t1 <= 1.0f ? t1 :
                0.0f <= t2 && t2 <= 1.0f ? t2 :
                // outside all
                clamp(t1, 0.0f, 1.0f);
        }
    } else {
        // solve cubic: https://peter-shepherd.com/personal_development/mathematics/polynomials/cubicAlgebra.htm
        const float a1 = b / a;
        const float b1 = c / a;
        const float c1 = d / a;
        const float p = (3.0f * b1 - a1 * a1) / 3.0f;
        const float q = (2.0f * a1 * a1 * a1 - 9.0f * a1 * b1 + 27.0f * c1) / 27.0f;

        if (p == 0.0f) {
            if (q == 0.0f) {
                t = 0.0f;
            } else {
                const float t1 = pow(-q, 1.0f / 3.0f) - a1 / 3.0f;
                const float t2 = pow(-q, 1.0f / 3.0f) * (-0.5f + sqrt(3.0f) / 2.0f) - a1 / 3.0f;
                const float t3 = pow(-q, 1.0f / 3.0f) * (-0.5f - sqrt(3.0f) / 2.0f) - a1 / 3.0f;

                t = 0.0f <= t1 && t1 <= 1.0f ? t1 :
                    0.0f <= t2 && t2 <= 1.0f ? t2 :
                    0.0f <= t3 && t3 <= 1.0f ? t3 :
                    // outside all
                    clamp(t1, 0.0f, 1.0f);
            }
        } else {
            if (q == 0.0f) {
                const float t1 = -a1 / 3.0f;
                const float t2 = sqrt(-p) - a1 / 3.0f;
                const float t3 = -sqrt(-p) - a1 / 3.0f;

                t = 0.0f <= t1 && t1 <= 1.0f ? t1 :
                    0.0f <= t2 && t2 <= 1.0f ? t2 :
                    0.0f <= t3 && t3 <= 1.0f ? t3 :
                    // outside all
                    clamp(t1, 0.0f, 1.0f);
            } else {
                const float dc = (q * q) / 4.0f + (p * p * p) / 27.0f;

                if (dc == 0.0f) {
                    // two reals
                    const float t1 = 2.0f * pow(-q / 2.0f, 1.0f / 3.0f) - a1 / 3.0f;
                    const float t2 = pow(q / 2.0f, 1.0f / 3.0f) - a1 / 3.0f;

                    t = 0.0f <= t1 && t1 <= 1.0f ? t1 :
                        0.0f <= t2 && t2 <= 1.0f ? t2 :
                        // outside all
                        clamp(t1, 0.0f, 1.0f);
                } else if (dc > 0.0f) {
                    // one real and two img
                    const float u1 = pow(-(q / 2.0f) + sqrt(dc), 1.0f / 3.0f);
                    const float v1 = pow(q / 2.0f + sqrt(dc), 1.0f / 3.0f);

                    const float t1 = u1 - v1 - a1 / 3.0f;
                    const float t2 = -0.5 * (u1 - v1) + (u1 + v1) * sqrt(3.0f) / 2.0f - a1 / 3.0f;
                    const float t3 = -0.5 * (u1 - v1) - (u1 + v1) * sqrt(3.0f) / 2.0f - a1 / 3.0f;

                    t = 0.0f <= t1 && t1 <= 1.0f ? t1 :
                        0.0f <= t2 && t2 <= 1.0f ? t2 :
                        0.0f <= t3 && t3 <= 1.0f ? t3 :
                        // outside all
                        clamp(t1, 0.0f, 1.0f);
                } else {
                    // irreducible case
                    const float r = sqrt(pow(-p / 3.0f, 3.0f));
                    const float phi = acos(-q / (2.0f * r));

                    const float t1 = 2.0f * pow(r, 1.0f / 3.0f) * cos(phi / 3.0f) - a1 / 3.0f;
                    const float t2 = 2.0f * pow(r, 1.0f / 3.0f) * cos((phi + 2.0f * 3.1415926) / 3.0f) - a1 / 3.0f;
                    const float t3 = 2.0f * pow(r, 1.0f / 3.0f) * cos((phi + 4.0f * 3.1415926) / 3.0f) - a1 / 3.0f;

                    t = 0.0f <= t1 && t1 <= 1.0f ? t1 :
                        0.0f <= t2 && t2 <= 1.0f ? t2 :
                        0.0f <= t3 && t3 <= 1.0f ? t3 :
                        // outside all
                        clamp(t1, 0.0f, 1.0f);
                }
            }
        }
    }

    // y = (p1.y * 3.0 - p2.y * 3.0 + 1.0) * t^3 + (p2.y * 3.0 - p1.y * 6.0) * t^2 + p1.y * 3.0 * t

    return (p1.y * 3.0f - p2.y * 3.0f + 1.0f) * t * t * t + (p2.y * 3.0f - p1.y * 6.0f) * t * t + p1.y * 3.0f * t;
}
float time_remap(in float start, in float end, in float t) {
    return clamp((t - start) / (end - start), 0.0, 1.0);
}
float map_value(in float start, in float end, in float r) {
    return start * (1.0 - r) + end * r;
}

void main() {
    const vec2 p = vec2((gl_VertexIndex & 0x01) == 0 ? 0.0 : 1.0, (gl_VertexIndex & 0x02) == 0 ? 0.0 : 1.0);

    const vec4 pos_st = vec4(
        instanceDataArray[gl_InstanceIndex].pos_width_animation_data.x == instanceDataArray[gl_InstanceIndex].pos_width_animation_data.y
            ? instanceDataArray[gl_InstanceIndex].pos_st.x
            : map_value(instanceDataArray[gl_InstanceIndex].pos_st.x, instanceDataArray[gl_InstanceIndex].pos_width_animation_data.z, interpolate_curve(instanceDataArray[gl_InstanceIndex].pos_width_curve_control_points.xy, instanceDataArray[gl_InstanceIndex].pos_width_curve_control_points.zw, time_remap(instanceDataArray[gl_InstanceIndex].pos_width_animation_data.x, instanceDataArray[gl_InstanceIndex].pos_width_animation_data.y, current_sec))),
        instanceDataArray[gl_InstanceIndex].pos_height_animation_data.x == instanceDataArray[gl_InstanceIndex].pos_height_animation_data.y
            ? instanceDataArray[gl_InstanceIndex].pos_st.y
            : map_value(instanceDataArray[gl_InstanceIndex].pos_st.y, instanceDataArray[gl_InstanceIndex].pos_height_animation_data.z, interpolate_curve(instanceDataArray[gl_InstanceIndex].pos_height_curve_control_points.xy, instanceDataArray[gl_InstanceIndex].pos_height_curve_control_points.zw, time_remap(instanceDataArray[gl_InstanceIndex].pos_height_animation_data.x, instanceDataArray[gl_InstanceIndex].pos_height_animation_data.y, current_sec))),
        instanceDataArray[gl_InstanceIndex].pos_x_animation_data.x == instanceDataArray[gl_InstanceIndex].pos_x_animation_data.y
            ? instanceDataArray[gl_InstanceIndex].pos_st.z
            : map_value(instanceDataArray[gl_InstanceIndex].pos_st.z, instanceDataArray[gl_InstanceIndex].pos_x_animation_data.z, interpolate_curve(instanceDataArray[gl_InstanceIndex].pos_x_curve_control_points.xy, instanceDataArray[gl_InstanceIndex].pos_x_curve_control_points.zw, time_remap(instanceDataArray[gl_InstanceIndex].pos_x_animation_data.x, instanceDataArray[gl_InstanceIndex].pos_x_animation_data.y, current_sec))),
        instanceDataArray[gl_InstanceIndex].pos_y_animation_data.x == instanceDataArray[gl_InstanceIndex].pos_y_animation_data.y
            ? instanceDataArray[gl_InstanceIndex].pos_st.w
            : map_value(instanceDataArray[gl_InstanceIndex].pos_st.w, instanceDataArray[gl_InstanceIndex].pos_y_animation_data.z, interpolate_curve(instanceDataArray[gl_InstanceIndex].pos_y_curve_control_points.xy, instanceDataArray[gl_InstanceIndex].pos_y_curve_control_points.zw, time_remap(instanceDataArray[gl_InstanceIndex].pos_y_animation_data.x, instanceDataArray[gl_InstanceIndex].pos_y_animation_data.y, current_sec)))
    );

    relativePixelCoord_renderSizePixels = vec4(p * pos_st.xy, pos_st.xy);
    vec4 pos4 = instanceDataArray[gl_InstanceIndex].position_modifier_matrix * vec4(relativePixelCoord_renderSizePixels.xy, 0.0, 1.0) + vec4(pos_st.zw, 0.0, 0.0);
    gl_Position = vec4(
        pos4.xy * 2.0 / screenSize - 1.0,
        0.0,
        1.0
    );
    uvOffset_texSizePixels = vec4(
        instanceDataArray[gl_InstanceIndex].uv_st.zw,
        instanceDataArray[gl_InstanceIndex].tex_size_pixels_composite_mode_opacity.xy
    );
    uv_compositeMode_opacity = vec4(
        p * instanceDataArray[gl_InstanceIndex].uv_st.xy + uvOffset_texSizePixels.xy,
        instanceDataArray[gl_InstanceIndex].tex_size_pixels_composite_mode_opacity.zw
    );
    sliceBordersLTRB = instanceDataArray[gl_InstanceIndex].slice_borders;
    colorTintOut = instanceDataArray[gl_InstanceIndex].color_tint;
    texSlicedSizePixels = vec4(uvOffset_texSizePixels.zw * instanceDataArray[gl_InstanceIndex].uv_st.xy, 0.0f, 0.0f);
}
