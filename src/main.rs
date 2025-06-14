mod app_state;
mod component;
mod composite;
mod coordinate;
mod feature;
mod hittest;
mod input;
mod mathext;
mod peridot;
mod platform;
mod shell;
mod subsystem;
mod svg;
mod text;
mod thirdparty;

use std::{
    cell::{Cell, RefCell, UnsafeCell},
    collections::{HashMap, VecDeque},
    path::Path,
    rc::Rc,
};

use app_state::AppState;
use bedrock::{
    self as br, CommandBufferMut, CommandPoolMut, DescriptorPoolMut, Device, DeviceMemoryMut,
    Fence, FenceMut, Image, ImageChild, InstanceChild, MemoryBound, PhysicalDevice, RenderPass,
    ShaderModule, Swapchain, VkHandle, VkHandleMut, VkObject, VkRawHandle,
};
use component::CommonButtonView;
use composite::{
    AnimatableColor, AnimatableFloat, AnimationData, AtlasRect, CompositeInstanceData,
    CompositeInstanceManager, CompositeMode, CompositeRect, CompositeRenderingData,
    CompositeRenderingInstruction, CompositeStreamingData, CompositeTree,
    CompositeTreeFloatParameterRef, CompositeTreeRef, CompositionSurfaceAtlas, RenderPassType,
};
use coordinate::SizePixels;
use feature::editing_atlas_renderer::EditingAtlasRenderer;
use hittest::{
    CursorShape, HitTestTreeActionHandler, HitTestTreeData, HitTestTreeManager, HitTestTreeRef,
};
use input::{EventContinueControl, PointerInputManager};
use subsystem::{StagingScratchBufferManager, Subsystem};
use text::TextLayout;
use thirdparty::{
    dbus, fontconfig,
    freetype::{self, FreeType},
};

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
    UIPopupClose {
        id: uuid::Uuid,
    },
    UIMessageDialogRequest {
        content: String,
    },
    UIPopupUnmount {
        id: uuid::Uuid,
    },
}

pub struct AppEventBus {
    queue: UnsafeCell<VecDeque<AppEvent>>,
}
impl AppEventBus {
    pub fn push(&self, e: AppEvent) {
        unsafe { &mut *self.queue.get() }.push_back(e);
    }

    fn pop(&self) -> Option<AppEvent> {
        unsafe { &mut *self.queue.get() }.pop_front()
    }
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
const IA_STATE_TRIFAN: &'static br::PipelineInputAssemblyStateCreateInfo =
    &br::PipelineInputAssemblyStateCreateInfo::new(br::PrimitiveTopology::TriangleFan);
const VI_STATE_EMPTY: &'static br::PipelineVertexInputStateCreateInfo<'static> =
    &br::PipelineVertexInputStateCreateInfo::new(&[], &[]);
const VI_STATE_FLOAT4_ONLY: &'static br::PipelineVertexInputStateCreateInfo<'static> =
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
const VI_STATE_FLOAT2_ONLY: &'static br::PipelineVertexInputStateCreateInfo<'static> =
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
    );

#[derive(br::SpecializationConstants)]
pub struct FillcolorRConstants {
    #[constant_id = 0]
    pub r: f32,
}

#[derive(br::SpecializationConstants)]
pub struct RoundedRectConstants {
    #[constant_id = 0]
    pub corner_radius: f32,
}

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

pub struct PresenterInitContext<'d, 'r> {
    pub for_view: ViewInitContext<'d, 'r>,
    pub app_state: &'r mut AppState<'d>,
}

pub struct ViewFeedbackContext {
    pub composite_tree: CompositeTree,
    pub current_sec: f32,
}

