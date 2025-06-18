#version 450

layout(location = 0) in vec4 pos_st;
layout(location = 1) in vec4 uv_st;

layout(push_constant) uniform PushConstant {
    vec2 rtSizePixels;
};

layout(set = 0, binding = 0) uniform Params {
    vec2 offset;
    vec2 _size;
};

layout(location = 0) out vec2 uv;

void main() {
    const vec2 normalized_pos = vec2((gl_VertexIndex & 0x01) == 0 ? 0.0 : 1.0, (gl_VertexIndex & 0x02) == 0 ? 0.0 : 1.0);

    gl_Position = vec4((fma(normalized_pos, pos_st.xy, pos_st.zw) + offset) * 2.0 / rtSizePixels - 1.0, 0.0, 1.0);
    uv = fma(normalized_pos, uv_st.xy, uv_st.zw);
}
