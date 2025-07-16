use std::{collections::HashMap, path::Path};

use crate::{
    BLEND_STATE_SINGLE_NONE, FillcolorRConstants, IA_STATE_TRIFAN, IA_STATE_TRILIST,
    MS_STATE_EMPTY, RASTER_STATE_DEFAULT_FILL_NOCULL, RoundedRectConstants, VI_STATE_EMPTY,
    VI_STATE_FLOAT2_ONLY,
    composite::{
        AtlasRect, CompositeInstanceManager, CompositeRect, CompositeTree, CompositeTreeRef,
        CompositionSurfaceAtlas, UnboundedCompositeInstanceManager,
        UnboundedCompositionSurfaceAtlas,
    },
    helper_types::SafeF32,
    hittest::{HitTestTreeData, HitTestTreeManager, HitTestTreeRef},
    subsystem::{StagingScratchBufferManager, Subsystem, SubsystemShaderModuleRef},
    text::TextLayout,
};

use bedrock::{
    self as br, CommandBufferMut, Device, DeviceChild, DeviceMemoryMut, ImageChild, ImageChildMut,
    MemoryBound, RenderPass, ShaderModule, VkHandle,
};
use bitflags::bitflags;

pub struct FontSet {
    pub ui_default: freetype::Owned<freetype::Face>,
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct RenderPassOptions: u32 {
        const FULL_PIXEL_RENDER = 0x01;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontType {
    UI,
}

pub struct AppBaseSystem<'subsystem> {
    pub subsystem: &'subsystem Subsystem,
    pub atlas: UnboundedCompositionSurfaceAtlas,
    pub composite_tree: CompositeTree,
    pub composite_instance_manager: UnboundedCompositeInstanceManager,
    pub hit_tree: HitTestTreeManager<'subsystem>,
    pub fonts: FontSet,
    rounded_fill_rect_cache: HashMap<(SafeF32, SafeF32), AtlasRect>,
    rounded_rect_cache: HashMap<(SafeF32, SafeF32, SafeF32), AtlasRect>,
    text_cache: HashMap<FontType, HashMap<String, AtlasRect>>,
}
impl Drop for AppBaseSystem<'_> {
    fn drop(&mut self) {
        unsafe {
            self.composite_instance_manager
                .drop_with_gfx_device(&self.subsystem);
            self.atlas.drop_with_gfx_device(&self.subsystem);
        }
    }
}
impl<'subsystem> AppBaseSystem<'subsystem> {
    pub fn new(subsystem: &'subsystem Subsystem) -> Self {
        // initialize font systems
        #[cfg(unix)]
        fontconfig::init();

        let (primary_face_path, primary_face_index);
        #[cfg(unix)]
        {
            let mut fc_pat = fontconfig::Pattern::new();
            fc_pat.add_family_name(c"system-ui");
            fc_pat.add_weight(80);
            fontconfig::Config::current()
                .unwrap()
                .substitute(&mut fc_pat, fontconfig::MatchKind::Pattern);
            fc_pat.default_substitute();
            let fc_set = fontconfig::Config::current()
                .unwrap()
                .sort(&mut fc_pat, true)
                .unwrap();
            let mut primary_face_info = None;
            for &f in fc_set.fonts() {
                let file_path = f.get_file_path(0).unwrap();
                let index = f.get_face_index(0).unwrap();

                tracing::debug!(?file_path, index, "match font");

                if primary_face_info.is_none() {
                    primary_face_info = Some((file_path.to_owned(), index));
                }
            }
            let Some((path, index)) = primary_face_info else {
                tracing::error!("No UI face found");
                std::process::exit(1);
            };
            primary_face_path = path;
            primary_face_index = index;
        }
        // TODO: mock
        #[cfg(windows)]
        {
            primary_face_path = c"C:\\Windows\\Fonts\\YuGothR.ttc";
            primary_face_index = 0;
        }

        let ft_face = match subsystem
            .ft
            .write()
            .new_face(&primary_face_path, primary_face_index as _)
        {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create ft face");
                std::process::exit(1);
            }
        };

        let composite_instance_buffer = CompositeInstanceManager::new(subsystem);
        let composition_alphamask_surface_atlas = CompositionSurfaceAtlas::new(
            subsystem,
            subsystem.adapter_properties.limits.maxImageDimension2D,
            br::vk::VK_FORMAT_R8_UNORM,
        );

