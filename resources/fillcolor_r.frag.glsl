#version 450

layout(location = 0) out vec4 color;
layout(constant_id = 0) const float r = 0.0;

void main() {
    color = vec4(r);
}
