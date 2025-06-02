mod app_state;
mod composite;
mod coordinate;
mod feature;
mod fontconfig;
mod freetype;
mod harfbuzz;
mod hittest;
mod input;
mod linux_input_event_codes;
mod peridot;
mod subsystem;
mod text;
mod wl;

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    time::Duration,
};

use app_state::AppState;
use bedrock::{
    self as br, CommandBufferMut, CommandPoolMut, DescriptorPoolMut, Device, DeviceMemoryMut,
    Fence, FenceMut, Image, ImageChild, InstanceChild, MemoryBound, PhysicalDevice, RenderPass,
    ShaderModule, SurfaceCreateInfo, Swapchain, VkHandle, VkHandleMut,
};
use composite::{
    AnimatableColor, AnimationData, AtlasRect, CompositeInstanceData, CompositeInstanceManager,
    CompositeMode, CompositeRect, CompositeStreamingData, CompositeTree, CompositeTreeRef,
    CompositionSurfaceAtlas,
};
use coordinate::SizePixels;
use feature::editing_atlas_renderer::EditingAtlasRenderer;
use freetype::FreeType;
use hittest::{
    CursorShape, HitTestTreeActionHandler, HitTestTreeData, HitTestTreeManager, HitTestTreeRef,
};
use input::{EventContinueControl, PointerInputManager};
use linux_input_event_codes::BTN_LEFT;
use subsystem::{StagingScratchBufferManager, StagingScratchBufferMapMode, Subsystem};
use text::TextLayout;
use wl::{WpCursorShapeDeviceV1Shape, WpCursorShapeManagerV1};

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
const IA_STATE_TRISTRIP: &'static br::PipelineInputAssemblyStateCreateInfo =
    &br::PipelineInputAssemblyStateCreateInfo::new(br::PrimitiveTopology::TriangleStrip);
const VI_STATE_EMPTY: &'static br::PipelineVertexInputStateCreateInfo<'static> =
    &br::PipelineVertexInputStateCreateInfo::new(&[], &[]);
const VI_STATE_POSITION4_ONLY: &'static br::PipelineVertexInputStateCreateInfo<'static> =
    &br::PipelineVertexInputStateCreateInfo::new(
        &[br::VertexInputBindingDescription::per_vertex_typed::<
            [f32; 4],
        >(0)],
        &[br::VertexInputAttributeDescription {
            location: 0,
            binding: 0,
            format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
            offset: 0,
        }],
    );

pub struct FontSet {
    pub ui_default: freetype::Owned<freetype::Face>,
}

pub struct ViewInitContext<'d, 'r> {
    pub subsystem: &'d Subsystem,
    pub staging_scratch_buffer: &'r mut StagingScratchBufferManager<'d>,
    pub atlas: &'r mut CompositionSurfaceAtlas<'d>,
    pub ui_scale_factor: f32,
    pub composite_tree: &'r mut CompositeTree,
    pub composite_instance_manager: &'r mut CompositeInstanceManager<'d>,
    pub ht: &'r mut HitTestTreeManager<'d, AppUpdateContext<'d>>,
    pub fonts: &'r mut FontSet,
}

pub struct AppHeaderBaseView {
    height: f32,
    ct_root: CompositeTreeRef,
}
impl AppHeaderBaseView {
    const TITLE_SPACING: f32 = 16.0;
    const TITLE_LEFT_OFFSET: f32 = 36.0;

    pub fn new(ctx: &mut ViewInitContext) -> Self {
        let title = "Peridot SpriteAtlas Visualizer/Editor";
        let text_layout = TextLayout::build_simple(title, &mut ctx.fonts.ui_default);
        let text_atlas_rect = ctx
            .atlas
            .alloc(text_layout.width_px(), text_layout.height_px());
        let bg_atlas_rect = ctx.atlas.alloc(1, 2);

        let height = text_layout.height() / ctx.ui_scale_factor + Self::TITLE_SPACING * 2.0;

        let text_stg_image_pixels =
            text_layout.build_stg_image_pixel_buffer(ctx.staging_scratch_buffer);
        let bg_stg_image_pixels = ctx.staging_scratch_buffer.reserve(2);
        let ptr = ctx
            .staging_scratch_buffer
            .map(&bg_stg_image_pixels, StagingScratchBufferMapMode::Write)
            .unwrap();
        unsafe {
            ptr.addr_of_mut::<u8>(0).write(0xff);
            ptr.addr_of_mut::<u8>(1).write(0x00);
        }
        drop(ptr);

        let mut cp = br::CommandPoolObject::new(
            &ctx.subsystem,
            &br::CommandPoolCreateInfo::new(ctx.subsystem.graphics_queue_family_index).transient(),
        )
        .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            &ctx.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(
                &br::CommandBufferBeginInfo::new().onetime_submit(),
                &ctx.subsystem,
            )
            .unwrap()
        }
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[br::ImageMemoryBarrier2::new(
                ctx.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
        ))
        .inject(|r| {
            let (tb, to) = ctx.staging_scratch_buffer.of(&text_stg_image_pixels);
            let (b, o) = ctx.staging_scratch_buffer.of(&bg_stg_image_pixels);

            // TODO: ここ使うリソースいっしょだったらバッチするようにしたい
            r.copy_buffer_to_image(
                tb,
                ctx.atlas.resource().image(),
                br::ImageLayout::TransferDestOpt,
                &[br::vk::VkBufferImageCopy {
                    bufferOffset: to,
                    bufferRowLength: text_layout.width_px(),
                    bufferImageHeight: text_layout.height_px(),
                    imageSubresource: br::ImageSubresourceLayers::new(
                        br::AspectMask::COLOR,
                        0,
                        0..1,
                    ),
                    imageOffset: text_atlas_rect.lt_offset().with_z(0),
                    imageExtent: text_atlas_rect.extent().with_depth(1),
                }],
            )
            .copy_buffer_to_image(
                b,
                ctx.atlas.resource().image(),
                br::ImageLayout::TransferDestOpt,
                &[br::vk::VkBufferImageCopy {
                    bufferOffset: o,
                    bufferRowLength: 1,
                    bufferImageHeight: 2,
                    imageSubresource: br::ImageSubresourceLayers::new(
                        br::AspectMask::COLOR,
                        0,
                        0..1,
                    ),
                    imageOffset: bg_atlas_rect.lt_offset().with_z(0),
                    imageExtent: bg_atlas_rect.extent().with_depth(1),
                }],
            )
        })
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[br::ImageMemoryBarrier2::new(
                ctx.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
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
        ctx.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_root = ctx.composite_tree.register(CompositeRect {
            relative_size_adjustment: [1.0, 0.0],
            size: [0.0, height * ctx.ui_scale_factor],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.0, 0.0, 0.0, 0.25])),
            texatlas_rect: bg_atlas_rect,
            instance_slot_index: Some(ctx.composite_instance_manager.alloc()),
            ..Default::default()
        });
        let ct_title = ctx.composite_tree.register(CompositeRect {
            size: [text_layout.width(), text_layout.height()],
            offset: [
                Self::TITLE_LEFT_OFFSET * ctx.ui_scale_factor,
                Self::TITLE_SPACING * ctx.ui_scale_factor,
            ],
            texatlas_rect: text_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            instance_slot_index: Some(ctx.composite_instance_manager.alloc()),
            ..Default::default()
        });

        ctx.composite_tree.add_child(ct_root, ct_title);

        Self { height, ct_root }
    }

    pub fn mount(&self, ct_parent: CompositeTreeRef, ct: &mut CompositeTree) {
        ct.add_child(ct_parent, self.ct_root);
    }
}

pub struct AppHeaderPresenter {
    base_view: AppHeaderBaseView,
}
impl AppHeaderPresenter {
    pub fn new(init: &mut ViewInitContext) -> Self {
        let base_view = AppHeaderBaseView::new(init);

        Self { base_view }
    }

    pub fn mount(&self, ct_parent: CompositeTreeRef, ct: &mut CompositeTree) {
        self.base_view.mount(ct_parent, ct);
    }

    pub const fn height(&self) -> f32 {
        self.base_view.height
    }
}

pub struct SpriteListToggleButtonView {
    icon_atlas_rect: AtlasRect,
    ct_root: CompositeTreeRef,
    ct_icon: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    ui_scale_factor: Cell<f32>,
    hovering: Cell<bool>,
    pressing: Cell<bool>,
}
impl SpriteListToggleButtonView {
    const SIZE: f32 = 20.0;
    const ICON_SIZE: f32 = 7.0;
    const ICON_THICKNESS: f32 = 1.5;

    const ICON_VERTICES: &'static [[f32; 2]] = const {
        let top_left: (f32, f32) = (0.4 - (0.5 * Self::ICON_THICKNESS / Self::ICON_SIZE), 0.0);
        let top_right: (f32, f32) = (0.4 + (0.5 * Self::ICON_THICKNESS / Self::ICON_SIZE), 0.0);
        let middle_left: (f32, f32) = (0.0, 0.5);
        let middle_right: (f32, f32) = (Self::ICON_THICKNESS / Self::ICON_SIZE, 0.5);
        let bottom_left: (f32, f32) = (0.4 - (0.5 * Self::ICON_THICKNESS / Self::ICON_SIZE), 1.0);
        let bottom_right: (f32, f32) = (0.4 + (0.5 * Self::ICON_THICKNESS / Self::ICON_SIZE), 1.0);