        Self {
            atlas: composition_alphamask_surface_atlas.unbound(),
            composite_tree: CompositeTree::new(),
            composite_instance_manager: composite_instance_buffer.unbound(),
            hit_tree: HitTestTreeManager::new(),
            fonts: FontSet {
                ui_default: ft_face,
            },
            subsystem,
            rounded_fill_rect_cache: HashMap::new(),
            rounded_rect_cache: HashMap::new(),
            text_cache: HashMap::new(),
        }
    }

    pub fn rescale_fonts(&mut self, scale: f32) {
        if let Err(e) =
            self.fonts
                .ui_default
                .set_char_size((10.0 * 64.0) as _, 0, (96.0 * scale) as _, 0)
        {
            tracing::warn!(reason = ?e, "Failed to set char size");
        }

        // evict all text caches
        self.text_cache.clear();
    }

    pub const fn mask_atlas_format(&self) -> br::Format {
        self.atlas.format()
    }

    pub const fn mask_atlas_size(&self) -> u32 {
        self.atlas.size()
    }

    pub fn render_to_mask_atlas_pass(
        &self,
        options: RenderPassOptions,
    ) -> br::Result<impl br::RenderPass + use<'subsystem>> {
        br::RenderPassObject::new(
            self.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(br::vk::VK_FORMAT_R8_UNORM)
                    .color_memory_op(
                        if options.contains(RenderPassOptions::FULL_PIXEL_RENDER) {
                            br::LoadOp::DontCare
                        } else {
                            br::LoadOp::Clear
                        },
                        br::StoreOp::Store,
                    )
                    .layout_transition(
                        br::ImageLayout::Undefined,
                        br::ImageLayout::ShaderReadOnlyOpt,
                    )],
                &[br::SubpassDescription2::new()
                    .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
                &[br::SubpassDependency2::new(
                    br::SubpassIndex::Internal(0),
                    br::SubpassIndex::External,
                )
                .by_region()
                .of_memory(
                    br::AccessFlags::COLOR_ATTACHMENT.write,
                    br::AccessFlags::SHADER.read,
                )
                .of_execution(
                    br::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    br::PipelineStageFlags::FRAGMENT_SHADER,
                )],
            ),
        )
    }

    pub const fn mask_atlas_resource_transparent_ref(
        &self,
    ) -> &br::VkHandleRef<br::vk::VkImageView> {
        self.atlas.resource_view_transparent_ref()
    }

    pub const fn mask_atlas_image_transparent_ref(&self) -> &br::VkHandleRef<br::vk::VkImage> {
        self.atlas.image_transparent_ref()
    }

    #[inline(always)]
    pub fn alloc_mask_atlas_rect(
        &mut self,
        required_width: u32,
        required_height: u32,
    ) -> AtlasRect {
        self.atlas.alloc(required_width, required_height)
    }

    #[inline(always)]
    pub fn free_mask_atlas_rect(&mut self, rect: AtlasRect) {
        self.atlas.free(rect)
    }

    #[tracing::instrument(
        name = "AppBaseSystem::text_mask",
        skip(self, staging_scratch_buffer),
        err(Display)
    )]
    pub fn text_mask(
        &mut self,
        staging_scratch_buffer: &mut StagingScratchBufferManager,
        font_type: FontType,
        text: &str,
    ) -> br::Result<AtlasRect> {
        if let Some(&r) = self.text_cache.get(&font_type).and_then(|x| x.get(text)) {
            // found in cache
            return Ok(r);
        }

        tracing::info!("creating fresh");
        let layout = TextLayout::build_simple(
            text,
            match font_type {
                FontType::UI => &mut self.fonts.ui_default,
            },
        );
        let atlas_rect = self.alloc_mask_atlas_rect(layout.width_px(), layout.height_px());
        let image_pixels = layout.build_stg_image_pixel_buffer(staging_scratch_buffer);

        self.sync_execute_graphics_commands(|rec| {
            rec.pipeline_barrier_2(&br::DependencyInfo::new(
                &[],
                &[],
                &[self
                    .barrier_for_mask_atlas_resource()
                    .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())
                    .of_execution(br::PipelineStageFlags2(0), br::PipelineStageFlags2::COPY)],
            ))
            .inject(|r| {
                let (b, o) = staging_scratch_buffer.of(&image_pixels);

                r.copy_buffer_to_image(
                    b,
                    &self.mask_atlas_image_transparent_ref(),
                    br::ImageLayout::TransferDestOpt,
                    &[br::vk::VkBufferImageCopy {
                        bufferOffset: o,
                        bufferRowLength: layout.width_px(),
                        bufferImageHeight: layout.height_px(),
                        imageSubresource: br::ImageSubresourceLayers::new(
                            br::AspectMask::COLOR,
                            0,
                            0..1,
                        ),
                        imageOffset: atlas_rect.lt_offset().with_z(0),
                        imageExtent: atlas_rect.extent().with_depth(1),
                    }],
                )
            })
            .pipeline_barrier_2(&br::DependencyInfo::new(
                &[],
                &[],
                &[self
                    .barrier_for_mask_atlas_resource()
                    .transit_from(
                        br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt),
                    )
                    .of_memory(
                        br::AccessFlags2::TRANSFER.write,
                        br::AccessFlags2::SHADER.read,
                    )
                    .of_execution(
                        br::PipelineStageFlags2::COPY,
                        br::PipelineStageFlags2::FRAGMENT_SHADER,
                    )],
            ))
        })?;

        self.text_cache
            .entry(font_type)
            .or_insert_with(HashMap::new)
            .insert(text.into(), atlas_rect);
        Ok(atlas_rect)
    }

    #[tracing::instrument(name = "AppBaseSystem::rounded_rect_mask", skip(self), err(Display))]
    pub fn rounded_rect_mask(
        &mut self,
        render_scale: SafeF32,
        radius: SafeF32,
        thickness: SafeF32,
    ) -> br::Result<AtlasRect> {
        if let Some(&r) = self
            .rounded_rect_cache
            .get(&(render_scale, radius, thickness))
        {
            // found in cache
            return Ok(r);
        }

        tracing::info!("creating fresh");
        let size_px = ((radius.value() * 2.0 + 1.0) * render_scale.value()).ceil() as u32;
        let atlas_rect = self.alloc_mask_atlas_rect(size_px, size_px);

        let render_pass = self.render_to_mask_atlas_pass(RenderPassOptions::FULL_PIXEL_RENDER)?;
        let framebuffer = br::FramebufferObject::new(
            self.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[self
                    .atlas
                    .resource_view_transparent_ref()
                    .as_transparent_ref()],
                self.atlas.size(),
                self.atlas.size(),
            ),
        )?;

        let [pipeline] =
            self.create_graphics_pipelines_array(&[br::GraphicsPipelineCreateInfo::new(
                self.require_empty_pipeline_layout(),
                render_pass.subpass(0),
                &[
                    self.require_shader("resources/filltri.vert")
                        .on_stage(br::ShaderStage::Vertex, c"main"),
                    self.require_shader("resources/rounded_rect_border.frag")
                        .on_stage(br::ShaderStage::Fragment, c"main")
                        .with_specialization_info(&br::SpecializationInfo::new(
                            &RoundedRectConstants {
                                corner_radius: radius.value(),
                                thickness: thickness.value(),
                            },
                        )),
                ],
                VI_STATE_EMPTY,
                IA_STATE_TRILIST,
                &br::PipelineViewportStateCreateInfo::new(
                    &[atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                    &[atlas_rect.vk_rect()],
                ),
                RASTER_STATE_DEFAULT_FILL_NOCULL,
                BLEND_STATE_SINGLE_NONE,
            )
            .set_multisample_state(MS_STATE_EMPTY)])?;

        self.sync_execute_graphics_commands(|rec| {
            rec.begin_render_pass2(
                &br::RenderPassBeginInfo::new(
                    &render_pass,
                    &framebuffer,
                    atlas_rect.vk_rect(),
                    &[br::ClearValue::color_f32([0.0; 4])],
                ),
                &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
            )
            .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
            .draw(3, 1, 0, 0)
            .end_render_pass2(&br::SubpassEndInfo::new())
        })?;

        self.rounded_rect_cache
            .insert((render_scale, radius, thickness), atlas_rect);
        Ok(atlas_rect)
    }

    #[tracing::instrument(
        name = "AppBaseSystem::rounded_fill_rect_mask",
        skip(self),
        err(Display)
    )]
    pub fn rounded_fill_rect_mask(
        &mut self,
        render_scale: SafeF32,
        radius: SafeF32,
    ) -> br::Result<AtlasRect> {
        if let Some(r) = self.rounded_fill_rect_cache.get(&(render_scale, radius)) {
            // found in cache
            tracing::trace!("hit cache");
            return Ok(r.clone());
        }

        tracing::info!("creating fresh image");
        let size_px = ((radius.value() * 2.0 + 1.0) * render_scale.value()).ceil() as u32;
        let atlas_rect = self.alloc_mask_atlas_rect(size_px, size_px);

        let render_pass = self.render_to_mask_atlas_pass(RenderPassOptions::FULL_PIXEL_RENDER)?;
        let framebuffer = br::FramebufferObject::new(
            self.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[self
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                self.mask_atlas_size(),
                self.mask_atlas_size(),
            ),
        )?;

        let [pipeline] = self
            .create_graphics_pipelines_array(&[br::GraphicsPipelineCreateInfo::new(
                self.require_empty_pipeline_layout(),
                render_pass.subpass(0),
                &[
                    self.require_shader("resources/filltri.vert")
                        .on_stage(br::ShaderStage::Vertex, c"main"),
                    self.require_shader("resources/rounded_rect.frag")
                        .on_stage(br::ShaderStage::Fragment, c"main"),
                ],
                VI_STATE_EMPTY,
                IA_STATE_TRILIST,
                &br::PipelineViewportStateCreateInfo::new(
                    &[atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                    &[atlas_rect.vk_rect()],
                ),
                RASTER_STATE_DEFAULT_FILL_NOCULL,
                BLEND_STATE_SINGLE_NONE,
            )
            .set_multisample_state(MS_STATE_EMPTY)])
            .unwrap();

        self.sync_execute_graphics_commands(|rec| {
            rec.begin_render_pass2(
                &br::RenderPassBeginInfo::new(
                    &render_pass,
                    &framebuffer,
                    atlas_rect.vk_rect(),
                    &[br::ClearValue::color_f32([0.0; 4])],
                ),
                &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
            )
            .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
            .draw(3, 1, 0, 0)
            .end_render_pass2(&br::SubpassEndInfo::new())
        })?;

        self.rounded_fill_rect_cache
            .insert((render_scale, radius), atlas_rect);
        Ok(atlas_rect)
    }

    #[tracing::instrument(name = "AppBaseSystem::rasterize_svg", skip(self), fields(path = %path.as_ref().display()))]
    pub fn rasterize_svg(
        &mut self,
        width_px: u32,
        height_px: u32,
        path: impl AsRef<Path>,
    ) -> br::Result<AtlasRect> {
        let atlas_rect = self.alloc_mask_atlas_rect(width_px, height_px);

        let icon_svg_content = match std::fs::read_to_string(path) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to load icon svg");
                std::process::abort();
            }
        };
        let mut reader = quick_xml::Reader::from_str(&icon_svg_content);
        let mut svg_path_commands = Vec::new();
        let mut viewbox = None;
        loop {
            match reader.read_event().unwrap() {
                quick_xml::events::Event::Start(x) => {
                    println!("xml start: {x:?}");
                    println!("  name {:?}", x.name());
                    for a in x.attributes().with_checks(false) {
                        let a = a.unwrap();
                        println!("  attr {:?} = {:?}", a.key, a.unescape_value());
                    }

                    if x.name().0 == b"svg" {
                        let viewbox_value = &x
                            .attributes()
                            .with_checks(false)
                            .find(|x| x.as_ref().is_ok_and(|x| x.key.0 == b"viewBox"))
                            .unwrap()
                            .unwrap()
                            .value;
                        viewbox = Some(crate::svg::ViewBox::from_str_bytes(viewbox_value));
                    }
                }
                quick_xml::events::Event::End(x) => {
                    println!("xml end: {x:?}");
                    println!("  name {:?}", x.name());
                }
                quick_xml::events::Event::Empty(x) => {
                    println!("xml empty: {x:?}");
                    println!("  name {:?}", x.name());
                    for a in x.attributes().with_checks(false) {
                        let a = a.unwrap();
                        println!("  attr {:?} = {:?}", a.key, a.unescape_value());
                    }

                    if x.name().0 == b"path" {
                        let path_data = &x
                            .attributes()
                            .with_checks(false)
                            .find(|x| x.as_ref().is_ok_and(|x| x.key.0 == b"d"))
                            .unwrap()
                            .unwrap()
                            .value;
                        for x in crate::svg::InstructionParser::new_bytes(path_data) {
                            println!("  path inst: {x:?}");
                            svg_path_commands.push(x);
                        }
                    }
                }
                quick_xml::events::Event::Text(x) => println!("xml text: {x:?}"),
                quick_xml::events::Event::CData(x) => println!("xml cdata: {x:?}"),
                quick_xml::events::Event::Comment(x) => println!("xml comment: {x:?}"),
                quick_xml::events::Event::Decl(x) => println!("xml decl: {x:?}"),
                quick_xml::events::Event::PI(x) => println!("xml pi: {x:?}"),
                quick_xml::events::Event::DocType(x) => println!("xml doctype: {x:?}"),
                quick_xml::events::Event::Eof => {
                    println!("eof");
                    break;
                }
            }
        }

        let viewbox = viewbox.unwrap();

        // rasterize icon svg
        let mut stencil_buffer = br::ImageObject::new(
            self.subsystem,
            &br::ImageCreateInfo::new(atlas_rect.extent(), br::vk::VK_FORMAT_S8_UINT)
                .as_depth_stencil_attachment()
                .as_transient_attachment()
                .sample_counts(4),
        )?;
        let stencil_buffer_mem =
            self.alloc_device_local_memory_for_requirements(&stencil_buffer.requirements());
        stencil_buffer.bind(&stencil_buffer_mem, 0).unwrap();
        let stencil_buffer = br::ImageViewBuilder::new(
            stencil_buffer,
            br::ImageSubresourceRange::new(br::AspectMask::STENCIL, 0..1, 0..1),
        )
        .create()?;

        let mut ms_color_buffer = br::ImageObject::new(
            self.subsystem,
            &br::ImageCreateInfo::new(atlas_rect.extent(), br::vk::VK_FORMAT_R8_UNORM)
                .as_color_attachment()
                .usage_with(br::ImageUsageFlags::TRANSFER_SRC)
                .sample_counts(4),
        )?;
        let ms_color_buffer_mem =
            self.alloc_device_local_memory_for_requirements(&ms_color_buffer.requirements());
        ms_color_buffer.bind(&ms_color_buffer_mem, 0).unwrap();
        let ms_color_buffer = br::ImageViewBuilder::new(
            ms_color_buffer,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()?;

        let render_pass = br::RenderPassObject::new(
            self.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[
                    br::AttachmentDescription2::new(br::vk::VK_FORMAT_R8_UNORM)
                        .color_memory_op(br::LoadOp::Clear, br::StoreOp::DontCare)
                        .with_layout_to(br::ImageLayout::TransferSrcOpt.from_undefined())
                        .samples(4),
                    br::AttachmentDescription2::new(br::vk::VK_FORMAT_S8_UINT)
                        .stencil_memory_op(br::LoadOp::Clear, br::StoreOp::DontCare)
                        .with_layout_to(br::ImageLayout::DepthStencilReadOnlyOpt.from_undefined())
                        .samples(4),
                ],
                &[
                    br::SubpassDescription2::new()
                        .depth_stencil(&br::AttachmentReference2::depth_stencil_attachment_opt(1)),
                    br::SubpassDescription2::new()
                        .colors(&[br::AttachmentReference2::color_attachment_opt(0)])
                        .depth_stencil(&br::AttachmentReference2::depth_stencil_readonly_opt(1)),
                ],
                &[
                    br::SubpassDependency2::new(
                        br::SubpassIndex::Internal(0),
                        br::SubpassIndex::Internal(1),
                    )
                    .by_region()
                    .of_memory(
                        br::AccessFlags::DEPTH_STENCIL_ATTACHMENT.write,
                        br::AccessFlags::DEPTH_STENCIL_ATTACHMENT.read,
                    )
                    .of_execution(
                        br::PipelineStageFlags::LATE_FRAGMENT_TESTS,
                        br::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
                    ),
                    br::SubpassDependency2::new(
                        br::SubpassIndex::Internal(1),
                        br::SubpassIndex::External,
                    )
                    .by_region()
                    .of_memory(
                        br::AccessFlags::COLOR_ATTACHMENT.write,
                        br::AccessFlags::TRANSFER.read,
                    )
                    .of_execution(
                        br::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                        br::PipelineStageFlags::TRANSFER,
                    ),
                ],
            ),
        )
        .unwrap();
        let fb = br::FramebufferObject::new(
            self.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[
                    ms_color_buffer.as_transparent_ref(),
                    stencil_buffer.as_transparent_ref(),
                ],
                atlas_rect.width(),
                atlas_rect.height(),
            ),
        )
        .unwrap();

        let local_viewports = [atlas_rect
            .extent()
            .into_rect(br::Offset2D::ZERO)
            .make_viewport(0.0..1.0)];
        let local_scissor_rects = [atlas_rect.extent().into_rect(br::Offset2D::ZERO)];
        let vp_state_local =
            br::PipelineViewportStateCreateInfo::new_array(&local_viewports, &local_scissor_rects);
        let sop_invert_always = br::vk::VkStencilOpState {
            failOp: br::StencilOp::Invert as _,
            passOp: br::StencilOp::Invert as _,
            depthFailOp: br::StencilOp::Invert as _,
            compareOp: br::CompareOp::Always as _,
            compareMask: 0,
            writeMask: 0x01,
            reference: 0x01,
        };
        let sop_testonly_equal_1 = br::vk::VkStencilOpState {
            failOp: br::StencilOp::Keep as _,
            passOp: br::StencilOp::Keep as _,
            depthFailOp: br::StencilOp::Keep as _,
            compareOp: br::CompareOp::Equal as _,
            compareMask: 0x01,
            reference: 0x01,
            writeMask: 0,
        };
        let [
            first_stencil_shape_pipeline,
            curve_stencil_shape_pipeline,
            colorize_pipeline,
        ] = self
            .create_graphics_pipelines_array(&[
                // first stencil shape pipeline
                br::GraphicsPipelineCreateInfo::new(
                    self.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        self.require_shader("resources/normalized_01_2d.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        self.require_shader("resources/stencil_only.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_FLOAT2_ONLY,
                    IA_STATE_TRIFAN,
                    &vp_state_local,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_depth_stencil_state(
                    &br::PipelineDepthStencilStateCreateInfo::new()
                        .stencil_test(true)
                        .stencil_state_back(sop_invert_always.clone())
                        .stencil_state_front(sop_invert_always.clone()),
                )
                .set_multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
                ),
                // curve stencil shape
                br::GraphicsPipelineCreateInfo::new(
                    self.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        self.require_shader("resources/normalized_01_2d_with_uv.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        self.require_shader("resources/stencil_loop_blinn_curve.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    &br::PipelineVertexInputStateCreateInfo::new(
                        &[br::VertexInputBindingDescription::per_vertex_typed::<
                            [f32; 4],
                        >(0)],
                        &[
                            br::VertexInputAttributeDescription {
                                location: 0,
                                binding: 0,
                                format: br::vk::VK_FORMAT_R32G32_SFLOAT,
                                offset: 0,
                            },
                            br::VertexInputAttributeDescription {
                                location: 1,
                                binding: 0,
                                format: br::vk::VK_FORMAT_R32G32_SFLOAT,
                                offset: core::mem::size_of::<[f32; 2]>() as _,
                            },
                        ],
                    ),
                    IA_STATE_TRILIST,
                    &vp_state_local,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_depth_stencil_state(
                    &br::PipelineDepthStencilStateCreateInfo::new()
                        .stencil_test(true)
                        .stencil_state_back(sop_invert_always.clone())
                        .stencil_state_front(sop_invert_always),
                )
                .set_multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
                ),
                // colorize pipeline
                br::GraphicsPipelineCreateInfo::new(
                    self.require_empty_pipeline_layout(),
                    render_pass.subpass(1),
                    &[
                        self.require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        self.require_shader("resources/fillcolor_r.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &FillcolorRConstants { r: 1.0 },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &vp_state_local,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_depth_stencil_state(
                    &br::PipelineDepthStencilStateCreateInfo::new()
                        .stencil_test(true)
                        .stencil_state_back(sop_testonly_equal_1.clone())
                        .stencil_state_front(sop_testonly_equal_1),
                )
                .set_multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new()
                        .rasterization_samples(4)
                        .enable_alpha_to_coverage(),
                ),
            ])
            .unwrap();

        let mut trifan_points = Vec::<[f32; 2]>::new();
        let mut trifan_point_ranges = Vec::new();
        let mut curve_triangle_points = Vec::<[f32; 4]>::new();
        let mut cubic_last_control_point = None::<(f32, f32)>;
        let mut quadratic_last_control_point = None::<(f32, f32)>;
        let mut current_figure_first_index = None;
        let mut p = (0.0f32, 0.0f32);
        for x in svg_path_commands.iter() {
            match x {
                &crate::svg::Instruction::Move { absolute, x, y } => {
                    if current_figure_first_index.is_some() {
                        panic!("not closed last figure");
                    }

                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p = if absolute { (x, y) } else { (p.0 + x, p.1 + y) };
                    current_figure_first_index = Some(trifan_points.len());

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &crate::svg::Instruction::Line { absolute, x, y } => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p = if absolute { (x, y) } else { (p.0 + x, p.1 + y) };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &crate::svg::Instruction::HLine { absolute, x } => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p.0 = if absolute { x } else { p.0 + x };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &crate::svg::Instruction::VLine { absolute, y } => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p.1 = if absolute { y } else { p.1 + y };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &crate::svg::Instruction::BezierCurve {
                    absolute,
                    c1_x,
                    c1_y,
                    c2_x,
                    c2_y,
                    x,
                    y,
                } => {
                    let figure = lyon_geom::CubicBezierSegment {
                        from: lyon_geom::point(p.0, p.1),
                        ctrl1: if absolute {
                            lyon_geom::point(c1_x, c1_y)
                        } else {
                            lyon_geom::point(p.0 + c1_x, p.1 + c1_y)
                        },
                        ctrl2: if absolute {
                            lyon_geom::point(c2_x, c2_y)
                        } else {
                            lyon_geom::point(p.0 + c2_x, p.1 + c2_y)
                        },
                        to: if absolute {
                            lyon_geom::point(x, y)
                        } else {
                            lyon_geom::point(p.0 + x, p.1 + y)
                        },
                    };

                    figure.for_each_quadratic_bezier(1.0, &mut |q| {
                        curve_triangle_points.extend([
                            [
                                viewbox.translate_x(q.from.x),
                                viewbox.translate_y(q.from.y),
                                0.0,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.ctrl.x),
                                viewbox.translate_y(q.ctrl.y),
                                0.5,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.to.x),
                                viewbox.translate_y(q.to.y),
                                1.0,
                                1.0,
                            ],
                        ]);

                        // TODO: おなじ位置の頂点を出力する場合があるのでもうちょい最適化したいかも
                        trifan_points
                            .push([viewbox.translate_x(q.from.x), viewbox.translate_y(q.from.y)]);
                        trifan_points
                            .push([viewbox.translate_x(q.to.x), viewbox.translate_y(q.to.y)]);
                    });

                    cubic_last_control_point = Some((figure.ctrl2.x, figure.ctrl2.y));
                    quadratic_last_control_point = None;
                    p = (figure.to.x, figure.to.y);
                }
                &crate::svg::Instruction::SequentialBezierCurve {
                    absolute,
                    c2_x,
                    c2_y,
                    x,
                    y,
                } => {
                    let figure = lyon_geom::CubicBezierSegment {
                        from: lyon_geom::point(p.0, p.1),
                        ctrl1: if let Some((lc2_x, lc2_y)) = cubic_last_control_point {
                            let d = (p.0 - lc2_x, p.1 - lc2_y);
                            lyon_geom::point(p.0 + d.0, p.1 + d.1)
                        } else {
                            lyon_geom::point(p.0, p.1)
                        },
                        ctrl2: if absolute {
                            lyon_geom::point(c2_x, c2_y)
                        } else {
                            lyon_geom::point(p.0 + c2_x, p.1 + c2_y)
                        },
                        to: if absolute {
                            lyon_geom::point(x, y)
                        } else {
                            lyon_geom::point(p.0 + x, p.1 + y)
                        },
                    };

                    figure.for_each_quadratic_bezier(1.0, &mut |q| {
                        curve_triangle_points.extend([
                            [
                                viewbox.translate_x(q.from.x),
                                viewbox.translate_y(q.from.y),
                                0.0,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.ctrl.x),
                                viewbox.translate_y(q.ctrl.y),
                                0.5,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.to.x),
                                viewbox.translate_y(q.to.y),
                                1.0,
                                1.0,
                            ],
                        ]);

                        // TODO: おなじ位置の頂点を出力する場合があるのでもうちょい最適化したい
                        trifan_points
                            .push([viewbox.translate_x(q.from.x), viewbox.translate_y(q.from.y)]);
                        trifan_points
                            .push([viewbox.translate_x(q.to.x), viewbox.translate_y(q.to.y)]);
                    });

                    cubic_last_control_point = Some((figure.ctrl2.x, figure.ctrl2.y));
                    quadratic_last_control_point = None;
                    p = (figure.to.x, figure.to.y);
                }
                &crate::svg::Instruction::QuadraticBezierCurve {
                    absolute,
                    cx,
                    cy,
                    x,
                    y,
                } => {
                    curve_triangle_points.extend([
                        [viewbox.translate_x(p.0), viewbox.translate_y(p.1), 0.0, 0.0],
                        if absolute {
                            [viewbox.translate_x(cx), viewbox.translate_y(cy), 0.5, 0.0]
                        } else {
                            [
                                viewbox.translate_x(p.0 + cx),
                                viewbox.translate_y(p.1 + cy),
                                0.5,
                                0.0,
                            ]
                        },
                        if absolute {
                            [viewbox.translate_x(x), viewbox.translate_y(y), 1.0, 1.0]
                        } else {
                            [
                                viewbox.translate_x(p.0 + x),
                                viewbox.translate_y(p.1 + y),
                                1.0,
                                1.0,
                            ]
                        },
                    ]);
                    cubic_last_control_point = None;
                    quadratic_last_control_point = Some(if absolute {
                        (cx, cy)
                    } else {
                        (p.0 + cx, p.1 + cy)
                    });
                    p = if absolute { (x, y) } else { (p.0 + x, p.1 + y) };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &crate::svg::Instruction::SequentialQuadraticBezierCurve { absolute, x, y } => {
                    let (cx, cy) = if let Some((lcx, lcy)) = quadratic_last_control_point {
                        let d = (p.0 - lcx, p.1 - lcy);
                        (p.0 + d.0, p.1 + d.1)
                    } else {
                        p
                    };

                    curve_triangle_points.extend([
                        [viewbox.translate_x(p.0), viewbox.translate_y(p.1), 0.0, 0.0],
                        [viewbox.translate_x(cx), viewbox.translate_y(cy), 0.5, 0.0],
                        if absolute {
                            [viewbox.translate_x(x), viewbox.translate_y(y), 1.0, 1.0]
                        } else {
                            [
                                viewbox.translate_x(p.0 + x),
                                viewbox.translate_y(p.1 + y),
                                1.0,
                                1.0,
                            ]
                        },
                    ]);
                    cubic_last_control_point = None;
                    quadratic_last_control_point = Some((cx, cy));
                    p = if absolute { (x, y) } else { (p.0 + x, p.1 + y) };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &crate::svg::Instruction::Arc {
                    absolute,
                    rx,
                    ry,
                    angle,
                    large_arc_flag,
                    sweep_flag,
                    x,
                    y,
                } => {
                    let figure = lyon_geom::SvgArc {
                        from: lyon_geom::point(p.0, p.1),
                        to: if absolute {
                            lyon_geom::point(x, y)
                        } else {
                            lyon_geom::point(p.0 + x, p.1 + y)
                        },
                        radii: lyon_geom::vector(rx, ry),
                        x_rotation: lyon_geom::Angle::degrees(angle),
                        flags: lyon_geom::ArcFlags {
                            large_arc: large_arc_flag,
                            sweep: sweep_flag,
                        },
                    };

                    figure.for_each_quadratic_bezier(&mut |q| {
                        curve_triangle_points.extend([
                            [
                                viewbox.translate_x(q.from.x),
                                viewbox.translate_y(q.from.y),
                                0.0,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.ctrl.x),
                                viewbox.translate_y(q.ctrl.y),
                                0.5,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.to.x),
                                viewbox.translate_y(q.to.y),
                                1.0,
                                1.0,
                            ],
                        ]);

                        // TODO: おなじ位置の頂点を出力する場合があるのでもうちょい最適化したい
                        trifan_points
                            .push([viewbox.translate_x(q.from.x), viewbox.translate_y(q.from.y)]);
                        trifan_points
                            .push([viewbox.translate_x(q.to.x), viewbox.translate_y(q.to.y)]);
                    });

                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p = (figure.to.x, figure.to.y);
                }
                &crate::svg::Instruction::Close => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    let x = current_figure_first_index.take().unwrap();
                    let p = (trifan_points[x][0], trifan_points[x][1]);
                    trifan_point_ranges.push(x..trifan_points.len());

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
            }
        }
        if let Some(x) = current_figure_first_index {
            // unprocessed final figure
            trifan_point_ranges.push(x..trifan_points.len());
        }

        let curve_triangle_points_offset = trifan_points.len() * core::mem::size_of::<[f32; 2]>();
        let mut vbuf = br::BufferObject::new(
            self.subsystem,
            &br::BufferCreateInfo::new(
                curve_triangle_points_offset
                    + curve_triangle_points.len() * core::mem::size_of::<[f32; 4]>(),
                br::BufferUsage::VERTEX_BUFFER,
            ),
        )
        .unwrap();
        let req = vbuf.requirements();
        let memindex = self.find_direct_memory_index(req.memoryTypeBits).unwrap();
        let mut mem = br::DeviceMemoryObject::new(
            self.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        vbuf.bind(&mem, 0).unwrap();
        let h = mem.native_ptr();
        let requires_flush = !self.is_coherent_memory_type(memindex);
        let ptr = mem.map(0..req.size as _).unwrap();
        unsafe {
            core::ptr::copy_nonoverlapping(
                trifan_points.as_ptr(),
                ptr.addr_of_mut(0),
                trifan_points.len(),
            );
            core::ptr::copy_nonoverlapping(
                curve_triangle_points.as_ptr(),
                ptr.addr_of_mut(curve_triangle_points_offset),
                curve_triangle_points.len(),
            );
        }
        if requires_flush {
            unsafe {
                self.subsystem
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(h, 0, req.size)])
                    .unwrap();
            }
        }
        unsafe {
            mem.unmap();
        }

        self.sync_execute_graphics_commands(|rec| {
            rec.begin_render_pass2(
                &br::RenderPassBeginInfo::new(
                    &render_pass,
                    &fb,
                    atlas_rect.extent().into_rect(br::Offset2D::ZERO),
                    &[
                        br::ClearValue::color_f32([0.0; 4]),
                        br::ClearValue::depth_stencil(1.0, 0),
                    ],
                ),
                &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
            )
            .bind_pipeline(
                br::PipelineBindPoint::Graphics,
                &first_stencil_shape_pipeline,
            )
            .bind_vertex_buffer_array(0, &[vbuf.as_transparent_ref()], &[0])
            .inject(|r| {
                trifan_point_ranges
                    .into_iter()
                    .fold(r, |r, vr| r.draw(vr.len() as _, 1, vr.start as _, 0))
            })
            .inject(|r| {
                if curve_triangle_points.is_empty() {
                    // no curves
                    return r;
                }

                r.bind_pipeline(
                    br::PipelineBindPoint::Graphics,
                    &curve_stencil_shape_pipeline,
                )
                .bind_vertex_buffer_array(
                    0,
                    &[vbuf.as_transparent_ref()],
                    &[curve_triangle_points_offset as _],
                )
                .draw(curve_triangle_points.len() as _, 1, 0, 0)
            })
            .next_subpass2(
                &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                &br::SubpassEndInfo::new(),
            )
            .bind_pipeline(br::PipelineBindPoint::Graphics, &colorize_pipeline)
            .draw(3, 1, 0, 0)
            .end_render_pass2(&br::SubpassEndInfo::new())
            .pipeline_barrier_2(&br::DependencyInfo::new(
                &[],
                &[],
                &[self
                    .barrier_for_mask_atlas_resource()
                    .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())
                    .of_execution(br::PipelineStageFlags2(0), br::PipelineStageFlags2::RESOLVE)],
            ))
            .resolve_image(
                ms_color_buffer.image(),
                br::ImageLayout::TransferSrcOpt,
                &self.mask_atlas_image_transparent_ref(),
                br::ImageLayout::TransferDestOpt,
                &[br::vk::VkImageResolve {
                    srcSubresource: br::ImageSubresourceLayers::new(br::AspectMask::COLOR, 0, 0..1),
                    srcOffset: br::Offset3D::ZERO,
                    dstSubresource: br::ImageSubresourceLayers::new(br::AspectMask::COLOR, 0, 0..1),
                    dstOffset: atlas_rect.lt_offset().with_z(0),
                    extent: atlas_rect.extent().with_depth(1),
                }],
            )
            .pipeline_barrier_2(&br::DependencyInfo::new(
                &[],
                &[],
                &[self
                    .barrier_for_mask_atlas_resource()
                    .transit_from(
                        br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt),
                    )
                    .of_memory(
                        br::AccessFlags2::TRANSFER.write,
                        br::AccessFlags2::SHADER.read,
                    )
                    .of_execution(
                        br::PipelineStageFlags2::RESOLVE,
                        br::PipelineStageFlags2::FRAGMENT_SHADER,
                    )],
            ))
        })?;

        Ok(atlas_rect)
    }

    #[inline(always)]
    pub fn barrier_for_mask_atlas_resource(&self) -> br::ImageMemoryBarrier2 {
        br::ImageMemoryBarrier2::new(
            self.atlas.image_transparent_ref(),
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
    }

    #[inline(always)]
    pub fn register_composite_rect(&mut self, rect: CompositeRect) -> CompositeTreeRef {
        self.composite_tree.register(rect)
    }

    #[inline(always)]
    pub fn set_composite_tree_parent(&mut self, child: CompositeTreeRef, parent: CompositeTreeRef) {
        self.composite_tree.add_child(parent, child);
    }

    #[inline(always)]
    pub fn create_hit_tree(&mut self, init: HitTestTreeData<'subsystem>) -> HitTestTreeRef {
        self.hit_tree.create(init)
    }

    #[inline(always)]
    pub fn set_hit_tree_parent(&mut self, child: HitTestTreeRef, parent: HitTestTreeRef) {
        self.hit_tree.add_child(parent, child);
    }

    #[inline]
    pub fn set_tree_parent(
        &mut self,
        child: (CompositeTreeRef, HitTestTreeRef),
        parent: (CompositeTreeRef, HitTestTreeRef),
    ) {
        self.set_composite_tree_parent(child.0, parent.0);
        self.set_hit_tree_parent(child.1, parent.1);
    }

    #[inline(always)]
    pub fn find_direct_memory_index(&self, index_mask: u32) -> Option<u32> {
        self.subsystem.find_direct_memory_index(index_mask)
    }

    #[inline(always)]
    pub fn find_device_local_memory_index(&self, index_mask: u32) -> Option<u32> {
        self.subsystem.find_device_local_memory_index(index_mask)
    }

    #[inline(always)]
    pub fn find_host_visible_memory_index(&self, index_mask: u32) -> Option<u32> {
        self.subsystem.find_host_visible_memory_index(index_mask)
    }

    #[inline(always)]
    pub fn is_coherent_memory_type(&self, index: u32) -> bool {
        self.subsystem.is_coherent_memory_type(index)
    }

    #[inline(always)]
    pub fn require_shader(&self, path: impl AsRef<Path>) -> SubsystemShaderModuleRef<'subsystem> {
        self.subsystem.require_shader(path)
    }

    #[inline(always)]
    pub fn require_empty_pipeline_layout(
        &self,
    ) -> &impl br::VkHandle<Handle = br::vk::VkPipelineLayout> {
        self.subsystem.require_empty_pipeline_layout()
    }

    #[inline(always)]
    pub fn create_graphics_pipelines_array<const N: usize>(
        &self,
        create_info_array: &[br::GraphicsPipelineCreateInfo; N],
    ) -> br::Result<[br::PipelineObject<&'subsystem Subsystem>; N]> {
        self.subsystem
            .create_graphics_pipelines_array(create_info_array)
    }

    #[inline(always)]
    pub fn create_transient_graphics_command_pool(&self) -> br::Result<impl br::CommandPoolMut> {
        self.subsystem.create_transient_graphics_command_pool()
    }

    pub fn sync_execute_graphics_commands(
        &self,
        rec: impl for<'e> FnOnce(br::CmdRecord<'e, Subsystem>) -> br::CmdRecord<'e, Subsystem>,
    ) -> br::Result<()> {
        let mut cp = self.create_transient_graphics_command_pool()?;
        let [mut cb] = br::CommandBufferObject::alloc_array(
            self.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )?;
        rec(unsafe {
            cb.begin(
                &br::CommandBufferBeginInfo::new().onetime_submit(),
                self.subsystem,
            )?
        })
        .end()?;

        self.sync_execute_graphics_command_buffers(&[br::CommandBufferSubmitInfo::new(&cb)])
    }

    #[inline(always)]
    pub fn sync_execute_graphics_command_buffers(
        &self,
        buffers: &[br::CommandBufferSubmitInfo],
    ) -> br::Result<()> {
        self.subsystem.sync_execute_graphics_commands(buffers)
    }

    #[tracing::instrument(skip(self), fields(memory_type_index))]
    pub fn alloc_device_local_memory(
        &self,
        size: br::DeviceSize,
        memory_type_index_mask: u32,
    ) -> br::DeviceMemoryObject<&'subsystem Subsystem> {
        let Some(memindex) = self.find_device_local_memory_index(memory_type_index_mask) else {
            tracing::error!("no suitable memory");
            std::process::exit(1);
        };
        tracing::Span::current().record("memory_type_index", memindex);

        match br::DeviceMemoryObject::new(
            self.subsystem,
            &br::MemoryAllocateInfo::new(size, memindex),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to allocate device local memory");
                std::process::exit(1);
            }
        }
    }

    #[inline(always)]
    pub fn alloc_device_local_memory_for_requirements(
        &self,
        req: &br::vk::VkMemoryRequirements,
    ) -> br::DeviceMemoryObject<&'subsystem Subsystem> {
        self.alloc_device_local_memory(req.size, req.memoryTypeBits)
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct MemoryCharacteristics : u8 {
        const COHERENT = 0x01;
    }
}

