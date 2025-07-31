macro_rules! warn_bailout_scope {
    ($label: lifetime, $x: expr, $msg: literal) => {
        match $x {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(reason = ?e, $msg);
                break $label;
            }
        }
    }
}

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
    cell::{RefCell, UnsafeCell},
    collections::{BTreeSet, HashMap, VecDeque},
    rc::Rc,
};

use crate::{
    base_system::{FontType, inject_cmd_end_render_pass2, inject_cmd_pipeline_barrier_2},
    coordinate::SizePixels,
};
use app_state::AppState;
use base_system::{AppBaseSystem, WindowCornerCutoutRenderer, prof::ProfilingContext};

use bedrock::{
    self as br, CommandBufferMut, CommandPoolMut, Device, Fence, FenceMut, InstanceChild,
    PhysicalDevice, Swapchain, VkHandle, VkHandleMut, VkObject, VkRawHandle,
};
use bg_worker::{BackgroundWorker, BackgroundWorkerViewFeedback};
use composite::{
    AnimatableColor, AnimatableFloat, AnimationCurve, CompositeMode, CompositeRect,
    CompositeRenderer, CompositeRenderingData, CompositeStreamingData, CompositeTree,
    CompositeTreeRef, RenderPassAfterOperation, RenderPassRequirements,
};
use hittest::{HitTestTreeData, HitTestTreeManager};
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
    ToplevelWindowToggleMaximizeRestoreRequest,
    MainWindowPointerMove {
        surface_x: f32,
        surface_y: f32,
    },
    MainWindowPointerLeftDown,
    MainWindowPointerLeftUp,
    MainWindowTiledStateChanged {
        is_tiled: bool,
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
    AppMenuRequestOpen,
    AppMenuRequestSave,
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
    AddSpriteByPathList(Vec<std::path::PathBuf>),
    UIShowDragAndDropOverlay,
    UIHideDragAndDropOverlay,
}

pub struct AppEventBus {
    queue: UnsafeCell<VecDeque<AppEvent>>,
    #[cfg(target_os = "linux")]
    efd: linux_eventfd::EventFD,
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

pub struct ViewInitContext<'app_system, 'subsystem> {
    pub base_system: &'app_system mut AppBaseSystem<'subsystem>,
    pub ui_scale_factor: f32,
}

pub struct PresenterInitContext<'state, 'app_system, 'subsystem> {
    pub for_view: ViewInitContext<'app_system, 'subsystem>,
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
            .text_mask(FontType::UIExtraLarge, "Drop to add")
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

    pub fn rescale(&self, base_system: &mut AppBaseSystem, ui_scale_factor: f32) {
        base_system.free_mask_atlas_rect(
            self.ct_text
                .entity(&base_system.composite_tree)
                .texatlas_rect,
        );

        let text_atlas_rect = base_system
            .text_mask(FontType::UIExtraLarge, "Drop to add")
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

    let syslink = SystemLink::new();

    let events = AppEventBus {
        queue: UnsafeCell::new(VecDeque::new()),
        #[cfg(target_os = "linux")]
        efd: linux_eventfd::EventFD::new(0, linux_eventfd::EventFDOptions::NONBLOCK).unwrap(),
        #[cfg(windows)]
        event_notify: platform::win32::event::EventObject::new(None, true, false).unwrap(),
    };

    let subsystem = Subsystem::init();
    let mut app_system = AppBaseSystem::new(&subsystem);
    let mut app_shell = AppShell::new(&events, &mut app_system as _);
    let mut app_state = RefCell::new(AppState::new());

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
        &syslink,
    );

    bg_worker.teardown();
    drop(task_worker);
}

pub struct Application<'subsystem> {
    app_header: feature::app_header::Presenter,
    app_menu: feature::app_menu::Presenter,
    editing_atlas_plane: Rc<feature::editing_atlas_renderer::Presenter<'subsystem>>,
    editing_atlas_current_bound_pipeline: RenderPassRequirements,
    sprite_list_pane: feature::sprite_list_pane::Presenter,
    dnd_overlay: DragAndDropOverlayView,
}
impl<'subsystem> Application<'subsystem> {
    pub fn new(
        init_context: &mut PresenterInitContext<'_, '_, 'subsystem>,
        composite_renderer: &CompositeRenderer<'subsystem>,
        rt_size: br::Extent2D,
        needs_window_command_buttons: bool,
    ) -> Self {
        let app_header =
            feature::app_header::Presenter::new(init_context, needs_window_command_buttons);
        let app_menu = feature::app_menu::Presenter::new(init_context, app_header.height());
        let editing_atlas_plane = Rc::new(feature::editing_atlas_renderer::Presenter::new(
            init_context,
            composite_renderer.subpass_final(),
            rt_size,
        ));
        let editing_atlas_current_bound_pipeline = RenderPassRequirements {
            after_operation: RenderPassAfterOperation::None,
            continued: false,
        };
        let sprite_list_pane =
            feature::sprite_list_pane::Presenter::new(init_context, app_header.height());

        let dnd_overlay = DragAndDropOverlayView::new(&mut init_context.for_view);

        editing_atlas_plane.mount(
            init_context.for_view.base_system,
            (CompositeTree::ROOT, HitTestTreeManager::ROOT),
        );
        sprite_list_pane.mount(
            init_context.for_view.base_system,
            CompositeTree::ROOT,
            HitTestTreeManager::ROOT,
        );
        app_menu.mount(
            init_context.for_view.base_system,
            CompositeTree::ROOT,
            HitTestTreeManager::ROOT,
        );
        app_header.mount(
            init_context.for_view.base_system,
            CompositeTree::ROOT,
            HitTestTreeManager::ROOT,
        );
        dnd_overlay.mount(init_context.for_view.base_system, CompositeTree::ROOT);

        // initial state modification
        editing_atlas_plane.set_offset(
            0.0,
            app_header.height() * init_context.for_view.ui_scale_factor,
        );

        Self {
            app_header,
            app_menu,
            editing_atlas_plane,
            editing_atlas_current_bound_pipeline,
            sprite_list_pane,
            dnd_overlay,
        }
    }

