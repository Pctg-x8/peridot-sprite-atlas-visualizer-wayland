#version 450

// https://community.arm.com/cfs-file/__key/communityserver-blogs-components-weblogfiles/00-00-00-20-66/siggraph2015_2D00_mmg_2D00_marius_2D00_notes.pdf

layout(location = 0) in vec2 uv_center;
layout(location = 1) in vec4 uv_lb_rt;
layout(location = 2) in vec4 uv_lt_rb;

layout(set = 0, binding = 0) uniform sampler2D tex;

layout(location = 0) out vec4 color_out;

void main() {
    vec4 a = texture(tex, uv_center) * 4.0;
    a += texture(tex, uv_lb_rt.xy);
    a += texture(tex, uv_lb_rt.zw);
    a += texture(tex, uv_lt_rb.xy);
    a += texture(tex, uv_lt_rb.zw);

    color_out = a * 0.125;
}