        &[
            [top_left.0 * 2.0 - 1.0, top_left.1 * 2.0 - 1.0],
            [top_right.0 * 2.0 - 1.0, top_right.1 * 2.0 - 1.0],
            [middle_left.0 * 2.0 - 1.0, middle_left.1 * 2.0 - 1.0],
            [middle_right.0 * 2.0 - 1.0, middle_right.1 * 2.0 - 1.0],
            [bottom_left.0 * 2.0 - 1.0, bottom_left.1 * 2.0 - 1.0],
            [bottom_right.0 * 2.0 - 1.0, bottom_right.1 * 2.0 - 1.0],
            [(top_left.0 + 0.6) * 2.0 - 1.0, top_left.1 * 2.0 - 1.0],
            [(top_right.0 + 0.6) * 2.0 - 1.0, top_right.1 * 2.0 - 1.0],
            [(middle_left.0 + 0.6) * 2.0 - 1.0, middle_left.1 * 2.0 - 1.0],
            [
                (middle_right.0 + 0.6) * 2.0 - 1.0,
                middle_right.1 * 2.0 - 1.0,
            ],
            [(bottom_left.0 + 0.6) * 2.0 - 1.0, bottom_left.1 * 2.0 - 1.0],
            [
                (bottom_right.0 + 0.6) * 2.0 - 1.0,
                bottom_right.1 * 2.0 - 1.0,
            ],
        ]
    };
    const ICON_INDICES: &'static [u16] = &[
        0u16, 1, 2, 2, 1, 3, 2, 3, 4, 4, 3, 5, 6, 7, 8, 8, 7, 9, 8, 9, 10, 10, 9, 11,
    ];

    pub fn new(init: &mut ViewInitContext) -> Self {
        let icon_size_px = (Self::ICON_SIZE * init.ui_scale_factor).ceil() as u32;
        let icon_atlas_rect = init.atlas.alloc(icon_size_px, icon_size_px);
        let circle_atlas_rect = init.atlas.alloc(
            (Self::SIZE * init.ui_scale_factor) as _,
            (Self::SIZE * init.ui_scale_factor) as _,
        );

        let bufsize = Self::ICON_VERTICES.len() * core::mem::size_of::<[f32; 2]>()
            + Self::ICON_INDICES.len() * core::mem::size_of::<u16>();
        let mut buf = br::BufferObject::new(
            &init.subsystem,
            &br::BufferCreateInfo::new(
                bufsize,
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::INDEX_BUFFER,
            ),
        )
        .unwrap();
        let mreq = buf.requirements();
        let memindex = init
            .subsystem
            .adapter_memory_info
            .types()
            .iter()
            .enumerate()
            .find(|(n, t)| {
                (mreq.memoryTypeBits & (1 << n)) != 0
                    && t.property_flags().has_all(
                        br::MemoryPropertyFlags::DEVICE_LOCAL
                            | br::MemoryPropertyFlags::HOST_VISIBLE,
                    )
            })
            .expect("no suitable memory")
            .0 as u32;
        let mut mem = br::DeviceMemoryObject::new(
            &init.subsystem,
            &br::MemoryAllocateInfo::new(mreq.size, memindex),
        )
        .unwrap();
        buf.bind(&mem, 0).unwrap();
        let n = mem.native_ptr();
        let ptr = mem.map(0..bufsize).unwrap();
        unsafe {
            core::ptr::copy_nonoverlapping(
                Self::ICON_VERTICES.as_ptr(),
                ptr.addr_of_mut(0),
                Self::ICON_VERTICES.len(),
            );
            core::ptr::copy_nonoverlapping(
                Self::ICON_INDICES.as_ptr(),
                ptr.addr_of_mut(Self::ICON_VERTICES.len() * core::mem::size_of::<[f32; 2]>()),
                Self::ICON_INDICES.len(),
            );
        }
        if !init.subsystem.adapter_memory_info.is_coherent(memindex) {
            unsafe {
                init.subsystem
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                        n,
                        0,
                        bufsize as _,
                    )])
                    .unwrap();
            }
        }
        ptr.end();

        let mut msaa_buffer = br::ImageObject::new(
            &init.subsystem,
            &br::ImageCreateInfo::new(icon_atlas_rect.extent(), br::vk::VK_FORMAT_R8_UNORM)
                .as_color_attachment()
                .usage_with(br::ImageUsageFlags::TRANSFER_SRC)
                .sample_counts(4),
        )
        .unwrap();
        let mreq = msaa_buffer.requirements();
        let memindex = init
            .subsystem
            .adapter_memory_info
            .find_device_local_index(mreq.memoryTypeBits)
            .expect("no suitable memory for msaa buffer");
        let msaa_mem = br::DeviceMemoryObject::new(
            &init.subsystem,
            &br::MemoryAllocateInfo::new(mreq.size, memindex),
        )
        .unwrap();
        msaa_buffer.bind(&msaa_mem, 0).unwrap();
        let msaa_buffer = br::ImageViewBuilder::new(
            msaa_buffer,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()
        .unwrap();

        let rp = br::RenderPassObject::new(
            &init.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(br::vk::VK_FORMAT_R8_UNORM)
                    .color_memory_op(br::LoadOp::Clear, br::StoreOp::Store)
                    .layout_transition(br::ImageLayout::Undefined, br::ImageLayout::TransferSrcOpt)
                    .samples(4)],
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
                    br::AccessFlags::TRANSFER.read,
                )
                .of_execution(
                    br::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    br::PipelineStageFlags::TRANSFER,
                )],
            ),
        )
        .unwrap();
        let fb = br::FramebufferObject::new(
            &init.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp,
                &[msaa_buffer.as_transparent_ref()],
                icon_atlas_rect.width(),
                icon_atlas_rect.height(),
            ),
        )
        .unwrap();
        let rp_direct = br::RenderPassObject::new(
            &init.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(br::vk::VK_FORMAT_R8_UNORM)
                    .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)
                    .layout_transition(
                        br::ImageLayout::Undefined,
                        br::ImageLayout::ShaderReadOnlyOpt,
                    )],
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
        let fb_direct = br::FramebufferObject::new(
            &init.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp_direct,
                &[init.atlas.resource().as_transparent_ref()],
                init.atlas.size(),
                init.atlas.size(),
            ),
        )
        .unwrap();

        let vsh = init
            .subsystem
            .load_shader("resources/notrans.vert")
            .unwrap();
        let fsh = init
            .subsystem
            .load_shader("resources/fillcolor_r.frag")
            .unwrap();
        let vsh_circle = init
            .subsystem
            .load_shader("resources/filltri.vert")
            .unwrap();
        let fsh_circle = init
            .subsystem
            .load_shader("resources/aa_circle.frag")
            .unwrap();
        let pl = br::PipelineLayoutObject::new(
            &init.subsystem,
            &br::PipelineLayoutCreateInfo::new(&[], &[]),
        )
        .unwrap();
        #[derive(br::SpecializationConstants)]
        struct FragmentShaderParams {
            #[constant_id = 0]
            pub r: f32,
        }
        #[derive(br::SpecializationConstants)]
        struct CircleFragmentShaderParams {
            #[constant_id = 0]
            pub softness: f32,
        }
        let [pipeline, pipeline_circle] = init
            .subsystem
            .new_graphics_pipeline_array(
                &[
                    br::GraphicsPipelineCreateInfo::new(
                        &pl,
                        rp.subpass(0),
                        &[
                            vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                            fsh.on_stage(br::ShaderStage::Fragment, c"main")
                                .with_specialization_info(&br::SpecializationInfo::new(
                                    &FragmentShaderParams { r: 1.0 },
                                )),
                        ],
                        &br::PipelineVertexInputStateCreateInfo::new(
                            &[br::VertexInputBindingDescription::per_vertex_typed::<
                                [f32; 2],
                            >(0)],
                            &[br::VertexInputAttributeDescription {
                                location: 0,
                                binding: 0,
                                format: br::vk::VK_FORMAT_R32G32_SFLOAT,
                                offset: 0,
                            }],
                        ),
                        &IA_STATE_TRILIST,
                        &br::PipelineViewportStateCreateInfo::new(
                            &[icon_atlas_rect
                                .extent()
                                .into_rect(br::Offset2D::ZERO)
                                .make_viewport(0.0..1.0)],
                            &[icon_atlas_rect.extent().into_rect(br::Offset2D::ZERO)],
                        ),
                        &RASTER_STATE_DEFAULT_FILL_NOCULL,
                        &BLEND_STATE_SINGLE_NONE,
                    )
                    .multisample_state(
                        &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
                    ),
                    br::GraphicsPipelineCreateInfo::new(
                        &pl,
                        rp_direct.subpass(0),
                        &[
                            vsh_circle.on_stage(br::ShaderStage::Vertex, c"main"),
                            fsh_circle
                                .on_stage(br::ShaderStage::Fragment, c"main")
                                .with_specialization_info(&br::SpecializationInfo::new(
                                    &CircleFragmentShaderParams { softness: 0.0 },
                                )),
                        ],
                        &VI_STATE_EMPTY,
                        &IA_STATE_TRILIST,
                        &br::PipelineViewportStateCreateInfo::new(
                            &[circle_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                            &[circle_atlas_rect.vk_rect()],
                        ),
                        &RASTER_STATE_DEFAULT_FILL_NOCULL,
                        &BLEND_STATE_SINGLE_NONE,
                    )
                    .multisample_state(&MS_STATE_EMPTY),
                ],
                None::<&br::PipelineCacheObject<&br::DeviceObject<&br::InstanceObject>>>,
            )
            .unwrap();

        let mut cp = br::CommandPoolObject::new(
            &init.subsystem,
            &br::CommandPoolCreateInfo::new(init.subsystem.graphics_queue_family_index).transient(),
        )
        .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            &init.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(
                &br::CommandBufferBeginInfo::new().onetime_submit(),
                &init.subsystem,
            )
            .unwrap()
        }
        .begin_render_pass2(
            &br::RenderPassBeginInfo::new(
                &rp,
                &fb,
                icon_atlas_rect.extent().into_rect(br::Offset2D::ZERO),
                &[br::ClearValue::color_f32([0.0; 4])],
            ),
            &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
        )
        .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
        .bind_vertex_buffer_array(0, &[buf.as_transparent_ref()], &[0])
        .bind_index_buffer(
            &buf,
            Self::ICON_VERTICES.len() * core::mem::size_of::<[f32; 2]>(),
            br::IndexType::U16,
        )
        .draw_indexed(Self::ICON_INDICES.len() as _, 1, 0, 0, 0)
        .end_render_pass2(&br::SubpassEndInfo::new())
        .begin_render_pass2(
            &br::RenderPassBeginInfo::new(
                &rp_direct,
                &fb_direct,
                circle_atlas_rect.vk_rect(),
                &[br::ClearValue::color_f32([0.0; 4])],
            ),
            &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
        )
        .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline_circle)
        .draw(3, 1, 0, 0)
        .end_render_pass2(&br::SubpassEndInfo::new())
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[init
                .atlas
                .resource()
                .image()
                .memory_barrier2(br::ImageSubresourceRange::new(
                    br::AspectMask::COLOR,
                    0..1,
                    0..1,
                ))
                .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
        ))
        .resolve_image(
            msaa_buffer.image(),
            br::ImageLayout::TransferSrcOpt,
            init.atlas.resource().image(),
            br::ImageLayout::TransferDestOpt,
            &[br::vk::VkImageResolve {
                srcSubresource: br::ImageSubresourceLayers::new(br::AspectMask::COLOR, 0, 0..1),
                srcOffset: br::Offset3D::ZERO,
                dstSubresource: br::ImageSubresourceLayers::new(br::AspectMask::COLOR, 0, 0..1),
                dstOffset: icon_atlas_rect.lt_offset().with_z(0),
                extent: icon_atlas_rect.extent().with_depth(1),
            }],
        )
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[init
                .atlas
                .resource()
                .image()
                .memory_barrier2(br::ImageSubresourceRange::new(
                    br::AspectMask::COLOR,
                    0..1,
                    0..1,
                ))
                .from(
                    br::PipelineStageFlags2::RESOLVE,
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
        .end()
        .unwrap();

        init.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_root = init.composite_tree.register(CompositeRect {
            size: [
                Self::SIZE * init.ui_scale_factor,
                Self::SIZE * init.ui_scale_factor,
            ],
            offset: [0.0, 8.0 * init.ui_scale_factor],
            relative_offset_adjustment: [1.0, 0.0],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: circle_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([1.0, 1.0, 1.0, 0.0])),
            ..Default::default()
        });
        let ct_icon = init.composite_tree.register(CompositeRect {
            offset: [
                -Self::ICON_SIZE * 0.5 * init.ui_scale_factor,
                -Self::ICON_SIZE * 0.5 * init.ui_scale_factor,
            ],
            relative_offset_adjustment: [0.5, 0.5],
            size: [
                Self::ICON_SIZE * init.ui_scale_factor,
                Self::ICON_SIZE * init.ui_scale_factor,
            ],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: icon_atlas_rect.clone(),
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        init.composite_tree.add_child(ct_root, ct_icon);

        let ht_root = init.ht.create(HitTestTreeData {
            width: Self::SIZE,
            height: Self::SIZE,
            top: 8.0,
            left_adjustment_factor: 1.0,
            ..Default::default()
        });

        Self {
            icon_atlas_rect,
            ct_root,
            ct_icon,
            ht_root,
            ui_scale_factor: Cell::new(init.ui_scale_factor),
            hovering: Cell::new(false),
            pressing: Cell::new(false),
        }
    }

    pub fn mount(
        &self,
        composite_tree: &mut CompositeTree,
        ct_parent: CompositeTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
        ht_parent: HitTestTreeRef,
    ) {
        composite_tree.add_child(ct_parent, self.ct_root);
        ht.add_child(ht_parent, self.ht_root);
    }

    pub fn place_inner(
        &self,
        ct: &mut CompositeTree,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
        current_sec: f32,
    ) {
        let ui_scale_factor = self.ui_scale_factor.get();

        ct.get_mut(self.ct_root).offset[0] = 8.0 * ui_scale_factor;
        ct.get_mut(self.ct_root).animation_data_left = Some(AnimationData {
            to_value: (-Self::SIZE - 8.0) * ui_scale_factor,
            start_sec: current_sec,
            end_sec: current_sec + 0.25,
            curve_p1: (0.25, 0.8),
            curve_p2: (0.5, 1.0),
        });
        ct.mark_dirty(self.ct_root);
        ht.get_data_mut(self.ht_root).left = -Self::SIZE - 8.0;
    }

    pub fn place_outer(
        &self,
        ct: &mut CompositeTree,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
        current_sec: f32,
    ) {
        let ui_scale_factor = self.ui_scale_factor.get();

        ct.get_mut(self.ct_root).offset[0] = (-Self::SIZE - 8.0) * ui_scale_factor;
        ct.get_mut(self.ct_root).animation_data_left = Some(AnimationData {
            to_value: 8.0 * ui_scale_factor,
            start_sec: current_sec,
            end_sec: current_sec + 0.25,
            curve_p1: (0.25, 0.8),
            curve_p2: (0.5, 1.0),
        });
        ct.mark_dirty(self.ct_root);
        ht.get_data_mut(self.ht_root).left = 8.0;
    }

    pub fn flip_icon(&self, flipped: bool, ct: &mut CompositeTree) {
        let ct_icon = ct.get_mut(self.ct_icon);
        ct_icon.texatlas_rect = self.icon_atlas_rect.clone();
        if flipped {
            core::mem::swap(
                &mut ct_icon.texatlas_rect.left,
                &mut ct_icon.texatlas_rect.right,
            );
        }

        ct.mark_dirty(self.ct_icon);
    }

    fn update_button_bg_opacity(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        let opacity = match (self.hovering.get(), self.pressing.get()) {
            (_, true) => 0.375,
            (true, _) => 0.25,
            _ => 0.0,
        };

        let current = match composite_tree.get(self.ct_root).composite_mode {
            CompositeMode::ColorTint(ref x) => x.compute(current_sec),
            _ => unreachable!(),
        };
        composite_tree.get_mut(self.ct_root).composite_mode =
            CompositeMode::ColorTint(AnimatableColor::Animated(
                current,
                AnimationData {
                    to_value: [1.0, 1.0, 1.0, opacity],
                    start_sec: current_sec,
                    end_sec: current_sec + 0.1,
                    curve_p1: (0.5, 0.0),
                    curve_p2: (0.5, 1.0),
                },
            ));
        composite_tree.mark_dirty(self.ct_root);
    }

    pub fn on_hover(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        self.hovering.set(true);
        self.update_button_bg_opacity(composite_tree, current_sec);
    }

    pub fn on_leave(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        self.hovering.set(false);
        // はずれたらpressingもなかったことにする
        self.pressing.set(false);

        self.update_button_bg_opacity(composite_tree, current_sec);
    }

    pub fn on_press(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        self.pressing.set(true);
        self.update_button_bg_opacity(composite_tree, current_sec);
    }

    pub fn on_release(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        self.pressing.set(false);
        self.update_button_bg_opacity(composite_tree, current_sec);
    }
}

pub struct SpriteListCellView {
    ct_root: CompositeTreeRef,
    ct_bg: CompositeTreeRef,
    ct_bg_selected: CompositeTreeRef,
    ct_label: CompositeTreeRef,
    top: Cell<f32>,
    ui_scale: Cell<f32>,
}
impl SpriteListCellView {
    const CORNER_RADIUS: f32 = 8.0;
    const MARGIN_H: f32 = 16.0;
    const HEIGHT: f32 = 24.0;
    const LABEL_MARGIN_LEFT: f32 = 8.0;

    pub fn new(init: &mut ViewInitContext, init_label: &str, init_top: f32) -> Self {
        let label_layout = TextLayout::build_simple(init_label, &mut init.fonts.ui_default);
        let label_atlas_rect = init
            .atlas
            .alloc(label_layout.width_px(), label_layout.height_px());
        let label_stg_image_pixels =
            label_layout.build_stg_image_pixel_buffer(init.staging_scratch_buffer);
        let bg_atlas_rect = init.atlas.alloc(
            ((Self::CORNER_RADIUS * 2.0 + 1.0) * init.ui_scale_factor) as _,
            ((Self::CORNER_RADIUS * 2.0 + 1.0) * init.ui_scale_factor) as _,
        );

        let render_pass = br::RenderPassObject::new(
            &init.subsystem,
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
            &init.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[init.atlas.resource().as_transparent_ref()],
                init.atlas.size(),
                init.atlas.size(),
            ),
        )
        .unwrap();

        let vsh = init
            .subsystem
            .load_shader("resources/filltri.vert")
            .unwrap();
        let fsh = init
            .subsystem
            .load_shader("resources/rounded_rect.frag")
            .unwrap();

        let pipeline_layout = br::PipelineLayoutObject::new(
            &init.subsystem,
            &br::PipelineLayoutCreateInfo::new(&[], &[]),
        )
        .unwrap();
        let [pipeline] = init
            .subsystem
            .new_graphics_pipeline_array(
                &[br::GraphicsPipelineCreateInfo::new(
                    &pipeline_layout,
                    render_pass.subpass(0),
                    &[
                        br::PipelineShaderStage::new(br::ShaderStage::Vertex, &vsh, c"main"),
                        br::PipelineShaderStage::new(br::ShaderStage::Fragment, &fsh, c"main"),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[bg_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                        &[bg_atlas_rect.vk_rect()],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(MS_STATE_EMPTY)],
                None::<&br::PipelineCacheObject<&br::DeviceObject<&br::InstanceObject>>>,
            )
            .unwrap();

        let mut cp = br::CommandPoolObject::new(
            &init.subsystem,
            &br::CommandPoolCreateInfo::new(init.subsystem.graphics_queue_family_index).transient(),
        )
        .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            &init.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(&br::CommandBufferBeginInfo::new(), &init.subsystem)
                .unwrap()
        }
        .inject(|r| {
            let (b, o) = init.staging_scratch_buffer.of(&label_stg_image_pixels);

            r.copy_buffer_to_image(
                b,
                init.atlas.resource().image(),
                br::ImageLayout::TransferDestOpt,
                &[br::vk::VkBufferImageCopy {
                    bufferOffset: o,
                    bufferRowLength: label_layout.width_px(),
                    bufferImageHeight: label_layout.height_px(),
                    imageSubresource: br::ImageSubresourceLayers::new(
                        br::AspectMask::COLOR,
                        0,
                        0..1,
                    ),
                    imageOffset: label_atlas_rect.lt_offset().with_z(0),
                    imageExtent: label_atlas_rect.extent().with_depth(1),
                }],
            )
        })
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[init
                .atlas
                .resource()
                .image()
                .memory_barrier2(br::ImageSubresourceRange::new(
                    br::AspectMask::COLOR,
                    0..1,
                    0..1,
                ))
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
                bg_atlas_rect.vk_rect(),
                &[br::ClearValue::color_f32([0.0; 4])],
            ),
            &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
        )
        .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
        .draw(3, 1, 0, 0)
        .end_render_pass2(&br::SubpassEndInfo::new())
        .end()
        .unwrap();

        init.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_root = init.composite_tree.register(CompositeRect {
            offset: [
                Self::MARGIN_H * init.ui_scale_factor,
                init_top * init.ui_scale_factor,
            ],
            relative_size_adjustment: [1.0, 0.0],
            size: [
                -Self::MARGIN_H * 2.0 * init.ui_scale_factor,
                Self::HEIGHT * init.ui_scale_factor,
            ],
            ..Default::default()
        });
        let ct_label = init.composite_tree.register(CompositeRect {
            offset: [
                Self::LABEL_MARGIN_LEFT * init.ui_scale_factor,
                -label_layout.height() * 0.5,
            ],
            relative_offset_adjustment: [0.0, 0.5],
            size: [label_layout.width(), label_layout.height()],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.1, 0.1, 0.1, 1.0])),
            texatlas_rect: label_atlas_rect,
            ..Default::default()
        });
        let ct_bg = init.composite_tree.register(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([1.0, 1.0, 1.0, 0.25])),
            texatlas_rect: bg_atlas_rect.clone(),
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            ..Default::default()
        });
        let ct_bg_selected = init.composite_tree.register(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.6, 0.8, 1.0, 0.0])),
            texatlas_rect: bg_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            ..Default::default()
        });

        init.composite_tree.add_child(ct_root, ct_bg_selected);
        init.composite_tree.add_child(ct_root, ct_bg);
        init.composite_tree.add_child(ct_root, ct_label);

        Self {
            ct_root,
            ct_label,
            ct_bg,
            ct_bg_selected,
            top: Cell::new(init_top),
            ui_scale: Cell::new(init.ui_scale_factor),
        }
    }

    pub fn mount(&self, ct_parent: CompositeTreeRef, ct: &mut CompositeTree) {
        ct.add_child(ct_parent, self.ct_root);
    }

    pub fn unmount(&self, ct: &mut CompositeTree) {
        ct.remove_child(self.ct_root);
    }
}

