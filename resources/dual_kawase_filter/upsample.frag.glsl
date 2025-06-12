#version 450

// https://community.arm.com/cfs-file/__key/communityserver-blogs-components-weblogfiles/00-00-00-20-66/siggraph2015_2D00_mmg_2D00_marius_2D00_notes.pdf

layout(location = 0) in vec4 uv_lr;
layout(location = 1) in vec4 uv_tb;
layout(location = 2) in vec4 uv_lt_rb;
layout(location = 3) in vec4 uv_lb_rt;

layout(set = 0, binding = 0) uniform sampler2D tex;

layout(location = 0) out vec4 color_out;

void main() {
    vec4 a = texture(tex, uv_lr.xy);
    a += texture(tex, uv_lt_rb.xy) * 2.0;
    a += texture(tex, uv_tb.xy);
    a += texture(tex, uv_lb_rt.zw) * 2.0;
    a += texture(tex, uv_lr.zw);
    a += texture(tex, uv_lt_rb.zw) * 2.0;
    a += texture(tex, uv_tb.zw);
    a += texture(tex, uv_lb_rt.xy) * 2.0;

    color_out = a / 12.0;
}
