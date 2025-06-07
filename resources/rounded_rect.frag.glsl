#version 450

layout(constant_id = 0) const float CornerRadius = 24.0;

layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 col_out;

float d_circle(vec2 p, vec2 center, float radius) {
    return 1.0 - smoothstep(radius - 0.5, radius + 0.5, length(center - p));
}

void main() {
    const float RenderSize = CornerRadius * 2.0 + 1.0;
    const vec2 uvPixels = uv * RenderSize;

    const float edge1 = d_circle(uvPixels, vec2(CornerRadius, CornerRadius), CornerRadius);
    const float edge2 = d_circle(uvPixels, vec2(RenderSize - CornerRadius, CornerRadius), CornerRadius);
    const float edge3 = d_circle(uvPixels, vec2(CornerRadius, RenderSize - CornerRadius), CornerRadius);
    const float edge4 = d_circle(uvPixels, vec2(RenderSize - CornerRadius, RenderSize - CornerRadius), CornerRadius);

    col_out = vec4(max(max(edge1, edge2), max(edge3, edge4)), 0.0, 0.0, 1.0);
}
