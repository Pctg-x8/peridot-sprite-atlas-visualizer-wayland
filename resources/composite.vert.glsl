#version 450

layout(location = 0) in vec4 pos_st;
layout(location = 1) in vec4 uv_st;
layout(location = 2) in vec4 slice_borders;
layout(location = 3) in vec4 texSizePixels_compositeMode;
layout(location = 4) in vec4 colorTint;

layout(push_constant, std140) uniform PushConstants {
    vec2 screenSize;
};

layout(location = 0) out vec4 uv_compositeMode;
layout(location = 1) out vec4 uvOffset_texSizePixels;
layout(location = 2) out vec4 relativePixelCoord_renderSizePixels;
layout(location = 3) out vec4 sliceBordersLTRB;
layout(location = 4) out vec4 colorTintOut;
layout(location = 5) out vec4 texSlicedSizePixels;

void main() {
    const vec2 p = vec2((gl_VertexIndex & 0x01) == 0 ? 0.0 : 1.0, (gl_VertexIndex & 0x02) == 0 ? 0.0 : 1.0);

    relativePixelCoord_renderSizePixels = vec4(p * pos_st.xy, pos_st.xy);
    uvOffset_texSizePixels = vec4(uv_st.zw, texSizePixels_compositeMode.xy);
    gl_Position = vec4((relativePixelCoord_renderSizePixels.xy + pos_st.zw) * 2.0 / screenSize - 1.0, 0.0, 1.0);
    uv_compositeMode = vec4(p * uv_st.xy + uvOffset_texSizePixels.xy, texSizePixels_compositeMode.z, 0.0);
    sliceBordersLTRB = slice_borders;
    colorTintOut = colorTint;
    texSlicedSizePixels = vec4(texSizePixels_compositeMode.xy * uv_st.xy, 0.0f, 0.0f);
}
