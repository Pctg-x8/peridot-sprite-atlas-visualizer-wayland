#version 450

layout(constant_id = 0) const uint MaxCount = 3;

layout(location = 0) in vec2 uv;

layout(push_constant) uniform PushConstants {
    vec4 _vsh_;
    vec4 uv_limit;
    vec2 uv_step;
    float factors[MaxCount];
};
layout(set = 0, binding = 0) uniform sampler2D tex;

layout(location = 0) out vec4 color;

vec4 smp(in vec2 uv) {
    if (uv.x < uv_limit.x || uv_limit.z < uv.x) {
        return vec4(0.0f);
    }
    if (uv.y < uv_limit.y || uv_limit.w < uv.y) {
        return vec4(0.0f);
    }

    return texture(tex, uv);
}

void main() {
    color = vec4(0.0f);
    for (uint i = 0; i < MaxCount; i++) {
        color += (smp(uv + uv_step * i) + smp(uv - uv_step * i)) * 0.5 * factors[i];
    }
}
