mod app_state;
mod base_system;
mod bg_worker;
mod composite;
mod coordinate;
mod feature;
mod hittest;
mod input;
mod mathext;
mod peridot;
mod platform;
mod quadtree;
mod shell;
mod source_reader;
mod subsystem;
mod svg;
mod text;
mod thirdparty;
mod trigger_cell;
mod uikit;

#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::{
    cell::{Cell, RefCell, UnsafeCell},
    collections::{BTreeSet, HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
};

use crate::{composite::FloatParameter, quadtree::QuadTree};
use app_state::{AppState, SpriteInfo};
use base_system::AppBaseSystem;
use bedrock::{
    self as br, CommandBufferMut, CommandPoolMut, DescriptorPoolMut, Device, DeviceMemoryMut,
    Fence, FenceMut, ImageChild, InstanceChild, MemoryBound, PhysicalDevice, RenderPass,
    ShaderModule, Swapchain, VkHandle, VkHandleMut, VkObject, VkRawHandle,
};
use bg_worker::{BackgroundWorker, BackgroundWorkerViewFeedback};
use composite::{
    AnimatableColor, AnimatableFloat, AnimationData, AtlasRect, CompositeInstanceData,
    CompositeMode, CompositeRect, CompositeRenderingData, CompositeRenderingInstruction,
    CompositeStreamingData, CompositeTree, CompositeTreeFloatParameterRef, CompositeTreeRef,
    RenderPassAfterOperation, RenderPassRequirements,
};
use coordinate::SizePixels;
use feature::editing_atlas_renderer::EditingAtlasRenderer;
use hittest::{
    CursorShape, HitTestTreeActionHandler, HitTestTreeData, HitTestTreeManager, HitTestTreeRef,
};
use input::EventContinueControl;
use parking_lot::RwLock;
use shell::AppShell;
use subsystem::{StagingScratchBufferManager, Subsystem};
use text::TextLayout;
use trigger_cell::TriggerCell;

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
    ToplevelWindowMinimizeRequest,
    ToplevelWindowMaximizeRequest,
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
    AppMenuToggle,
    AppMenuRequestAddSprite,
    BeginBackgroundWork {
        thread_number: usize,
        message: String,
    },
    EndBackgroundWork {
        thread_number: usize,
    },
    SelectSprite {
        index: usize,
    },
    DeselectSprite,
}

pub struct AppEventBus {
    queue: UnsafeCell<VecDeque<AppEvent>>,
    #[cfg(target_os = "linux")]
    efd: platform::linux::EventFD,
    #[cfg(windows)]
    event_notify: platform::win32::event::EventObject,
}
impl AppEventBus {
    pub fn push(&self, e: AppEvent) {
        unsafe { &mut *self.queue.get() }.push_back(e);
        #[cfg(target_os = "linux")]
        self.efd.add(1).unwrap();
        #[cfg(windows)]
        platform::win32::event::EventObject::new(None, true, false).unwrap();
    }

    fn pop(&self) -> Option<AppEvent> {
        unsafe { &mut *self.queue.get() }.pop_front()
    }

    fn notify_clear(&self) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        match self.efd.take() {
            // WouldBlock(EAGAIN)はでてきてもOK
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
            Ok(_) => Ok(()),
        }
        #[cfg(windows)]
        {
            self.event_notify.reset().map_err(From::from)
        }
    }
}

const MS_STATE_EMPTY: &'static br::PipelineMultisampleStateCreateInfo =
    &br::PipelineMultisampleStateCreateInfo::new();
const BLEND_STATE_SINGLE_NONE: &'static br::PipelineColorBlendStateCreateInfo<'static> =
    &br::PipelineColorBlendStateCreateInfo::new(&[
        br::vk::VkPipelineColorBlendAttachmentState::NOBLEND,
    ]);
const BLEND_STATE_SINGLE_PREMULTIPLIED: &'static br::PipelineColorBlendStateCreateInfo<'static> =
    &br::PipelineColorBlendStateCreateInfo::new(&[
        br::vk::VkPipelineColorBlendAttachmentState::PREMULTIPLIED,
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

pub trait ViewUpdate {
    fn update(&self, ct: &mut CompositeTree, ht: &mut HitTestTreeManager, current_sec: f32);
}

pub struct ViewInitContext<'r, 'app_system, 'subsystem> {
    pub base_system: &'app_system mut AppBaseSystem<'subsystem>,
    pub staging_scratch_buffer: &'r mut StagingScratchBufferManager<'subsystem>,
    pub ui_scale_factor: f32,
}

pub struct PresenterInitContext<'r, 'state, 'app_system, 'subsystem> {
    pub for_view: ViewInitContext<'r, 'app_system, 'subsystem>,
    pub app_state: &'state mut AppState<'subsystem>,
}

pub struct ViewFeedbackContext<'base_system, 'subsystem> {
    pub base_system: &'base_system mut AppBaseSystem<'subsystem>,
    pub current_sec: f32,
}

pub struct AppUpdateContext<'d, 'subsystem> {
    pub event_queue: &'d AppEventBus,
    pub state: &'d RefCell<AppState<'subsystem>>,
    pub ui_scale_factor: f32,
}

pub struct BufferedStagingScratchBuffer<'subsystem> {
    buffers: Vec<RwLock<StagingScratchBufferManager<'subsystem>>>,
    active_index: usize,
}
impl<'subsystem> BufferedStagingScratchBuffer<'subsystem> {
    pub fn new(subsystem: &'subsystem Subsystem, count: usize) -> Self {
        Self {
            buffers: core::iter::repeat_with(|| {
                RwLock::new(StagingScratchBufferManager::new(subsystem))
            })
            .take(count)
            .collect(),
            active_index: 0,
        }
    }

    pub fn flip_next_and_ready(&mut self) {
        self.active_index = (self.active_index + 1) % self.buffers.len();
        self.buffers[self.active_index].get_mut().reset();
    }

    pub fn active_buffer<'s>(
        &'s self,
    ) -> parking_lot::RwLockReadGuard<'s, StagingScratchBufferManager<'subsystem>> {
        self.buffers[self.active_index].read()
    }

    pub fn active_buffer_mut<'s>(&'s mut self) -> &'s mut StagingScratchBufferManager<'subsystem> {
        self.buffers[self.active_index].get_mut()
    }

    pub fn active_buffer_locked<'s>(
        &'s self,
    ) -> parking_lot::RwLockWriteGuard<'s, StagingScratchBufferManager<'subsystem>> {
        self.buffers[self.active_index].write()
    }
}

/// Attachment Description that Renders to read-only texture(shader read-only) covering entire pixels(no blending with previous content and nothing needs to be cleared)
const fn attachment_description_2_full_pixel_render_to_ro_texture(
    format: br::Format,
) -> br::AttachmentDescription2 {
    br::AttachmentDescription2::new(format)
        .with_layout_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())
        .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)
}

const fn const_subpass_description_2_single_color_write_only<const ATTACHMENT_INDEX: u32>()
-> br::SubpassDescription2<'static> {
    br::SubpassDescription2::new().colors(
        &const {
            [br::AttachmentReference2::color_attachment_opt(
                ATTACHMENT_INDEX,
            )]
        },
    )
}

enum CurrentSelectedSpriteTrigger {
    Focus {
        global_x_pixels: f32,
        global_y_pixels: f32,
        width_pixels: f32,
        height_pixels: f32,
    },
    Hide,
}
pub struct CurrentSelectedSpriteMarkerView {
    ct_root: CompositeTreeRef,
    global_x_param: CompositeTreeFloatParameterRef,
    global_y_param: CompositeTreeFloatParameterRef,
    view_offset_x_param: CompositeTreeFloatParameterRef,
    view_offset_y_param: CompositeTreeFloatParameterRef,
    focus_trigger: Cell<Option<CurrentSelectedSpriteTrigger>>,
    view_offset_x: Cell<f32>,
    view_offset_y: Cell<f32>,
}
impl CurrentSelectedSpriteMarkerView {
    const CORNER_RADIUS: f32 = 4.0;
    const THICKNESS: f32 = 2.0;
    const COLOR: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