#[derive(Debug, Clone, Copy)]
pub enum BufferMapMode {
    Read = 0x01,
    Write = 0x02,
    ReadWrite = 0x03,
}
impl BufferMapMode {
    const fn is_read(&self) -> bool {
        matches!(*self, Self::Read | Self::ReadWrite)
    }

    const fn is_write(&self) -> bool {
        matches!(*self, Self::Write | Self::ReadWrite)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BufferCreationError {
    #[error(transparent)]
    Vulkan(#[from] br::vk::VkResult),
    #[error("no suitable memory")]
    NoSuitableMemory,
}

pub struct MemoryBoundBuffer<'subsystem> {
    buffer: br::BufferObject<&'subsystem Subsystem>,
    memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    memory_characteristics: MemoryCharacteristics,
}
impl<'subsystem> br::VkHandle for MemoryBoundBuffer<'subsystem> {
    type Handle = br::vk::VkBuffer;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.buffer.native_ptr()
    }
}
impl<'subsystem> br::VkHandleMut for MemoryBoundBuffer<'subsystem> {
    #[inline(always)]
    fn native_ptr_mut(&mut self) -> Self::Handle {
        self.buffer.native_ptr_mut()
    }
}
impl<'subsystem> br::DeviceChildHandle for MemoryBoundBuffer<'subsystem> {
    #[inline(always)]
    fn device_handle(&self) -> bedrock::vk::VkDevice {
        self.buffer.device_handle()
    }
}
impl<'subsystem> br::DeviceChild for MemoryBoundBuffer<'subsystem> {
    type ConcreteDevice = &'subsystem Subsystem;

