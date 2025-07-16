#version 450

#define FRAGMENT_SHADER
#include "composite.resources.glsl"

layout(location = 0) out vec4 col_out;

float rel_uv_9s_axis(in float relativePixelCoord, in float renderSizePixels, in float texSizePixels, in float sliceBorderLT, in float sliceBorderRB, in float texSlicedSizePixels) {
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
    const float uvSizeInTex = (texSlicedSizePixels - sliceBorderRB - sliceBorderLT) / texSizePixels;
    const float uvOffset = sliceBorderLT / texSizePixels;

    return uvOffset + r * uvSizeInTex;
}

vec4 tex_9s(in vec2 relativePixelCoord, in vec2 renderSizePixels, in vec2 uvOffset, in vec2 texSizePixels, in vec4 sliceBordersLTRB, in vec2 texSlicedSizePixels) {
    const vec2 uv = vec2(
        rel_uv_9s_axis(relativePixelCoord.x, renderSizePixels.x, texSizePixels.x, sliceBordersLTRB.x, sliceBordersLTRB.z, texSlicedSizePixels.x),
        rel_uv_9s_axis(relativePixelCoord.y, renderSizePixels.y, texSizePixels.y, sliceBordersLTRB.y, sliceBordersLTRB.w, texSlicedSizePixels.y)
    );

    return texture(tex, uvOffset + uv);
}

void main() {
    if (rectMaskInScreenUV.x > screenUV.x || screenUV.x > rectMaskInScreenUV.z || rectMaskInScreenUV.y > screenUV.y || screenUV.y > rectMaskInScreenUV.w) {
        // out of mask
        discard;
    }

    if (uv_compositeMode_opacity.z == 2.0 || uv_compositeMode_opacity.z == 4.0) {
        // no texture mapping
        col_out = colorTint;
    } else if (sliceBordersLTRB == vec4(0.0f)) {
        // no 9slices
        col_out = texture(tex, uv_compositeMode_opacity.xy);
    } else {
        col_out = tex_9s(relativePixelCoord_renderSizePixels.xy, relativePixelCoord_renderSizePixels.zw, uvOffset_texSizePixels.xy, uvOffset_texSizePixels.zw, sliceBordersLTRB, texSlicedSizePixels.xy);
    }

    if (uv_compositeMode_opacity.z == 1.0 || uv_compositeMode_opacity.z == 3.0) {
        // input is r8 format
        col_out = colorTint * vec4(1.0, 1.0, 1.0, col_out.r);
    }

    const float soft_alpharate = min(
        min(
            clamp(screenUV.x / rectMaskSoftnessInScreenUV.x, 0.0, 1.0),
            clamp((rectMaskInScreenUV.z - screenUV.x) / rectMaskSoftnessInScreenUV.z, 0.0, 1.0)
        ),
        min(
            clamp(screenUV.y / rectMaskSoftnessInScreenUV.y, 0.0, 1.0),
            clamp((rectMaskInScreenUV.w - screenUV.y) / rectMaskSoftnessInScreenUV.w, 0.0, 1.0)
        )
    );
    col_out.a *= soft_alpharate;

    // apply opacity and premultiply
    col_out.a *= uv_compositeMode_opacity.w;
    col_out.rgb *= col_out.a;

    if (col_out.a > 0.0 && (uv_compositeMode_opacity.z == 3.0 || uv_compositeMode_opacity.z == 4.0)) {
        // blend with backdrop(forced alpha = 1)
        const vec4 backdrop = texture(backdrop_tex, screenUV.xy);
        col_out = vec4(col_out.rgb + backdrop.rgb * (1.0 - col_out.a), 1.0);
    }
}