    pub fn new(init: &mut ViewInitContext) -> Self {
        let render_size_px = ((Self::CORNER_RADIUS * 2.0 + 1.0) * init.ui_scale_factor) as u32;
        let border_image_atlas_rect = init.base_system.atlas.alloc(render_size_px, render_size_px);

        let render_pass = br::RenderPassObject::new(
            init.base_system.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[attachment_description_2_full_pixel_render_to_ro_texture(
                    init.base_system.atlas.format(),
                )],
                &[const_subpass_description_2_single_color_write_only::<0>()],
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
            init.base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[init
                    .base_system
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                init.base_system.atlas.size(),
                init.base_system.atlas.size(),
            ),
        )
        .unwrap();

        let [pipeline] = init
            .base_system
            .create_graphics_pipelines_array(&[br::GraphicsPipelineCreateInfo::new(
                init.base_system.require_empty_pipeline_layout(),
                render_pass.subpass(0),
                &[
                    init.base_system
                        .require_shader("resources/filltri.vert")
                        .on_stage(br::ShaderStage::Vertex, c"main"),
                    init.base_system
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
                    &[border_image_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                    &[border_image_atlas_rect.vk_rect()],
                ),
                RASTER_STATE_DEFAULT_FILL_NOCULL,
                BLEND_STATE_SINGLE_NONE,
            )
            .set_multisample_state(MS_STATE_EMPTY)])
            .unwrap();

        init.base_system
            .sync_execute_graphics_commands(|rec| {
                rec.begin_render_pass2(
                    &br::RenderPassBeginInfo::new(
                        &render_pass,
                        &framebuffer,
                        border_image_atlas_rect.vk_rect(),
                        &[br::ClearValue::color_f32([0.0; 4])],
                    ),
                    &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                )
                .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
                .draw(3, 1, 0, 0)
                .end_render_pass2(&br::SubpassEndInfo::new())
            })
            .unwrap();
        drop((pipeline, framebuffer, render_pass));

        let global_x_param = init
            .base_system
            .composite_tree
            .parameter_store_mut()
            .alloc_float(FloatParameter::Value(0.0));
        let global_y_param = init
            .base_system
            .composite_tree
            .parameter_store_mut()
            .alloc_float(FloatParameter::Value(0.0));
        let view_offset_x_param = init
            .base_system
            .composite_tree
            .parameter_store_mut()
            .alloc_float(FloatParameter::Value(0.0));
        let view_offset_y_param = init
            .base_system
            .composite_tree
            .parameter_store_mut()
            .alloc_float(FloatParameter::Value(0.0));

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            offset: [
                AnimatableFloat::Expression(Box::new(move |store| {
                    store.float_value(global_x_param) + store.float_value(view_offset_x_param)
                })),
                AnimatableFloat::Expression(Box::new(move |store| {
                    store.float_value(global_y_param) + store.float_value(view_offset_y_param)
                })),
            ],
            has_bitmap: true,
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            texatlas_rect: border_image_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value(Self::COLOR)),
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });

        Self {
            ct_root,
            global_x_param,
            global_y_param,
            view_offset_x_param,
            view_offset_y_param,
            focus_trigger: Cell::new(None),
            view_offset_x: Cell::new(0.0),
            view_offset_y: Cell::new(0.0),
        }
    }

    pub fn mount(&self, ct_parent: CompositeTreeRef, ct: &mut CompositeTree) {
        ct.add_child(ct_parent, self.ct_root);
    }

    pub fn update(&self, ct: &mut CompositeTree, current_sec: f32) {
        match self.focus_trigger.replace(None) {
            None => (),
            Some(CurrentSelectedSpriteTrigger::Focus {
                global_x_pixels,
                global_y_pixels,
                width_pixels,
                height_pixels,
            }) => {
                ct.parameter_store_mut()
                    .set_float(self.global_x_param, FloatParameter::Value(global_x_pixels));
                ct.parameter_store_mut()
                    .set_float(self.global_y_param, FloatParameter::Value(global_y_pixels));
                ct.get_mut(self.ct_root).size = [
                    AnimatableFloat::Value(width_pixels),
                    AnimatableFloat::Value(height_pixels),
                ];

                ct.get_mut(self.ct_root).scale_x = AnimatableFloat::Animated(
                    1.3,
                    AnimationData {
                        to_value: 1.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.15,
                        curve_p1: (0.0, 0.0),
                        curve_p2: (0.0, 1.0),
                        event_on_complete: None,
                    },
                );
                ct.get_mut(self.ct_root).scale_y = AnimatableFloat::Animated(
                    1.3,
                    AnimationData {
                        to_value: 1.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.15,
                        curve_p1: (0.0, 0.0),
                        curve_p2: (0.0, 1.0),
                        event_on_complete: None,
                    },
                );
                ct.get_mut(self.ct_root).opacity = AnimatableFloat::Animated(
                    0.0,
                    AnimationData {
                        to_value: 1.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.15,
                        curve_p1: (0.5, 0.5),
                        curve_p2: (0.5, 0.5),
                        event_on_complete: None,
                    },
                );
            }
            Some(CurrentSelectedSpriteTrigger::Hide) => {
                ct.get_mut(self.ct_root).opacity = AnimatableFloat::Animated(
                    1.0,
                    AnimationData {
                        to_value: 0.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.15,
                        curve_p1: (0.5, 0.5),
                        curve_p2: (0.5, 0.5),
                        event_on_complete: None,
                    },
                );
            }
        }

        ct.parameter_store_mut().set_float(
            self.view_offset_x_param,
            FloatParameter::Value(self.view_offset_x.get()),
        );
        ct.parameter_store_mut().set_float(
            self.view_offset_y_param,
            FloatParameter::Value(self.view_offset_y.get()),
        );

        ct.mark_dirty(self.ct_root);
    }

    pub fn focus(&self, x_pixels: f32, y_pixels: f32, width_pixels: f32, height_pixels: f32) {
        self.focus_trigger
            .set(Some(CurrentSelectedSpriteTrigger::Focus {
                global_x_pixels: x_pixels,
                global_y_pixels: y_pixels,
                width_pixels,
                height_pixels,
            }));
    }

    pub fn hide(&self) {
        self.focus_trigger
            .set(Some(CurrentSelectedSpriteTrigger::Hide));
    }

    pub fn set_view_offset(&self, offset_x_pixels: f32, offset_y_pixels: f32) {
        self.view_offset_x.set(offset_x_pixels);
        self.view_offset_y.set(offset_y_pixels);
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
    is_dirty: Cell<bool>,
    place_inner: TriggerCell<bool>,
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
        let icon_atlas_rect = init
            .base_system
            .alloc_mask_atlas_rect(icon_size_px, icon_size_px);
        let circle_atlas_rect = init.base_system.alloc_mask_atlas_rect(
            (Self::SIZE * init.ui_scale_factor) as _,
            (Self::SIZE * init.ui_scale_factor) as _,
        );

        let bufsize = Self::ICON_VERTICES.len() * core::mem::size_of::<[f32; 2]>()
            + Self::ICON_INDICES.len() * core::mem::size_of::<u16>();
        let mut buf = br::BufferObject::new(
            &init.base_system.subsystem,
            &br::BufferCreateInfo::new(
                bufsize,
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::INDEX_BUFFER,
            ),
        )
        .unwrap();
        let mreq = buf.requirements();
        let memindex = init
            .base_system
            .find_direct_memory_index(mreq.memoryTypeBits)
            .expect("no suitable memory");
        let mut mem = br::DeviceMemoryObject::new(
            &init.base_system.subsystem,
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
        if !init.base_system.is_coherent_memory_type(memindex) {
            unsafe {
                init.base_system
                    .subsystem
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
            &init.base_system.subsystem,
            &br::ImageCreateInfo::new(icon_atlas_rect.extent(), br::vk::VK_FORMAT_R8_UNORM)
                .as_color_attachment()
                .usage_with(br::ImageUsageFlags::TRANSFER_SRC)
                .sample_counts(4),
        )
        .unwrap();
        let msaa_mem = init
            .base_system
            .alloc_device_local_memory_for_requirements(&msaa_buffer.requirements());
        msaa_buffer.bind(&msaa_mem, 0).unwrap();
        let msaa_buffer = br::ImageViewBuilder::new(
            msaa_buffer,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()
        .unwrap();

        let rp = br::RenderPassObject::new(
            init.base_system.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(br::vk::VK_FORMAT_R8_UNORM)
                    .color_memory_op(br::LoadOp::Clear, br::StoreOp::Store)
                    .layout_transition(br::ImageLayout::Undefined, br::ImageLayout::TransferSrcOpt)
                    .samples(4)],
                &[const_subpass_description_2_single_color_write_only::<0>()],
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
            init.base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp,
                &[msaa_buffer.as_transparent_ref()],
                icon_atlas_rect.width(),
                icon_atlas_rect.height(),
            ),
        )
        .unwrap();
        let rp_direct = br::RenderPassObject::new(
            init.base_system.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[attachment_description_2_full_pixel_render_to_ro_texture(
                    br::vk::VK_FORMAT_R8_UNORM,
                )],
                &[const_subpass_description_2_single_color_write_only::<0>()],
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
            init.base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp_direct,
                &[init
                    .base_system
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                init.base_system.mask_atlas_size(),
                init.base_system.mask_atlas_size(),
            ),
        )
        .unwrap();

        #[derive(br::SpecializationConstants)]
        struct CircleFragmentShaderParams {
            #[constant_id = 0]
            pub softness: f32,
        }
        let [pipeline, pipeline_circle] = init
            .base_system
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    init.base_system.require_empty_pipeline_layout(),
                    rp.subpass(0),
                    &[
                        init.base_system
                            .require_shader("resources/notrans.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.base_system
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
                .set_multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
                ),
                br::GraphicsPipelineCreateInfo::new(
                    init.base_system.require_empty_pipeline_layout(),
                    rp_direct.subpass(0),
                    &[
                        init.base_system
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.base_system
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
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        init.base_system
            .sync_execute_graphics_commands(|rec| {
                rec.begin_render_pass2(
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
                        .base_system
                        .barrier_for_mask_atlas_resource()
                        .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
                ))
                .resolve_image(
                    msaa_buffer.image(),
                    br::ImageLayout::TransferSrcOpt,
                    init.base_system.mask_atlas_image_transparent_ref(),
                    br::ImageLayout::TransferDestOpt,
                    &[br::vk::VkImageResolve {
                        srcSubresource: br::ImageSubresourceLayers::new(
                            br::AspectMask::COLOR,
                            0,
                            0..1,
                        ),
                        srcOffset: br::Offset3D::ZERO,
                        dstSubresource: br::ImageSubresourceLayers::new(
                            br::AspectMask::COLOR,
                            0,
                            0..1,
                        ),
                        dstOffset: icon_atlas_rect.lt_offset().with_z(0),
                        extent: icon_atlas_rect.extent().with_depth(1),
                    }],
                )
                .pipeline_barrier_2(&br::DependencyInfo::new(
                    &[],
                    &[],
                    &[init
                        .base_system
                        .barrier_for_mask_atlas_resource()
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
            })
            .unwrap();
        drop((
            pipeline,
            pipeline_circle,
            fb_direct,
            rp_direct,
            fb,
            rp,
            msaa_mem,
            msaa_buffer,
            mem,
            buf,
        ));

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            size: [
                AnimatableFloat::Value(Self::SIZE * init.ui_scale_factor),
                AnimatableFloat::Value(Self::SIZE * init.ui_scale_factor),
            ],
            offset: [
                AnimatableFloat::Value((-Self::SIZE - 8.0) * init.ui_scale_factor),
                AnimatableFloat::Value(8.0 * init.ui_scale_factor),
            ],
            relative_offset_adjustment: [1.0, 0.0],
            has_bitmap: true,
            texatlas_rect: circle_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([1.0, 1.0, 1.0, 0.0])),
            ..Default::default()
        });
        let ct_icon = init.base_system.register_composite_rect(CompositeRect {
            offset: [
                AnimatableFloat::Value(-Self::ICON_SIZE * 0.5 * init.ui_scale_factor),
                AnimatableFloat::Value(-Self::ICON_SIZE * 0.5 * init.ui_scale_factor),
            ],
            relative_offset_adjustment: [0.5, 0.5],
            size: [
                AnimatableFloat::Value(Self::ICON_SIZE * init.ui_scale_factor),
                AnimatableFloat::Value(Self::ICON_SIZE * init.ui_scale_factor),
            ],
            has_bitmap: true,
            texatlas_rect: icon_atlas_rect.clone(),
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        init.base_system.set_composite_tree_parent(ct_icon, ct_root);

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            width: Self::SIZE,
            height: Self::SIZE,
            top: 8.0,
            left: -Self::SIZE - 8.0,
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
            is_dirty: Cell::new(false),
            place_inner: TriggerCell::new(true),
        }
    }

    pub fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        app_system.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    pub fn update(&self, app_system: &mut AppBaseSystem, current_sec: f32) {
        let ui_scale_factor = self.ui_scale_factor.get();

        if let Some(place_inner) = self.place_inner.get_if_triggered() {
            if place_inner {
                app_system.composite_tree.get_mut(self.ct_root).offset[0] =
                    AnimatableFloat::Animated(
                        8.0 * ui_scale_factor,
                        AnimationData {
                            to_value: (-Self::SIZE - 8.0) * ui_scale_factor,
                            start_sec: current_sec,
                            end_sec: current_sec + 0.25,
                            curve_p1: (0.25, 0.8),
                            curve_p2: (0.5, 1.0),
                            event_on_complete: None,
                        },
                    );
                app_system.composite_tree.mark_dirty(self.ct_root);
                app_system.hit_tree.get_data_mut(self.ht_root).left = -Self::SIZE - 8.0;
            } else {
                app_system.composite_tree.get_mut(self.ct_root).offset[0] =
                    AnimatableFloat::Animated(
                        (-Self::SIZE - 8.0) * ui_scale_factor,
                        AnimationData {
                            to_value: 8.0 * ui_scale_factor,
                            start_sec: current_sec,
                            end_sec: current_sec + 0.25,
                            curve_p1: (0.25, 0.8),
                            curve_p2: (0.5, 1.0),
                            event_on_complete: None,
                        },
                    );
                app_system.composite_tree.mark_dirty(self.ct_root);
                app_system.hit_tree.get_data_mut(self.ht_root).left = 8.0;
            }

            let ct_icon = app_system.composite_tree.get_mut(self.ct_icon);
            ct_icon.texatlas_rect = self.icon_atlas_rect.clone();
            if !place_inner {
                // flip icon when placed outer
                core::mem::swap(
                    &mut ct_icon.texatlas_rect.left,
                    &mut ct_icon.texatlas_rect.right,
                );
            }

            app_system.composite_tree.mark_dirty(self.ct_icon);
        }

        if !self.is_dirty.replace(false) {
            // not modified
            return;
        }

        let opacity = match (self.hovering.get(), self.pressing.get()) {
            (_, true) => 0.375,
            (true, _) => 0.25,
            _ => 0.0,
        };

        let current = match app_system.composite_tree.get(self.ct_root).composite_mode {
            CompositeMode::ColorTint(ref x) => {
                x.evaluate(current_sec, app_system.composite_tree.parameter_store())
            }
            _ => unreachable!(),
        };
        app_system
            .composite_tree
            .get_mut(self.ct_root)
            .composite_mode = CompositeMode::ColorTint(AnimatableColor::Animated(
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
        app_system.composite_tree.mark_dirty(self.ct_root);
    }

    pub fn place_inner(&self) {
        self.place_inner.set(true);
    }

    pub fn place_outer(&self) {
        self.place_inner.set(false);
    }

    pub fn on_hover(&self) {
        self.hovering.set(true);
        self.is_dirty.set(true);
    }

    pub fn on_leave(&self) {
        self.hovering.set(false);
        // はずれたらpressingもなかったことにする
        self.pressing.set(false);
        self.is_dirty.set(true);
    }

    pub fn on_press(&self) {
        self.pressing.set(true);
        self.is_dirty.set(true);
    }

    pub fn on_release(&self) {
        self.pressing.set(false);
        self.is_dirty.set(true);
    }
}

pub struct SpriteListCellView {
    ct_root: CompositeTreeRef,
    ct_bg: CompositeTreeRef,
    ct_bg_selected: CompositeTreeRef,
    ct_label: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    top: Cell<f32>,
    ui_scale: Cell<f32>,
    hovering: TriggerCell<bool>,
    bound_sprite_index: Cell<usize>,
}
impl SpriteListCellView {
    const CORNER_RADIUS: f32 = 8.0;
    const MARGIN_H: f32 = 16.0;
    const HEIGHT: f32 = 24.0;
    const LABEL_MARGIN_LEFT: f32 = 8.0;

    #[tracing::instrument(name = "SpriteListCellView::new", skip(init))]
    pub fn new(
        init: &mut ViewInitContext,
        init_label: &str,
        init_top: f32,
        init_sprite_index: usize,
    ) -> Self {
        let label_layout =
            TextLayout::build_simple(init_label, &mut init.base_system.fonts.ui_default);
        let label_atlas_rect = init
            .base_system
            .alloc_mask_atlas_rect(label_layout.width_px(), label_layout.height_px());
        let label_stg_image_pixels =
            label_layout.build_stg_image_pixel_buffer(init.staging_scratch_buffer);
        let bg_atlas_rect = init.base_system.alloc_mask_atlas_rect(
            ((Self::CORNER_RADIUS * 2.0 + 1.0) * init.ui_scale_factor) as _,
            ((Self::CORNER_RADIUS * 2.0 + 1.0) * init.ui_scale_factor) as _,
        );

        let render_pass = br::RenderPassObject::new(
            init.base_system.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[
                    br::AttachmentDescription2::new(init.base_system.mask_atlas_format())
                        .with_layout_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())
                        .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store),
                ],
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
            init.base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[init
                    .base_system
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                init.base_system.mask_atlas_size(),
                init.base_system.mask_atlas_size(),
            ),
        )
        .unwrap();

        let [pipeline] = init
            .base_system
            .create_graphics_pipelines_array(&[br::GraphicsPipelineCreateInfo::new(
                init.base_system.require_empty_pipeline_layout(),
                render_pass.subpass(0),
                &[
                    init.base_system
                        .require_shader("resources/filltri.vert")
                        .on_stage(br::ShaderStage::Vertex, c"main"),
                    init.base_system
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
            .set_multisample_state(MS_STATE_EMPTY)])
            .unwrap();

        init.base_system
            .sync_execute_graphics_commands(|rec| {
                rec.pipeline_barrier_2(&br::DependencyInfo::new(
                    &[],
                    &[],
                    &[init
                        .base_system
                        .barrier_for_mask_atlas_resource()
                        .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
                ))
                .inject(|r| {
                    let (b, o) = init.staging_scratch_buffer.of(&label_stg_image_pixels);

                    r.copy_buffer_to_image(
                        b,
                        init.base_system.mask_atlas_image_transparent_ref(),
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
                        .base_system
                        .barrier_for_mask_atlas_resource()
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
            })
            .unwrap();
        drop((pipeline, framebuffer, render_pass));

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            offset: [
                AnimatableFloat::Value(Self::MARGIN_H * init.ui_scale_factor),
                AnimatableFloat::Value(init_top * init.ui_scale_factor),
            ],
            relative_size_adjustment: [1.0, 0.0],
            size: [
                AnimatableFloat::Value(-Self::MARGIN_H * 2.0 * init.ui_scale_factor),
                AnimatableFloat::Value(Self::HEIGHT * init.ui_scale_factor),
            ],
            ..Default::default()
        });
        let ct_label = init.base_system.register_composite_rect(CompositeRect {
            offset: [
                AnimatableFloat::Value(Self::LABEL_MARGIN_LEFT * init.ui_scale_factor),
                AnimatableFloat::Value(-label_layout.height() * 0.5),
            ],
            relative_offset_adjustment: [0.0, 0.5],
            size: [
                AnimatableFloat::Value(label_layout.width()),
                AnimatableFloat::Value(label_layout.height()),
            ],
            has_bitmap: true,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            texatlas_rect: label_atlas_rect,
            ..Default::default()
        });
        let ct_bg = init.base_system.register_composite_rect(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            has_bitmap: true,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([
                1.0, 1.0, 1.0, 0.125,
            ])),
            texatlas_rect: bg_atlas_rect.clone(),
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });
        let ct_bg_selected = init.base_system.register_composite_rect(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            has_bitmap: true,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.6, 0.8, 1.0, 0.25])),
            texatlas_rect: bg_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });

        init.base_system
            .set_composite_tree_parent(ct_bg_selected, ct_root);
        init.base_system.set_composite_tree_parent(ct_bg, ct_root);
        init.base_system
            .set_composite_tree_parent(ct_label, ct_root);

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            left: Self::MARGIN_H,
            top: init_top,
            width_adjustment_factor: 1.0,
            width: -Self::MARGIN_H * 2.0,
            height: Self::HEIGHT,
            ..Default::default()
        });

        Self {
            ct_root,
            ct_label,
            ct_bg,
            ct_bg_selected,
            ht_root,
            top: Cell::new(init_top),
            ui_scale: Cell::new(init.ui_scale_factor),
            hovering: TriggerCell::new(false),
            bound_sprite_index: Cell::new(init_sprite_index),
        }
    }

    pub fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
        base_system: &mut AppBaseSystem,
    ) {
        base_system.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    pub fn update(&self, base_system: &mut AppBaseSystem, current_sec: f32) {
        if let Some(hovering) = self.hovering.get_if_triggered() {
            base_system.composite_tree.get_mut(self.ct_bg).opacity = if hovering {
                AnimatableFloat::Animated(
                    0.0,
                    AnimationData {
                        to_value: 1.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.1,
                        curve_p1: (0.5, 0.5),
                        curve_p2: (0.5, 0.5),
                        event_on_complete: None,
                    },
                )
            } else {
                AnimatableFloat::Animated(
                    1.0,
                    AnimationData {
                        to_value: 0.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.1,
                        curve_p1: (0.5, 0.5),
                        curve_p2: (0.5, 0.5),
                        event_on_complete: None,
                    },
                )
            };
        }
    }

    pub fn unmount(&self, base_system: &mut AppBaseSystem) {
        base_system.composite_tree.remove_child(self.ct_root);
        base_system.hit_tree.remove_child(self.ht_root);
    }

    pub fn on_hover(&self) {
        self.hovering.set(true);
    }

    pub fn on_leave(&self) {
        self.hovering.set(false);
    }

    pub fn on_select(&self, ct: &mut CompositeTree) {
        ct.get_mut(self.ct_bg_selected).opacity = AnimatableFloat::Value(1.0);
    }

    pub fn on_deselect(&self, ct: &mut CompositeTree) {
        ct.get_mut(self.ct_bg_selected).opacity = AnimatableFloat::Value(0.0);
    }

    pub fn set_top(&self, top: f32, base_system: &mut AppBaseSystem) {
        let ui_scale_factor = self.ui_scale.get();

        base_system.composite_tree.get_mut(self.ct_root).offset[1] =
            AnimatableFloat::Value(top * ui_scale_factor);
        base_system.hit_tree.get_data_mut(self.ht_root).top = top;
        self.top.set(top);
    }

    pub fn set_name(&self, name: &str, subsystem: &Subsystem) {
        // TODO
    }

    pub fn bind_sprite_index(&self, index: usize) {
        self.bound_sprite_index.set(index);
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
    shown: TriggerCell<bool>,
    ui_scale_factor: Cell<f32>,
    is_dirty: Cell<bool>,
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
        let frame_image_atlas_rect = init
            .base_system
            .alloc_mask_atlas_rect(render_size_px, render_size_px);

        let title_blur_pixels =
            (Self::BLUR_AMOUNT_ONEDIR as f32 * init.ui_scale_factor).ceil() as _;
        let title_layout =
            TextLayout::build_simple("Sprites", &mut init.base_system.fonts.ui_default);
        let title_atlas_rect = init
            .base_system
            .alloc_mask_atlas_rect(title_layout.width_px(), title_layout.height_px());
        let title_blurred_atlas_rect = init.base_system.alloc_mask_atlas_rect(
            title_layout.width_px() + (title_blur_pixels * 2 + 1),
            title_layout.height_px() + (title_blur_pixels * 2 + 1),
        );
        let title_stg_image_pixels =
            title_layout.build_stg_image_pixel_buffer(init.staging_scratch_buffer);

        let mut title_blurred_work_image = br::ImageObject::new(
            init.base_system.subsystem,
            &br::ImageCreateInfo::new(
                title_blurred_atlas_rect.extent(),
                br::vk::VK_FORMAT_R8_UNORM,
            )
            .as_color_attachment()
            .sampled(),
        )
        .unwrap();
        let title_blurred_work_image_mem = init
            .base_system
            .alloc_device_local_memory_for_requirements(&title_blurred_work_image.requirements());
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
            init.base_system.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[
                    br::AttachmentDescription2::new(init.base_system.mask_atlas_format())
                        .with_layout_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())
                        .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store),
                ],
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
            init.base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[init
                    .base_system
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                init.base_system.mask_atlas_size(),
                init.base_system.mask_atlas_size(),
            ),
        )
        .unwrap();
        let title_blurred_work_framebuffer = br::FramebufferObject::new(
            init.base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[title_blurred_work_image_view.as_transparent_ref()],
                title_blurred_atlas_rect.width(),
                title_blurred_atlas_rect.height(),
            ),
        )
        .unwrap();

        let vsh_blur = init
            .base_system
            .require_shader("resources/filltri_uvmod.vert");
        let fsh_blur = init
            .base_system
            .require_shader("resources/blit_axis_convolved.frag");
        #[derive(br::SpecializationConstants)]
        struct ConvolutionFragmentShaderParams {
            #[constant_id = 0]
            max_count: u32,
        }

        let smp =
            br::SamplerObject::new(&init.base_system.subsystem, &br::SamplerCreateInfo::new())
                .unwrap();
        let dsl_tex1 = br::DescriptorSetLayoutObject::new(
            init.base_system.subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
                .make_binding(0, 1)
                .with_immutable_samplers(&[smp.as_transparent_ref()])]),
        )
        .unwrap();
        let mut dp = br::DescriptorPoolObject::new(
            init.base_system.subsystem,
            &br::DescriptorPoolCreateInfo::new(
                2,
                &[br::DescriptorType::CombinedImageSampler.make_size(2)],
            ),
        )
        .unwrap();
        let [ds_title, ds_title2] = dp
            .alloc_array(&[dsl_tex1.as_transparent_ref(), dsl_tex1.as_transparent_ref()])
            .unwrap();
        init.base_system.subsystem.update_descriptor_sets(
            &[
                ds_title
                    .binding_at(0)
                    .write(br::DescriptorContents::CombinedImageSampler(vec![
                        br::DescriptorImageInfo::new(
                            &init.base_system.mask_atlas_resource_transparent_ref(),
                            br::ImageLayout::ShaderReadOnlyOpt,
                        ),
                    ])),
                ds_title2
                    .binding_at(0)
                    .write(br::DescriptorContents::CombinedImageSampler(vec![
                        br::DescriptorImageInfo::new(
                            &title_blurred_work_image_view.as_transparent_ref(),
                            br::ImageLayout::ShaderReadOnlyOpt,
                        ),
                    ])),
            ],
            &[],
        );

        let blur_pipeline_layout = br::PipelineLayoutObject::new(
            init.base_system.subsystem,
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
            .base_system
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    init.base_system.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.base_system
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.base_system
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
                .set_multisample_state(MS_STATE_EMPTY),
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
                .set_multisample_state(MS_STATE_EMPTY),
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
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        fn gauss_distrib(x: f32, p: f32) -> f32 {
            (core::f32::consts::TAU * p.powi(2)).sqrt().recip()
                * (-x.powi(2) / (2.0 * p.powi(2))).exp()
        }
        let mut fsh_h_params = vec![0.0f32; title_blur_pixels as usize + 6];
        let mut fsh_v_params = vec![0.0f32; title_blur_pixels as usize + 6];
        // uv_limit
        fsh_h_params[0] = init
            .base_system
            .atlas
            .uv_from_pixels(title_atlas_rect.left as _);
        fsh_h_params[1] = init
            .base_system
            .atlas
            .uv_from_pixels(title_atlas_rect.top as _);
        fsh_h_params[2] = init
            .base_system
            .atlas
            .uv_from_pixels(title_atlas_rect.right as _);
        fsh_h_params[3] = init
            .base_system
            .atlas
            .uv_from_pixels(title_atlas_rect.bottom as _);
        fsh_v_params[2] = 1.0;
        fsh_v_params[3] = 1.0;
        // uv_step
        fsh_h_params[4] = 1.0 / init.base_system.mask_atlas_size() as f32;
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

        init.base_system
            .sync_execute_graphics_commands(|rec| {
                rec.pipeline_barrier_2(&br::DependencyInfo::new(
                    &[],
                    &[],
                    &[init
                        .base_system
                        .barrier_for_mask_atlas_resource()
                        .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
                ))
                .inject(|r| {
                    let (b, o) = init.staging_scratch_buffer.of(&title_stg_image_pixels);

                    r.copy_buffer_to_image(
                        b,
                        init.base_system.mask_atlas_image_transparent_ref(),
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
                        .base_system
                        .barrier_for_mask_atlas_resource()
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
                        init.base_system.atlas.uv_from_pixels(
                            (title_atlas_rect.width() + title_blur_pixels * 2 + 1) as _,
                        ),
                        init.base_system.atlas.uv_from_pixels(
                            (title_atlas_rect.height() + title_blur_pixels * 2 + 1) as _,
                        ),
                        init.base_system.atlas.uv_from_pixels(
                            title_atlas_rect.left as f32 - title_blur_pixels as f32,
                        ),
                        init.base_system
                            .atlas
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
            })
            .unwrap();
        drop((
            pipeline,
            pipeline_blur1,
            pipeline_blur,
            blur_pipeline_layout,
            ds_title,
            ds_title2,
            dp,
            dsl_tex1,
            smp,
            fsh_blur,
            vsh_blur,
            title_blurred_work_framebuffer,
            framebuffer,
            render_pass,
            title_blurred_work_image_view,
            title_blurred_work_image_mem,
        ));

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            offset: [
                AnimatableFloat::Value(Self::FLOATING_MARGIN * init.ui_scale_factor),
                AnimatableFloat::Value(header_height * init.ui_scale_factor),
            ],
            size: [
                AnimatableFloat::Value(Self::INIT_WIDTH * init.ui_scale_factor),
                AnimatableFloat::Value(
                    -(header_height + Self::FLOATING_MARGIN) * init.ui_scale_factor,
                ),
            ],
            relative_size_adjustment: [0.0, 1.0],
            has_bitmap: true,
            texatlas_rect: frame_image_atlas_rect.clone(),
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            composite_mode: CompositeMode::ColorTintBackdropBlur(
                AnimatableColor::Value([1.0, 1.0, 1.0, 0.0625]),
                AnimatableFloat::Value(3.0),
            ),
            ..Default::default()
        });
        let ct_title_blurred = init.base_system.register_composite_rect(CompositeRect {
            has_bitmap: true,
            offset: [
                AnimatableFloat::Value(-(title_blurred_atlas_rect.width() as f32 * 0.5)),
                AnimatableFloat::Value(
                    (12.0 - Self::BLUR_AMOUNT_ONEDIR as f32) * init.ui_scale_factor,
                ),
            ],
            relative_offset_adjustment: [0.5, 0.0],
            size: [
                AnimatableFloat::Value(title_blurred_atlas_rect.width() as f32),
                AnimatableFloat::Value(title_blurred_atlas_rect.height() as f32),
            ],
            texatlas_rect: title_blurred_atlas_rect.clone(),
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.1, 0.1, 0.1, 1.0])),
            ..Default::default()
        });
        let ct_title = init.base_system.register_composite_rect(CompositeRect {
            has_bitmap: true,
            offset: [
                AnimatableFloat::Value(-(title_atlas_rect.width() as f32 * 0.5)),
                AnimatableFloat::Value(12.0 * init.ui_scale_factor),
            ],
            relative_offset_adjustment: [0.5, 0.0],
            size: [
                AnimatableFloat::Value(title_atlas_rect.width() as f32),
                AnimatableFloat::Value(title_atlas_rect.height() as f32),
            ],
            texatlas_rect: title_atlas_rect.clone(),
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        init.base_system
            .set_composite_tree_parent(ct_title_blurred, ct_root);
        init.base_system
            .set_composite_tree_parent(ct_title, ct_root);

        let ht_frame = init.base_system.create_hit_tree(HitTestTreeData {
            top: header_height,
            left: Self::FLOATING_MARGIN,
            width: Self::INIT_WIDTH,
            height: -Self::FLOATING_MARGIN - header_height,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });
        let ht_resize_area = init.base_system.create_hit_tree(HitTestTreeData {
            left: -Self::RESIZE_AREA_WIDTH * 0.5,
            left_adjustment_factor: 1.0,
            width: Self::RESIZE_AREA_WIDTH,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });

        init.base_system
            .set_hit_tree_parent(ht_resize_area, ht_frame);

        Self {
            frame_image_atlas_rect,
            title_atlas_rect,
            title_blurred_atlas_rect,
            ct_root,
            ht_frame,
            ht_resize_area,
            width: Cell::new(Self::INIT_WIDTH),
            shown: TriggerCell::new(true),
            ui_scale_factor: Cell::new(init.ui_scale_factor as _),
            is_dirty: Cell::new(false),
        }
    }

    pub fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        app_system.set_tree_parent((self.ct_root, self.ht_frame), (ct_parent, ht_parent));
    }

    pub fn update(&self, app_system: &mut AppBaseSystem, current_sec: f32) {
        if let Some(shown) = self.shown.get_if_triggered() {
            if shown {
                app_system.composite_tree.get_mut(self.ct_root).offset[0] =
                    AnimatableFloat::Animated(
                        -self.width.get() * self.ui_scale_factor.get(),
                        AnimationData {
                            to_value: Self::FLOATING_MARGIN * self.ui_scale_factor.get(),
                            start_sec: current_sec,
                            end_sec: current_sec + 0.25,
                            curve_p1: (0.4, 1.25),
                            curve_p2: (0.5, 1.0),
                            event_on_complete: None,
                        },
                    );
                app_system.composite_tree.mark_dirty(self.ct_root);
                app_system.hit_tree.get_data_mut(self.ht_frame).left = Self::FLOATING_MARGIN;
            } else {
                app_system.composite_tree.get_mut(self.ct_root).offset[0] =
                    AnimatableFloat::Animated(
                        Self::FLOATING_MARGIN * self.ui_scale_factor.get(),
                        AnimationData {
                            to_value: -self.width.get() * self.ui_scale_factor.get(),
                            start_sec: current_sec,
                            end_sec: current_sec + 0.25,
                            curve_p1: (0.4, 1.25),
                            curve_p2: (0.5, 1.0),
                            event_on_complete: None,
                        },
                    );
                app_system.composite_tree.mark_dirty(self.ct_root);
                app_system.hit_tree.get_data_mut(self.ht_frame).left = -self.width.get();
            }
        }

        if !self.is_dirty.replace(false) {
            // no modification
            return;
        }

        let width = self.width.get();
        app_system.composite_tree.get_mut(self.ct_root).size[0] =
            AnimatableFloat::Value(width * self.ui_scale_factor.get());
        app_system.composite_tree.mark_dirty(self.ct_root);
        app_system.hit_tree.get_data_mut(self.ht_frame).width = width;
    }

    pub fn set_width(&self, width: f32) {
        self.width.set(width);
        self.is_dirty.set(true);
    }

    pub fn show(&self) {
        self.shown.set(true);
    }

    pub fn hide(&self) {
        self.shown.set(false);
    }
}

