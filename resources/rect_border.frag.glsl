#version 450

layout(constant_id = 0) const float RenderSize = 16.0;
layout(constant_id = 1) const float Thickness = 1.0;

layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 col_out;

void main() {
    const vec2 uvpx = uv * RenderSize;
    const float edge_left = 1.0 - smoothstep(Thickness - 0.5, Thickness + 0.5, uvpx.x);
    const float edge_right = smoothstep(RenderSize - Thickness - 0.5, RenderSize - Thickness + 0.5, uvpx.x);
    const float edge_top = 1.0 - smoothstep(Thickness - 0.5, Thickness + 0.5, uvpx.y);
    const float edge_bottom = smoothstep(RenderSize - Thickness - 0.5, RenderSize - Thickness + 0.5, uvpx.y);
    const float d = max(max(edge_left, edge_right), max(edge_top, edge_bottom));

    col_out = vec4(d);
}