    #[inline(always)]
    fn device(&self) -> &Self::ConcreteDevice {
        self.buffer.device()
    }
}
impl<'subsystem> br::Buffer for MemoryBoundBuffer<'subsystem> {}
impl<'subsystem> MemoryBoundBuffer<'subsystem> {
    #[tracing::instrument(
        name = "MemoryBoundBuffer::new_writable",
        skip(base_system),
        err(Display)
    )]
    pub fn new_writable(
        base_system: &AppBaseSystem<'subsystem>,
        size: usize,
        usage: br::BufferUsage,
    ) -> Result<MemoryBoundBuffer<'subsystem>, BufferCreationError> {
        // TODO: direct memoryにするかはサイズとかみて判断する
        let mut b = br::BufferObject::new(
            base_system.subsystem,
            &br::BufferCreateInfo::new(size, usage),
        )?;
        let req = b.requirements();
        let Some(memindex) = base_system.find_direct_memory_index(req.memoryTypeBits) else {
            return Err(BufferCreationError::NoSuitableMemory);
        };
        let mem = br::DeviceMemoryObject::new(
            base_system.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )?;
        b.bind(&mem, 0)?;

        Ok(Self {
            buffer: b,
            memory: mem,
            memory_characteristics: if base_system.is_coherent_memory_type(memindex) {
                MemoryCharacteristics::COHERENT
            } else {
                MemoryCharacteristics::empty()
            },
        })
    }

    pub fn map<'b>(
        &'b mut self,
        range: core::ops::Range<usize>,
        mode: BufferMapMode,
    ) -> br::Result<MappedBuffer<'b, 'subsystem>> {
        let p = unsafe { self.memory.map_raw(range.start as _..range.end as _)? };
        if mode.is_read()
            && !self
                .memory_characteristics
                .contains(MemoryCharacteristics::COHERENT)
        {
            unsafe {
                self.buffer
                    .device()
                    .invalidate_memory_range(&[br::MappedMemoryRange::new(
                        &self.memory,
                        range.start as _..range.end as _,
                    )])?;
            }
        }

        Ok(MappedBuffer {
            ptr: unsafe { core::ptr::NonNull::new_unchecked(p) },
            range,
            mode,
            buffer: self,
        })
    }
}