pub struct SpriteListPaneActionHandler {
    view: Rc<SpriteListPaneView>,
    toggle_button_view: Rc<SpriteListToggleButtonView>,
    cell_views: RefCell<Vec<SpriteListCellView>>,
    ht_resize_area: HitTestTreeRef,
    resize_state: Cell<Option<(f32, f32)>>,
    shown: Cell<bool>,
}
impl HitTestTreeActionHandler for SpriteListPaneActionHandler {
    fn cursor_shape(&self, sender: HitTestTreeRef, _context: &mut AppUpdateContext) -> CursorShape {
        if sender == self.ht_resize_area && self.shown.get() {
            return CursorShape::ResizeHorizontal;
        }

        CursorShape::Default
    }

    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view.on_hover();

            return EventContinueControl::STOP_PROPAGATION;
        }

        for v in self.cell_views.borrow().iter() {
            if sender == v.ht_root {
                v.on_hover();
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        EventContinueControl::empty()
    }

    fn on_pointer_leave(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view.on_leave();

            return EventContinueControl::STOP_PROPAGATION;
        }

        for v in self.cell_views.borrow().iter() {
            if sender == v.ht_root {
                v.on_leave();
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        EventContinueControl::empty()
    }

    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        args: &hittest::PointerActionArgs,
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
            self.toggle_button_view.on_press();

            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_move(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if self.shown.get() {
            if sender == self.view.ht_frame {
                // guard fallback
                return EventContinueControl::STOP_PROPAGATION;
            }

            if sender == self.ht_resize_area {
                if let Some((base_width, base_cx)) = self.resize_state.get() {
                    let w = (base_width + (args.client_x - base_cx)).max(16.0);
                    self.view.set_width(w);

                    return EventContinueControl::STOP_PROPAGATION;
                }
            }
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if self.shown.get() {
            if sender == self.view.ht_frame {
                // guard fallback
                return EventContinueControl::STOP_PROPAGATION;
            }

            if sender == self.ht_resize_area {
                if let Some((base_width, base_cx)) = self.resize_state.replace(None) {
                    let w = (base_width + (args.client_x - base_cx)).max(16.0);
                    self.view.set_width(w);

                    return EventContinueControl::RELEASE_CAPTURE_ELEMENT;
                }
            }
        }

        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view.on_release();

            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_click(
        &self,
        sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        _args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if self.shown.get() && sender == self.view.ht_frame {
            // guard fallback
            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.toggle_button_view.ht_root {
            let show = !self.shown.get();
            self.shown.set(show);

            if show {
                self.view.show();
                self.toggle_button_view.place_inner();
            } else {
                self.view.hide();
                self.toggle_button_view.place_outer();
            }

            return EventContinueControl::STOP_PROPAGATION
                | EventContinueControl::RECOMPUTE_POINTER_ENTER;
        }

        for v in self.cell_views.borrow().iter() {
            if sender == v.ht_root {
                context
                    .state
                    .borrow_mut()
                    .select_sprite(v.bound_sprite_index.get());
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        EventContinueControl::empty()
    }
}

pub struct SpriteListPanePresenter {
    view: Rc<SpriteListPaneView>,
    needs_rebuild_list_cells: Rc<Cell<bool>>,
    sprite_list_contents: Rc<RefCell<Vec<(String, bool)>>>,
    ui_scale_factor: f32,
    ht_action_handler: Rc<SpriteListPaneActionHandler>,
}
impl SpriteListPanePresenter {
    pub fn new(init: &mut PresenterInitContext, header_height: f32) -> Self {
        let view = Rc::new(SpriteListPaneView::new(&mut init.for_view, header_height));
        let toggle_button_view = Rc::new(SpriteListToggleButtonView::new(&mut init.for_view));

        toggle_button_view.mount(init.for_view.base_system, view.ct_root, view.ht_frame);

        let needs_rebuild_list_cells = Rc::new(Cell::new(false));
        let sprite_list_contents = Rc::new(RefCell::new(Vec::new()));
        init.app_state.register_sprites_view_feedback({
            let sprite_list_contents = Rc::downgrade(&sprite_list_contents);
            let needs_rebuild_list_cells = Rc::downgrade(&needs_rebuild_list_cells);

            move |sprites| {
                let Some(needs_rebuild_list_cells) = needs_rebuild_list_cells.upgrade() else {
                    // presenter teardown-ed
                    return;
                };
                let Some(sprite_list_contents) = sprite_list_contents.upgrade() else {
                    // presenter teardown-ed
                    return;
                };

                sprite_list_contents.borrow_mut().clear();
                sprite_list_contents
                    .borrow_mut()
                    .extend(sprites.iter().map(|x| (x.name.clone(), x.selected)));
                needs_rebuild_list_cells.set(true);
            }
        });

        let ht_action_handler = Rc::new(SpriteListPaneActionHandler {
            view: view.clone(),
            toggle_button_view: toggle_button_view.clone(),
            cell_views: RefCell::new(Vec::new()),
            ht_resize_area: view.ht_resize_area,
            resize_state: Cell::new(None),
            shown: Cell::new(true),
        });
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(view.ht_frame, &ht_action_handler);
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(view.ht_resize_area, &ht_action_handler);
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(toggle_button_view.ht_root, &ht_action_handler);

        Self {
            view,
            needs_rebuild_list_cells,
            sprite_list_contents,
            ui_scale_factor: init.for_view.ui_scale_factor,
            ht_action_handler,
        }
    }

    pub fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        self.view.mount(app_system, ct_parent, ht_parent);
    }

    pub fn update<'r, 'base_system, 'subsystem>(
        &mut self,
        app_system: &'base_system mut AppBaseSystem<'subsystem>,
        current_sec: f32,
        staging_scratch_buffer: &'r mut StagingScratchBufferManager<'subsystem>,
    ) {
        self.ht_action_handler.view.update(app_system, current_sec);
        self.ht_action_handler
            .toggle_button_view
            .update(app_system, current_sec);

        if self.needs_rebuild_list_cells.replace(false) {
            let sprite_list_contents = self.sprite_list_contents.borrow();
            let visible_contents = &sprite_list_contents[..];
            let mut cell_views = self.ht_action_handler.cell_views.borrow_mut();
            for (n, &(ref c, sel)) in visible_contents.iter().enumerate() {
                if cell_views.len() == n {
                    // create new one
                    let new_cell = SpriteListCellView::new(
                        &mut ViewInitContext {
                            base_system: app_system,
                            staging_scratch_buffer,
                            ui_scale_factor: self.ui_scale_factor,
                        },
                        c,
                        32.0 + n as f32 * SpriteListCellView::HEIGHT,
                        n,
                    );
                    new_cell.mount(
                        self.ht_action_handler.view.ct_root,
                        self.ht_action_handler.view.ht_frame,
                        app_system,
                    );
                    app_system
                        .hit_tree
                        .set_action_handler(new_cell.ht_root, &self.ht_action_handler);
                    if sel {
                        new_cell.on_select(&mut app_system.composite_tree);
                    }

                    cell_views.push(new_cell);
                    continue;
                }

                // reuse existing
                cell_views[n].set_top(32.0 + n as f32 * SpriteListCellView::HEIGHT, app_system);
                cell_views[n].set_name(c, app_system.subsystem);
                cell_views[n].bind_sprite_index(n);
                if sel {
                    cell_views[n].on_select(&mut app_system.composite_tree);
                } else {
                    cell_views[n].on_deselect(&mut app_system.composite_tree);
                }
            }
        }

        for v in self.ht_action_handler.cell_views.borrow().iter() {
            v.update(app_system, current_sec);
        }
    }
}

enum DragState {
    None,
    Grid {
        base_x_pixels: f32,
        base_y_pixels: f32,
        drag_start_client_x_pixels: f32,
        drag_start_client_y_pixels: f32,
    },
    Sprite {
        index: usize,
        base_x_pixels: f32,
        base_y_pixels: f32,
        base_width_pixels: f32,
        base_height_pixels: f32,
        drag_start_client_x_pixels: f32,
        drag_start_client_y_pixels: f32,
    },
}

struct HitTestRootTreeActionHandler<'subsystem> {
    sprites_qt: RefCell<QuadTree>,
    sprite_rects_cached: RefCell<Vec<(u32, u32, u32, u32)>>,
    current_selected_sprite_marker_view: std::rc::Weak<CurrentSelectedSpriteMarkerView>,
    editing_atlas_renderer: std::rc::Weak<RefCell<EditingAtlasRenderer<'subsystem>>>,
    ht_titlebar: Option<HitTestTreeRef>,
    drag_state: RefCell<DragState>,
}
impl<'subsystem> HitTestRootTreeActionHandler<'subsystem> {
    pub fn new(
        editing_atlas_renderer: &Rc<RefCell<EditingAtlasRenderer<'subsystem>>>,
        current_selected_sprite_marker_view: &Rc<CurrentSelectedSpriteMarkerView>,
        ht_titlebar: Option<HitTestTreeRef>,
    ) -> Self {
        Self {
            editing_atlas_renderer: Rc::downgrade(editing_atlas_renderer),
            current_selected_sprite_marker_view: Rc::downgrade(current_selected_sprite_marker_view),
            ht_titlebar,
            sprites_qt: RefCell::new(QuadTree::new()),
            sprite_rects_cached: RefCell::new(Vec::new()),
            drag_state: RefCell::new(DragState::None),
        }
    }
}
impl<'c> HitTestTreeActionHandler for HitTestRootTreeActionHandler<'c> {
    fn role(&self, sender: HitTestTreeRef) -> Option<hittest::Role> {
        if self.ht_titlebar.is_some_and(|x| sender == x) {
            return Some(hittest::Role::TitleBar);
        }

        None
    }

    fn on_pointer_down(
        &self,
        _sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        let Some(ear) = self.editing_atlas_renderer.upgrade() else {
            // app teardown-ed
            return EventContinueControl::empty();
        };
        let Some(marker_view) = self.current_selected_sprite_marker_view.upgrade() else {
            // app teardown-ed
            return EventContinueControl::empty();
        };

        let [current_offset_x, current_offset_y] = ear.borrow().offset();
        let pointing_x = args.client_x * context.ui_scale_factor - current_offset_x;
        let pointing_y = args.client_y * context.ui_scale_factor - current_offset_y;

        let state_locked = context.state.borrow();
        let sprite_drag_target_index =
            state_locked
                .selected_sprites_with_index()
                .rev()
                .find(|(_, x)| {
                    x.left as f32 <= pointing_x
                        && pointing_x <= x.right() as f32
                        && x.top as f32 <= pointing_y
                        && pointing_y <= x.bottom() as f32
                });
        if let Some((sprite_drag_target_index, target_sprite_ref)) = sprite_drag_target_index {
            // 選択中のスプライトの上で操作が開始された
            marker_view.hide();
            *self.drag_state.borrow_mut() = DragState::Sprite {
                index: sprite_drag_target_index,
                base_x_pixels: target_sprite_ref.left as f32,
                base_y_pixels: target_sprite_ref.top as f32,
                base_width_pixels: target_sprite_ref.width as f32,
                base_height_pixels: target_sprite_ref.height as f32,
                drag_start_client_x_pixels: args.client_x * context.ui_scale_factor,
                drag_start_client_y_pixels: args.client_y * context.ui_scale_factor,
            };
        } else {
            *self.drag_state.borrow_mut() = DragState::Grid {
                base_x_pixels: current_offset_x,
                base_y_pixels: current_offset_y,
                drag_start_client_x_pixels: args.client_x * context.ui_scale_factor,
                drag_start_client_y_pixels: args.client_y * context.ui_scale_factor,
            };
        }

        EventContinueControl::CAPTURE_ELEMENT
    }

    fn on_pointer_move(
        &self,
        _sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        let Some(ear) = self.editing_atlas_renderer.upgrade() else {
            // app teardown-ed
            return EventContinueControl::empty();
        };

        match &*self.drag_state.borrow() {
            DragState::None => (),
            DragState::Grid {
                base_x_pixels,
                base_y_pixels,
                drag_start_client_x_pixels,
                drag_start_client_y_pixels,
            } => {
                let dx = args.client_x * context.ui_scale_factor - drag_start_client_x_pixels;
                let dy = args.client_y * context.ui_scale_factor - drag_start_client_y_pixels;
                let ox = base_x_pixels + dx;
                let oy = base_y_pixels + dy;

                ear.borrow_mut().set_offset(ox, oy);

                if let Some(marker_view) = self.current_selected_sprite_marker_view.upgrade() {
                    marker_view.set_view_offset(ox, oy);
                }

                return EventContinueControl::STOP_PROPAGATION;
            }
            &DragState::Sprite {
                index,
                base_x_pixels,
                base_y_pixels,
                drag_start_client_x_pixels,
                drag_start_client_y_pixels,
                ..
            } => {
                let (dx, dy) = (
                    (args.client_x * context.ui_scale_factor) - drag_start_client_x_pixels,
                    (args.client_y * context.ui_scale_factor) - drag_start_client_y_pixels,
                );
                let (sx, sy) = (
                    (base_x_pixels + dx).max(0.0) as u32,
                    (base_y_pixels + dy).max(0.0) as u32,
                );
                ear.borrow_mut()
                    .update_sprite_offset(index, sx as _, sy as _);

                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        _sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        let Some(ear) = self.editing_atlas_renderer.upgrade() else {
            // app teardown-ed
            return EventContinueControl::empty();
        };
        let Some(marker_view) = self.current_selected_sprite_marker_view.upgrade() else {
            // app teardown-ed
            return EventContinueControl::empty();
        };

        match self.drag_state.replace(DragState::None) {
            DragState::None => (),
            DragState::Grid {
                base_x_pixels,
                base_y_pixels,
                drag_start_client_x_pixels,
                drag_start_client_y_pixels,
            } => {
                let dx = args.client_x * context.ui_scale_factor - drag_start_client_x_pixels;
                let dy = args.client_y * context.ui_scale_factor - drag_start_client_y_pixels;
                let ox = base_x_pixels + dx;
                let oy = base_y_pixels + dy;

                ear.borrow_mut().set_offset(ox, oy);

                if let Some(marker_view) = self.current_selected_sprite_marker_view.upgrade() {
                    marker_view.set_view_offset(ox, oy);
                }

                return EventContinueControl::STOP_PROPAGATION
                    | EventContinueControl::RELEASE_CAPTURE_ELEMENT;
            }
            DragState::Sprite {
                index,
                base_x_pixels,
                base_y_pixels,
                base_width_pixels,
                base_height_pixels,
                drag_start_client_x_pixels,
                drag_start_client_y_pixels,
            } => {
                let (dx, dy) = (
                    (args.client_x * context.ui_scale_factor) - drag_start_client_x_pixels,
                    (args.client_y * context.ui_scale_factor) - drag_start_client_y_pixels,
                );
                let (sx, sy) = (
                    (base_x_pixels + dx).max(0.0) as u32,
                    (base_y_pixels + dy).max(0.0) as u32,
                );
                context.state.borrow_mut().set_sprite_offset(index, sx, sy);

                // 選択インデックスが変わるわけではないのでここで選択枠Viewを復帰させる
                marker_view.focus(sx as _, sy as _, base_width_pixels, base_height_pixels);

                return EventContinueControl::STOP_PROPAGATION
                    | EventContinueControl::RELEASE_CAPTURE_ELEMENT;
            }
        }

        EventContinueControl::empty()
    }

    fn on_click(
        &self,
        _sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        let Some(ear) = self.editing_atlas_renderer.upgrade() else {
            // app teardown-ed
            return EventContinueControl::empty();
        };

        let x = args.client_x * context.ui_scale_factor - ear.borrow().offset()[0];
        let y = args.client_y * context.ui_scale_factor - ear.borrow().offset()[1];

        let mut max_index = None;
        for n in self
            .sprites_qt
            .borrow()
            .iter_possible_element_indices(x as _, y as _)
        {
            let (l, t, r, b) = self.sprite_rects_cached.borrow()[n];
            if l as f32 <= x && x <= r as f32 && t as f32 <= y && y <= b as f32 {
                // 大きいインデックスのものが最前面にいるのでmaxをとる
                max_index = Some(max_index.map_or(n, |x: usize| x.max(n)));
            }
        }

        if let Some(mx) = max_index {
            context
                .event_queue
                .push(AppEvent::SelectSprite { index: mx });
        } else {
            context.event_queue.push(AppEvent::DeselectSprite);
        }

        EventContinueControl::STOP_PROPAGATION
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
    ) -> impl Iterator<Item = br::VkHandleRef<'x, br::vk::VkImageView>> + use<'x> {
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
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(%info, "application panic");
    }));

    tracing::info!("Initializing BaseSystem...");
    let setup_timer = std::time::Instant::now();

    #[cfg(unix)]
    let dbus =
        thirdparty::dbus::Connection::connect_bus(thirdparty::dbus::BusType::Session).unwrap();
    #[cfg(unix)]
    let mut dbus = DBusLink {
        con: RefCell::new(dbus),
    };

    let events = AppEventBus {
        queue: UnsafeCell::new(VecDeque::new()),
        #[cfg(target_os = "linux")]
        efd: platform::linux::EventFD::new(0, platform::linux::EventFDOptions::NONBLOCK).unwrap(),
        #[cfg(windows)]
        event_notify: platform::win32::event::EventObject::new(None, true, false).unwrap(),
    };

    let subsystem = Subsystem::init();
    let mut app_system = AppBaseSystem::new(&subsystem);
    let mut app_shell = AppShell::new(&events, &mut app_system as _);
    let mut app_state = RefCell::new(AppState::new());

    #[cfg(target_os = "linux")]
    unsafe {
        DBUS_WAIT_FOR_REPLY_WAKERS = &mut HashMap::new() as *mut _;
        DBUS_WAIT_FOR_SIGNAL_WAKERS = &mut HashMap::new() as *mut _;
    }
    let bg_worker = BackgroundWorker::new();
    let task_worker = smol::LocalExecutor::new();

    app_system.rescale_fonts(app_shell.ui_scale_factor());

    let elapsed = setup_timer.elapsed();
    tracing::info!(?elapsed, "Initializing BaseSystem done!");

    app_main(
        &mut app_system,
        &mut app_shell,
        &events,
        &mut app_state,
        &task_worker,
        &bg_worker,
        #[cfg(unix)]
        &mut dbus,
    );

    bg_worker.teardown();
    drop(task_worker);
}

fn app_main<'sys, 'event_bus, 'subsystem>(
    app_system: &'sys mut AppBaseSystem<'subsystem>,
    app_shell: &'sys mut AppShell<'event_bus, 'subsystem>,
    events: &'event_bus AppEventBus,
    app_state: &'sys mut RefCell<AppState<'subsystem>>,
    task_worker: &smol::LocalExecutor<'sys>,
    bg_worker: &BackgroundWorker<'subsystem>,
    #[cfg(unix)] dbus: &'sys mut DBusLink,
) {
    tracing::info!("Initializing Peridot SpriteAtlas Visualizer/Editor");
    let setup_timer = std::time::Instant::now();

    let staging_scratch_buffers = Arc::new(RwLock::new(BufferedStagingScratchBuffer::new(
        &app_system.subsystem,
        2,
    )));

    let client_size = Cell::new(app_shell.client_size());

    let mut sc = PrimaryRenderTarget::new(SubsystemBoundSurface {
        handle: unsafe {
            app_shell
                .create_vulkan_surface(app_system.subsystem.instance())
                .unwrap()
        },
        subsystem: app_system.subsystem,
    });

    let mut composite_backdrop_buffers =
        Vec::<br::ImageViewObject<br::ImageObject<&Subsystem>>>::with_capacity(16);
    let mut composite_backdrop_buffer_memory = br::DeviceMemoryObject::new(
        app_system.subsystem,
        &br::MemoryAllocateInfo::new(10, app_system.find_device_local_memory_index(!0).unwrap()),
    )
    .unwrap();
    let mut composite_backdrop_blur_destination_fbs = Vec::with_capacity(16);
    let mut composite_backdrop_buffers_invalidated = true;

    let mut composite_grab_buffer = br::ImageObject::new(
        app_system.subsystem,
        &br::ImageCreateInfo::new(sc.size, sc.color_format())
            .sampled()
            .transfer_dest(),
    )
    .unwrap();
    let mut composite_grab_buffer_memory = app_system
        .alloc_device_local_memory_for_requirements(&composite_grab_buffer.requirements());
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
        app_system.subsystem,
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
        app_system.subsystem,
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
        app_system.subsystem,
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
        app_system.subsystem,
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
        app_system.subsystem,
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
                app_system.subsystem,
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
                app_system.subsystem,
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
                app_system.subsystem,
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
                app_system.subsystem,
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
            app_system.subsystem,
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
    let mut blur_temporal_buffer_memory =
        app_system.alloc_device_local_memory(top, memory_index_mask);
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
                app_system.subsystem,
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
                app_system.subsystem,
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
        br::SamplerObject::new(app_system.subsystem, &br::SamplerCreateInfo::new()).unwrap();

    let composite_vsh = app_system.require_shader("resources/composite.vert");
    let composite_fsh = app_system.require_shader("resources/composite.frag");
    let composite_shader_stages = [
        composite_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
        composite_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
    ];
    let composite_backdrop_blur_downsample_vsh =
        app_system.require_shader("resources/dual_kawase_filter/downsample.vert");
    let composite_backdrop_blur_downsample_fsh =
        app_system.require_shader("resources/dual_kawase_filter/downsample.frag");
    let composite_backdrop_blur_upsample_vsh =
        app_system.require_shader("resources/dual_kawase_filter/upsample.vert");
    let composite_backdrop_blur_upsample_fsh =
        app_system.require_shader("resources/dual_kawase_filter/upsample.frag");
    let composite_backdrop_blur_downsample_stages = [
        composite_backdrop_blur_downsample_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
        composite_backdrop_blur_downsample_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
    ];
    let composite_backdrop_blur_upsample_stages = [
        composite_backdrop_blur_upsample_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
        composite_backdrop_blur_upsample_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
    ];

    let composite_descriptor_layout = br::DescriptorSetLayoutObject::new(
        app_system.subsystem,
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
        app_system.subsystem,
        &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
            .make_binding(0, 1)
            .only_for_fragment()]),
    )
    .unwrap();
    let composite_backdrop_blur_input_descriptor_layout = br::DescriptorSetLayoutObject::new(
        app_system.subsystem,
        &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
            .make_binding(0, 1)
            .only_for_fragment()]),
    )
    .unwrap();
    let mut descriptor_pool = br::DescriptorPoolObject::new(
        app_system.subsystem,
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
                app_system
                    .composite_instance_manager
                    .buffer_transparent_ref(),
                0..(core::mem::size_of::<CompositeInstanceData>() * 1024) as _,
            ),
        ),
        composite_alphamask_group_descriptor.binding_at(1).write(
            br::DescriptorContents::uniform_buffer(
                app_system
                    .composite_instance_manager
                    .streaming_buffer_transparent_ref(),
                0..core::mem::size_of::<CompositeStreamingData>() as _,
            ),
        ),
        composite_alphamask_group_descriptor.binding_at(2).write(
            br::DescriptorContents::CombinedImageSampler(vec![
                br::DescriptorImageInfo::new(
                    app_system.mask_atlas_resource_transparent_ref(),
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
    app_system
        .subsystem
        .update_descriptor_sets(&descriptor_writes, &[]);

    let mut composite_backdrop_buffer_descriptor_pool = br::DescriptorPoolObject::new(
        app_system.subsystem,
        &br::DescriptorPoolCreateInfo::new(
            16,
            &[br::DescriptorType::CombinedImageSampler.make_size(16)],
        ),
    )
    .unwrap();
    let mut composite_backdrop_buffer_descriptor_sets = Vec::<br::DescriptorSet>::with_capacity(16);
    let mut composite_backdrop_buffer_descriptor_pool_capacity = 16;

    let composite_pipeline_layout = br::PipelineLayoutObject::new(
        app_system.subsystem,
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
        app_system.subsystem,
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
    ] = app_system
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
            .set_multisample_state(MS_STATE_EMPTY),
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
            .set_multisample_state(MS_STATE_EMPTY),
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
            .set_multisample_state(MS_STATE_EMPTY),
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
            .set_multisample_state(MS_STATE_EMPTY),
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
    let mut blur_downsample_pipelines = app_system
        .subsystem
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
                    .set_multisample_state(MS_STATE_EMPTY)
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let mut blur_upsample_pipelines = app_system
        .subsystem
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
                    .set_multisample_state(MS_STATE_EMPTY)
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();

    let mut staging_scratch_buffer_locked =
        parking_lot::RwLockWriteGuard::map(staging_scratch_buffers.write(), |x| {
            x.active_buffer_mut()
        });
    tracing::info!(value = app_shell.ui_scale_factor(), "initial ui scale");
    let mut init_context = PresenterInitContext {
        for_view: ViewInitContext {
            base_system: app_system,
            staging_scratch_buffer: &mut staging_scratch_buffer_locked,
            ui_scale_factor: app_shell.ui_scale_factor(),
        },
        app_state: app_state.get_mut(),
    };

    let editing_atlas_renderer = Rc::new(RefCell::new(EditingAtlasRenderer::new(
        init_context.for_view.base_system,
        main_rp_final.subpass(0),
        sc.size,
        SizePixels {
            width: 32,
            height: 32,
        },
    )));
    let mut editing_atlas_current_bound_pipeline = RenderPassRequirements {
        after_operation: RenderPassAfterOperation::None,
        continued: false,
    };
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
    let mut sprite_list_pane = SpriteListPanePresenter::new(&mut init_context, app_header.height());
    let app_menu = feature::app_menu::Presenter::new(&mut init_context, app_header.height());

    let current_selected_sprite_marker_view = Rc::new(CurrentSelectedSpriteMarkerView::new(
        &mut init_context.for_view,
    ));

    drop(init_context);
    drop(staging_scratch_buffer_locked);

    current_selected_sprite_marker_view.mount(CompositeTree::ROOT, &mut app_system.composite_tree);
    sprite_list_pane.mount(app_system, CompositeTree::ROOT, HitTestTreeManager::ROOT);
    app_menu.mount(app_system, CompositeTree::ROOT, HitTestTreeManager::ROOT);
    app_header.mount(app_system, CompositeTree::ROOT, HitTestTreeManager::ROOT);

    let popup_hit_layer = app_system.create_hit_tree(HitTestTreeData {
        width_adjustment_factor: 1.0,
        height_adjustment_factor: 1.0,
        ..Default::default()
    });
    app_system.set_hit_tree_parent(popup_hit_layer, HitTestTreeManager::ROOT);

    editing_atlas_renderer
        .borrow_mut()
        .set_offset(0.0, app_header.height() * app_shell.ui_scale_factor());

    let ht_root_fallback_action_handler = Rc::new(HitTestRootTreeActionHandler::new(
        &editing_atlas_renderer,
        &current_selected_sprite_marker_view,
        None,
    ));
    app_system
        .hit_tree
        .set_action_handler(HitTestTreeManager::ROOT, &ht_root_fallback_action_handler);

    app_state.get_mut().register_sprites_view_feedback({
        let ht_root_fallback_action_handler = Rc::downgrade(&ht_root_fallback_action_handler);
        let editing_atlas_renderer = Rc::downgrade(&editing_atlas_renderer);
        let bg_worker = bg_worker.enqueue_access().downgrade();
        let mut last_selected_index = None;
        let staging_scratch_buffers = Arc::downgrade(&staging_scratch_buffers);
        let current_selected_sprite_marker_view =
            Rc::downgrade(&current_selected_sprite_marker_view);

        move |sprites| {
            let Some(ht_root_fallback_action_handler) = ht_root_fallback_action_handler.upgrade()
            else {
                // app teardown-ed
                return;
            };
            let Some(editing_atlas_renderer) = editing_atlas_renderer.upgrade() else {
                // app teardown-ed
                return;
            };
            let Some(bg_worker) = bg_worker.upgrade() else {
                // app teardown-ed
                return;
            };
            let Some(marker_view) = current_selected_sprite_marker_view.upgrade() else {
                // app teardown-ed
                return;
            };

            while ht_root_fallback_action_handler
                .sprite_rects_cached
                .borrow()
                .len()
                > sprites.len()
            {
                // 削除分
                let n = ht_root_fallback_action_handler
                    .sprite_rects_cached
                    .borrow()
                    .len()
                    - 1;
                let old = ht_root_fallback_action_handler
                    .sprite_rects_cached
                    .borrow_mut()
                    .pop()
                    .unwrap();
                let (index, level) = QuadTree::rect_index_and_level(old.0, old.1, old.2, old.3);

                ht_root_fallback_action_handler
                    .sprites_qt
                    .borrow_mut()
                    .element_index_for_region[level][index as usize]
                    .remove(&n);
            }
            for (n, (old, new)) in ht_root_fallback_action_handler
                .sprite_rects_cached
                .borrow_mut()
                .iter_mut()
                .zip(sprites.iter())
                .enumerate()
            {
                // 移動分
                if old.0 == new.left
                    && old.1 == new.top
                    && old.2 == new.right()
                    && old.3 == new.bottom()
                {
                    // 座標変化なし
                    continue;
                }

                let (old_index, old_level) =
                    QuadTree::rect_index_and_level(old.0, old.1, old.2, old.3);
                let (new_index, new_level) =
                    QuadTree::rect_index_and_level(new.left, new.top, new.right(), new.bottom());
                *old = (new.left, new.top, new.right(), new.bottom());

                if old_level == new_level && old_index == new_index {
                    // 所属ブロックに変化なし
                    continue;
                }

                ht_root_fallback_action_handler
                    .sprites_qt
                    .borrow_mut()
                    .element_index_for_region[old_level][old_index as usize]
                    .remove(&n);
                ht_root_fallback_action_handler
                    .sprites_qt
                    .borrow_mut()
                    .bind(new_level, new_index, n);
            }
            let new_base = ht_root_fallback_action_handler
                .sprite_rects_cached
                .borrow()
                .len();
            for (n, new) in sprites.iter().enumerate().skip(new_base) {
                // 追加分
                let (index, level) =
                    QuadTree::rect_index_and_level(new.left, new.top, new.right(), new.bottom());
                ht_root_fallback_action_handler
                    .sprites_qt
                    .borrow_mut()
                    .bind(level, index, n);
                ht_root_fallback_action_handler
                    .sprite_rects_cached
                    .borrow_mut()
                    .push((new.left, new.top, new.right(), new.bottom()));
            }

            editing_atlas_renderer.borrow().update_sprites(
                sprites,
                &bg_worker,
                &staging_scratch_buffers,
            );

            // TODO: Model的には複数選択できる形にしてるけどViewはどうしようか......
            let selected_index = sprites.iter().position(|x| x.selected);
            if selected_index != last_selected_index {
                last_selected_index = selected_index;
                if let Some(x) = selected_index {
                    marker_view.focus(
                        sprites[x].left as _,
                        sprites[x].top as _,
                        sprites[x].width as _,
                        sprites[x].height as _,
                    );
                } else {
                    marker_view.hide();
                }
            }
        }
    });

    {
        let mut staging_scratch_buffer_locked =
            parking_lot::RwLockWriteGuard::map(staging_scratch_buffers.write(), |x| {
                x.active_buffer_mut()
            });

        tracing::debug!(
            byte_size = staging_scratch_buffer_locked.total_reserved_amount(),
            "Reserved Staging Buffers during UI initialization",
        );
        staging_scratch_buffer_locked.reset();
        app_system.hit_tree.dump(HitTestTreeManager::ROOT);
    }

    let mut main_cp = br::CommandPoolObject::new(
        app_system.subsystem,
        &br::CommandPoolCreateInfo::new(app_system.subsystem.graphics_queue_family_index),
    )
    .unwrap();
    let mut main_cbs = br::CommandBufferObject::alloc(
        app_system.subsystem,
        &br::CommandBufferAllocateInfo::new(
            &mut main_cp,
            sc.backbuffer_count() as _,
            br::CommandBufferLevel::Primary,
        ),
    )
    .unwrap();
    let mut main_cb_invalid = true;

    let mut update_cp = br::CommandPoolObject::new(
        app_system.subsystem,
        &br::CommandPoolCreateInfo::new(app_system.subsystem.graphics_queue_family_index),
    )
    .unwrap();
    let [mut update_cb] = br::CommandBufferObject::alloc_array(
        app_system.subsystem,
        &br::CommandBufferFixedCountAllocateInfo::new(
            &mut update_cp,
            br::CommandBufferLevel::Primary,
        ),
    )
    .unwrap();

    let mut acquire_completion =
        br::SemaphoreObject::new(app_system.subsystem, &br::SemaphoreCreateInfo::new()).unwrap();
    acquire_completion
        .set_name(Some(c"acquire_completion"))
        .unwrap();
    let render_completion_per_backbuffer = (0..sc.backbuffer_count())
        .map(|n| {
            let o = br::SemaphoreObject::new(app_system.subsystem, &br::SemaphoreCreateInfo::new())
                .unwrap();
            o.set_name(Some(&unsafe {
                std::ffi::CString::from_vec_unchecked(format!("render_completion#{n}").into_bytes())
            }))
            .unwrap();
            o
        })
        .collect::<Vec<_>>();
    let mut last_render_command_fence =
        br::FenceObject::new(app_system.subsystem, &br::FenceCreateInfo::new(0)).unwrap();
    last_render_command_fence
        .set_name(Some(c"last_render_command_fence"))
        .unwrap();
    let mut last_rendering = false;
    let mut last_update_command_fence =
        br::FenceObject::new(app_system.subsystem, &br::FenceCreateInfo::new(0)).unwrap();
    last_update_command_fence
        .set_name(Some(c"last_update_command_fence"))
        .unwrap();
    let mut last_updating = false;

    app_state.get_mut().synchronize_view();
    app_shell.flush();

    #[cfg(target_os = "linux")]
    pub enum PollFDType {
        AppEventBus,
        AppShellDisplay,
        BackgroundWorkerViewFeedback,
        DBusWatch(*mut thirdparty::dbus::WatchRef),
    }
    #[cfg(target_os = "linux")]
    struct PollFDPool {
        types: Vec<PollFDType>,
        freelist: BTreeSet<u64>,
    }
    #[cfg(target_os = "linux")]
    impl PollFDPool {
        pub fn new() -> Self {
            Self {
                types: Vec::new(),
                freelist: BTreeSet::new(),
            }
        }

        pub fn alloc(&mut self, t: PollFDType) -> u64 {
            if let Some(x) = self.freelist.pop_first() {
                // use preallocated
                self.types[x as usize] = t;
                return x;
            }

            // allocate new one
            self.types.push(t);
            (self.types.len() - 1) as _
        }

        pub fn free(&mut self, x: u64) {
            self.freelist.insert(x);
        }

        pub fn get(&self, x: u64) -> Option<&PollFDType> {
            self.types.get(x as usize)
        }
    }

    #[cfg(target_os = "linux")]
    let poll_fd_pool = RefCell::new(PollFDPool::new());
    #[cfg(target_os = "linux")]
    let epoll = platform::linux::Epoll::new(0).unwrap();
    #[cfg(target_os = "linux")]
    epoll
        .add(
            &events.efd,
            platform::linux::EPOLLIN,
            platform::linux::EpollData::U64(
                poll_fd_pool.borrow_mut().alloc(PollFDType::AppEventBus),
            ),
        )
        .unwrap();
    #[cfg(target_os = "linux")]
    epoll
        .add(
            &app_shell.display_fd(),
            platform::linux::EPOLLIN,
            platform::linux::EpollData::U64(
                poll_fd_pool.borrow_mut().alloc(PollFDType::AppShellDisplay),
            ),
        )
        .unwrap();
    #[cfg(target_os = "linux")]
    epoll
        .add(
            bg_worker.main_thread_waker(),
            platform::linux::EPOLLIN,
            platform::linux::EpollData::U64(
                poll_fd_pool
                    .borrow_mut()
                    .alloc(PollFDType::BackgroundWorkerViewFeedback),
            ),
        )
        .unwrap();

    #[cfg(target_os = "linux")]
    struct DBusWatcher<'e> {
        epoll: &'e platform::linux::Epoll,
        fd_pool: &'e RefCell<PollFDPool>,
        fd_to_pool_index: HashMap<core::ffi::c_int, u64>,
    }
    #[cfg(target_os = "linux")]
    impl thirdparty::dbus::WatchFunction for DBusWatcher<'_> {
        fn add(&mut self, watch: &mut thirdparty::dbus::WatchRef) -> bool {
            if watch.enabled() {
                let mut event_type = 0;
                let flags = watch.flags();
                if flags.contains(thirdparty::dbus::WatchFlags::READABLE) {
                    event_type |= platform::linux::EPOLLIN;
                }
                if flags.contains(thirdparty::dbus::WatchFlags::WRITABLE) {
                    event_type |= platform::linux::EPOLLOUT;
                }

                let pool_index = self
                    .fd_pool
                    .borrow_mut()
                    .alloc(PollFDType::DBusWatch(watch as *mut _));

                self.epoll
                    .add(
                        &watch.as_raw_fd(),
                        event_type,
                        platform::linux::EpollData::U64(pool_index),
                    )
                    .unwrap();
                self.fd_to_pool_index.insert(watch.as_raw_fd(), pool_index);
            }

            true
        }

        fn remove(&mut self, watch: &mut thirdparty::dbus::WatchRef) {
            let Some(pool_index) = self.fd_to_pool_index.remove(&watch.as_raw_fd()) else {
                // not bound
                return;
            };

            match self.epoll.del(&watch.as_raw_fd()) {
                // ENOENTは無視
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => {
                    tracing::error!(reason = ?e, "Failed to remove dbus watch");
                }
                Ok(_) => (),
            }

            self.fd_pool.borrow_mut().free(pool_index);
        }

        fn toggled(&mut self, watch: &mut thirdparty::dbus::WatchRef) {
            let mut event_type = 0;
            let flags = watch.flags();
            if flags.contains(thirdparty::dbus::WatchFlags::READABLE) {
                event_type |= platform::linux::EPOLLIN;
            }
            if flags.contains(thirdparty::dbus::WatchFlags::WRITABLE) {
                event_type |= platform::linux::EPOLLOUT;
            }

            if watch.enabled() {
                let pool_index = self
                    .fd_pool
                    .borrow_mut()
                    .alloc(PollFDType::DBusWatch(watch as *mut _));

                self.epoll
                    .add(
                        &watch.as_raw_fd(),
                        event_type,
                        platform::linux::EpollData::U64(pool_index),
                    )
                    .unwrap();
                self.fd_to_pool_index.insert(watch.as_raw_fd(), pool_index);
            } else {
                let Some(pool_index) = self.fd_to_pool_index.remove(&watch.as_raw_fd()) else {
                    // not bound
                    return;
                };

                match self.epoll.del(&watch.as_raw_fd()) {
                    // ENOENTは無視
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => {
                        tracing::error!(reason = ?e, "Failed to remove dbus watch");
                    }
                    Ok(_) => (),
                }

                self.fd_pool.borrow_mut().free(pool_index);
            }
        }
    }

    #[cfg(target_os = "linux")]
    dbus.con
        .get_mut()
        .set_watch_functions(Box::new(DBusWatcher {
            epoll: &epoll,
            fd_pool: &poll_fd_pool,
            fd_to_pool_index: HashMap::new(),
        }));

    let elapsed = setup_timer.elapsed();
    tracing::info!(?elapsed, "App Setup done!");

    // initial post event
    events.push(AppEvent::ToplevelWindowFrameTiming);

    let t = std::time::Instant::now();
    let mut frame_resize_request = None;
    let mut last_pointer_pos = (0.0f32, 0.0f32);
    let mut last_composite_render_instructions = CompositeRenderingData {
        instructions: Vec::new(),
        render_passes: Vec::new(),
        required_backdrop_buffer_count: 0,
    };
    let mut composite_instance_buffer_dirty = false;
    let mut popups = HashMap::<uuid::Uuid, uikit::message_dialog::Presenter>::new();
    #[cfg(target_os = "linux")]
    let mut epoll_events =
        [const { core::mem::MaybeUninit::<platform::linux::epoll_event>::uninit() }; 8];
    let mut app_update_context = AppUpdateContext {
        event_queue: &events,
        state: &app_state,
        ui_scale_factor: app_shell.ui_scale_factor(),
    };
    'app: loop {
        #[cfg(target_os = "linux")]
        {
            app_shell.prepare_read_events().unwrap();
            let wake_count = epoll.wait(&mut epoll_events, None).unwrap();
            let mut shell_event_processed = false;
            for e in &epoll_events[..wake_count] {
                let e = unsafe { e.assume_init_ref() };

                match poll_fd_pool.borrow().get(unsafe { e.data.u64 }) {
                    Some(&PollFDType::AppEventBus) => {
                        // app event
                    }
                    Some(&PollFDType::AppShellDisplay) => {
                        // display event
                        app_shell.read_and_process_events().unwrap();
                        shell_event_processed = true;
                    }
                    Some(&PollFDType::BackgroundWorkerViewFeedback) => {
                        // 先にclearする
                        match bg_worker.clear_view_feedback_notification() {
                            Ok(_) => (),
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
                            Err(e) => {
                                tracing::warn!(reason = ?e, "Failed to clear bg worker view feedback notification signal")
                            }
                        }

                        while let Some(vf) = bg_worker.try_pop_view_feedback() {
                            match vf {
                                BackgroundWorkerViewFeedback::BeginWork(thread_number, message) => {
                                    app_update_context.event_queue.push(
                                        AppEvent::BeginBackgroundWork {
                                            thread_number,
                                            message,
                                        },
                                    )
                                }
                                BackgroundWorkerViewFeedback::EndWork(thread_number) => {
                                    app_update_context
                                        .event_queue
                                        .push(AppEvent::EndBackgroundWork { thread_number })
                                }
                            }
                        }
                    }
                    Some(&PollFDType::DBusWatch(watch_ptr)) => {
                        let watch_ptr = unsafe { &mut *watch_ptr };
                        let mut flags = thirdparty::dbus::WatchFlags::empty();
                        if (e.events & platform::linux::EPOLLIN) != 0 {
                            flags |= thirdparty::dbus::WatchFlags::READABLE;
                        }
                        if (e.events & platform::linux::EPOLLOUT) != 0 {
                            flags |= thirdparty::dbus::WatchFlags::WRITABLE;
                        }
                        if (e.events & platform::linux::EPOLLERR) != 0 {
                            flags |= thirdparty::dbus::WatchFlags::ERROR;
                        }
                        if (e.events & platform::linux::EPOLLHUP) != 0 {
                            flags |= thirdparty::dbus::WatchFlags::HANGUP;
                        }
                        if !watch_ptr.handle(flags) {
                            tracing::warn!(?flags, "dbus_watch_handle failed");
                        }
                    }
                    // ignore
                    None => (),
                }
            }
            if !shell_event_processed {
                app_shell.cancel_read_events();
            }
        }
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::WAIT_TIMEOUT;

            let r = unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    MSG_WAIT_FOR_MULTIPLE_OBJECTS_EX_FLAGS, QS_ALLINPUT,
                };

                windows::Win32::UI::WindowsAndMessaging::MsgWaitForMultipleObjectsEx(
                    Some(&[
                        events.event_notify.handle(),
                        bg_worker.main_thread_waker().handle(),
                    ]),
                    app_shell.next_frame_left_ms() as _,
                    QS_ALLINPUT,
                    MSG_WAIT_FOR_MULTIPLE_OBJECTS_EX_FLAGS(0),
                )
            };

            // とりあえず全部処理しておく

            match bg_worker.clear_view_feedback_notification() {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
                Err(e) => {
                    tracing::warn!(reason = ?e, "Failed to clear bg worker view feedback notification signal")
                }
            }

            while let Some(vf) = bg_worker.try_pop_view_feedback() {
                match vf {
                    BackgroundWorkerViewFeedback::BeginWork(thread_number, message) => {
                        app_update_context
                            .event_queue
                            .push(AppEvent::BeginBackgroundWork {
                                thread_number,
                                message,
                            })
                    }
                    BackgroundWorkerViewFeedback::EndWork(thread_number) => app_update_context
                        .event_queue
                        .push(AppEvent::EndBackgroundWork { thread_number }),
                }
            }

            app_shell.process_pending_events();

            if r.0 == WAIT_TIMEOUT.0 {
                // timeout
                events.push(AppEvent::ToplevelWindowFrameTiming);
            }
        }

        #[cfg(target_os = "linux")]
        while let Some(m) = dbus.underlying_mut().pop_message() {
            println!("!! dbus msg recv: {}", m.r#type());
            if m.r#type() == thirdparty::dbus::MESSAGE_TYPE_METHOD_RETURN {
                // method return
                println!(
                    "!! method return data: {} {:?}",
                    m.reply_serial(),
                    m.signature()
                );

                if let Some(wakers) =
                    unsafe { &mut *DBUS_WAIT_FOR_REPLY_WAKERS }.remove(&m.reply_serial())
                {
                    let wake_count = wakers.len();
                    for ((sink, w), m) in
                        wakers.into_iter().zip(core::iter::repeat_n(m, wake_count))
                    {
                        let Some(sink1) = sink.upgrade() else {
                            // abandoned
                            continue;
                        };

                        sink1.set(Some(m));
                        drop(sink); // drop before wake(unchain weak ref)
                        w.wake();
                    }
                }
            } else if m.r#type() == thirdparty::dbus::MESSAGE_TYPE_SIGNAL {
                // signal
                println!(
                    "!! signal data: {:?} {:?} {:?}",
                    m.path(),
                    m.interface(),
                    m.member()
                );

                let path = m.path().unwrap();
                let interface = m.interface().unwrap();
                let member = m.member().unwrap();

                if let Some(wakers) = unsafe { &mut *DBUS_WAIT_FOR_SIGNAL_WAKERS }.remove(&(
                    path.into(),
                    interface.into(),
                    member.into(),
                )) {
                    let wake_count = wakers.len();
                    for ((sink, w), m) in
                        wakers.into_iter().zip(core::iter::repeat_n(m, wake_count))
                    {
                        let Some(sink1) = sink.upgrade() else {
                            // abandoned
                            continue;
                        };

                        sink1.set(Some(m));
                        drop(sink); // drop before wake(unchain weak ref)
                        w.wake();
                    }
                }
            }
        }
        task_worker.try_tick();
        while let Some(e) = app_update_context.event_queue.pop() {
            match e {
                AppEvent::ToplevelWindowClose => {
                    app_shell.close_safe();
                    break 'app;
                }
                AppEvent::ToplevelWindowMinimizeRequest => {
                    app_shell.minimize();
                }
                AppEvent::ToplevelWindowMaximizeRequest => {
                    app_shell.maximize();
                }
                AppEvent::ToplevelWindowFrameTiming => {
                    let current_t = t.elapsed();
                    let current_sec = current_t.as_secs_f32();

                    if last_rendering {
                        last_render_command_fence.wait().unwrap();
                        last_render_command_fence.reset().unwrap();
                        last_rendering = false;
                    }

                    current_selected_sprite_marker_view
                        .update(&mut app_system.composite_tree, current_sec);
                    app_header.update(&mut app_system.composite_tree, current_sec);
                    app_menu.update(app_system, current_sec);
                    sprite_list_pane.update(
                        app_system,
                        current_sec,
                        &mut staging_scratch_buffers.write().active_buffer_mut(),
                    );
                    for p in popups.values() {
                        p.update(&mut app_system.composite_tree, current_sec);
                    }

                    {
                        // もろもろの判定がめんどいのでいったん毎回updateする
                        let n = app_system
                            .composite_instance_manager
                            .staging_memory_raw_handle();
                        let r = app_system.composite_instance_manager.range_all();
                        let flush_required = app_system
                            .composite_instance_manager
                            .memory_stg_requires_explicit_flush();
                        let ptr = unsafe {
                            app_system
                                .composite_instance_manager
                                .map_staging(&app_system.subsystem)
                                .unwrap()
                        };
                        let composite_render_instructions = unsafe {
                            app_system.composite_tree.update(
                                sc.size,
                                current_t.as_secs_f32(),
                                app_system.atlas.vk_extent(),
                                ptr.ptr(),
                                &events,
                            )
                        };
                        if flush_required {
                            unsafe {
                                app_system
                                    .subsystem
                                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                                        n, 0, r.end as _,
                                    )])
                                    .unwrap();
                            }
                        }
                        drop(ptr);

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
                                        app_system.subsystem,
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
                        || editing_atlas_renderer.borrow().is_dirty();

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
                                app_system.subsystem,
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
                            app_system.find_device_local_memory_index(memory_index_mask)
                        else {
                            tracing::error!(
                                memory_index_mask,
                                "no suitable memory for composition backdrop buffers"
                            );
                            std::process::exit(1);
                        };
                        composite_backdrop_buffer_memory = br::DeviceMemoryObject::new(
                            app_system.subsystem,
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
                                    app_system.subsystem,
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

                        app_system.subsystem.update_descriptor_sets(
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

                        let mut staging_scratch_buffers_locked = staging_scratch_buffers.write();

                        last_update_command_fence.reset().unwrap();
                        unsafe {
                            update_cp.reset(br::CommandPoolResetFlags::EMPTY).unwrap();
                        }
                        let rec = unsafe {
                            update_cb
                                .begin(&br::CommandBufferBeginInfo::new(), &app_system.subsystem)
                                .unwrap()
                        };
                        let rec = if composite_instance_buffer_dirty {
                            app_system.composite_instance_manager.sync_buffer(rec)
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
                            editing_atlas_renderer.borrow_mut().process_dirty_data(
                                &staging_scratch_buffers_locked.active_buffer(),
                                r,
                            )
                        })
                        .end()
                        .unwrap();

                        staging_scratch_buffers_locked.flip_next_and_ready();
                    }

                    let n = app_system
                        .composite_instance_manager
                        .streaming_memory_raw_handle();
                    let flush_required = app_system
                        .composite_instance_manager
                        .streaming_memory_requires_flush();
                    let mapped = unsafe {
                        app_system
                            .composite_instance_manager
                            .map_streaming(&app_system.subsystem)
                            .unwrap()
                    };
                    unsafe {
                        core::ptr::write(&mut (*mapped.ptr()).current_sec, current_t.as_secs_f32());
                    }
                    if flush_required {
                        unsafe {
                            app_system
                                .subsystem
                                .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                                    n,
                                    0,
                                    core::mem::size_of::<CompositeStreamingData>() as _,
                                )])
                                .unwrap();
                        }
                    }
                    drop(mapped);

                    if needs_update {
                        app_system
                            .subsystem
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
                        if last_composite_render_instructions.render_passes[0]
                            != editing_atlas_current_bound_pipeline
                        {
                            editing_atlas_current_bound_pipeline =
                                last_composite_render_instructions.render_passes[0];
                            editing_atlas_renderer.borrow_mut().recreate(
                                &app_system.subsystem,
                                match (
                                    editing_atlas_current_bound_pipeline.after_operation,
                                    editing_atlas_current_bound_pipeline.continued,
                                ) {
                                    (RenderPassAfterOperation::None, false) => {
                                        main_rp_final.subpass(0)
                                    }
                                    (RenderPassAfterOperation::None, true) => {
                                        main_rp_continue_final.subpass(0)
                                    }
                                    (RenderPassAfterOperation::Grab, false) => {
                                        main_rp_grabbed.subpass(0)
                                    }
                                    (RenderPassAfterOperation::Grab, true) => {
                                        main_rp_continue_grabbed.subpass(0)
                                    }
                                },
                                sc.size,
                            );
                        }

                        for (n, cb) in main_cbs.iter_mut().enumerate() {
                            let (first_rp, first_fb) =
                                match last_composite_render_instructions.render_passes[0] {
                                    RenderPassRequirements {
                                        continued: true, ..
                                    } => unreachable!("cannot continue at first"),
                                    RenderPassRequirements {
                                        after_operation: RenderPassAfterOperation::Grab,
                                        continued: false,
                                    } => (&main_rp_grabbed, &main_grabbed_fbs[n]),
                                    RenderPassRequirements {
                                        after_operation: RenderPassAfterOperation::None,
                                        continued: false,
                                    } => (&main_rp_final, &main_final_fbs[n]),
                                };

                            unsafe {
                                cb.begin(&br::CommandBufferBeginInfo::new(), &app_system.subsystem)
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
                                editing_atlas_renderer
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

                                                let (rp, fb) = match last_composite_render_instructions.render_passes[rpt_pointer] {
                                                    RenderPassRequirements { continued: false, .. } => unreachable!("not at first(must be continued)"),
                                                    RenderPassRequirements { continued: true, after_operation: RenderPassAfterOperation::Grab } => {
                                                        (&main_rp_continue_grabbed, &main_continue_grabbed_fbs[n])
                                                    }
                                                    RenderPassRequirements { continued: true, after_operation: RenderPassAfterOperation::None } => {
                                                        (&main_rp_continue_final, &main_continue_final_fbs[n])
                                                    }
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
                                                        match last_composite_render_instructions.render_passes[rpt_pointer] {
                                                            RenderPassRequirements { continued: false, after_operation: RenderPassAfterOperation::Grab } => {
                                                                &composite_pipeline_grabbed
                                                            }
                                                            RenderPassRequirements { continued: false, after_operation: RenderPassAfterOperation::None } => {
                                                                &composite_pipeline_final
                                                            }
                                                            RenderPassRequirements { continued: true, after_operation: RenderPassAfterOperation::Grab } => {
                                                                &composite_pipeline_continue_grabbed
                                                            }
                                                            RenderPassRequirements { continued: true, after_operation: RenderPassAfterOperation::None } => {
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

                    let next = match sc.acquire_next(
                        None,
                        br::CompletionHandlerMut::Queue(
                            acquire_completion.as_transparent_ref_mut(),
                        ),
                    ) {
                        Ok(x) => x,
                        Err(e) if e == br::vk::VK_ERROR_OUT_OF_DATE_KHR => {
                            // TODO: OUT_OF_DATE handling(Waylandでもこれ出るようならこっちにリサイズ処理を寄せたほうがいいかもしれんね)
                            tracing::warn!("swapchain out of date");
                            continue;
                        }
                        Err(e) => Err(e).unwrap(),
                    };
                    app_system
                        .subsystem
                        .submit_graphics_works(
                            &[br::SubmitInfo2::new(
                                &[br::SemaphoreSubmitInfo::new(&acquire_completion)
                                    .on_color_attachment_output()],
                                &[br::CommandBufferSubmitInfo::new(&main_cbs[next as usize])],
                                &[br::SemaphoreSubmitInfo::new(
                                    &render_completion_per_backbuffer[next as usize],
                                )
                                .on_color_attachment_output()],
                            )],
                            Some(last_render_command_fence.as_transparent_ref_mut()),
                        )
                        .unwrap();
                    last_rendering = true;
                    match app_system.subsystem.queue_present(&br::PresentInfo::new(
                        &[render_completion_per_backbuffer[next as usize].as_transparent_ref()],
                        &[sc.as_transparent_ref()],
                        &[next],
                        &mut [br::vk::VkResult(0)],
                    )) {
                        Ok(_) => (),
                        Err(e) if e == br::vk::VK_ERROR_OUT_OF_DATE_KHR => {
                            tracing::warn!("swapchain out of date");
                            continue;
                        }
                        Err(e) => Err(e).unwrap(),
                    }

                    app_shell.request_next_frame();
                }
                AppEvent::ToplevelWindowConfigure { width, height } => {
                    tracing::trace!(width, height, "ToplevelWindowConfigure");
                    frame_resize_request = Some((width, height));
                }
                AppEvent::ToplevelWindowSurfaceConfigure { serial } => {
                    if let Some((w, h)) = frame_resize_request.take() {
                        if w != sc.size.width || h != sc.size.height {
                            tracing::trace!(w, h, last_rendering, "frame resize");

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
                                        app_system.subsystem,
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
                                        app_system.subsystem,
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
                                        app_system.subsystem,
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
                                        app_system.subsystem,
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
                                app_system.subsystem,
                                &br::ImageCreateInfo::new(sc.size, sc.color_format())
                                    .sampled()
                                    .transfer_dest(),
                            )
                            .unwrap();
                            composite_grab_buffer_memory = app_system
                                .alloc_device_local_memory_for_requirements(
                                    &composite_grab_buffer1.requirements(),
                                );
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
                                    app_system.subsystem,
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
                            blur_temporal_buffer_memory =
                                app_system.alloc_device_local_memory(top, memory_index_mask);
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
                                        app_system.subsystem,
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
                                            app_system.subsystem,
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
                            app_system
                                .subsystem
                                .update_descriptor_sets(&descriptor_writes, &[]);

                            let screen_viewport = [sc
                                .size
                                .into_rect(br::Offset2D::ZERO)
                                .make_viewport(0.0..1.0)];
                            let screen_scissor = [sc.size.into_rect(br::Offset2D::ZERO)];
                            let screen_viewport_state =
                                br::PipelineViewportStateCreateInfo::new_array(
                                    &screen_viewport,
                                    &screen_scissor,
                                );
                            let [
                                composite_pipeline_grabbed1,
                                composite_pipeline_final1,
                                composite_pipeline_continue_grabbed1,
                                composite_pipeline_continue_final1,
                            ] = app_system
                                .create_graphics_pipelines_array(&[
                                    br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp_grabbed.subpass(0),
                                        &composite_shader_stages,
                                        &composite_vinput,
                                        &composite_ia_state,
                                        &screen_viewport_state,
                                        &composite_raster_state,
                                        &composite_blend_state,
                                    )
                                    .set_multisample_state(MS_STATE_EMPTY),
                                    br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp_final.subpass(0),
                                        &composite_shader_stages,
                                        &composite_vinput,
                                        &composite_ia_state,
                                        &screen_viewport_state,
                                        &composite_raster_state,
                                        &composite_blend_state,
                                    )
                                    .set_multisample_state(MS_STATE_EMPTY),
                                    br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp_continue_grabbed.subpass(0),
                                        &composite_shader_stages,
                                        &composite_vinput,
                                        &composite_ia_state,
                                        &screen_viewport_state,
                                        &composite_raster_state,
                                        &composite_blend_state,
                                    )
                                    .set_multisample_state(MS_STATE_EMPTY),
                                    br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp_continue_final.subpass(0),
                                        &composite_shader_stages,
                                        &composite_vinput,
                                        &composite_ia_state,
                                        &screen_viewport_state,
                                        &composite_raster_state,
                                        &composite_blend_state,
                                    )
                                    .set_multisample_state(MS_STATE_EMPTY),
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
                            blur_downsample_pipelines = app_system
                                .subsystem
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
                                            .set_multisample_state(MS_STATE_EMPTY)
                                        })
                                        .collect::<Vec<_>>(),
                                )
                                .unwrap();
                            blur_upsample_pipelines = app_system
                                .subsystem
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
                                            .set_multisample_state(MS_STATE_EMPTY)
                                        })
                                        .collect::<Vec<_>>(),
                                )
                                .unwrap();

                            editing_atlas_renderer.borrow_mut().recreate(
                                &app_system.subsystem,
                                match editing_atlas_current_bound_pipeline {
                                    RenderPassRequirements {
                                        continued: false,
                                        after_operation: RenderPassAfterOperation::Grab,
                                    } => main_rp_grabbed.subpass(0),
                                    RenderPassRequirements {
                                        continued: false,
                                        after_operation: RenderPassAfterOperation::None,
                                    } => main_rp_final.subpass(0),
                                    RenderPassRequirements {
                                        continued: true,
                                        after_operation: RenderPassAfterOperation::Grab,
                                    } => main_rp_continue_grabbed.subpass(0),
                                    RenderPassRequirements {
                                        continued: true,
                                        after_operation: RenderPassAfterOperation::None,
                                    } => main_rp_continue_final.subpass(0),
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
                    app_update_context.ui_scale_factor = app_shell.ui_scale_factor();
                    let (cw, ch) = client_size.get();

                    unsafe { &mut *app_shell.pointer_input_manager.get() }.handle_mouse_move(
                        surface_x,
                        surface_y,
                        cw,
                        ch,
                        &mut app_system.hit_tree,
                        &mut app_update_context,
                        HitTestTreeManager::ROOT,
                    );
                    app_shell.set_cursor_shape(
                        enter_serial,
                        unsafe { &mut *app_shell.pointer_input_manager.get() }
                            .cursor_shape(&mut app_system.hit_tree, &mut app_update_context),
                    );

                    last_pointer_pos = (surface_x, surface_y);
                }
                AppEvent::MainWindowPointerLeftDown { enter_serial } => {
                    app_update_context.ui_scale_factor = app_shell.ui_scale_factor();
                    let (cw, ch) = client_size.get();

                    unsafe { &mut *app_shell.pointer_input_manager.get() }.handle_mouse_left_down(
                        &app_shell,
                        last_pointer_pos.0,
                        last_pointer_pos.1,
                        cw,
                        ch,
                        &mut app_system.hit_tree,
                        &mut app_update_context,
                        HitTestTreeManager::ROOT,
                    );
                    app_shell.set_cursor_shape(
                        enter_serial,
                        unsafe { &mut *app_shell.pointer_input_manager.get() }
                            .cursor_shape(&mut app_system.hit_tree, &mut app_update_context),
                    );
                }
                AppEvent::MainWindowPointerLeftUp { enter_serial } => {
                    app_update_context.ui_scale_factor = app_shell.ui_scale_factor();
                    let (cw, ch) = client_size.get();

                    unsafe { &mut *app_shell.pointer_input_manager.get() }.handle_mouse_left_up(
                        &app_shell,
                        last_pointer_pos.0,
                        last_pointer_pos.1,
                        cw,
                        ch,
                        &mut app_system.hit_tree,
                        &mut app_update_context,
                        HitTestTreeManager::ROOT,
                    );
                    app_shell.set_cursor_shape(
                        enter_serial,
                        unsafe { &mut *app_shell.pointer_input_manager.get() }
                            .cursor_shape(&mut app_system.hit_tree, &mut app_update_context),
                    );
                }
                AppEvent::UIMessageDialogRequest { content } => {
                    let mut staging_scratch_buffer_locked =
                        parking_lot::RwLockWriteGuard::map(staging_scratch_buffers.write(), |x| {
                            x.active_buffer_mut()
                        });
                    let id = uuid::Uuid::new_v4();
                    let p = uikit::message_dialog::Presenter::new(
                        &mut PresenterInitContext {
                            for_view: ViewInitContext {
                                base_system: app_system,
                                staging_scratch_buffer: &mut staging_scratch_buffer_locked,
                                ui_scale_factor: app_shell.ui_scale_factor(),
                            },
                            app_state: &mut *app_state.borrow_mut(),
                        },
                        id,
                        &content,
                    );
                    p.show(
                        app_system,
                        CompositeTree::ROOT,
                        popup_hit_layer,
                        t.elapsed().as_secs_f32(),
                    );

                    // TODO: ここでRECOMPUTE_POINTER_ENTER相当の処理をしないといけない(ポインタを動かさないかぎりEnter状態が続くのでマスクを貫通できる)
                    // クローズしたときも同じ

                    tracing::debug!(
                        byte_size = staging_scratch_buffer_locked.total_reserved_amount(),
                        "Reserved Staging Buffers during Popup UI",
                    );
                    staging_scratch_buffer_locked.reset();

                    popups.insert(id, p);
                }
                AppEvent::UIPopupClose { id } => {
                    if let Some(inst) = popups.get(&id) {
                        inst.hide(app_system, t.elapsed().as_secs_f32());
                    }
                }
                AppEvent::UIPopupUnmount { id } => {
                    if let Some(inst) = popups.remove(&id) {
                        inst.unmount(&mut app_system.composite_tree);
                    }
                }
                AppEvent::AppMenuToggle => {
                    app_state.borrow_mut().toggle_menu();
                }
                AppEvent::AppMenuRequestAddSprite => {
                    #[cfg(target_os = "linux")]
                    task_worker
                        .spawn(app_menu_on_add_sprite(dbus, app_shell, events, app_state))
                        .detach();
                    #[cfg(windows)]
                    task_worker
                        .spawn(app_menu_on_add_sprite(app_shell, app_state))
                        .detach();
                    #[cfg(not(any(target_os = "linux", windows)))]
                    events.push(AppEvent::UIMessageDialogRequest {
                        content: "[DEBUG] app_menu_on_add_sprite not implemented".into(),
                    });
                }
                AppEvent::BeginBackgroundWork {
                    thread_number,
                    message,
                } => {
                    tracing::trace!(thread_number, message, "TODO: BeginBackgroundWork");
                }
                AppEvent::EndBackgroundWork { thread_number } => {
                    tracing::trace!(thread_number, "TODO: EndBackgroundWork");
                }
                AppEvent::SelectSprite { index } => {
                    app_state.borrow_mut().select_sprite(index);
                }
                AppEvent::DeselectSprite => {
                    app_state.borrow_mut().deselect_sprite();
                }
            }
            app_update_context.event_queue.notify_clear().unwrap();
        }
    }

    if let Err(e) = unsafe { app_system.subsystem.wait() } {
        tracing::warn!(reason = ?e, "Error in waiting pending works before shutdown");
    }
}

