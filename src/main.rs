mod fontconfig;
mod freetype;
mod harfbuzz;
mod hittest;
mod input;
mod linux_input_event_codes;
mod wl;

use std::{
    cell::Cell,
    collections::{BTreeSet, VecDeque},
    io::Read,
    path::Path,
    rc::Rc,
};

use ::fontconfig::{FC_FAMILY, FcMatchPattern};
use bedrock::{
    self as br, CommandBufferMut, CommandPoolMut, DescriptorPoolMut, Device, DeviceMemoryMut,
    Fence, FenceMut, Image, ImageChild, ImageSubresourceSlice, Instance, MemoryBound,
    PhysicalDevice, QueueMut, RenderPass, Swapchain, VkHandle, VkHandleMut, VulkanStructure,
};
use freetype::FreeType;
use freetype2::{
    FT_Bitmap, FT_Bool, FT_GlyphSlotRec, FT_LOAD_DEFAULT, FT_RENDER_MODE_LIGHT,
    FT_RENDER_MODE_NORMAL,
};
use hittest::{
    CursorShape, HitTestTreeActionHandler, HitTestTreeData, HitTestTreeManager, HitTestTreeRef,
};
use input::{EventContinueControl, PointerInputManager};
use linux_input_event_codes::BTN_LEFT;
use wl::{WpCursorShapeDeviceV1, WpCursorShapeDeviceV1Shape, WpCursorShapeManagerV1};

pub enum AppEvent {
    ToplevelWindowConfigure {
        width: u32,
        height: u32,
    },
    ToplevelWindowSurfaceConfigure {
        serial: u32,
    },
    ToplevelWindowClose,
    ToplevelWindowFrameTiming,
    MainWindowPointerMove {
        enter_serial: u32,
        surface_x: f32,
        surface_y: f32,
    },
    MainWindowPointerLeftDown {
        enter_serial: u32,
    },
    MainWindowPointerLeftUp {
        enter_serial: u32,
    },
}

fn load_spv_file(path: impl AsRef<Path>) -> std::io::Result<Vec<u32>> {
    let mut f = std::fs::File::open(path)?;
    let byte_length = f.metadata()?.len();
    assert!((byte_length & 0x03) == 0);
    let mut words = vec![0u32; byte_length as usize >> 2];
    f.read_exact(unsafe {
        core::slice::from_raw_parts_mut(words.as_mut_ptr() as *mut u8, words.len() << 2)
    })?;

    Ok(words)
}

const MS_STATE_EMPTY: &'static br::PipelineMultisampleStateCreateInfo =
    &br::PipelineMultisampleStateCreateInfo::new();
const BLEND_STATE_SINGLE_NONE: &'static br::PipelineColorBlendStateCreateInfo<'static> =
    &br::PipelineColorBlendStateCreateInfo::new(&[
        br::vk::VkPipelineColorBlendAttachmentState::NOBLEND,
    ]);
const RASTER_STATE_DEFAULT_FILL_NOCULL: &'static br::PipelineRasterizationStateCreateInfo =
    &br::PipelineRasterizationStateCreateInfo::new(
        br::PolygonMode::Fill,
        br::CullModeFlags::NONE,
        br::FrontFace::CounterClockwise,
    );
const IA_STATE_TRILIST: &'static br::PipelineInputAssemblyStateCreateInfo =
    &br::PipelineInputAssemblyStateCreateInfo::new(br::PrimitiveTopology::TriangleList);
const VI_STATE_EMPTY: &'static br::PipelineVertexInputStateCreateInfo<'static> =
    &br::PipelineVertexInputStateCreateInfo::new(&[], &[]);

pub struct FontSet {
    pub ui_default: freetype::Owned<freetype::Face>,
}

#[repr(C)]
struct AtlasViewGridParams {
    pub offset: [f32; 2],
    pub size: [f32; 2],
}

pub struct AtlasView<'d> {
    pub param_buffer: br::BufferObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    _param_buffer_memory: br::DeviceMemoryObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    grid_vsh: br::ShaderModuleObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    grid_fsh: br::ShaderModuleObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    pub render_pipeline_layout:
        br::PipelineLayoutObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    pub render_pipeline: br::PipelineObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    pub dsl_param: br::DescriptorSetLayoutObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    pub dp: br::DescriptorPoolObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    pub ds_param: br::DescriptorSet,
}
impl<'d> AtlasView<'d> {
    pub fn new(
        device: &'d br::DeviceObject<&'d br::InstanceObject>,
        adapter_memory_info: &br::MemoryProperties,
        graphics_queue_family_index: u32,
        graphics_queue: &mut impl br::QueueMut,
        rendered_pass: br::SubpassRef<impl br::RenderPass>,
        main_buffer_size: br::Extent2D,
    ) -> Self {
        let mut param_buffer = br::BufferObject::new(
            device,
            &br::BufferCreateInfo::new_for_type::<AtlasViewGridParams>(
                br::BufferUsage::UNIFORM_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        )
        .unwrap();
        let mreq = param_buffer.requirements();
        let memindex = adapter_memory_info
            .find_device_local_index(mreq.memoryTypeBits)
            .expect("no suitable memory property");
        let param_buffer_memory =
            br::DeviceMemoryObject::new(device, &br::MemoryAllocateInfo::new(mreq.size, memindex))
                .unwrap();
        param_buffer.bind(&param_buffer_memory, 0).unwrap();

        let mut param_buffer_stg = br::BufferObject::new(
            device,
            &br::BufferCreateInfo::new_for_type::<AtlasViewGridParams>(
                br::BufferUsage::TRANSFER_SRC,
            ),
        )
        .unwrap();
        let mreq = param_buffer_stg.requirements();
        let memindex = adapter_memory_info
            .find_host_visible_index(mreq.memoryTypeBits)
            .expect("no suitable memory property");
        let mut param_buffer_stg_memory =
            br::DeviceMemoryObject::new(device, &br::MemoryAllocateInfo::new(mreq.size, memindex))
                .unwrap();
        param_buffer_stg.bind(&param_buffer_stg_memory, 0).unwrap();
        let n = param_buffer_stg_memory.native_ptr();
        let ptr = param_buffer_stg_memory
            .map(0..core::mem::size_of::<AtlasViewGridParams>())
            .unwrap();
        unsafe {
            core::ptr::write(
                ptr.get_mut(0),
                AtlasViewGridParams {
                    offset: [0.0, 0.0],
                    size: [64.0, 64.0],
                },
            );
        }
        if !adapter_memory_info.is_coherent(memindex) {
            unsafe {
                device
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new(
                        &br::VkHandleRef::dangling(n),
                        0..core::mem::size_of::<AtlasViewGridParams>() as _,
                    )])
                    .unwrap();
            }
        }
        ptr.end();