pub struct SpriteListPaneView {
    pub frame_image_atlas_rect: AtlasRect,
    pub title_atlas_rect: AtlasRect,
    pub title_blurred_atlas_rect: AtlasRect,
    ct_root: CompositeTreeRef,
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

    pub fn new(init: &mut ViewInitContext, header_height: f32) -> Self {
        let render_size_px = ((Self::CORNER_RADIUS * 2.0 + 1.0) * init.ui_scale_factor) as u32;
        let frame_image_atlas_rect = init.atlas.alloc(render_size_px, render_size_px);

        let title_blur_pixels =
            (Self::BLUR_AMOUNT_ONEDIR as f32 * init.ui_scale_factor).ceil() as _;
        let title_layout = TextLayout::build_simple("Sprites", &mut init.fonts.ui_default);
        let title_atlas_rect = init
            .atlas
            .alloc(title_layout.width_px(), title_layout.height_px());
        let title_blurred_atlas_rect = init.atlas.alloc(
            title_layout.width_px() + (title_blur_pixels * 2 + 1),
            title_layout.height_px() + (title_blur_pixels * 2 + 1),
        );
        let title_stg_image_pixels =
            title_layout.build_stg_image_pixel_buffer(init.staging_scratch_buffer);

        let mut title_blurred_work_image = br::ImageObject::new(
            &init.subsystem,
            &br::ImageCreateInfo::new(
                title_blurred_atlas_rect.extent(),
                br::vk::VK_FORMAT_R8_UNORM,
            )
            .as_color_attachment()
            .sampled(),
        )
        .unwrap();
        let mreq = title_blurred_work_image.requirements();
        let memindex = init
            .subsystem
            .adapter_memory_info
            .find_device_local_index(mreq.memoryTypeBits)
            .expect("no suitable memory index");
        let title_blurred_work_image_mem = br::DeviceMemoryObject::new(
            &init.subsystem,
            &br::MemoryAllocateInfo::new(mreq.size, memindex),
        )
        .unwrap();
        title_blurred_work_image
            .bind(&title_blurred_work_image_mem, 0)
            .unwrap();
        let title_blurred_work_image_view = br::ImageViewBuilder::new(
            title_blurred_work_image,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()
        .unwrap();

        let render_pass = br::RenderPassObject::new(
            &init.subsystem,
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
            &init.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[init.atlas.resource().as_transparent_ref()],
                init.atlas.size(),
                init.atlas.size(),
            ),
        )
        .unwrap();
        let title_blurred_work_framebuffer = br::FramebufferObject::new(
            &init.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[title_blurred_work_image_view.as_transparent_ref()],
                title_blurred_atlas_rect.width(),
                title_blurred_atlas_rect.height(),
            ),
        )
        .unwrap();

