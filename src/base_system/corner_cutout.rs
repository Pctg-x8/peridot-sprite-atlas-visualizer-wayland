use bedrock::{
    self as br, DescriptorPoolMut, Device, MemoryBound, RenderPass, ShaderModule, VkHandle,
};

use crate::{
    BLEND_STATE_SINGLE_NONE, IA_STATE_TRILIST, IA_STATE_TRISTRIP, MS_STATE_EMPTY,
    RASTER_STATE_DEFAULT_FILL_NOCULL, VI_STATE_EMPTY, atlas::AtlasRect, subsystem::Subsystem,
};

use super::{
    AppBaseSystem, RenderPassOptions, inject_cmd_begin_render_pass2, inject_cmd_end_render_pass2,
    inject_cmd_pipeline_barrier_2,
};

pub struct WindowCornerCutoutRenderer<'subsystem> {
    atlas_rect: AtlasRect,
    pipeline_layout: br::PipelineLayoutObject<&'subsystem Subsystem>,
    pipeline: br::PipelineObject<&'subsystem Subsystem>,
    pipeline_cont: br::PipelineObject<&'subsystem Subsystem>,
    vbuf: br::BufferObject<&'subsystem Subsystem>,
    _vbuf_memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    _input_dsl: br::DescriptorSetLayoutObject<&'subsystem Subsystem>,
    _dp: br::DescriptorPoolObject<&'subsystem Subsystem>,
    input_descriptor_set: br::DescriptorSet,
    _sampler: br::SamplerObject<&'subsystem Subsystem>,
}
impl<'subsystem> WindowCornerCutoutRenderer<'subsystem> {
    const VI_STATE: &'static br::PipelineVertexInputStateCreateInfo<'static> =
        &br::PipelineVertexInputStateCreateInfo::new(
            &[const { br::VertexInputBindingDescription::per_instance_typed::<[[f32; 2]; 2]>(0) }],
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
        );
    const BLEND_STATE: &'static br::PipelineColorBlendStateCreateInfo<'static> =
        &br::PipelineColorBlendStateCreateInfo::new(&[
            br::vk::VkPipelineColorBlendAttachmentState {
                // simply overwrite alpha
                blendEnable: true as _,
                srcColorBlendFactor: br::vk::VK_BLEND_FACTOR_ZERO,
                dstColorBlendFactor: br::vk::VK_BLEND_FACTOR_SRC_ALPHA,
                colorBlendOp: br::vk::VK_BLEND_OP_ADD,
                srcAlphaBlendFactor: br::vk::VK_BLEND_FACTOR_ONE,
                dstAlphaBlendFactor: br::vk::VK_BLEND_FACTOR_ZERO,
                alphaBlendOp: br::vk::VK_BLEND_OP_ADD,
                colorWriteMask: br::vk::VK_COLOR_COMPONENT_A_BIT
                    | br::vk::VK_COLOR_COMPONENT_B_BIT
                    | br::vk::VK_COLOR_COMPONENT_G_BIT
                    | br::vk::VK_COLOR_COMPONENT_R_BIT,
            },
        ]);