pub struct AppUpdateContext<'d> {
    pub for_view_feedback: ViewFeedbackContext,
    pub state: AppState<'d>,
    pub editing_atlas_renderer: Rc<RefCell<EditingAtlasRenderer<'d>>>,
    pub event_queue: &'d AppEventBus,
    pub dbus: &'d dbus::Connection,
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

    #[tracing::instrument(name = "SpriteListToggleButtonView::new", skip(init))]
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
            init.subsystem,
            &br::BufferCreateInfo::new(
                bufsize,
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::INDEX_BUFFER,
            ),
        )
        .unwrap();
        let mreq = buf.requirements();
        let memindex = init
            .subsystem
            .find_direct_memory_index(mreq.memoryTypeBits)
            .expect("no suitable memory");
        let mut mem = br::DeviceMemoryObject::new(
            init.subsystem,
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
        if !init.subsystem.is_coherent_memory_type(memindex) {
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
            init.subsystem,
            &br::ImageCreateInfo::new(icon_atlas_rect.extent(), br::vk::VK_FORMAT_R8_UNORM)
                .as_color_attachment()
                .usage_with(br::ImageUsageFlags::TRANSFER_SRC)
                .sample_counts(4),
        )
        .unwrap();
        let mreq = msaa_buffer.requirements();
        let memindex = init
            .subsystem
            .find_device_local_memory_index(mreq.memoryTypeBits)
            .expect("no suitable memory for msaa buffer");
        let msaa_mem = br::DeviceMemoryObject::new(
            init.subsystem,
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
            init.subsystem,
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
            init.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp,
                &[msaa_buffer.as_transparent_ref()],
                icon_atlas_rect.width(),
                icon_atlas_rect.height(),
            ),
        )
        .unwrap();
        let rp_direct = br::RenderPassObject::new(
            init.subsystem,
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
            init.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp_direct,
                &[init.atlas.resource().as_transparent_ref()],
                init.atlas.size(),
                init.atlas.size(),
            ),
        )
        .unwrap();

        #[derive(br::SpecializationConstants)]
        struct CircleFragmentShaderParams {
            #[constant_id = 0]
            pub softness: f32,
        }
        let [pipeline, pipeline_circle] = init
            .subsystem
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    init.subsystem.require_empty_pipeline_layout(),
                    rp.subpass(0),
                    &[
                        init.subsystem
                            .require_shader("resources/notrans.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
                            .require_shader("resources/fillcolor_r.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &FillcolorRConstants { r: 1.0 },
                            )),
                    ],
                    VI_STATE_FLOAT2_ONLY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[icon_atlas_rect
                            .extent()
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[icon_atlas_rect.extent().into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
                ),
                br::GraphicsPipelineCreateInfo::new(
                    init.subsystem.require_empty_pipeline_layout(),
                    rp_direct.subpass(0),
                    &[
                        init.subsystem
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
                            .require_shader("resources/aa_circle.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &CircleFragmentShaderParams { softness: 0.0 },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[circle_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                        &[circle_atlas_rect.vk_rect()],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        let mut cp = init
            .subsystem
            .create_transient_graphics_command_pool()
            .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            init.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(
                &br::CommandBufferBeginInfo::new().onetime_submit(),
                init.subsystem,
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
            event_on_complete: None,
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
            event_on_complete: None,
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
            CompositeMode::ColorTint(ref x) => {
                x.evaluate(current_sec, composite_tree.parameter_store())
            }
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
                    event_on_complete: None,
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

    #[tracing::instrument(name = "SpriteListCellView::new", skip(init))]
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
                &[br::AttachmentDescription2::new(init.atlas.format())
                    .with_layout_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())
                    .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)],
                &[br::SubpassDescription2::new()
                    .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
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

        let [pipeline] = init
            .subsystem
            .create_graphics_pipelines_array(&[br::GraphicsPipelineCreateInfo::new(
                init.subsystem.require_empty_pipeline_layout(),
                render_pass.subpass(0),
                &[
                    init.subsystem
                        .require_shader("resources/filltri.vert")
                        .on_stage(br::ShaderStage::Vertex, c"main"),
                    init.subsystem
                        .require_shader("resources/rounded_rect.frag")
                        .on_stage(br::ShaderStage::Fragment, c"main"),
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
            .multisample_state(MS_STATE_EMPTY)])
            .unwrap();

        let mut cp = init
            .subsystem
            .create_transient_graphics_command_pool()
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
            &[br::ImageMemoryBarrier2::new(
                init.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
        ))
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

    #[tracing::instrument(name = "SpriteListPaneView::new", skip(init))]
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
            init.subsystem,
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
            .find_device_local_memory_index(mreq.memoryTypeBits)
            .expect("no suitable memory index");
        let title_blurred_work_image_mem = br::DeviceMemoryObject::new(
            init.subsystem,
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
            init.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(init.atlas.format())
                    .with_layout_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())
                    .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)],
                &[br::SubpassDescription2::new()
                    .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
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

        let vsh_blur = init
            .subsystem
            .require_shader("resources/filltri_uvmod.vert");
        let fsh_blur = init
            .subsystem
            .require_shader("resources/blit_axis_convolved.frag");
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

        let blur_pipeline_layout = br::PipelineLayoutObject::new(
            init.subsystem,
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
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    init.subsystem.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.subsystem
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
                            .require_shader("resources/rounded_rect.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main"),
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
            ])
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

        let mut cp = init
            .subsystem
            .create_transient_graphics_command_pool()
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
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            composite_mode: CompositeMode::ColorTintBackdropBlur(
                AnimatableColor::Value([1.0, 1.0, 1.0, 0.25]),
                AnimatableFloat::Value(3.0),
            ),
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
            event_on_complete: None,
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
            event_on_complete: None,
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
            self.toggle_button_view.on_hover(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );

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
            self.toggle_button_view.on_leave(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );

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
            self.toggle_button_view.on_press(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );

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
                    self.view
                        .set_width(w, &mut context.for_view_feedback.composite_tree, ht);

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
                    self.view
                        .set_width(w, &mut context.for_view_feedback.composite_tree, ht);

                    return EventContinueControl::RELEASE_CAPTURE_ELEMENT;
                }
            }
        }

        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view.on_release(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );

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
                self.view.show(
                    &mut context.for_view_feedback.composite_tree,
                    ht,
                    context.for_view_feedback.current_sec,
                );
                self.toggle_button_view.place_inner(
                    &mut context.for_view_feedback.composite_tree,
                    ht,
                    context.for_view_feedback.current_sec,
                );
            } else {
                self.view.hide(
                    &mut context.for_view_feedback.composite_tree,
                    ht,
                    context.for_view_feedback.current_sec,
                );
                self.toggle_button_view.place_outer(
                    &mut context.for_view_feedback.composite_tree,
                    ht,
                    context.for_view_feedback.current_sec,
                );
            }

            self.toggle_button_view
                .flip_icon(!show, &mut context.for_view_feedback.composite_tree);

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
    pub fn new(init: &mut PresenterInitContext, header_height: f32) -> Self {
        let view = Rc::new(SpriteListPaneView::new(&mut init.for_view, header_height));
        let toggle_button_view = Rc::new(SpriteListToggleButtonView::new(&mut init.for_view));

        let cell_view = SpriteListCellView::new(&mut init.for_view, "sprite cell", 32.0);

        toggle_button_view.mount(
            init.for_view.composite_tree,
            view.ct_root,
            init.for_view.ht,
            view.ht_frame,
        );
        toggle_button_view.place_inner(init.for_view.composite_tree, init.for_view.ht, -0.25);

        cell_view.mount(view.ct_root, init.for_view.composite_tree);

        let ht_action_handler = Rc::new(SpriteListPaneActionHandler {
            view: view.clone(),
            toggle_button_view: toggle_button_view.clone(),
            ht_resize_area: view.ht_resize_area,
            resize_state: Cell::new(None),
            shown: Cell::new(true),
        });
        init.for_view.ht.get_data_mut(view.ht_frame).action_handler =
            Some(Rc::downgrade(&ht_action_handler) as _);
        init.for_view
            .ht
            .get_data_mut(view.ht_resize_area)
            .action_handler = Some(Rc::downgrade(&ht_action_handler) as _);
        init.for_view
            .ht
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

#[derive(Debug, Clone, Copy)]
pub enum AppMenuCommandIdentifier {
    AddSprite,
    Save,
}

struct AppMenuButtonView {
    ct_root: CompositeTreeRef,
    ct_icon: CompositeTreeRef,
    ct_label: CompositeTreeRef,
    ct_bg_alpha_rate_shown: CompositeTreeFloatParameterRef,
    ct_bg_alpha_rate_pointer: CompositeTreeFloatParameterRef,
    ht_root: HitTestTreeRef,
    left: f32,
    top: f32,
    ui_scale_factor: f32,
    hovering: Cell<bool>,
    pressing: Cell<bool>,
    command_id: AppMenuCommandIdentifier,
}
impl AppMenuButtonView {
    const ICON_SIZE: f32 = 24.0;
    const BUTTON_HEIGHT: f32 = Self::ICON_SIZE + 8.0 * 2.0;
    const HPADDING: f32 = 16.0;
    const ICON_LABEL_GAP: f32 = 4.0;

    const CONTENT_COLOR_SHOWN: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const CONTENT_COLOR_HIDDEN: [f32; 4] = [1.0, 1.0, 1.0, 0.0];

    #[tracing::instrument(name = "AppMenuButtonView::new", skip(init), fields(icon_path = %icon_path.as_ref().display()))]
    pub fn new(
        init: &mut ViewInitContext,
        label: &str,
        icon_path: impl AsRef<Path>,
        left: f32,
        top: f32,
        command_id: AppMenuCommandIdentifier,
    ) -> Self {
        let label_layout = TextLayout::build_simple(label, &mut init.fonts.ui_default);

        let bg_atlas_rect = init.atlas.alloc(
            ((Self::BUTTON_HEIGHT + 1.0) * init.ui_scale_factor) as u32,
            ((Self::BUTTON_HEIGHT + 1.0) * init.ui_scale_factor) as u32,
        );
        let icon_atlas_rect = init.atlas.alloc(
            (Self::ICON_SIZE * init.ui_scale_factor) as _,
            (Self::ICON_SIZE * init.ui_scale_factor) as _,
        );
        let label_atlas_rect = init
            .atlas
            .alloc(label_layout.width_px(), label_layout.height_px());

        let icon_svg_content = std::fs::read_to_string(icon_path).unwrap();
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
                        viewbox = Some(svg::ViewBox::from_str_bytes(viewbox_value));
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
                        for x in svg::InstructionParser::new_bytes(path_data) {
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
            init.subsystem,
            &br::ImageCreateInfo::new(icon_atlas_rect.extent(), br::vk::VK_FORMAT_S8_UINT)
                .as_depth_stencil_attachment()
                .as_transient_attachment()
                .sample_counts(4),
        )
        .unwrap();
        let req = stencil_buffer.requirements();
        let memindex = init
            .subsystem
            .find_device_local_memory_index(req.memoryTypeBits)
            .expect("No suitable memory for stencil buffer");
        let stencil_buffer_mem = br::DeviceMemoryObject::new(
            init.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        stencil_buffer.bind(&stencil_buffer_mem, 0).unwrap();
        let stencil_buffer = br::ImageViewBuilder::new(
            stencil_buffer,
            br::ImageSubresourceRange::new(br::AspectMask::STENCIL, 0..1, 0..1),
        )
        .create()
        .unwrap();

        let mut ms_color_buffer = br::ImageObject::new(
            init.subsystem,
            &br::ImageCreateInfo::new(icon_atlas_rect.extent(), br::vk::VK_FORMAT_R8_UNORM)
                .as_color_attachment()
                .usage_with(br::ImageUsageFlags::TRANSFER_SRC)
                .sample_counts(4),
        )
        .unwrap();
        let req = ms_color_buffer.requirements();
        let memindex = init
            .subsystem
            .find_device_local_memory_index(req.memoryTypeBits)
            .expect("No suitable memory for msaa color buffer");
        let ms_color_buffer_mem = br::DeviceMemoryObject::new(
            init.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        ms_color_buffer.bind(&ms_color_buffer_mem, 0).unwrap();
        let ms_color_buffer = br::ImageViewBuilder::new(
            ms_color_buffer,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()
        .unwrap();

        let render_pass = br::RenderPassObject::new(
            init.subsystem,
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
            init.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[
                    ms_color_buffer.as_transparent_ref(),
                    stencil_buffer.as_transparent_ref(),
                ],
                icon_atlas_rect.width(),
                icon_atlas_rect.height(),
            ),
        )
        .unwrap();

        let round_rect_rp = br::RenderPassObject::new(
            init.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(init.atlas.format())
                    .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)
                    .with_layout_to(br::ImageLayout::TransferDestOpt.from_undefined())],
                &[br::SubpassDescription2::new()
                    .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
                &[],
            ),
        )
        .unwrap();
        let round_rect_fb = br::FramebufferObject::new(
            init.subsystem,
            &br::FramebufferCreateInfo::new(
                &round_rect_rp,
                &[init.atlas.resource().as_transparent_ref()],
                init.atlas.size(),
                init.atlas.size(),
            ),
        )
        .unwrap();

        let local_viewports = [icon_atlas_rect
            .extent()
            .into_rect(br::Offset2D::ZERO)
            .make_viewport(0.0..1.0)];
        let local_scissor_rects = [icon_atlas_rect.extent().into_rect(br::Offset2D::ZERO)];
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
            round_rect_pipeline,
            first_stencil_shape_pipeline,
            curve_stencil_shape_pipeline,
            colorize_pipeline,
        ] = init
            .subsystem
            .create_graphics_pipelines_array(&[
                // round rect pipeline
                br::GraphicsPipelineCreateInfo::new(
                    init.subsystem.require_empty_pipeline_layout(),
                    round_rect_rp.subpass(0),
                    &[
                        init.subsystem
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
                            .require_shader("resources/rounded_rect.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &RoundedRectConstants {
                                    corner_radius: Self::BUTTON_HEIGHT * 0.5,
                                },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new_array(
                        &[bg_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                        &[bg_atlas_rect.vk_rect()],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(MS_STATE_EMPTY),
                // first stencil shape pipeline
                br::GraphicsPipelineCreateInfo::new(
                    init.subsystem.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.subsystem
                            .require_shader("resources/normalized_01_2d.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
                            .require_shader("resources/stencil_only.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_FLOAT2_ONLY,
                    IA_STATE_TRIFAN,
                    &vp_state_local,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .depth_stencil_state(
                    &br::PipelineDepthStencilStateCreateInfo::new()
                        .stencil_test(true)
                        .stencil_state_back(sop_invert_always.clone())
                        .stencil_state_front(sop_invert_always.clone()),
                )
                .multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
                ),
                // curve stencil shape
                br::GraphicsPipelineCreateInfo::new(
                    init.subsystem.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.subsystem
                            .require_shader("resources/normalized_01_2d_with_uv.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
                            .require_shader("resources/stencil_loop_blinn_curve.frag")
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
                .depth_stencil_state(
                    &br::PipelineDepthStencilStateCreateInfo::new()
                        .stencil_test(true)
                        .stencil_state_back(sop_invert_always.clone())
                        .stencil_state_front(sop_invert_always),
                )
                .multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
                ),
                // colorize pipeline
                br::GraphicsPipelineCreateInfo::new(
                    init.subsystem.require_empty_pipeline_layout(),
                    render_pass.subpass(1),
                    &[
                        init.subsystem
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
                            .require_shader("resources/fillcolor_r.frag")
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
                .depth_stencil_state(
                    &br::PipelineDepthStencilStateCreateInfo::new()
                        .stencil_test(true)
                        .stencil_state_back(sop_testonly_equal_1.clone())
                        .stencil_state_front(sop_testonly_equal_1),
                )
                .multisample_state(
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
                &svg::Instruction::Move { absolute, x, y } => {
                    if current_figure_first_index.is_some() {
                        panic!("not closed last figure");
                    }

                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p = if absolute { (x, y) } else { (p.0 + x, p.1 + y) };
                    current_figure_first_index = Some(trifan_points.len());

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &svg::Instruction::Line { absolute, x, y } => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p = if absolute { (x, y) } else { (p.0 + x, p.1 + y) };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &svg::Instruction::HLine { absolute, x } => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p.0 = if absolute { x } else { p.0 + x };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &svg::Instruction::VLine { absolute, y } => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p.1 = if absolute { y } else { p.1 + y };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &svg::Instruction::BezierCurve {
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
                &svg::Instruction::SequentialBezierCurve {
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
                &svg::Instruction::QuadraticBezierCurve {
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
                &svg::Instruction::SequentialQuadraticBezierCurve { absolute, x, y } => {
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
                &svg::Instruction::Arc {
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
                &svg::Instruction::Close => {
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
            init.subsystem,
            &br::BufferCreateInfo::new(
                curve_triangle_points_offset
                    + curve_triangle_points.len() * core::mem::size_of::<[f32; 4]>(),
                br::BufferUsage::VERTEX_BUFFER,
            ),
        )
        .unwrap();
        let req = vbuf.requirements();
        let memindex = init
            .subsystem
            .find_direct_memory_index(req.memoryTypeBits)
            .unwrap();
        let mut mem = br::DeviceMemoryObject::new(
            init.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        vbuf.bind(&mem, 0).unwrap();
        let h = mem.native_ptr();
        let requires_flush = !init.subsystem.is_coherent_memory_type(memindex);
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
                init.subsystem
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(h, 0, req.size)])
                    .unwrap();
            }
        }
        unsafe {
            mem.unmap();
        }

        let label_image_pixels =
            label_layout.build_stg_image_pixel_buffer(&mut init.staging_scratch_buffer);

        let mut cp = init
            .subsystem
            .create_transient_graphics_command_pool()
            .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            init.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(
                &br::CommandBufferBeginInfo::new().onetime_submit(),
                init.subsystem,
            )
            .unwrap()
        }
        .begin_render_pass2(
            &br::RenderPassBeginInfo::new(
                &round_rect_rp,
                &round_rect_fb,
                bg_atlas_rect.vk_rect(),
                &[],
            ),
            &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
        )
        .bind_pipeline(br::PipelineBindPoint::Graphics, &round_rect_pipeline)
        .draw(3, 1, 0, 0)
        .end_render_pass2(&br::SubpassEndInfo::new())
        .begin_render_pass2(
            &br::RenderPassBeginInfo::new(
                &render_pass,
                &fb,
                icon_atlas_rect.extent().into_rect(br::Offset2D::ZERO),
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
            &[br::ImageMemoryBarrier2::new(
                init.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())
            .of_execution(br::PipelineStageFlags2(0), br::PipelineStageFlags2::RESOLVE)],
        ))
        .resolve_image(
            ms_color_buffer.image(),
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
        .inject(|r| {
            let (b, o) = init.staging_scratch_buffer.of(&label_image_pixels);

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
            &[br::ImageMemoryBarrier2::new(
                init.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .transit_from(br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt))
            .of_memory(
                br::AccessFlags2::TRANSFER.write,
                br::AccessFlags2::SHADER.read,
            )
            .of_execution(
                br::PipelineStageFlags2::RESOLVE,
                br::PipelineStageFlags2::FRAGMENT_SHADER,
            )],
        ))
        .end()
        .unwrap();
        init.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_bg_alpha_rate_shown = init
            .composite_tree
            .parameter_store_mut()
            .alloc_float(AnimatableFloat::Value(0.0));
        let ct_bg_alpha_rate_pointer = init
            .composite_tree
            .parameter_store_mut()
            .alloc_float(AnimatableFloat::Value(0.0));
        let ct_root = init.composite_tree.register(CompositeRect {
            offset: [left * init.ui_scale_factor, top * init.ui_scale_factor],
            size: [
                (Self::ICON_SIZE + Self::ICON_LABEL_GAP + Self::HPADDING * 2.0)
                    * init.ui_scale_factor
                    + label_layout.width(),
                Self::BUTTON_HEIGHT * init.ui_scale_factor,
            ],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: bg_atlas_rect,
            slice_borders: [Self::BUTTON_HEIGHT * 0.5 * init.ui_scale_factor; 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Expression(Box::new(
                move |ps| {
                    let opacity = ps.float_value(ct_bg_alpha_rate_shown) * 0.25
                        + ps.float_value(ct_bg_alpha_rate_pointer) * 0.25;

                    [1.0, 1.0, 1.0, opacity]
                },
            ))),
            ..Default::default()
        });
        let ct_icon = init.composite_tree.register(CompositeRect {
            size: [
                Self::ICON_SIZE * init.ui_scale_factor,
                Self::ICON_SIZE * init.ui_scale_factor,
            ],
            offset: [
                Self::HPADDING * init.ui_scale_factor,
                -Self::ICON_SIZE * 0.5 * init.ui_scale_factor,
            ],
            relative_offset_adjustment: [0.0, 0.5],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: icon_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value(
                Self::CONTENT_COLOR_HIDDEN,
            )),
            ..Default::default()
        });
        let ct_label = init.composite_tree.register(CompositeRect {
            size: [label_layout.width(), label_layout.height()],
            offset: [
                (Self::HPADDING + Self::ICON_SIZE + Self::ICON_LABEL_GAP) * init.ui_scale_factor,
                -label_layout.height() * 0.5,
            ],
            relative_offset_adjustment: [0.0, 0.5],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: label_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value(
                Self::CONTENT_COLOR_HIDDEN,
            )),
            ..Default::default()
        });

        init.composite_tree.add_child(ct_root, ct_icon);
        init.composite_tree.add_child(ct_root, ct_label);

        let ht_root = init.ht.create(HitTestTreeData {
            left,
            top,
            width: (Self::ICON_SIZE + Self::ICON_LABEL_GAP + Self::HPADDING * 2.0)
                + label_layout.width() / init.ui_scale_factor,
            height: Self::BUTTON_HEIGHT,
            ..Default::default()
        });

        Self {
            ct_root,
            ct_icon,
            ct_label,
            ct_bg_alpha_rate_shown,
            ct_bg_alpha_rate_pointer,
            ht_root,
            left,
            top,
            ui_scale_factor: init.ui_scale_factor,
            hovering: Cell::new(false),
            pressing: Cell::new(false),
            command_id,
        }
    }

    pub fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        ct: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
    ) {
        ct.add_child(ct_parent, self.ct_root);
        ht.add_child(ht_parent, self.ht_root);
    }

    pub fn show(&self, ct: &mut CompositeTree, current_sec: f32) {
        ct.parameter_store_mut().set_float(
            self.ct_bg_alpha_rate_shown,
            AnimatableFloat::Animated(
                0.0,
                AnimationData {
                    to_value: 1.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ),
        );
        ct.get_mut(self.ct_icon).composite_mode =
            CompositeMode::ColorTint(AnimatableColor::Animated(
                Self::CONTENT_COLOR_HIDDEN,
                AnimationData {
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    to_value: Self::CONTENT_COLOR_SHOWN,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ));
        ct.get_mut(self.ct_label).composite_mode =
            CompositeMode::ColorTint(AnimatableColor::Animated(
                Self::CONTENT_COLOR_HIDDEN,
                AnimationData {
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    to_value: Self::CONTENT_COLOR_SHOWN,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ));
        // TODO: ここでui_scale_factor適用するとui_scale_factorがかわったときにアニメーションが破綻するので別のところにおいたほうがよさそう(CompositeTreeで位置計算するときに適用する)
        ct.get_mut(self.ct_root).offset[0] = (self.left + 8.0) * self.ui_scale_factor;
        ct.get_mut(self.ct_root).animation_data_left = Some(AnimationData {
            start_sec: current_sec,
            end_sec: current_sec + 0.25,
            to_value: self.left * self.ui_scale_factor,
            curve_p1: (0.5, 0.5),
            curve_p2: (0.5, 1.0),
            event_on_complete: None,
        });

        ct.mark_dirty(self.ct_root);
        ct.mark_dirty(self.ct_icon);
        ct.mark_dirty(self.ct_label);
    }

    pub fn hide(&self, ct: &mut CompositeTree, current_sec: f32) {
        ct.parameter_store_mut().set_float(
            self.ct_bg_alpha_rate_shown,
            AnimatableFloat::Animated(
                1.0,
                AnimationData {
                    to_value: 0.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ),
        );
        ct.get_mut(self.ct_icon).composite_mode =
            CompositeMode::ColorTint(AnimatableColor::Animated(
                Self::CONTENT_COLOR_SHOWN,
                AnimationData {
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    to_value: Self::CONTENT_COLOR_HIDDEN,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ));
        ct.get_mut(self.ct_label).composite_mode =
            CompositeMode::ColorTint(AnimatableColor::Animated(
                Self::CONTENT_COLOR_SHOWN,
                AnimationData {
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    to_value: Self::CONTENT_COLOR_HIDDEN,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ));

        ct.mark_dirty(self.ct_icon);
        ct.mark_dirty(self.ct_label);
    }

    fn update_pointer_opacity_value_rate(&self, ct: &mut CompositeTree, current_sec: f32) {
        let current = ct
            .parameter_store()
            .evaluate_float(self.ct_bg_alpha_rate_pointer, current_sec);
        let target = match (self.hovering.get(), self.pressing.get()) {
            (true, true) => 1.0,
            (false, _) => 0.0,
            _ => 0.5,
        };

        ct.parameter_store_mut().set_float(
            self.ct_bg_alpha_rate_pointer,
            AnimatableFloat::Animated(
                current,
                AnimationData {
                    to_value: target,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.1,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ),
        );
    }

    pub fn on_pointer_enter(&self, ct: &mut CompositeTree, current_sec: f32) {
        self.hovering.set(true);
        self.update_pointer_opacity_value_rate(ct, current_sec);
    }

    pub fn on_pointer_leave(&self, ct: &mut CompositeTree, current_sec: f32) {
        // はなれた際はpressingもなかったことにする
        self.hovering.set(false);
        self.pressing.set(false);
        self.update_pointer_opacity_value_rate(ct, current_sec);
    }

    pub fn on_press(&self, ct: &mut CompositeTree, current_sec: f32) {
        self.pressing.set(true);
        self.update_pointer_opacity_value_rate(ct, current_sec);
    }

    pub fn on_release(&self, ct: &mut CompositeTree, current_sec: f32) {
        self.pressing.set(false);
        self.update_pointer_opacity_value_rate(ct, current_sec);
    }
}

struct AppMenuBaseView {
    ct_root: CompositeTreeRef,
    ht_root: HitTestTreeRef,
}
impl AppMenuBaseView {
    #[tracing::instrument(name = "AppMenuBaseView::new", skip(init))]
    pub fn new(init: &mut ViewInitContext) -> Self {
        let ct_root = init.composite_tree.register(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            composite_mode: CompositeMode::FillColor(AnimatableColor::Value([0.0, 0.0, 0.0, 0.0])),
            ..Default::default()
        });

        let ht_root = init.ht.create(HitTestTreeData {
            width_adjustment_factor: 1.0,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });

        Self { ct_root, ht_root }
    }

    pub fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        ct: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
    ) {
        ct.add_child(ct_parent, self.ct_root);
        ht.add_child(ht_parent, self.ht_root);
    }

    pub fn show(&self, ct: &mut CompositeTree, current_sec: f32) {
        ct.get_mut(self.ct_root).composite_mode = CompositeMode::FillColorBackdropBlur(
            AnimatableColor::Animated(
                [0.0, 0.0, 0.0, 0.0],
                AnimationData {
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    to_value: [0.0, 0.0, 0.0, 0.25],
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ),
            AnimatableFloat::Animated(
                0.0,
                AnimationData {
                    to_value: 3.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ),
        );
        ct.mark_dirty(self.ct_root);
    }

    pub fn hide(&self, ct: &mut CompositeTree, current_sec: f32) {
        ct.get_mut(self.ct_root).composite_mode = CompositeMode::FillColorBackdropBlur(
            AnimatableColor::Animated(
                [0.0, 0.0, 0.0, 0.25],
                AnimationData {
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    to_value: [0.0, 0.0, 0.0, 0.0],
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ),
            AnimatableFloat::Animated(
                3.0,
                AnimationData {
                    to_value: 0.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ),
        );
        ct.mark_dirty(self.ct_root);
    }
}

struct AppMenuActionHandler {
    base_view: Rc<AppMenuBaseView>,
    item_views: Vec<Rc<AppMenuButtonView>>,
    shown: Cell<bool>,
}
impl<'c> HitTestTreeActionHandler<'c> for AppMenuActionHandler {
    type Context = AppUpdateContext<'c>;

    fn hit_active(&self, _sender: HitTestTreeRef, _context: &Self::Context) -> bool {
        self.shown.get()
    }

    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        for v in self.item_views.iter() {
            if sender == v.ht_root {
                v.on_pointer_enter(
                    &mut context.for_view_feedback.composite_tree,
                    context.for_view_feedback.current_sec,
                );
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.base_view.ht_root {
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
        for v in self.item_views.iter() {
            if sender == v.ht_root {
                v.on_pointer_leave(
                    &mut context.for_view_feedback.composite_tree,
                    context.for_view_feedback.current_sec,
                );
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.base_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        for v in self.item_views.iter() {
            if sender == v.ht_root {
                v.on_press(
                    &mut context.for_view_feedback.composite_tree,
                    context.for_view_feedback.current_sec,
                );
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.base_view.ht_root {
            context
                .state
                .toggle_menu(&mut context.for_view_feedback, ht);

            return EventContinueControl::STOP_PROPAGATION
                | EventContinueControl::RECOMPUTE_POINTER_ENTER;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_move(
        &self,
        sender: HitTestTreeRef,
        _context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.base_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        for v in self.item_views.iter() {
            if sender == v.ht_root {
                v.on_release(
                    &mut context.for_view_feedback.composite_tree,
                    context.for_view_feedback.current_sec,
                );
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.base_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_click(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        for v in self.item_views.iter() {
            if sender == v.ht_root {
                // TODO: click action
                match v.command_id {
                    AppMenuCommandIdentifier::AddSprite => {
                        println!("Add Sprite");

                        let mut dbus =
                            dbus::Connection::connect_bus(dbus::BusType::Session).unwrap();
                        let mut reply = dbus
                            .send_with_reply(
                                &mut dbus::Message::new_method_call(
                                    Some(c"org.freedesktop.portal.Desktop"),
                                    c"/org/freedesktop/portal/desktop",
                                    Some(c"org.freedesktop.DBus.Introspectable"),
                                    c"Introspect",
                                )
                                .unwrap(),
                                None,
                            )
                            .unwrap();
                        // TODO: これUIだして待つべきか？ローカルだからあんまり待たないような気もするが......
                        reply.block();
                        let reply_msg = reply.steal_reply().unwrap();
                        let mut reply_iter = reply_msg.iter();
                        assert_eq!(reply_iter.arg_type(), b's' as _);
                        let mut sp = core::mem::MaybeUninit::<*const core::ffi::c_char>::uninit();
                        unsafe {
                            reply_iter.get_value_basic(sp.as_mut_ptr() as _);
                        }
                        let doc = unsafe {
                            core::ffi::CStr::from_ptr(sp.assume_init())
                                .to_str()
                                .unwrap()
                        };

                        let mut has_file_chooser = false;
                        if let Err(e) = dbus::introspect_document::read_toplevel(
                            &mut quick_xml::Reader::from_str(doc),
                            |_, ifname, r| {
                                has_file_chooser =
                                    ifname.as_ref() == b"org.freedesktop.portal.FileChooser";

                                dbus::introspect_document::skip_read_interface_tag_contents(r)
                            },
                        ) {
                            tracing::warn!(reason = ?e, "Failed to parse introspection document from portal object");
                        }

                        if !has_file_chooser {
                            context.event_queue.push(AppEvent::UIMessageDialogRequest {
                                content: String::from(
                                    "org.freedesktop.portal.FileChooser not found",
                                ),
                            });

                            return EventContinueControl::STOP_PROPAGATION;
                        }

                        println!("AddSprite: file chooser found!");
                    }
                    AppMenuCommandIdentifier::Save => {
                        println!("Save");
                    }
                }

                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.base_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }
}

pub struct AppMenuPresenter {
    base_view: Rc<AppMenuBaseView>,
    _action_handler: Rc<AppMenuActionHandler>,
}
impl AppMenuPresenter {
    pub fn new(init: &mut PresenterInitContext, header_height: f32) -> Self {
        let base_view = Rc::new(AppMenuBaseView::new(&mut init.for_view));
        let add_button = Rc::new(AppMenuButtonView::new(
            &mut init.for_view,
            "Add Sprite",
            "resources/icons/add.svg",
            64.0,
            header_height + 32.0,
            AppMenuCommandIdentifier::AddSprite,
        ));
        let save_button = Rc::new(AppMenuButtonView::new(
            &mut init.for_view,
            "Save",
            "resources/icons/save.svg",
            64.0,
            header_height + 32.0 + AppMenuButtonView::BUTTON_HEIGHT + 16.0,
            AppMenuCommandIdentifier::Save,
        ));

        add_button.mount(
            base_view.ct_root,
            init.for_view.composite_tree,
            base_view.ht_root,
            init.for_view.ht,
        );
        save_button.mount(
            base_view.ct_root,
            init.for_view.composite_tree,
            base_view.ht_root,
            init.for_view.ht,
        );

        let action_handler = Rc::new(AppMenuActionHandler {
            base_view: base_view.clone(),
            item_views: vec![add_button.clone(), save_button.clone()],
            shown: Cell::new(false),
        });

        init.app_state.register_visible_menu_view_feedback({
            let base_view = Rc::downgrade(&base_view);
            let action_handler = Rc::downgrade(&action_handler);
            let add_button = Rc::downgrade(&add_button);
            let save_button = Rc::downgrade(&save_button);

            move |update_context, _, visible| {
                let Some(base_view) = base_view.upgrade() else {
                    // app teardown-ed
                    return;
                };
                let Some(action_handler) = action_handler.upgrade() else {
                    // app teardown-ed
                    return;
                };
                let Some(add_button) = add_button.upgrade() else {
                    // app teardown-ed
                    return;
                };
                let Some(save_button) = save_button.upgrade() else {
                    // app teardown-ed
                    return;
                };

                if visible {
                    base_view.show(
                        &mut update_context.composite_tree,
                        update_context.current_sec,
                    );
                    add_button.show(
                        &mut update_context.composite_tree,
                        update_context.current_sec,
                    );
                    save_button.show(
                        &mut update_context.composite_tree,
                        update_context.current_sec + 0.05,
                    );
                } else {
                    base_view.hide(
                        &mut update_context.composite_tree,
                        update_context.current_sec,
                    );
                    add_button.hide(
                        &mut update_context.composite_tree,
                        update_context.current_sec,
                    );
                    save_button.hide(
                        &mut update_context.composite_tree,
                        update_context.current_sec,
                    );
                }

                action_handler.shown.set(visible);
            }
        });
        init.for_view
            .ht
            .set_action_handler(base_view.ht_root, &action_handler);
        init.for_view
            .ht
            .set_action_handler(add_button.ht_root, &action_handler);
        init.for_view
            .ht
            .set_action_handler(save_button.ht_root, &action_handler);

        Self {
            base_view,
            _action_handler: action_handler,
        }
    }

    pub fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        ct: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
    ) {
        self.base_view.mount(ct_parent, ct, ht_parent, ht);
    }
}

const POPUP_ANIMATION_DURATION: f32 = 0.2;
const POPUP_MASK_OPACITY: f32 = 0.125;
const POPUP_MASK_BLUR_POWER: f32 = 3.0;

struct PopupMaskView {
    ct_root: CompositeTreeRef,
    ht_root: HitTestTreeRef,
}
impl PopupMaskView {
    pub fn new(init: &mut ViewInitContext) -> Self {
        let ct_root = init.composite_tree.register(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            composite_mode: CompositeMode::FillColor(AnimatableColor::Value([0.0, 0.0, 0.0, 0.0])),
            ..Default::default()
        });

        let ht_root = init.ht.create(HitTestTreeData {
            width_adjustment_factor: 1.0,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });

        Self { ct_root, ht_root }
    }

    pub fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        ct: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
    ) {
        ct.add_child(ct_parent, self.ct_root);
        ht.add_child(ht_parent, self.ht_root);
    }

    pub fn unmount_ht(&self, ht: &mut HitTestTreeManager<AppUpdateContext<'_>>) {
        ht.remove_child(self.ht_root);
    }

    pub fn unmount_visual(&self, ct: &mut CompositeTree) {
        ct.remove_child(self.ct_root);
    }

    pub fn show(&self, ct: &mut CompositeTree, current_sec: f32) {
        ct.get_mut(self.ct_root).composite_mode = CompositeMode::FillColorBackdropBlur(
            AnimatableColor::Animated(
                [0.0, 0.0, 0.0, 0.0],
                AnimationData {
                    to_value: [0.0, 0.0, 0.0, POPUP_MASK_OPACITY],
                    start_sec: current_sec,
                    end_sec: current_sec + POPUP_ANIMATION_DURATION,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ),
            AnimatableFloat::Animated(
                0.0,
                AnimationData {
                    to_value: POPUP_MASK_BLUR_POWER,
                    start_sec: current_sec,
                    end_sec: current_sec + POPUP_ANIMATION_DURATION,
                    curve_p1: (0.25, 0.5),
                    curve_p2: (0.5, 1.0),
                    event_on_complete: None,
                },
            ),
        );

        ct.mark_dirty(self.ct_root);
    }

    pub fn hide(&self, ct: &mut CompositeTree, current_sec: f32, event_on_complete: AppEvent) {
        ct.get_mut(self.ct_root).composite_mode = CompositeMode::FillColorBackdropBlur(
            AnimatableColor::Animated(
                [0.0, 0.0, 0.0, POPUP_MASK_OPACITY],
                AnimationData {
                    to_value: [0.0, 0.0, 0.0, 0.0],
                    start_sec: current_sec,
                    end_sec: current_sec + POPUP_ANIMATION_DURATION,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: Some(event_on_complete),
                },
            ),
            AnimatableFloat::Animated(
                POPUP_MASK_BLUR_POWER,
                AnimationData {
                    to_value: 0.0,
                    start_sec: current_sec,
                    end_sec: current_sec + POPUP_ANIMATION_DURATION,
                    curve_p1: (0.5, 0.5),
                    curve_p2: (0.5, 0.5),
                    event_on_complete: None,
                },
            ),
        );

        ct.mark_dirty(self.ct_root);
    }
}

struct PopupCommonFrameView {
    ct_root: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    height: f32,
    ui_scale_factor: f32,
}
impl PopupCommonFrameView {
    const CORNER_RADIUS: f32 = 16.0;

    pub fn new(init: &mut ViewInitContext, width: f32, height: f32) -> Self {
        let render_size_px = ((Self::CORNER_RADIUS * 2.0 + 1.0) * init.ui_scale_factor) as u32;
        let frame_image_atlas_rect = init.atlas.alloc(render_size_px, render_size_px);
        let frame_border_image_atlas_rect = init.atlas.alloc(render_size_px, render_size_px);

        let render_pass = br::RenderPassObject::new(
            init.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(init.atlas.format())
                    .with_layout_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())
                    .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)],
                &[br::SubpassDescription2::new()
                    .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
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

        let [pipeline, pipeline_border] = init
            .subsystem
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    init.subsystem.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.subsystem
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
                            .require_shader("resources/rounded_rect.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &RoundedRectConstants {
                                    corner_radius: Self::CORNER_RADIUS,
                                },
                            )),
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
                    init.subsystem.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.subsystem
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
                            .require_shader("resources/rounded_rect_border.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &RoundedRectConstants {
                                    corner_radius: Self::CORNER_RADIUS,
                                },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[frame_border_image_atlas_rect
                            .vk_rect()
                            .make_viewport(0.0..1.0)],
                        &[frame_border_image_atlas_rect.vk_rect()],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        let mut cp = init
            .subsystem
            .create_transient_graphics_command_pool()
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
                &framebuffer,
                frame_border_image_atlas_rect.vk_rect(),
                &[br::ClearValue::color_f32([0.0; 4])],
            ),
            &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
        )
        .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline_border)
        .draw(3, 1, 0, 0)
        .end_render_pass2(&br::SubpassEndInfo::new())
        .end()
        .unwrap();

        init.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_root = init.composite_tree.register(CompositeRect {
            offset: [
                -width * 0.5 * init.ui_scale_factor,
                -height * 0.5 * init.ui_scale_factor,
            ],
            relative_offset_adjustment: [0.5, 0.5],
            size: [width * init.ui_scale_factor, height * init.ui_scale_factor],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: frame_image_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.0, 0.0, 0.0, 1.0])),
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });
        let ct_border = init.composite_tree.register(CompositeRect {
            offset: [
                -width * 0.5 * init.ui_scale_factor,
                -height * 0.5 * init.ui_scale_factor,
            ],
            relative_offset_adjustment: [0.5, 0.5],
            size: [width * init.ui_scale_factor, height * init.ui_scale_factor],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: frame_border_image_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([
                0.25, 0.25, 0.25, 1.0,
            ])),
            ..Default::default()
        });

        init.composite_tree.add_child(ct_root, ct_border);

        let ht_root = init.ht.create(HitTestTreeData {
            left: -width * 0.5,
            top: -height * 0.5,
            left_adjustment_factor: 0.5,
            top_adjustment_factor: 0.5,
            width,
            height,
            ..Default::default()
        });

        Self {
            ct_root,
            ht_root,
            height,
            ui_scale_factor: init.ui_scale_factor,
        }
    }

    pub fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        ct: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
    ) {
        ct.add_child(ct_parent, self.ct_root);
        ht.add_child(ht_parent, self.ht_root);
    }

    pub fn show(&self, ct: &mut CompositeTree, current_sec: f32) {
        ct.get_mut(self.ct_root).opacity = AnimatableFloat::Animated(
            0.0,
            AnimationData {
                to_value: 1.0,
                start_sec: current_sec,
                end_sec: current_sec + POPUP_ANIMATION_DURATION,
                curve_p1: (0.5, 0.5),
                curve_p2: (0.5, 0.5),
                event_on_complete: None,
            },
        );
        ct.get_mut(self.ct_root).offset[1] = (-0.5 * self.height + 8.0) * self.ui_scale_factor;
        ct.get_mut(self.ct_root).animation_data_top = Some(AnimationData {
            to_value: (-0.5 * self.height) * self.ui_scale_factor,
            start_sec: current_sec,
            end_sec: current_sec + POPUP_ANIMATION_DURATION,
            curve_p1: (0.25, 0.5),
            curve_p2: (0.5, 0.9),
            event_on_complete: None,
        });
        ct.get_mut(self.ct_root).scale_x = AnimatableFloat::Animated(
            0.9,
            AnimationData {
                to_value: 1.0,
                start_sec: current_sec,
                end_sec: current_sec + POPUP_ANIMATION_DURATION,
                curve_p1: (0.25, 0.5),
                curve_p2: (0.5, 0.9),
                event_on_complete: None,
            },
        );
        ct.get_mut(self.ct_root).scale_y = AnimatableFloat::Animated(
            0.9,
            AnimationData {
                to_value: 1.0,
                start_sec: current_sec,
                end_sec: current_sec + POPUP_ANIMATION_DURATION,
                curve_p1: (0.25, 0.5),
                curve_p2: (0.5, 0.9),
                event_on_complete: None,
            },
        );

        ct.mark_dirty(self.ct_root);
    }

    pub fn hide(&self, ct: &mut CompositeTree, current_sec: f32) {
        ct.get_mut(self.ct_root).opacity = AnimatableFloat::Animated(
            1.0,
            AnimationData {
                to_value: 0.0,
                start_sec: current_sec,
                end_sec: current_sec + POPUP_ANIMATION_DURATION,
                curve_p1: (0.5, 0.5),
                curve_p2: (0.5, 0.5),
                event_on_complete: None,
            },
        );
        ct.get_mut(self.ct_root).offset[1] = (-0.5 * self.height) * self.ui_scale_factor;
        ct.get_mut(self.ct_root).animation_data_top = Some(AnimationData {
            to_value: (-0.5 * self.height + 8.0) * self.ui_scale_factor,
            start_sec: current_sec,
            end_sec: current_sec + POPUP_ANIMATION_DURATION,
            curve_p1: (0.25, 0.5),
            curve_p2: (0.5, 0.9),
            event_on_complete: None,
        });
        ct.get_mut(self.ct_root).scale_x = AnimatableFloat::Animated(
            1.0,
            AnimationData {
                to_value: 0.9,
                start_sec: current_sec,
                end_sec: current_sec + POPUP_ANIMATION_DURATION,
                curve_p1: (0.25, 0.5),
                curve_p2: (0.5, 0.9),
                event_on_complete: None,
            },
        );
        ct.get_mut(self.ct_root).scale_y = AnimatableFloat::Animated(
            1.0,
            AnimationData {
                to_value: 0.9,
                start_sec: current_sec,
                end_sec: current_sec + POPUP_ANIMATION_DURATION,
                curve_p1: (0.25, 0.5),
                curve_p2: (0.5, 0.9),
                event_on_complete: None,
            },
        );

        ct.mark_dirty(self.ct_root);
    }
}

struct MessageDialogContentView {
    ct_root: CompositeTreeRef,
    preferred_width: f32,
    preferred_height: f32,
}
impl MessageDialogContentView {
    const FRAME_PADDING_H: f32 = 32.0;
    const FRAME_PADDING_V: f32 = 16.0;

    #[tracing::instrument(name = "MessageDialogContentView::new", skip(init))]
    pub fn new(init: &mut ViewInitContext, content: &str) -> Self {
        let text_layout = TextLayout::build_simple(content, &mut init.fonts.ui_default);
        let text_atlas_rect = init
            .atlas
            .alloc(text_layout.width_px(), text_layout.height_px());
        let text_image_pixels =
            text_layout.build_stg_image_pixel_buffer(&mut init.staging_scratch_buffer);

        let mut cp = init
            .subsystem
            .create_transient_graphics_command_pool()
            .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            init.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(
                &br::CommandBufferBeginInfo::new().onetime_submit(),
                init.subsystem,
            )
            .unwrap()
        }
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[br::ImageMemoryBarrier2::new(
                init.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
        ))
        .inject(|r| {
            let (b, o) = init.staging_scratch_buffer.of(&text_image_pixels);

            r.copy_buffer_to_image(
                b,
                init.atlas.resource().image(),
                br::ImageLayout::TransferDestOpt,
                &[br::vk::VkBufferImageCopy {
                    bufferOffset: o,
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
        })
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[br::ImageMemoryBarrier2::new(
                init.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .transit_from(br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt))
            .from(
                br::PipelineStageFlags2::COPY,
                br::AccessFlags2::TRANSFER.write,
            )
            .to(
                br::PipelineStageFlags2::FRAGMENT_SHADER,
                br::AccessFlags2::SHADER.read,
            )],
        ))
        .end()
        .unwrap();
        init.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_root = init.composite_tree.register(CompositeRect {
            size: [text_layout.width(), text_layout.height()],
            offset: [
                -text_layout.width() * 0.5,
                Self::FRAME_PADDING_V * init.ui_scale_factor,
            ],
            relative_offset_adjustment: [0.5, 0.0],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: text_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        Self {
            ct_root,
            preferred_width: Self::FRAME_PADDING_H * 2.0
                + text_layout.width() / init.ui_scale_factor,
            preferred_height: Self::FRAME_PADDING_V * 2.0
                + text_layout.height() / init.ui_scale_factor,
        }
    }

    pub fn mount(&self, ct_parent: CompositeTreeRef, ct: &mut CompositeTree) {
        ct.add_child(ct_parent, self.ct_root);
    }
}

struct MessageDialogActionHandler {
    mask_view: PopupMaskView,
    frame_view: PopupCommonFrameView,
    confirm_button: CommonButtonView,
    popup_id: uuid::Uuid,
}
impl<'c> HitTestTreeActionHandler<'c> for MessageDialogActionHandler {
    type Context = AppUpdateContext<'c>;

    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if self.confirm_button.is_sender(sender) {
            self.confirm_button.on_hover(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );

            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.frame_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.mask_view.ht_root {
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
        if self.confirm_button.is_sender(sender) {
            self.confirm_button.on_leave(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );

            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.frame_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.mask_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if self.confirm_button.is_sender(sender) {
            self.confirm_button.on_press(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );

            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.frame_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.mask_view.ht_root {
            context
                .event_queue
                .push(AppEvent::UIPopupClose { id: self.popup_id });
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if self.confirm_button.is_sender(sender) {
            self.confirm_button.on_release(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );

            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.frame_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.mask_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_click(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if self.confirm_button.is_sender(sender) {
            context
                .event_queue
                .push(AppEvent::UIPopupClose { id: self.popup_id });

            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.frame_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.mask_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }
}

pub struct MessageDialogPresenter {
    action_handler: Rc<MessageDialogActionHandler>,
}
impl MessageDialogPresenter {
    pub fn new(init: &mut PresenterInitContext, popup_id: uuid::Uuid, content: &str) -> Self {
        let content_view = MessageDialogContentView::new(&mut init.for_view, content);
        let confirm_button = CommonButtonView::new(&mut init.for_view, "OK");
        let frame_view = PopupCommonFrameView::new(
            &mut init.for_view,
            content_view.preferred_width,
            content_view.preferred_height + 4.0 + confirm_button.preferred_height(),
        );
        let mask_view = PopupMaskView::new(&mut init.for_view);

        frame_view.mount(
            mask_view.ct_root,
            init.for_view.composite_tree,
            mask_view.ht_root,
            init.for_view.ht,
        );
        content_view.mount(frame_view.ct_root, init.for_view.composite_tree);
        confirm_button.mount(
            frame_view.ct_root,
            init.for_view.composite_tree,
            frame_view.ht_root,
            init.for_view.ht,
        );

        {
            let confirm_button_ct = confirm_button.ct_mut(init.for_view.composite_tree);
            let confirm_button_ht = confirm_button.ht_mut(init.for_view.ht);

            confirm_button_ct.relative_offset_adjustment = [0.5, 0.0];
            confirm_button_ct.offset = [
                -0.5 * confirm_button_ct.size[0],
                (content_view.preferred_height - 4.0) * init.for_view.ui_scale_factor,
            ];
            confirm_button_ht.left_adjustment_factor = 0.5;
            confirm_button_ht.left = -0.5 * confirm_button_ht.width;
            confirm_button_ht.top = content_view.preferred_height - 4.0;
        }

        let action_handler = Rc::new(MessageDialogActionHandler {
            mask_view,
            frame_view,
            confirm_button,
            popup_id,
        });
        init.for_view
            .ht
            .set_action_handler(action_handler.mask_view.ht_root, &action_handler);
        init.for_view
            .ht
            .set_action_handler(action_handler.frame_view.ht_root, &action_handler);
        action_handler
            .confirm_button
            .bind_action_handler(&action_handler, init.for_view.ht);

        Self { action_handler }
    }

    pub fn show(
        &self,
        ct_parent: CompositeTreeRef,
        ct: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
        current_sec: f32,
    ) {
        self.action_handler
            .mask_view
            .mount(ct_parent, ct, ht_parent, ht);
        self.action_handler.mask_view.show(ct, current_sec);
        self.action_handler.frame_view.show(ct, current_sec);
    }

    pub fn hide(
        &self,
        ct: &mut CompositeTree,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
        current_sec: f32,
    ) {
        self.action_handler.mask_view.unmount_ht(ht);
        self.action_handler.mask_view.hide(
            ct,
            current_sec,
            AppEvent::UIPopupUnmount {
                id: self.action_handler.popup_id,
            },
        );
        self.action_handler.frame_view.hide(ct, current_sec);
    }

    pub fn unmount(&self, ct: &mut CompositeTree) {
        self.action_handler.mask_view.unmount_visual(ct);
    }
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
impl br::VkObject for SubsystemBoundSurface<'_> {
    const TYPE: br::vk::VkObjectType = br::vk::VkSurfaceKHR::OBJECT_TYPE;
}
impl br::InstanceChild for SubsystemBoundSurface<'_> {
    type ConcreteInstance = <Subsystem as br::InstanceChild>::ConcreteInstance;

    #[inline]
    fn instance(&self) -> &Self::ConcreteInstance {
        self.subsystem.instance()
    }
}
impl br::Surface for SubsystemBoundSurface<'_> {}

struct TemporalSwapchain<'s> {
    subsystem: &'s Subsystem,
    handle: br::vk::VkSwapchainKHR,
    size: br::Extent2D,
    format: br::Format,
}
impl Drop for TemporalSwapchain<'_> {
    fn drop(&mut self) {
        unsafe {
            br::vkfn_wrapper::destroy_swapchain(self.subsystem.native_ptr(), self.handle, None);
        }
    }
}
impl br::VkHandle for TemporalSwapchain<'_> {
    type Handle = br::vk::VkSwapchainKHR;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.handle
    }
}
impl br::VkObject for TemporalSwapchain<'_> {
    const TYPE: br::vk::VkObjectType = br::vk::VkSwapchainKHR::OBJECT_TYPE;
}
impl br::DeviceChildHandle for TemporalSwapchain<'_> {
    #[inline(always)]
    fn device_handle(&self) -> br::vk::VkDevice {
        self.subsystem.native_ptr()
    }
}
impl<'s> br::DeviceChild for TemporalSwapchain<'s> {
    type ConcreteDevice = &'s Subsystem;

    #[inline(always)]
    fn device(&self) -> &Self::ConcreteDevice {
        &self.subsystem
    }
}
impl br::Swapchain for TemporalSwapchain<'_> {
    fn size(&self) -> &br::Extent2D {
        &self.size
    }

    fn format(&self) -> br::Format {
        self.format
    }
}

pub struct PrimaryRenderTarget<'s> {
    subsystem: &'s Subsystem,
    surface: br::vk::VkSurfaceKHR,
    swapchain: br::vk::VkSwapchainKHR,
    backbuffers: Vec<br::vk::VkImage>,
    backbuffer_views: Vec<br::vk::VkImageView>,
    size: br::Extent2D,
    format: br::SurfaceFormat,
    transform: br::SurfaceTransformFlags,
    composite_alpha: br::CompositeAlphaFlags,
}
impl Drop for PrimaryRenderTarget<'_> {
    fn drop(&mut self) {
        unsafe {
            for x in self.backbuffer_views.drain(..) {
                br::vkfn_wrapper::destroy_image_view(self.subsystem.native_ptr(), x, None);
            }

            br::vkfn_wrapper::destroy_swapchain(self.subsystem.native_ptr(), self.swapchain, None);
            br::vkfn_wrapper::destroy_surface(
                self.subsystem.instance().native_ptr(),
                self.surface,
                None,
            );
        }
    }
}
impl br::VkHandle for PrimaryRenderTarget<'_> {
    type Handle = br::vk::VkSwapchainKHR;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.swapchain
    }
}
impl br::VkHandleMut for PrimaryRenderTarget<'_> {
    #[inline(always)]
    fn native_ptr_mut(&mut self) -> Self::Handle {
        self.swapchain
    }
}
impl br::DeviceChildHandle for PrimaryRenderTarget<'_> {
    #[inline(always)]
    fn device_handle(&self) -> bedrock::vk::VkDevice {
        self.subsystem.native_ptr()
    }
}
impl<'s> br::DeviceChild for PrimaryRenderTarget<'s> {
    type ConcreteDevice = &'s Subsystem;

    #[inline(always)]
    fn device(&self) -> &Self::ConcreteDevice {
        &self.subsystem
    }
}
impl br::Swapchain for PrimaryRenderTarget<'_> {
    #[inline(always)]
    fn size(&self) -> &br::Extent2D {
        &self.size
    }

    #[inline(always)]
    fn format(&self) -> br::Format {
        self.format.format
    }
}
impl<'s> PrimaryRenderTarget<'s> {
    fn new(surface: SubsystemBoundSurface<'s>) -> Self {
        let surface_caps = surface
            .subsystem
            .adapter()
            .surface_capabilities(&surface)
            .unwrap();
        let surface_formats = surface
            .subsystem
            .adapter()
            .surface_formats_alloc(&surface)
            .unwrap();
        let sc_transform = if surface_caps
            .supported_transforms()
            .has_any(br::SurfaceTransformFlags::IDENTITY)
        {
            br::SurfaceTransformFlags::IDENTITY
        } else {
            surface_caps.current_transform()
        };
        let sc_composite_alpha = if surface_caps
            .supported_composite_alpha()
            .has_any(br::CompositeAlphaFlags::OPAQUE)
        {
            br::CompositeAlphaFlags::OPAQUE
        } else {
            br::CompositeAlphaFlags::INHERIT
        };
        let sc_format = surface_formats
            .iter()
            .find(|x| {
                x.format == br::vk::VK_FORMAT_R8G8B8A8_UNORM
                    && x.colorSpace == br::vk::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR
            })
            .unwrap()
            .clone();
        let sc_size = br::Extent2D {
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

        let sc = TemporalSwapchain {
            handle: unsafe {
                br::vkfn_wrapper::create_swapchain(
                    surface.subsystem.native_ptr(),
                    &br::SwapchainCreateInfo::new(
                        &surface,
                        2,
                        sc_format,
                        sc_size,
                        br::ImageUsageFlags::COLOR_ATTACHMENT | br::ImageUsageFlags::TRANSFER_SRC,
                    )
                    .pre_transform(sc_transform)
                    .composite_alpha(sc_composite_alpha),
                    None,
                )
                .unwrap()
            },
            subsystem: surface.subsystem,
            size: sc_size,
            format: sc_format.format,
        };

        sc.set_name(Some(c"primary swapchain")).unwrap();

        let backbuffer_views = sc
            .images_alloc()
            .unwrap()
            .into_iter()
            .map(|bb| {
                br::ImageViewBuilder::new(
                    bb,
                    br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
                )
                .create()
                .unwrap()
            })
            .collect::<Vec<_>>();

        let (backbuffer_views, backbuffers): (Vec<_>, Vec<_>) = backbuffer_views
            .into_iter()
            .map(|x| {
                let (v, i) = x.unmanage();

                (v, i.native_ptr())
            })
            .unzip();
        let swapchain = unsafe { core::ptr::read(&sc.handle) };
        let subsystem = unsafe { core::ptr::read(&surface.subsystem) };
        let surface1 = unsafe { core::ptr::read(&surface.handle) };
        core::mem::forget(sc);
        core::mem::forget(surface);

        Self {
            subsystem,
            surface: surface1,
            swapchain,
            backbuffers,
            backbuffer_views,
            size: sc_size,
            format: sc_format,
            transform: sc_transform,
            composite_alpha: sc_composite_alpha,
        }
    }

    pub const fn color_format(&self) -> br::Format {
        self.format.format
    }

    #[inline(always)]
    pub fn backbuffer_count(&self) -> usize {
        self.backbuffers.len()
    }

    #[inline(always)]
    pub fn backbuffer_image<'x>(&'x self, index: usize) -> br::VkHandleRef<'x, br::vk::VkImage> {
        unsafe { br::VkHandleRef::dangling(self.backbuffers[index]) }
    }

    #[inline]
    pub fn backbuffer_views<'x>(
        &'x self,
    ) -> impl Iterator<Item = br::VkHandleRef<'x, br::vk::VkImageView>> + 'x {
        self.backbuffer_views
            .iter()
            .map(|&x| unsafe { br::VkHandleRef::dangling(x) })
    }

    pub fn resize(&mut self, new_size: br::Extent2D) {
        self.backbuffers.clear();
        unsafe {
            for x in self.backbuffer_views.drain(..) {
                br::vkfn_wrapper::destroy_image_view(self.subsystem.native_ptr(), x, None);
            }

            br::vkfn_wrapper::destroy_swapchain(self.subsystem.native_ptr(), self.swapchain, None);
        }

        self.swapchain = unsafe {
            br::vkfn_wrapper::create_swapchain(
                self.subsystem.native_ptr(),
                &br::SwapchainCreateInfo::new(
                    &br::VkHandleRef::dangling(self.surface),
                    2,
                    self.format,
                    new_size,
                    br::ImageUsageFlags::COLOR_ATTACHMENT | br::ImageUsageFlags::TRANSFER_SRC,
                )
                .pre_transform(self.transform)
                .composite_alpha(self.composite_alpha),
                None,
            )
            .unwrap()
        };

        let backbuffer_count = unsafe {
            br::vkfn_wrapper::get_swapchain_image_count(self.subsystem.native_ptr(), self.swapchain)
                .unwrap()
        };

        let mut buf = Vec::with_capacity(backbuffer_count as _);
        unsafe {
            buf.set_len(buf.capacity());
        }
        unsafe {
            br::vkfn_wrapper::get_swapchain_images(
                self.subsystem.native_ptr(),
                self.swapchain,
                &mut buf,
            )
            .unwrap();
        }

        if self.backbuffers.capacity() < backbuffer_count as usize {
            self.backbuffers
                .reserve(backbuffer_count as usize - self.backbuffers.capacity());
        }
        if self.backbuffer_views.capacity() < backbuffer_count as usize {
            self.backbuffer_views
                .reserve(backbuffer_count as usize - self.backbuffer_views.capacity());
        }
        for b in buf.into_iter() {
            self.backbuffers.push(b);
            self.backbuffer_views.push(unsafe {
                br::vkfn_wrapper::create_image_view(
                    self.subsystem.native_ptr(),
                    &br::ImageViewCreateInfo::new(
                        &br::VkHandleRef::dangling(b),
                        br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
                        br::vk::VK_IMAGE_VIEW_TYPE_2D,
                        self.format.format,
                    ),
                    None,
                )
                .unwrap()
            });
        }

        self.size = new_size;
    }
}

fn main() {
    tracing_subscriber::fmt()
        .pretty()
        .with_thread_names(true)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(%info, "application panic");
    }));

    tracing::info!("Initializing Peridot SpriteAtlas Visualizer/Editor");
    let setup_timer = std::time::Instant::now();

    let mut dbus = dbus::Connection::connect_bus(dbus::BusType::Session).unwrap();
    let mut pc = dbus
        .send_with_reply(
            &mut dbus::Message::new_method_call(
                Some(c"org.freedesktop.portal.Desktop"),
                c"/org/freedesktop/portal/desktop",
                Some(c"org.freedesktop.DBus.Introspectable"),
                c"Introspect",
            )
            .unwrap(),
            None,
        )
        .expect("no reply or out of memory");
    pc.block();
    let reply = pc.steal_reply().unwrap();
    assert_eq!(reply.r#type(), dbus::MESSAGE_TYPE_METHOD_RETURN);

    let mut reply_iter = reply.iter();
    assert_eq!(reply_iter.arg_type(), b's' as _);
    let mut strptr = core::mem::MaybeUninit::<*const core::ffi::c_char>::uninit();
    unsafe {
        reply_iter.get_value_basic(strptr.as_mut_ptr() as _);
    }
    let resp = unsafe {
        core::ffi::CStr::from_ptr(strptr.assume_init())
            .to_str()
            .unwrap()
    };
    println!("introspect return: {resp}");

    dbus::introspect_document::read_toplevel(
        &mut quick_xml::Reader::from_str(resp),
        |nodes, ifname, r| {
            let ifname =
                quick_xml::escape::unescape(unsafe { core::str::from_utf8_unchecked(&ifname) });
            println!("nodes: {nodes:?} interface: {ifname:?}");

            dbus::introspect_document::read_interface_tag_content(r, |e, r| match e {
                dbus::introspect_document::InterfaceElementContent::Method { name, empty } => {
                    let name = quick_xml::escape::unescape(unsafe {
                        core::str::from_utf8_unchecked(&name)
                    });
                    println!("    method: {name:?}");

                    if !empty {
                        dbus::introspect_document::read_method_tag_content(r, |e, _| match e {
                            dbus::introspect_document::MethodSignalElementContent::Arg {
                                name,
                                r#type,
                                direction,
                            } => {
                                let name = quick_xml::escape::unescape(unsafe {
                                    core::str::from_utf8_unchecked(&name)
                                });
                                let r#type = quick_xml::escape::unescape(unsafe {
                                    core::str::from_utf8_unchecked(&r#type)
                                });
                                let direction = direction.as_ref().map(|x| {
                                    quick_xml::escape::unescape(unsafe {
                                        core::str::from_utf8_unchecked(x)
                                    })
                                });
                                println!("      arg: {type:?} {name:?} ({direction:?})");

                                Ok(())
                            }
                            dbus::introspect_document::MethodSignalElementContent::Annotation {
                                name,
                                value,
                            } => {
                                let name = quick_xml::escape::unescape(unsafe {
                                    core::str::from_utf8_unchecked(&name)
                                });
                                let value = quick_xml::escape::unescape(unsafe {
                                    core::str::from_utf8_unchecked(&value)
                                });
                                println!("      annotation: {name:?} = {value:?}");

                                Ok(())
                            }
                        })
                    } else {
                        Ok(())
                    }
                }
                dbus::introspect_document::InterfaceElementContent::Signal { name, empty } => {
                    let name = quick_xml::escape::unescape(unsafe {
                        core::str::from_utf8_unchecked(&name)
                    });
                    println!("    signal: {name:?}");

                    if !empty {
                        dbus::introspect_document::read_signal_tag_content(r, |e, _| match e {
                            dbus::introspect_document::MethodSignalElementContent::Arg {
                                name,
                                r#type,
                                direction,
                            } => {
                                let name = quick_xml::escape::unescape(unsafe {
                                    core::str::from_utf8_unchecked(&name)
                                });
                                let r#type = quick_xml::escape::unescape(unsafe {
                                    core::str::from_utf8_unchecked(&r#type)
                                });
                                let direction = direction.as_deref().map(|x| {
                                    quick_xml::escape::unescape(unsafe {
                                        core::str::from_utf8_unchecked(x)
                                    })
                                });
                                println!("      arg: {type:?} {name:?} ({direction:?})");

                                Ok(())
                            }
                            dbus::introspect_document::MethodSignalElementContent::Annotation {
                                name,
                                value,
                            } => {
                                let name = quick_xml::escape::unescape(unsafe {
                                    core::str::from_utf8_unchecked(&name)
                                });
                                let value = quick_xml::escape::unescape(unsafe {
                                    core::str::from_utf8_unchecked(&value)
                                });
                                println!("      annotation: {name:?} = {value:?}");

                                Ok(())
                            }
                        })
                    } else {
                        Ok(())
                    }
                }
                dbus::introspect_document::InterfaceElementContent::Property {
                    name,
                    r#type,
                    access,
                } => {
                    let name = quick_xml::escape::unescape(unsafe {
                        core::str::from_utf8_unchecked(&name)
                    });
                    let r#type = quick_xml::escape::unescape(unsafe {
                        core::str::from_utf8_unchecked(&r#type)
                    });
                    let access = quick_xml::escape::unescape(unsafe {
                        core::str::from_utf8_unchecked(&access)
                    });
                    println!("    property: {type:?} {name:?} ({access:?})");

                    Ok(())
                }
            })
        },
    )
    .unwrap();

    assert_eq!(reply_iter.has_next(), false);

    let events = AppEventBus {
        queue: UnsafeCell::new(VecDeque::new()),
    };

    // initialize font systems
    fontconfig::init();
    let mut ft = FreeType::new().expect("Failed to initialize FreeType");
    let hinting = unsafe { ft.get_property::<u32>(c"cff", c"hinting-engine").unwrap() };
    let no_stem_darkening = unsafe {
        ft.get_property::<freetype2::FT_Bool>(c"cff", c"no-stem-darkening")
            .unwrap()
    };
    tracing::debug!(hinting, no_stem_darkening, "freetype cff properties");
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

    let mut app_state = AppState::new();

    let mut app_shell = shell::wayland::AppShell::new(&events);
    let client_size = Cell::new((640.0f32, 480.0));
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

    let mut sc = PrimaryRenderTarget::new(SubsystemBoundSurface {
        handle: unsafe {
            app_shell
                .create_vulkan_surface(subsystem.instance())
                .unwrap()
        },
        subsystem: &subsystem,
    });

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
    let Some((primary_face_path, primary_face_index)) = primary_face_info else {
        tracing::error!("No UI face found");
        std::process::exit(1);
    };

    let mut ft_face = match ft.new_face(&primary_face_path, primary_face_index as _) {
        Ok(x) => x,
        Err(e) => {
            tracing::error!(reason = ?e, "Failed to create ft face");
            std::process::exit(1);
        }
    };
    if let Err(e) = ft_face.set_char_size(
        (10.0 * 64.0) as _,
        0,
        (96.0 * app_shell.ui_scale_factor()) as _,
        0,
    ) {
        tracing::warn!(reason = ?e, "Failed to set char size");
    }

    let mut font_set = FontSet {
        ui_default: ft_face,
    };

    let mut composite_instance_buffer = CompositeInstanceManager::new(&subsystem);
    let mut composite_tree = CompositeTree::new();

    let mut composite_backdrop_buffers =
        Vec::<br::ImageViewObject<br::ImageObject<&Subsystem>>>::with_capacity(16);
    let mut composite_backdrop_buffer_memory = br::DeviceMemoryObject::new(
        &subsystem,
        &br::MemoryAllocateInfo::new(10, subsystem.find_device_local_memory_index(!0).unwrap()),
    )
    .unwrap();
    let mut composite_backdrop_blur_destination_fbs = Vec::with_capacity(16);
    let mut composite_backdrop_buffers_invalidated = true;

    let mut composite_grab_buffer = br::ImageObject::new(
        &subsystem,
        &br::ImageCreateInfo::new(sc.size, sc.color_format())
            .sampled()
            .transfer_dest(),
    )
    .unwrap();
    let req = composite_grab_buffer.requirements();
    let Some(memindex) = subsystem.find_device_local_memory_index(req.memoryTypeBits) else {
        tracing::error!(
            memory_index_mask = req.memoryTypeBits,
            "no suitable memory for composite grab buffer"
        );
        std::process::exit(1);
    };
    let mut composite_grab_buffer_memory =
        br::DeviceMemoryObject::new(&subsystem, &br::MemoryAllocateInfo::new(req.size, memindex))
            .unwrap();
    composite_grab_buffer
        .bind(&composite_grab_buffer_memory, 0)
        .unwrap();
    let mut composite_grab_buffer = br::ImageViewBuilder::new(
        composite_grab_buffer,
        br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
    )
    .create()
    .unwrap();

    let main_rp_grabbed = br::RenderPassObject::new(
        &subsystem,
        &br::RenderPassCreateInfo2::new(
            &[br::AttachmentDescription2::new(sc.color_format())
                .with_layout_to(br::ImageLayout::TransferSrcOpt.from_undefined())
                .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)],
            &[br::SubpassDescription2::new()
                .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
            &[br::SubpassDependency2::new(
                br::SubpassIndex::Internal(0),
                br::SubpassIndex::External,
            )
            .of_execution(
                br::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                br::PipelineStageFlags::TRANSFER,
            )
            .of_memory(
                br::AccessFlags::COLOR_ATTACHMENT.write,
                br::AccessFlags::TRANSFER.read,
            )],
        ),
    )
    .unwrap();
    main_rp_grabbed.set_name(Some(c"main_rp_grabbed")).unwrap();
    let main_rp_final = br::RenderPassObject::new(
        &subsystem,
        &br::RenderPassCreateInfo2::new(
            &[br::AttachmentDescription2::new(sc.color_format())
                .with_layout_to(br::ImageLayout::PresentSrc.from_undefined())
                .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)],
            &[br::SubpassDescription2::new()
                .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
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
    main_rp_final.set_name(Some(c"main_rp_final")).unwrap();
    let main_rp_continue_grabbed = br::RenderPassObject::new(
        &subsystem,
        &br::RenderPassCreateInfo2::new(
            &[br::AttachmentDescription2::new(sc.color_format())
                .with_layout_to(
                    br::ImageLayout::TransferSrcOpt.from(br::ImageLayout::TransferSrcOpt),
                )
                .color_memory_op(br::LoadOp::Load, br::StoreOp::Store)],
            &[br::SubpassDescription2::new()
                .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
            &[br::SubpassDependency2::new(
                br::SubpassIndex::Internal(0),
                br::SubpassIndex::External,
            )
            .of_execution(
                br::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                br::PipelineStageFlags::TRANSFER,
            )
            .of_memory(
                br::AccessFlags::COLOR_ATTACHMENT.write,
                br::AccessFlags::TRANSFER.read,
            )],
        ),
    )
    .unwrap();
    main_rp_continue_grabbed
        .set_name(Some(c"main_rp_continue_grabbed"))
        .unwrap();
    let main_rp_continue_final = br::RenderPassObject::new(
        &subsystem,
        &br::RenderPassCreateInfo2::new(
            &[br::AttachmentDescription2::new(sc.color_format())
                .with_layout_to(br::ImageLayout::PresentSrc.from(br::ImageLayout::TransferSrcOpt))
                .color_memory_op(br::LoadOp::Load, br::StoreOp::Store)],
            &[br::SubpassDescription2::new()
                .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
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
    main_rp_continue_final
        .set_name(Some(c"main_rp_continue_final"))
        .unwrap();
    let composite_backdrop_blur_rp = br::RenderPassObject::new(
        &subsystem,
        &br::RenderPassCreateInfo2::new(
            &[br::AttachmentDescription2::new(sc.color_format())
                .with_layout_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())
                .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)],
            &[br::SubpassDescription2::new()
                .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
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
    composite_backdrop_blur_rp
        .set_name(Some(c"composite_backdrop_blur_rp"))
        .unwrap();

    let mut main_grabbed_fbs = sc
        .backbuffer_views()
        .map(|bb| {
            br::FramebufferObject::new(
                &subsystem,
                &br::FramebufferCreateInfo::new(
                    &main_rp_grabbed,
                    &[bb.as_transparent_ref()],
                    sc.size.width,
                    sc.size.height,
                ),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let mut main_final_fbs = sc
        .backbuffer_views()
        .map(|bb| {
            br::FramebufferObject::new(
                &subsystem,
                &br::FramebufferCreateInfo::new(
                    &main_rp_final,
                    &[bb.as_transparent_ref()],
                    sc.size.width,
                    sc.size.height,
                ),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let mut main_continue_grabbed_fbs = sc
        .backbuffer_views()
        .map(|bb| {
            br::FramebufferObject::new(
                &subsystem,
                &br::FramebufferCreateInfo::new(
                    &main_rp_continue_grabbed,
                    &[bb.as_transparent_ref()],
                    sc.size.width,
                    sc.size.height,
                ),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let mut main_continue_final_fbs = sc
        .backbuffer_views()
        .map(|bb| {
            br::FramebufferObject::new(
                &subsystem,
                &br::FramebufferCreateInfo::new(
                    &main_rp_continue_final,
                    &[bb.as_transparent_ref()],
                    sc.size.width,
                    sc.size.height,
                ),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();

    const BLUR_SAMPLE_STEPS: usize = 2;
    let mut blur_temporal_buffers = Vec::with_capacity(BLUR_SAMPLE_STEPS);
    let mut resources_offsets = Vec::with_capacity(BLUR_SAMPLE_STEPS);
    let mut top = 0;
    let mut memory_index_mask = !0u32;
    for lv in 0..BLUR_SAMPLE_STEPS {
        let r = br::ImageObject::new(
            &subsystem,
            &br::ImageCreateInfo::new(
                br::Extent2D {
                    width: sc.size.width >> (lv + 1),
                    height: sc.size.height >> (lv + 1),
                },
                sc.color_format(),
            )
            .sampled()
            .as_color_attachment(),
        )
        .unwrap();
        let req = r.requirements();
        assert!(req.alignment.is_power_of_two());
        let offset = (top + req.alignment - 1) & !(req.alignment - 1);

        top = offset + req.size;
        memory_index_mask &= req.memoryTypeBits;
        resources_offsets.push((r, offset));
    }
    let mut blur_temporal_buffer_memory = br::DeviceMemoryObject::new(
        &subsystem,
        &br::MemoryAllocateInfo::new(
            top,
            subsystem
                .find_device_local_memory_index(memory_index_mask)
                .unwrap(),
        ),
    )
    .unwrap();
    for (mut r, o) in resources_offsets {
        r.bind(&blur_temporal_buffer_memory, o as _).unwrap();

        blur_temporal_buffers.push(
            br::ImageViewBuilder::new(
                r,
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .create()
            .unwrap(),
        );
    }
    let mut blur_downsample_pass_fbs = blur_temporal_buffers
        .iter()
        .enumerate()
        .map(|(lv, b)| {
            br::FramebufferObject::new(
                &subsystem,
                &br::FramebufferCreateInfo::new(
                    &composite_backdrop_blur_rp,
                    &[b.as_transparent_ref()],
                    sc.size.width >> (lv + 1),
                    sc.size.height >> (lv + 1),
                ),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let mut blur_upsample_pass_fixed_fbs = blur_temporal_buffers
        .iter()
        .take(blur_temporal_buffers.len() - 1)
        .enumerate()
        .map(|(lv, b)| {
            br::FramebufferObject::new(
                &subsystem,
                &br::FramebufferCreateInfo::new(
                    &composite_backdrop_blur_rp,
                    &[b.as_transparent_ref()],
                    sc.size.width >> (lv + 1),
                    sc.size.height >> (lv + 1),
                ),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();

    let composite_sampler =
        br::SamplerObject::new(&subsystem, &br::SamplerCreateInfo::new()).unwrap();

    let composite_vsh = subsystem.require_shader("resources/composite.vert");
    let composite_fsh = subsystem.require_shader("resources/composite.frag");
    let composite_shader_stages = [
        composite_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
        composite_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
    ];
    let composite_backdrop_blur_downsample_vsh =
        subsystem.require_shader("resources/dual_kawase_filter/downsample.vert");
    let composite_backdrop_blur_downsample_fsh =
        subsystem.require_shader("resources/dual_kawase_filter/downsample.frag");
    let composite_backdrop_blur_upsample_vsh =
        subsystem.require_shader("resources/dual_kawase_filter/upsample.vert");
    let composite_backdrop_blur_upsample_fsh =
        subsystem.require_shader("resources/dual_kawase_filter/upsample.frag");
    let composite_backdrop_blur_downsample_stages = [
        composite_backdrop_blur_downsample_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
        composite_backdrop_blur_downsample_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
    ];
    let composite_backdrop_blur_upsample_stages = [
        composite_backdrop_blur_upsample_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
        composite_backdrop_blur_upsample_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
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
            br::DescriptorType::CombinedImageSampler
                .make_binding(3, 1)
                .only_for_fragment(),
        ]),
    )
    .unwrap();
    let composite_backdrop_descriptor_layout = br::DescriptorSetLayoutObject::new(
        &subsystem,
        &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
            .make_binding(0, 1)
            .only_for_fragment()]),
    )
    .unwrap();
    let composite_backdrop_blur_input_descriptor_layout = br::DescriptorSetLayoutObject::new(
        &subsystem,
        &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
            .make_binding(0, 1)
            .only_for_fragment()]),
    )
    .unwrap();
    let mut descriptor_pool = br::DescriptorPoolObject::new(
        &subsystem,
        &br::DescriptorPoolCreateInfo::new(
            (1 + (BLUR_SAMPLE_STEPS + 1)) as _,
            &[
                br::DescriptorType::CombinedImageSampler
                    .make_size((1 + (BLUR_SAMPLE_STEPS + 1)) as _),
                br::DescriptorType::UniformBuffer.make_size(1),
                br::DescriptorType::StorageBuffer.make_size(1),
            ],
        ),
    )
    .unwrap();
    let [composite_alphamask_group_descriptor] = descriptor_pool
        .alloc_array(&[composite_descriptor_layout.as_transparent_ref()])
        .unwrap();
    let blur_fixed_descriptors = descriptor_pool
        .alloc(
            &core::iter::repeat_n(
                composite_backdrop_blur_input_descriptor_layout.as_transparent_ref(),
                BLUR_SAMPLE_STEPS + 1,
            )
            .collect::<Vec<_>>(),
        )
        .unwrap();
    let mut descriptor_writes = vec![
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
        blur_fixed_descriptors[0].binding_at(0).write(
            br::DescriptorContents::CombinedImageSampler(vec![
                br::DescriptorImageInfo::new(
                    &composite_grab_buffer,
                    br::ImageLayout::ShaderReadOnlyOpt,
                )
                .with_sampler(&composite_sampler),
            ]),
        ),
    ];
    descriptor_writes.extend((0..BLUR_SAMPLE_STEPS).map(|n| {
        blur_fixed_descriptors[n + 1].binding_at(0).write(
            br::DescriptorContents::CombinedImageSampler(vec![
                br::DescriptorImageInfo::new(
                    &blur_temporal_buffers[n],
                    br::ImageLayout::ShaderReadOnlyOpt,
                )
                .with_sampler(&composite_sampler),
            ]),
        )
    }));
    subsystem.update_descriptor_sets(&descriptor_writes, &[]);

    let mut composite_backdrop_buffer_descriptor_pool = br::DescriptorPoolObject::new(
        &subsystem,
        &br::DescriptorPoolCreateInfo::new(
            16,
            &[br::DescriptorType::CombinedImageSampler.make_size(16)],
        ),
    )
    .unwrap();
    let mut composite_backdrop_buffer_descriptor_sets = Vec::<br::DescriptorSet>::with_capacity(16);
    let mut composite_backdrop_buffer_descriptor_pool_capacity = 16;

    let composite_pipeline_layout = br::PipelineLayoutObject::new(
        &subsystem,
        &br::PipelineLayoutCreateInfo::new(
            &[
                composite_descriptor_layout.as_transparent_ref(),
                composite_backdrop_descriptor_layout.as_transparent_ref(),
            ],
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
    let composite_blend_state = br::PipelineColorBlendStateCreateInfo::new(&[
        br::vk::VkPipelineColorBlendAttachmentState::PREMULTIPLIED,
    ]);

    let blur_pipeline_layout = br::PipelineLayoutObject::new(
        &subsystem,
        &br::PipelineLayoutCreateInfo::new(
            &[composite_backdrop_blur_input_descriptor_layout.as_transparent_ref()],
            &[br::PushConstantRange::for_type::<[f32; 3]>(
                br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                0,
            )],
        ),
    )
    .unwrap();

    let [
        mut composite_pipeline_grabbed,
        mut composite_pipeline_final,
        mut composite_pipeline_continue_grabbed,
        mut composite_pipeline_continue_final,
    ] = subsystem
        .create_graphics_pipelines_array(&[
            br::GraphicsPipelineCreateInfo::new(
                &composite_pipeline_layout,
                main_rp_grabbed.subpass(0),
                &composite_shader_stages,
                &composite_vinput,
                &composite_ia_state,
                &br::PipelineViewportStateCreateInfo::new(
                    &[sc.size
                        .into_rect(br::Offset2D::ZERO)
                        .make_viewport(0.0..1.0)],
                    &[sc.size.into_rect(br::Offset2D::ZERO)],
                ),
                &composite_raster_state,
                &composite_blend_state,
            )
            .multisample_state(MS_STATE_EMPTY),
            br::GraphicsPipelineCreateInfo::new(
                &composite_pipeline_layout,
                main_rp_final.subpass(0),
                &composite_shader_stages,
                &composite_vinput,
                &composite_ia_state,
                &br::PipelineViewportStateCreateInfo::new(
                    &[sc.size
                        .into_rect(br::Offset2D::ZERO)
                        .make_viewport(0.0..1.0)],
                    &[sc.size.into_rect(br::Offset2D::ZERO)],
                ),
                &composite_raster_state,
                &composite_blend_state,
            )
            .multisample_state(MS_STATE_EMPTY),
            br::GraphicsPipelineCreateInfo::new(
                &composite_pipeline_layout,
                main_rp_continue_grabbed.subpass(0),
                &composite_shader_stages,
                &composite_vinput,
                &composite_ia_state,
                &br::PipelineViewportStateCreateInfo::new(
                    &[sc.size
                        .into_rect(br::Offset2D::ZERO)
                        .make_viewport(0.0..1.0)],
                    &[sc.size.into_rect(br::Offset2D::ZERO)],
                ),
                &composite_raster_state,
                &composite_blend_state,
            )
            .multisample_state(MS_STATE_EMPTY),
            br::GraphicsPipelineCreateInfo::new(
                &composite_pipeline_layout,
                main_rp_continue_final.subpass(0),
                &composite_shader_stages,
                &composite_vinput,
                &composite_ia_state,
                &br::PipelineViewportStateCreateInfo::new(
                    &[sc.size
                        .into_rect(br::Offset2D::ZERO)
                        .make_viewport(0.0..1.0)],
                    &[sc.size.into_rect(br::Offset2D::ZERO)],
                ),
                &composite_raster_state,
                &composite_blend_state,
            )
            .multisample_state(MS_STATE_EMPTY),
        ])
        .unwrap();
    let blur_sample_viewport_scissors = (0..BLUR_SAMPLE_STEPS + 1)
        .map(|lv| {
            let size = br::Extent2D {
                width: sc.size.width >> lv,
                height: sc.size.height >> lv,
            };

            (
                [size.into_rect(br::Offset2D::ZERO).make_viewport(0.0..1.0)],
                [size.into_rect(br::Offset2D::ZERO)],
            )
        })
        .collect::<Vec<_>>();
    let blur_sample_viewport_states = blur_sample_viewport_scissors
        .iter()
        .map(|(vp, sc)| br::PipelineViewportStateCreateInfo::new(vp, sc))
        .collect::<Vec<_>>();
    let mut blur_downsample_pipelines = subsystem
        .create_graphics_pipelines(
            &blur_sample_viewport_states
                .iter()
                .skip(1)
                .map(|vp_state| {
                    br::GraphicsPipelineCreateInfo::new(
                        &blur_pipeline_layout,
                        composite_backdrop_blur_rp.subpass(0),
                        &composite_backdrop_blur_downsample_stages,
                        VI_STATE_EMPTY,
                        IA_STATE_TRILIST,
                        vp_state,
                        RASTER_STATE_DEFAULT_FILL_NOCULL,
                        BLEND_STATE_SINGLE_NONE,
                    )
                    .multisample_state(MS_STATE_EMPTY)
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let mut blur_upsample_pipelines = subsystem
        .create_graphics_pipelines(
            &blur_sample_viewport_states
                .iter()
                .take(blur_sample_viewport_states.len() - 1)
                .map(|vp_state| {
                    br::GraphicsPipelineCreateInfo::new(
                        &blur_pipeline_layout,
                        composite_backdrop_blur_rp.subpass(0),
                        &composite_backdrop_blur_upsample_stages,
                        VI_STATE_EMPTY,
                        IA_STATE_TRILIST,
                        vp_state,
                        RASTER_STATE_DEFAULT_FILL_NOCULL,
                        BLEND_STATE_SINGLE_NONE,
                    )
                    .multisample_state(MS_STATE_EMPTY)
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();

    let mut init_context = PresenterInitContext {
        for_view: ViewInitContext {
            subsystem: &subsystem,
            staging_scratch_buffer: &mut staging_scratch_buffer,
            atlas: &mut composition_alphamask_surface_atlas,
            ui_scale_factor: app_shell.ui_scale_factor(),
            fonts: &mut font_set,
            composite_tree: &mut composite_tree,
            composite_instance_manager: &mut composite_instance_buffer,
            ht: &mut ht_manager,
        },
        app_state: &mut app_state,
    };

    let editing_atlas_renderer = Rc::new(RefCell::new(EditingAtlasRenderer::new(
        &init_context.for_view.subsystem,
        main_rp_final.subpass(0),
        sc.size,
        SizePixels {
            width: 32,
            height: 32,
        },
    )));
    let mut editing_atlas_current_bound_pipeline = RenderPassType::Final;
    init_context.app_state.register_atlas_size_view_feedback({
        let editing_atlas_renderer = Rc::downgrade(&editing_atlas_renderer);

        move |size| {
            let Some(editing_atlas_renderer) = editing_atlas_renderer.upgrade() else {
                // app teardown-ed
                return;
            };

            editing_atlas_renderer.borrow_mut().set_atlas_size(*size);
        }
    });

    let app_header = feature::app_header::Presenter::new(&mut init_context);
    let sprite_list_pane = SpriteListPanePresenter::new(&mut init_context, app_header.height());
    let app_menu = AppMenuPresenter::new(&mut init_context, app_header.height());

    sprite_list_pane.mount(
        &mut composite_tree,
        CompositeTree::ROOT,
        &mut ht_manager,
        ht_root,
    );
    app_menu.mount(
        CompositeTree::ROOT,
        &mut composite_tree,
        ht_root,
        &mut ht_manager,
    );
    app_header.mount(
        CompositeTree::ROOT,
        &mut composite_tree,
        ht_root,
        &mut ht_manager,
    );

    editing_atlas_renderer
        .borrow_mut()
        .set_offset(0.0, app_header.height() * app_shell.ui_scale_factor());

    tracing::debug!(
        byte_size = staging_scratch_buffer.total_reserved_amount(),
        "Reserved Staging Buffers during UI initialization",
    );
    staging_scratch_buffer.reset();
    ht_manager.dump(ht_root);

    let mut main_cp = br::CommandPoolObject::new(
        &subsystem,
        &br::CommandPoolCreateInfo::new(subsystem.graphics_queue_family_index),
    )
    .unwrap();
    let mut main_cbs = br::CommandBufferObject::alloc(
        &subsystem,
        &br::CommandBufferAllocateInfo::new(
            &mut main_cp,
            sc.backbuffer_count() as _,
            br::CommandBufferLevel::Primary,
        ),
    )
    .unwrap();
    let mut main_cb_invalid = true;

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
    acquire_completion
        .set_name(Some(c"acquire_completion"))
        .unwrap();
    let render_completion =
        br::SemaphoreObject::new(&subsystem, &br::SemaphoreCreateInfo::new()).unwrap();
    render_completion
        .set_name(Some(c"render_completion"))
        .unwrap();
    let mut last_render_command_fence =
        br::FenceObject::new(&subsystem, &br::FenceCreateInfo::new(0)).unwrap();
    last_render_command_fence
        .set_name(Some(c"last_render_command_fence"))
        .unwrap();
    let mut last_rendering = false;
    let mut last_update_command_fence =
        br::FenceObject::new(&subsystem, &br::FenceCreateInfo::new(0)).unwrap();
    last_update_command_fence
        .set_name(Some(c"last_update_command_fence"))
        .unwrap();
    let mut last_updating = false;

    let mut app_update_context = AppUpdateContext {
        for_view_feedback: ViewFeedbackContext {
            composite_tree,
            current_sec: 0.0,
        },
        state: app_state,
        editing_atlas_renderer,
        event_queue: &events,
        dbus: &dbus,
    };
    app_update_context
        .state
        .synchronize_view(&mut app_update_context.for_view_feedback, &mut ht_manager);

    app_shell.flush();

    let elapsed = setup_timer.elapsed();
    tracing::info!(?elapsed, "App Setup done!");

    // initial post event
    app_update_context
        .event_queue
        .push(AppEvent::ToplevelWindowFrameTiming);

    let t = std::time::Instant::now();
    let mut frame_resize_request = None;
    let mut last_pointer_pos = (0.0f32, 0.0f32);
    let mut last_composite_render_instructions = CompositeRenderingData {
        instructions: Vec::new(),
        render_pass_types: Vec::new(),
        required_backdrop_buffer_count: 0,
    };
    let mut composite_instance_buffer_dirty = false;
    let mut popups = HashMap::<uuid::Uuid, MessageDialogPresenter>::new();
    'app: loop {
        app_shell.process_pending_events();
        while let Some(e) = app_update_context.event_queue.pop() {
            match e {
                AppEvent::ToplevelWindowClose => break 'app,
                AppEvent::ToplevelWindowFrameTiming => {
                    let current_t = t.elapsed();

                    if last_rendering {
                        last_render_command_fence.wait().unwrap();
                        last_render_command_fence.reset().unwrap();
                        last_rendering = false;
                    }

                    {
                        // もろもろの判定がめんどいのでいったん毎回updateする
                        let n = composite_instance_buffer.memory_stg().native_ptr();
                        let r = composite_instance_buffer.range_all();
                        let flush_required =
                            composite_instance_buffer.memory_stg_requires_explicit_flush();
                        let ptr = composite_instance_buffer
                            .memory_stg_exc()
                            .map(r.clone())
                            .unwrap();
                        let composite_render_instructions = unsafe {
                            app_update_context.for_view_feedback.composite_tree.update(
                                sc.size,
                                current_t.as_secs_f32(),
                                composition_alphamask_surface_atlas.vk_extent(),
                                &ptr,
                                &app_update_context.event_queue,
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

                        if last_composite_render_instructions != composite_render_instructions {
                            // needs update render commands
                            if !main_cb_invalid {
                                unsafe {
                                    main_cp.reset(br::CommandPoolResetFlags::EMPTY).unwrap();
                                }

                                main_cb_invalid = true;
                            }

                            if composite_render_instructions.required_backdrop_buffer_count
                                > composite_backdrop_buffer_descriptor_pool_capacity
                            {
                                // resize pool
                                composite_backdrop_buffer_descriptor_pool =
                                    br::DescriptorPoolObject::new(
                                        &subsystem,
                                        &br::DescriptorPoolCreateInfo::new(
                                            composite_render_instructions
                                                .required_backdrop_buffer_count
                                                .max(1)
                                                as _,
                                            &[br::DescriptorType::CombinedImageSampler.make_size(
                                                composite_render_instructions
                                                    .required_backdrop_buffer_count
                                                    .max(1)
                                                    as _,
                                            )],
                                        ),
                                    )
                                    .unwrap();
                                composite_backdrop_buffer_descriptor_pool_capacity =
                                    composite_render_instructions.required_backdrop_buffer_count;
                                composite_backdrop_buffer_descriptor_sets.reserve(
                                    composite_render_instructions.required_backdrop_buffer_count
                                        - composite_backdrop_buffer_descriptor_sets.len(),
                                );
                            } else {
                                // just reset
                                unsafe {
                                    composite_backdrop_buffer_descriptor_pool.reset(0).unwrap();
                                }
                            }
                            composite_backdrop_buffer_descriptor_sets.clear();
                            composite_backdrop_buffer_descriptor_sets.extend(
                                composite_backdrop_buffer_descriptor_pool
                                    .alloc(
                                        &core::iter::repeat(
                                            composite_backdrop_descriptor_layout
                                                .as_transparent_ref(),
                                        )
                                        .take(
                                            composite_render_instructions
                                                .required_backdrop_buffer_count
                                                .max(1),
                                        )
                                        .collect::<Vec<_>>(),
                                    )
                                    .unwrap(),
                            );
                            composite_backdrop_buffers_invalidated = true;

                            last_composite_render_instructions = composite_render_instructions;
                        }

                        composite_instance_buffer_dirty = true;
                    }

                    let composite_instance_buffer_dirty =
                        core::mem::replace(&mut composite_instance_buffer_dirty, false);
                    let mut needs_update = composite_instance_buffer_dirty
                        || app_update_context
                            .editing_atlas_renderer
                            .borrow()
                            .is_dirty();

                    if composite_backdrop_buffers_invalidated {
                        composite_backdrop_buffers_invalidated = false;

                        composite_backdrop_blur_destination_fbs.clear();
                        composite_backdrop_buffers.clear();
                        drop(composite_backdrop_buffer_memory);
                        let mut image_objects =
                            Vec::with_capacity(composite_backdrop_buffers.len());
                        let mut offsets = Vec::with_capacity(composite_backdrop_buffers.len());
                        let mut top = 0u64;
                        let mut memory_index_mask = !0u32;
                        for _ in 0..last_composite_render_instructions
                            .required_backdrop_buffer_count
                            .max(1)
                        {
                            let image = br::ImageObject::new(
                                &subsystem,
                                &br::ImageCreateInfo::new(sc.size, sc.color_format())
                                    .sampled()
                                    .as_color_attachment()
                                    .transfer_dest(),
                            )
                            .unwrap();
                            let req = image.requirements();
                            assert!(req.alignment.is_power_of_two());
                            let offset = (top + req.alignment - 1) & !(req.alignment - 1);
                            top = offset + req.size;
                            memory_index_mask &= req.memoryTypeBits;

                            offsets.push(offset);
                            image_objects.push(image);
                        }
                        let Some(memindex) =
                            subsystem.find_device_local_memory_index(memory_index_mask)
                        else {
                            tracing::error!(
                                memory_index_mask,
                                "no suitable memory for composition backdrop buffers"
                            );
                            std::process::exit(1);
                        };
                        composite_backdrop_buffer_memory = br::DeviceMemoryObject::new(
                            &subsystem,
                            &br::MemoryAllocateInfo::new(top.max(64), memindex),
                        )
                        .unwrap();
                        for (mut r, o) in image_objects.into_iter().zip(offsets.into_iter()) {
                            r.bind(&composite_backdrop_buffer_memory, o as _).unwrap();

                            composite_backdrop_buffers.push(
                                br::ImageViewBuilder::new(
                                    r,
                                    br::ImageSubresourceRange::new(
                                        br::AspectMask::COLOR,
                                        0..1,
                                        0..1,
                                    ),
                                )
                                .create()
                                .unwrap(),
                            );
                        }

                        composite_backdrop_blur_destination_fbs.extend(
                            composite_backdrop_buffers.iter().map(|b| {
                                br::FramebufferObject::new(
                                    &subsystem,
                                    &br::FramebufferCreateInfo::new(
                                        &composite_backdrop_blur_rp,
                                        &[b.as_transparent_ref()],
                                        sc.size.width,
                                        sc.size.height,
                                    ),
                                )
                                .unwrap()
                            }),
                        );

                        subsystem.update_descriptor_sets(
                            &composite_backdrop_buffers
                                .iter()
                                .zip(composite_backdrop_buffer_descriptor_sets.iter())
                                .map(|(v, d)| {
                                    d.binding_at(0).write(
                                        br::DescriptorContents::CombinedImageSampler(vec![
                                            br::DescriptorImageInfo::new(
                                                v,
                                                br::ImageLayout::ShaderReadOnlyOpt,
                                            )
                                            .with_sampler(&composite_sampler),
                                        ]),
                                    )
                                })
                                .collect::<Vec<_>>(),
                            &[],
                        );

                        needs_update = true;
                    }

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
                            &[br::ImageMemoryBarrier2::new(
                                composite_backdrop_buffers[0].image(),
                                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
                            )
                            .transit_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())],
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

                    if main_cb_invalid {
                        if last_composite_render_instructions.render_pass_types[0]
                            != editing_atlas_current_bound_pipeline
                        {
                            editing_atlas_current_bound_pipeline =
                                last_composite_render_instructions.render_pass_types[0];
                            app_update_context
                                .editing_atlas_renderer
                                .borrow_mut()
                                .recreate(
                                    &subsystem,
                                    match editing_atlas_current_bound_pipeline {
                                        RenderPassType::Grabbed => main_rp_grabbed.subpass(0),
                                        RenderPassType::Final => main_rp_final.subpass(0),
                                        RenderPassType::ContinueGrabbed => {
                                            main_rp_continue_grabbed.subpass(0)
                                        }
                                        RenderPassType::ContinueFinal => {
                                            main_rp_continue_final.subpass(0)
                                        }
                                    },
                                    sc.size,
                                );
                        }

                        for (n, cb) in main_cbs.iter_mut().enumerate() {
                            let (first_rp, first_fb) = match last_composite_render_instructions
                                .render_pass_types[0]
                            {
                                RenderPassType::Grabbed => (&main_rp_grabbed, &main_grabbed_fbs[n]),
                                RenderPassType::Final => (&main_rp_final, &main_final_fbs[n]),
                                _ => unreachable!("cannot continue at first"),
                            };

                            unsafe {
                                cb.begin(&br::CommandBufferBeginInfo::new(), &subsystem)
                                    .unwrap()
                            }
                            .begin_render_pass2(
                                &br::RenderPassBeginInfo::new(
                                    first_rp,
                                    first_fb,
                                    sc.size.into_rect(br::Offset2D::ZERO),
                                    &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 1.0])],
                                ),
                                &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                            )
                            .inject(|r| {
                                app_update_context
                                    .editing_atlas_renderer
                                    .borrow()
                                    .render_commands(sc.size, r)
                            })
                            .inject(|mut r| {
                                let mut rpt_pointer = 0;
                                let mut in_render_pass = true;
                                let mut pipeline_bound = false;

                                for x in last_composite_render_instructions.instructions.iter() {
                                    match x {
                                        &CompositeRenderingInstruction::DrawInstanceRange {
                                            ref index_range,
                                            backdrop_buffer,
                                        } => {
                                            if !in_render_pass {
                                                in_render_pass = true;

                                                let (rp, fb) = match last_composite_render_instructions.render_pass_types[rpt_pointer] {
                                                    RenderPassType::ContinueGrabbed => {
                                                        (&main_rp_continue_grabbed, &main_continue_grabbed_fbs[n])
                                                    }
                                                    RenderPassType::ContinueFinal => {
                                                        (&main_rp_continue_final, &main_continue_final_fbs[n])
                                                    }
                                                    _ => unreachable!("not at first"),
                                                };

                                                r = r.begin_render_pass2(
                                                    &br::RenderPassBeginInfo::new(
                                                        rp,
                                                        fb,
                                                        sc.size.into_rect(br::Offset2D::ZERO),
                                                        &[br::ClearValue::color_f32([
                                                            0.0, 0.0, 0.0, 1.0,
                                                        ])],
                                                    ),
                                                    &br::SubpassBeginInfo::new(
                                                        br::SubpassContents::Inline,
                                                    ),
                                                );
                                            }
                                            if !pipeline_bound {
                                                pipeline_bound = true;

                                                r = r
                                                    .bind_pipeline(
                                                        br::PipelineBindPoint::Graphics,
                                                        match last_composite_render_instructions.render_pass_types[rpt_pointer] {
                                                            RenderPassType::Grabbed => {
                                                                &composite_pipeline_grabbed
                                                            }
                                                            RenderPassType::Final => {
                                                                &composite_pipeline_final
                                                            }
                                                            RenderPassType::ContinueGrabbed => {
                                                                &composite_pipeline_continue_grabbed
                                                            }
                                                            RenderPassType::ContinueFinal => {
                                                                &composite_pipeline_continue_final
                                                            }
                                                        },
                                                    )
                                                    .push_constant(
                                                        &composite_pipeline_layout,
                                                        br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                                                        0,
                                                        &[
                                                            sc.size.width as f32,
                                                            sc.size.height as f32,
                                                        ],
                                                    )
                                                    .bind_descriptor_sets(
                                                        br::PipelineBindPoint::Graphics,
                                                        &composite_pipeline_layout,
                                                        0,
                                                        &[composite_alphamask_group_descriptor],
                                                        &[],
                                                    );
                                            }

                                            r = r
                                                .bind_descriptor_sets(
                                                    br::PipelineBindPoint::Graphics,
                                                    &composite_pipeline_layout,
                                                    1,
                                                    &[composite_backdrop_buffer_descriptor_sets
                                                        [backdrop_buffer]],
                                                    &[],
                                                )
                                                .draw(
                                                    4,
                                                    index_range.len() as _,
                                                    0,
                                                    index_range.start as _,
                                                )
                                        }
                                        CompositeRenderingInstruction::GrabBackdrop => {
                                            r = r
                                                .end_render_pass2(&br::SubpassEndInfo::new())
                                                .pipeline_barrier_2(&br::DependencyInfo::new(
                                                    &[],
                                                    &[],
                                                    &[
                                                        br::ImageMemoryBarrier2::new(
                                                            composite_grab_buffer.image(),
                                                            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1)
                                                        ).transit_to(br::ImageLayout::TransferDestOpt.from_undefined())
                                                    ]
                                                ))
                                                .copy_image(
                                                    &sc.backbuffer_image(n),
                                                    br::ImageLayout::TransferSrcOpt,
                                                    composite_grab_buffer.image(),
                                                    br::ImageLayout::TransferDestOpt,
                                                    &[br::ImageCopy {
                                                        srcSubresource:
                                                            br::ImageSubresourceLayers::new(
                                                                br::AspectMask::COLOR,
                                                                0,
                                                                0..1,
                                                            ),
                                                        dstSubresource:
                                                            br::ImageSubresourceLayers::new(
                                                                br::AspectMask::COLOR,
                                                                0,
                                                                0..1,
                                                            ),
                                                        srcOffset: br::Offset3D::ZERO,
                                                        dstOffset: br::Offset3D::ZERO,
                                                        extent: sc.size.with_depth(1),
                                                    }],
                                                )
                                                .pipeline_barrier_2(&br::DependencyInfo::new(
                                                    &[],
                                                    &[],
                                                    &[br::ImageMemoryBarrier2::new(
                                                        composite_grab_buffer.image(),
                                                        br::ImageSubresourceRange::new(
                                                            br::AspectMask::COLOR,
                                                            0..1,
                                                            0..1,
                                                        ),
                                                    )
                                                    .transit_from(
                                                        br::ImageLayout::TransferDestOpt
                                                            .to(br::ImageLayout::ShaderReadOnlyOpt),
                                                    )
                                                    .from(
                                                        br::PipelineStageFlags2::COPY,
                                                        br::AccessFlags2::TRANSFER.write,
                                                    )
                                                    .to(
                                                        br::PipelineStageFlags2::FRAGMENT_SHADER,
                                                        br::AccessFlags2::SHADER.read,
                                                    )],
                                                ));
                                            rpt_pointer += 1;
                                            in_render_pass = false;
                                            pipeline_bound = false;
                                        }
                                        &CompositeRenderingInstruction::GenerateBackdropBlur {
                                            stdev,
                                            dest_backdrop_buffer,
                                            // 本来は必要な範囲だけ処理できれば効率いいんだけど面倒なので全面処理しちゃう
                                            ..
                                        } => {
                                            // downsample
                                            for lv in 0..BLUR_SAMPLE_STEPS {
                                                r = r
                                                    .begin_render_pass2(
                                                        &br::RenderPassBeginInfo::new(
                                                            &composite_backdrop_blur_rp,
                                                            &blur_downsample_pass_fbs[lv],
                                                            br::Rect2D {
                                                                offset: br::Offset2D::ZERO,
                                                                extent: br::Extent2D {
                                                                    width: sc.size.width
                                                                        >> (lv + 1),
                                                                    height: sc.size.height
                                                                        >> (lv + 1),
                                                                },
                                                            },
                                                            &[br::ClearValue::color_f32([
                                                                0.0, 0.0, 0.0, 0.0,
                                                            ])],
                                                        ),
                                                        &br::SubpassBeginInfo::new(
                                                            br::SubpassContents::Inline,
                                                        ),
                                                    )
                                                    .bind_pipeline(
                                                        br::PipelineBindPoint::Graphics,
                                                        &blur_downsample_pipelines[lv],
                                                    )
                                                    .push_constant(
                                                        &blur_pipeline_layout,
                                                        br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                                                        0,
                                                        &[
                                                            ((sc.size.width >> lv) as f32).recip(),
                                                            ((sc.size.height >> lv) as f32).recip(),
                                                            stdev.value(),
                                                        ],
                                                    )
                                                    .bind_descriptor_sets(
                                                        br::PipelineBindPoint::Graphics,
                                                        &blur_pipeline_layout,
                                                        0,
                                                        &[blur_fixed_descriptors[lv]],
                                                        &[],
                                                    )
                                                    .draw(3, 1, 0, 0)
                                                    .end_render_pass2(&br::SubpassEndInfo::new());
                                            }
                                            // upsample
                                            for lv in (0..BLUR_SAMPLE_STEPS).rev() {
                                                r = r
                                                    .begin_render_pass2(
                                                        &br::RenderPassBeginInfo::new(
                                                            &composite_backdrop_blur_rp,
                                                            if lv == 0 {
                                                                // final upsample
                                                                &composite_backdrop_blur_destination_fbs[dest_backdrop_buffer]
                                                            } else {
                                                                &blur_upsample_pass_fixed_fbs
                                                                    [lv - 1]
                                                            },
                                                            br::Rect2D {
                                                                offset: br::Offset2D::ZERO,
                                                                extent: br::Extent2D {
                                                                    width: sc.size.width >> lv,
                                                                    height: sc.size.height >> lv,
                                                                },
                                                            },
                                                            &[br::ClearValue::color_f32([
                                                                0.0, 0.0, 0.0, 0.0,
                                                            ])],
                                                        ),
                                                        &br::SubpassBeginInfo::new(
                                                            br::SubpassContents::Inline,
                                                        ),
                                                    )
                                                    .bind_pipeline(
                                                        br::PipelineBindPoint::Graphics,
                                                        &blur_upsample_pipelines[lv],
                                                    )
                                                    .push_constant(
                                                        &blur_pipeline_layout,
                                                        br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                                                        0,
                                                        &[
                                                            ((sc.size.width >> (lv + 1)) as f32)
                                                                .recip(),
                                                            ((sc.size.height >> (lv + 1)) as f32)
                                                                .recip(),
                                                            stdev.value(),
                                                        ],
                                                    )
                                                    .bind_descriptor_sets(
                                                        br::PipelineBindPoint::Graphics,
                                                        &blur_pipeline_layout,
                                                        0,
                                                        &[blur_fixed_descriptors[lv + 1]],
                                                        &[],
                                                    )
                                                    .draw(3, 1, 0, 0)
                                                    .end_render_pass2(&br::SubpassEndInfo::new());
                                            }
                                        }
                                    };
                                }

                                r
                            })
                            .end_render_pass2(&br::SubpassEndInfo::new())
                            .end()
                            .unwrap();
                        }

                        main_cb_invalid = false;
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
                    tracing::trace!(width, height, "ToplevelWindowConfigure");
                    frame_resize_request = Some((width, height));
                }
                AppEvent::ToplevelWindowSurfaceConfigure { serial } => {
                    if let Some((w, h)) = frame_resize_request.take() {
                        if w != sc.size.width || h != sc.size.height {
                            tracing::trace!(w, h, "frame resize");

                            client_size.set((w as f32, h as f32));

                            if last_rendering {
                                last_render_command_fence.wait().unwrap();
                                last_render_command_fence.reset().unwrap();
                                last_rendering = false;
                            }

                            unsafe {
                                main_cp.reset(br::CommandPoolResetFlags::EMPTY).unwrap();
                            }
                            main_cb_invalid = true;

                            sc.resize(br::Extent2D {
                                width: (w as f32 * app_shell.ui_scale_factor()) as _,
                                height: (h as f32 * app_shell.ui_scale_factor()) as _,
                            });

                            main_grabbed_fbs = sc
                                .backbuffer_views()
                                .map(|bb| {
                                    br::FramebufferObject::new(
                                        &subsystem,
                                        &br::FramebufferCreateInfo::new(
                                            &main_rp_grabbed,
                                            &[bb.as_transparent_ref()],
                                            sc.size.width,
                                            sc.size.height,
                                        ),
                                    )
                                    .unwrap()
                                })
                                .collect::<Vec<_>>();
                            main_final_fbs = sc
                                .backbuffer_views()
                                .map(|bb| {
                                    br::FramebufferObject::new(
                                        &subsystem,
                                        &br::FramebufferCreateInfo::new(
                                            &main_rp_final,
                                            &[bb.as_transparent_ref()],
                                            sc.size.width,
                                            sc.size.height,
                                        ),
                                    )
                                    .unwrap()
                                })
                                .collect::<Vec<_>>();
                            main_continue_grabbed_fbs = sc
                                .backbuffer_views()
                                .map(|bb| {
                                    br::FramebufferObject::new(
                                        &subsystem,
                                        &br::FramebufferCreateInfo::new(
                                            &main_rp_continue_grabbed,
                                            &[bb.as_transparent_ref()],
                                            sc.size.width,
                                            sc.size.height,
                                        ),
                                    )
                                    .unwrap()
                                })
                                .collect::<Vec<_>>();
                            main_continue_final_fbs = sc
                                .backbuffer_views()
                                .map(|bb| {
                                    br::FramebufferObject::new(
                                        &subsystem,
                                        &br::FramebufferCreateInfo::new(
                                            &main_rp_continue_final,
                                            &[bb.as_transparent_ref()],
                                            sc.size.width,
                                            sc.size.height,
                                        ),
                                    )
                                    .unwrap()
                                })
                                .collect::<Vec<_>>();

                            composite_backdrop_buffers_invalidated = true;

                            drop(composite_grab_buffer);
                            drop(composite_grab_buffer_memory);
                            let mut composite_grab_buffer1 = br::ImageObject::new(
                                &subsystem,
                                &br::ImageCreateInfo::new(sc.size, sc.color_format())
                                    .sampled()
                                    .transfer_dest(),
                            )
                            .unwrap();
                            let req = composite_grab_buffer1.requirements();
                            let Some(memindex) =
                                subsystem.find_device_local_memory_index(req.memoryTypeBits)
                            else {
                                tracing::error!(
                                    memory_index_mask = req.memoryTypeBits,
                                    "no suitable memory for composite grab buffer"
                                );
                                std::process::exit(1);
                            };
                            composite_grab_buffer_memory = br::DeviceMemoryObject::new(
                                &subsystem,
                                &br::MemoryAllocateInfo::new(req.size, memindex),
                            )
                            .unwrap();
                            composite_grab_buffer1
                                .bind(&composite_grab_buffer_memory, 0)
                                .unwrap();
                            composite_grab_buffer = br::ImageViewBuilder::new(
                                composite_grab_buffer1,
                                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
                            )
                            .create()
                            .unwrap();

                            blur_upsample_pass_fixed_fbs.clear();
                            blur_downsample_pass_fbs.clear();
                            blur_temporal_buffers.clear();
                            drop(blur_temporal_buffer_memory);
                            let mut resources_offsets = Vec::with_capacity(2);
                            let mut top = 0;
                            let mut memory_index_mask = !0u32;
                            for lv in 0..2 {
                                let r = br::ImageObject::new(
                                    &subsystem,
                                    &br::ImageCreateInfo::new(
                                        br::Extent2D {
                                            width: sc.size.width >> (lv + 1),
                                            height: sc.size.height >> (lv + 1),
                                        },
                                        sc.color_format(),
                                    )
                                    .sampled()
                                    .as_color_attachment(),
                                )
                                .unwrap();
                                let req = r.requirements();
                                assert!(req.alignment.is_power_of_two());
                                let offset = (top + req.alignment - 1) & !(req.alignment - 1);

                                top = offset + req.size;
                                memory_index_mask &= req.memoryTypeBits;
                                resources_offsets.push((r, offset));
                            }
                            blur_temporal_buffer_memory = br::DeviceMemoryObject::new(
                                &subsystem,
                                &br::MemoryAllocateInfo::new(
                                    top,
                                    subsystem
                                        .find_device_local_memory_index(memory_index_mask)
                                        .unwrap(),
                                ),
                            )
                            .unwrap();
                            for (mut r, o) in resources_offsets {
                                r.bind(&blur_temporal_buffer_memory, o as _).unwrap();

                                blur_temporal_buffers.push(
                                    br::ImageViewBuilder::new(
                                        r,
                                        br::ImageSubresourceRange::new(
                                            br::AspectMask::COLOR,
                                            0..1,
                                            0..1,
                                        ),
                                    )
                                    .create()
                                    .unwrap(),
                                );
                            }
                            blur_downsample_pass_fbs.extend(
                                blur_temporal_buffers.iter().enumerate().map(|(lv, b)| {
                                    br::FramebufferObject::new(
                                        &subsystem,
                                        &br::FramebufferCreateInfo::new(
                                            &composite_backdrop_blur_rp,
                                            &[b.as_transparent_ref()],
                                            sc.size.width >> (lv + 1),
                                            sc.size.height >> (lv + 1),
                                        ),
                                    )
                                    .unwrap()
                                }),
                            );
                            blur_upsample_pass_fixed_fbs.extend(
                                blur_temporal_buffers
                                    .iter()
                                    .take(blur_temporal_buffers.len() - 1)
                                    .enumerate()
                                    .map(|(lv, b)| {
                                        br::FramebufferObject::new(
                                            &subsystem,
                                            &br::FramebufferCreateInfo::new(
                                                &composite_backdrop_blur_rp,
                                                &[b.as_transparent_ref()],
                                                sc.size.width >> (lv + 1),
                                                sc.size.height >> (lv + 1),
                                            ),
                                        )
                                        .unwrap()
                                    }),
                            );

                            let mut descriptor_writes =
                                vec![blur_fixed_descriptors[0].binding_at(0).write(
                                    br::DescriptorContents::CombinedImageSampler(vec![
                                    br::DescriptorImageInfo::new(
                                        &composite_grab_buffer,
                                        br::ImageLayout::ShaderReadOnlyOpt,
                                    )
                                    .with_sampler(&composite_sampler),
                                ]),
                                )];
                            descriptor_writes.extend((0..BLUR_SAMPLE_STEPS).map(|n| {
                                blur_fixed_descriptors[n + 1].binding_at(0).write(
                                    br::DescriptorContents::CombinedImageSampler(vec![
                                        br::DescriptorImageInfo::new(
                                            &blur_temporal_buffers[n],
                                            br::ImageLayout::ShaderReadOnlyOpt,
                                        )
                                        .with_sampler(&composite_sampler),
                                    ]),
                                )
                            }));
                            subsystem.update_descriptor_sets(&descriptor_writes, &[]);

                            let [
                                composite_pipeline_grabbed1,
                                composite_pipeline_final1,
                                composite_pipeline_continue_grabbed1,
                                composite_pipeline_continue_final1,
                            ] = subsystem
                                .create_graphics_pipelines_array(&[
                                    br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp_grabbed.subpass(0),
                                        &composite_shader_stages,
                                        &composite_vinput,
                                        &composite_ia_state,
                                        &br::PipelineViewportStateCreateInfo::new(
                                            &[sc.size
                                                .into_rect(br::Offset2D::ZERO)
                                                .make_viewport(0.0..1.0)],
                                            &[sc.size.into_rect(br::Offset2D::ZERO)],
                                        ),
                                        &composite_raster_state,
                                        &composite_blend_state,
                                    )
                                    .multisample_state(MS_STATE_EMPTY),
                                    br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp_final.subpass(0),
                                        &composite_shader_stages,
                                        &composite_vinput,
                                        &composite_ia_state,
                                        &br::PipelineViewportStateCreateInfo::new(
                                            &[sc.size
                                                .into_rect(br::Offset2D::ZERO)
                                                .make_viewport(0.0..1.0)],
                                            &[sc.size.into_rect(br::Offset2D::ZERO)],
                                        ),
                                        &composite_raster_state,
                                        &composite_blend_state,
                                    )
                                    .multisample_state(MS_STATE_EMPTY),
                                    br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp_continue_grabbed.subpass(0),
                                        &composite_shader_stages,
                                        &composite_vinput,
                                        &composite_ia_state,
                                        &br::PipelineViewportStateCreateInfo::new(
                                            &[sc.size
                                                .into_rect(br::Offset2D::ZERO)
                                                .make_viewport(0.0..1.0)],
                                            &[sc.size.into_rect(br::Offset2D::ZERO)],
                                        ),
                                        &composite_raster_state,
                                        &composite_blend_state,
                                    )
                                    .multisample_state(MS_STATE_EMPTY),
                                    br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp_continue_final.subpass(0),
                                        &composite_shader_stages,
                                        &composite_vinput,
                                        &composite_ia_state,
                                        &br::PipelineViewportStateCreateInfo::new(
                                            &[sc.size
                                                .into_rect(br::Offset2D::ZERO)
                                                .make_viewport(0.0..1.0)],
                                            &[sc.size.into_rect(br::Offset2D::ZERO)],
                                        ),
                                        &composite_raster_state,
                                        &composite_blend_state,
                                    )
                                    .multisample_state(MS_STATE_EMPTY),
                                ])
                                .unwrap();
                            composite_pipeline_grabbed = composite_pipeline_grabbed1;
                            composite_pipeline_final = composite_pipeline_final1;
                            composite_pipeline_continue_grabbed =
                                composite_pipeline_continue_grabbed1;
                            composite_pipeline_continue_final = composite_pipeline_continue_final1;

                            drop(blur_upsample_pipelines);
                            drop(blur_downsample_pipelines);
                            let blur_sample_viewport_scissors = (0..BLUR_SAMPLE_STEPS + 1)
                                .map(|lv| {
                                    let size = br::Extent2D {
                                        width: sc.size.width >> lv,
                                        height: sc.size.height >> lv,
                                    };

                                    (
                                        [size
                                            .into_rect(br::Offset2D::ZERO)
                                            .make_viewport(0.0..1.0)],
                                        [size.into_rect(br::Offset2D::ZERO)],
                                    )
                                })
                                .collect::<Vec<_>>();
                            let blur_sample_viewport_states = blur_sample_viewport_scissors
                                .iter()
                                .map(|(vp, sc)| br::PipelineViewportStateCreateInfo::new(vp, sc))
                                .collect::<Vec<_>>();
                            blur_downsample_pipelines = subsystem
                                .create_graphics_pipelines(
                                    &blur_sample_viewport_states
                                        .iter()
                                        .skip(1)
                                        .map(|vp_state| {
                                            br::GraphicsPipelineCreateInfo::new(
                                                &blur_pipeline_layout,
                                                composite_backdrop_blur_rp.subpass(0),
                                                &composite_backdrop_blur_downsample_stages,
                                                VI_STATE_EMPTY,
                                                IA_STATE_TRILIST,
                                                vp_state,
                                                RASTER_STATE_DEFAULT_FILL_NOCULL,
                                                BLEND_STATE_SINGLE_NONE,
                                            )
                                            .multisample_state(MS_STATE_EMPTY)
                                        })
                                        .collect::<Vec<_>>(),
                                )
                                .unwrap();
                            blur_upsample_pipelines = subsystem
                                .create_graphics_pipelines(
                                    &blur_sample_viewport_states
                                        .iter()
                                        .take(blur_sample_viewport_states.len() - 1)
                                        .map(|vp_state| {
                                            br::GraphicsPipelineCreateInfo::new(
                                                &blur_pipeline_layout,
                                                composite_backdrop_blur_rp.subpass(0),
                                                &composite_backdrop_blur_upsample_stages,
                                                VI_STATE_EMPTY,
                                                IA_STATE_TRILIST,
                                                vp_state,
                                                RASTER_STATE_DEFAULT_FILL_NOCULL,
                                                BLEND_STATE_SINGLE_NONE,
                                            )
                                            .multisample_state(MS_STATE_EMPTY)
                                        })
                                        .collect::<Vec<_>>(),
                                )
                                .unwrap();

                            app_update_context
                                .editing_atlas_renderer
                                .borrow_mut()
                                .recreate(
                                    &subsystem,
                                    match editing_atlas_current_bound_pipeline {
                                        RenderPassType::Grabbed => main_rp_grabbed.subpass(0),
                                        RenderPassType::Final => main_rp_final.subpass(0),
                                        RenderPassType::ContinueGrabbed => {
                                            main_rp_continue_grabbed.subpass(0)
                                        }
                                        RenderPassType::ContinueFinal => {
                                            main_rp_continue_final.subpass(0)
                                        }
                                    },
                                    sc.size,
                                );
                        }
                    }

                    app_shell.post_configure(serial);
                }
                AppEvent::MainWindowPointerMove {
                    enter_serial,
                    surface_x,
                    surface_y,
                } => {
                    app_update_context.for_view_feedback.current_sec = t.elapsed().as_secs_f32();
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
                    app_update_context.for_view_feedback.current_sec = t.elapsed().as_secs_f32();
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
                    app_update_context.for_view_feedback.current_sec = t.elapsed().as_secs_f32();
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
                AppEvent::UIMessageDialogRequest { content } => {
                    let id = uuid::Uuid::new_v4();
                    let p = MessageDialogPresenter::new(
                        &mut PresenterInitContext {
                            for_view: ViewInitContext {
                                subsystem: &subsystem,
                                staging_scratch_buffer: &mut staging_scratch_buffer,
                                atlas: &mut composition_alphamask_surface_atlas,
                                ui_scale_factor: app_shell.ui_scale_factor(),
                                fonts: &mut font_set,
                                composite_tree: &mut app_update_context
                                    .for_view_feedback
                                    .composite_tree,
                                composite_instance_manager: &mut composite_instance_buffer,
                                ht: &mut ht_manager,
                            },
                            app_state: &mut app_update_context.state,
                        },
                        id,
                        &content,
                    );
                    p.show(
                        CompositeTree::ROOT,
                        &mut app_update_context.for_view_feedback.composite_tree,
                        ht_root,
                        &mut ht_manager,
                        t.elapsed().as_secs_f32(),
                    );

                    // TODO: ここでRECOMPUTE_POINTER_ENTER相当の処理をしないといけない(ポインタを動かさないかぎりEnter状態が続くのでマスクを貫通できる)
                    // クローズしたときも同じ

                    tracing::debug!(
                        byte_size = staging_scratch_buffer.total_reserved_amount(),
                        "Reserved Staging Buffers during Popup UI",
                    );
                    staging_scratch_buffer.reset();

                    popups.insert(id, p);
                }
                AppEvent::UIPopupClose { id } => {
                    if let Some(inst) = popups.get(&id) {
                        inst.hide(
                            &mut app_update_context.for_view_feedback.composite_tree,
                            &mut ht_manager,
                            t.elapsed().as_secs_f32(),
                        );
                    }
                }
                AppEvent::UIPopupUnmount { id } => {
                    if let Some(inst) = popups.remove(&id) {
                        inst.unmount(&mut app_update_context.for_view_feedback.composite_tree);
                    }
                }
            }
        }
    }

    if let Err(e) = unsafe { subsystem.wait() } {
        tracing::warn!(reason = ?e, "Error in waiting pending works before shutdown");
    }
}
