#version 450

layout(location = 0) in vec2 uv;

layout(push_constant) uniform PushConstants {
    vec2 pixelSize;
};

layout(set = 0, binding = 0) uniform Params {
    vec2 offset;
    vec2 size;
};

layout(location = 0) out vec4 color;

void main() {
    const vec2 uv1 = uv - offset / pixelSize;
    const vec2 lv0 = 1.0 - smoothstep(1.0 / pixelSize, 2.0 / pixelSize, abs(uv1));
    const float b0 = 1.0 - (1.0 - lv0.x) * (1.0 - lv0.y);

    const vec2 div = pixelSize / size;
    const vec2 xr = abs(fract(uv1 * div) - 0.5) * 2.0;
    const vec2 lv = smoothstep(vec2(1.0) - (vec2(1.0) / (pixelSize / div / vec2(2.0))), vec2(1.0), xr);
    const float b = 1.0 - (1.0 - lv.x) * (1.0 - lv.y);

    color = mix(vec4(0.1, 0.1, 0.15, 1.0), vec4(0.5, 0.5, 0.5, 1.0), 1.0 - (1.0 - b) * (1.0 - b0));
}