        let mut cp = br::CommandPoolObject::new(
            device,
            &br::CommandPoolCreateInfo::new(graphics_queue_family_index),
        )
        .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            device,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(&br::CommandBufferBeginInfo::new(), device)
                .unwrap()
        }
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[br::MemoryBarrier2::new()
                .from(br::PipelineStageFlags2::HOST, br::AccessFlags2::HOST.write)
                .to(
                    br::PipelineStageFlags2::COPY,
                    br::AccessFlags2::TRANSFER.read,
                )],
            &[],
            &[],
        ))
        .copy_buffer(
            &param_buffer_stg,
            &param_buffer,
            &[br::BufferCopy::mirror_data::<AtlasViewGridParams>(0)],
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
        .end()
        .unwrap();
        graphics_queue
            .submit2(
                &[br::SubmitInfo2::new(
                    &[],
                    &[br::CommandBufferSubmitInfo::new(&cb)],
                    &[],
                )],
                None,
            )
            .unwrap();
        graphics_queue.wait().unwrap();

        let dsl_param = br::DescriptorSetLayoutObject::new(
            device,
            &br::DescriptorSetLayoutCreateInfo::new(&[
                br::DescriptorType::UniformBuffer.make_binding(0, 1)
            ]),
        )
        .unwrap();
        let mut dp = br::DescriptorPoolObject::new(
            device,
            &br::DescriptorPoolCreateInfo::new(
                1,
                &[br::DescriptorType::UniformBuffer.make_size(1)],
            ),
        )
        .unwrap();
        let [ds_param] = dp.alloc_array(&[dsl_param.as_transparent_ref()]).unwrap();
        device.update_descriptor_sets(
            &[ds_param
                .binding_at(0)
                .write(br::DescriptorContents::uniform_buffer(
                    &param_buffer,
                    0..core::mem::size_of::<AtlasViewGridParams>() as _,
                ))],
            &[],
        );

        let vsh = br::ShaderModuleObject::new(
            device,
            &br::ShaderModuleCreateInfo::new(&load_spv_file("resources/filltri.vert").unwrap()),
        )
        .unwrap();
        let fsh = br::ShaderModuleObject::new(
            device,
            &br::ShaderModuleCreateInfo::new(&load_spv_file("resources/grid.frag").unwrap()),
        )
        .unwrap();

        let render_pipeline_layout = br::PipelineLayoutObject::new(
            device,
            &br::PipelineLayoutCreateInfo::new(
                &[dsl_param.as_transparent_ref()],
                &[br::vk::VkPushConstantRange::for_type::<[f32; 2]>(
                    br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
                    0,
                )],
            ),
        )
        .unwrap();
        let [render_pipeline] = device
            .new_graphics_pipeline_array(
                &[br::GraphicsPipelineCreateInfo::new(
                    &render_pipeline_layout,
                    rendered_pass,
                    &[
                        br::PipelineShaderStage::new(br::ShaderStage::Vertex, &vsh, c"main"),
                        br::PipelineShaderStage::new(br::ShaderStage::Fragment, &fsh, c"main"),
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
            dsl_param,
            dp,
            ds_param,
            grid_vsh: vsh,
            grid_fsh: fsh,
            render_pipeline_layout,
            render_pipeline,
        }
    }

    pub fn recreate(
        &mut self,
        device: &'d br::DeviceObject<&'d br::InstanceObject>,
        rendered_pass: br::SubpassRef<impl br::RenderPass>,
        main_buffer_size: br::Extent2D,
    ) {
        let [render_pipeline] = device
            .new_graphics_pipeline_array(
                &[br::GraphicsPipelineCreateInfo::new(
                    &self.render_pipeline_layout,
                    rendered_pass,
                    &[
                        br::PipelineShaderStage::new(
                            br::ShaderStage::Vertex,
                            &self.grid_vsh,
                            c"main",
                        ),
                        br::PipelineShaderStage::new(
                            br::ShaderStage::Fragment,
                            &self.grid_fsh,
                            c"main",
                        ),
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

pub struct SpriteListPaneView {
    pub frame_image_atlas_rect: AtlasRect,
    pub title_atlas_rect: AtlasRect,
    pub title_blurred_atlas_rect: AtlasRect,
    ct_root: usize,
    ht_frame: HitTestTreeRef,
    ht_resize_area: HitTestTreeRef,
    width: Cell<f32>,
    ui_scale_factor: Cell<f32>,
}
impl SpriteListPaneView {
    const CORNER_RADIUS: f32 = 24.0;
    const BLUR_AMOUNT_ONEDIR: u32 = 8;
    const FLOATING_MARGIN: f32 = 8.0;
    const INIT_WIDTH: f32 = 320.0;
    const RESIZE_AREA_WIDTH: f32 = 8.0;

    pub fn new(
        device: &br::DeviceObject<&br::InstanceObject>,
        adapter_memory_info: &br::MemoryProperties,
        graphics_queue_family_index: u32,
        graphics_queue: &mut impl br::QueueMut,
        atlas: &mut CompositionSurfaceAtlas,
        bitmap_scale: u32,
        header_height: f32,
        fonts: &mut FontSet,
        composite_tree: &mut CompositeTree,
        composite_instance_manager: &mut CompositeInstanceManager,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
    ) -> Self {
        let render_size_px = ((Self::CORNER_RADIUS * 2.0 + 1.0) * bitmap_scale as f32) as u32;
        let frame_image_atlas_rect = atlas.alloc(render_size_px, render_size_px);

        let title_blur_pixels = Self::BLUR_AMOUNT_ONEDIR * bitmap_scale;
        let title_layout = TextLayout::build_simple("Sprites", &mut fonts.ui_default);
        let title_atlas_rect = atlas.alloc(title_layout.width_px(), title_layout.height_px());
        let title_blurred_atlas_rect = atlas.alloc(
            title_layout.width_px() + (title_blur_pixels * 2 + 1),
            title_layout.height_px() + (title_blur_pixels * 2 + 1),
        );
        let (title_stg_image, _title_stg_image_mem) =
            title_layout.build_stg_image(device, adapter_memory_info);

        let mut title_blurred_work_image = br::ImageObject::new(
            device,
            &br::ImageCreateInfo::new(
                title_blurred_atlas_rect.extent(),
                br::vk::VK_FORMAT_R8_UNORM,
            )
            .as_color_attachment()
            .sampled(),
        )
        .unwrap();
        let mreq = title_blurred_work_image.requirements();
        let memindex = adapter_memory_info
            .find_device_local_index(mreq.memoryTypeBits)
            .expect("no suitable memory index");
        let title_blurred_work_image_mem =
            br::DeviceMemoryObject::new(device, &br::MemoryAllocateInfo::new(mreq.size, memindex))
                .unwrap();
        title_blurred_work_image
            .bind(&title_blurred_work_image_mem, 0)
            .unwrap();
        let title_blurred_work_image_view = title_blurred_work_image
            .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
            .view_builder()
            .create()
            .unwrap();

        let render_pass = br::RenderPassObject::new(
            device,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(br::vk::VK_FORMAT_R8_UNORM)
                    .layout_transition(
                        br::ImageLayout::Undefined,
                        br::ImageLayout::ShaderReadOnlyOpt,
                    )
                    .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)],
                &[
                    br::SubpassDescription2::new().colors(&[br::AttachmentReference2::color(
                        0,
                        br::ImageLayout::ColorAttachmentOpt,
                    )]),
                ],
                &[br::SubpassDependency2::new(
                    br::SubpassIndex::Internal(0),
                    br::SubpassIndex::External,
                )
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
        .unwrap();
        let framebuffer = br::FramebufferObject::new(
            device,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[atlas.resource.as_transparent_ref()],
                atlas.size,
                atlas.size,
            ),
        )
        .unwrap();
        let title_blurred_work_framebuffer = br::FramebufferObject::new(
            device,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[title_blurred_work_image_view.as_transparent_ref()],
                title_blurred_atlas_rect.width(),
                title_blurred_atlas_rect.height(),
            ),
        )
        .unwrap();

        let vsh = br::ShaderModuleObject::new(
            device,
            &br::ShaderModuleCreateInfo::new(&load_spv_file("resources/filltri.vert").unwrap()),
        )
        .unwrap();
        let fsh = br::ShaderModuleObject::new(
            device,
            &br::ShaderModuleCreateInfo::new(
                &load_spv_file("resources/rounded_rect.frag").unwrap(),
            ),
        )
        .unwrap();
        let vsh_blur = br::ShaderModuleObject::new(
            device,
            &br::ShaderModuleCreateInfo::new(
                &load_spv_file("resources/filltri_uvmod.vert").unwrap(),
            ),
        )
        .unwrap();
        let fsh_blur = br::ShaderModuleObject::new(
            device,
            &br::ShaderModuleCreateInfo::new(
                &load_spv_file("resources/blit_axis_convolved.frag").unwrap(),
            ),
        )
        .unwrap();

        let dsl_tex1 = br::DescriptorSetLayoutObject::new(
            device,
            &br::DescriptorSetLayoutCreateInfo::new(&[
                br::DescriptorType::CombinedImageSampler.make_binding(0, 1)
            ]),
        )
        .unwrap();
        let smp = br::SamplerObject::new(device, &br::SamplerCreateInfo::new()).unwrap();
        let mut dp = br::DescriptorPoolObject::new(
            device,
            &br::DescriptorPoolCreateInfo::new(
                2,
                &[br::DescriptorType::CombinedImageSampler.make_size(2)],
            ),
        )
        .unwrap();
        let [ds_title, ds_title2] = dp
            .alloc_array(&[dsl_tex1.as_transparent_ref(), dsl_tex1.as_transparent_ref()])
            .unwrap();
        device.update_descriptor_sets(
            &[
                ds_title
                    .binding_at(0)
                    .write(br::DescriptorContents::CombinedImageSampler(vec![
                        br::DescriptorImageInfo::new(
                            &atlas.resource.as_transparent_ref(),
                            br::ImageLayout::ShaderReadOnlyOpt,
                        )
                        .with_sampler(&smp),
                    ])),
                ds_title2
                    .binding_at(0)
                    .write(br::DescriptorContents::CombinedImageSampler(vec![
                        br::DescriptorImageInfo::new(
                            &title_blurred_work_image_view.as_transparent_ref(),
                            br::ImageLayout::ShaderReadOnlyOpt,
                        )
                        .with_sampler(&smp),
                    ])),
            ],
            &[],
        );

        let pipeline_layout =
            br::PipelineLayoutObject::new(device, &br::PipelineLayoutCreateInfo::new(&[], &[]))
                .unwrap();
        let blur_pipeline_layout = br::PipelineLayoutObject::new(
            device,
            &br::PipelineLayoutCreateInfo::new(
                &[dsl_tex1.as_transparent_ref()],
                &[
                    br::vk::VkPushConstantRange::for_type::<[f32; 4]>(
                        br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                        0,
                    ),
                    br::vk::VkPushConstantRange::new(
                        br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
                        core::mem::size_of::<[f32; 4]>() as _
                            ..(core::mem::size_of::<[f32; 4]>()
                                + core::mem::size_of::<[f32; 4]>()
                                + core::mem::size_of::<[f32; 2]>()
                                + (core::mem::size_of::<f32>() * title_blur_pixels as usize))
                                as _,
                    ),
                ],
            ),
        )
        .unwrap();
        let [pipeline, pipeline_blur1, pipeline_blur] = device
            .new_graphics_pipeline_array(
                &[
                    br::GraphicsPipelineCreateInfo::new(
                        &pipeline_layout,
                        render_pass.subpass(0),
                        &[
                            br::PipelineShaderStage::new(br::ShaderStage::Vertex, &vsh, c"main"),
                            br::PipelineShaderStage::new(br::ShaderStage::Fragment, &fsh, c"main"),
                        ],
                        VI_STATE_EMPTY,
                        IA_STATE_TRILIST,
                        &br::PipelineViewportStateCreateInfo::new(
                            &[frame_image_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                            &[frame_image_atlas_rect.vk_rect()],
                        ),
                        RASTER_STATE_DEFAULT_FILL_NOCULL,
                        BLEND_STATE_SINGLE_NONE,
                    )
                    .multisample_state(MS_STATE_EMPTY),
                    br::GraphicsPipelineCreateInfo::new(
                        &blur_pipeline_layout,
                        render_pass.subpass(0),
                        &[
                            br::PipelineShaderStage::new(
                                br::ShaderStage::Vertex,
                                &vsh_blur,
                                c"main",
                            ),
                            br::PipelineShaderStage::new(
                                br::ShaderStage::Fragment,
                                &fsh_blur,
                                c"main",
                            )
                            .with_specialization_info(
                                &br::SpecializationInfo::from_any_type(
                                    &[br::vk::VkSpecializationMapEntry::for_type::<u32>(0, 0)],
                                    &title_blur_pixels,
                                ),
                            ),
                        ],
                        VI_STATE_EMPTY,
                        IA_STATE_TRILIST,
                        &br::PipelineViewportStateCreateInfo::new(
                            &[title_blurred_atlas_rect
                                .extent()
                                .into_rect(br::Offset2D::ZERO)
                                .make_viewport(0.0..1.0)],
                            &[title_blurred_atlas_rect
                                .extent()
                                .into_rect(br::Offset2D::ZERO)],
                        ),
                        RASTER_STATE_DEFAULT_FILL_NOCULL,
                        BLEND_STATE_SINGLE_NONE,
                    )
                    .multisample_state(MS_STATE_EMPTY),
                    br::GraphicsPipelineCreateInfo::new(
                        &blur_pipeline_layout,
                        render_pass.subpass(0),
                        &[
                            br::PipelineShaderStage::new(
                                br::ShaderStage::Vertex,
                                &vsh_blur,
                                c"main",
                            ),
                            br::PipelineShaderStage::new(
                                br::ShaderStage::Fragment,
                                &fsh_blur,
                                c"main",
                            )
                            .with_specialization_info(
                                &br::SpecializationInfo::from_any_type(
                                    &[br::vk::VkSpecializationMapEntry::for_type::<u32>(0, 0)],
                                    &title_blur_pixels,
                                ),
                            ),
                        ],
                        VI_STATE_EMPTY,
                        IA_STATE_TRILIST,
                        &br::PipelineViewportStateCreateInfo::new(
                            &[title_blurred_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                            &[title_blurred_atlas_rect.vk_rect()],
                        ),
                        RASTER_STATE_DEFAULT_FILL_NOCULL,
                        BLEND_STATE_SINGLE_NONE,
                    )
                    .multisample_state(MS_STATE_EMPTY),
                ],
                None::<&br::PipelineCacheObject<&br::DeviceObject<&br::InstanceObject>>>,
            )
            .unwrap();

        fn gauss_distrib(x: f32, p: f32) -> f32 {
            (core::f32::consts::TAU * p.powi(2)).sqrt().recip()
                * (-x.powi(2) / (2.0 * p.powi(2))).exp()
        }
        let mut fsh_h_params = vec![0.0f32; title_blur_pixels as usize + 6];
        let mut fsh_v_params = vec![0.0f32; title_blur_pixels as usize + 6];
        // uv_limit
        fsh_h_params[0] = title_atlas_rect.left as f32 / atlas.size as f32;
        fsh_h_params[1] = title_atlas_rect.top as f32 / atlas.size as f32;
        fsh_h_params[2] = title_atlas_rect.right as f32 / atlas.size as f32;
        fsh_h_params[3] = title_atlas_rect.bottom as f32 / atlas.size as f32;
        fsh_v_params[2] = 1.0;
        fsh_v_params[3] = 1.0;
        // uv_step
        fsh_h_params[4] = 1.0 / atlas.size as f32;
        fsh_v_params[5] = 1.0 / title_blurred_atlas_rect.height() as f32;
        // factors
        let mut t = 0.0;
        for n in 0..title_blur_pixels as usize {
            let v = gauss_distrib(n as f32, title_blur_pixels as f32 / 3.0);
            fsh_h_params[n + 6] = v;
            fsh_v_params[n + 6] = v;

            t += v;
        }
        for n in 0..title_blur_pixels as usize {
            fsh_h_params[n + 6] /= t;
            fsh_v_params[n + 6] /= t;
        }

        let mut cp = br::CommandPoolObject::new(
            device,
            &br::CommandPoolCreateInfo::new(graphics_queue_family_index).transient(),
        )
        .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            device,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(&br::CommandBufferBeginInfo::new(), device)
                .unwrap()
        }
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[
                title_stg_image
                    .by_ref()
                    .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
                    .memory_barrier2()
                    .from(br::PipelineStageFlags2::HOST, br::AccessFlags2::HOST.write)
                    .to(
                        br::PipelineStageFlags2::COPY,
                        br::AccessFlags2::TRANSFER.read,
                    )
                    .transit_to(br::ImageLayout::TransferSrcOpt.from_undefined()),
                atlas
                    .resource
                    .image()
                    .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
                    .memory_barrier2()
                    .transit_to(br::ImageLayout::TransferDestOpt.from_undefined()),
            ],
        ))
        .copy_image(
            &title_stg_image,
            br::ImageLayout::TransferSrcOpt,
            atlas.resource.image(),
            br::ImageLayout::TransferDestOpt,
            &[br::vk::VkImageCopy {
                srcSubresource: br::ImageSubresourceLayers::new(br::AspectMask::COLOR, 0, 0..1),
                srcOffset: br::Offset3D::ZERO,
                dstSubresource: br::ImageSubresourceLayers::new(br::AspectMask::COLOR, 0, 0..1),
                dstOffset: title_atlas_rect.lt_offset().with_z(0),
                extent: title_atlas_rect.extent().with_depth(1),
            }],
        )
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[atlas
                .resource
                .image()
                .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
                .memory_barrier2()
                .from(
                    br::PipelineStageFlags2::COPY,
                    br::AccessFlags2::TRANSFER.write,
                )
                .to(
                    br::PipelineStageFlags2::FRAGMENT_SHADER,
                    br::AccessFlags2::SHADER_SAMPLED_READ,
                )
                .transit_from(
                    br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt),
                )],
        ))
        .begin_render_pass2(
            &br::RenderPassBeginInfo::new(
                &render_pass,
                &framebuffer,
                frame_image_atlas_rect.vk_rect(),
                &[br::ClearValue::color_f32([0.0; 4])],
            ),
            &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
        )
        .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
        .draw(3, 1, 0, 0)
        .end_render_pass2(&br::SubpassEndInfo::new())
        .begin_render_pass2(
            &br::RenderPassBeginInfo::new(
                &render_pass,
                &title_blurred_work_framebuffer,
                title_blurred_atlas_rect
                    .extent()
                    .into_rect(br::Offset2D::ZERO),
                &[br::ClearValue::color_f32([0.0; 4])],
            ),
            &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
        )
        .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline_blur1)
        .bind_descriptor_sets(
            br::PipelineBindPoint::Graphics,
            &blur_pipeline_layout,
            0,
            &[ds_title],
            &[],
        )
        .push_constant(
            &blur_pipeline_layout,
            br::vk::VK_SHADER_STAGE_VERTEX_BIT,
            0,
            &[
                ((title_atlas_rect.width() + title_blur_pixels * 2 + 1) as f32 / atlas.size as f32),
                ((title_atlas_rect.height() + title_blur_pixels * 2 + 1) as f32
                    / atlas.size as f32),
                ((title_atlas_rect.left as f32 - title_blur_pixels as f32) / atlas.size as f32),
                ((title_atlas_rect.top as f32 - title_blur_pixels as f32) / atlas.size as f32),
            ],
        )
        .push_constant_slice(
            &blur_pipeline_layout,
            br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
            core::mem::size_of::<[f32; 4]>() as _,
            &fsh_h_params,
        )
        .draw(3, 1, 0, 0)
        .end_render_pass2(&br::SubpassEndInfo::new())
        .begin_render_pass2(
            &br::RenderPassBeginInfo::new(
                &render_pass,
                &framebuffer,
                title_blurred_atlas_rect.vk_rect(),
                &[br::ClearValue::color_f32([0.0; 4])],
            ),
            &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
        )
        .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline_blur)
        .bind_descriptor_sets(
            br::PipelineBindPoint::Graphics,
            &blur_pipeline_layout,
            0,
            &[ds_title2],
            &[],
        )
        .push_constant(
            &blur_pipeline_layout,
            br::vk::VK_SHADER_STAGE_VERTEX_BIT,
            0,
            &[1.0f32, 1.0, 0.0, 0.0],
        )
        .push_constant_slice(
            &blur_pipeline_layout,
            br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
            core::mem::size_of::<[f32; 4]>() as _,
            &fsh_v_params,
        )
        .draw(3, 1, 0, 0)
        .end_render_pass2(&br::SubpassEndInfo::new())
        .end()
        .unwrap();

        graphics_queue
            .submit2(
                &[br::SubmitInfo2::new(
                    &[],
                    &[br::CommandBufferSubmitInfo::new(&cb)],
                    &[],
                )],
                None,
            )
            .unwrap();
        graphics_queue.wait().unwrap();

        let ct_root = composite_tree.alloc();
        {
            let ct_root = composite_tree.get_mut(ct_root);

            ct_root.instance_slot_index = Some(composite_instance_manager.alloc());
            ct_root.offset = [
                Self::FLOATING_MARGIN * bitmap_scale as f32,
                header_height * bitmap_scale as f32,
            ];
            ct_root.size = [
                Self::INIT_WIDTH * bitmap_scale as f32,
                -(header_height + Self::FLOATING_MARGIN) * bitmap_scale as f32,
            ];
            ct_root.relative_size_adjustment = [0.0, 1.0];
            ct_root.texatlas_rect = frame_image_atlas_rect.clone();
            ct_root.slice_borders = [
                Self::CORNER_RADIUS * bitmap_scale as f32,
                Self::CORNER_RADIUS * bitmap_scale as f32,
                Self::CORNER_RADIUS * bitmap_scale as f32,
                Self::CORNER_RADIUS * bitmap_scale as f32,
            ];
            ct_root.composite_mode = CompositeMode::ColorTint([1.0, 1.0, 1.0, 0.5]);
        }
        let ct_title_blurred = composite_tree.alloc();
        composite_tree.add_child(ct_root, ct_title_blurred);
        {
            let ct_title_blurred = composite_tree.get_mut(ct_title_blurred);

            ct_title_blurred.instance_slot_index = Some(composite_instance_manager.alloc());
            ct_title_blurred.offset = [
                -(title_blurred_atlas_rect.width() as f32 * 0.5),
                (8.0 - Self::BLUR_AMOUNT_ONEDIR as f32) * bitmap_scale as f32,
            ];
            ct_title_blurred.relative_offset_adjustment = [0.5, 0.0];
            ct_title_blurred.size = [
                title_blurred_atlas_rect.width() as f32,
                title_blurred_atlas_rect.height() as f32,
            ];
            ct_title_blurred.texatlas_rect = title_blurred_atlas_rect.clone();
            ct_title_blurred.composite_mode = CompositeMode::ColorTint([0.9, 0.9, 0.9, 1.0]);
        }
        let ct_title = composite_tree.alloc();
        composite_tree.add_child(ct_root, ct_title);
        {
            let ct_title = composite_tree.get_mut(ct_title);

            ct_title.instance_slot_index = Some(composite_instance_manager.alloc());
            ct_title.offset = [
                -(title_atlas_rect.width() as f32 * 0.5),
                8.0 * bitmap_scale as f32,
            ];
            ct_title.relative_offset_adjustment = [0.5, 0.0];
            ct_title.size = [
                title_atlas_rect.width() as f32,
                title_atlas_rect.height() as f32,
            ];
            ct_title.texatlas_rect = title_atlas_rect.clone();
            ct_title.composite_mode = CompositeMode::ColorTint([0.1, 0.1, 0.1, 1.0]);
        }

        let ht_frame = ht.create(HitTestTreeData {
            top: header_height,
            left: Self::FLOATING_MARGIN,
            width: Self::INIT_WIDTH,
            height: -Self::FLOATING_MARGIN - header_height,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });
        let ht_resize_area = ht.create(HitTestTreeData {
            left: -Self::RESIZE_AREA_WIDTH * 0.5,
            left_adjustment_factor: 1.0,
            width: Self::RESIZE_AREA_WIDTH,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });
        ht.add_child(ht_frame, ht_resize_area);

        Self {
            frame_image_atlas_rect,
            title_atlas_rect,
            title_blurred_atlas_rect,
            ct_root,
            ht_frame,
            ht_resize_area,
            width: Cell::new(Self::INIT_WIDTH),
            ui_scale_factor: Cell::new(bitmap_scale as _),
        }
    }

    pub fn mount(
        &self,
        ct: &mut CompositeTree,
        ct_parent: usize,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
        ht_parent: HitTestTreeRef,
    ) {
        ct.add_child(ct_parent, self.ct_root);
        ht.add_child(ht_parent, self.ht_frame);
    }

    pub fn set_width(
        &self,
        width: f32,
        ct: &mut CompositeTree,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
    ) {
        ct.get_mut(self.ct_root).size[0] = width * self.ui_scale_factor.get();
        ct.mark_dirty(self.ct_root);
        ht.get_data_mut(self.ht_frame).width = width;

        self.width.set(width);
    }
}

