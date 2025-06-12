#version 450

// https://community.arm.com/cfs-file/__key/communityserver-blogs-components-weblogfiles/00-00-00-20-66/siggraph2015_2D00_mmg_2D00_marius_2D00_notes.pdf

layout(push_constant, std140) uniform Params {
    vec2 texel_size;
    float offset_scale;
};

layout(location = 0) out vec4 uv_lr;
layout(location = 1) out vec4 uv_tb;
layout(location = 2) out vec4 uv_lt_rb;
layout(location = 3) out vec4 uv_lb_rt;

void main() {
    const vec2 p = vec2(gl_VertexIndex != 1 ? 0.0 : 2.0, gl_VertexIndex != 2 ? 0.0 : 2.0);
    const vec2 half_pixel = texel_size * 0.5;

    uv_lr.xy = p - vec2(half_pixel.x, 0.0) * 2.0 * offset_scale;
    uv_lr.zw = p + vec2(half_pixel.x, 0.0) * 2.0 * offset_scale;
    uv_tb.xy = p - vec2(0.0, half_pixel.y) * 2.0 * offset_scale;
    uv_tb.zw = p + vec2(0.0, half_pixel.y) * 2.0 * offset_scale;
    uv_lb_rt.xy = p + half_pixel * offset_scale;
    uv_lb_rt.zw = p - half_pixel * offset_scale;
    uv_lt_rb.xy = p + vec2(half_pixel.x, -half_pixel.y) * offset_scale;
    uv_lt_rb.zw = p - vec2(half_pixel.x, -half_pixel.y) * offset_scale;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