#[must_use]
pub struct MappedBuffer<'b, 'subsystem> {
    ptr: core::ptr::NonNull<core::ffi::c_void>,
    range: core::ops::Range<usize>,
    mode: BufferMapMode,
    buffer: &'b mut MemoryBoundBuffer<'subsystem>,
}
impl Drop for MappedBuffer<'_, '_> {
    fn drop(&mut self) {
        tracing::warn!("MappedBuffer must be closed with `unmap`!");
    }
}
impl<'b, 'subsystem> MappedBuffer<'b, 'subsystem> {
    pub fn unmap(self) -> br::Result<()> {
        if self.mode.is_write()
            && !self
                .buffer
                .memory_characteristics
                .contains(MemoryCharacteristics::COHERENT)
        {
            unsafe {
                self.buffer
                    .device()
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new(
                        &self.buffer.memory,
                        self.range.start as _..self.range.end as _,
                    )])?;
            }
        }

        unsafe {
            self.buffer.memory.unmap();
        }

        core::mem::forget(self);
        Ok(())
    }

    pub const fn ptr_of<T>(&self) -> core::ptr::NonNull<T> {
        self.ptr.cast()
    }

    pub const fn addr_of_mut<T>(&self, byte_offset: usize) -> *mut T {
        unsafe { self.ptr.byte_add(byte_offset).cast().as_ptr() }
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct RenderTextureFlags : u8 {
        const ALLOW_TRANSFER_SRC = 0x01;
        const NON_SAMPLED = 0x02;
    }
}