pub struct SpriteListPaneActionHandler {
    view: Rc<SpriteListPaneView>,
    ht_resize_area: HitTestTreeRef,
    resize_state: Cell<Option<(f32, f32)>>,
}
impl HitTestTreeActionHandler for SpriteListPaneActionHandler {
    type Context = AppUpdateContext;

    fn cursor_shape(&self, sender: HitTestTreeRef, _context: &mut Self::Context) -> CursorShape {
        if sender == self.ht_resize_area {
            return CursorShape::ResizeHorizontal;
        }

        CursorShape::Default
    }

    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        _context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        args: hittest::PointerActionArgs,
    ) -> input::EventContinueControl {
        if sender == self.ht_resize_area {
            self.resize_state
                .set(Some((self.view.width.get(), args.client_x)));

            return EventContinueControl::CAPTURE_ELEMENT | EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_move(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.ht_resize_area {
            if let Some((base_width, base_cx)) = self.resize_state.get() {
                let w = (base_width + (args.client_x - base_cx)).max(16.0);
                self.view.set_width(w, &mut context.composite_tree, ht);

                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.ht_resize_area {
            if let Some((base_width, base_cx)) = self.resize_state.replace(None) {
                let w = (base_width + (args.client_x - base_cx)).max(16.0);
                self.view.set_width(w, &mut context.composite_tree, ht);

                return EventContinueControl::RELEASE_CAPTURE_ELEMENT;
            }
        }

        EventContinueControl::empty()
    }
}

pub struct SpriteListPanePresenter {
    view: Rc<SpriteListPaneView>,
    _ht_action_handler: Rc<SpriteListPaneActionHandler>,
}
impl SpriteListPanePresenter {
    pub fn new(
        device: &br::DeviceObject<&br::InstanceObject>,
        adapter_memory_info: &br::MemoryProperties,
        graphics_queue_family_index: u32,
        graphics_queue: &mut impl br::QueueMut,
        atlas: &mut CompositionSurfaceAtlas,
        bitmap_scale: u32,
        header_height: f32,
        fonts: &mut FontSet,
        composite_tree: &mut CompositeTree,
        composite_instance_manager: &mut CompositeInstanceManager,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
    ) -> Self {
        let view = Rc::new(SpriteListPaneView::new(
            device,
            adapter_memory_info,
            graphics_queue_family_index,
            graphics_queue,
            atlas,
            bitmap_scale,
            header_height,
            fonts,
            composite_tree,
            composite_instance_manager,
            ht,
        ));

        let ht_action_handler = Rc::new(SpriteListPaneActionHandler {
            view: view.clone(),
            ht_resize_area: view.ht_resize_area,
            resize_state: Cell::new(None),
        });
        ht.get_data_mut(view.ht_resize_area).action_handler =
            Some(Rc::downgrade(&ht_action_handler) as _);

        Self {
            view,
            _ht_action_handler: ht_action_handler,
        }
    }

    pub fn mount(
        &self,
        ct: &mut CompositeTree,
        ct_parent: usize,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
        ht_parent: HitTestTreeRef,
    ) {
        self.view.mount(ct, ct_parent, ht, ht_parent);
    }
}

#[derive(Debug, Clone)]
pub struct AtlasRect {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}
impl AtlasRect {
    pub const fn width(&self) -> u32 {
        self.right - self.left
    }

    pub const fn height(&self) -> u32 {
        self.bottom - self.top
    }

    pub const fn lt_offset(&self) -> br::Offset2D {
        br::Offset2D {
            x: self.left as _,
            y: self.top as _,
        }
    }

    pub const fn extent(&self) -> br::Extent2D {
        br::Extent2D {
            width: self.width(),
            height: self.height(),
        }
    }

    pub const fn vk_rect(&self) -> br::Rect2D {
        self.extent().into_rect(self.lt_offset())
    }
}

pub struct CompositionSurfaceAtlas<'d> {
    resource: br::ImageViewObject<br::ImageObject<&'d br::DeviceObject<&'d br::InstanceObject>>>,
    memory: br::DeviceMemoryObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    residency_bitmap: Vec<u8>,
    size: u32,
    used_left: u32,
    used_top: u32,
    current_line_top: u32,
}
impl<'d> CompositionSurfaceAtlas<'d> {
    // TODO: できればPhysical Deviceからとれる値をつかったほうがいい
    // 1024なら大抵は問題ないとは思うが...
    const GRANULARITY: u32 = 1024;

    pub fn new(
        device: &'d br::DeviceObject<&'d br::InstanceObject>,
        queue: &mut br::QueueObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
        memory_properties: &br::MemoryProperties,
        size: u32,
        pixel_format: br::vk::VkFormat,
    ) -> Self {
        let bpp = match pixel_format {
            br::vk::VK_FORMAT_R8_UNORM => 1,
            _ => unimplemented!("bpp"),
        };

        let image = br::ImageObject::new(
            device,
            &br::ImageCreateInfo::new(
                br::vk::VkExtent2D {
                    width: size,
                    height: size,
                },
                pixel_format,
            )
            .as_color_attachment()
            .sampled()
            .transfer_dest()
            .flags(br::ImageFlags::SPARSE_BINDING | br::ImageFlags::SPARSE_RESIDENCY),
        )
        .unwrap();
        let resource = image
            .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
            .view_builder()
            .create()
            .unwrap();
        assert!(size % Self::GRANULARITY == 0);
        let bitmap_div = size / Self::GRANULARITY;
        let mut residency_bitmap = vec![0; (bitmap_div * bitmap_div) as usize];
        println!(
            "ComositionSurfaceAtlas: managing {size}x{size} atlas dividing with {} pixels ({} blocks)",
            Self::GRANULARITY,
            bitmap_div * bitmap_div
        );

        let image_memory_requirements = resource.image().sparse_requirements_alloc();
        for x in image_memory_requirements.iter() {
            println!("image memory requirements: {x:?}");
        }

        let image_memory_requirements = resource.image().requirements();
        println!("image memory requirements: {image_memory_requirements:?}");

        let memory_index = memory_properties
            .find_device_local_index(image_memory_requirements.memoryTypeBits)
            .expect("no suitable memory for surface atlas");
        let memory = br::DeviceMemoryObject::new(
            device,
            &br::MemoryAllocateInfo::new(
                (Self::GRANULARITY * Self::GRANULARITY * bpp) as _,
                memory_index,
            ),
        )
        .expect("Failed to allocate first memory block");

        unsafe {
            queue
                .bind_sparse_raw(
                    &[br::vk::VkBindSparseInfo {
                        sType: br::vk::VkBindSparseInfo::TYPE,
                        pNext: core::ptr::null(),
                        waitSemaphoreCount: 0,
                        pWaitSemaphores: core::ptr::null(),
                        signalSemaphoreCount: 0,
                        pSignalSemaphores: core::ptr::null(),
                        bufferBindCount: 0,
                        pBufferBinds: core::ptr::null(),
                        imageBindCount: 1,
                        pImageBinds: [br::vk::VkSparseImageMemoryBindInfo {
                            image: resource.image().native_ptr(),
                            bindCount: 1,
                            pBinds: [br::vk::VkSparseImageMemoryBind {
                                subresource: br::vk::VkImageSubresource {
                                    aspectMask: br::AspectMask::COLOR.bits(),
                                    mipLevel: 0,
                                    arrayLayer: 0,
                                },
                                offset: br::Offset3D::ZERO,
                                extent: br::Extent3D {
                                    width: Self::GRANULARITY,
                                    height: Self::GRANULARITY,
                                    depth: 1,
                                },
                                memory: memory.native_ptr(),
                                memoryOffset: 0,
                                flags: 0,
                            }]
                            .as_ptr(),
                        }]
                        .as_ptr(),
                        imageOpaqueBindCount: 0,
                        pImageOpaqueBinds: core::ptr::null(),
                    }],
                    None,
                )
                .expect("Failed to bind first memory block");
        }
        residency_bitmap[0] = 0x01;

        Self {
            resource,
            memory,
            residency_bitmap,
            size,
            used_left: 0,
            used_top: 0,
            current_line_top: 0,
        }
    }

    pub fn alloc(&mut self, required_width: u32, required_height: u32) -> AtlasRect {
        if self.used_left + required_width > Self::GRANULARITY {
            // 横が越える
            // TODO: 本当はこのあたりでタイルを拡張しないといけない
            self.used_left = 0;
            self.used_top += self.current_line_top;
            self.current_line_top = 0;

            if self.used_top > Self::GRANULARITY {
                todo!("alloc new tile");
            }
        }

        let l = self.used_left;
        self.used_left += required_width;
        self.current_line_top = self.current_line_top.max(required_height);

        AtlasRect {
            left: l,
            top: self.used_top,
            right: l + required_width,
            bottom: self.used_top + required_height,
        }
    }
}

#[repr(C)]
pub struct CompositeInstanceData {
    pub pos_st: [f32; 4],
    pub uv_st: [f32; 4],
    pub slice_borders: [f32; 4],
    pub tex_size_pixels_composite_mode: [f32; 4],
    pub color_tint: [f32; 4],
}

pub enum CompositeMode {
    DirectSourceOver,
    ColorTint([f32; 4]),
}
impl CompositeMode {
    pub const fn shader_mode_value(&self) -> f32 {
        match self {
            Self::DirectSourceOver => 0.0,
            Self::ColorTint(_) => 1.0,
        }
    }
}

pub struct CompositeRect {
    pub instance_slot_index: Option<usize>,
    pub offset: [f32; 2],
    pub size: [f32; 2],
    pub relative_offset_adjustment: [f32; 2],
    pub relative_size_adjustment: [f32; 2],
    pub texatlas_rect: AtlasRect,
    pub slice_borders: [f32; 4],
    pub composite_mode: CompositeMode,
    pub dirty: bool,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

pub struct CompositeInstanceManager<'d> {
    buffer: br::BufferObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    memory: br::DeviceMemoryObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    buffer_stg: br::BufferObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    memory_stg: br::DeviceMemoryObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
    stg_mem_requires_flush: bool,
    capacity: usize,
    count: usize,
    free: BTreeSet<usize>,
}
impl<'d> CompositeInstanceManager<'d> {
    const INIT_CAP: usize = 1024;

    pub fn new(
        device: &'d br::DeviceObject<&'d br::InstanceObject>,
        memory_info: &br::MemoryProperties,
    ) -> Self {
        let mut buffer = br::BufferObject::new(
            device,
            &br::BufferCreateInfo::new(
                core::mem::size_of::<CompositeInstanceData>() * Self::INIT_CAP,
                br::BufferUsage::VERTEX_BUFFER.transfer_dest(),
            ),
        )
        .expect("Failed to create composite instance buffer");
        let buffer_mreq = buffer.requirements();
        let memory_index = memory_info
            .find_device_local_index(buffer_mreq.memoryTypeBits)
            .expect("no suitable memory");
        let memory = br::DeviceMemoryObject::new(
            device,
            &br::MemoryAllocateInfo::new(buffer_mreq.size, memory_index),
        )
        .expect("Failed to allocate composite instance data memory");
        buffer
            .bind(&memory, 0)
            .expect("Failed to bind buffer memory");

        let mut buffer_stg = br::BufferObject::new(
            device,
            &br::BufferCreateInfo::new(
                core::mem::size_of::<CompositeInstanceData>() * Self::INIT_CAP,
                br::BufferUsage::TRANSFER_SRC,
            ),
        )
        .expect("Failed to create composite instance staging buffer");
        let buffer_mreq = buffer.requirements();
        let memory_index = memory_info
            .find_host_visible_index(buffer_mreq.memoryTypeBits)
            .expect("no suitable memory");
        let stg_mem_requires_flush = !memory_info.is_coherent(memory_index);
        let memory_stg = br::DeviceMemoryObject::new(
            device,
            &br::MemoryAllocateInfo::new(buffer_mreq.size, memory_index),
        )
        .expect("Failed to allocate composite instance data staging memory");
        buffer_stg
            .bind(&memory_stg, 0)
            .expect("Failed to bind staging buffer memory");

        Self {
            buffer,
            memory,
            buffer_stg,
            memory_stg,
            stg_mem_requires_flush,
            capacity: Self::INIT_CAP,
            count: 0,
            free: BTreeSet::new(),
        }
    }

    pub fn alloc(&mut self) -> usize {
        if let Some(x) = self.free.pop_first() {
            return x;
        }

        self.count += 1;
        if self.count >= self.capacity {
            todo!("instance buffer overflow!");
        }

        self.count - 1
    }

    pub const fn range_all(&self) -> core::ops::Range<usize> {
        0..core::mem::size_of::<CompositeInstanceData>() * self.count
    }
}

pub struct CompositeTree {
    rects: Vec<CompositeRect>,
    unused: BTreeSet<usize>,
    dirty: bool,
}
impl CompositeTree {
    pub fn new() -> Self {
        let mut rects = Vec::new();
        // root is filling rect
        rects.push(CompositeRect {
            instance_slot_index: None,
            offset: [0.0, 0.0],
            size: [0.0, 0.0],
            relative_offset_adjustment: [0.0, 0.0],
            relative_size_adjustment: [1.0, 1.0],
            texatlas_rect: AtlasRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            slice_borders: [0.0, 0.0, 0.0, 0.0],
            dirty: false,
            composite_mode: CompositeMode::DirectSourceOver,
            parent: None,
            children: Vec::new(),
        });

        Self {
            rects,
            unused: BTreeSet::new(),
            dirty: false,
        }
    }

    pub fn alloc(&mut self) -> usize {
        if let Some(x) = self.unused.pop_first() {
            self.rects[x] = CompositeRect {
                instance_slot_index: None,
                offset: [0.0; 2],
                size: [0.0; 2],
                relative_offset_adjustment: [0.0; 2],
                relative_size_adjustment: [0.0; 2],
                texatlas_rect: AtlasRect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
                slice_borders: [0.0; 4],
                composite_mode: CompositeMode::DirectSourceOver,
                dirty: false,
                parent: None,
                children: Vec::new(),
            };

            return x;
        }

        self.rects.push(CompositeRect {
            instance_slot_index: None,
            offset: [0.0; 2],
            size: [0.0; 2],
            relative_offset_adjustment: [0.0; 2],
            relative_size_adjustment: [0.0; 2],
            texatlas_rect: AtlasRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            slice_borders: [0.0; 4],
            composite_mode: CompositeMode::DirectSourceOver,
            dirty: false,
            parent: None,
            children: Vec::new(),
        });
        self.rects.len() - 1
    }

    pub fn free(&mut self, index: usize) {
        self.unused.insert(index);
    }

    pub fn get(&self, index: usize) -> &CompositeRect {
        &self.rects[index]
    }

    pub fn get_mut(&mut self, index: usize) -> &mut CompositeRect {
        &mut self.rects[index]
    }

    pub fn mark_dirty(&mut self, index: usize) {
        self.rects[index].dirty = true;
        self.dirty = true;
    }

    pub fn take_dirty(&mut self) -> bool {
        core::mem::replace(&mut self.dirty, false)
    }

    pub fn add_child(&mut self, parent: usize, child: usize) {
        if let Some(p) = self.rects[child].parent.replace(parent) {
            // unlink from old parent
            self.rects[p].children.retain(|&x| x != child);
        }

        self.rects[parent].children.push(child);
    }

    pub fn remove_child(&mut self, child: usize) {
        if let Some(p) = self.rects[child].parent.take() {
            self.rects[p].children.retain(|&x| x != child);
        }
    }

    pub unsafe fn sink_all(
        &mut self,
        size: br::Extent2D,
        tex_size: br::Extent2D,
        mapped_ptr: &br::MappedMemory<'_, impl br::DeviceMemoryMut + ?Sized>,
    ) {
        println!("sink all: {}x{}", size.width, size.height);
        let mut targets = vec![(0, (0.0, 0.0, size.width as f32, size.height as f32))];
        while !targets.is_empty() {
            let current = core::mem::replace(&mut targets, Vec::new());
            for (r, (effective_base_left, effective_base_top, effective_width, effective_height)) in
                current
            {
                let r = &mut self.rects[r];
                r.dirty = false;
                let left = effective_base_left
                    + (effective_width * r.relative_offset_adjustment[0])
                    + r.offset[0];
                let top = effective_base_top
                    + (effective_height * r.relative_offset_adjustment[1])
                    + r.offset[1];
                let w = effective_width * r.relative_size_adjustment[0] + r.size[0];
                let h = effective_height * r.relative_size_adjustment[1] + r.size[1];

                if let Some(instance_slot_index) = r.instance_slot_index {
                    unsafe {
                        core::ptr::write(
                            mapped_ptr.get_mut(
                                core::mem::size_of::<CompositeInstanceData>() * instance_slot_index,
                            ),
                            CompositeInstanceData {
                                pos_st: [w, h, left, top],
                                uv_st: [
                                    r.texatlas_rect.width() as f32 / tex_size.width as f32,
                                    r.texatlas_rect.height() as f32 / tex_size.height as f32,
                                    r.texatlas_rect.left as f32 / tex_size.width as f32,
                                    r.texatlas_rect.top as f32 / tex_size.height as f32,
                                ],
                                slice_borders: r.slice_borders,
                                tex_size_pixels_composite_mode: [
                                    tex_size.width as _,
                                    tex_size.height as _,
                                    r.composite_mode.shader_mode_value(),
                                    0.0,
                                ],
                                color_tint: match r.composite_mode {
                                    CompositeMode::DirectSourceOver => [0.0; 4],
                                    CompositeMode::ColorTint(t) => t,
                                },
                            },
                        );
                    }
                }

                targets.extend(r.children.iter().map(|&x| (x, (left, top, w, h))));
            }
        }
    }
}

struct GlyphBitmap {
    pub buf: Box<[u8]>,
    pub width: usize,
    pub pitch: usize,
    pub rows: usize,
    pub left_offset: isize,
    pub ascending_pixels: isize,
}
impl GlyphBitmap {
    pub fn copy_from_ft_glyph_slot(slot: &FT_GlyphSlotRec) -> Self {
        assert!(
            slot.bitmap.pitch >= 0,
            "inverted flow is not supported at this point"
        );
        let bytes = slot.bitmap.pitch as usize * slot.bitmap.rows as usize;
        let mut buf = Vec::with_capacity(bytes);
        unsafe {
            buf.set_len(bytes);
        }
        let mut buf = buf.into_boxed_slice();
        unsafe {
            core::ptr::copy_nonoverlapping(slot.bitmap.buffer, buf.as_mut_ptr(), bytes);
        }

        Self {
            buf,
            width: slot.bitmap.width as _,
            pitch: slot.bitmap.pitch as _,
            rows: slot.bitmap.rows as _,
            left_offset: slot.bitmap_left as _,
            ascending_pixels: slot.bitmap_top as _,
        }
    }
}

pub struct TextLayout {
    bitmaps: Vec<GlyphBitmap>,
    final_left_pos: f32,
    final_top_pos: f32,
    max_ascender: i32,
    max_descender: i32,
}
impl TextLayout {
    pub fn build_simple(text: &str, face: &mut freetype::Face) -> Self {
        let mut hb_buffer = harfbuzz::Buffer::new();
        hb_buffer.add(text);
        hb_buffer.guess_segment_properties();
        let mut hb_font = harfbuzz::Font::from_ft_face_referenced(face);
        harfbuzz::shape(&mut hb_font, &mut hb_buffer, &[]);
        let (glyph_infos, glyph_positions) = hb_buffer.get_shape_results();
        let mut left_pos = 0.0;
        let mut top_pos = 0.0;
        let mut max_ascender = 0;
        let mut max_descender = 0;
        // println!(
        //     "base metrics: {} {}",
        //     face.ascender_pixels(),
        //     face.height_pixels()
        // );
        let mut glyph_bitmaps = Vec::with_capacity(glyph_infos.len());
        for (info, pos) in glyph_infos.iter().zip(glyph_positions.iter()) {
            face.set_transform(
                None,
                Some(&freetype2::FT_Vector {
                    x: (left_pos * 64.0) as _,
                    y: (top_pos * 64.0) as _,
                }),
            );
            face.load_glyph(info.codepoint, FT_LOAD_DEFAULT).unwrap();
            face.render_glyph(FT_RENDER_MODE_NORMAL).unwrap();
            let slot = face.glyph_slot().unwrap();

            // println!(
            //     "glyph {} {} {} {} {} {} {} {} {} {}",
            //     info.codepoint,
            //     pos.x_advance as f32 / 64.0,
            //     pos.y_advance as f32 / 64.0,
            //     pos.x_offset,
            //     pos.y_offset,
            //     slot.bitmap_left,
            //     slot.bitmap_top,
            //     slot.bitmap.width,
            //     slot.bitmap.rows,
            //     slot.bitmap.pitch,
            // );

            glyph_bitmaps.push(GlyphBitmap::copy_from_ft_glyph_slot(slot));

            left_pos += pos.x_advance as f32 / 64.0;
            top_pos += pos.y_advance as f32 / 64.0;
            max_ascender = max_ascender.max(slot.bitmap_top);
            max_descender = max_descender.max(slot.bitmap.rows as i32 - slot.bitmap_top);
        }
        // println!("final metrics: {left_pos} {top_pos} {max_ascender} {max_descender}");

        Self {
            bitmaps: glyph_bitmaps,
            final_left_pos: left_pos,
            final_top_pos: top_pos,
            max_ascender,
            max_descender,
        }
    }

    pub const fn width(&self) -> f32 {
        self.final_left_pos
    }

    #[inline]
    pub fn width_px(&self) -> u32 {
        self.width().ceil() as _
    }

    pub const fn height(&self) -> f32 {
        self.max_ascender as f32 + self.max_descender as f32
    }

    #[inline]
    pub fn height_px(&self) -> u32 {
        self.height().ceil() as _
    }

    pub fn build_stg_image<'d, D: br::Device + 'd>(
        &self,
        device: &'d D,
        adapter_memory_info: &br::MemoryProperties,
    ) -> (br::ImageObject<&'d D>, br::DeviceMemoryObject<&'d D>) {
        let mut img = br::ImageObject::new(
            device,
            &br::ImageCreateInfo::new(
                br::Extent2D {
                    width: self.width_px(),
                    height: self.height_px(),
                },
                br::vk::VK_FORMAT_R8_UNORM,
            )
            .usage_with(br::ImageUsageFlags::TRANSFER_SRC)
            .use_linear_tiling(),
        )
        .expect("Failed to create staging text image");
        let mreq = img.requirements();
        let memory_index = adapter_memory_info
            .find_host_visible_index(mreq.memoryTypeBits)
            .expect("no suitable memory for image staging");
        let mut mem = br::DeviceMemoryObject::new(
            device,
            &br::MemoryAllocateInfo::new(mreq.size, memory_index),
        )
        .expect("Failed to allocate text surface stg memory");
        img.bind(&mem, 0).expect("Failed to bind stg memory");
        let subresource_layout = img
            .by_ref()
            .subresource(br::AspectMask::COLOR, 0, 0)
            .layout_info();

        let n = mem.native_ptr();
        let ptr = mem
            .map(0..(subresource_layout.rowPitch * self.height_px() as br::DeviceSize) as _)
            .unwrap();
        for b in self.bitmaps.iter() {
            for y in 0..b.rows {
                let dy = y as isize + self.max_ascender as isize - b.ascending_pixels;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        b.buf.as_ptr().add(b.pitch * y),
                        ptr.addr_of_mut(
                            subresource_layout.rowPitch as usize * dy as usize
                                + b.left_offset as usize,
                        ),
                        b.width,
                    )
                }
            }
        }
        if !adapter_memory_info.is_coherent(memory_index) {
            unsafe {
                device
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                        n,
                        0,
                        subresource_layout.rowPitch * self.height_px() as br::DeviceSize,
                    )])
                    .unwrap();
            }
        }
        ptr.end();

        (img, mem)
    }
}