#[cfg(target_os = "linux")]
struct DBusLink {
    con: RefCell<thirdparty::dbus::Connection>,
}
#[cfg(target_os = "linux")]
impl DBusLink {
    #[inline(always)]
    pub fn underlying_mut(&self) -> std::cell::RefMut<thirdparty::dbus::Connection> {
        self.con.borrow_mut()
    }

    pub async fn send(
        &self,
        mut msg: thirdparty::dbus::Message,
    ) -> Option<thirdparty::dbus::Message> {
        let Some(serial) = self.con.borrow_mut().send_with_serial(&mut msg) else {
            return None;
        };

        Some(DBusWaitForReplyFuture::new(serial).await)
    }
}

#[cfg(windows)]
async fn app_menu_on_add_sprite<'sys, 'subsystem>(
    shell: &'sys AppShell<'sys, 'subsystem>,
    app_state: &'sys RefCell<AppState<'subsystem>>,
) {
    let added_paths = shell.select_added_sprites().await;

    let mut added_sprites = Vec::with_capacity(added_paths.len());
    for path in added_paths {
        if path.is_dir() {
            // process all files in directory(rec)
            for entry in walkdir::WalkDir::new(&path)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if !path.is_file() {
                    // 自分自身を含むみたいなのでその場合は見逃す
                    continue;
                }

                let mut fs = std::fs::File::open(&path).unwrap();
                let Some(png_meta) = source_reader::png::Metadata::try_read(&mut fs) else {
                    // PNGじゃないのは一旦見逃す
                    continue;
                };

                added_sprites.push(SpriteInfo::new(
                    path.file_stem().unwrap().to_str().unwrap().into(),
                    path.to_path_buf(),
                    png_meta.width,
                    png_meta.height,
                ));
            }
        } else {
            let mut fs = std::fs::File::open(&path).unwrap();
            let png_meta = match source_reader::png::Metadata::try_read(&mut fs) {
                Some(x) => x,
                None => {
                    tracing::warn!(?path, "not a png?");
                    continue;
                }
            };

            added_sprites.push(SpriteInfo::new(
                path.file_stem().unwrap().to_str().unwrap().into(),
                path.to_path_buf(),
                png_meta.width,
                png_meta.height,
            ));
        }
    }

    app_state.borrow_mut().add_sprites(added_sprites);
}

