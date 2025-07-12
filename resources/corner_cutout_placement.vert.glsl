#version 450

layout(constant_id = 0) const float width_vp = 16.0;
layout(constant_id = 1) const float height_vp = 16.0;
layout(constant_id = 2) const float uv_scale_x = 1.0;
layout(constant_id = 3) const float uv_scale_y = 1.0;
layout(constant_id = 4) const float uv_trans_x = 1.0;
layout(constant_id = 5) const float uv_trans_y = 1.0;

layout(location = 0) in vec2 relative_base;
layout(location = 1) in vec2 direction;

layout(location = 0) out vec2 uv;

void main() {
    const vec2 base_01 = vec2((gl_VertexIndex & 0x01) == 0 ? 0.0 : 1.0, (gl_VertexIndex & 0x02) == 0 ? 0.0 : 1.0);

    uv = fma(base_01, vec2(uv_scale_x, uv_scale_y), vec2(uv_trans_x, uv_trans_y));
    gl_Position = vec4(fma(vec2(width_vp, height_vp) * base_01, direction, relative_base), 0.0, 1.0);
}