        let vsh = init
            .subsystem
            .load_shader("resources/filltri.vert")
            .unwrap();
        let fsh = init
            .subsystem
            .load_shader("resources/rounded_rect.frag")
            .unwrap();
        let vsh_blur = init
            .subsystem
            .load_shader("resources/filltri_uvmod.vert")
            .unwrap();
        let fsh_blur = init
            .subsystem
            .load_shader("resources/blit_axis_convolved.frag")
            .unwrap();
        #[derive(br::SpecializationConstants)]
        struct ConvolutionFragmentShaderParams {
            #[constant_id = 0]
            max_count: u32,
        }

        let dsl_tex1 = br::DescriptorSetLayoutObject::new(
            &init.subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[
                br::DescriptorType::CombinedImageSampler.make_binding(0, 1)
            ]),
        )
        .unwrap();
        let smp = br::SamplerObject::new(&init.subsystem, &br::SamplerCreateInfo::new()).unwrap();
        let mut dp = br::DescriptorPoolObject::new(
            &init.subsystem,
            &br::DescriptorPoolCreateInfo::new(
                2,
                &[br::DescriptorType::CombinedImageSampler.make_size(2)],
            ),
        )
        .unwrap();
        let [ds_title, ds_title2] = dp
            .alloc_array(&[dsl_tex1.as_transparent_ref(), dsl_tex1.as_transparent_ref()])
            .unwrap();
        init.subsystem.update_descriptor_sets(
            &[
                ds_title
                    .binding_at(0)
                    .write(br::DescriptorContents::CombinedImageSampler(vec![
                        br::DescriptorImageInfo::new(
                            &init.atlas.resource().as_transparent_ref(),
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

        let pipeline_layout = br::PipelineLayoutObject::new(
            &init.subsystem,
            &br::PipelineLayoutCreateInfo::new(&[], &[]),
        )
        .unwrap();
        let blur_pipeline_layout = br::PipelineLayoutObject::new(
            &init.subsystem,
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
        let [pipeline, pipeline_blur1, pipeline_blur] = init
            .subsystem
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
                            vsh_blur.on_stage(br::ShaderStage::Vertex, c"main"),
                            fsh_blur
                                .on_stage(br::ShaderStage::Fragment, c"main")
                                .with_specialization_info(&br::SpecializationInfo::new(
                                    &ConvolutionFragmentShaderParams {
                                        max_count: title_blur_pixels,
                                    },
                                )),
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
                            vsh_blur.on_stage(br::ShaderStage::Vertex, c"main"),
                            fsh_blur
                                .on_stage(br::ShaderStage::Fragment, c"main")
                                .with_specialization_info(&br::SpecializationInfo::new(
                                    &ConvolutionFragmentShaderParams {
                                        max_count: title_blur_pixels,
                                    },
                                )),
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
        fsh_h_params[0] = init.atlas.uv_from_pixels(title_atlas_rect.left as _);
        fsh_h_params[1] = init.atlas.uv_from_pixels(title_atlas_rect.top as _);
        fsh_h_params[2] = init.atlas.uv_from_pixels(title_atlas_rect.right as _);
        fsh_h_params[3] = init.atlas.uv_from_pixels(title_atlas_rect.bottom as _);
        fsh_v_params[2] = 1.0;
        fsh_v_params[3] = 1.0;
        // uv_step
        fsh_h_params[4] = 1.0 / init.atlas.size() as f32;
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
            &init.subsystem,
            &br::CommandPoolCreateInfo::new(init.subsystem.graphics_queue_family_index).transient(),
        )
        .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            &init.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(&br::CommandBufferBeginInfo::new(), &init.subsystem)
                .unwrap()
        }
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[init
                .atlas
                .resource()
                .image()
                .memory_barrier2(br::ImageSubresourceRange::new(
                    br::AspectMask::COLOR,
                    0..1,
                    0..1,
                ))
                .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
        ))
        .inject(|r| {
            let (b, o) = init.staging_scratch_buffer.of(&title_stg_image_pixels);

            r.copy_buffer_to_image(
                b,
                init.atlas.resource().image(),
                br::ImageLayout::TransferDestOpt,
                &[br::vk::VkBufferImageCopy {
                    bufferOffset: o,
                    bufferRowLength: title_layout.width_px(),
                    bufferImageHeight: title_layout.height_px(),
                    imageSubresource: br::ImageSubresourceLayers::new(
                        br::AspectMask::COLOR,
                        0,
                        0..1,
                    ),
                    imageOffset: title_atlas_rect.lt_offset().with_z(0),
                    imageExtent: title_atlas_rect.extent().with_depth(1),
                }],
            )
        })
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[init
                .atlas
                .resource()
                .image()
                .memory_barrier2(br::ImageSubresourceRange::new(
                    br::AspectMask::COLOR,
                    0..1,
                    0..1,
                ))
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
                init.atlas
                    .uv_from_pixels((title_atlas_rect.width() + title_blur_pixels * 2 + 1) as _),
                init.atlas
                    .uv_from_pixels((title_atlas_rect.height() + title_blur_pixels * 2 + 1) as _),
                init.atlas
                    .uv_from_pixels(title_atlas_rect.left as f32 - title_blur_pixels as f32),
                init.atlas
                    .uv_from_pixels(title_atlas_rect.top as f32 - title_blur_pixels as f32),
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

        init.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_root = init.composite_tree.register(CompositeRect {
            offset: [
                Self::FLOATING_MARGIN * init.ui_scale_factor,
                header_height * init.ui_scale_factor,
            ],
            size: [
                Self::INIT_WIDTH * init.ui_scale_factor,
                -(header_height + Self::FLOATING_MARGIN) * init.ui_scale_factor,
            ],
            relative_size_adjustment: [0.0, 1.0],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: frame_image_atlas_rect.clone(),
            slice_borders: [
                Self::CORNER_RADIUS * init.ui_scale_factor,
                Self::CORNER_RADIUS * init.ui_scale_factor,
                Self::CORNER_RADIUS * init.ui_scale_factor,
                Self::CORNER_RADIUS * init.ui_scale_factor,
            ],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([1.0, 1.0, 1.0, 0.5])),
            ..Default::default()
        });
        let ct_title_blurred = init.composite_tree.register(CompositeRect {
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            offset: [
                -(title_blurred_atlas_rect.width() as f32 * 0.5),
                (12.0 - Self::BLUR_AMOUNT_ONEDIR as f32) * init.ui_scale_factor,
            ],
            relative_offset_adjustment: [0.5, 0.0],
            size: [
                title_blurred_atlas_rect.width() as f32,
                title_blurred_atlas_rect.height() as f32,
            ],
            texatlas_rect: title_blurred_atlas_rect.clone(),
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });
        let ct_title = init.composite_tree.register(CompositeRect {
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            offset: [
                -(title_atlas_rect.width() as f32 * 0.5),
                12.0 * init.ui_scale_factor,
            ],
            relative_offset_adjustment: [0.5, 0.0],
            size: [
                title_atlas_rect.width() as f32,
                title_atlas_rect.height() as f32,
            ],
            texatlas_rect: title_atlas_rect.clone(),
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.1, 0.1, 0.1, 1.0])),
            ..Default::default()
        });

        init.composite_tree.add_child(ct_root, ct_title_blurred);
        init.composite_tree.add_child(ct_root, ct_title);

        let ht_frame = init.ht.create(HitTestTreeData {
            top: header_height,
            left: Self::FLOATING_MARGIN,
            width: Self::INIT_WIDTH,
            height: -Self::FLOATING_MARGIN - header_height,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });
        let ht_resize_area = init.ht.create(HitTestTreeData {
            left: -Self::RESIZE_AREA_WIDTH * 0.5,
            left_adjustment_factor: 1.0,
            width: Self::RESIZE_AREA_WIDTH,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });
        init.ht.add_child(ht_frame, ht_resize_area);

        Self {
            frame_image_atlas_rect,
            title_atlas_rect,
            title_blurred_atlas_rect,
            ct_root,
            ht_frame,
            ht_resize_area,
            width: Cell::new(Self::INIT_WIDTH),
            ui_scale_factor: Cell::new(init.ui_scale_factor as _),
        }
    }

    pub fn mount(
        &self,
        ct: &mut CompositeTree,
        ct_parent: CompositeTreeRef,
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

    pub fn show(
        &self,
        ct: &mut CompositeTree,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
        current_sec: f32,
    ) {
        ct.get_mut(self.ct_root).offset[0] = -self.width.get() * self.ui_scale_factor.get();
        ct.get_mut(self.ct_root).animation_data_left = Some(AnimationData {
            to_value: Self::FLOATING_MARGIN * self.ui_scale_factor.get(),
            start_sec: current_sec,
            end_sec: current_sec + 0.25,
            curve_p1: (0.4, 1.25),
            curve_p2: (0.5, 1.0),
        });
        ct.mark_dirty(self.ct_root);
        ht.get_data_mut(self.ht_frame).left = Self::FLOATING_MARGIN;
    }

    pub fn hide(
        &self,
        ct: &mut CompositeTree,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
        current_sec: f32,
    ) {
        ct.get_mut(self.ct_root).offset[0] = Self::FLOATING_MARGIN * self.ui_scale_factor.get();
        ct.get_mut(self.ct_root).animation_data_left = Some(AnimationData {
            to_value: -self.width.get() * self.ui_scale_factor.get(),
            start_sec: current_sec,
            end_sec: current_sec + 0.25,
            curve_p1: (0.4, 1.25),
            curve_p2: (0.5, 1.0),
        });
        ct.mark_dirty(self.ct_root);
        ht.get_data_mut(self.ht_frame).left = -self.width.get();
    }
}