#[cfg(target_os = "linux")]
async fn app_menu_on_add_sprite<'subsystem>(
    dbus: &DBusLink,
    shell: &AppShell<'_, 'subsystem>,
    events: &AppEventBus,
    app_state: &RefCell<AppState<'subsystem>>,
) {
    // TODO: これUIだして待つべきか？ローカルだからあんまり待たないような気もするが......
    let reply_msg = dbus
        .send(
            thirdparty::dbus::Message::new_method_call(
                Some(c"org.freedesktop.portal.Desktop"),
                c"/org/freedesktop/portal/desktop",
                Some(c"org.freedesktop.DBus.Introspectable"),
                c"Introspect",
            )
            .unwrap(),
        )
        .await
        .unwrap();
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
    if let Err(e) = thirdparty::dbus::introspect_document::read_toplevel(
        &mut quick_xml::Reader::from_str(doc),
        |_, ifname, r| {
            has_file_chooser = ifname.as_ref() == b"org.freedesktop.portal.FileChooser";

            thirdparty::dbus::introspect_document::skip_read_interface_tag_contents(r)
        },
    ) {
        tracing::warn!(reason = ?e, "Failed to parse introspection document from portal object");
    }

    if !has_file_chooser {
        // FileChooserなし

        events.push(AppEvent::UIMessageDialogRequest {
            content: String::from("org.freedesktop.portal.FileChooser not found"),
        });

        return;
    }

    let reply_msg = dbus
        .send({
            let mut msg = thirdparty::dbus::Message::new_method_call(
                Some(c"org.freedesktop.portal.Desktop"),
                c"/org/freedesktop/portal/desktop",
                Some(c"org.freedesktop.DBus.Properties"),
                c"Get",
            )
            .unwrap();
            let mut msg_args_appender = msg.iter_append();
            msg_args_appender.append_cstr(c"org.freedesktop.portal.FileChooser");
            msg_args_appender.append_cstr(c"version");
            drop(msg_args_appender);

            msg
        })
        .await
        .unwrap();
    if let Some(error) = reply_msg.try_get_error() {
        tracing::error!(reason = ?error, "FileChooser version get failed");

        return;
    }

    let mut reply_iter = reply_msg.iter();
    assert_eq!(reply_iter.arg_type(), b'v' as _);
    let mut content_iter = reply_iter.recurse();
    assert_eq!(content_iter.arg_type(), b'u' as _);
    let mut v = core::mem::MaybeUninit::<u32>::uninit();
    unsafe {
        content_iter.get_value_basic(v.as_mut_ptr() as _);
    }

    println!("AddSprite: file chooser found! version = {}", unsafe {
        v.assume_init()
    });

    let unique_name = dbus.underlying_mut().unique_name().unwrap().to_owned();
    println!("unique name: {unique_name:?}");
    let unique_name_portal_request_path = unique_name
        .to_str()
        .unwrap()
        .strip_prefix(':')
        .unwrap()
        .replace('.', "_");
    let dialog_token = uuid::Uuid::new_v4().as_simple().to_string();
    let request_object_path = std::ffi::CString::new(format!(
        "/org/freedesktop/portal/desktop/request/{unique_name_portal_request_path}/{dialog_token}"
    ))
    .unwrap();
    let mut signal_awaiter = DBusWaitForSignalFuture::new(
        request_object_path.clone(),
        c"org.freedesktop.portal.Request".into(),
        c"Response".into(),
    );

    let (exported_shell_handle, exported_shell) = 'optin_exported: {
        if let Some(mut x) = shell.try_export_toplevel() {
            struct Handler {
                handle: Option<std::ffi::CString>,
            }
            impl thirdparty::wl::ZxdgExportedV2EventListener for Handler {
                fn handle(
                    &mut self,
                    _sender: &mut thirdparty::wl::ZxdgExportedV2,
                    handle: &core::ffi::CStr,
                ) {
                    self.handle = Some(handle.into());
                }
            }
            let mut handler = Handler { handle: None };
            if let Err(e) = x.add_listener(&mut handler) {
                tracing::warn!(target = "ZxdgExportedV2", reason = ?e, "Failed to add listener");
                break 'optin_exported (c"".into(), None);
            }
            shell.sync();

            (handler.handle.unwrap_or_else(|| c"".into()), Some(x))
        } else {
            (c"".into(), None)
        }
    };

    let reply_msg = dbus
        .send({
            let mut msg = thirdparty::dbus::Message::new_method_call(
                Some(c"org.freedesktop.portal.Desktop"),
                c"/org/freedesktop/portal/desktop",
                Some(c"org.freedesktop.portal.FileChooser"),
                c"OpenFile",
            )
            .unwrap();
            let mut msg_args_appender = msg.iter_append();
            msg_args_appender.append_cstr(&exported_shell_handle);
            msg_args_appender.append_cstr(c"Add Sprite");
            let mut options_appender = msg_args_appender
                .open_container(thirdparty::dbus::TYPE_ARRAY, Some(c"{sv}"))
                .unwrap();
            let mut dict_appender = options_appender.open_dict_entry_container().unwrap();
            dict_appender.append_cstr(c"handle_token");
            dict_appender
                .append_variant_cstr(&std::ffi::CString::new(dialog_token.clone()).unwrap());
            dict_appender.close();
            let mut dict_appender = options_appender.open_dict_entry_container().unwrap();
            dict_appender.append_cstr(c"multiple");
            dict_appender.append_variant_bool(true);
            dict_appender.close();
            options_appender.close();

            msg
        })
        .await
        .unwrap();
    if let Some(error) = reply_msg.try_get_error() {
        tracing::error!(reason = ?error, "FileChooser.OpenFile failed");

        return;
    }

    let mut reply_iter = reply_msg.iter();
    assert_eq!(reply_iter.arg_type(), b'o' as _);
    let mut sp = core::mem::MaybeUninit::<*const core::ffi::c_char>::uninit();
    unsafe {
        reply_iter.get_value_basic(sp.as_mut_ptr() as _);
    }
    let open_file_dialog_handle = unsafe { core::ffi::CStr::from_ptr(sp.assume_init()) };
    println!("OpenFile dialog handle: {open_file_dialog_handle:?}");

    if open_file_dialog_handle != &request_object_path as &core::ffi::CStr {
        tracing::debug!(
            ?open_file_dialog_handle,
            ?request_object_path,
            "returned object_path did not match with the expected, switching request object..."
        );
        signal_awaiter = DBusWaitForSignalFuture::new(
            open_file_dialog_handle.into(),
            c"org.freedesktop.portal.Request".into(),
            c"Response".into(),
        );
    }

    let resp = signal_awaiter.await;
    drop(exported_shell);

    println!("open file response! {:?}", resp.signature());
    let mut resp_iter = resp.iter();
    assert_eq!(resp_iter.arg_type(), thirdparty::dbus::TYPE_UINT);
    let response = unsafe { resp_iter.get_u32_unchecked() };
    if response != 0 {
        tracing::warn!(code = response, "FileChooser.OpenFile has cancelled");
        return;
    }

    resp_iter.next();
    assert_eq!(resp_iter.arg_type(), thirdparty::dbus::TYPE_ARRAY);
    let mut resp_results_iter = resp_iter.recurse();
    let mut uris = Vec::new();
    while resp_results_iter.arg_type() != thirdparty::dbus::TYPE_INVALID {
        assert_eq!(
            resp_results_iter.arg_type(),
            thirdparty::dbus::TYPE_DICT_ENTRY
        );
        let mut kv_iter = resp_results_iter.recurse();

        assert_eq!(kv_iter.arg_type(), thirdparty::dbus::TYPE_STRING);
        match unsafe { kv_iter.get_cstr_unchecked() } {
            x if x == c"uris" => {
                kv_iter.next();

                let mut value_iter = kv_iter.begin_iter_variant_content().unwrap();
                let mut iter = value_iter.begin_iter_array_content().unwrap();
                while iter.arg_type() != thirdparty::dbus::TYPE_INVALID {
                    assert_eq!(iter.arg_type(), thirdparty::dbus::TYPE_STRING);
                    uris.push(std::ffi::CString::from(unsafe {
                        iter.get_cstr_unchecked()
                    }));
                    iter.next();
                }
            }
            x if x == c"choices" => {
                kv_iter.next();

                let mut value_iter = kv_iter.begin_iter_variant_content().unwrap();
                let mut iter = value_iter.begin_iter_array_content().unwrap();
                while iter.arg_type() != thirdparty::dbus::TYPE_INVALID {
                    assert_eq!(iter.arg_type(), thirdparty::dbus::TYPE_STRUCT);
                    let mut elements_iter = iter.recurse();
                    assert_eq!(elements_iter.arg_type(), thirdparty::dbus::TYPE_STRING);
                    let key =
                        unsafe { std::ffi::CString::from(elements_iter.get_cstr_unchecked()) };
                    elements_iter.next();
                    assert_eq!(elements_iter.arg_type(), thirdparty::dbus::TYPE_STRING);
                    let value =
                        unsafe { std::ffi::CString::from(elements_iter.get_cstr_unchecked()) };
                    println!("choices {key:?} -> {value:?}");
                    drop(elements_iter);
                    iter.next();
                }
            }
            x if x == c"current_filter" => {
                kv_iter.next();

                assert_eq!(kv_iter.arg_type(), thirdparty::dbus::TYPE_STRUCT);
                let mut struct_iter = kv_iter.recurse();
                assert_eq!(struct_iter.arg_type(), thirdparty::dbus::TYPE_STRING);
                let filter_name =
                    unsafe { std::ffi::CString::from(struct_iter.get_cstr_unchecked()) };
                struct_iter.next();
                assert_eq!(struct_iter.arg_type(), thirdparty::dbus::TYPE_ARRAY);
                let mut array_iter = struct_iter.recurse();
                while array_iter.arg_type() != thirdparty::dbus::TYPE_INVALID {
                    assert_eq!(array_iter.arg_type(), thirdparty::dbus::TYPE_STRUCT);
                    let mut struct_iter = array_iter.recurse();
                    assert_eq!(struct_iter.arg_type(), thirdparty::dbus::TYPE_UINT);
                    let v = unsafe { struct_iter.get_u32_unchecked() };
                    struct_iter.next();
                    assert_eq!(struct_iter.arg_type(), thirdparty::dbus::TYPE_STRING);
                    let f = unsafe { struct_iter.get_cstr_unchecked() };
                    println!("filter {filter_name:?}: {v} {f:?}");
                    array_iter.next();
                }
            }
            c => unreachable!("unexpected result entry: {c:?}"),
        }

        resp_results_iter.next();
    }

    println!("selected: {uris:?}");

    let mut added_sprites = Vec::with_capacity(uris.len());
    for x in uris {
        let path = std::path::PathBuf::from(match x.to_str() {
            Ok(x) => x.strip_prefix("file://").unwrap(),
            Err(e) => {
                tracing::warn!(reason = ?e, "invalid path");
                continue;
            }
        });
        if path.is_dir() {
            // process all files in directory(rec)
            for entry in walkdir::WalkDir::new(&path)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if !path.is_file() {
                    // 自分自身を含むみたいなのでその場合は見逃す
                    continue;
                }

                let mut fs = std::fs::File::open(&path).unwrap();
                let Some(png_meta) = source_reader::png::Metadata::try_read(&mut fs) else {
                    // PNGじゃないのは一旦見逃す
                    continue;
                };

                added_sprites.push(SpriteInfo::new(
                    path.file_stem().unwrap().to_str().unwrap().into(),
                    path.to_path_buf(),
                    png_meta.width,
                    png_meta.height,
                ));
            }
        } else {
            let mut fs = std::fs::File::open(&path).unwrap();
            let png_meta = match source_reader::png::Metadata::try_read(&mut fs) {
                Some(x) => x,
                None => {
                    tracing::warn!(?path, "not a png?");
                    continue;
                }
            };

            added_sprites.push(SpriteInfo::new(
                path.file_stem().unwrap().to_str().unwrap().into(),
                path.to_path_buf(),
                png_meta.width,
                png_meta.height,
            ));
        }
    }

    app_state.borrow_mut().add_sprites(added_sprites);
}

