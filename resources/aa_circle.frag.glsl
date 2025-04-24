#version 450

layout(constant_id = 0) const float softness = 0.0;

layout(location = 0) in vec2 uv;

layout(location = 0) out vec4 color;

void main() {
    const float d = length(uv - vec2(0.5)) * 2.0;
    color = vec4(1.0 - smoothstep(1.0 - softness, 1.0, d));
}