pub struct SpriteListPaneActionHandler {
    view: Rc<SpriteListPaneView>,
    toggle_button_view: Rc<SpriteListToggleButtonView>,
    ht_resize_area: HitTestTreeRef,
    resize_state: Cell<Option<(f32, f32)>>,
    shown: Cell<bool>,
}
impl<'c> HitTestTreeActionHandler<'c> for SpriteListPaneActionHandler {
    type Context = AppUpdateContext<'c>;

    fn cursor_shape(&self, sender: HitTestTreeRef, _context: &mut Self::Context) -> CursorShape {
        if sender == self.ht_resize_area && self.shown.get() {
            return CursorShape::ResizeHorizontal;
        }

        CursorShape::Default
    }

    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view
                .on_hover(&mut context.composite_tree, context.current_sec);

            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_leave(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view
                .on_leave(&mut context.composite_tree, context.current_sec);

            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        args: hittest::PointerActionArgs,
    ) -> input::EventContinueControl {
        if self.shown.get() {
            if sender == self.view.ht_frame {
                // guard fallback
                return EventContinueControl::STOP_PROPAGATION;
            }

            if sender == self.ht_resize_area {
                self.resize_state
                    .set(Some((self.view.width.get(), args.client_x)));

                return EventContinueControl::CAPTURE_ELEMENT
                    | EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view
                .on_press(&mut context.composite_tree, context.current_sec);

            return EventContinueControl::STOP_PROPAGATION;
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
        if self.shown.get() {
            if sender == self.view.ht_frame {
                // guard fallback
                return EventContinueControl::STOP_PROPAGATION;
            }

            if sender == self.ht_resize_area {
                if let Some((base_width, base_cx)) = self.resize_state.get() {
                    let w = (base_width + (args.client_x - base_cx)).max(16.0);
                    self.view.set_width(w, &mut context.composite_tree, ht);

                    return EventContinueControl::STOP_PROPAGATION;
                }
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
        if self.shown.get() {
            if sender == self.view.ht_frame {
                // guard fallback
                return EventContinueControl::STOP_PROPAGATION;
            }

            if sender == self.ht_resize_area {
                if let Some((base_width, base_cx)) = self.resize_state.replace(None) {
                    let w = (base_width + (args.client_x - base_cx)).max(16.0);
                    self.view.set_width(w, &mut context.composite_tree, ht);

                    return EventContinueControl::RELEASE_CAPTURE_ELEMENT;
                }
            }
        }

        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view
                .on_release(&mut context.composite_tree, context.current_sec);

            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_click(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if self.shown.get() && sender == self.view.ht_frame {
            // guard fallback
            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.toggle_button_view.ht_root {
            let show = !self.shown.get();
            self.shown.set(show);

            if show {
                self.view
                    .show(&mut context.composite_tree, ht, context.current_sec);
                self.toggle_button_view.place_inner(
                    &mut context.composite_tree,
                    ht,
                    context.current_sec,
                );
            } else {
                self.view
                    .hide(&mut context.composite_tree, ht, context.current_sec);
                self.toggle_button_view.place_outer(
                    &mut context.composite_tree,
                    ht,
                    context.current_sec,
                );
            }

            self.toggle_button_view
                .flip_icon(!show, &mut context.composite_tree);

            return EventContinueControl::STOP_PROPAGATION
                | EventContinueControl::RECOMPUTE_POINTER_ENTER;
        }

        EventContinueControl::empty()
    }
}

pub struct SpriteListPanePresenter {
    view: Rc<SpriteListPaneView>,
    cell_view: SpriteListCellView,
    _ht_action_handler: Rc<SpriteListPaneActionHandler>,
}
impl SpriteListPanePresenter {
    pub fn new(init: &mut ViewInitContext, header_height: f32) -> Self {
        let view = Rc::new(SpriteListPaneView::new(init, header_height));
        let toggle_button_view = Rc::new(SpriteListToggleButtonView::new(init));

        let cell_view = SpriteListCellView::new(init, "sprite cell", 32.0);

        toggle_button_view.mount(init.composite_tree, view.ct_root, init.ht, view.ht_frame);
        toggle_button_view.place_inner(init.composite_tree, init.ht, -0.25);

        cell_view.mount(view.ct_root, init.composite_tree);

        let ht_action_handler = Rc::new(SpriteListPaneActionHandler {
            view: view.clone(),
            toggle_button_view: toggle_button_view.clone(),
            ht_resize_area: view.ht_resize_area,
            resize_state: Cell::new(None),
            shown: Cell::new(true),
        });
        init.ht.get_data_mut(view.ht_frame).action_handler =
            Some(Rc::downgrade(&ht_action_handler) as _);
        init.ht.get_data_mut(view.ht_resize_area).action_handler =
            Some(Rc::downgrade(&ht_action_handler) as _);
        init.ht
            .get_data_mut(toggle_button_view.ht_root)
            .action_handler = Some(Rc::downgrade(&ht_action_handler) as _);

        Self {
            view,
            cell_view,
            _ht_action_handler: ht_action_handler,
        }
    }

    pub fn mount(
        &self,
        ct: &mut CompositeTree,
        ct_parent: CompositeTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext>,
        ht_parent: HitTestTreeRef,
    ) {
        self.view.mount(ct, ct_parent, ht, ht_parent);
    }
}

pub struct AppUpdateContext<'d> {
    composite_tree: CompositeTree,
    state: AppState<'d>,
    editing_atlas_renderer: Rc<RefCell<EditingAtlasRenderer<'d>>>,
    current_sec: f32,
}

struct HitTestRootTreeActionHandler {
    editing_atlas_dragging: Cell<bool>,
    editing_atlas_drag_start_x: Cell<f32>,
    editing_atlas_drag_start_y: Cell<f32>,
    editing_atlas_drag_start_offset_x: Cell<f32>,
    editing_atlas_drag_start_offset_y: Cell<f32>,
}
impl<'c> HitTestTreeActionHandler<'c> for HitTestRootTreeActionHandler {
    type Context = AppUpdateContext<'c>;

    fn on_pointer_down(
        &self,
        _sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        self.editing_atlas_dragging.set(true);
        self.editing_atlas_drag_start_x.set(args.client_x);
        self.editing_atlas_drag_start_y.set(args.client_y);
        self.editing_atlas_drag_start_offset_x
            .set(context.editing_atlas_renderer.borrow().offset()[0]);
        self.editing_atlas_drag_start_offset_y
            .set(context.editing_atlas_renderer.borrow().offset()[1]);

        EventContinueControl::CAPTURE_ELEMENT
    }

    fn on_pointer_move(
        &self,
        _sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if self.editing_atlas_dragging.get() {
            let dx = args.client_x - self.editing_atlas_drag_start_x.get();
            let dy = args.client_y - self.editing_atlas_drag_start_y.get();

            // TODO: あとでui_scale_factorをみれるようにする
            context.editing_atlas_renderer.borrow_mut().set_offset(
                self.editing_atlas_drag_start_offset_x.get() + dx * 2.0,
                self.editing_atlas_drag_start_offset_y.get() + dy * 2.0,
            );
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        _sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if self.editing_atlas_dragging.replace(false) {
            let dx = args.client_x - self.editing_atlas_drag_start_x.get();
            let dy = args.client_y - self.editing_atlas_drag_start_y.get();

            // TODO: あとでui_scale_factorをみれるようにする
            context.editing_atlas_renderer.borrow_mut().set_offset(
                self.editing_atlas_drag_start_offset_x.get() + dx * 2.0,
                self.editing_atlas_drag_start_offset_y.get() + dy * 2.0,
            );
        }

        EventContinueControl::RELEASE_CAPTURE_ELEMENT
    }
}

struct FrameCallback {
    app_event_queue: *mut VecDeque<AppEvent>,
}
impl wl::CallbackEventListener for FrameCallback {
    fn done(&mut self, _: &mut wl::Callback, _: u32) {
        unsafe { &mut *self.app_event_queue }.push_back(AppEvent::ToplevelWindowFrameTiming);
    }
}

enum PointerOnSurface {
    None,
    Main { serial: u32 },
}
struct WaylandShellEventHandler {
    app_event_bus: *mut VecDeque<AppEvent>,
    ui_scale_factor: Rc<Cell<f32>>,
    pointer_on_surface: PointerOnSurface,
    main_surface_proxy_ptr: *mut wl::Surface,
}
impl wl::XdgSurfaceEventListener for WaylandShellEventHandler {
    fn configure(&mut self, _: &mut wl::XdgSurface, serial: u32) {
        unsafe { &mut *self.app_event_bus }
            .push_back(AppEvent::ToplevelWindowSurfaceConfigure { serial });
    }
}
impl wl::XdgToplevelEventListener for WaylandShellEventHandler {
    fn configure(&mut self, _: &mut wl::XdgToplevel, width: i32, height: i32, states: &[i32]) {
        unsafe { &mut *self.app_event_bus }.push_back(AppEvent::ToplevelWindowConfigure {
            width: width as _,
            height: height as _,
        });

        println!(
            "configure: {width} {height} {states:?} th: {:?}",
            std::thread::current().id()
        );
    }

    fn close(&mut self, _: &mut wl::XdgToplevel) {
        unsafe { &mut *self.app_event_bus }.push_back(AppEvent::ToplevelWindowClose);
    }

    fn configure_bounds(&mut self, _toplevel: &mut wl::XdgToplevel, width: i32, height: i32) {
        println!(
            "configure bounds: {width} {height} th: {:?}",
            std::thread::current().id()
        );
    }

    fn wm_capabilities(&mut self, _toplevel: &mut wl::XdgToplevel, capabilities: &[i32]) {
        println!(
            "wm capabilities: {capabilities:?} th: {:?}",
            std::thread::current().id()
        );
    }
}
impl wl::SurfaceEventListener for WaylandShellEventHandler {
    fn enter(&mut self, _surface: &mut wl::Surface, _output: &mut wl::Output) {
        println!("enter output");
    }

    fn leave(&mut self, _surface: &mut wl::Surface, _output: &mut wl::Output) {
        println!("leave output");
    }

    fn preferred_buffer_scale(&mut self, surface: &mut wl::Surface, factor: i32) {
        println!("preferred buffer scale: {factor}");
        self.ui_scale_factor.set(factor as _);
        // 同じ値を適用することでdpi-awareになるらしい
        surface.set_buffer_scale(factor).unwrap();
        surface.commit().unwrap();
    }

    fn preferred_buffer_transform(&mut self, _surface: &mut wl::Surface, transform: u32) {
        println!("preferred buffer transform: {transform}");
    }
}
impl wl::PointerEventListener for WaylandShellEventHandler {
    fn enter(
        &mut self,
        _pointer: &mut wl::Pointer,
        serial: u32,
        surface: &mut wl::Surface,
        surface_x: wl::Fixed,
        surface_y: wl::Fixed,
    ) {
        self.pointer_on_surface = if core::ptr::addr_eq(surface, self.main_surface_proxy_ptr) {
            PointerOnSurface::Main { serial }
        } else {
            PointerOnSurface::None
        };

        match self.pointer_on_surface {
            PointerOnSurface::None => (),
            PointerOnSurface::Main { serial } => {
                unsafe { &mut *self.app_event_bus }.push_back(AppEvent::MainWindowPointerMove {
                    enter_serial: serial,
                    surface_x: surface_x.to_f32(),
                    surface_y: surface_y.to_f32(),
                })
            }
        }
    }
    fn leave(&mut self, _pointer: &mut wl::Pointer, _serial: u32, surface: &mut wl::Surface) {
        match self.pointer_on_surface {
            PointerOnSurface::None => (),
            PointerOnSurface::Main { .. } => {
                if core::ptr::addr_eq(surface, self.main_surface_proxy_ptr) {
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
            PointerOnSurface::Main { serial } => {
                unsafe { &mut *self.app_event_bus }.push_back(AppEvent::MainWindowPointerMove {
                    enter_serial: serial,
                    surface_x: surface_x.to_f32(),
                    surface_y: surface_y.to_f32(),
                })
            }
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
                    unsafe { &mut *self.app_event_bus }.push_back(
                        AppEvent::MainWindowPointerLeftDown {
                            enter_serial: serial,
                        },
                    );
                } else if button == BTN_LEFT && state == wl::PointerButtonState::Released {
                    unsafe { &mut *self.app_event_bus }.push_back(
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

    fn axis_relative_direction(&mut self, _pointer: &mut wl::Pointer, axis: u32, direction: u32) {
        println!("axis relative direction: {axis} {direction}");
    }
}
impl wl::CallbackEventListener for WaylandShellEventHandler {
    fn done(&mut self, _callback: &mut wl::Callback, _data: u32) {
        unsafe { &mut *self.app_event_bus }.push_back(AppEvent::ToplevelWindowFrameTiming);
    }
}

struct AppShell {
    shell_event_handler: Box<WaylandShellEventHandler>,
    display: wl::Display,
    surface: core::ptr::NonNull<wl::Surface>,
    xdg_surface: core::ptr::NonNull<wl::XdgSurface>,
    cursor_shape_device: core::ptr::NonNull<wl::WpCursorShapeDeviceV1>,
    frame_callback: core::ptr::NonNull<wl::Callback>,
}
impl AppShell {
    pub fn new(events: *mut VecDeque<AppEvent>) -> Self {
        let mut dp = wl::Display::connect().unwrap();
        let mut registry = dp.get_registry().unwrap();
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
                unimplemented!("wl global remove: {name}");
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
        drop(registry);

        let mut compositor = rl.compositor.take().unwrap();
        let mut xdg_wm_base = rl.xdg_wm_base.take().unwrap();
        let mut seat = rl.seat.take().unwrap();
        let mut cursor_shape_manager = rl.cursor_shape_manager.take().unwrap();

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
        dp.roundtrip().expect("Failed to sync");

        let mut pointer = seat_listener.pointer.take().expect("no pointer from seat");
        let cursor_shape_device = cursor_shape_manager
            .get_pointer(&mut pointer)
            .expect("Failed to get cursor shape device");

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

        let mut frame = wl_surface.frame().expect("Failed to request next frame");

        let mut shell_event_handler = Box::new(WaylandShellEventHandler {
            app_event_bus: events,
            ui_scale_factor: Rc::new(Cell::new(2.0)),
            pointer_on_surface: PointerOnSurface::None,
            main_surface_proxy_ptr: wl_surface.as_raw() as _,
        });

        pointer.add_listener(&mut *shell_event_handler).unwrap();
        xdg_surface
            .add_listener(&mut *shell_event_handler)
            .expect("Failed to register toplevel surface event");
        xdg_toplevel
            .add_listener(&mut *shell_event_handler)
            .expect("Failed to register toplevel window event");
        wl_surface.add_listener(&mut *shell_event_handler).unwrap();
        frame
            .add_listener(&mut *shell_event_handler)
            .expect("Failed to set frame callback");

        wl_surface.commit().expect("Failed to commit surface");

        compositor.leak();
        xdg_wm_base.leak();
        seat.leak();
        cursor_shape_manager.leak();
        xdg_toplevel.leak();
        pointer.leak();

        Self {
            shell_event_handler,
            display: dp,
            surface: wl_surface.unwrap(),
            xdg_surface: xdg_surface.unwrap(),
            cursor_shape_device: cursor_shape_device.unwrap(),
            frame_callback: frame.unwrap(),
        }
    }

    pub unsafe fn create_vulkan_surface(
        &mut self,
        instance: &impl br::Instance,
    ) -> br::Result<br::vk::VkSurfaceKHR> {
        unsafe {
            br::WaylandSurfaceCreateInfo::new(
                self.display.as_raw() as _,
                self.surface.as_ptr() as _,
            )
            .execute(instance, None)
        }
    }

    pub fn flush(&mut self) {
        self.display.flush().unwrap();
    }

    pub fn process_pending_events(&mut self) {
        self.display.dispatch().expect("Failed to dispatch");
    }

    pub fn request_next_frame(&mut self) {
        self.frame_callback = unsafe { self.surface.as_mut() }
            .frame()
            .expect("Failed to request next frame")
            .unwrap();
        unsafe { self.frame_callback.as_mut() }
            .add_listener(&mut *self.shell_event_handler)
            .expect("Failed to set frame callback");
    }

    pub fn post_configure(&mut self, serial: u32) {
        println!("ToplevelWindowSurfaceConfigure {serial}");
        unsafe { self.xdg_surface.as_mut() }
            .ack_configure(serial)
            .expect("Failed to ack configure");
    }

    pub fn set_cursor_shape(&mut self, enter_serial: u32, shape: CursorShape) {
        unsafe { self.cursor_shape_device.as_mut() }
            .set_shape(
                enter_serial,
                match shape {
                    CursorShape::Default => WpCursorShapeDeviceV1Shape::Default,
                    CursorShape::ResizeHorizontal => WpCursorShapeDeviceV1Shape::EwResize,
                },
            )
            .unwrap();
    }

    pub fn ui_scale_factor(&self) -> f32 {
        self.shell_event_handler.ui_scale_factor.get()
    }
}

fn main() {
    let mut events = VecDeque::new();
    let mut app_shell = AppShell::new(&mut events);

    // initialize font systems
    crate::fontconfig::init();
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

    let subsystem = Subsystem::init();
    let mut staging_scratch_buffer = StagingScratchBufferManager::new(&subsystem);
    let mut composition_alphamask_surface_atlas = CompositionSurfaceAtlas::new(
        &subsystem,
        subsystem.adapter_properties.limits.maxImageDimension2D,
        br::vk::VK_FORMAT_R8_UNORM,
    );

    let client_size = Cell::new((640.0f32, 480.0));
    let mut app_state = AppState::new();
    let mut pointer_input_manager = PointerInputManager::new();
    let mut ht_manager = HitTestTreeManager::new();
    let ht_root_fallback_action_handler = Rc::new(HitTestRootTreeActionHandler {
        editing_atlas_dragging: Cell::new(false),
        editing_atlas_drag_start_x: Cell::new(0.0),
        editing_atlas_drag_start_y: Cell::new(0.0),
        editing_atlas_drag_start_offset_x: Cell::new(0.0),
        editing_atlas_drag_start_offset_y: Cell::new(0.0),
    });
    let ht_root = ht_manager.create(HitTestTreeData {
        left: 0.0,
        top: 0.0,
        left_adjustment_factor: 0.0,
        top_adjustment_factor: 0.0,
        width: 0.0,
        height: 0.0,
        width_adjustment_factor: 1.0,
        height_adjustment_factor: 1.0,
        action_handler: Some(Rc::downgrade(&ht_root_fallback_action_handler) as _),
    });

    struct SubsystemBoundSurface<'s> {
        handle: br::vk::VkSurfaceKHR,
        subsystem: &'s Subsystem,
    }
    impl Drop for SubsystemBoundSurface<'_> {
        fn drop(&mut self) {
            unsafe {
                br::vkfn_wrapper::destroy_surface(
                    self.subsystem.instance().native_ptr(),
                    self.handle,
                    None,
                );
            }
        }
    }
    impl br::VkHandle for SubsystemBoundSurface<'_> {
        type Handle = br::vk::VkSurfaceKHR;

        #[inline(always)]
        fn native_ptr(&self) -> Self::Handle {
            self.handle
        }
    }
    impl br::InstanceChild for SubsystemBoundSurface<'_> {
        type ConcreteInstance = <Subsystem as br::InstanceChild>::ConcreteInstance;

        #[inline]
        fn instance(&self) -> &Self::ConcreteInstance {
            self.subsystem.instance()
        }
    }
    impl br::Surface for SubsystemBoundSurface<'_> {}

    let surface = SubsystemBoundSurface {
        handle: unsafe {
            app_shell
                .create_vulkan_surface(subsystem.instance())
                .unwrap()
        },
        subsystem: &subsystem,
    };
    let surface_caps = subsystem.adapter().surface_capabilities(&surface).unwrap();
    let surface_formats = subsystem.adapter().surface_formats_alloc(&surface).unwrap();
    let sc_transform = if surface_caps
        .supported_transforms()
        .has_any(br::SurfaceTransformFlags::IDENTITY)
    {
        br::SurfaceTransformFlags::IDENTITY.bits()
    } else {
        surface_caps.currentTransform
    };
    let sc_composite_alpha = if surface_caps
        .supported_composite_alpha()
        .has_any(br::CompositeAlphaFlags::OPAQUE)
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
        .create(&subsystem)
        .unwrap(),
    );

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

    let mut ft_face = ft
        .new_face(&primary_face_path, primary_face_index as _)
        .expect("Failed to create ft face");
    ft_face
        .set_char_size(
            (10.0 * 64.0) as _,
            0,
            (96.0 * app_shell.ui_scale_factor()) as _,
            0,
        )
        .expect("Failed to set char size");

    let mut font_set = FontSet {
        ui_default: ft_face,
    };

    let mut composite_instance_buffer = CompositeInstanceManager::new(&subsystem);
    let mut composite_tree = CompositeTree::new();

    let main_rp = br::RenderPassObject::new(
        &subsystem,
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
            br::ImageViewBuilder::new(
                bb.clone_parent(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .create()
            .unwrap()
        })
        .collect::<Vec<_>>();
    let mut main_fbs = backbuffer_views
        .iter()
        .map(|bb| {
            br::FramebufferObject::new(
                &subsystem,
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

    let composite_sampler =
        br::SamplerObject::new(&subsystem, &br::SamplerCreateInfo::new()).unwrap();

    let composite_vsh = subsystem.load_shader("resources/composite.vert").unwrap();
    let composite_fsh = subsystem.load_shader("resources/composite.frag").unwrap();
    let composite_shader_stages = [
        br::PipelineShaderStage::new(br::ShaderStage::Vertex, &composite_vsh, c"main"),
        br::PipelineShaderStage::new(br::ShaderStage::Fragment, &composite_fsh, c"main"),
    ];

    let composite_descriptor_layout = br::DescriptorSetLayoutObject::new(
        &subsystem,
        &br::DescriptorSetLayoutCreateInfo::new(&[
            br::DescriptorType::StorageBuffer
                .make_binding(0, 1)
                .for_shader_stage(
                    br::vk::VK_SHADER_STAGE_VERTEX_BIT | br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
                ),
            br::DescriptorType::UniformBuffer
                .make_binding(1, 1)
                .for_shader_stage(br::vk::VK_SHADER_STAGE_VERTEX_BIT),
            br::DescriptorType::CombinedImageSampler
                .make_binding(2, 1)
                .only_for_fragment(),
        ]),
    )
    .unwrap();
    let mut descriptor_pool = br::DescriptorPoolObject::new(
        &subsystem,
        &br::DescriptorPoolCreateInfo::new(
            1,
            &[
                br::DescriptorType::CombinedImageSampler.make_size(1),
                br::DescriptorType::UniformBuffer.make_size(1),
                br::DescriptorType::StorageBuffer.make_size(1),
            ],
        ),
    )
    .unwrap();
    let [composite_alphamask_group_descriptor] = descriptor_pool
        .alloc_array(&[composite_descriptor_layout.as_transparent_ref()])
        .unwrap();
    subsystem.update_descriptor_sets(
        &[
            composite_alphamask_group_descriptor.binding_at(0).write(
                br::DescriptorContents::storage_buffer(
                    composite_instance_buffer.buffer(),
                    0..(core::mem::size_of::<CompositeInstanceData>() * 1024) as _,
                ),
            ),
            composite_alphamask_group_descriptor.binding_at(1).write(
                br::DescriptorContents::uniform_buffer(
                    composite_instance_buffer.streaming_buffer(),
                    0..core::mem::size_of::<CompositeStreamingData>() as _,
                ),
            ),
            composite_alphamask_group_descriptor.binding_at(2).write(
                br::DescriptorContents::CombinedImageSampler(vec![
                    br::DescriptorImageInfo::new(
                        composition_alphamask_surface_atlas.resource(),
                        br::ImageLayout::ShaderReadOnlyOpt,
                    )
                    .with_sampler(&composite_sampler),
                ]),
            ),
        ],
        &[],
    );

    let composite_pipeline_layout = br::PipelineLayoutObject::new(
        &subsystem,
        &br::PipelineLayoutCreateInfo::new(
            &[composite_descriptor_layout.as_transparent_ref()],
            &[br::vk::VkPushConstantRange::for_type::<[f32; 2]>(
                br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                0,
            )],
        ),
    )
    .unwrap();
    let composite_vinput = br::PipelineVertexInputStateCreateInfo::new(&[], &[]);
    let composite_ia_state =
        br::PipelineInputAssemblyStateCreateInfo::new(br::PrimitiveTopology::TriangleStrip);
    let composite_raster_state = br::PipelineRasterizationStateCreateInfo::new(
        br::PolygonMode::Fill,
        br::CullModeFlags::NONE,
        br::FrontFace::CounterClockwise,
    );

    let [mut composite_pipeline] = subsystem
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

    let editing_atlas_renderer = Rc::new(RefCell::new(EditingAtlasRenderer::new(
        &subsystem,
        main_rp.subpass(0),
        sc_size,
        SizePixels {
            width: 32,
            height: 32,
        },
    )));
    app_state.register_atlas_size_view_feedback({
        let editing_atlas_renderer = Rc::downgrade(&editing_atlas_renderer);

        move |size| {
            let Some(editing_atlas_renderer) = editing_atlas_renderer.upgrade() else {
                // app teardown-ed
                return;
            };

            editing_atlas_renderer.borrow_mut().set_atlas_size(*size);
        }
    });

    let mut init_context = ViewInitContext {
        subsystem: &subsystem,
        staging_scratch_buffer: &mut staging_scratch_buffer,
        atlas: &mut composition_alphamask_surface_atlas,
        ui_scale_factor: app_shell.ui_scale_factor(),
        fonts: &mut font_set,
        composite_tree: &mut composite_tree,
        composite_instance_manager: &mut composite_instance_buffer,
        ht: &mut ht_manager,
    };

    let app_header = AppHeaderPresenter::new(&mut init_context);
    app_header.mount(CompositeTree::ROOT, init_context.composite_tree);

    let sprite_list_pane = SpriteListPanePresenter::new(&mut init_context, app_header.height());
    sprite_list_pane.mount(
        &mut composite_tree,
        CompositeTree::ROOT,
        &mut ht_manager,
        ht_root,
    );

    println!(
        "Reserved Staging Buffers during UI initialization: {}",
        staging_scratch_buffer.total_reserved_amount()
    );
    ht_manager.dump(ht_root);

    let n = composite_instance_buffer.memory_stg().native_ptr();
    let r = composite_instance_buffer.range_all();
    let flush_required = composite_instance_buffer.memory_stg_requires_explicit_flush();
    let ptr = composite_instance_buffer
        .memory_stg_exc()
        .map(r.clone())
        .unwrap();
    let mut composite_instance_count = unsafe {
        composite_tree.sink_all(
            sc_size,
            0.0,
            br::Extent2D {
                width: composition_alphamask_surface_atlas.size(),
                height: composition_alphamask_surface_atlas.size(),
            },
            &ptr,
        )
    };
    if flush_required {
        unsafe {
            subsystem
                .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(n, 0, r.end as _)])
                .unwrap();
        }
    }
    ptr.end();
    let mut composite_instance_buffer_dirty = true;
    composite_tree.take_dirty(); // mark processed

    let mut main_cp = br::CommandPoolObject::new(
        &subsystem,
        &br::CommandPoolCreateInfo::new(subsystem.graphics_queue_family_index),
    )
    .unwrap();
    let mut main_cbs = br::CommandBufferObject::alloc(
        &subsystem,
        &br::CommandBufferAllocateInfo::new(
            &mut main_cp,
            main_fbs.len() as _,
            br::CommandBufferLevel::Primary,
        ),
    )
    .unwrap();

    for (cb, fb) in main_cbs.iter_mut().zip(main_fbs.iter()) {
        unsafe {
            cb.begin(&br::CommandBufferBeginInfo::new(), &subsystem)
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
        .bind_pipeline(
            br::PipelineBindPoint::Graphics,
            &editing_atlas_renderer.borrow().render_pipeline,
        )
        .push_constant(
            &editing_atlas_renderer.borrow().render_pipeline_layout,
            br::vk::VK_SHADER_STAGE_FRAGMENT_BIT | br::vk::VK_SHADER_STAGE_VERTEX_BIT,
            0,
            &[sc_size.width as f32, sc_size.height as f32],
        )
        .bind_descriptor_sets(
            br::PipelineBindPoint::Graphics,
            &editing_atlas_renderer.borrow().render_pipeline_layout,
            0,
            &[editing_atlas_renderer.borrow().ds_param],
            &[],
        )
        .draw(3, 1, 0, 0)
        .bind_pipeline(
            br::PipelineBindPoint::Graphics,
            &editing_atlas_renderer.borrow().bg_render_pipeline,
        )
        .bind_vertex_buffer_array(
            0,
            &[editing_atlas_renderer
                .borrow()
                .bg_vertex_buffer
                .as_transparent_ref()],
            &[0],
        )
        .draw(4, 1, 0, 0)
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
            &[composite_alphamask_group_descriptor],
            &[],
        )
        .draw(4, composite_instance_count as _, 0, 0)
        .end_render_pass2(&br::SubpassEndInfo::new())
        .end()
        .unwrap();
    }

    let mut update_cp = br::CommandPoolObject::new(
        &subsystem,
        &br::CommandPoolCreateInfo::new(subsystem.graphics_queue_family_index),
    )
    .unwrap();
    let [mut update_cb] = br::CommandBufferObject::alloc_array(
        &subsystem,
        &br::CommandBufferFixedCountAllocateInfo::new(
            &mut update_cp,
            br::CommandBufferLevel::Primary,
        ),
    )
    .unwrap();

    let mut acquire_completion =
        br::SemaphoreObject::new(&subsystem, &br::SemaphoreCreateInfo::new()).unwrap();
    let render_completion =
        br::SemaphoreObject::new(&subsystem, &br::SemaphoreCreateInfo::new()).unwrap();
    let mut last_render_command_fence =
        br::FenceObject::new(&subsystem, &br::FenceCreateInfo::new(0)).unwrap();
    let mut last_rendering;
    let mut last_update_command_fence =
        br::FenceObject::new(&subsystem, &br::FenceCreateInfo::new(0)).unwrap();
    let mut last_updating = false;

    // fire initial update/render
    if core::mem::replace(&mut composite_instance_buffer_dirty, false) {
        unsafe {
            update_cb
                .begin(&br::CommandBufferBeginInfo::new(), &subsystem)
                .unwrap()
        }
        .inject(|r| composite_instance_buffer.sync_buffer(r))
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[br::MemoryBarrier2::new()
                .from(
                    br::PipelineStageFlags2::COPY,
                    br::AccessFlags2::TRANSFER.write,
                )
                .to(
                    br::PipelineStageFlags2::VERTEX_SHADER,
                    br::AccessFlags2::SHADER.read,
                )],
            &[],
            &[],
        ))
        .end()
        .unwrap();
        subsystem
            .submit_graphics_works(
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
    subsystem
        .submit_graphics_works(
            &[br::SubmitInfo2::new(
                &[br::SemaphoreSubmitInfo::new(&acquire_completion).on_color_attachment_output()],
                &[br::CommandBufferSubmitInfo::new(&main_cbs[next as usize])],
                &[br::SemaphoreSubmitInfo::new(&render_completion).on_color_attachment_output()],
            )],
            Some(last_render_command_fence.as_transparent_ref_mut()),
        )
        .unwrap();
    last_rendering = true;
    subsystem
        .queue_present(&br::PresentInfo::new(
            &[render_completion.as_transparent_ref()],
            &[sc.as_transparent_ref()],
            &[next],
            &mut [br::vk::VkResult(0)],
        ))
        .unwrap();

    let mut app_update_context = AppUpdateContext {
        composite_tree,
        state: app_state,
        editing_atlas_renderer,
        current_sec: 0.0,
    };

    app_shell.flush();

    let t = std::time::Instant::now();
    let mut last_frame_t = Duration::ZERO;
    let mut frame_resize_request = None;
    let mut last_render_scale = app_shell.ui_scale_factor();
    let mut last_render_size = sc_size;
    let mut last_pointer_pos = (0.0f32, 0.0f32);
    let mut last_composite_instance_count = composite_instance_count;
    'app: loop {
        app_shell.process_pending_events();
        for e in events.drain(..) {
            match e {
                AppEvent::ToplevelWindowClose => break 'app,
                AppEvent::ToplevelWindowFrameTiming => {
                    let current_t = t.elapsed();
                    let dt = current_t - last_frame_t;
                    last_frame_t = current_t;
                    // print!("frame {dt:?}\n");

                    if last_rendering {
                        last_render_command_fence.wait().unwrap();
                        last_render_command_fence.reset().unwrap();
                        last_rendering = false;
                    }

                    if app_update_context.composite_tree.take_dirty()
                        || last_render_scale != app_shell.ui_scale_factor()
                        || last_render_size != sc_size
                        || true
                    {
                        let n = composite_instance_buffer.memory_stg().native_ptr();
                        let r = composite_instance_buffer.range_all();
                        let flush_required =
                            composite_instance_buffer.memory_stg_requires_explicit_flush();
                        let ptr = composite_instance_buffer
                            .memory_stg_exc()
                            .map(r.clone())
                            .unwrap();
                        composite_instance_count = unsafe {
                            app_update_context.composite_tree.sink_all(
                                sc_size,
                                current_t.as_secs_f32(),
                                br::Extent2D::spread1(
                                    composition_alphamask_surface_atlas.size() as _
                                ),
                                &ptr,
                            )
                        };
                        if flush_required {
                            unsafe {
                                subsystem
                                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                                        n, 0, r.end as _,
                                    )])
                                    .unwrap();
                            }
                        }
                        ptr.end();
                        composite_instance_buffer_dirty = true;

                        last_render_scale = app_shell.ui_scale_factor();
                        last_render_size = sc_size;
                    }

                    let composite_instance_buffer_dirty =
                        core::mem::replace(&mut composite_instance_buffer_dirty, false);
                    let needs_update = composite_instance_buffer_dirty
                        || app_update_context
                            .editing_atlas_renderer
                            .borrow()
                            .is_dirty();

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
                                .begin(&br::CommandBufferBeginInfo::new(), &subsystem)
                                .unwrap()
                        };
                        let rec = if composite_instance_buffer_dirty {
                            composite_instance_buffer.sync_buffer(rec)
                        } else {
                            rec
                        };
                        rec.pipeline_barrier_2(&br::DependencyInfo::new(
                            &[br::MemoryBarrier2::new()
                                .from(
                                    br::PipelineStageFlags2::COPY,
                                    br::AccessFlags2::TRANSFER.write,
                                )
                                .to(
                                    br::PipelineStageFlags2::VERTEX_SHADER,
                                    br::AccessFlags2::SHADER.read,
                                )],
                            &[],
                            &[],
                        ))
                        .inject(|r| {
                            app_update_context
                                .editing_atlas_renderer
                                .borrow_mut()
                                .process_dirty_data(r)
                        })
                        .end()
                        .unwrap();
                    }

                    let n = composite_instance_buffer
                        .streaming_memory_exc()
                        .native_ptr();
                    let flush_required =
                        composite_instance_buffer.streaming_memory_requires_flush();
                    let mapped = composite_instance_buffer
                        .streaming_memory_exc()
                        .map(0..core::mem::size_of::<CompositeStreamingData>())
                        .unwrap();
                    unsafe {
                        core::ptr::write(
                            &mut (*mapped.addr_of_mut::<CompositeStreamingData>(0)).current_sec,
                            current_t.as_secs_f32(),
                        );
                    }
                    if flush_required {
                        unsafe {
                            subsystem
                                .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                                    n,
                                    0,
                                    core::mem::size_of::<CompositeStreamingData>() as _,
                                )])
                                .unwrap();
                        }
                    }
                    mapped.end();

                    if needs_update {
                        subsystem
                            .submit_graphics_works(
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

                    if last_composite_instance_count != composite_instance_count {
                        // needs update render commands
                        unsafe {
                            main_cp.reset(br::CommandPoolResetFlags::EMPTY).unwrap();
                        }

                        for (cb, fb) in main_cbs.iter_mut().zip(main_fbs.iter()) {
                            unsafe {
                                cb.begin(&br::CommandBufferBeginInfo::new(), &subsystem)
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
                            .bind_pipeline(
                                br::PipelineBindPoint::Graphics,
                                &app_update_context
                                    .editing_atlas_renderer
                                    .borrow()
                                    .render_pipeline,
                            )
                            .push_constant(
                                &app_update_context
                                    .editing_atlas_renderer
                                    .borrow()
                                    .render_pipeline_layout,
                                br::vk::VK_SHADER_STAGE_FRAGMENT_BIT
                                    | br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                                0,
                                &[sc_size.width as f32, sc_size.height as f32],
                            )
                            .bind_descriptor_sets(
                                br::PipelineBindPoint::Graphics,
                                &app_update_context
                                    .editing_atlas_renderer
                                    .borrow()
                                    .render_pipeline_layout,
                                0,
                                &[app_update_context.editing_atlas_renderer.borrow().ds_param],
                                &[],
                            )
                            .draw(3, 1, 0, 0)
                            .bind_pipeline(
                                br::PipelineBindPoint::Graphics,
                                &app_update_context
                                    .editing_atlas_renderer
                                    .borrow()
                                    .bg_render_pipeline,
                            )
                            .bind_vertex_buffer_array(
                                0,
                                &[app_update_context
                                    .editing_atlas_renderer
                                    .borrow()
                                    .bg_vertex_buffer
                                    .as_transparent_ref()],
                                &[0],
                            )
                            .draw(4, 1, 0, 0)
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
                                &[composite_alphamask_group_descriptor],
                                &[],
                            )
                            .draw(4, composite_instance_count as _, 0, 0)
                            .end_render_pass2(&br::SubpassEndInfo::new())
                            .end()
                            .unwrap();
                        }

                        last_composite_instance_count = composite_instance_count;
                    }

                    let next = sc
                        .acquire_next(
                            None,
                            br::CompletionHandlerMut::Queue(
                                acquire_completion.as_transparent_ref_mut(),
                            ),
                        )
                        .unwrap();
                    subsystem
                        .submit_graphics_works(
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
                    subsystem
                        .queue_present(&br::PresentInfo::new(
                            &[render_completion.as_transparent_ref()],
                            &[sc.as_transparent_ref()],
                            &[next],
                            &mut [br::vk::VkResult(0)],
                        ))
                        .unwrap();

                    app_shell.request_next_frame();
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
                            sc_size.width = (w as f32 * app_shell.ui_scale_factor()) as _;
                            sc_size.height = (h as f32 * app_shell.ui_scale_factor()) as _;

                            if last_rendering {
                                last_render_command_fence.wait().unwrap();
                                last_render_command_fence.reset().unwrap();
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
                                .create(&subsystem)
                                .unwrap(),
                            );

                            backbuffer_views = sc
                                .images_alloc()
                                .unwrap()
                                .into_iter()
                                .map(|bb| {
                                    br::ImageViewBuilder::new(
                                        bb.clone_parent(),
                                        br::ImageSubresourceRange::new(
                                            br::AspectMask::COLOR,
                                            0..1,
                                            0..1,
                                        ),
                                    )
                                    .create()
                                    .unwrap()
                                })
                                .collect::<Vec<_>>();
                            main_fbs = backbuffer_views
                                .iter()
                                .map(|bb| {
                                    br::FramebufferObject::new(
                                        &subsystem,
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

                            let [composite_pipeline1] = subsystem
                                .new_graphics_pipeline_array(
                                    &[br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp.subpass(0),
                                        &composite_shader_stages,
                                        &composite_vinput,
                                        &composite_ia_state,
                                        &br::PipelineViewportStateCreateInfo::new(
                                            &[sc_size.into_rect(br::Offset2D::ZERO).make_viewport(0.0..1.0)],
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
                            composite_pipeline = composite_pipeline1;

                            app_update_context
                                .editing_atlas_renderer
                                .borrow_mut()
                                .recreate(&subsystem, main_rp.subpass(0), sc_size);

                            for (cb, fb) in main_cbs.iter_mut().zip(main_fbs.iter()) {
                                unsafe {
                                    cb.begin(&br::CommandBufferBeginInfo::new(), &subsystem)
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
                                .bind_pipeline(
                                    br::PipelineBindPoint::Graphics,
                                    &app_update_context
                                        .editing_atlas_renderer
                                        .borrow()
                                        .render_pipeline,
                                )
                                .push_constant(
                                    &app_update_context
                                        .editing_atlas_renderer
                                        .borrow()
                                        .render_pipeline_layout,
                                    br::vk::VK_SHADER_STAGE_FRAGMENT_BIT
                                        | br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                                    0,
                                    &[sc_size.width as f32, sc_size.height as f32],
                                )
                                .bind_descriptor_sets(
                                    br::PipelineBindPoint::Graphics,
                                    &app_update_context
                                        .editing_atlas_renderer
                                        .borrow()
                                        .render_pipeline_layout,
                                    0,
                                    &[app_update_context.editing_atlas_renderer.borrow().ds_param],
                                    &[],
                                )
                                .draw(3, 1, 0, 0)
                                .bind_pipeline(
                                    br::PipelineBindPoint::Graphics,
                                    &app_update_context
                                        .editing_atlas_renderer
                                        .borrow()
                                        .bg_render_pipeline,
                                )
                                .bind_vertex_buffer_array(
                                    0,
                                    &[app_update_context
                                        .editing_atlas_renderer
                                        .borrow()
                                        .bg_vertex_buffer
                                        .as_transparent_ref()],
                                    &[0],
                                )
                                .draw(4, 1, 0, 0)
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
                                    &[composite_alphamask_group_descriptor],
                                    &[],
                                )
                                .draw(4, composite_instance_count as _, 0, 0)
                                .end_render_pass2(&br::SubpassEndInfo::new())
                                .end()
                                .unwrap();
                            }
                        }
                    }

                    app_shell.post_configure(serial);
                }
                AppEvent::MainWindowPointerMove {
                    enter_serial,
                    surface_x,
                    surface_y,
                } => {
                    app_update_context.current_sec = t.elapsed().as_secs_f32();
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
                    app_shell.set_cursor_shape(
                        enter_serial,
                        pointer_input_manager
                            .cursor_shape(&mut ht_manager, &mut app_update_context),
                    );

                    last_pointer_pos = (surface_x, surface_y);
                }
                AppEvent::MainWindowPointerLeftDown { enter_serial } => {
                    app_update_context.current_sec = t.elapsed().as_secs_f32();
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

                    app_shell.set_cursor_shape(
                        enter_serial,
                        pointer_input_manager
                            .cursor_shape(&mut ht_manager, &mut app_update_context),
                    );
                }
                AppEvent::MainWindowPointerLeftUp { enter_serial } => {
                    app_update_context.current_sec = t.elapsed().as_secs_f32();
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

                    app_shell.set_cursor_shape(
                        enter_serial,
                        pointer_input_manager
                            .cursor_shape(&mut ht_manager, &mut app_update_context),
                    );
                }
            }
        }
    }

    unsafe {
        subsystem.wait().unwrap();
    }
}
