.SUFFIXES: .vert.glsl .frag.glsl .vert .frag

all: $(patsubst %.vert.glsl,%.vert,$(wildcard *.vert.glsl)) $(patsubst %.frag.glsl,%.frag,$(wildcard *.frag.glsl))

%.vert: %.vert.glsl
	glslc -fshader-stage=vert -o $@ $<

%.frag: %.frag.glsl
	glslc -fshader-stage=frag -o $@ $<