#[derive(Debug, Clone)]
pub struct RenderTextureOptions {
    pub flags: RenderTextureFlags,
    pub msaa_count: Option<br::vk::VkSampleCountFlagBits>,
}
impl Default for RenderTextureOptions {
    fn default() -> Self {
        Self {
            flags: RenderTextureFlags::empty(),
            msaa_count: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PixelFormat {
    R8,
}
impl PixelFormat {
    const fn vk_format(&self) -> br::Format {
        match self {
            Self::R8 => br::vk::VK_FORMAT_R8_UNORM,
        }
    }

    const fn aspect_mask(&self) -> br::AspectMask {
        match self {
            Self::R8 => br::AspectMask::COLOR,
        }
    }
}

pub struct RenderTexture<'subsystem> {
    res: br::ImageViewObject<br::ImageObject<&'subsystem Subsystem>>,
    mem: br::DeviceMemoryObject<&'subsystem Subsystem>,
    size: br::Extent2D,
    pixel_format: PixelFormat,
    msaa_count: Option<br::vk::VkSampleCountFlagBits>,
}
impl<'subsystem> br::VkHandle for RenderTexture<'subsystem> {
    type Handle = br::vk::VkImageView;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.res.native_ptr()
    }
}
impl<'subsystem> br::VkHandleMut for RenderTexture<'subsystem> {
    #[inline(always)]
    fn native_ptr_mut(&mut self) -> Self::Handle {
        self.res.native_ptr_mut()
    }
}
impl<'subsystem> br::DeviceChildHandle for RenderTexture<'subsystem> {
    #[inline(always)]
    fn device_handle(&self) -> bedrock::vk::VkDevice {
        self.res.device_handle()
    }
}
impl<'subsystem> br::DeviceChild for RenderTexture<'subsystem> {
    type ConcreteDevice = &'subsystem Subsystem;

