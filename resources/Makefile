.SUFFIXES: .vert.glsl .frag.glsl .vert .frag

RESULT_SETS = $(patsubst %.vert.glsl,%.vert,$(wildcard *.vert.glsl)) \
	$(patsubst %.frag.glsl,%.frag,$(wildcard *.frag.glsl)) \
	$(patsubst %.vert.glsl,%.vert,$(wildcard dual_kawase_filter/*.vert.glsl)) \
	$(patsubst %.frag.glsl,%.frag,$(wildcard dual_kawase_filter/*.frag.glsl))

all: $(RESULT_SETS)

%.vert: %.vert.glsl
	glslc -fshader-stage=vert -o $@ $<

%.frag: %.frag.glsl
	glslc -fshader-stage=frag -o $@ $<

composite.vert: composite.vert.glsl composite.resources.glsl
	glslc -fshader-stage=vert -o composite.vert composite.vert.glsl

composite.frag: composite.frag.glsl composite.resources.glsl
	glslc -fshader-stage=frag -o composite.frag composite.frag.glsl