#[cfg(target_os = "linux")]
static mut DBUS_WAIT_FOR_REPLY_WAKERS: *mut HashMap<
    u32,
    Vec<(
        std::rc::Weak<std::cell::Cell<Option<thirdparty::dbus::Message>>>,
        core::task::Waker,
    )>,
> = core::ptr::null_mut();

#[cfg(target_os = "linux")]
static mut DBUS_WAIT_FOR_SIGNAL_WAKERS: *mut HashMap<
    (std::ffi::CString, std::ffi::CString, std::ffi::CString),
    Vec<(
        std::rc::Weak<std::cell::Cell<Option<thirdparty::dbus::Message>>>,
        core::task::Waker,
    )>,
> = core::ptr::null_mut();

#[cfg(target_os = "linux")]
pub struct DBusWaitForReplyFuture {
    serial: u32,
    reply: std::rc::Rc<std::cell::Cell<Option<thirdparty::dbus::Message>>>,
}
#[cfg(target_os = "linux")]
impl DBusWaitForReplyFuture {
    pub fn new(serial: u32) -> Self {
        Self {
            serial,
            reply: std::rc::Rc::new(std::cell::Cell::new(None)),
        }
    }
}
#[cfg(target_os = "linux")]
impl core::future::Future for DBusWaitForReplyFuture {
    type Output = thirdparty::dbus::Message;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        let Some(reply_mut_ref) = std::rc::Rc::get_mut(&mut this.reply) else {
            // still waiting(already registered)
            return core::task::Poll::Pending;
        };