    #[inline(always)]
    fn device(&self) -> &Self::ConcreteDevice {
        self.res.device()
    }
}
impl<'subsystem> RenderTexture<'subsystem> {
    #[tracing::instrument(name = "RenderTexture::new", skip(base_sys), err(Display))]
    pub fn new(
        base_sys: &AppBaseSystem<'subsystem>,
        size: br::Extent2D,
        format: PixelFormat,
        options: &RenderTextureOptions,
    ) -> br::Result<RenderTexture<'subsystem>> {
        let mut create_info =
            br::ImageCreateInfo::new(size, format.vk_format()).as_color_attachment();
        if options
            .flags
            .contains(RenderTextureFlags::ALLOW_TRANSFER_SRC)
        {
            create_info = create_info.usage_with(br::ImageUsageFlags::TRANSFER_SRC);
        }
        if !options.flags.contains(RenderTextureFlags::NON_SAMPLED) {
            create_info = create_info.usage_with(br::ImageUsageFlags::SAMPLED);
        }
        if let Some(msaa_count) = options.msaa_count {
            create_info = create_info.sample_counts(msaa_count);
        }

        let mut img = br::ImageObject::new(base_sys.subsystem, &create_info)?;
        let mem = base_sys.alloc_device_local_memory_for_requirements(&img.requirements());
        img.bind(&mem, 0)?;

