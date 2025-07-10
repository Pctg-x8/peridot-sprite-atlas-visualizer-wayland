#version 450

layout(location = 0) in vec2 uv;
layout(location = 0) out float o;

void main() {
    const vec2 uv1 = 1.0 - uv;
    o = 1.0 - smoothstep(1.0, 1.0 + 1.0 / 16.0, length(uv1));
}
