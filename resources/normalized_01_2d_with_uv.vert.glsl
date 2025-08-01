#version 450

layout(location = 0) in vec2 pos;
layout(location = 1) in vec2 uv;

layout(location = 0) out vec2 uv_out;

void main() {
    gl_Position = vec4(pos * 2.0 - 1.0, 0.0, 1.0);
    uv_out = uv;
}
