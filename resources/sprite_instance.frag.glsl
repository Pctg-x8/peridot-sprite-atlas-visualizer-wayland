#version 450

layout(location = 0) in vec2 uv;

layout(set = 1, binding = 0) uniform sampler2D tex;

layout(location = 0) out vec4 color_out;

void main() {
    color_out = texture(tex, uv);
    color_out.rgb *= color_out.a;
}