    pub fn rescale(&self, base_sys: &mut AppBaseSystem<'subsystem>, ui_scale_factor: f32) {
        self.app_header.rescale(base_sys, ui_scale_factor);
        self.app_menu.rescale(base_sys, ui_scale_factor);
        self.editing_atlas_plane.rescale(base_sys, ui_scale_factor);
        self.sprite_list_pane
            .rescale(base_sys, unsafe { SafeF32::new_unchecked(ui_scale_factor) });
        self.dnd_overlay.rescale(base_sys, ui_scale_factor);
    }

    pub fn update<'base_sys>(
        &self,
        base_sys: &'base_sys mut AppBaseSystem<'subsystem>,
        current_sec: f32,
    ) {
        self.editing_atlas_plane.update(base_sys, current_sec);
        self.app_header.update(base_sys, current_sec);
        self.app_menu.update(base_sys, current_sec);
        self.sprite_list_pane.update(base_sys, current_sec);
    }

    pub fn resize_frame(
        &self,
        size: SizePixels,
        base_sys: &AppBaseSystem<'subsystem>,
        composite_renderer: &CompositeRenderer,
    ) {
        self.editing_atlas_plane.recreate_render_resources(
            base_sys,
            composite_renderer.select_subpass(&self.editing_atlas_current_bound_pipeline),
            size.into(),
        );
    }

    pub fn needs_update_command(&self) -> bool {
        self.editing_atlas_plane.needs_update()
    }
}

