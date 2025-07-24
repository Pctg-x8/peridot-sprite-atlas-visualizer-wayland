mod app_state;
mod atlas;
mod base_system;
mod bg_worker;
mod composite;
mod coordinate;
mod feature;
mod helper_types;
mod hittest;
mod input;
mod mathext;
mod peridot;
mod platform;
mod quadtree;
mod shell;
mod source_reader;
mod subsystem;
mod text;
mod trigger_cell;
mod uikit;

use helper_types::SafeF32;
use shared_perflog_proto::{ProfileMarker, ProfileMarkerCategory};
use uikit::popup::PopupManager;

#[cfg(all(unix, not(target_os = "macos")))]
use std::os::fd::AsRawFd;
use std::{
    cell::{Cell, RefCell, UnsafeCell},
    collections::{BTreeSet, HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
};

use crate::{
    atlas::AtlasRect,
    base_system::{
        FontType, create_render_pass2, inject_cmd_begin_render_pass2, inject_cmd_end_render_pass2,
        inject_cmd_pipeline_barrier_2,
    },
    composite::FloatParameter,
    quadtree::QuadTree,
};
use app_state::AppState;
use base_system::{
    AppBaseSystem, RenderPassOptions,
    prof::ProfilingContext,
    scratch_buffer::{BufferedStagingScratchBuffer, StagingScratchBufferManager},
};

use bedrock::{
    self as br, CommandBufferMut, CommandPoolMut, DescriptorPoolMut, Device, Fence, FenceMut,
    ImageChild, InstanceChild, MemoryBound, PhysicalDevice, RenderPass, ShaderModule, Swapchain,
    VkHandle, VkHandleMut, VkObject, VkRawHandle,
};
use bg_worker::{BackgroundWorker, BackgroundWorkerViewFeedback};
use composite::{
    AnimatableColor, AnimatableFloat, AnimationCurve, BackdropEffectBlurProcessor,
    COMPOSITE_PUSH_CONSTANT_RANGES, CompositeInstanceData, CompositeMode, CompositeRect,
    CompositeRenderingData, CompositeStreamingData, CompositeTree, CompositeTreeFloatParameterRef,
    CompositeTreeRef, RenderPassAfterOperation, RenderPassRequirements,
    populate_composite_render_commands,
};
use coordinate::SizePixels;
use feature::editing_atlas_renderer::EditingAtlasRenderer;
use hittest::{HitTestTreeActionHandler, HitTestTreeData, HitTestTreeManager, HitTestTreeRef};
use input::EventContinueControl;
use parking_lot::RwLock;
use shell::AppShell;
use subsystem::Subsystem;

pub enum AppEvent {
    ToplevelWindowNewSize {
        width_px: u32,
        height_px: u32,
    },
    ToplevelWindowClose,
    ToplevelWindowFrameTiming,
    ToplevelWindowMinimizeRequest,
    ToplevelWindowMaximizeRequest,
    MainWindowPointerMove {
        surface_x: f32,
        surface_y: f32,
    },
    MainWindowPointerLeftDown,
    MainWindowPointerLeftUp,
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
    AddSpritesByUriList(Vec<std::ffi::CString>),
    UIShowDragAndDropOverlay,
    UIHideDragAndDropOverlay,
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
        #[cfg(target_os = "macos")]
        {
            // TODO
            Ok(())
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
    #[constant_id = 1]
    pub thickness: f32,
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
    const CORNER_RADIUS: SafeF32 = unsafe { SafeF32::new_unchecked(4.0) };
    const THICKNESS: SafeF32 = unsafe { SafeF32::new_unchecked(2.0) };
    const COLOR: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

    pub fn new(init: &mut ViewInitContext) -> Self {
        let border_image_atlas_rect = init
            .base_system
            .rounded_rect_mask(
                unsafe { SafeF32::new_unchecked(init.ui_scale_factor) },
                Self::CORNER_RADIUS,
                Self::THICKNESS,
            )
            .unwrap();

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
            slice_borders: [Self::CORNER_RADIUS.value() * init.ui_scale_factor; 4],
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

    pub fn rescale(&self, base_system: &mut AppBaseSystem, ui_scale_factor: f32) {
        base_system
            .free_mask_atlas_rect(base_system.composite_tree.get(self.ct_root).texatlas_rect);
        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .texatlas_rect = base_system
            .rounded_rect_mask(
                unsafe { SafeF32::new_unchecked(ui_scale_factor) },
                Self::CORNER_RADIUS,
                Self::THICKNESS,
            )
            .unwrap();

        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .slice_borders = [Self::CORNER_RADIUS.value() * ui_scale_factor; 4];
        base_system.composite_tree.mark_dirty(self.ct_root);
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

                ct.get_mut(self.ct_root).scale_x = AnimatableFloat::Animated {
                    from_value: 1.3,
                    to_value: 1.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.15,
                    curve: AnimationCurve::CubicBezier {
                        p1: (0.0, 0.0),
                        p2: (0.0, 1.0),
                    },
                    event_on_complete: None,
                };
                ct.get_mut(self.ct_root).scale_y = AnimatableFloat::Animated {
                    from_value: 1.3,
                    to_value: 1.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.15,
                    curve: AnimationCurve::CubicBezier {
                        p1: (0.0, 0.0),
                        p2: (0.0, 1.0),
                    },
                    event_on_complete: None,
                };

                ct.get_mut(self.ct_root).opacity = AnimatableFloat::Animated {
                    from_value: 0.0,
                    to_value: 1.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.15,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                };
            }
            Some(CurrentSelectedSpriteTrigger::Hide) => {
                ct.get_mut(self.ct_root).opacity = AnimatableFloat::Animated {
                    from_value: 1.0,
                    to_value: 0.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.15,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                };
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

pub struct DragAndDropOverlayView {
    ct_root: CompositeTreeRef,
    ct_text: CompositeTreeRef,
}
impl DragAndDropOverlayView {
    const BG_COLOR: AnimatableColor = AnimatableColor::Value([1.0, 1.0, 1.0, 0.125]);

    #[tracing::instrument(name = "DragAndDropOverlayView::new", skip(init))]
    pub fn new(init: &mut ViewInitContext) -> Self {
        let text_atlas_rect = init
            .base_system
            .text_mask(
                init.staging_scratch_buffer,
                FontType::UIExtraLarge,
                "Drop to add",
            )
            .unwrap();

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            has_bitmap: true,
            composite_mode: CompositeMode::FillColorBackdropBlur(
                Self::BG_COLOR,
                AnimatableFloat::Value(0.0),
            ),
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });
        let ct_text = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(text_atlas_rect.width() as f32 / init.ui_scale_factor),
                AnimatableFloat::Value(text_atlas_rect.height() as f32 / init.ui_scale_factor),
            ],
            offset: [
                AnimatableFloat::Value(
                    -0.5 * text_atlas_rect.width() as f32 / init.ui_scale_factor,
                ),
                AnimatableFloat::Value(
                    -0.5 * text_atlas_rect.height() as f32 / init.ui_scale_factor,
                ),
            ],
            relative_offset_adjustment: [0.5, 0.5],
            has_bitmap: true,
            texatlas_rect: text_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.5, 0.5, 0.5, 0.75])),
            ..Default::default()
        });

        init.base_system.set_composite_tree_parent(ct_text, ct_root);

        Self { ct_root, ct_text }
    }

    pub fn mount(&self, base_system: &mut AppBaseSystem, ct_parent: CompositeTreeRef) {
        base_system.set_composite_tree_parent(self.ct_root, ct_parent);
    }

    pub fn rescale(
        &self,
        base_system: &mut AppBaseSystem,
        staging_scratch_buffer: &mut StagingScratchBufferManager,
        ui_scale_factor: f32,
    ) {
        base_system.free_mask_atlas_rect(
            self.ct_text
                .entity(&base_system.composite_tree)
                .texatlas_rect,
        );

        let text_atlas_rect = base_system
            .text_mask(
                staging_scratch_buffer,
                FontType::UIExtraLarge,
                "Drop to add",
            )
            .unwrap();

        let cr = self
            .ct_text
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.texatlas_rect = text_atlas_rect;
        cr.base_scale_factor = ui_scale_factor;
    }

    pub fn show(&self, base_system: &mut AppBaseSystem, current_sec: f32) {
        self.ct_root
            .entity_mut_dirtified(&mut base_system.composite_tree)
            .opacity = AnimatableFloat::Animated {
            from_value: 0.0,
            to_value: 1.0,
            start_sec: current_sec,
            end_sec: current_sec + 0.25,
            curve: AnimationCurve::Linear,
            event_on_complete: None,
        };
        self.ct_root
            .entity_mut_dirtified(&mut base_system.composite_tree)
            .composite_mode = CompositeMode::FillColorBackdropBlur(
            Self::BG_COLOR,
            AnimatableFloat::Animated {
                from_value: 0.0,
                to_value: 9.0,
                start_sec: current_sec,
                end_sec: current_sec + 0.125,
                curve: AnimationCurve::CubicBezier {
                    p1: (0.5, 0.5),
                    p2: (0.5, 1.0),
                },
                event_on_complete: None,
            },
        );
    }

    pub fn hide(&self, base_system: &mut AppBaseSystem, current_sec: f32) {
        self.ct_root
            .entity_mut_dirtified(&mut base_system.composite_tree)
            .opacity = AnimatableFloat::Animated {
            from_value: 1.0,
            to_value: 0.0,
            start_sec: current_sec,
            end_sec: current_sec + 0.125,
            curve: AnimationCurve::Linear,
            event_on_complete: None,
        };
        self.ct_root
            .entity_mut_dirtified(&mut base_system.composite_tree)
            .composite_mode = CompositeMode::FillColorBackdropBlur(
            Self::BG_COLOR,
            AnimatableFloat::Animated {
                from_value: 9.0,
                to_value: 0.0,
                start_sec: current_sec,
                end_sec: current_sec + 0.25,
                curve: AnimationCurve::CubicBezier {
                    p1: (0.5, 0.5),
                    p2: (0.5, 1.0),
                },
                event_on_complete: None,
            },
        );
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
    present_mode: br::PresentMode,
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
        let surface_present_modes = surface
            .subsystem
            .adapter()
            .surface_present_modes_alloc(&surface)
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
            .has_any(br::CompositeAlphaFlags::PRE_MULTIPLIED)
        {
            br::CompositeAlphaFlags::PRE_MULTIPLIED
        } else {
            br::CompositeAlphaFlags::INHERIT
        };
        let mut sc_format = None;
        for &f in surface_formats.iter() {
            tracing::debug!(format = ?f.format, color_space = ?f.colorSpace, "surface format");
            // prefer first format
            if sc_format.is_none()
                && (f.format == br::vk::VK_FORMAT_R8G8B8A8_UNORM
                    || f.format == br::vk::VK_FORMAT_B8G8R8A8_UNORM)
                && f.colorSpace == br::vk::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR
            {
                sc_format = Some(f);
            }
        }
        let sc_format = sc_format.unwrap();
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
        let present_mode = if surface_present_modes.contains(&br::PresentMode::Mailbox) {
            br::PresentMode::Mailbox
        } else {
            surface_present_modes[0]
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
                    .composite_alpha(sc_composite_alpha)
                    .present_mode(present_mode),
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
            present_mode,
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
                .composite_alpha(self.composite_alpha)
                .present_mode(self.present_mode),
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

    #[cfg(all(unix, not(target_os = "macos")))]
    let dbus = dbus::Connection::connect_bus(dbus::BusType::Session).unwrap();
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut dbus = DBusLink { con: dbus };

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
        #[cfg(all(unix, not(target_os = "macos")))]
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
    #[cfg(all(unix, not(target_os = "macos")))] dbus: &'sys mut DBusLink,
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
            .with_usage(br::ImageUsageFlags::SAMPLED | br::ImageUsageFlags::TRANSFER_DEST),
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

    let main_rp_grabbed = create_render_pass2(
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
    let main_rp_final = create_render_pass2(
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
    let main_rp_continue_grabbed = create_render_pass2(
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
    let main_rp_continue_final = create_render_pass2(
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

    let composite_sampler =
        br::SamplerObject::new(app_system.subsystem, &br::SamplerCreateInfo::new()).unwrap();

    let composite_vsh = app_system.require_shader("resources/composite.vert");
    let composite_fsh = app_system.require_shader("resources/composite.frag");
    let composite_shader_stages = [
        composite_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
        composite_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
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
            COMPOSITE_PUSH_CONSTANT_RANGES,
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

    let mut backdrop_fx_blur_processor =
        BackdropEffectBlurProcessor::new(app_system, sc.size, sc.color_format());

    let mut fixed_descriptor_pool = br::DescriptorPoolObject::new(
        app_system.subsystem,
        &br::DescriptorPoolCreateInfo::new(
            (1 + backdrop_fx_blur_processor.fixed_descriptor_set_count()) as _,
            &[
                br::DescriptorType::CombinedImageSampler
                    .make_size((1 + backdrop_fx_blur_processor.fixed_descriptor_set_count()) as _),
                br::DescriptorType::UniformBuffer.make_size(1),
                br::DescriptorType::StorageBuffer.make_size(1),
            ],
        ),
    )
    .unwrap();
    let [composite_alphamask_group_descriptor] = fixed_descriptor_pool
        .alloc_array(&[composite_descriptor_layout.as_transparent_ref()])
        .unwrap();
    let blur_fixed_descriptors =
        backdrop_fx_blur_processor.alloc_fixed_descriptor_sets(&mut fixed_descriptor_pool);
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
    ];
    backdrop_fx_blur_processor.write_input_descriptor_sets(
        &mut descriptor_writes,
        &composite_grab_buffer,
        &blur_fixed_descriptors,
    );
    app_system
        .subsystem
        .update_descriptor_sets(&descriptor_writes, &[]);

    let mut corner_cutout_renderer = if !app_shell.server_side_decoration_provided() {
        // window decorations should be rendered by client size(not provided by window system server)
        Some(WindowCornerCutoutRenderer::new(
            app_system,
            &composite_sampler,
            sc.size,
            main_rp_final.subpass(0),
            main_rp_continue_final.subpass(0),
        ))
    } else {
        None
    };

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
    let mut sprite_list_pane =
        feature::sprite_list_pane::Presenter::new(&mut init_context, app_header.height());
    let app_menu = feature::app_menu::Presenter::new(&mut init_context, app_header.height());

    let current_selected_sprite_marker_view = Rc::new(CurrentSelectedSpriteMarkerView::new(
        &mut init_context.for_view,
    ));
    let dnd_overlay = DragAndDropOverlayView::new(&mut init_context.for_view);

    drop(init_context);
    drop(staging_scratch_buffer_locked);

    current_selected_sprite_marker_view.mount(CompositeTree::ROOT, &mut app_system.composite_tree);
    sprite_list_pane.mount(app_system, CompositeTree::ROOT, HitTestTreeManager::ROOT);
    app_menu.mount(app_system, CompositeTree::ROOT, HitTestTreeManager::ROOT);
    app_header.mount(app_system, CompositeTree::ROOT, HitTestTreeManager::ROOT);
    dnd_overlay.mount(app_system, CompositeTree::ROOT);

    // reordering hit for popups
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
        let staging_scratch_buffer_locked =
            parking_lot::RwLockWriteGuard::map(staging_scratch_buffers.write(), |x| {
                x.active_buffer_mut()
            });

        tracing::debug!(
            byte_size = staging_scratch_buffer_locked.total_reserved_amount(),
            "Reserved Staging Buffers during UI initialization",
        );
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
    dbus.con.set_watch_functions(Box::new(DBusWatcher {
        epoll: &epoll,
        fd_pool: &poll_fd_pool,
        fd_to_pool_index: HashMap::new(),
    }));

    let mut popup_manager = PopupManager::new(popup_hit_layer, CompositeTree::ROOT);

    // initialize misc state
    let mut active_ui_scale = app_shell.ui_scale_factor();
    let mut newsize_request = None;
    let mut last_pointer_pos = (0.0f32, 0.0f32);
    let mut last_composite_render_instructions = CompositeRenderingData {
        instructions: Vec::new(),
        render_passes: Vec::new(),
        required_backdrop_buffer_count: 0,
    };
    let mut composite_instance_buffer_dirty = false;

    // initial post event
    events.push(AppEvent::ToplevelWindowFrameTiming);

    let mut app_update_context = AppUpdateContext {
        event_queue: &events,
        state: &app_state,
        ui_scale_factor: app_shell.ui_scale_factor(),
    };

    let elapsed = setup_timer.elapsed();
    tracing::info!(?elapsed, "App Setup done!");

    #[cfg(target_os = "linux")]
    let mut epoll_events =
        [const { core::mem::MaybeUninit::<platform::linux::epoll_event>::uninit() }; 8];
    let t = std::time::Instant::now();
    let mut _profiler = ProfilingContext::init("./local/profile");
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
                        let mut flags = dbus::WatchFlags::empty();
                        if (e.events & platform::linux::EPOLLIN) != 0 {
                            flags |= dbus::WatchFlags::READABLE;
                        }
                        if (e.events & platform::linux::EPOLLOUT) != 0 {
                            flags |= dbus::WatchFlags::WRITABLE;
                        }
                        if (e.events & platform::linux::EPOLLERR) != 0 {
                            flags |= dbus::WatchFlags::ERROR;
                        }
                        if (e.events & platform::linux::EPOLLHUP) != 0 {
                            flags |= dbus::WatchFlags::HANGUP;
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

            dispatch_dbus(&dbus);
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
        #[cfg(target_os = "macos")]
        {
            // TODO: ここで止まってしまうのでメインループの仕組みごっそり変える必要がある
            app_shell.process_pending_events();
        }

        let current_ui_scale = app_shell.ui_scale_factor();
        if active_ui_scale != current_ui_scale {
            tracing::info!(
                from = active_ui_scale,
                to = current_ui_scale,
                "UI Rescaling"
            );
            active_ui_scale = current_ui_scale;
            app_system.rescale_fonts(active_ui_scale);

            let mut staging_scratch_buffer_locked =
                parking_lot::RwLockWriteGuard::map(staging_scratch_buffers.write(), |x| {
                    x.active_buffer_mut()
                });

            app_header.rescale(
                app_system,
                &mut staging_scratch_buffer_locked,
                active_ui_scale,
            );
            app_menu.rescale(
                app_system,
                &mut staging_scratch_buffer_locked,
                active_ui_scale,
            );
            current_selected_sprite_marker_view.rescale(app_system, active_ui_scale);
            sprite_list_pane.rescale(app_system, &mut staging_scratch_buffer_locked, unsafe {
                SafeF32::new_unchecked(active_ui_scale)
            });
            dnd_overlay.rescale(
                app_system,
                &mut staging_scratch_buffer_locked,
                active_ui_scale,
            );

            tracing::debug!(
                byte_size = staging_scratch_buffer_locked.total_reserved_amount(),
                "Reserved Staging Buffers during UI Rescaling",
            );
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
                    app_shell.toggle_maximize_restore();
                }
                AppEvent::ToplevelWindowFrameTiming => {
                    let mut _pf = _profiler.begin_frame();

                    let current_t = t.elapsed();
                    let current_sec = current_t.as_secs_f32();

                    if last_rendering {
                        last_render_command_fence.wait().unwrap();
                        last_render_command_fence.reset().unwrap();
                        last_rendering = false;
                    }

                    if let Some((width, height)) = newsize_request.take() {
                        let _pf = _pf.scoped(ProfileMarker::Resize);

                        let w_dip = width as f32 / app_shell.ui_scale_factor();
                        let h_dip = height as f32 / app_shell.ui_scale_factor();
                        tracing::trace!(width, height, "frame resize");

                        client_size.set((w_dip, h_dip));

                        unsafe {
                            main_cp.reset(br::CommandPoolResetFlags::EMPTY).unwrap();
                        }
                        main_cb_invalid = true;

                        sc.resize(br::Extent2D { width, height });

                        main_grabbed_fbs.clear();
                        main_final_fbs.clear();
                        main_continue_grabbed_fbs.clear();
                        main_continue_final_fbs.clear();
                        for bb in sc.backbuffer_views() {
                            main_grabbed_fbs.push(
                                br::FramebufferObject::new(
                                    app_system.subsystem,
                                    &br::FramebufferCreateInfo::new(
                                        &main_rp_grabbed,
                                        &[bb.as_transparent_ref()],
                                        sc.size.width,
                                        sc.size.height,
                                    ),
                                )
                                .unwrap(),
                            );
                            main_final_fbs.push(
                                br::FramebufferObject::new(
                                    app_system.subsystem,
                                    &br::FramebufferCreateInfo::new(
                                        &main_rp_final,
                                        &[bb.as_transparent_ref()],
                                        sc.size.width,
                                        sc.size.height,
                                    ),
                                )
                                .unwrap(),
                            );
                            main_continue_grabbed_fbs.push(
                                br::FramebufferObject::new(
                                    app_system.subsystem,
                                    &br::FramebufferCreateInfo::new(
                                        &main_rp_continue_grabbed,
                                        &[bb.as_transparent_ref()],
                                        sc.size.width,
                                        sc.size.height,
                                    ),
                                )
                                .unwrap(),
                            );
                            main_continue_final_fbs.push(
                                br::FramebufferObject::new(
                                    app_system.subsystem,
                                    &br::FramebufferCreateInfo::new(
                                        &main_rp_continue_final,
                                        &[bb.as_transparent_ref()],
                                        sc.size.width,
                                        sc.size.height,
                                    ),
                                )
                                .unwrap(),
                            );
                        }

                        composite_backdrop_buffers_invalidated = true;

                        drop(composite_grab_buffer);
                        drop(composite_grab_buffer_memory);
                        let mut composite_grab_buffer1 = br::ImageObject::new(
                            app_system.subsystem,
                            &br::ImageCreateInfo::new(sc.size, sc.color_format()).with_usage(
                                br::ImageUsageFlags::SAMPLED | br::ImageUsageFlags::TRANSFER_DEST,
                            ),
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

                        backdrop_fx_blur_processor.recreate_rt_resources(
                            app_system,
                            sc.size,
                            sc.color_format(),
                        );

                        let mut descriptor_writes = Vec::new();
                        backdrop_fx_blur_processor.write_input_descriptor_sets(
                            &mut descriptor_writes,
                            &composite_grab_buffer,
                            &blur_fixed_descriptors,
                        );
                        app_system
                            .subsystem
                            .update_descriptor_sets(&descriptor_writes, &[]);

                        let composite_vsh = app_system.require_shader("resources/composite.vert");
                        let composite_fsh = app_system.require_shader("resources/composite.frag");
                        let composite_shader_stages = [
                            composite_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                            composite_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                        ];

                        let screen_viewport = [sc
                            .size
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)];
                        let screen_scissor = [sc.size.into_rect(br::Offset2D::ZERO)];
                        let screen_viewport_state = br::PipelineViewportStateCreateInfo::new_array(
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
                        composite_pipeline_continue_grabbed = composite_pipeline_continue_grabbed1;
                        composite_pipeline_continue_final = composite_pipeline_continue_final1;

                        if let Some(ref mut r) = corner_cutout_renderer {
                            r.resize_rt(
                                app_system,
                                sc.size,
                                main_rp_final.subpass(0),
                                main_rp_continue_final.subpass(0),
                            );
                        }

                        editing_atlas_renderer.borrow_mut().recreate(
                            app_system,
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

                    current_selected_sprite_marker_view
                        .update(&mut app_system.composite_tree, current_sec);
                    app_header.update(&mut app_system.composite_tree, current_sec);
                    app_menu.update(app_system, current_sec);
                    sprite_list_pane.update(
                        app_system,
                        current_sec,
                        &mut staging_scratch_buffers.write().active_buffer_mut(),
                    );
                    popup_manager.update(app_system, current_sec);

                    {
                        let _pf = _pf.scoped(ProfileMarker::PopulateCompositeInstances);

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
                                // invalidate first
                                if let Err(e) =
                                    unsafe { main_cp.reset(br::CommandPoolResetFlags::EMPTY) }
                                {
                                    tracing::warn!(reason = ?e, "main command pool reset failed");
                                }

                                main_cb_invalid = true;
                            }

                            if composite_render_instructions.required_backdrop_buffer_count
                                > composite_backdrop_buffer_descriptor_pool_capacity
                            {
                                // resize pool
                                let object_count = composite_render_instructions
                                    .required_backdrop_buffer_count
                                    .max(1);

                                composite_backdrop_buffer_descriptor_pool =
                                    br::DescriptorPoolObject::new(
                                        app_system.subsystem,
                                        &br::DescriptorPoolCreateInfo::new(
                                            object_count as _,
                                            &[br::DescriptorType::CombinedImageSampler
                                                .make_size(object_count as _)],
                                        ),
                                    )
                                    .unwrap();
                                composite_backdrop_buffer_descriptor_pool_capacity = object_count;
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
                                        &core::iter::repeat_n(
                                            composite_backdrop_descriptor_layout
                                                .as_transparent_ref(),
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
                                &br::ImageCreateInfo::new(sc.size, sc.color_format()).with_usage(
                                    br::ImageUsageFlags::SAMPLED
                                        | br::ImageUsageFlags::COLOR_ATTACHMENT
                                        | br::ImageUsageFlags::TRANSFER_DEST,
                                ),
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
                                        backdrop_fx_blur_processor.final_render_pass(),
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
                        let rec =
                            unsafe { update_cb.begin(&br::CommandBufferBeginInfo::new()).unwrap() };
                        let rec = if composite_instance_buffer_dirty {
                            app_system.composite_instance_manager.sync_buffer(rec)
                        } else {
                            rec
                        };
                        rec.inject(|r| {
                            inject_cmd_pipeline_barrier_2(
                                r,
                                app_system.subsystem,
                                &br::DependencyInfo::new(
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
                                        br::ImageSubresourceRange::new(
                                            br::AspectMask::COLOR,
                                            0..1,
                                            0..1,
                                        ),
                                    )
                                    .transit_to(
                                        br::ImageLayout::ShaderReadOnlyOpt.from_undefined(),
                                    )],
                                ),
                            )
                        })
                        .inject(|r| {
                            editing_atlas_renderer.borrow_mut().process_dirty_data(
                                app_system.subsystem,
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

                    _pf.record(
                        ProfileMarker::MainCommandBufferPopulation,
                        ProfileMarkerCategory::Begin,
                    );
                    if main_cb_invalid {
                        if last_composite_render_instructions.render_passes[0]
                            != editing_atlas_current_bound_pipeline
                        {
                            editing_atlas_current_bound_pipeline =
                                last_composite_render_instructions.render_passes[0];
                            editing_atlas_renderer.borrow_mut().recreate(
                                app_system,
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

                            unsafe { cb.begin(&br::CommandBufferBeginInfo::new()).unwrap() }
                                .inject(|r| {
                                    inject_cmd_begin_render_pass2(
                                        r,
                                        app_system.subsystem,
                                        &br::RenderPassBeginInfo::new(
                                            first_rp,
                                            first_fb,
                                            sc.size.into_rect(br::Offset2D::ZERO),
                                            &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 1.0])],
                                        ),
                                        &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                                    )
                                })
                                .inject(|r| {
                                    editing_atlas_renderer.borrow().render_commands(sc.size, r)
                                })
                                .inject(|r| {
                                    populate_composite_render_commands(
                                        r,
                                        app_system.subsystem,
                                        true,
                                        sc.size,
                                        &last_composite_render_instructions,
                                        |rpreq| match rpreq {
                                            RenderPassRequirements {
                                                continued: false, ..
                                            } => unreachable!("not at first(must be continued)"),
                                            RenderPassRequirements {
                                                continued: true,
                                                after_operation: RenderPassAfterOperation::Grab,
                                            } => (
                                                main_rp_continue_grabbed.as_transparent_ref(),
                                                main_continue_grabbed_fbs[n].as_transparent_ref(),
                                            ),
                                            RenderPassRequirements {
                                                continued: true,
                                                after_operation: RenderPassAfterOperation::None,
                                            } => (
                                                main_rp_continue_final.as_transparent_ref(),
                                                main_continue_final_fbs[n].as_transparent_ref(),
                                            ),
                                        },
                                        |rpreq| match rpreq {
                                            RenderPassRequirements {
                                                continued: false,
                                                after_operation: RenderPassAfterOperation::Grab,
                                            } => composite_pipeline_grabbed.as_transparent_ref(),
                                            RenderPassRequirements {
                                                continued: false,
                                                after_operation: RenderPassAfterOperation::None,
                                            } => composite_pipeline_final.as_transparent_ref(),
                                            RenderPassRequirements {
                                                continued: true,
                                                after_operation: RenderPassAfterOperation::Grab,
                                            } => composite_pipeline_continue_grabbed
                                                .as_transparent_ref(),
                                            RenderPassRequirements {
                                                continued: true,
                                                after_operation: RenderPassAfterOperation::None,
                                            } => composite_pipeline_continue_final
                                                .as_transparent_ref(),
                                        },
                                        &composite_pipeline_layout,
                                        composite_alphamask_group_descriptor,
                                        &composite_backdrop_buffer_descriptor_sets,
                                        &composite_grab_buffer,
                                        &sc.backbuffer_image(n),
                                        &backdrop_fx_blur_processor,
                                        &blur_fixed_descriptors,
                                        &composite_backdrop_blur_destination_fbs,
                                    )
                                })
                                .inject(|r| {
                                    if app_shell.is_tiled() {
                                        // shell window is tiled(no decorations needed)
                                        return r;
                                    }

                                    let Some(ref renderer) = corner_cutout_renderer else {
                                        // no client side decoration
                                        return r;
                                    };

                                    let rp_last_continued = last_composite_render_instructions
                                        .render_passes
                                        .last()
                                        .map_or(false, |x| x.continued);
                                    assert!(
                                        last_composite_render_instructions
                                            .render_passes
                                            .last()
                                            .is_none_or(|x| x.after_operation
                                                == RenderPassAfterOperation::None)
                                    );

                                    renderer.populate_commands(r, rp_last_continued)
                                })
                                .inject(|r| {
                                    inject_cmd_end_render_pass2(
                                        r,
                                        app_system.subsystem,
                                        &br::SubpassEndInfo::new(),
                                    )
                                })
                                .end()
                                .unwrap();
                        }

                        main_cb_invalid = false;
                    }
                    _pf.record(
                        ProfileMarker::MainCommandBufferPopulation,
                        ProfileMarkerCategory::End,
                    );

                    _pf.record(
                        ProfileMarker::RenderWorkSubmission,
                        ProfileMarkerCategory::Begin,
                    );
                    let next = match sc.acquire_next(
                        None,
                        br::CompletionHandlerMut::Queue(
                            acquire_completion.as_transparent_ref_mut(),
                        ),
                    ) {
                        Ok(x) => x,
                        Err(e) if e == br::vk::VK_ERROR_OUT_OF_DATE_KHR => {
                            tracing::warn!("swapchain out of date");
                            // force recreate resources
                            newsize_request = Some(app_shell.client_size_pixels());
                            continue;
                        }
                        Err(e) => {
                            tracing::error!(reason = ?e, "vkAcquireNextImageKHR failed");
                            std::process::abort();
                        }
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
                    let mut results = [br::vk::VkResult(0)];
                    match app_system.subsystem.queue_present(&br::PresentInfo::new(
                        &[render_completion_per_backbuffer[next as usize].as_transparent_ref()],
                        &[sc.as_transparent_ref()],
                        &[next],
                        &mut results,
                    )) {
                        Ok(_) => (),
                        Err(e) if e == br::vk::VK_ERROR_OUT_OF_DATE_KHR => {
                            tracing::warn!(?results, "swapchain out of date");
                            // force recreate resources
                            newsize_request = Some(app_shell.client_size_pixels());
                            continue;
                        }
                        Err(e) => {
                            tracing::error!(reason = ?e, "vkQueuePresentKHR failed");
                            std::process::abort();
                        }
                    }
                    _pf.record(
                        ProfileMarker::RenderWorkSubmission,
                        ProfileMarkerCategory::End,
                    );

                    app_shell.request_next_frame();
                    drop(_pf);
                }
                AppEvent::ToplevelWindowNewSize {
                    width_px,
                    height_px,
                } => {
                    if sc.size.width != width_px || sc.size.height != height_px {
                        newsize_request = Some((width_px, height_px));
                    }
                }
                AppEvent::MainWindowPointerMove {
                    surface_x,
                    surface_y,
                } => {
                    app_update_context.ui_scale_factor = app_shell.ui_scale_factor();
                    let (cw, ch) = client_size.get();

                    unsafe { &mut *app_shell.pointer_input_manager().get() }.handle_mouse_move(
                        surface_x,
                        surface_y,
                        cw,
                        ch,
                        &mut app_system.hit_tree,
                        &mut app_update_context,
                        HitTestTreeManager::ROOT,
                    );
                    app_shell.set_cursor_shape(
                        unsafe { &mut *app_shell.pointer_input_manager().get() }
                            .cursor_shape(&mut app_system.hit_tree, &mut app_update_context),
                    );

                    last_pointer_pos = (surface_x, surface_y);
                }
                AppEvent::MainWindowPointerLeftDown => {
                    app_update_context.ui_scale_factor = app_shell.ui_scale_factor();
                    let (cw, ch) = client_size.get();

                    unsafe { &mut *app_shell.pointer_input_manager().get() }
                        .handle_mouse_left_down(
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
                        unsafe { &mut *app_shell.pointer_input_manager().get() }
                            .cursor_shape(&mut app_system.hit_tree, &mut app_update_context),
                    );
                }
                AppEvent::MainWindowPointerLeftUp => {
                    app_update_context.ui_scale_factor = app_shell.ui_scale_factor();
                    let (cw, ch) = client_size.get();

                    unsafe { &mut *app_shell.pointer_input_manager().get() }.handle_mouse_left_up(
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
                        unsafe { &mut *app_shell.pointer_input_manager().get() }
                            .cursor_shape(&mut app_system.hit_tree, &mut app_update_context),
                    );
                }
                AppEvent::UIMessageDialogRequest { content } => {
                    let mut staging_scratch_buffer_locked =
                        parking_lot::RwLockWriteGuard::map(staging_scratch_buffers.write(), |x| {
                            x.active_buffer_mut()
                        });

                    popup_manager.spawn(
                        &mut PresenterInitContext {
                            for_view: ViewInitContext {
                                base_system: app_system,
                                staging_scratch_buffer: &mut staging_scratch_buffer_locked,
                                ui_scale_factor: active_ui_scale,
                            },
                            app_state: &mut *app_state.borrow_mut(),
                        },
                        t.elapsed().as_secs_f32(),
                        &content,
                    );
                    unsafe { &mut *app_shell.pointer_input_manager().get() }.recompute_enter_leave(
                        client_size.get().0,
                        client_size.get().1,
                        &mut app_system.hit_tree,
                        &mut app_update_context,
                        HitTestTreeManager::ROOT,
                    );
                }
                AppEvent::UIPopupClose { id } => {
                    popup_manager.close(app_system, t.elapsed().as_secs_f32(), &id);
                    unsafe { &mut *app_shell.pointer_input_manager().get() }.recompute_enter_leave(
                        client_size.get().0,
                        client_size.get().1,
                        &mut app_system.hit_tree,
                        &mut app_update_context,
                        HitTestTreeManager::ROOT,
                    );
                }
                AppEvent::UIPopupUnmount { id } => {
                    popup_manager.remove(app_system, &id);
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
                AppEvent::AddSpritesByUriList(uris) => {
                    app_state.borrow_mut().add_sprites_by_uri_list(uris);
                }
                AppEvent::UIShowDragAndDropOverlay => {
                    dnd_overlay.show(app_system, t.elapsed().as_secs_f32());
                }
                AppEvent::UIHideDragAndDropOverlay => {
                    dnd_overlay.hide(app_system, t.elapsed().as_secs_f32());
                }
            }
            app_update_context.event_queue.notify_clear().unwrap();
        }
    }

    _profiler.flush();

    if let Err(e) = unsafe { app_system.subsystem.wait() } {
        tracing::warn!(reason = ?e, "Error in waiting pending works before shutdown");
    }
}

#[cfg(target_os = "linux")]
struct DBusLink {
    con: dbus::Connection,
}
#[cfg(target_os = "linux")]
impl DBusLink {
    #[inline(always)]
    pub fn underlying(&self) -> &dbus::Connection {
        &self.con
    }

    pub async fn send(&self, mut msg: dbus::Message) -> Option<dbus::Message> {
        let Some(serial) = self.con.send_with_serial(&mut msg) else {
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
    app_state
        .borrow_mut()
        .add_sprites_from_file_paths(added_paths);
}

#[cfg(unix)]
struct DesktopPortal;
#[cfg(unix)]
impl DesktopPortal {
    #[tracing::instrument(name = "DesktopPortal::try_get_file_chooser", skip(dbus))]
    pub async fn try_get_file_chooser(dbus: &DBusLink) -> Option<DesktopPortalFileChooser> {
        let reply_msg = dbus
            .send(
                dbus::Message::new_method_call(
                    Some(c"org.freedesktop.portal.Desktop"),
                    c"/org/freedesktop/portal/desktop",
                    Some(c"org.freedesktop.DBus.Introspectable"),
                    c"Introspect",
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let reply_iter = reply_msg.iter();
        let doc = reply_iter
            .try_get_cstr()
            .expect("invalid introspection response")
            .to_str()
            .unwrap();

        let mut has_file_chooser = false;
        if let Err(e) = dbus::introspect_document::read_toplevel(
            &mut quick_xml::Reader::from_str(doc),
            |_, ifname, r| {
                has_file_chooser = ifname.as_ref() == b"org.freedesktop.portal.FileChooser";

                dbus::introspect_document::skip_read_interface_tag_contents(r)
            },
        ) {
            tracing::warn!(reason = ?e, "Failed to parse introspection document from portal object");
        }

        has_file_chooser.then_some(DesktopPortalFileChooser)
    }

    #[inline(always)]
    pub fn open_request_object(path: std::ffi::CString) -> DesktopPortalRequestObject {
        DesktopPortalRequestObject::new(path)
    }

    #[inline(always)]
    pub fn open_request_object_for_token(
        dbus: &DBusLink,
        token: &str,
    ) -> DesktopPortalRequestObject {
        DesktopPortalRequestObject::from_token(dbus, token)
    }
}

#[cfg(unix)]
struct DesktopPortalFileChooser;
#[cfg(unix)]
impl DesktopPortalFileChooser {
    pub async fn get_version(&self, dbus: &DBusLink) -> Result<u32, dbus::Error> {
        let reply_msg = dbus
            .send({
                let mut msg = dbus::Message::new_method_call(
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
            return Err(error);
        }

        let mut reply_iter = reply_msg.iter();
        assert_eq!(reply_iter.arg_type(), dbus::TYPE_VARIANT);
        Ok(reply_iter
            .recurse()
            .try_get_u32()
            .expect("unexpected version value"))
    }

    /// https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html#org-freedesktop-portal-filechooser-openfile
    pub async fn open_file(
        &self,
        dbus: &DBusLink,
        parent_window: Option<&core::ffi::CStr>,
        title: &core::ffi::CStr,
        options_builder: impl FnOnce(&mut dbus::MessageIterAppendContainer<dbus::MessageIterAppend>),
    ) -> Result<DesktopPortalRequestObject, dbus::Error> {
        let Some(resp) = dbus
            .send({
                let mut msg = dbus::Message::new_method_call(
                    Some(c"org.freedesktop.portal.Desktop"),
                    c"/org/freedesktop/portal/desktop",
                    Some(c"org.freedesktop.portal.FileChooser"),
                    c"OpenFile",
                )
                .unwrap();
                let mut msg_args_appender = msg.iter_append();
                msg_args_appender.append_cstr(parent_window.unwrap_or(c""));
                msg_args_appender.append_cstr(title);
                let mut options_appender = msg_args_appender
                    .open_container(dbus::TYPE_ARRAY, Some(c"{sv}"))
                    .unwrap();
                options_builder(&mut options_appender);
                options_appender.close();

                msg
            })
            .await
        else {
            unreachable!("no response for desktop-portal call?");
        };

        if let Some(e) = resp.try_get_error() {
            return Err(e);
        }

        let reply_iter = resp.iter();
        let handle = DesktopPortalRequestObject::new(
            reply_iter
                .try_get_object_path()
                .expect("invalid response")
                .into(),
        );
        assert!(!reply_iter.has_next(), "reply data left");
        Ok(handle)
    }
}

#[cfg(unix)]
pub struct DesktopPortalRequestObject {
    object_path: std::ffi::CString,
}
#[cfg(unix)]
impl DesktopPortalRequestObject {
    pub fn new(object_path: std::ffi::CString) -> Self {
        Self { object_path }
    }

    fn from_token(dbus: &DBusLink, token: &str) -> Self {
        let mut object_path = String::from("/org/freedesktop/portal/desktop/request/");
        object_path.extend(
            dbus.underlying()
                .unique_name()
                .unwrap()
                .to_str()
                .unwrap()
                .strip_prefix(':')
                .unwrap()
                .replace('.', "_")
                .chars(),
        );
        object_path.push_str(token);

        Self::new(unsafe { std::ffi::CString::from_vec_unchecked(object_path.into_bytes()) })
    }

    #[inline(always)]
    pub fn points_same_object(&self, other: &Self) -> bool {
        self.object_path == other.object_path
    }

    pub fn wait_for_response(&self) -> DBusWaitForSignalFuture {
        DBusWaitForSignalFuture::new(
            self.object_path.clone(),
            c"org.freedesktop.portal.Request".into(),
            c"Response".into(),
        )
    }
}

#[cfg(target_os = "linux")]
async fn app_menu_on_add_sprite<'subsystem>(
    dbus: &DBusLink,
    shell: &AppShell<'_, 'subsystem>,
    events: &AppEventBus,
    app_state: &RefCell<AppState<'subsystem>>,
) {
    // TODO: これUIだして待つべきか？ローカルだからあんまり待たないような気もするが......
    let Some(dp_file_chooser) = DesktopPortal::try_get_file_chooser(dbus).await else {
        // FileChooserなし
        events.push(AppEvent::UIMessageDialogRequest {
            content: "org.freedesktop.portal.FileChooser not found".into(),
        });

        return;
    };

    let version = match dp_file_chooser.get_version(dbus).await {
        Ok(x) => x,
        Err(e) => {
            tracing::error!(reason = ?e, "FileChooser version get failed");
            events.push(AppEvent::UIMessageDialogRequest {
                content: format!("FileChooser version get failed: reason={e:?}"),
            });

            return;
        }
    };
    tracing::trace!(version, "AddSprite: file chooser found!");

    let dialog_token = uuid::Uuid::new_v4().as_simple().to_string();
    let mut request_object = DesktopPortal::open_request_object_for_token(dbus, &dialog_token);

    let exported_shell = shell.try_export_toplevel();
    let r = dp_file_chooser
        .open_file(
            dbus,
            exported_shell.as_ref().map(|x| x.handle.as_c_str()),
            c"Add Sprite",
            |options_appender| {
                let mut dict_appender = options_appender.open_dict_entry_container().unwrap();
                dict_appender.append_cstr(c"handle_token");
                dict_appender
                    .append_variant_cstr(&std::ffi::CString::new(dialog_token.clone()).unwrap());
                dict_appender.close();

                let mut dict_appender = options_appender.open_dict_entry_container().unwrap();
                dict_appender.append_cstr(c"multiple");
                dict_appender.append_variant_bool(true);
                dict_appender.close();
            },
        )
        .await;
    let request_handle = match r {
        Ok(x) => x,
        Err(e) => {
            tracing::error!(reason = ?e, "FileChooser.OpenFile failed");
            events.push(AppEvent::UIMessageDialogRequest {
                content: format!("FileChooser.OpenFile failed: reason={e:?}"),
            });

            return;
        }
    };

    if !request_object.points_same_object(&request_handle) {
        tracing::debug!(
            open_file_dialog_handle = ?request_handle.object_path,
            request_object_path = ?request_object.object_path,
            "returned object_path did not match with the expected, switching request object..."
        );
        request_object = request_handle;
    }

    let resp = request_object.wait_for_response().await;
    drop(exported_shell);

    let mut resp_iter = resp.iter();
    let response = resp_iter.try_get_u32().expect("unexpected type");
    if response != 0 {
        tracing::warn!(code = response, "FileChooser.OpenFile has cancelled");
        return;
    }

    resp_iter.next();
    assert_eq!(resp_iter.arg_type(), dbus::TYPE_ARRAY);
    let mut resp_results_iter = resp_iter.recurse();
    let mut uris = Vec::new();
    while resp_results_iter.arg_type() != dbus::TYPE_INVALID {
        assert_eq!(resp_results_iter.arg_type(), dbus::TYPE_DICT_ENTRY);
        let mut kv_iter = resp_results_iter.recurse();

        match kv_iter.try_get_cstr().expect("unexpected key value") {
            x if x == c"uris" => {
                kv_iter.next();

                let mut value_iter = kv_iter
                    .try_begin_iter_variant_content()
                    .expect("invalid uris value");
                let mut iter = value_iter
                    .try_begin_iter_array_content()
                    .expect("invalid uris value content");
                while iter.arg_type() != dbus::TYPE_INVALID {
                    uris.push(std::ffi::CString::from(
                        iter.try_get_cstr().expect("unexpected uris value"),
                    ));
                    iter.next();
                }
            }
            x if x == c"choices" => {
                kv_iter.next();

                let mut value_iter = kv_iter
                    .try_begin_iter_variant_content()
                    .expect("invalid choices value");
                let mut iter = value_iter
                    .try_begin_iter_array_content()
                    .expect("invalid choices value content");
                while iter.arg_type() != dbus::TYPE_INVALID {
                    let mut elements_iter = iter
                        .try_begin_iter_struct_content()
                        .expect("invalid choices value content element");
                    let key = elements_iter
                        .try_get_cstr()
                        .expect("unexpected key value")
                        .to_owned();
                    elements_iter.next();
                    let value = elements_iter
                        .try_get_cstr()
                        .expect("unexpected value")
                        .to_owned();
                    println!("choices {key:?} -> {value:?}");
                    drop(elements_iter);

                    iter.next();
                }
            }
            x if x == c"current_filter" => {
                kv_iter.next();

                let mut struct_iter = kv_iter
                    .try_begin_iter_struct_content()
                    .expect("invalid current_filter value");
                let filter_name = struct_iter
                    .try_get_cstr()
                    .expect("unexpected filter name value")
                    .to_owned();
                struct_iter.next();
                let mut array_iter = struct_iter
                    .try_begin_iter_array_content()
                    .expect("invalid current_filter value content");
                while array_iter.arg_type() != dbus::TYPE_INVALID {
                    let mut struct_iter = array_iter
                        .try_begin_iter_struct_content()
                        .expect("invalid current_filter value content element");
                    let v = struct_iter.try_get_u32().expect("unexpected type");
                    struct_iter.next();
                    let f = struct_iter.try_get_cstr().expect("unexpected filter value");
                    println!("filter {filter_name:?}: {v} {f:?}");
                    drop(struct_iter);

                    array_iter.next();
                }
            }
            c => unreachable!("unexpected result entry: {c:?}"),
        }

        resp_results_iter.next();
    }

    app_state.borrow_mut().add_sprites_by_uri_list(uris);
}

#[cfg(target_os = "linux")]
pub enum PollFDType {
    AppEventBus,
    AppShellDisplay,
    BackgroundWorkerViewFeedback,
    DBusWatch(*mut dbus::WatchRef),
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
struct DBusWatcher<'e> {
    epoll: &'e platform::linux::Epoll,
    fd_pool: &'e RefCell<PollFDPool>,
    fd_to_pool_index: HashMap<core::ffi::c_int, u64>,
}
#[cfg(target_os = "linux")]
impl dbus::WatchFunction for DBusWatcher<'_> {
    fn add(&mut self, watch: &mut dbus::WatchRef) -> bool {
        if watch.enabled() {
            let mut event_type = 0;
            let flags = watch.flags();
            if flags.contains(dbus::WatchFlags::READABLE) {
                event_type |= platform::linux::EPOLLIN;
            }
            if flags.contains(dbus::WatchFlags::WRITABLE) {
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

    fn remove(&mut self, watch: &mut dbus::WatchRef) {
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

    fn toggled(&mut self, watch: &mut dbus::WatchRef) {
        let mut event_type = 0;
        let flags = watch.flags();
        if flags.contains(dbus::WatchFlags::READABLE) {
            event_type |= platform::linux::EPOLLIN;
        }
        if flags.contains(dbus::WatchFlags::WRITABLE) {
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
fn dispatch_dbus(dbus: &DBusLink) {
    while let Some(m) = dbus.underlying().pop_message() {
        let span =
            tracing::info_span!(target: "dbus_loop", "dbus message recv", r#type = m.r#type());
        let _enter = span.enter();
        if m.r#type() == dbus::MESSAGE_TYPE_METHOD_RETURN {
            // method return
            tracing::trace!(target: "dbus_loop", reply_serial = m.reply_serial(), signature = ?m.signature(), "method return data");
            wake_for_reply(m);
        } else if m.r#type() == dbus::MESSAGE_TYPE_SIGNAL {
            // signal
            tracing::trace!(target: "dbus_loop", path = ?m.path(), interface = ?m.interface(), member = ?m.member(), "signal data");
            wake_for_signal(
                m.path().unwrap().into(),
                m.interface().unwrap().into(),
                m.member().unwrap().into(),
                m,
            );
        } else {
            tracing::trace!(target: "dbus_loop", "unknown dbus message");
        }
    }
}

#[cfg(target_os = "linux")]
static mut DBUS_WAIT_FOR_REPLY_WAKERS: *mut HashMap<
    u32,
    Vec<(
        std::rc::Weak<std::cell::Cell<Option<dbus::Message>>>,
        core::task::Waker,
    )>,
> = core::ptr::null_mut();
#[cfg(target_os = "linux")]
fn wake_for_reply(reply: dbus::Message) {
    let Some(wakers) = unsafe { &mut *DBUS_WAIT_FOR_REPLY_WAKERS }.remove(&reply.reply_serial())
    else {
        return;
    };

    let wake_count = wakers.len();
    for ((sink, w), m) in wakers
        .into_iter()
        .zip(core::iter::repeat_n(reply, wake_count))
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

#[cfg(target_os = "linux")]
static mut DBUS_WAIT_FOR_SIGNAL_WAKERS: *mut HashMap<
    (std::ffi::CString, std::ffi::CString, std::ffi::CString),
    Vec<(
        std::rc::Weak<std::cell::Cell<Option<dbus::Message>>>,
        core::task::Waker,
    )>,
> = core::ptr::null_mut();
#[cfg(target_os = "linux")]
fn wake_for_signal(
    path: std::ffi::CString,
    interface: std::ffi::CString,
    member: std::ffi::CString,
    message: dbus::Message,
) {
    let Some(wakers) =
        unsafe { &mut *DBUS_WAIT_FOR_SIGNAL_WAKERS }.remove(&(path, interface, member))
    else {
        return;
    };

    let wake_count = wakers.len();
    for ((sink, w), m) in wakers
        .into_iter()
        .zip(core::iter::repeat_n(message, wake_count))
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

#[cfg(target_os = "linux")]
pub struct DBusWaitForReplyFuture {
    serial: u32,
    reply: std::rc::Rc<std::cell::Cell<Option<dbus::Message>>>,
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
    type Output = dbus::Message;

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
    message: std::rc::Rc<std::cell::Cell<Option<dbus::Message>>>,
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
    type Output = dbus::Message;

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

struct WindowCornerCutoutRenderer<'subsystem> {
    atlas_rect: AtlasRect,
    pipeline_layout: br::PipelineLayoutObject<&'subsystem Subsystem>,
    pipeline: br::PipelineObject<&'subsystem Subsystem>,
    pipeline_cont: br::PipelineObject<&'subsystem Subsystem>,
    vbuf: br::BufferObject<&'subsystem Subsystem>,
    _vbuf_memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    _input_dsl: br::DescriptorSetLayoutObject<&'subsystem Subsystem>,
    _dp: br::DescriptorPoolObject<&'subsystem Subsystem>,
    input_descriptor_set: br::DescriptorSet,
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
        skip(base_system, sampler, rendered_subpass, rendered_subpass_cont)
    )]
    pub fn new(
        base_system: &mut AppBaseSystem<'subsystem>,
        sampler: &(impl br::VkHandle<Handle = br::vk::VkSampler> + ?Sized),
        rt_size: br::Extent2D,
        rendered_subpass: br::SubpassRef<impl br::VkHandle<Handle = br::vk::VkRenderPass> + ?Sized>,
        rendered_subpass_cont: br::SubpassRef<
            impl br::VkHandle<Handle = br::vk::VkRenderPass> + ?Sized,
        >,
    ) -> Self {
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
                &RASTER_STATE_DEFAULT_FILL_NOCULL,
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
