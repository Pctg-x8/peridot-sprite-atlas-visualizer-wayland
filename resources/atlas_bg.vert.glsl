#version 450

layout(location = 0) in vec4 posPixels;

layout(push_constant) uniform PushConstants {
    vec2 pixelSize;
};

layout(set = 0, binding = 0) uniform Params {
    vec2 offset;
    vec2 _size;
};

layout(location = 0) out vec2 pixelCoord;

void main() {
    pixelCoord = posPixels.xy;
    gl_Position.xy = 2.0 * (posPixels.xy + offset) / pixelSize - 1.0;
    gl_Position.zw = vec2(0.0, 1.0);
}