fn app_main<'sys, 'event_bus, 'subsystem>(
    app_system: &'sys mut AppBaseSystem<'subsystem>,
    app_shell: &'sys mut AppShell<'event_bus, 'subsystem>,
    events: &'event_bus AppEventBus,
    app_state: &'sys mut RefCell<AppState<'subsystem>>,
    task_worker: &smol::LocalExecutor<'sys>,
    bg_worker: &BackgroundWorker<'subsystem>,
    syslink: &'sys SystemLink,
) {
    tracing::info!("Initializing Peridot SpriteAtlas Visualizer/Editor");
    let setup_timer = std::time::Instant::now();

    let mut sc = PrimaryRenderTarget::new(SubsystemBoundSurface {
        handle: match unsafe { app_shell.create_vulkan_surface(app_system.subsystem.instance()) } {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create vulkan surface");
                std::process::abort();
            }
        },
        subsystem: app_system.subsystem,
    });
    let mut composite_renderer = CompositeRenderer::new(app_system, &sc);
    let mut corner_cutout_renderer = if !app_shell.server_side_decoration_provided() {
        // window decorations should be rendered by client size(not provided by window system server)
        Some(WindowCornerCutoutRenderer::new(
            app_system,
            sc.size,
            composite_renderer.subpass_final(),
            composite_renderer.subpass_continue_final(),
        ))
    } else {
        None
    };

    let mut active_ui_scale = app_shell.ui_scale_factor();
    tracing::info!(value = active_ui_scale, "initial ui scale");

    let mut app = Application::new(
        &mut PresenterInitContext {
            for_view: ViewInitContext {
                base_system: app_system,
                ui_scale_factor: active_ui_scale,
            },
            app_state: app_state.get_mut(),
        },
        &composite_renderer,
        sc.size,
        app_shell.needs_window_command_buttons(),
    );

    tracing::debug!(
        byte_size = app_system
            .active_staging_buffer_locked()
            .total_reserved_amount(),
        "Reserved Staging Buffers during UI initialization",
    );
    app_system.hit_tree.dump(HitTestTreeManager::ROOT);

    // reordering hit for popups
    let popup_hit_layer = app_system.create_hit_tree(HitTestTreeData {
        width_adjustment_factor: 1.0,
        height_adjustment_factor: 1.0,
        ..Default::default()
    });
    app_system.set_hit_tree_parent(popup_hit_layer, HitTestTreeManager::ROOT);
    let mut popup_manager = PopupManager::new(popup_hit_layer, CompositeTree::ROOT);

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
    let mut poll_fd_pool = RefCell::new(PollFDPool::new());
    #[cfg(target_os = "linux")]
    let epoll = linux_epoll::Epoll::new(0).unwrap();
    #[cfg(target_os = "linux")]
    epoll
        .add(
            &events.efd,
            linux_epoll::EPOLLIN,
            linux_epoll::EpollData::U64(poll_fd_pool.get_mut().alloc(PollFDType::AppEventBus)),
        )
        .unwrap();
    #[cfg(target_os = "linux")]
    epoll
        .add(
            &app_shell.display_fd(),
            linux_epoll::EPOLLIN,
            linux_epoll::EpollData::U64(poll_fd_pool.get_mut().alloc(PollFDType::AppShellDisplay)),
        )
        .unwrap();
    #[cfg(target_os = "linux")]
    epoll
        .add(
            bg_worker.main_thread_waker(),
            linux_epoll::EPOLLIN,
            linux_epoll::EpollData::U64(
                poll_fd_pool
                    .get_mut()
                    .alloc(PollFDType::BackgroundWorkerViewFeedback),
            ),
        )
        .unwrap();

    #[cfg(target_os = "linux")]
    syslink.dbus.con.set_watch_functions(Box::new(DBusWatcher {
        epoll: &epoll,
        fd_pool: &poll_fd_pool,
        fd_to_pool_index: HashMap::new(),
    }));

    // initialize misc state
    let mut newsize_request = None;
    let mut last_pointer_pos = (0.0f32, 0.0f32);
    let mut last_composite_render_instructions = CompositeRenderingData {
        instructions: Vec::new(),
        render_passes: Vec::new(),
        required_backdrop_buffer_count: 0,
    };
    let mut composite_instance_buffer_dirty;

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
        [const { core::mem::MaybeUninit::<linux_epoll::epoll_event>::uninit() }; 8];
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
                        if (e.events & linux_epoll::EPOLLIN) != 0 {
                            flags |= dbus::WatchFlags::READABLE;
                        }
                        if (e.events & linux_epoll::EPOLLOUT) != 0 {
                            flags |= dbus::WatchFlags::WRITABLE;
                        }
                        if (e.events & linux_epoll::EPOLLERR) != 0 {
                            flags |= dbus::WatchFlags::ERROR;
                        }
                        if (e.events & linux_epoll::EPOLLHUP) != 0 {
                            flags |= dbus::WatchFlags::HANGUP;
                        }
                        if !watch_ptr.handle(flags) {
                            tracing::warn!(?flags, "dbus_watch_handle failed");
                        }

                        syslink.dbus.dispatch();
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
            app.rescale(app_system, active_ui_scale);
        }

        task_worker.try_tick();

        app.editing_atlas_plane.sync_with_app_state(
            app_system,
            &app_state.borrow(),
            &bg_worker.enqueue_access(),
        );

        while let Some(e) = app_update_context.event_queue.pop() {
            match e {
                AppEvent::ToplevelWindowClose => {
                    app_shell.close_safe();
                    break 'app;
                }
                AppEvent::ToplevelWindowMinimizeRequest => {
                    app_shell.minimize();
                }
                AppEvent::ToplevelWindowToggleMaximizeRestoreRequest => {
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
                        tracing::trace!(width, height, "frame resize");

                        unsafe {
                            main_cp.reset(br::CommandPoolResetFlags::EMPTY).unwrap();
                        }
                        main_cb_invalid = true;

                        sc.resize(br::Extent2D { width, height });

                        let mut descriptor_writes = Vec::new();
                        composite_renderer.recreate_rt_resources(
                            app_system,
                            &sc,
                            &mut descriptor_writes,
                        );
                        app_system
                            .subsystem
                            .update_descriptor_sets(&descriptor_writes, &[]);

                        if let Some(ref mut r) = corner_cutout_renderer {
                            r.resize_rt(
                                app_system,
                                sc.size,
                                composite_renderer.subpass_final(),
                                composite_renderer.subpass_continue_final(),
                            );
                        }

                        app.resize_frame(sc.size.into(), app_system, &composite_renderer);
                    }

                    app.update(app_system, current_sec);
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

                            composite_renderer.ready_input_backdrop_descriptor_sets(
                                composite_render_instructions.required_backdrop_buffer_count,
                            );

                            if composite_render_instructions.render_passes[0]
                                != app.editing_atlas_current_bound_pipeline
                            {
                                // editing atlas render pass changes
                                app.editing_atlas_current_bound_pipeline =
                                    composite_render_instructions.render_passes[0];
                                app.editing_atlas_plane.recreate_render_resources(
                                    app_system,
                                    composite_renderer
                                        .select_subpass(&app.editing_atlas_current_bound_pipeline),
                                    sc.size,
                                );
                            }

                            last_composite_render_instructions = composite_render_instructions;
                        }

                        composite_instance_buffer_dirty = true;
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

                    let composite_instance_buffer_dirty =
                        core::mem::replace(&mut composite_instance_buffer_dirty, false);
                    let mut needs_update =
                        composite_instance_buffer_dirty || app.needs_update_command();
                    if composite_renderer.update_backdrop_resources(app_system, &sc) {
                        needs_update = true;
                    }

                    if needs_update {
                        let _pf = _pf.scoped(ProfileMarker::UpdateWorkSubmission);

                        if last_updating {
                            last_update_command_fence.wait().unwrap();
                        }

                        let mut staging_scratch_buffers_locked = app_system.lock_staging_buffers();
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
                                    &[
                                        // Note: 0番目はbackdropなしの番兵としてつかわれるので初期化しておく
                                        br::ImageMemoryBarrier2::new(
                                            composite_renderer.default_backdrop_buffer(),
                                            br::ImageSubresourceRange::new(
                                                br::AspectMask::COLOR,
                                                0..1,
                                                0..1,
                                            ),
                                        )
                                        .transit_to(
                                            br::ImageLayout::ShaderReadOnlyOpt.from_undefined(),
                                        ),
                                    ],
                                ),
                            )
                        })
                        .inject(|r| {
                            app.editing_atlas_plane.process_dirty_data(
                                app_system.subsystem,
                                &staging_scratch_buffers_locked.active_buffer(),
                                r,
                            )
                        })
                        .end()
                        .unwrap();
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

                        staging_scratch_buffers_locked.flip_next_and_ready();
                        last_updating = true;
                    }

                    if main_cb_invalid {
                        let _pf = _pf.scoped(ProfileMarker::MainCommandBufferPopulation);

                        for (n, cb) in main_cbs.iter_mut().enumerate() {
                            unsafe { cb.begin(&br::CommandBufferBeginInfo::new()).unwrap() }
                                .inject(|r| {
                                    composite_renderer.populate_commands(
                                        r,
                                        &last_composite_render_instructions,
                                        sc.size,
                                        &sc.backbuffer_image(n),
                                        n,
                                        |token, r| {
                                            app.editing_atlas_plane
                                                .handle_custom_render(&token, sc.size, r)
                                        },
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
                    let (cw, ch) = app_shell.client_size();

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
                    let (cw, ch) = app_shell.client_size();

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
                    let (cw, ch) = app_shell.client_size();

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
                    let (cw, ch) = app_shell.client_size();

                    popup_manager.spawn(
                        &mut PresenterInitContext {
                            for_view: ViewInitContext {
                                base_system: app_system,
                                ui_scale_factor: active_ui_scale,
                            },
                            app_state: &mut *app_state.borrow_mut(),
                        },
                        t.elapsed().as_secs_f32(),
                        &content,
                    );
                    unsafe { &mut *app_shell.pointer_input_manager().get() }.recompute_enter_leave(
                        cw,
                        ch,
                        &mut app_system.hit_tree,
                        &mut app_update_context,
                        HitTestTreeManager::ROOT,
                    );
                }
                AppEvent::UIPopupClose { id } => {
                    let (cw, ch) = app_shell.client_size();

                    popup_manager.close(app_system, t.elapsed().as_secs_f32(), &id);
                    unsafe { &mut *app_shell.pointer_input_manager().get() }.recompute_enter_leave(
                        cw,
                        ch,
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
                    task_worker
                        .spawn(app_menu_on_add_sprite(
                            syslink, app_shell, events, app_state,
                        ))
                        .detach();
                }
                AppEvent::AppMenuRequestOpen => {
                    task_worker
                        .spawn(app_menu_on_open(syslink, app_shell, app_state, events))
                        .detach();
                }
                AppEvent::AppMenuRequestSave => {
                    task_worker
                        .spawn(app_menu_on_save(syslink, app_shell, app_state, events))
                        .detach();
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
                AppEvent::AddSpriteByPathList(paths) => {
                    app_state.borrow_mut().add_sprites_from_file_paths(paths);
                }
                AppEvent::UIShowDragAndDropOverlay => {
                    app.dnd_overlay.show(app_system, t.elapsed().as_secs_f32());
                }
                AppEvent::UIHideDragAndDropOverlay => {
                    app.dnd_overlay.hide(app_system, t.elapsed().as_secs_f32());
                }
                AppEvent::MainWindowTiledStateChanged { is_tiled } => {
                    app.app_header.on_shell_tiling_changed(app_system, is_tiled);
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

async fn app_menu_on_add_sprite<'subsystem>(
    syslink: &SystemLink,
    shell: &AppShell<'_, 'subsystem>,
    events: &AppEventBus,
    app_state: &RefCell<AppState<'subsystem>>,
) {
    let added_paths = match syslink.select_sprite_files(shell).await {
        Ok(x) => x,
        Err(e) => {
            e.ui_feedback(events);
            return;
        }
    };

    app_state
        .borrow_mut()
        .add_sprites_from_file_paths(added_paths);
}

async fn app_menu_on_open<'sys, 'subsystem>(
    syslink: &'sys SystemLink,
    shell: &'sys AppShell<'_, 'subsystem>,
    app_state: &'sys RefCell<AppState<'subsystem>>,
    event_bus: &AppEventBus,
) {
    let path = match syslink.select_open_file(shell).await {
        Ok(Some(x)) => x,
        Ok(None) => return,
        Err(e) => {
            e.ui_feedback(event_bus);
            return;
        }
    };

    if app_state.borrow_mut().load(&path).is_err() {
        event_bus.push(AppEvent::UIMessageDialogRequest {
            content: "Opening failed".into(),
        });
    }
}

async fn app_menu_on_save<'sys, 'subsystem>(
    syslink: &'sys SystemLink,
    shell: &'sys AppShell<'_, 'subsystem>,
    app_state: &'sys RefCell<AppState<'subsystem>>,
    event_bus: &AppEventBus,
) {
    let path = match syslink.select_save_file(shell).await {
        Ok(Some(x)) => x,
        Ok(None) => return,
        Err(e) => {
            e.ui_feedback(event_bus);
            return;
        }
    };

    if app_state.borrow_mut().save(&path).is_err() {
        event_bus.push(AppEvent::UIMessageDialogRequest {
            content: "Saving failed".into(),
        });
    }
}

#[cfg(unix)]
pub struct DesktopPortal {
    file_chooser: smol::lock::OnceCell<Option<DesktopPortalFileChooser>>,
}
#[cfg(unix)]
impl DesktopPortal {
    pub fn new() -> Self {
        Self {
            file_chooser: smol::lock::OnceCell::new(),
        }
    }

    #[tracing::instrument(name = "DesktopPortal::try_get_file_chooser", skip(self, dbus))]
    pub async fn try_get_file_chooser<'x>(
        &'x self,
        dbus: &DBusLink,
    ) -> Option<&'x DesktopPortalFileChooser> {
        self.file_chooser.get_or_init(|| async {
            let reply_msg = dbus
                .wait_for_reply(
                    dbus_proto::introspect(
                        dbus.underlying(),
                        Some(c"org.freedesktop.portal.Desktop"),
                        c"/org/freedesktop/portal/desktop",
                    )
                )
                .await;
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

            let version = match desktop_portal_proto::file_chooser::read_get_version_reply(
                dbus.wait_for_reply(desktop_portal_proto::file_chooser::get_version(dbus.underlying())).await
            ) {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!(reason = ?e, "FileChooser get version failed, assuming v1");
                    1
                }
            };

            has_file_chooser.then_some(DesktopPortalFileChooser { version })
        }).await.as_ref()
    }

    pub const fn open_request_object(
        path: desktop_portal_proto::ObjectPath,
    ) -> DesktopPortalRequestObject {
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
pub struct DesktopPortalFileChooser {
    #[allow(dead_code)]
    version: u32,
}
#[cfg(unix)]
impl DesktopPortalFileChooser {
    /// https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html#org-freedesktop-portal-filechooser-openfile
    pub async fn open_file(
        &self,
        dbus: &DBusLink,
        parent_window: Option<&core::ffi::CStr>,
        title: &core::ffi::CStr,
        options_builder: impl FnOnce(desktop_portal_proto::file_chooser::OpenFileOptionsAppender),
    ) -> Result<DesktopPortalRequestObject, dbus::Error> {
        Ok(DesktopPortal::open_request_object(
            desktop_portal_proto::file_chooser::read_open_file_reply(
                dbus.wait_for_reply(desktop_portal_proto::file_chooser::open_file(
                    dbus.underlying(),
                    parent_window,
                    title,
                    options_builder,
                ))
                .await,
            )?,
        ))
    }

    /// https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html#org-freedesktop-portal-filechooser-savefile
    pub async fn save_file(
        &self,
        dbus: &DBusLink,
        parent_window: Option<&core::ffi::CStr>,
        title: &core::ffi::CStr,
        options_builder: impl FnOnce(desktop_portal_proto::file_chooser::SaveFileOptionsAppender),
    ) -> Result<DesktopPortalRequestObject, dbus::Error> {
        Ok(DesktopPortal::open_request_object(
            desktop_portal_proto::file_chooser::read_save_file_reply(
                dbus.wait_for_reply(desktop_portal_proto::file_chooser::save_file(
                    dbus.underlying(),
                    parent_window,
                    title,
                    options_builder,
                ))
                .await,
            )?,
        ))
    }
}

#[cfg(unix)]
pub struct DesktopPortalRequestObject(desktop_portal_proto::ObjectPath);
#[cfg(unix)]
impl DesktopPortalRequestObject {
    pub const fn new(object_path: desktop_portal_proto::ObjectPath) -> Self {
        Self(object_path)
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

        Self::new(unsafe {
            core::mem::transmute(std::ffi::CString::from_vec_unchecked(
                object_path.into_bytes(),
            ))
        })
    }

    #[inline(always)]
    pub fn points_same_object(&self, other: &Self) -> bool {
        self.0 == other.0
    }

    #[inline]
    pub fn wait_for_response<'link>(
        &self,
        dbus: &'link DBusLink,
    ) -> DBusWaitForSignalFuture<'link> {
        dbus.wait_for_signal(
            std::rc::Rc::from(self.0.as_c_str()),
            std::rc::Rc::from(c"org.freedesktop.portal.Request"),
            std::rc::Rc::from(c"Response"),
        )
    }
}

#[cfg(windows)]
#[derive(Debug, thiserror::Error)]
pub enum SystemLinkError {
    #[error("unrecoverable exception: {0:?}")]
    UnrecoverableException(windows::core::Error),
}
#[cfg(windows)]
impl SystemLinkError {
    pub fn ui_feedback(self, events: &AppEventBus) {
        match self {
            Self::UnrecoverableException(_) => {
                events.push(AppEvent::UIMessageDialogRequest {
                    content: "Operation failed".into(),
                });
            }
        }
    }
}

#[cfg(windows)]
macro_rules! syslink_unrecoverable {
    ($x: expr, $msg: literal) => {
        match $x {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, $msg);
                return Err(SystemLinkError::UnrecoverableException(e));
            }
        }
    }
}

#[cfg(windows)]
pub struct SystemLink {}
#[cfg(windows)]
impl SystemLink {
    pub fn new() -> Self {
        Self {}
    }

    #[tracing::instrument(
        name = "SystemLink::select_sprite_files",
        skip(self, for_shell),
        ret(Debug)
    )]
    pub async fn select_sprite_files(
        &self,
        for_shell: &AppShell<'_, '_>,
    ) -> Result<Vec<std::path::PathBuf>, SystemLinkError> {
        let picker = syslink_unrecoverable!(
            windows::Storage::Pickers::FileOpenPicker::new(),
            "FileOpenPicker::new failed"
        );
        syslink_unrecoverable!(
            unsafe {
                syslink_unrecoverable!(
                    windows::core::Interface::cast::<
                        windows::Win32::UI::Shell::IInitializeWithWindow,
                    >(&picker),
                    "querying IInitializeWithWindow failed"
                )
                .Initialize(for_shell.hwnd())
            },
            "picker initialization failed"
        );

        'try_set_filter: {
            warn_bailout_scope!(
                'try_set_filter,
                warn_bailout_scope!(
                    'try_set_filter,
                    picker.FileTypeFilter(),
                    "getting FileTypeFilter failed"
                )
                .Append(windows::core::h!(".png")),
                "appending filter failed"
            );
        }

        let files = syslink_unrecoverable!(
            syslink_unrecoverable!(
                picker.PickMultipleFilesAsync(),
                "FileOpenPicker.PickMultipleFilesAsync failed"
            )
            .await,
            "Error while PickMultipleFilesAsync operation"
        );

        let mut paths = match files.Size() {
            Ok(count) => Vec::with_capacity(count as _),
            Err(e) => {
                tracing::warn!(reason = ?e, "getting file count failed, operation may be suboptimal");
                Vec::new()
            }
        };
        let files_iter = syslink_unrecoverable!(files.First(), "getting files iterator failed");
        while files_iter.HasCurrent().unwrap_or_else(|e| {
            tracing::warn!(reason = ?e, "getting HasCurrent failed, canceling iteration");
            false
        }) {
            'try_get_file_path: {
                let current = match files_iter.Current() {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::warn!(reason = ?e, "getting Current element failed, skipping");
                        break 'try_get_file_path;
                    }
                };
                let path = match current.Path() {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::warn!(reason = ?e, "getting Path failed, skipping");
                        break 'try_get_file_path;
                    }
                };
                paths.push(path.to_os_string().into());
            }

            syslink_unrecoverable!(files_iter.MoveNext(), "moving to next failed");
        }

        Ok(paths)
    }

    #[tracing::instrument(
        name = "SystemLink::select_open_file",
        skip(self, for_shell),
        ret(Debug)
    )]
    pub async fn select_open_file(
        &self,
        for_shell: &AppShell<'_, '_>,
    ) -> Result<Option<std::path::PathBuf>, SystemLinkError> {
        let picker = syslink_unrecoverable!(
            windows::Storage::Pickers::FileOpenPicker::new(),
            "FileOpenPiciker::new failed"
        );
        syslink_unrecoverable!(
            unsafe {
                syslink_unrecoverable!(
                    windows::core::Interface::cast::<
                        windows::Win32::UI::Shell::IInitializeWithWindow,
                    >(&picker,),
                    "querying IInitializeWithWindow failed"
                )
                .Initialize(for_shell.hwnd())
            },
            "picker initialization failed"
        );

        'try_set_filter: {
            warn_bailout_scope!(
                'try_set_filter,
                warn_bailout_scope!(
                    'try_set_filter,
                    picker.FileTypeFilter(),
                    "getting FileTypeFilter failed"
                )
                .Append(windows::core::h!(".psa")),
                "appending filter failed"
            );
        }

        let file = match syslink_unrecoverable!(
            picker.PickSingleFileAsync(),
            "PickSingleFileAsync failed"
        )
        .await
        {
            Ok(x) => x,
            Err(e) if e.code() == windows::Win32::Foundation::S_OK => {
                tracing::warn!("Operation was cancelled");
                return Ok(None);
            }
            Err(e) => {
                tracing::error!(reason = ?e, "FileOpenPicker.PickSingleFileAsync failed");
                return Err(SystemLinkError::UnrecoverableException(e));
            }
        };
        Ok(Some(
            syslink_unrecoverable!(file.Path(), "getting path failed")
                .to_os_string()
                .into(),
        ))
    }

    #[tracing::instrument(
        name = "SystemLink::select_save_file",
        skip(self, for_shell),
        ret(Debug)
    )]
    pub async fn select_save_file(
        &self,
        for_shell: &AppShell<'_, '_>,
    ) -> Result<Option<std::path::PathBuf>, SystemLinkError> {
        let picker = syslink_unrecoverable!(
            windows::Storage::Pickers::FileSavePicker::new(),
            "FileSavePicker::new failed"
        );
        syslink_unrecoverable!(
            unsafe {
                syslink_unrecoverable!(
                    windows::core::Interface::cast::<
                        windows::Win32::UI::Shell::IInitializeWithWindow,
                    >(&picker,),
                    "querying IInitializeWithWindow failed"
                )
                .Initialize(for_shell.hwnd())
            },
            "picker initialization failed"
        );

        syslink_unrecoverable!(
            syslink_unrecoverable!(picker.FileTypeChoices(), "getting FileTypeChoices failed")
                .Insert(
                    windows::core::h!("Peridot Sprite Atlas asset"),
                    &windows_collections::IVector::from(shell::win32::ReadOnlySliceAsVector(&[
                        windows::core::HSTRING::from(".psa"),
                    ])),
                ),
            "inserting filter failed"
        );

        let file =
            match syslink_unrecoverable!(picker.PickSaveFileAsync(), "PickSaveFileAsync failed")
                .await
            {
                Ok(x) => x,
                Err(e) if e.code() == windows::Win32::Foundation::S_OK => {
                    tracing::warn!("Operation was cancelled");
                    return Ok(None);
                }
                Err(e) => {
                    tracing::error!(reason = ?e, "FileSavePicker.PickSaveFileAsync failed");
                    return Err(SystemLinkError::UnrecoverableException(e));
                }
            };
        Ok(Some(
            syslink_unrecoverable!(file.Path(), "getting path failed")
                .to_os_string()
                .into(),
        ))
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, thiserror::Error)]
pub enum SelectSpriteFilesError {
    #[error("No org.freedesktop.portal.FileChooser found")]
    NoFileChooser,
    #[error("FileChooser.OpenFile failed: {0:?}")]
    OpenFileFailed(dbus::Error),
    #[error("FileChooser.SaveFile failed: {0:?}")]
    SaveFileFailed(dbus::Error),
}
#[cfg(target_os = "linux")]
impl SelectSpriteFilesError {
    pub fn ui_feedback(self, events: &AppEventBus) {
        match self {
            Self::NoFileChooser => {
                events.push(AppEvent::UIMessageDialogRequest {
                    content: "org.freedesktop.portal.FileChooser not found".into(),
                });
            }
            Self::OpenFileFailed(_) => {
                events.push(AppEvent::UIMessageDialogRequest {
                    content: "FileChooser.OpenFile failed".into(),
                });
            }
            Self::SaveFileFailed(_) => {
                events.push(AppEvent::UIMessageDialogRequest {
                    content: "FileChooser.SaveFile failed".into(),
                });
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub struct SystemLink {
    dbus: DBusLink,
    dp: DesktopPortal,
}
#[cfg(target_os = "linux")]
impl SystemLink {
    pub fn new() -> Self {
        Self {
            dbus: DBusLink::new(),
            dp: DesktopPortal::new(),
        }
    }

    #[tracing::instrument(
        name = "SystemLink::select_sprite_files",
        skip(self, for_shell),
        ret(Debug)
    )]
    pub async fn select_sprite_files(
        &self,
        for_shell: &AppShell<'_, '_>,
    ) -> Result<Vec<std::path::PathBuf>, SelectSpriteFilesError> {
        let file_chooser = self
            .dp
            .try_get_file_chooser(&self.dbus)
            .await
            .ok_or(SelectSpriteFilesError::NoFileChooser)?;

        let dialog_token = uuid::Uuid::new_v4().as_simple().to_string();
        let mut request_object =
            DesktopPortal::open_request_object_for_token(&self.dbus, &dialog_token);

        let exported_shell = for_shell.try_export_toplevel();
        let request_handle = file_chooser
            .open_file(
                &self.dbus,
                exported_shell.as_ref().map(|x| x.handle.as_c_str()),
                c"Add Sprite",
                |mut options_appender| {
                    options_appender.append_handle_token(
                        &std::ffi::CString::new(dialog_token.clone()).unwrap(),
                    );
                    options_appender.append_multiple(true);
                    options_appender.append_current_filter(
                        c"Images",
                        [desktop_portal_proto::file_chooser::Filter::MIME(
                            c"image/*".into(),
                        )],
                    );
                    options_appender.append_filters([
                        (
                            c"Images",
                            [desktop_portal_proto::file_chooser::Filter::MIME(
                                c"image/*".into(),
                            )],
                        ),
                        (
                            c"All files",
                            [desktop_portal_proto::file_chooser::Filter::Glob(
                                c"*.*".into(),
                            )],
                        ),
                    ]);
                },
            )
            .await
            .map_err(SelectSpriteFilesError::OpenFileFailed)?;
        if !request_object.points_same_object(&request_handle) {
            tracing::debug!(
                open_file_dialog_handle = ?request_handle.0,
                request_object_path = ?request_object.0,
                "returned object_path did not match with the expected, switching request object..."
            );
            request_object = request_handle;
        }
        let resp = request_object.wait_for_response(&self.dbus).await;
        drop(exported_shell);

        let mut resp_iter = resp.iter();
        let response = desktop_portal_proto::RequestResponseCode::read(&resp_iter);
        if response != desktop_portal_proto::RequestResponseCode::Success {
            tracing::warn!(?response, "Operation was cancelled");
            return Ok(Vec::new());
        }

        resp_iter.next();
        let res = desktop_portal_proto::file_chooser::ResponseResults::read_all(&mut resp_iter);
        Ok(res
            .uris
            .into_iter()
            .map(|x| {
                std::path::PathBuf::from(desktop_portal_proto::file_chooser::uri_path_part(
                    dbus_proto::cstr2str(&x),
                ))
            })
            .collect())
    }

    #[tracing::instrument(
        name = "SystemLink::select_open_file",
        skip(self, for_shell),
        ret(Debug)
    )]
    pub async fn select_open_file(
        &self,
        for_shell: &AppShell<'_, '_>,
    ) -> Result<Option<std::path::PathBuf>, SelectSpriteFilesError> {
        let file_chooser = self
            .dp
            .try_get_file_chooser(&self.dbus)
            .await
            .ok_or(SelectSpriteFilesError::NoFileChooser)?;

        let dialog_token = uuid::Uuid::new_v4().as_simple().to_string();
        let mut request_object =
            DesktopPortal::open_request_object_for_token(&self.dbus, &dialog_token);

        let exported_shell = for_shell.try_export_toplevel();
        let request_handle = file_chooser
            .open_file(
                &self.dbus,
                exported_shell.as_ref().map(|x| x.handle.as_c_str()),
                c"Open",
                |mut options_appender| {
                    options_appender.append_handle_token(
                        &std::ffi::CString::new(dialog_token.clone()).unwrap(),
                    );
                    options_appender.append_multiple(false);
                    options_appender.append_current_filter(
                        c"Peridot Sprite Atlas asset",
                        [desktop_portal_proto::file_chooser::Filter::Glob(
                            c"*.psa".into(),
                        )],
                    );
                },
            )
            .await
            .map_err(SelectSpriteFilesError::OpenFileFailed)?;
        if !request_object.points_same_object(&request_handle) {
            tracing::debug!(
                open_file_dialog_handle = ?request_handle.0,
                request_object_path = ?request_object.0,
                "returned object_path did not match with the expected, switching request object..."
            );
            request_object = request_handle;
        }
        let resp = request_object.wait_for_response(&self.dbus).await;
        drop(exported_shell);

        let mut resp_iter = resp.iter();
        let response = desktop_portal_proto::RequestResponseCode::read(&resp_iter);
        if response != desktop_portal_proto::RequestResponseCode::Success {
            tracing::warn!(?response, "Operation was cancelled");
            return Ok(None);
        }

        resp_iter.next();
        let res = desktop_portal_proto::file_chooser::ResponseResults::read_all(&mut resp_iter);
        match res.uris[..] {
            [] => Ok(None),
            [ref uri, ..] => Ok(Some(std::path::PathBuf::from(
                desktop_portal_proto::file_chooser::uri_path_part(dbus_proto::cstr2str(uri)),
            ))),
        }
    }

