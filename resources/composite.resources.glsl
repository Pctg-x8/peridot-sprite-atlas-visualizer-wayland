
#ifdef VERTEX_SHADER
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
#endif

#ifdef FRAGMENT_SHADER
layout(set = 0, binding = 2) uniform sampler2D tex;
layout(set = 1, binding = 0) uniform sampler2D backdrop_tex;
#endif
