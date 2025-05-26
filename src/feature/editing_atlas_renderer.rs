use bedrock::{self as br, DescriptorPoolMut, Device, MemoryBound, ShaderModule, VkHandle};

use crate::{
    BLEND_STATE_SINGLE_NONE, IA_STATE_TRILIST, MS_STATE_EMPTY, RASTER_STATE_DEFAULT_FILL_NOCULL,
    VI_STATE_EMPTY, subsystem::Subsystem,
};

#[repr(C)]
struct GridParams {
    pub offset: [f32; 2],
    pub size: [f32; 2],
}

pub struct EditingAtlasRenderer<'d> {
    pub param_buffer: br::BufferObject<&'d Subsystem>,
    _param_buffer_memory: br::DeviceMemoryObject<&'d Subsystem>,
    current_params_data: GridParams,
    param_is_dirty: bool,
    grid_vsh: br::ShaderModuleObject<&'d Subsystem>,
    grid_fsh: br::ShaderModuleObject<&'d Subsystem>,
    pub render_pipeline_layout: br::PipelineLayoutObject<&'d Subsystem>,
    pub render_pipeline: br::PipelineObject<&'d Subsystem>,
    _dsl_param: br::DescriptorSetLayoutObject<&'d Subsystem>,
    _dp: br::DescriptorPoolObject<&'d Subsystem>,
    pub ds_param: br::DescriptorSet,
}
impl<'d> EditingAtlasRenderer<'d> {
    pub fn new(
        subsystem: &'d Subsystem,
        rendered_pass: br::SubpassRef<impl br::RenderPass>,
        main_buffer_size: br::Extent2D,
    ) -> Self {
        let mut param_buffer = br::BufferObject::new(
            subsystem,
            &br::BufferCreateInfo::new_for_type::<GridParams>(
                br::BufferUsage::UNIFORM_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        )
        .unwrap();
        let mreq = param_buffer.requirements();
        let memindex = subsystem
            .adapter_memory_info
            .find_device_local_index(mreq.memoryTypeBits)
            .expect("no suitable memory property");
        let param_buffer_memory = br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(mreq.size, memindex),
        )
        .unwrap();
        param_buffer.bind(&param_buffer_memory, 0).unwrap();

        let dsl_param = br::DescriptorSetLayoutObject::new(
            subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[
                br::DescriptorType::UniformBuffer.make_binding(0, 1)
            ]),
        )
        .unwrap();
        let mut dp = br::DescriptorPoolObject::new(
            subsystem,
            &br::DescriptorPoolCreateInfo::new(
                1,
                &[br::DescriptorType::UniformBuffer.make_size(1)],
            ),
        )
        .unwrap();
        let [ds_param] = dp.alloc_array(&[dsl_param.as_transparent_ref()]).unwrap();
        subsystem.update_descriptor_sets(
            &[ds_param
                .binding_at(0)
                .write(br::DescriptorContents::uniform_buffer(
                    &param_buffer,
                    0..core::mem::size_of::<GridParams>() as _,
                ))],
            &[],
        );

        let vsh = subsystem.load_shader("resources/filltri.vert").unwrap();
        let fsh = subsystem.load_shader("resources/grid.frag").unwrap();

        let render_pipeline_layout = br::PipelineLayoutObject::new(
            subsystem,
            &br::PipelineLayoutCreateInfo::new(
                &[dsl_param.as_transparent_ref()],
                &[br::PushConstantRange::for_type::<[f32; 2]>(
                    br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
                    0,
                )],
            ),
        )
        .unwrap();
        let [render_pipeline] = subsystem
            .new_graphics_pipeline_array(
                &[br::GraphicsPipelineCreateInfo::new(
                    &render_pipeline_layout,
                    rendered_pass,
                    &[
                        vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[main_buffer_size
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[main_buffer_size.into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(MS_STATE_EMPTY)],
                None::<&br::PipelineCacheObject<&'d br::DeviceObject<&'d br::InstanceObject>>>,
            )
            .unwrap();

        Self {
            param_buffer,
            _param_buffer_memory: param_buffer_memory,
            current_params_data: GridParams {
                offset: [0.0, 0.0],
                size: [64.0, 64.0],
            },
            param_is_dirty: true,
            _dsl_param: dsl_param,
            _dp: dp,
            ds_param,
            grid_vsh: vsh,
            grid_fsh: fsh,
            render_pipeline_layout,
            render_pipeline,
        }
    }

    pub const fn offset(&self) -> [f32; 2] {
        self.current_params_data.offset
    }

    pub fn set_offset(&mut self, x: f32, y: f32) {
        self.current_params_data.offset = [x, y];
        self.param_is_dirty = true;
    }

    pub const fn is_dirty(&self) -> bool {
        self.param_is_dirty
    }

    pub fn process_dirty_data<'c, E>(&mut self, rec: br::CmdRecord<'c, E>) -> br::CmdRecord<'c, E> {
        if !self.param_is_dirty {
            return rec;
        }

        self.param_is_dirty = false;
        rec.update_buffer(
            &self.param_buffer,
            0,
            core::mem::size_of::<GridParams>() as _,
            &self.current_params_data,
        )
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[br::MemoryBarrier2::new()
                .from(
                    br::PipelineStageFlags2::COPY,
                    br::AccessFlags2::TRANSFER.write,
                )
                .to(
                    br::PipelineStageFlags2::FRAGMENT_SHADER,
                    br::AccessFlags2::SHADER.read,
                )],
            &[],
            &[],
        ))
    }

    pub fn recreate(
        &mut self,
        device: &'d Subsystem,
        rendered_pass: br::SubpassRef<impl br::RenderPass>,
        main_buffer_size: br::Extent2D,
    ) {
        let [render_pipeline] = device
            .new_graphics_pipeline_array(
                &[br::GraphicsPipelineCreateInfo::new(
                    &self.render_pipeline_layout,
                    rendered_pass,
                    &[
                        self.grid_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        self.grid_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[main_buffer_size
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[main_buffer_size.into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(MS_STATE_EMPTY)],
                None::<&br::PipelineCacheObject<&'d br::DeviceObject<&'d br::InstanceObject>>>,
            )
            .unwrap();

        self.render_pipeline = render_pipeline;
    }
}