pub struct AppState {}

pub struct AppUpdateContext {
    composite_tree: CompositeTree,
    state: AppState,
}

fn main() {
    let mut dp = wl::Display::connect().expect("Failed to connect to wayland display");
    let mut registry = dp.get_registry().expect("Failed to get global registry");
    struct RegistryListener {
        compositor: Option<wl::Owned<wl::Compositor>>,
        xdg_wm_base: Option<wl::Owned<wl::XdgWmBase>>,
        seat: Option<wl::Owned<wl::Seat>>,
        cursor_shape_manager: Option<wl::Owned<WpCursorShapeManagerV1>>,
    }
    impl wl::RegistryListener for RegistryListener {
        fn global(
            &mut self,
            registry: &mut wl::Registry,
            name: u32,
            interface: &core::ffi::CStr,
            version: u32,
        ) {
            println!("wl global: {name} {interface:?} {version}");

            if interface == c"wl_compositor" {
                self.compositor = Some(
                    registry
                        .bind(name, version)
                        .expect("Failed to bind compositor interface"),
                );
            }

            if interface == c"xdg_wm_base" {
                self.xdg_wm_base = Some(
                    registry
                        .bind(name, version)
                        .expect("Failed to bind xdg wm base interface"),
                );
            }

            if interface == c"wl_seat" {
                self.seat = Some(
                    registry
                        .bind(name, version)
                        .expect("Failed to bind seat interface"),
                );
            }

            if interface == c"wp_cursor_shape_manager_v1" {
                self.cursor_shape_manager = Some(
                    registry
                        .bind(name, version)
                        .expect("Failed to bind wp_cursor_shape_manager_v1 interface"),
                );
            }
        }

        fn global_remove(&mut self, _registry: &mut wl::Registry, name: u32) {
            println!("wl global remove: {name}");
        }
    }
    let mut rl = RegistryListener {
        compositor: None,
        xdg_wm_base: None,
        seat: None,
        cursor_shape_manager: None,
    };
    registry
        .add_listener(&mut rl)
        .expect("Failed to register listener");
    dp.roundtrip().expect("Failed to roundtrip events");

    let mut compositor = rl.compositor.expect("no wl_compositor");
    let mut xdg_wm_base = rl.xdg_wm_base.expect("no xdg_wm_base");
    let mut seat = rl.seat.expect("no seat?");
    let mut cursor_shape_manager = rl
        .cursor_shape_manager
        .expect("no wp_cursor_shape_manager_v1");

    struct SeatListener {
        pointer: Option<wl::Owned<wl::Pointer>>,
    }
    impl wl::SeatEventListener for SeatListener {
        fn capabilities(&mut self, seat: &mut wl::Seat, capabilities: u32) {
            println!("seat cb: 0x{capabilities:04x}");

            if (capabilities & 0x01) != 0 {
                // pointer
                self.pointer = Some(seat.get_pointer().expect("Failed to get pointer"));
            }
        }

        fn name(&mut self, _seat: &mut wl::Seat, name: &core::ffi::CStr) {
            println!("seat name: {name:?}");
        }
    }
    let mut seat_listener = SeatListener { pointer: None };
    seat.add_listener(&mut seat_listener).unwrap();

    let mut events = VecDeque::new();

    let client_size = Cell::new((640.0f32, 480.0));
    let mut app_state = AppState {};
    let mut pointer_input_manager = PointerInputManager::new();
    let mut ht_manager = HitTestTreeManager::new();
    let ht_root = ht_manager.create(HitTestTreeData {
        left: 0.0,
        top: 0.0,
        left_adjustment_factor: 0.0,
        top_adjustment_factor: 0.0,
        width: 0.0,
        height: 0.0,
        width_adjustment_factor: 1.0,
        height_adjustment_factor: 1.0,
        action_handler: None,
    });

    let mut wl_surface = compositor
        .create_surface()
        .expect("Failed to create wl_surface");
    let mut xdg_surface = xdg_wm_base
        .get_xdg_surface(&mut wl_surface)
        .expect("Failed to get xdg surface");
    let mut xdg_toplevel = xdg_surface
        .get_toplevel()
        .expect("Failed to get xdg toplevel");
    xdg_toplevel
        .set_app_id(c"io.ct2.peridot.tools.sprite_atlas")
        .expect("Failed to set app id");
    xdg_toplevel
        .set_title(c"Peridot SpriteAtlas Visualizer/Editor")
        .expect("Failed to set title");

    struct ToplevelSurfaceEventsHandler {
        app_event_queue: *mut VecDeque<AppEvent>,
    }
    impl wl::XdgSurfaceEventListener for ToplevelSurfaceEventsHandler {
        fn configure(&mut self, _: &mut wl::XdgSurface, serial: u32) {
            unsafe { &mut *self.app_event_queue }
                .push_back(AppEvent::ToplevelWindowSurfaceConfigure { serial });
        }
    }
    struct ToplevelWindowEventsHandler {
        app_event_queue: *mut VecDeque<AppEvent>,
    }
    impl wl::XdgToplevelEventListener for ToplevelWindowEventsHandler {
        fn configure(&mut self, _: &mut wl::XdgToplevel, width: i32, height: i32, states: &[i32]) {
            unsafe { &mut *self.app_event_queue }.push_back(AppEvent::ToplevelWindowConfigure {
                width: width as _,
                height: height as _,
            });

            println!(
                "configure: {width} {height} {states:?} th: {:?}",
                std::thread::current().id()
            );
        }

        fn close(&mut self, _: &mut wl::XdgToplevel) {
            unsafe { &mut *self.app_event_queue }.push_back(AppEvent::ToplevelWindowClose);
        }

        fn configure_bounds(&mut self, toplevel: &mut wl::XdgToplevel, width: i32, height: i32) {
            println!(
                "configure bounds: {width} {height} th: {:?}",
                std::thread::current().id()
            );
        }

        fn wm_capabilities(&mut self, toplevel: &mut wl::XdgToplevel, capabilities: &[i32]) {
            println!(
                "wm capabilities: {capabilities:?} th: {:?}",
                std::thread::current().id()
            );
        }
    }
    let mut tseh = ToplevelSurfaceEventsHandler {
        app_event_queue: &mut events as *mut _,
    };
    let mut tweh = ToplevelWindowEventsHandler {
        app_event_queue: &mut events as *mut _,
    };
    xdg_surface
        .add_listener(&mut tseh)
        .expect("Failed to register toplevel surface event");
    xdg_toplevel
        .add_listener(&mut tweh)
        .expect("Failed to register toplevel window event");

    struct SurfaceEvents {
        optimal_buffer_scale: u32,
    }
    impl wl::SurfaceEventListener for SurfaceEvents {
        fn enter(&mut self, surface: &mut wl::Surface, output: &mut wl::Output) {
            println!("enter output");
        }

        fn leave(&mut self, surface: &mut wl::Surface, output: &mut wl::Output) {
            println!("leave output");
        }

        fn preferred_buffer_scale(&mut self, surface: &mut wl::Surface, factor: i32) {
            println!("preferred buffer scale: {factor}");
            self.optimal_buffer_scale = factor as _;
            // 同じ値を適用することでdpi-awareになるらしい
            surface.set_buffer_scale(factor).unwrap();
            surface.commit().unwrap();
        }

        fn preferred_buffer_transform(&mut self, surface: &mut wl::Surface, transform: u32) {
            println!("preferred buffer transform: {transform}");
        }
    }
    let mut surface_events = SurfaceEvents {
        optimal_buffer_scale: 2,
    };
    wl_surface.add_listener(&mut surface_events).unwrap();

    wl_surface.commit().expect("Failed to commit surface");
    dp.roundtrip().expect("Failed to sync");

    let mut pointer = seat_listener.pointer.expect("no pointer from seat");
    let mut cursor_shape_device = cursor_shape_manager
        .get_pointer(&mut pointer)
        .expect("Failed to get cursor shape device");
    enum PointerOnSurface {
        None,
        Main { serial: u32 },
    }
    struct PointerEvents {
        pointer_on_surface: PointerOnSurface,
        main_surface_handle: *mut wl::Surface,
        app_event_queue: *mut VecDeque<AppEvent>,
    }
    impl wl::PointerEventListener for PointerEvents {
        fn enter(
            &mut self,
            _pointer: &mut wl::Pointer,
            serial: u32,
            surface: &mut wl::Surface,
            surface_x: wl::Fixed,
            surface_y: wl::Fixed,
        ) {
            self.pointer_on_surface = if core::ptr::addr_eq(surface, self.main_surface_handle) {
                PointerOnSurface::Main { serial }
            } else {
                PointerOnSurface::None
            };

            match self.pointer_on_surface {
                PointerOnSurface::None => (),
                PointerOnSurface::Main { serial } => unsafe { &mut *self.app_event_queue }
                    .push_back(AppEvent::MainWindowPointerMove {
                        enter_serial: serial,
                        surface_x: surface_x.to_f32(),
                        surface_y: surface_y.to_f32(),
                    }),
            }
        }
        fn leave(&mut self, _pointer: &mut wl::Pointer, _serial: u32, surface: &mut wl::Surface) {
            match self.pointer_on_surface {
                PointerOnSurface::None => (),
                PointerOnSurface::Main { .. } => {
                    if core::ptr::addr_eq(surface, self.main_surface_handle) {
                        self.pointer_on_surface = PointerOnSurface::None;
                    }
                }
            };
        }

        fn motion(
            &mut self,
            _pointer: &mut wl::Pointer,
            _time: u32,
            surface_x: wl::Fixed,
            surface_y: wl::Fixed,
        ) {
            match self.pointer_on_surface {
                PointerOnSurface::None => (),
                PointerOnSurface::Main { serial } => unsafe { &mut *self.app_event_queue }
                    .push_back(AppEvent::MainWindowPointerMove {
                        enter_serial: serial,
                        surface_x: surface_x.to_f32(),
                        surface_y: surface_y.to_f32(),
                    }),
            }
        }

        fn button(
            &mut self,
            _pointer: &mut wl::Pointer,
            serial: u32,
            time: u32,
            button: u32,
            state: wl::PointerButtonState,
        ) {
            println!("button: {serial} {time} {button} {}", state as u32);

            match self.pointer_on_surface {
                PointerOnSurface::None => (),
                PointerOnSurface::Main { serial } => {
                    if button == BTN_LEFT && state == wl::PointerButtonState::Pressed {
                        unsafe { &mut *self.app_event_queue }.push_back(
                            AppEvent::MainWindowPointerLeftDown {
                                enter_serial: serial,
                            },
                        );
                    } else if button == BTN_LEFT && state == wl::PointerButtonState::Released {
                        unsafe { &mut *self.app_event_queue }.push_back(
                            AppEvent::MainWindowPointerLeftUp {
                                enter_serial: serial,
                            },
                        );
                    }
                }
            }
        }

        fn axis(&mut self, _pointer: &mut wl::Pointer, time: u32, axis: u32, value: wl::Fixed) {
            println!("axis: {time} {axis} {}", value.to_f32());
        }

        fn frame(&mut self, _pointer: &mut wl::Pointer) {
            // do nothing
        }

        fn axis_source(&mut self, _pointer: &mut wl::Pointer, axis_source: u32) {
            println!("axis source: {axis_source}");
        }

        fn axis_stop(&mut self, _pointer: &mut wl::Pointer, _time: u32, axis: u32) {
            println!("axis stop: {axis}");
        }

        fn axis_discrete(&mut self, _pointer: &mut wl::Pointer, axis: u32, discrete: i32) {
            println!("axis discrete: {axis} {discrete}");
        }

        fn axis_value120(&mut self, _pointer: &mut wl::Pointer, axis: u32, value120: i32) {
            println!("axis value120: {axis} {value120}");
        }

        fn axis_relative_direction(
            &mut self,
            _pointer: &mut wl::Pointer,
            axis: u32,
            direction: u32,
        ) {
            println!("axis relative direction: {axis} {direction}");
        }
    }
    let mut pointer_events = PointerEvents {
        pointer_on_surface: PointerOnSurface::None,
        main_surface_handle: &mut *wl_surface as *mut _,
        app_event_queue: &mut events as *mut _,
    };
    pointer.add_listener(&mut pointer_events).unwrap();

    for x in br::instance_extension_properties(None).unwrap() {
        println!(
            "vkext {:?} version {}",
            x.extensionName.as_cstr().unwrap(),
            x.specVersion,
        );
    }

    let instance = br::InstanceObject::new(&br::InstanceCreateInfo::new(
        &br::ApplicationInfo::new(
            c"Peridot SpriteAtlas Visualizer",
            br::Version::new(0, 0, 1, 0),
            c"",
            br::Version::new(0, 0, 0, 0),
        )
        .api_version(br::Version::new(0, 1, 4, 0)),
        &[c"VK_LAYER_KHRONOS_validation".into()],
        &[c"VK_KHR_surface".into(), c"VK_KHR_wayland_surface".into()],
    ))
    .unwrap();
    let adapter = instance
        .iter_physical_devices()
        .expect("Failed to iterate physical devices")
        .next()
        .expect("no physical devices");
    let adapter_queue_info = adapter.queue_family_properties_alloc();
    for (n, q) in adapter_queue_info.iter().enumerate() {
        let mut v = Vec::with_capacity(4);
        if q.queue_flags().has(br::QueueFlags::GRAPHICS) {
            v.push("Graphics");
        }
        if q.queue_flags().has(br::QueueFlags::COMPUTE) {
            v.push("Compute");
        }
        if q.queue_flags().has(br::QueueFlags::TRANSFER) {
            v.push("Transfer");
        }
        if q.queue_flags().has(br::QueueFlags::SPARSE_BINDING) {
            v.push("Sparse Binding");
        }

        println!("Queue #{n}: x{} {}", q.queueCount, v.join(" / "));
    }
    let adapter_memory_info = adapter.memory_properties();
    for (n, p) in adapter_memory_info.types().iter().enumerate() {
        let h = &adapter_memory_info.heaps()[p.heapIndex as usize];

        let mut v = Vec::with_capacity(6);
        if p.property_flags()
            .has(br::MemoryPropertyFlags::DEVICE_LOCAL)
        {
            v.push("Device Local");
        }
        if p.property_flags()
            .has(br::MemoryPropertyFlags::HOST_VISIBLE)
        {
            v.push("Host Visible");
        }
        if p.property_flags()
            .has(br::MemoryPropertyFlags::HOST_COHERENT)
        {
            v.push("Host Coherent");
        }
        if p.property_flags().has(br::MemoryPropertyFlags::HOST_CACHED) {
            v.push("Host Cached");
        }
        if p.property_flags()
            .has(br::MemoryPropertyFlags::LAZILY_ALLOCATED)
        {
            v.push("Lazy Allocated");
        }
        if p.property_flags().has(br::MemoryPropertyFlags::PROTECTED) {
            v.push("Protected");
        }

        let mut hv = Vec::with_capacity(2);
        if h.flags().has(br::MemoryHeapFlags::DEVICE_LOCAL) {
            hv.push("Device Local");
        }
        if h.flags().has(br::MemoryHeapFlags::MULTI_INSTANCE) {
            hv.push("Multi Instance");
        }

        println!(
            "Memory Type #{n}: {} heap #{} ({}) size {}",
            v.join(" / "),
            p.heapIndex,
            hv.join(" / "),
            fmt_bytesize(h.size as _)
        );
    }
    let adapter_properties = adapter.properties();
    println!(
        "max texture2d size: {}",
        adapter_properties.limits.maxImageDimension2D
    );
    let adapter_sparse_image_format_props = adapter.sparse_image_format_properties_alloc(
        br::vk::VK_FORMAT_R8_UNORM,
        br::vk::VK_IMAGE_TYPE_2D,
        br::vk::VK_SAMPLE_COUNT_1_BIT,
        br::ImageUsageFlags::SAMPLED | br::ImageUsageFlags::COLOR_ATTACHMENT,
        br::vk::VK_IMAGE_TILING_OPTIMAL,
    );
    for x in adapter_sparse_image_format_props.iter() {
        println!(
            "sparse image format property: {:?} 0x{:04x} 0x{:04x}",
            x.imageGranularity, x.aspectMask, x.flags
        );
    }
    let graphics_queue_family_index = adapter_queue_info
        .find_matching_index(br::QueueFlags::GRAPHICS)
        .unwrap();
    let device = br::DeviceObject::new(
        &adapter,
        &br::DeviceCreateInfo::new(
            &[br::DeviceQueueCreateInfo::new(
                graphics_queue_family_index,
                &[1.0],
            )],
            &[],
            &[c"VK_KHR_swapchain".into()],
        )
        .with_next(
            &br::PhysicalDeviceFeatures2::new(br::vk::VkPhysicalDeviceFeatures {
                sparseBinding: true as _,
                sparseResidencyImage2D: true as _,
                ..Default::default()
            })
            .with_next(&mut br::vk::VkPhysicalDeviceSynchronization2Features {
                sType: <br::vk::VkPhysicalDeviceSynchronization2Features as VulkanStructure>::TYPE,
                pNext: core::ptr::null_mut(),
                synchronization2: 1,
            }),
        ),
    )
    .unwrap();
    let mut graphics_queue = device.queue(graphics_queue_family_index, 0);

    let mut composition_alphamask_surface_atlas = CompositionSurfaceAtlas::new(
        &device,
        &mut graphics_queue,
        &adapter_memory_info,
        adapter_properties.limits.maxImageDimension2D,
        br::vk::VK_FORMAT_R8_UNORM,
    );

    let surface = unsafe {
        br::SurfaceObject::new(
            &adapter,
            &br::vk::VkWaylandSurfaceCreateInfoKHR::new(dp.as_raw() as _, wl_surface.as_raw() as _),
        )
        .unwrap()
    };
    let surface_caps = adapter.surface_capabilities(&surface).unwrap();
    let surface_formats = adapter.surface_formats_alloc(&surface).unwrap();
    let sc_transform = if surface_caps
        .supported_transforms()
        .has(br::SurfaceTransformFlags::IDENTITY)
    {
        br::SurfaceTransformFlags::IDENTITY.bits()
    } else {
        surface_caps.currentTransform
    };
    let sc_composite_alpha = if surface_caps
        .supported_composite_alpha()
        .has(br::CompositeAlphaFlags::OPAQUE)
    {
        br::CompositeAlphaFlags::OPAQUE.bits()
    } else {
        br::CompositeAlphaFlags::INHERIT.bits()
    };
    let sc_format = surface_formats
        .iter()
        .find(|x| {
            x.format == br::vk::VK_FORMAT_R8G8B8A8_UNORM
                && x.colorSpace == br::vk::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR
        })
        .unwrap()
        .clone();
    let mut sc_size = br::vk::VkExtent2D {
        width: if surface_caps.currentExtent.width == 0xffff_ffff {
            640
        } else {
            surface_caps.currentExtent.width
        },
        height: if surface_caps.currentExtent.height == 0xffff_ffff {
            480
        } else {
            surface_caps.currentExtent.height
        },
    };
    let mut sc = Rc::new(
        br::SwapchainBuilder::new(
            &surface,
            2,
            sc_format.clone(),
            sc_size,
            br::ImageUsageFlags::COLOR_ATTACHMENT,
        )
        .pre_transform(sc_transform)
        .composite_alpha(sc_composite_alpha)
        .create(&device)
        .unwrap(),
    );

    crate::fontconfig::init();
    let mut fc_pat = crate::fontconfig::Pattern::new();
    fc_pat.add_family_name(c"system-ui");
    fc_pat.add_weight(80);
    crate::fontconfig::Config::current()
        .unwrap()
        .substitute(&mut fc_pat, crate::fontconfig::MatchKind::Pattern);
    fc_pat.default_substitute();
    let fc_set = crate::fontconfig::Config::current()
        .unwrap()
        .sort(&mut fc_pat, true)
        .unwrap();
    let mut primary_face_info = None;
    for &f in fc_set.fonts() {
        let file_path = f.get_file_path(0).unwrap();
        let index = f.get_face_index(0).unwrap();

        println!("match font: {file_path:?} {index}");

        if primary_face_info.is_none() {
            primary_face_info = Some((file_path.to_owned(), index));
        }
    }

    let (primary_face_path, primary_face_index) = primary_face_info.unwrap();

    let mut ft = FreeType::new().expect("Failed to initialize FreeType");
    let hinting = unsafe { ft.get_property::<u32>(c"cff", c"hinting-engine").unwrap() };
    println!("hinting engine: {hinting}");
    let no_stem_darkening = unsafe {
        ft.get_property::<freetype2::FT_Bool>(c"cff", c"no-stem-darkening")
            .unwrap()
    };
    println!("no stem darkening: {no_stem_darkening}");
    unsafe {
        ft.set_property(c"cff", c"no-stem-darkening", &(true as freetype2::FT_Bool))
            .unwrap();
    }
    let mut ft_face = ft
        .new_face(&primary_face_path, primary_face_index as _)
        .expect("Failed to create ft face");
    ft_face
        .set_char_size(
            (10.0 * 64.0) as _,
            0,
            96 * surface_events.optimal_buffer_scale,
            0,
        )
        .expect("Failed to set char size");

    let title_layout =
        TextLayout::build_simple("Peridot SpriteAtlas Visualizer/Editor", &mut ft_face);
    let (title_stg_image, _title_stg_image_mem) =
        title_layout.build_stg_image(&device, &adapter_memory_info);

    let text_surface_rect = composition_alphamask_surface_atlas
        .alloc(title_layout.width_px(), title_layout.height_px());
    println!("text surface rect: {text_surface_rect:?}");

    let mut upload_cp = br::CommandPoolObject::new(
        &device,
        &br::CommandPoolCreateInfo::new(graphics_queue_family_index).transient(),
    )
    .unwrap();
    let [mut upload_cb] = br::CommandBufferObject::alloc_array(
        &device,
        &br::CommandBufferFixedCountAllocateInfo::new(
            &mut upload_cp,
            br::CommandBufferLevel::Primary,
        ),
    )
    .unwrap();
    unsafe {
        upload_cb
            .begin(&br::CommandBufferBeginInfo::new().onetime_submit(), &device)
            .unwrap()
    }
    .pipeline_barrier_2(&br::DependencyInfo::new(
        &[],
        &[],
        &[
            title_stg_image
                .by_ref()
                .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
                .memory_barrier2()
                .from(br::PipelineStageFlags2::HOST, br::AccessFlags2::HOST.write)
                .to(
                    br::PipelineStageFlags2::COPY,
                    br::AccessFlags2::TRANSFER.read,
                )
                .transit_to(br::ImageLayout::TransferSrcOpt.from_undefined()),
            composition_alphamask_surface_atlas
                .resource
                .image()
                .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
                .memory_barrier2()
                .transit_to(br::ImageLayout::TransferDestOpt.from_undefined()),
        ],
    ))
    .copy_image(
        &title_stg_image,
        br::ImageLayout::TransferSrcOpt,
        composition_alphamask_surface_atlas.resource.image(),
        br::ImageLayout::TransferDestOpt,
        &[br::vk::VkImageCopy {
            srcSubresource: br::ImageSubresourceLayers::new(br::AspectMask::COLOR, 0, 0..1),
            srcOffset: br::Offset3D::ZERO,
            dstSubresource: br::ImageSubresourceLayers::new(br::AspectMask::COLOR, 0, 0..1),
            dstOffset: text_surface_rect.lt_offset().with_z(0),
            extent: text_surface_rect.extent().with_depth(1),
        }],
    )
    .pipeline_barrier_2(&br::DependencyInfo::new(
        &[],
        &[],
        &[composition_alphamask_surface_atlas
            .resource
            .image()
            .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
            .memory_barrier2()
            .from(
                br::PipelineStageFlags2::COPY,
                br::AccessFlags2::TRANSFER.write,
            )
            .to(
                br::PipelineStageFlags2::FRAGMENT_SHADER,
                br::AccessFlags2::SHADER_SAMPLED_READ,
            )
            .transit_from(br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt))],
    ))
    .end()
    .unwrap();
    graphics_queue
        .submit2(
            &[br::SubmitInfo2::new(
                &[],
                &[br::CommandBufferSubmitInfo::new(&upload_cb)],
                &[],
            )],
            None,
        )
        .unwrap();
    graphics_queue.wait().unwrap();

    let mut font_set = FontSet {
        ui_default: ft_face,
    };

    let main_rp = br::RenderPassObject::new(
        &device,
        &br::RenderPassCreateInfo2::new(
            &[br::AttachmentDescription2::new(sc_format.format)
                .layout_transition(br::ImageLayout::Undefined, br::ImageLayout::PresentSrc)
                .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)],
            &[
                br::SubpassDescription2::new().colors(&[br::AttachmentReference2::color(
                    0,
                    br::ImageLayout::ColorAttachmentOpt,
                )]),
            ],
            &[br::SubpassDependency2::new(
                br::SubpassIndex::Internal(0),
                br::SubpassIndex::External,
            )
            .of_execution(
                br::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                br::PipelineStageFlags(0),
            )
            .of_memory(
                br::AccessFlags::COLOR_ATTACHMENT.write,
                br::AccessFlags::MEMORY.read,
            )
            .by_region()],
        ),
    )
    .unwrap();

    let mut backbuffer_views = sc
        .images_alloc()
        .unwrap()
        .into_iter()
        .map(|bb| {
            bb.clone_parent()
                .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
                .view_builder()
                .create()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut main_fbs = backbuffer_views
        .iter()
        .map(|bb| {
            br::FramebufferObject::new(
                &device,
                &br::FramebufferCreateInfo::new(
                    &main_rp,
                    &[bb.as_transparent_ref()],
                    sc_size.width,
                    sc_size.height,
                ),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();

    let composite_sampler = br::SamplerObject::new(&device, &br::SamplerCreateInfo::new()).unwrap();

    let composite_vsh = br::ShaderModuleObject::new(
        &device,
        &br::ShaderModuleCreateInfo::new(&load_spv_file("resources/composite.vert").unwrap()),
    )
    .unwrap();
    let composite_fsh = br::ShaderModuleObject::new(
        &device,
        &br::ShaderModuleCreateInfo::new(&load_spv_file("resources/composite.frag").unwrap()),
    )
    .unwrap();
    let composite_shader_stages = [
        br::PipelineShaderStage::new(br::ShaderStage::Vertex, &composite_vsh, c"main"),
        br::PipelineShaderStage::new(br::ShaderStage::Fragment, &composite_fsh, c"main"),
    ];

    let composite_fsh_input_layout = br::DescriptorSetLayoutObject::new(
        &device,
        &br::DescriptorSetLayoutCreateInfo::new(&[
            br::DescriptorType::CombinedImageSampler.make_binding(0, 1)
        ]),
    )
    .unwrap();
    let mut descriptor_pool = br::DescriptorPoolObject::new(
        &device,
        &br::DescriptorPoolCreateInfo::new(
            1,
            &[br::DescriptorType::CombinedImageSampler.make_size(1)],
        ),
    )
    .unwrap();
    let [composite_alphamask_tex_descriptor] = descriptor_pool
        .alloc_array(&[composite_fsh_input_layout.as_transparent_ref()])
        .unwrap();
    device.update_descriptor_sets(
        &[composite_alphamask_tex_descriptor.binding_at(0).write(
            br::DescriptorContents::CombinedImageSampler(vec![
                br::DescriptorImageInfo::new(
                    &composition_alphamask_surface_atlas.resource,
                    br::ImageLayout::ShaderReadOnlyOpt,
                )
                .with_sampler(&composite_sampler),
            ]),
        )],
        &[],
    );

    let composite_pipeline_layout = br::PipelineLayoutObject::new(
        &device,
        &br::PipelineLayoutCreateInfo::new(
            &[composite_fsh_input_layout.as_transparent_ref()],
            &[br::vk::VkPushConstantRange::for_type::<[f32; 2]>(
                br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                0,
            )],
        ),
    )
    .unwrap();
    let composite_vbinds =
        [br::vk::VkVertexInputBindingDescription::per_instance_typed::<CompositeInstanceData>(0)];
    let composite_vinput = br::PipelineVertexInputStateCreateInfo::new(
        &composite_vbinds,
        &[
            br::vk::VkVertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: core::mem::offset_of!(CompositeInstanceData, pos_st) as _,
            },
            br::vk::VkVertexInputAttributeDescription {
                location: 1,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: core::mem::offset_of!(CompositeInstanceData, uv_st) as _,
            },
            br::vk::VkVertexInputAttributeDescription {
                location: 2,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: core::mem::offset_of!(CompositeInstanceData, slice_borders) as _,
            },
            br::vk::VkVertexInputAttributeDescription {
                location: 3,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: core::mem::offset_of!(CompositeInstanceData, tex_size_pixels_composite_mode)
                    as _,
            },
            br::vk::VkVertexInputAttributeDescription {
                location: 4,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: core::mem::offset_of!(CompositeInstanceData, color_tint) as _,
            },
        ],
    );
    let composite_ia_state =
        br::PipelineInputAssemblyStateCreateInfo::new(br::PrimitiveTopology::TriangleStrip);
    let composite_raster_state = br::PipelineRasterizationStateCreateInfo::new(
        br::PolygonMode::Fill,
        br::CullModeFlags::NONE,
        br::FrontFace::CounterClockwise,
    );

    let [mut composite_pipeline] = device
        .new_graphics_pipeline_array(
            &[br::GraphicsPipelineCreateInfo::new(
                &composite_pipeline_layout,
                main_rp.subpass(0),
                &composite_shader_stages,
                &composite_vinput,
                &composite_ia_state,
                &br::PipelineViewportStateCreateInfo::new(
                    &[br::Viewport {
                        x: 0.0,
                        y: 0.0,
                        width: sc_size.width as _,
                        height: sc_size.height as _,
                        minDepth: 0.0,
                        maxDepth: 1.0,
                    }],
                    &[sc_size.into_rect(br::Offset2D::ZERO)],
                ),
                &composite_raster_state,
                &br::PipelineColorBlendStateCreateInfo::new(&[
                    br::vk::VkPipelineColorBlendAttachmentState::PREMULTIPLIED,
                ]),
            )
            .multisample_state(&br::PipelineMultisampleStateCreateInfo::new())],
            None::<&br::PipelineCacheObject<&br::DeviceObject<&br::InstanceObject>>>,
        )
        .unwrap();

    let mut composite_instance_buffer =
        CompositeInstanceManager::new(&device, &adapter_memory_info);
    let mut composite_tree = CompositeTree::new();

    let title_cr = composite_tree.alloc();
    composite_tree.add_child(0, title_cr);
    {
        let title_cr = composite_tree.get_mut(title_cr);

        title_cr.instance_slot_index = Some(composite_instance_buffer.alloc());
        title_cr.offset = [
            24.0 * surface_events.optimal_buffer_scale as f32,
            16.0 * surface_events.optimal_buffer_scale as f32,
        ];
        title_cr.size = [
            text_surface_rect.width() as _,
            text_surface_rect.height() as _,
        ];
        title_cr.texatlas_rect = text_surface_rect.clone();
        title_cr.composite_mode = CompositeMode::ColorTint([1.0, 1.0, 1.0, 1.0]);
    }

    let header_size =
        16.0 + 16.0 + title_layout.height() / surface_events.optimal_buffer_scale as f32;

    let mut atlas_view = AtlasView::new(
        &device,
        &adapter_memory_info,
        graphics_queue_family_index,
        &mut graphics_queue,
        main_rp.subpass(0),
        sc_size,
    );

    let sprite_list_pane = SpriteListPanePresenter::new(
        &device,
        &adapter_memory_info,
        graphics_queue_family_index,
        &mut graphics_queue,
        &mut composition_alphamask_surface_atlas,
        surface_events.optimal_buffer_scale,
        header_size,
        &mut font_set,
        &mut composite_tree,
        &mut composite_instance_buffer,
        &mut ht_manager,
    );
    sprite_list_pane.mount(&mut composite_tree, 0, &mut ht_manager, ht_root);

    ht_manager.dump(ht_root);

    let n = composite_instance_buffer.memory_stg.native_ptr();
    let ptr = composite_instance_buffer
        .memory_stg
        .map(0..core::mem::size_of::<CompositeInstanceData>() * composite_instance_buffer.count)
        .unwrap();
    unsafe {
        composite_tree.sink_all(
            sc_size,
            br::Extent2D {
                width: composition_alphamask_surface_atlas.size,
                height: composition_alphamask_surface_atlas.size,
            },
            &ptr,
        );
    }
    if composite_instance_buffer.stg_mem_requires_flush {
        unsafe {
            device
                .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new(
                    &br::VkHandleRef::dangling(n),
                    0..core::mem::size_of::<CompositeInstanceData>() as _,
                )])
                .unwrap();
        }
    }
    ptr.end();
    let mut composite_instance_buffer_dirty = true;

    let mut main_cp = br::CommandPoolObject::new(
        &device,
        &br::CommandPoolCreateInfo::new(graphics_queue_family_index),
    )
    .unwrap();
    let mut main_cbs = br::CommandBufferObject::alloc(
        &device,
        &br::CommandBufferAllocateInfo::new(
            &mut main_cp,
            main_fbs.len() as _,
            br::CommandBufferLevel::Primary,
        ),
    )
    .unwrap();

    for (cb, fb) in main_cbs.iter_mut().zip(main_fbs.iter()) {
        unsafe {
            cb.begin(&br::CommandBufferBeginInfo::new(), &device)
                .unwrap()
        }
        .begin_render_pass2(
            &br::RenderPassBeginInfo::new(
                &main_rp,
                fb,
                sc_size.into_rect(br::Offset2D::ZERO),
                &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 1.0])],
            ),
            &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
        )
        .bind_pipeline(br::PipelineBindPoint::Graphics, &atlas_view.render_pipeline)
        .push_constant(
            &atlas_view.render_pipeline_layout,
            br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
            0,
            &[sc_size.width as f32, sc_size.height as f32],
        )
        .bind_descriptor_sets(
            br::PipelineBindPoint::Graphics,
            &atlas_view.render_pipeline_layout,
            0,
            &[atlas_view.ds_param],
            &[],
        )
        .draw(3, 1, 0, 0)
        .bind_pipeline(br::PipelineBindPoint::Graphics, &composite_pipeline)
        .bind_vertex_buffer_array(
            0,
            &[composite_instance_buffer.buffer.as_transparent_ref()],
            &[0],
        )
        .push_constant(
            &composite_pipeline_layout,
            br::vk::VK_SHADER_STAGE_VERTEX_BIT,
            0,
            &[sc_size.width as f32, sc_size.height as f32],
        )
        .bind_descriptor_sets(
            br::PipelineBindPoint::Graphics,
            &composite_pipeline_layout,
            0,
            &[composite_alphamask_tex_descriptor],
            &[],
        )
        .draw(4, composite_instance_buffer.count as _, 0, 0)
        .end_render_pass2(&br::SubpassEndInfo::new())
        .end()
        .unwrap();
    }

    let mut update_cp = br::CommandPoolObject::new(
        &device,
        &br::CommandPoolCreateInfo::new(graphics_queue_family_index),
    )
    .unwrap();
    let [mut update_cb] = br::CommandBufferObject::alloc_array(
        &device,
        &br::CommandBufferFixedCountAllocateInfo::new(
            &mut update_cp,
            br::CommandBufferLevel::Primary,
        ),
    )
    .unwrap();

    let mut acquire_completion =
        br::SemaphoreObject::new(&device, &br::SemaphoreCreateInfo::new()).unwrap();
    let render_completion =
        br::SemaphoreObject::new(&device, &br::SemaphoreCreateInfo::new()).unwrap();
    let mut last_render_command_fence =
        br::FenceObject::new(&device, &br::FenceCreateInfo::new(0)).unwrap();
    let mut last_rendering;
    let mut last_update_command_fence =
        br::FenceObject::new(&device, &br::FenceCreateInfo::new(0)).unwrap();
    let mut last_updating = false;

    struct FrameCallback {
        app_event_queue: *mut VecDeque<AppEvent>,
    }
    impl wl::CallbackEventListener for FrameCallback {
        fn done(&mut self, _: &mut wl::Callback, _: u32) {
            unsafe { &mut *self.app_event_queue }.push_back(AppEvent::ToplevelWindowFrameTiming);
        }
    }
    let mut frame_callback = FrameCallback {
        app_event_queue: &mut events as *mut _,
    };

    let mut frame = wl_surface.frame().expect("Failed to request next frame");
    frame
        .add_listener(&mut frame_callback)
        .expect("Failed to set frame callback");

    // fire initial update/render
    if core::mem::replace(&mut composite_instance_buffer_dirty, false) {
        unsafe {
            update_cb
                .begin(&br::CommandBufferBeginInfo::new(), &device)
                .unwrap()
        }
        .copy_buffer(
            &composite_instance_buffer.buffer_stg,
            &composite_instance_buffer.buffer,
            &[br::BufferCopy::mirror(
                0,
                (core::mem::size_of::<CompositeInstanceData>() * 1024) as _,
            )],
        )
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[br::MemoryBarrier2::new()
                .of_memory(
                    br::AccessFlags2::TRANSFER.write,
                    br::AccessFlags2::VERTEX_ATTRIBUTE_READ,
                )
                .of_execution(
                    br::PipelineStageFlags2::COPY,
                    br::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
                )],
            &[],
            &[],
        ))
        .end()
        .unwrap();
        graphics_queue
            .submit2(
                &[br::SubmitInfo2::new(
                    &[],
                    &[br::CommandBufferSubmitInfo::new(&update_cb)],
                    &[],
                )],
                Some(last_update_command_fence.as_transparent_ref_mut()),
            )
            .unwrap();
        last_updating = true;
    }
    let next = sc
        .acquire_next(
            None,
            br::CompletionHandlerMut::Queue(acquire_completion.as_transparent_ref_mut()),
        )
        .unwrap();
    graphics_queue
        .submit2(
            &[br::SubmitInfo2::new(
                &[br::SemaphoreSubmitInfo::new(&acquire_completion).on_color_attachment_output()],
                &[br::CommandBufferSubmitInfo::new(&main_cbs[next as usize])],
                &[br::SemaphoreSubmitInfo::new(&render_completion).on_color_attachment_output()],
            )],
            Some(last_render_command_fence.as_transparent_ref_mut()),
        )
        .unwrap();
    last_rendering = true;
    graphics_queue
        .present(&br::PresentInfo::new(
            &[render_completion.as_transparent_ref()],
            &[sc.as_transparent_ref()],
            &[next],
            &mut [br::vk::VkResult(0)],
        ))
        .unwrap();

    let mut app_update_context = AppUpdateContext {
        composite_tree,
        state: app_state,
    };

    dp.flush().unwrap();
    let mut t = std::time::Instant::now();
    let mut frame_resize_request = None;
    let mut last_render_scale = surface_events.optimal_buffer_scale;
    let mut last_render_size = sc_size;
    let mut last_pointer_pos = (0.0f32, 0.0f32);
    'app: loop {
        dp.dispatch().expect("Failed to dispatch");
        for e in events.drain(..) {
            match e {
                AppEvent::ToplevelWindowClose => break 'app,
                AppEvent::ToplevelWindowFrameTiming => {
                    let dt = t.elapsed();
                    t = std::time::Instant::now();
                    // print!("frame {dt:?}\n");

                    if last_rendering {
                        last_render_command_fence.wait().unwrap();
                        last_rendering = false;
                    }

                    if app_update_context.composite_tree.take_dirty()
                        || last_render_scale != surface_events.optimal_buffer_scale
                        || last_render_size != sc_size
                    {
                        let n = composite_instance_buffer.memory_stg.native_ptr();
                        let r = composite_instance_buffer.range_all();
                        let ptr = composite_instance_buffer.memory_stg.map(r.clone()).unwrap();
                        unsafe {
                            app_update_context.composite_tree.sink_all(
                                sc_size,
                                br::Extent2D {
                                    width: composition_alphamask_surface_atlas.size as _,
                                    height: composition_alphamask_surface_atlas.size as _,
                                },
                                &ptr,
                            );
                        }
                        if composite_instance_buffer.stg_mem_requires_flush {
                            unsafe {
                                device
                                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                                        n, 0, r.end as _,
                                    )])
                                    .unwrap();
                            }
                        }
                        ptr.end();
                        composite_instance_buffer_dirty = true;

                        last_render_scale = surface_events.optimal_buffer_scale;
                        last_render_size = sc_size;
                    }

                    let composite_instance_buffer_dirty =
                        core::mem::replace(&mut composite_instance_buffer_dirty, false);
                    let needs_update = composite_instance_buffer_dirty;

                    if needs_update {
                        if last_updating {
                            last_update_command_fence.wait().unwrap();
                            last_updating = false;
                        }

                        last_update_command_fence.reset().unwrap();
                        unsafe {
                            update_cp.reset(br::CommandPoolResetFlags::EMPTY).unwrap();
                        }
                        let rec = unsafe {
                            update_cb
                                .begin(&br::CommandBufferBeginInfo::new(), &device)
                                .unwrap()
                        };
                        let rec = if composite_instance_buffer_dirty {
                            rec.copy_buffer(
                                &composite_instance_buffer.buffer_stg,
                                &composite_instance_buffer.buffer,
                                &[br::BufferCopy::mirror(
                                    0,
                                    (core::mem::size_of::<CompositeInstanceData>() * 1024) as _,
                                )],
                            )
                        } else {
                            rec
                        };
                        rec.pipeline_barrier_2(&br::DependencyInfo::new(
                            &[br::MemoryBarrier2::new()
                                .of_memory(
                                    br::AccessFlags2::TRANSFER.write,
                                    br::AccessFlags2::VERTEX_ATTRIBUTE_READ,
                                )
                                .of_execution(
                                    br::PipelineStageFlags2::COPY,
                                    br::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
                                )],
                            &[],
                            &[],
                        ))
                        .end()
                        .unwrap();
                        graphics_queue
                            .submit2(
                                &[br::SubmitInfo2::new(
                                    &[],
                                    &[br::CommandBufferSubmitInfo::new(&update_cb)],
                                    &[],
                                )],
                                Some(last_update_command_fence.as_transparent_ref_mut()),
                            )
                            .unwrap();
                        last_updating = true;
                    }

                    last_render_command_fence.reset().unwrap();
                    let next = sc
                        .acquire_next(
                            None,
                            br::CompletionHandlerMut::Queue(
                                acquire_completion.as_transparent_ref_mut(),
                            ),
                        )
                        .unwrap();
                    graphics_queue
                        .submit2(
                            &[br::SubmitInfo2::new(
                                &[br::SemaphoreSubmitInfo::new(&acquire_completion)
                                    .on_color_attachment_output()],
                                &[br::CommandBufferSubmitInfo::new(&main_cbs[next as usize])],
                                &[br::SemaphoreSubmitInfo::new(&render_completion)
                                    .on_color_attachment_output()],
                            )],
                            Some(last_render_command_fence.as_transparent_ref_mut()),
                        )
                        .unwrap();
                    last_rendering = true;
                    graphics_queue
                        .present(&br::PresentInfo::new(
                            &[render_completion.as_transparent_ref()],
                            &[sc.as_transparent_ref()],
                            &[next],
                            &mut [br::vk::VkResult(0)],
                        ))
                        .unwrap();

                    frame = wl_surface.frame().expect("Failed to request next frame");
                    frame
                        .add_listener(&mut frame_callback)
                        .expect("Failed to set frame callback");
                }
                AppEvent::ToplevelWindowConfigure { width, height } => {
                    println!("ToplevelWindowConfigure {width} {height}");
                    frame_resize_request = Some((width, height));
                }
                AppEvent::ToplevelWindowSurfaceConfigure { serial } => {
                    if let Some((w, h)) = frame_resize_request.take() {
                        if w != sc_size.width || h != sc_size.height {
                            println!("frame resize: {w} {h}");

                            client_size.set((w as f32, h as f32));
                            sc_size.width = w * surface_events.optimal_buffer_scale;
                            sc_size.height = h * surface_events.optimal_buffer_scale;

                            if last_rendering {
                                last_render_command_fence.wait().unwrap();
                                last_rendering = false;
                            }

                            unsafe {
                                main_cp.reset(br::CommandPoolResetFlags::EMPTY).unwrap();
                            }
                            drop(main_fbs);
                            drop(backbuffer_views);
                            drop(sc);
                            sc = Rc::new(
                                br::SwapchainBuilder::new(
                                    &surface,
                                    2,
                                    sc_format.clone(),
                                    sc_size,
                                    br::ImageUsageFlags::COLOR_ATTACHMENT,
                                )
                                .pre_transform(sc_transform)
                                .composite_alpha(sc_composite_alpha)
                                .create(&device)
                                .unwrap(),
                            );

                            backbuffer_views = sc
                                .images_alloc()
                                .unwrap()
                                .into_iter()
                                .map(|bb| {
                                    bb.clone_parent()
                                        .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
                                        .view_builder()
                                        .create()
                                        .unwrap()
                                })
                                .collect::<Vec<_>>();
                            main_fbs = backbuffer_views
                                .iter()
                                .map(|bb| {
                                    br::FramebufferObject::new(
                                        &device,
                                        &br::FramebufferCreateInfo::new(
                                            &main_rp,
                                            &[bb.as_transparent_ref()],
                                            sc_size.width,
                                            sc_size.height,
                                        ),
                                    )
                                    .unwrap()
                                })
                                .collect::<Vec<_>>();

                            let [composite_pipeline1] = device
                                .new_graphics_pipeline_array(
                                    &[br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp.subpass(0),
                                        &composite_shader_stages,
                                        &composite_vinput,
                                        &composite_ia_state,
                                        &br::PipelineViewportStateCreateInfo::new(
                                            &[br::vk::VkViewport {
                                                x: 0.0,
                                                y: 0.0,
                                                width: sc_size.width as _,
                                                height: sc_size.height as _,
                                                minDepth: 0.0,
                                                maxDepth: 1.0,
                                            }],
                                            &[br::vk::VkRect2D {
                                                offset: br::vk::VkOffset2D::ZERO,
                                                extent: sc_size,
                                            }],
                                        ),
                                        &composite_raster_state,
                                        &br::PipelineColorBlendStateCreateInfo::new(&[
                                            br::vk::VkPipelineColorBlendAttachmentState::PREMULTIPLIED,
                                        ]),
                                    )
                                    .multisample_state(&br::PipelineMultisampleStateCreateInfo::new())],
                                    None::<&br::PipelineCacheObject<&br::DeviceObject<&br::InstanceObject>>>,
                                )
                                .unwrap();
                            composite_pipeline = composite_pipeline1;

                            atlas_view.recreate(&device, main_rp.subpass(0), sc_size);

                            for (cb, fb) in main_cbs.iter_mut().zip(main_fbs.iter()) {
                                unsafe {
                                    cb.begin(&br::CommandBufferBeginInfo::new(), &device)
                                        .unwrap()
                                }
                                .begin_render_pass2(
                                    &br::RenderPassBeginInfo::new(
                                        &main_rp,
                                        fb,
                                        br::vk::VkRect2D {
                                            offset: br::vk::VkOffset2D::ZERO,
                                            extent: sc_size,
                                        },
                                        &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 1.0])],
                                    ),
                                    &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                                )
                                .bind_pipeline(
                                    br::PipelineBindPoint::Graphics,
                                    &atlas_view.render_pipeline,
                                )
                                .push_constant(
                                    &atlas_view.render_pipeline_layout,
                                    br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
                                    0,
                                    &[sc_size.width as f32, sc_size.height as f32],
                                )
                                .bind_descriptor_sets(
                                    br::PipelineBindPoint::Graphics,
                                    &atlas_view.render_pipeline_layout,
                                    0,
                                    &[atlas_view.ds_param],
                                    &[],
                                )
                                .draw(3, 1, 0, 0)
                                .bind_pipeline(br::PipelineBindPoint::Graphics, &composite_pipeline)
                                .push_constant(
                                    &composite_pipeline_layout,
                                    br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                                    0,
                                    &[sc_size.width as f32, sc_size.height as f32],
                                )
                                .bind_descriptor_sets(
                                    br::PipelineBindPoint::Graphics,
                                    &composite_pipeline_layout,
                                    0,
                                    &[composite_alphamask_tex_descriptor],
                                    &[],
                                )
                                .bind_vertex_buffer_array(
                                    0,
                                    &[composite_instance_buffer.buffer.as_transparent_ref()],
                                    &[0],
                                )
                                .draw(4, composite_instance_buffer.count as _, 0, 0)
                                .end_render_pass2(&br::SubpassEndInfo::new())
                                .end()
                                .unwrap();
                            }
                        }
                    }

                    println!("ToplevelWindowSurfaceConfigure {serial}");
                    xdg_surface
                        .ack_configure(serial)
                        .expect("Failed to ack configure");
                }
                AppEvent::MainWindowPointerMove {
                    enter_serial,
                    surface_x,
                    surface_y,
                } => {
                    let (cw, ch) = client_size.get();
                    pointer_input_manager.handle_mouse_move(
                        surface_x,
                        surface_y,
                        cw,
                        ch,
                        &mut ht_manager,
                        &mut app_update_context,
                        ht_root,
                    );
                    let shape = pointer_input_manager
                        .cursor_shape(&mut ht_manager, &mut app_update_context);
                    cursor_shape_device
                        .set_shape(
                            enter_serial,
                            match shape {
                                CursorShape::Default => WpCursorShapeDeviceV1Shape::Default,
                                CursorShape::ResizeHorizontal => {
                                    WpCursorShapeDeviceV1Shape::EwResize
                                }
                            },
                        )
                        .unwrap();

                    last_pointer_pos = (surface_x, surface_y);
                }
                AppEvent::MainWindowPointerLeftDown { enter_serial } => {
                    let (cw, ch) = client_size.get();
                    pointer_input_manager.handle_mouse_left_down(
                        last_pointer_pos.0,
                        last_pointer_pos.1,
                        cw,
                        ch,
                        &mut ht_manager,
                        &mut app_update_context,
                        ht_root,
                    );

                    let shape = pointer_input_manager
                        .cursor_shape(&mut ht_manager, &mut app_update_context);
                    cursor_shape_device
                        .set_shape(
                            enter_serial,
                            match shape {
                                CursorShape::Default => WpCursorShapeDeviceV1Shape::Default,
                                CursorShape::ResizeHorizontal => {
                                    WpCursorShapeDeviceV1Shape::EwResize
                                }
                            },
                        )
                        .unwrap();
                }
                AppEvent::MainWindowPointerLeftUp { enter_serial } => {
                    let (cw, ch) = client_size.get();
                    pointer_input_manager.handle_mouse_left_up(
                        last_pointer_pos.0,
                        last_pointer_pos.1,
                        cw,
                        ch,
                        &mut ht_manager,
                        &mut app_update_context,
                        ht_root,
                    );

                    let shape = pointer_input_manager
                        .cursor_shape(&mut ht_manager, &mut app_update_context);
                    cursor_shape_device
                        .set_shape(
                            enter_serial,
                            match shape {
                                CursorShape::Default => WpCursorShapeDeviceV1Shape::Default,
                                CursorShape::ResizeHorizontal => {
                                    WpCursorShapeDeviceV1Shape::EwResize
                                }
                            },
                        )
                        .unwrap();
                }
            }
        }
    }

    unsafe {
        device.wait().unwrap();
    }
}

fn fmt_bytesize(x: usize) -> String {
    if x < 1000 {
        return format!("{x}bytes");
    }

    let (mut suffix, mut x) = ("KB", x as f64 / 1024.0);

    if x >= 1000.0 {
        suffix = "MB";
        x /= 1024.0;
    }

    if x >= 1000.0 {
        suffix = "GB";
        x /= 1024.0;
    }

    if x >= 1000.0 {
        suffix = "TB";
        x /= 1024.0;
    }

    format!("{x:.3} {suffix}")
}