        Ok(Self {
            res: br::ImageViewBuilder::new(
                img,
                br::ImageSubresourceRange::new(format.aspect_mask(), 0..1, 0..1),
            )
            .create()?,
            mem,
            size,
            pixel_format: format,
            msaa_count: options.msaa_count,
        })
    }

    pub fn make_attachment_description(&self) -> br::AttachmentDescription2 {
        let mut d = br::AttachmentDescription2::new(self.pixel_format.vk_format());
        if let Some(c) = self.msaa_count {
            d = d.samples(c);
        }

        d
    }

    pub const fn as_image(&self) -> &RenderTextureImageAccess<'subsystem> {
        unsafe { core::mem::transmute(self) }
    }

    pub const fn render_region(&self) -> br::Rect2D {
        self.size.into_rect(br::Offset2D::ZERO)
    }
}

#[repr(transparent)]
pub struct RenderTextureImageAccess<'subsystem>(RenderTexture<'subsystem>);
impl<'subsystem> br::VkHandle for RenderTextureImageAccess<'subsystem> {
    type Handle = br::vk::VkImage;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.0.res.image().native_ptr()
    }
}
impl<'subsystem> br::VkHandleMut for RenderTextureImageAccess<'subsystem> {
    #[inline(always)]
    fn native_ptr_mut(&mut self) -> Self::Handle {
        self.0.res.image_mut().native_ptr_mut()
    }
}
impl<'subsystem> br::DeviceChildHandle for RenderTextureImageAccess<'subsystem> {
    #[inline(always)]
    fn device_handle(&self) -> bedrock::vk::VkDevice {
        self.0.res.image().device_handle()
    }
}
impl<'subsystem> br::DeviceChild for RenderTextureImageAccess<'subsystem> {
    type ConcreteDevice = &'subsystem Subsystem;

    #[inline(always)]
    fn device(&self) -> &Self::ConcreteDevice {
        self.0.res.image().device()
    }
}