    #[tracing::instrument(
        name = "WindowCornerCutoutRenderer::new",
        skip(base_system, rendered_subpass, rendered_subpass_cont)
    )]
    pub fn new(
        base_system: &mut AppBaseSystem<'subsystem>,
        rt_size: br::Extent2D,
        rendered_subpass: br::SubpassRef<impl br::VkHandle<Handle = br::vk::VkRenderPass> + ?Sized>,
        rendered_subpass_cont: br::SubpassRef<
            impl br::VkHandle<Handle = br::vk::VkRenderPass> + ?Sized,
        >,
    ) -> Self {
        let sampler =
            br::SamplerObject::new(base_system.subsystem, &br::SamplerCreateInfo::new()).unwrap();

        let atlas_rect = base_system.alloc_mask_atlas_rect(32, 32);

        let rp = base_system
            .render_to_mask_atlas_pass(RenderPassOptions::FULL_PIXEL_RENDER)
            .unwrap();
        let fb = br::FramebufferObject::new(
            base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp,
                &[base_system
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                base_system.mask_atlas_size(),
                base_system.mask_atlas_size(),
            ),
        )
        .unwrap();
        let vsh = base_system.require_shader("resources/filltri.vert");
        let fsh = base_system.require_shader("resources/corner_cutout.frag");
        let [pipeline] = base_system
            .create_graphics_pipelines_array(&[br::GraphicsPipelineCreateInfo::new(
                base_system.require_empty_pipeline_layout(),
                rp.subpass(0),
                &[
                    vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                    fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                ],
                VI_STATE_EMPTY,
                IA_STATE_TRILIST,
                &br::PipelineViewportStateCreateInfo::new_array(
                    &[atlas_rect
                        .extent()
                        .into_rect(br::Offset2D::ZERO)
                        .make_viewport(0.0..1.0)],
                    &[atlas_rect.extent().into_rect(br::Offset2D::ZERO)],
                ),
                RASTER_STATE_DEFAULT_FILL_NOCULL,
                BLEND_STATE_SINGLE_NONE,
            )
            .set_multisample_state(MS_STATE_EMPTY)])
            .unwrap();
        base_system
            .sync_execute_graphics_commands(|rec| {
                rec.inject(|r| {
                    inject_cmd_begin_render_pass2(
                        r,
                        base_system.subsystem,
                        &br::RenderPassBeginInfo::new(&rp, &fb, atlas_rect.vk_rect(), &[]),
                        &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                    )
                })
                .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
                .draw(3, 1, 0, 0)
                .inject(|r| {
                    inject_cmd_end_render_pass2(
                        r,
                        base_system.subsystem,
                        &br::SubpassEndInfo::new(),
                    )
                })
            })
            .unwrap();
        drop(pipeline);

        let input_dsl = br::DescriptorSetLayoutObject::new(
            base_system.subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
                .make_binding(0, 1)
                .with_immutable_samplers(&[sampler.as_transparent_ref()])]),
        )
        .unwrap();
        let pipeline_layout = br::PipelineLayoutObject::new(
            base_system.subsystem,
            &br::PipelineLayoutCreateInfo::new(&[input_dsl.as_transparent_ref()], &[]),
        )
        .unwrap();

        let vsh = base_system.require_shader("resources/corner_cutout_placement.vert");
        let fsh = base_system.require_shader("resources/blit_alphamask.frag");
        let vsh_param = CornerCutoutVshConstants {
            width_vp: 32.0 / rt_size.width as f32,
            height_vp: 32.0 / rt_size.height as f32,
            uv_scale_x: (atlas_rect.width() as f32 - 0.5) / base_system.mask_atlas_size() as f32,
            uv_scale_y: (atlas_rect.height() as f32 - 0.5) / base_system.mask_atlas_size() as f32,
            uv_trans_x: (atlas_rect.left as f32 + 0.5) / base_system.mask_atlas_size() as f32,
            uv_trans_y: (atlas_rect.top as f32 + 0.5) / base_system.mask_atlas_size() as f32,
        };
        let vsh_spec = br::SpecializationInfo::new(&vsh_param);
        let shader_stages = [
            vsh.on_stage(br::ShaderStage::Vertex, c"main")
                .with_specialization_info(&vsh_spec),
            fsh.on_stage(br::ShaderStage::Fragment, c"main"),
        ];
        let viewport = [rt_size
            .into_rect(br::Offset2D::ZERO)
            .make_viewport(0.0..1.0)];
        let scissor = [rt_size.into_rect(br::Offset2D::ZERO)];
        let viewport_state = br::PipelineViewportStateCreateInfo::new_array(&viewport, &scissor);
        let [pipeline, pipeline_cont] = base_system
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    &pipeline_layout,
                    rendered_subpass,
                    &shader_stages,
                    Self::VI_STATE,
                    IA_STATE_TRISTRIP,
                    &viewport_state,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    Self::BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &pipeline_layout,
                    rendered_subpass_cont,
                    &shader_stages,
                    Self::VI_STATE,
                    IA_STATE_TRISTRIP,
                    &viewport_state,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    Self::BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        let mut vbuf = br::BufferObject::new(
            base_system.subsystem,
            &br::BufferCreateInfo::new(
                core::mem::size_of::<[[f32; 2]; 2]>() * 4,
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        )
        .unwrap();
        let mem = base_system.alloc_device_local_memory_for_requirements(&vbuf.requirements());
        vbuf.bind(&mem, 0).unwrap();
        base_system
            .sync_execute_graphics_commands(|rec| {
                rec.update_buffer_exact(
                    &vbuf,
                    0,
                    &[
                        [[-1.0f32, -1.0], [1.0, 1.0]],
                        [[1.0f32, -1.0], [-1.0, 1.0]],
                        [[-1.0f32, 1.0], [1.0, -1.0]],
                        [[1.0f32, 1.0], [-1.0, -1.0]],
                    ],
                )
                .inject(|r| {
                    inject_cmd_pipeline_barrier_2(
                        r,
                        base_system.subsystem,
                        &br::DependencyInfo::new(
                            &[br::MemoryBarrier2::new()
                                .from(
                                    br::PipelineStageFlags2::COPY,
                                    br::AccessFlags2::TRANSFER.write,
                                )
                                .to(
                                    br::PipelineStageFlags2::VERTEX_INPUT,
                                    br::AccessFlags2::VERTEX_ATTRIBUTE_READ,
                                )],
                            &[],
                            &[],
                        ),
                    )
                })
            })
            .unwrap();

        let mut dp = br::DescriptorPoolObject::new(
            base_system.subsystem,
            &br::DescriptorPoolCreateInfo::new(
                1,
                &[br::DescriptorType::CombinedImageSampler.make_size(1)],
            ),
        )
        .unwrap();
        let [input_descriptor_set] = dp.alloc_array(&[input_dsl.as_transparent_ref()]).unwrap();
        base_system.subsystem.update_descriptor_sets(
            &[input_descriptor_set.binding_at(0).write(
                br::DescriptorContents::combined_image_sampler(
                    base_system.mask_atlas_resource_transparent_ref(),
                    br::ImageLayout::ShaderReadOnlyOpt,
                ),
            )],
            &[],
        );

        Self {
            atlas_rect,
            pipeline_layout,
            pipeline,
            pipeline_cont,
            vbuf,
            _vbuf_memory: mem,
            _input_dsl: input_dsl,
            _dp: dp,
            input_descriptor_set,
            _sampler: sampler,
        }
    }

    pub fn resize_rt(
        &mut self,
        base_system: &mut AppBaseSystem<'subsystem>,
        rt_size: br::Extent2D,
        rendered_subpass: br::SubpassRef<impl br::VkHandle<Handle = br::vk::VkRenderPass> + ?Sized>,
        rendered_subpass_cont: br::SubpassRef<
            impl br::VkHandle<Handle = br::vk::VkRenderPass> + ?Sized,
        >,
    ) {
        let vsh = base_system.require_shader("resources/corner_cutout_placement.vert");
        let fsh = base_system.require_shader("resources/blit_alphamask.frag");
        let vsh_param = CornerCutoutVshConstants {
            width_vp: 32.0 / rt_size.width as f32,
            height_vp: 32.0 / rt_size.height as f32,
            uv_scale_x: self.atlas_rect.width() as f32 / base_system.mask_atlas_size() as f32,
            uv_scale_y: self.atlas_rect.height() as f32 / base_system.mask_atlas_size() as f32,
            uv_trans_x: self.atlas_rect.left as f32 / base_system.mask_atlas_size() as f32,
            uv_trans_y: self.atlas_rect.top as f32 / base_system.mask_atlas_size() as f32,
        };
        let vsh_spec = br::SpecializationInfo::new(&vsh_param);
        let shader_stages = [
            vsh.on_stage(br::ShaderStage::Vertex, c"main")
                .with_specialization_info(&vsh_spec),
            fsh.on_stage(br::ShaderStage::Fragment, c"main"),
        ];
        let viewport = [rt_size
            .into_rect(br::Offset2D::ZERO)
            .make_viewport(0.0..1.0)];
        let scissor = [rt_size.into_rect(br::Offset2D::ZERO)];
        let viewport_state = br::PipelineViewportStateCreateInfo::new_array(&viewport, &scissor);
        let [pipeline, pipeline_cont] = base_system
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    &self.pipeline_layout,
                    rendered_subpass,
                    &shader_stages,
                    Self::VI_STATE,
                    IA_STATE_TRISTRIP,
                    &viewport_state,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    Self::BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &self.pipeline_layout,
                    rendered_subpass_cont,
                    &shader_stages,
                    Self::VI_STATE,
                    IA_STATE_TRISTRIP,
                    &viewport_state,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    Self::BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        self.pipeline = pipeline;
        self.pipeline_cont = pipeline_cont;
    }

    #[inline]
    pub fn populate_commands<'x>(
        &self,
        rec: br::CmdRecord<'x>,
        continued_pass: bool,
    ) -> br::CmdRecord<'x> {
        rec.bind_pipeline(
            br::PipelineBindPoint::Graphics,
            if continued_pass {
                &self.pipeline_cont
            } else {
                &self.pipeline
            },
        )
        .bind_descriptor_sets(
            br::PipelineBindPoint::Graphics,
            &self.pipeline_layout,
            0,
            &[self.input_descriptor_set],
            &[],
        )
        .bind_vertex_buffer_array(0, &[self.vbuf.as_transparent_ref()], &[0])
        .draw(4, 4, 0, 0)
    }
}

#[derive(br::SpecializationConstants)]
struct CornerCutoutVshConstants {
    #[constant_id = 0]
    width_vp: f32,
    #[constant_id = 1]
    height_vp: f32,
    #[constant_id = 2]
    uv_scale_x: f32,
    #[constant_id = 3]
    uv_scale_y: f32,
    #[constant_id = 4]
    uv_trans_x: f32,
    #[constant_id = 5]
    uv_trans_y: f32,
}