        match reply_mut_ref.take() {
            None => {
                unsafe { &mut *DBUS_WAIT_FOR_REPLY_WAKERS }
                    .entry(this.serial)
                    .or_insert_with(Vec::new)
                    .push((std::rc::Rc::downgrade(&this.reply), cx.waker().clone()));

                core::task::Poll::Pending
            }
            Some(x) => core::task::Poll::Ready(x),
        }
    }
}

#[cfg(target_os = "linux")]
pub struct DBusWaitForSignalFuture {
    key: Option<(std::ffi::CString, std::ffi::CString, std::ffi::CString)>,
    message: std::rc::Rc<std::cell::Cell<Option<thirdparty::dbus::Message>>>,
}
#[cfg(target_os = "linux")]
impl DBusWaitForSignalFuture {
    pub fn new(
        object_path: std::ffi::CString,
        interface: std::ffi::CString,
        member: std::ffi::CString,
    ) -> Self {
        Self {
            key: Some((object_path, interface, member)),
            message: std::rc::Rc::new(std::cell::Cell::new(None)),
        }
    }
}
#[cfg(target_os = "linux")]
impl core::future::Future for DBusWaitForSignalFuture {
    type Output = thirdparty::dbus::Message;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        let Some(msg_mut_ref) = std::rc::Rc::get_mut(&mut this.message) else {
            // still waiting(already registered)
            return core::task::Poll::Pending;
        };

        match msg_mut_ref.take() {
            None => {
                unsafe { &mut *DBUS_WAIT_FOR_SIGNAL_WAKERS }
                    .entry(this.key.take().expect("polled twice"))
                    .or_insert_with(Vec::new)
                    .push((std::rc::Rc::downgrade(&this.message), cx.waker().clone()));

                core::task::Poll::Pending
            }
            Some(x) => core::task::Poll::Ready(x),
        }
    }
}
