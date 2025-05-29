#version 450

layout(location = 0) in vec2 pixelCoord;
layout(location = 0) out vec4 color;

float v(in vec2 pixelCoordInt) {
    const vec2 r = mod(trunc(pixelCoordInt / 16.0), 2.0);

    return r.x == r.y ? 1.0 : 0.0;
}

void main() {
    const vec2 f = fract(pixelCoord);
    const float v00 = v(trunc(pixelCoord));
    const float v10 = v(trunc(pixelCoord + vec2(1.0, 0.0)));
    const float v01 = v(trunc(pixelCoord + vec2(0.0, 1.0)));
    const float v11 = v(trunc(pixelCoord + vec2(1.0, 1.0)));

    const float v = mix(mix(v00, v10, f.x), mix(v01, v11, f.x), f.y);
    color = mix(vec4(0.75, 0.75, 0.75, 1.0), vec4(0.5, 0.5, 0.5, 1.0), v);
}
