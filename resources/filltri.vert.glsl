#version 450

layout(location = 0) out vec2 uv;

void main() {
    const vec2 p = vec2(gl_VertexIndex != 1 ? 0.0 : 2.0, gl_VertexIndex != 2 ? 0.0 : 2.0);

    uv = p;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
