#version 450

layout(location = 0) in vec4 uv_compositeMode;
layout(location = 1) in vec4 uvOffset_texSizePixels;
layout(location = 2) in vec4 relativePixelCoord_renderSizePixels;
layout(location = 3) in vec4 sliceBordersLTRB;
layout(location = 4) in vec4 colorTint;

layout(set = 0, binding = 0) uniform sampler2D tex;

layout(location = 0) out vec4 col_out;

float rel_uv_9s_axis(in float relativePixelCoord, in float renderSizePixels, in float texSizePixels, in float sliceBorderLT, in float sliceBorderRB) {
    if (relativePixelCoord < sliceBorderLT) {
        const float r = relativePixelCoord / sliceBorderLT;
        const float sliceBorderUVSizeInTex = sliceBorderLT / texSizePixels;

        return r * sliceBorderUVSizeInTex;
    }

    if (renderSizePixels - relativePixelCoord < sliceBorderRB) {
        const float r = (renderSizePixels - relativePixelCoord) / sliceBorderRB;
        const float sliceBorderUVSizeInTex = sliceBorderRB / texSizePixels;

        return r * sliceBorderUVSizeInTex;
    }

    const float r = (relativePixelCoord - sliceBorderLT) / (renderSizePixels - sliceBorderRB - sliceBorderLT);
    const float uvSizeInTex = (texSizePixels - sliceBorderRB - sliceBorderLT) / texSizePixels;
    const float uvOffset = sliceBorderLT / texSizePixels;

    return uvOffset + r * uvSizeInTex;
}

vec4 tex_9s(in vec2 relativePixelCoord, in vec2 renderSizePixels, in vec2 uvOffset, in vec2 texSizePixels, in vec4 sliceBordersLTRB) {
    const vec2 uv = vec2(
        rel_uv_9s_axis(relativePixelCoord.x, renderSizePixels.x, texSizePixels.x, sliceBordersLTRB.x, sliceBordersLTRB.z),
        rel_uv_9s_axis(relativePixelCoord.y, renderSizePixels.y, texSizePixels.y, sliceBordersLTRB.y, sliceBordersLTRB.w)
    );

    return texture(tex, uvOffset + uv);
}

void main() {
    if (sliceBordersLTRB == vec4(0.0f)) {
        // no 9slices
        col_out = texture(tex, uv_compositeMode.xy);
    } else {
        col_out = tex_9s(relativePixelCoord_renderSizePixels.xy, relativePixelCoord_renderSizePixels.zw, uvOffset_texSizePixels.xy, uvOffset_texSizePixels.zw, sliceBordersLTRB);
    }

    if (uv_compositeMode.z == 1.0) {
        // input is r8 format
        col_out = col_out.r * colorTint;
    }

    // premultiply
    col_out.rgb *= col_out.a;
}
