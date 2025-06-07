#version 450

layout(location = 0) in vec2 uv;

void main() {
    // https://www.microsoft.com/en-us/research/wp-content/uploads/2005/01/p1000-loop.pdf
    const float x = uv.x * uv.x - uv.y;
    if (x >= 0.0) discard;
}