    #[tracing::instrument(
        name = "SystemLink::select_save_file",
        skip(self, for_shell),
        ret(Debug)
    )]
    pub async fn select_save_file(
        &self,
        for_shell: &AppShell<'_, '_>,
    ) -> Result<Option<std::path::PathBuf>, SelectSpriteFilesError> {
        let file_chooser = self
            .dp
            .try_get_file_chooser(&self.dbus)
            .await
            .ok_or(SelectSpriteFilesError::NoFileChooser)?;

        let dialog_token = uuid::Uuid::new_v4().as_simple().to_string();
        let mut request_object =
            DesktopPortal::open_request_object_for_token(&self.dbus, &dialog_token);

        let exported_shell = for_shell.try_export_toplevel();
        let request_handle = file_chooser
            .save_file(
                &self.dbus,
                exported_shell.as_ref().map(|x| x.handle.as_c_str()),
                c"Save",
                |mut options_appender| {
                    options_appender.append_handle_token(
                        &std::ffi::CString::new(dialog_token.clone()).unwrap(),
                    );
                    options_appender.append_filters([(
                        c"Peridot Sprite Atlas asset",
                        [desktop_portal_proto::file_chooser::Filter::Glob(
                            c"*.psa".into(),
                        )],
                    )]);
                },
            )
            .await
            .map_err(SelectSpriteFilesError::SaveFileFailed)?;
        if !request_object.points_same_object(&request_handle) {
            tracing::debug!(
                open_file_dialog_handle = ?request_handle.0,
                request_object_path = ?request_object.0,
                "returned object_path did not match with the expected, switching request object..."
            );
            request_object = request_handle;
        }
        let resp = request_object.wait_for_response(&self.dbus).await;
        drop(exported_shell);

        let mut resp_iter = resp.iter();
        let response = desktop_portal_proto::RequestResponseCode::read(&resp_iter);
        if response != desktop_portal_proto::RequestResponseCode::Success {
            tracing::warn!(?response, "Operation was cancelled");
            return Ok(None);
        }

        resp_iter.next();
        let res = desktop_portal_proto::file_chooser::ResponseResults::read_all(&mut resp_iter);
        match res.uris[..] {
            [] => Ok(None),
            // TODO: add ext at here, if needed
            [ref uri, ..] => Ok(Some(std::path::PathBuf::from(
                desktop_portal_proto::file_chooser::uri_path_part(dbus_proto::cstr2str(uri)),
            ))),
        }
    }
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
    epoll: &'e linux_epoll::Epoll,
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
                event_type |= linux_epoll::EPOLLIN;
            }
            if flags.contains(dbus::WatchFlags::WRITABLE) {
                event_type |= linux_epoll::EPOLLOUT;
            }

            let pool_index = self
                .fd_pool
                .borrow_mut()
                .alloc(PollFDType::DBusWatch(watch as *mut _));

            self.epoll
                .add(
                    &watch.as_raw_fd(),
                    event_type,
                    linux_epoll::EpollData::U64(pool_index),
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
            event_type |= linux_epoll::EPOLLIN;
        }
        if flags.contains(dbus::WatchFlags::WRITABLE) {
            event_type |= linux_epoll::EPOLLOUT;
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
                    linux_epoll::EpollData::U64(pool_index),
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
pub struct DBusLink {
    con: dbus::Connection,
    wait_for_reply_wakers: RefCell<
        HashMap<
            u32,
            Vec<(
                std::rc::Weak<std::cell::Cell<Option<dbus::Message>>>,
                core::task::Waker,
            )>,
        >,
    >,
    wait_for_signal_wakers: RefCell<
        HashMap<
            (
                std::rc::Rc<core::ffi::CStr>,
                std::rc::Rc<core::ffi::CStr>,
                std::rc::Rc<core::ffi::CStr>,
            ),
            Vec<(
                std::rc::Weak<std::cell::Cell<Option<dbus::Message>>>,
                core::task::Waker,
            )>,
        >,
    >,
}
#[cfg(target_os = "linux")]
impl DBusLink {
    pub fn new() -> Self {
        Self {
            con: dbus::Connection::connect_bus(dbus::BusType::Session).unwrap(),
            wait_for_reply_wakers: RefCell::new(HashMap::new()),
            wait_for_signal_wakers: RefCell::new(HashMap::new()),
        }
    }

    #[inline(always)]
    pub fn underlying(&self) -> &dbus::Connection {
        &self.con
    }

    pub async fn send(&self, mut msg: dbus::Message) -> Option<dbus::Message> {
        let Some(serial) = self.con.send_with_serial(&mut msg) else {
            return None;
        };

        Some(self.wait_for_reply(serial).await)
    }

    #[inline]
    pub fn wait_for_reply<'link>(&'link self, serial: u32) -> DBusWaitForReplyFuture<'link> {
        DBusWaitForReplyFuture::new(self, serial)
    }

    #[inline]
    pub fn wait_for_signal<'link>(
        &'link self,
        object_path: std::rc::Rc<core::ffi::CStr>,
        interface: std::rc::Rc<core::ffi::CStr>,
        member: std::rc::Rc<core::ffi::CStr>,
    ) -> DBusWaitForSignalFuture<'link> {
        DBusWaitForSignalFuture::new(self, object_path, interface, member)
    }

    fn dispatch(&self) {
        while let Some(m) = self.con.pop_message() {
            let span =
                tracing::info_span!(target: "dbus_loop", "dbus message recv", r#type = m.r#type());
            let _enter = span.enter();
            if m.r#type() == dbus::MESSAGE_TYPE_METHOD_RETURN {
                // method return
                tracing::trace!(target: "dbus_loop", reply_serial = m.reply_serial(), signature = ?m.signature(), "method return data");
                self.wake_for_reply(m);
            } else if m.r#type() == dbus::MESSAGE_TYPE_SIGNAL {
                // signal
                tracing::trace!(target: "dbus_loop", path = ?m.path(), interface = ?m.interface(), member = ?m.member(), "signal data");
                self.wake_for_signal(
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

    pub fn register_wait_for_reply(
        &self,
        serial: u32,
        reply_sink: &std::rc::Rc<std::cell::Cell<Option<dbus::Message>>>,
        waker: &core::task::Waker,
    ) {
        self.wait_for_reply_wakers
            .borrow_mut()
            .entry(serial)
            .or_insert_with(Vec::new)
            .push((std::rc::Rc::downgrade(reply_sink), waker.clone()));
    }

    fn wake_for_reply(&self, reply: dbus::Message) {
        let Some(wakers) = self
            .wait_for_reply_wakers
            .borrow_mut()
            .remove(&reply.reply_serial())
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

    pub fn register_wait_for_signal(
        &self,
        key: (
            std::rc::Rc<core::ffi::CStr>,
            std::rc::Rc<core::ffi::CStr>,
            std::rc::Rc<core::ffi::CStr>,
        ),
        signal_sink: &std::rc::Rc<std::cell::Cell<Option<dbus::Message>>>,
        waker: &core::task::Waker,
    ) {
        self.wait_for_signal_wakers
            .borrow_mut()
            .entry(key)
            .or_insert_with(Vec::new)
            .push((std::rc::Rc::downgrade(signal_sink), waker.clone()));
    }

    fn wake_for_signal(
        &self,
        path: std::rc::Rc<core::ffi::CStr>,
        interface: std::rc::Rc<core::ffi::CStr>,
        member: std::rc::Rc<core::ffi::CStr>,
        message: dbus::Message,
    ) {
        let Some(wakers) = self
            .wait_for_signal_wakers
            .borrow_mut()
            .remove(&(path, interface, member))
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
}

#[cfg(target_os = "linux")]
pub struct DBusWaitForReplyFuture<'link> {
    link: &'link DBusLink,
    serial: u32,
    reply: std::rc::Rc<std::cell::Cell<Option<dbus::Message>>>,
}
#[cfg(target_os = "linux")]
impl<'link> DBusWaitForReplyFuture<'link> {
    pub fn new(link: &'link DBusLink, serial: u32) -> Self {
        Self {
            link,
            serial,
            reply: std::rc::Rc::new(std::cell::Cell::new(None)),
        }
    }
}
#[cfg(target_os = "linux")]
impl core::future::Future for DBusWaitForReplyFuture<'_> {
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
                this.link
                    .register_wait_for_reply(this.serial, &this.reply, cx.waker());

                core::task::Poll::Pending
            }
            Some(x) => core::task::Poll::Ready(x),
        }
    }
}

#[cfg(target_os = "linux")]
pub struct DBusWaitForSignalFuture<'link> {
    link: &'link DBusLink,
    key: (
        std::rc::Rc<std::ffi::CStr>,
        std::rc::Rc<std::ffi::CStr>,
        std::rc::Rc<std::ffi::CStr>,
    ),
    message: std::rc::Rc<std::cell::Cell<Option<dbus::Message>>>,
}
#[cfg(target_os = "linux")]
impl<'link> DBusWaitForSignalFuture<'link> {
    pub fn new(
        link: &'link DBusLink,
        object_path: std::rc::Rc<std::ffi::CStr>,
        interface: std::rc::Rc<std::ffi::CStr>,
        member: std::rc::Rc<std::ffi::CStr>,
    ) -> Self {
        Self {
            link,
            key: (object_path, interface, member),
            message: std::rc::Rc::new(std::cell::Cell::new(None)),
        }
    }
}
#[cfg(target_os = "linux")]
impl core::future::Future for DBusWaitForSignalFuture<'_> {
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
                this.link
                    .register_wait_for_signal(this.key.clone(), &this.message, cx.waker());

                core::task::Poll::Pending
            }
            Some(x) => core::task::Poll::Ready(x),
        }
    }
}
