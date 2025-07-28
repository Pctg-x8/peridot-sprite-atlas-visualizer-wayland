//! UI Rect Compositioning

use std::collections::{BTreeSet, HashMap};

use bedrock::{
    self as br, DescriptorPoolMut, Device, DeviceChildHandle, Image, ImageChild, MemoryBound,
    RenderPass, ShaderModule, TypedVulkanStructure, VkHandle,
};

use crate::{
    AppEvent, AppEventBus, BLEND_STATE_SINGLE_NONE, IA_STATE_TRILIST, MS_STATE_EMPTY,
    PrimaryRenderTarget, RASTER_STATE_DEFAULT_FILL_NOCULL, VI_STATE_EMPTY,
    atlas::{AtlasRect, DynamicAtlasManager},
    base_system::{
        AppBaseSystem, inject_cmd_begin_render_pass2, inject_cmd_end_render_pass2,
        inject_cmd_pipeline_barrier_2,
    },
    helper_types::SafeF32,
    mathext::Matrix4,
    subsystem::Subsystem,
};

pub const BLUR_SAMPLE_STEPS: usize = 4;

#[repr(C)]
pub struct CompositeInstanceData {
    /// scale_x(width), scale_y(height), translate_x(left), translate_y(top)
    pub pos_st: [f32; 4],
    pub uv_st: [f32; 4],
    pub position_modifier_matrix: [f32; 4 * 4],
    /// left, top, right, bottom (pixels from edge)
    pub slice_borders: [f32; 4],
    /// tex_size_w_px, tex_size_h_px, composite_mode, opacity
    pub tex_size_pixels_composite_mode_opacity: [f32; 4],
    pub color_tint: [f32; 4],
    /// start_sec, end_sec, to_value(fromはpos_stに設定されている値), reserved
    pub pos_x_animation_data: [f32; 4],
    /// x_p1x, x_p1y, x_p2x, x_p2y
    pub pos_x_curve_control_points: [f32; 4],
    /// start_sec, end_sec, to_value(fromはpos_stに設定されている値), reserved
    pub pos_y_animation_data: [f32; 4],
    /// y_p1x, y_p1y, y_p2x, y_p2y
    pub pos_y_curve_control_points: [f32; 4],
    /// start_sec, end_sec, to_value(fromはpos_stに設定されている値), reserved
    pub pos_width_animation_data: [f32; 4],
    /// w_p1x, w_p1y, w_p2x, w_p2y
    pub pos_width_curve_control_points: [f32; 4],
    /// start_sec, end_sec, to_value(fromはpos_stに設定されている値), reserved
    pub pos_height_animation_data: [f32; 4],
    /// h_p1x, h_p1y, h_p2x, h_p2y
    pub pos_height_curve_control_points: [f32; 4],
}

pub const COMPOSITE_PUSH_CONSTANT_RANGES: &'static [br::PushConstantRange] = &[
    // { screen_x_pixels: f32, screen_y_pixels: f32 }
    br::PushConstantRange::new(br::vk::VK_SHADER_STAGE_VERTEX_BIT, 0..8),
    // { rect_mask_left: f32, rect_mask_top: f32, rect_mask_right: f32, rect_mask_bottom: f32, rect_mask_left_softness: f32, rect_mask_top_softness: f32, rect_mask_right_softness: f32, rect_mask_bottom_softness: f32 }
    br::PushConstantRange::new(br::vk::VK_SHADER_STAGE_FRAGMENT_BIT, 16..48),
];

#[repr(C)]
pub struct CompositeStreamingData {
    pub current_sec: f32,
}

pub enum CompositeMode {
    DirectSourceOver,
    ColorTint(AnimatableColor),
    FillColor(AnimatableColor),
    ColorTintBackdropBlur(AnimatableColor, AnimatableFloat),
    FillColorBackdropBlur(AnimatableColor, AnimatableFloat),
}
impl CompositeMode {
    const fn shader_mode_value(&self) -> f32 {
        match self {
            Self::DirectSourceOver => 0.0,
            Self::ColorTint(_) => 1.0,
            Self::FillColor(_) => 2.0,
            Self::ColorTintBackdropBlur(_, _) => 3.0,
            Self::FillColorBackdropBlur(_, _) => 4.0,
        }
    }
}

const fn lerp(x: f32, a: f32, b: f32) -> f32 {
    a + (b - a) * x
}

const fn lerp4(x: f32, [a, c, e, g]: [f32; 4], [b, d, f, h]: [f32; 4]) -> [f32; 4] {
    [lerp(x, a, b), lerp(x, c, d), lerp(x, e, f), lerp(x, g, h)]
}

// TODO: このへんうまくまとめたいが......

pub enum FloatParameter {
    Value(f32),
    Animated {
        start_sec: f32,
        end_sec: f32,
        from_value: f32,
        to_value: f32,
        curve: AnimationCurve,
        event_on_complete: Option<AppEvent>,
    },
}
impl FloatParameter {
    pub fn evaluate(&self, current_sec: f32) -> f32 {
        match self {
            &Self::Value(x) => x,
            &Self::Animated {
                from_value,
                to_value,
                start_sec,
                end_sec,
                ref curve,
                ..
            } => lerp(
                curve.interpolate((current_sec - start_sec) / (end_sec - start_sec)),
                from_value,
                to_value,
            ),
        }
    }
}

pub enum AnimatableFloat {
    Value(f32),
    Expression(Box<dyn Fn(&CompositeTreeParameterStore) -> f32>),
    Animated {
        start_sec: f32,
        end_sec: f32,
        from_value: f32,
        to_value: f32,
        curve: AnimationCurve,
        event_on_complete: Option<AppEvent>,
    },
}
impl AnimatableFloat {
    pub fn evaluate(&self, current_sec: f32, parameter_store: &CompositeTreeParameterStore) -> f32 {
        match self {
            &Self::Value(x) => x,
            &Self::Expression(ref x) => x(parameter_store),
            &Self::Animated {
                from_value,
                to_value,
                start_sec,
                end_sec,
                ref curve,
                ..
            } => lerp(
                curve.interpolate((current_sec - start_sec) / (end_sec - start_sec)),
                from_value,
                to_value,
            ),
        }
    }

    fn process_on_complete(&mut self, current_sec: f32, q: &AppEventBus) {
        if let &mut Self::Animated {
            end_sec,
            ref mut event_on_complete,
            ..
        } = self
            && end_sec <= current_sec
        {
            if let Some(e) = event_on_complete.take() {
                q.push(e);
            }
        }
    }
}

pub enum AnimatableColor {
    Value([f32; 4]),
    Expression(Box<dyn Fn(&CompositeTreeParameterStore) -> [f32; 4]>),
    Animated {
        start_sec: f32,
        end_sec: f32,
        from_value: [f32; 4],
        to_value: [f32; 4],
        curve: AnimationCurve,
        event_on_complete: Option<AppEvent>,
    },
}
impl AnimatableColor {
    pub fn evaluate(
        &self,
        current_sec: f32,
        parameter_store: &CompositeTreeParameterStore,
    ) -> [f32; 4] {
        match self {
            &Self::Value(x) => x,
            &Self::Expression(ref f) => f(parameter_store),
            &Self::Animated {
                from_value,
                to_value,
                start_sec,
                end_sec,
                ref curve,
                ..
            } => lerp4(
                curve.interpolate((current_sec - start_sec) / (end_sec - start_sec)),
                from_value,
                to_value,
            ),
        }
    }

    fn process_on_complete(&mut self, current_sec: f32, q: &AppEventBus) {
        if let &mut Self::Animated {
            end_sec,
            ref mut event_on_complete,
            ..
        } = self
            && end_sec <= current_sec
        {
            if let Some(e) = event_on_complete.take() {
                q.push(e);
            }
        }
    }
}

#[derive(Clone)]
pub enum AnimationCurve {
    Linear,
    CubicBezier { p1: (f32, f32), p2: (f32, f32) },
}
impl AnimationCurve {
    #[inline]
    fn interpolate(&self, t: f32) -> f32 {
        match self {
            &AnimationCurve::Linear => t.clamp(0.0, 1.0),
            &AnimationCurve::CubicBezier { p1, p2 } => interpolate_cubic_bezier(t, p1, p2),
        }
    }
}

fn interpolate_cubic_bezier(t: f32, p1: (f32, f32), p2: (f32, f32)) -> f32 {
    // out of range
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }

    // p01 = mix(vec2(0.0), p1, t) = p1 * t
    // p12 = mix(p1, p2, t) = p1 * (1.0 - t) + p2 * t
    // p23 = mix(p2, vec2(1.0), t) = p2 * (1.0 - t) + vec2(t)
    // p012 = mix(p01, p12, t) = p01 * (1.0 - t) + p12 * t = p1 * t * (1.0 - t) + (p1 * (1.0 -t ) + p2 * t) * t =
    // p1 * t * (1.0 - t) + p1 * t * (1.0 - t) + p2 * t * t = p1 * 2.0 * t * (1.0 - t) + p2 * t * t =
    // p1 * (2.0 * t - 2.0 * t * t) + p2 * t * t
    // p123 = mix(p12, p23, t) = p12 * (1.0 - t) + p23 * t = (p1 * (1.0 - t) + p2 * t) * (1.0 - t) + (p2 * (1.0 - t) + vec2(t)) * t =
    // p1 * (1.0 - t) * (1.0 - t) + p2 * t * (1.0 - t) + p2 * (1.0 - t) * t  + vec2(t * t) =
    // p1 * (1.0 - t) * (1.0 - t) + p2 * 2.0 * t * (1.0 - t) + vec2(t * t) =
    // p1 * (1.0 - t) * (1.0 - t) + p2 * (2.0 * t - 2.0 * t * t) + vec2(t * t)
    // p = mix(p012, p123, t) = p012 * (1.0 - t) + p123 * t =
    // (p1 * (2.0 * t - 2.0 * t * t) + p2 * t * t) * (1.0 - t) + (p1 * (1.0 - t) * (1.0 - t) + p2 * (2.0 * t - 2.0 * t * t) + vec2(t * t)) * t =
    // p1 * (2.0 * t - 2.0 * t * t) * (1.0 - t) + p2 * t * t * (1.0 - t) + p1 * t * (1.0 - t) * (1.0 - t) + p2 * t * (2.0 * t - 2.0 * t * t) + vec2(t * t * t) =
    // p1 * 2.0 * t * (1.0 - t) * (1.0 - t) + p2 * t * t * (1.0 - t) + p1 * t * (1.0 - t) * (1.0 - t) + p2 * 2.0 * t * t * (1.0 - t) + vec2(t * t * t) =
    // p1 * 3.0 * t * (1.0 - t) * (1.0 - t) + p2 * 3.0 * t * t * (1.0 - t) + vec2(t * t * t)
    //
    // (1.0 - t)^2 = 1.0^2 - 2.0 * t + t^2
    //
    // x = (p1.x * 3.0 * t * (1.0 - t) * (1.0 - t) + p2.x * 3.0 * t * t * (1.0 - t) + t * t * t), t = ?
    // x = p1.x * (3.0 * t - 6.0 * t^2 + 3.0 * t^3) + p2.x * (3.0 * t^2 - 3.0 * t^3) + t^3
    // x = (p1.x * 3.0 - p2.x * 3.0 + 1.0) * t^3 + (-p1.x * 6.0 + p2.x * 3.0) * t^2 + p1.x * 3.0 * t
    // 0 = (p1.x * 3.0 - p2.x * 3.0 + 1.0) * t^3 + (-p1.x * 6.0 + p2.x * 3.0) * t^2 + p1.x * 3.0 * t - x

    // x = (p1.x * 3.0 - p2.x * 3.0 + 1.0) * t^3 + (p2.x * 3.0 - p1.x * 6.0) * t^2 + p1.x * 3.0 * t
    // t = ?
    let a = p1.0 * 3.0 - p2.0 * 3.0 + 1.0;
    let b = p2.0 * 3.0 - p1.0 * 6.0;
    let c = p1.0 * 3.0;
    let d = -t;

    let t0 = if a == 0.0 {
        // solve quadratic: (p2.x * 3.0 - p1.x * 6.0) * t^2 + p1.x * 3.0 * t - x = 0
        let dq = c * c - 4.0 * b * d;

        if dq < 0.0 {
            // no value
            return 0.0;
        } else if dq == 0.0 {
            // exactly one
            -c / (2.0 * b)
        } else {
            // select correct value
            let t1 = -c + dq.sqrt() / (2.0 * b);
            let t2 = -c - dq.sqrt() / (2.0 * b);

            if 0.0 <= t2 && t2 <= 1.0 {
                t2
            } else {
                t1.clamp(0.0, 1.0)
            }
        }
    } else {
        // solve cubic: https://peter-shepherd.com/personal_development/mathematics/polynomials/cubicAlgebra.htm
        let a1 = b / a;
        let b1 = c / a;
        let c1 = d / a;
        let p = (3.0 * b1 - a1 * a1) / 3.0;
        let q = (2.0 * a1 * a1 * a1 - 9.0 * a1 * b1 + 27.0 * c1) / 27.0;

        if p == 0.0 {
            if q == 0.0 {
                0.0
            } else {
                let t1 = (-q).cbrt() - a1 / 3.0;
                let t2 = (-q).cbrt() * (-0.5 * 3.0f32.sqrt() / 2.0) - a1 / 3.0;
                let t3 = (-q).cbrt() * (-0.5 - 3.0f32.sqrt() / 2.0) - a1 / 3.0;

                if 0.0 <= t3 && t3 <= 1.0 {
                    t3
                } else if 0.0 <= t2 && t2 <= 1.0 {
                    t2
                } else {
                    t1.clamp(0.0, 1.0)
                }
            }
        } else {
            if q == 0.0 {
                let t1 = -a1 / 3.0;
                let t2 = (-p).sqrt() - a1 / 3.0;
                let t3 = -(-p).sqrt() - a1 / 3.0;

                if 0.0 <= t3 && t3 <= 1.0 {
                    t3
                } else if 0.0 <= t2 && t2 <= 1.0 {
                    t2
                } else {
                    t1.clamp(0.0, 1.0)
                }
            } else {
                let dc = (q * q) / 4.0 + (p * p * p) / 27.0;

                if dc == 0.0 {
                    // two reals
                    let t1 = 2.0 * (-q / 2.0).cbrt() - a1 / 3.0;
                    let t2 = (q / 2.0).cbrt() - a1 / 3.0;

                    if 0.0 <= t2 && t2 <= 1.0 {
                        t2
                    } else {
                        t1.clamp(0.0, 1.0)
                    }
                } else if dc > 0.0 {
                    // one real and two img
                    let u1 = (-(q / 2.0) + dc.sqrt()).cbrt();
                    let v1 = (q / 2.0 + dc.sqrt()).cbrt();

                    let t1 = u1 - v1 - a1 / 3.0;
                    let t2 = -0.5 * (u1 - v1) + (u1 + v1) * 3.0f32.sqrt() / 2.0 - a1 / 3.0;
                    let t3 = -0.5 * (u1 - v1) - (u1 + v1) * 3.0f32.sqrt() / 2.0 - a1 / 3.0;

                    if 0.0 <= t3 && t3 <= 1.0 {
                        t3
                    } else if 0.0 <= t2 && t2 <= 1.0 {
                        t2
                    } else {
                        t1.clamp(0.0, 1.0)
                    }
                } else {
                    // irreducible case
                    let r = (-p / 3.0).powi(3).sqrt();
                    let phi = (-q / (2.0 * r)).acos();

                    let t1 = 2.0 * r.cbrt() * (phi / 3.0).cos() - a1 / 3.0;
                    let t2 =
                        2.0 * r.cbrt() * ((phi + core::f32::consts::TAU) / 3.0).cos() - a1 / 3.0;
                    let t3 = 3.0 * r.cbrt() * ((phi + core::f32::consts::TAU * 2.0) / 3.0).cos()
                        - a1 / 3.0;

                    if 0.0 <= t3 && t3 <= 1.0 {
                        t3
                    } else if 0.0 <= t2 && t2 <= 1.0 {
                        t2
                    } else {
                        t1.clamp(0.0, 1.0)
                    }
                }
            }
        }
    };

    // y = (p1.y * 3.0 - p2.y * 3.0 + 1.0) * t^3 + (p2.y * 3.0 - p1.y * 6.0) * t^2 + p1.y * 3.0 * t
    (p1.1 * 3.0 - p2.1 * 3.0 + 1.0) * t0.powi(3)
        + (p2.1 * 3.0 - p1.1 * 6.0) * t0.powi(2)
        + p1.1 * 3.0 * t0
}

#[derive(Clone, Copy)]
pub struct ClipConfig {
    pub left_softness: SafeF32,
    pub top_softness: SafeF32,
    pub right_softness: SafeF32,
    pub bottom_softness: SafeF32,
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomRenderToken(usize);

pub struct CompositeRect {
    pub has_bitmap: bool,
    pub base_scale_factor: f32,
    pub offset: [AnimatableFloat; 2],
    pub size: [AnimatableFloat; 2],
    pub relative_offset_adjustment: [f32; 2],
    pub relative_size_adjustment: [f32; 2],
    pub clip_child: Option<ClipConfig>,
    pub texatlas_rect: AtlasRect,
    pub slice_borders: [f32; 4],
    pub composite_mode: CompositeMode,
    pub custom_render_token: Option<CustomRenderToken>,
    pub opacity: AnimatableFloat,
    pub pivot: [f32; 2],
    pub scale_x: AnimatableFloat,
    pub scale_y: AnimatableFloat,
    pub dirty: bool,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}
impl Default for CompositeRect {
    fn default() -> Self {
        Self {
            has_bitmap: false,
            base_scale_factor: 1.0,
            offset: [const { AnimatableFloat::Value(0.0) }; 2],
            size: [const { AnimatableFloat::Value(0.0) }; 2],
            relative_offset_adjustment: [0.0, 0.0],
            relative_size_adjustment: [0.0, 0.0],
            clip_child: None,
            texatlas_rect: AtlasRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            slice_borders: [0.0, 0.0, 0.0, 0.0],
            dirty: false,
            composite_mode: CompositeMode::DirectSourceOver,
            custom_render_token: None,
            opacity: AnimatableFloat::Value(1.0),
            pivot: [0.5; 2],
            scale_x: AnimatableFloat::Value(1.0),
            scale_y: AnimatableFloat::Value(1.0),
            parent: None,
            children: Vec::new(),
        }
    }
}

/// Unbounded from gfx_device(must be externally managed)
pub struct UnboundedCompositeInstanceManager {
    buffer: br::vk::VkBuffer,
    memory: br::vk::VkDeviceMemory,
    streaming_buffer: br::vk::VkBuffer,
    streaming_memory: br::vk::VkDeviceMemory,
    streaming_memory_requires_flush: bool,
    buffer_stg: br::vk::VkBuffer,
    memory_stg: br::vk::VkDeviceMemory,
    stg_mem_requires_flush: bool,
    capacity: usize,
    count: usize,
    free: BTreeSet<usize>,
}
impl UnboundedCompositeInstanceManager {
    pub unsafe fn drop_with_gfx_device(&mut self, gfx_device: &Subsystem) {
        unsafe {
            br::vkfn_wrapper::free_memory(gfx_device.native_ptr(), self.memory_stg, None);
            br::vkfn_wrapper::destroy_buffer(gfx_device.native_ptr(), self.buffer_stg, None);
            br::vkfn_wrapper::free_memory(gfx_device.native_ptr(), self.streaming_memory, None);
            br::vkfn_wrapper::destroy_buffer(gfx_device.native_ptr(), self.streaming_buffer, None);
            br::vkfn_wrapper::free_memory(gfx_device.native_ptr(), self.memory, None);
            br::vkfn_wrapper::destroy_buffer(gfx_device.native_ptr(), self.buffer, None);
        }
    }

    const INIT_CAP: usize = 1024;

    pub fn new(subsystem: &Subsystem) -> Self {
        let mut buffer = br::BufferObject::new(
            subsystem,
            &br::BufferCreateInfo::new(
                core::mem::size_of::<CompositeInstanceData>() * Self::INIT_CAP,
                br::BufferUsage::STORAGE_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        )
        .expect("Failed to create composite instance buffer");
        let req = buffer.requirements();
        let Some(memory_index) = subsystem.find_device_local_memory_index(req.memoryTypeBits)
        else {
            tracing::error!(memory_index_mask = req.memoryTypeBits, "no suitable memory");
            std::process::exit(1);
        };
        let memory = br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(req.size, memory_index),
        )
        .expect("Failed to allocate composite instance data memory");
        buffer
            .bind(&memory, 0)
            .expect("Failed to bind buffer memory");

        let mut streaming_buffer = br::BufferObject::new(
            subsystem,
            &br::BufferCreateInfo::new_for_type::<CompositeStreamingData>(
                br::BufferUsage::UNIFORM_BUFFER,
            ),
        )
        .unwrap();
        let mreq = streaming_buffer.requirements();
        let Some(memory_index) = subsystem.find_direct_memory_index(mreq.memoryTypeBits) else {
            tracing::error!(
                memory_index_mask = mreq.memoryTypeBits,
                "no suitable memory for streaming"
            );
            std::process::exit(1);
        };
        let streaming_memory = br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(mreq.size, memory_index),
        )
        .unwrap();
        streaming_buffer
            .bind(&streaming_memory, 0)
            .expect("Failed to bind streaming buffer memory");
        let streaming_memory_requires_flush =
            !subsystem.adapter_memory_info.is_coherent(memory_index);

        let mut buffer_stg = br::BufferObject::new(
            subsystem,
            &br::BufferCreateInfo::new(
                core::mem::size_of::<CompositeInstanceData>() * Self::INIT_CAP,
                br::BufferUsage::TRANSFER_SRC,
            ),
        )
        .expect("Failed to create composite instance staging buffer");
        let buffer_mreq = buffer.requirements();
        let memory_index = subsystem
            .adapter_memory_info
            .find_host_visible_index(buffer_mreq.memoryTypeBits)
            .expect("no suitable memory");
        let stg_mem_requires_flush = !subsystem.adapter_memory_info.is_coherent(memory_index);
        let memory_stg = br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(buffer_mreq.size, memory_index),
        )
        .expect("Failed to allocate composite instance data staging memory");
        buffer_stg
            .bind(&memory_stg, 0)
            .expect("Failed to bind staging buffer memory");

        let (buffer, _) = buffer.unmanage();
        let (memory, _) = memory.unmanage();
        let (streaming_buffer, _) = streaming_buffer.unmanage();
        let (streaming_memory, _) = streaming_memory.unmanage();
        let (buffer_stg, _) = buffer_stg.unmanage();
        let (memory_stg, _) = memory_stg.unmanage();

        Self {
            buffer,
            memory,
            streaming_buffer,
            streaming_memory,
            streaming_memory_requires_flush,
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

    pub fn sync_buffer<'cb>(&self, cr: br::CmdRecord<'cb>) -> br::CmdRecord<'cb> {
        cr.copy_buffer(
            &unsafe { br::VkHandleRef::dangling(self.buffer_stg) },
            &unsafe { br::VkHandleRef::dangling(self.buffer) },
            &[br::BufferCopy::mirror(
                0,
                (core::mem::size_of::<CompositeInstanceData>() * self.capacity) as _,
            )],
        )
    }

    pub const fn streaming_memory_requires_flush(&self) -> bool {
        self.streaming_memory_requires_flush
    }

    pub const fn count(&self) -> usize {
        self.count
    }

    pub const fn memory_stg_requires_explicit_flush(&self) -> bool {
        self.stg_mem_requires_flush
    }

    pub const fn range_all(&self) -> core::ops::Range<usize> {
        0..core::mem::size_of::<CompositeInstanceData>() * self.count
    }

    pub const fn buffer_transparent_ref(&self) -> &br::VkHandleRef<br::vk::VkBuffer> {
        br::VkHandleRef::from_raw_ref(&self.buffer)
    }

    pub const fn streaming_buffer_transparent_ref(&self) -> &br::VkHandleRef<br::vk::VkBuffer> {
        br::VkHandleRef::from_raw_ref(&self.streaming_buffer)
    }

    pub const fn staging_memory_raw_handle(&self) -> br::vk::VkDeviceMemory {
        self.memory_stg
    }

    pub unsafe fn map_staging<'s, 'g>(
        &'s mut self,
        gfx_device: &'g Subsystem,
    ) -> br::Result<UnboundedCompositeInstanceMappedStagingMemory<'s, 'g>> {
        let ptr = unsafe {
            br::vkfn_wrapper::map_memory(
                gfx_device.native_ptr(),
                self.memory_stg,
                0,
                (core::mem::size_of::<CompositeInstanceData>() * self.capacity) as _,
                0,
            )?
        };

        Ok(UnboundedCompositeInstanceMappedStagingMemory(
            ptr, self, gfx_device,
        ))
    }

    pub const fn streaming_memory_raw_handle(&self) -> br::vk::VkDeviceMemory {
        self.streaming_memory
    }

    pub unsafe fn map_streaming<'s, 'g>(
        &'s mut self,
        gfx_device: &'g Subsystem,
    ) -> br::Result<UnboundedCompositeInstanceMappedStreamingMemory<'s, 'g>> {
        let ptr = unsafe {
            br::vkfn_wrapper::map_memory(
                gfx_device.native_ptr(),
                self.streaming_memory,
                0,
                core::mem::size_of::<CompositeStreamingData>() as _,
                0,
            )?
        };

        Ok(UnboundedCompositeInstanceMappedStreamingMemory(
            ptr, self, gfx_device,
        ))
    }
}

pub struct UnboundedCompositeInstanceMappedStagingMemory<'m, 'g>(
    *mut core::ffi::c_void,
    &'m mut UnboundedCompositeInstanceManager,
    &'g Subsystem,
);
impl Drop for UnboundedCompositeInstanceMappedStagingMemory<'_, '_> {
    fn drop(&mut self) {
        unsafe {
            br::vkfn_wrapper::unmap_memory(self.2.native_ptr(), self.1.memory_stg);
        }
    }
}
impl UnboundedCompositeInstanceMappedStagingMemory<'_, '_> {
    pub const fn ptr(&self) -> *mut core::ffi::c_void {
        self.0
    }
}

pub struct UnboundedCompositeInstanceMappedStreamingMemory<'m, 'g>(
    *mut core::ffi::c_void,
    &'m mut UnboundedCompositeInstanceManager,
    &'g Subsystem,
);
impl Drop for UnboundedCompositeInstanceMappedStreamingMemory<'_, '_> {
    fn drop(&mut self) {
        unsafe {
            br::vkfn_wrapper::unmap_memory(self.2.native_ptr(), self.1.streaming_memory);
        }
    }
}
impl UnboundedCompositeInstanceMappedStreamingMemory<'_, '_> {
    pub const fn ptr(&self) -> *mut CompositeStreamingData {
        self.0.cast()
    }
}

pub struct CompositeInstanceManager<'d> {
    gfx_device: &'d Subsystem,
    raw: UnboundedCompositeInstanceManager,
}
impl Drop for CompositeInstanceManager<'_> {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            self.raw.drop_with_gfx_device(self.gfx_device);
        }
    }
}
impl<'d> CompositeInstanceManager<'d> {
    #[tracing::instrument(skip(subsystem))]
    pub fn new(subsystem: &'d Subsystem) -> Self {
        Self {
            raw: UnboundedCompositeInstanceManager::new(subsystem),
            gfx_device: subsystem,
        }
    }

    pub const fn unbound(self) -> UnboundedCompositeInstanceManager {
        let raw = unsafe { core::ptr::read(&self.raw) };
        core::mem::forget(self);

        raw
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CompositeTreeRef(usize);
impl CompositeTreeRef {
    #[inline(always)]
    pub fn entity<'c>(&self, mgr: &'c CompositeTree) -> &'c CompositeRect {
        mgr.get(*self)
    }

    #[inline(always)]
    pub fn entity_mut<'c>(&self, mgr: &'c mut CompositeTree) -> &'c mut CompositeRect {
        mgr.get_mut(*self)
    }

    #[inline(always)]
    pub fn entity_mut_dirtified<'c>(&self, mgr: &'c mut CompositeTree) -> &'c mut CompositeRect {
        mgr.mark_dirty(*self);
        mgr.get_mut(*self)
    }

    #[inline(always)]
    pub fn mark_dirty(&self, mgr: &mut CompositeTree) {
        mgr.mark_dirty(*self);
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CompositeTreeFloatParameterRef(usize);

pub struct CompositeTreeParameterStore {
    float_parameters: Vec<FloatParameter>,
    float_values: Vec<f32>,
    unused_float_parameters: BTreeSet<usize>,
}
impl CompositeTreeParameterStore {
    pub fn alloc_float(&mut self, init: FloatParameter) -> CompositeTreeFloatParameterRef {
        if let Some(x) = self.unused_float_parameters.pop_first() {
            self.float_parameters[x] = init;
            return CompositeTreeFloatParameterRef(x);
        }

        self.float_parameters.push(init);
        self.float_values.push(0.0);
        CompositeTreeFloatParameterRef(self.float_parameters.len() - 1)
    }

    pub fn free_float(&mut self, r: CompositeTreeFloatParameterRef) {
        self.unused_float_parameters.insert(r.0);
    }

    pub fn set_float(&mut self, r: CompositeTreeFloatParameterRef, a: FloatParameter) {
        self.float_parameters[r.0] = a;
    }

    pub fn evaluate_float(&self, r: CompositeTreeFloatParameterRef, current_sec: f32) -> f32 {
        self.float_parameters[r.0].evaluate(current_sec)
    }

    pub fn float_value(&self, r: CompositeTreeFloatParameterRef) -> f32 {
        self.float_values[r.0]
    }

    fn evaluate_all(&mut self, current_sec: f32) {
        for (v, p) in self
            .float_values
            .iter_mut()
            .zip(self.float_parameters.iter())
        {
            *v = p.evaluate(current_sec);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderPassAfterOperation {
    None,
    Grab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderPassRequirements {
    pub after_operation: RenderPassAfterOperation,
    pub continued: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompositeRenderingInstruction {
    DrawInstanceRange {
        index_range: core::ops::Range<usize>,
        backdrop_buffer: usize,
    },
    InsertCustomRenderCommands(CustomRenderToken),
    SetClip {
        shader_parameters: [SafeF32; 8],
    },
    ClearClip,
    GrabBackdrop,
    GenerateBackdropBlur {
        stdev: SafeF32,
        dest_backdrop_buffer: usize,
        rects: Vec<br::Rect2D>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct CompositeRenderingData {
    pub instructions: Vec<CompositeRenderingInstruction>,
    pub render_passes: Vec<RenderPassRequirements>,
    pub required_backdrop_buffer_count: usize,
}

const fn rect_overlaps(a: &br::Rect2D, b: &br::Rect2D) -> bool {
    b.offset.x - (a.extent.width as i32) < a.offset.x
        && a.offset.x < b.offset.x + (b.extent.width as i32)
        && b.offset.y - (a.extent.height as i32) < a.offset.y
        && a.offset.y < b.offset.y + (b.extent.height as i32)
}

struct CompositeRenderingInstructionBuilder {
    insts: Vec<CompositeRenderingInstruction>,
    render_passes: Vec<RenderPassRequirements>,
    last_free_backdrop_buffer: usize,
    active_backdrop_blur_index_for_stdev: HashMap<SafeF32, usize>,
    current_backdrop_overlap_rects: Vec<br::Rect2D>,
    backdrop_active: bool,
    max_backdrop_buffer_count: usize,
    screen_rect: br::Rect2D,
    active_clip_parameters: Option<[SafeF32; 8]>,
    clip_invalidated: bool,
}
impl CompositeRenderingInstructionBuilder {
    fn new(screen_size: br::Extent2D) -> Self {
        Self {
            insts: vec![CompositeRenderingInstruction::ClearClip],
            render_passes: Vec::new(),
            last_free_backdrop_buffer: 0,
            active_backdrop_blur_index_for_stdev: HashMap::new(),
            current_backdrop_overlap_rects: Vec::new(),
            backdrop_active: false,
            max_backdrop_buffer_count: 0,
            screen_rect: screen_size.into_rect(br::Offset2D::ZERO),
            active_clip_parameters: None,
            clip_invalidated: true,
        }
    }

    fn build(mut self) -> CompositeRenderingData {
        // process for last backdrop layer
        self.max_backdrop_buffer_count = self
            .max_backdrop_buffer_count
            .max(self.last_free_backdrop_buffer);
        let rpr = RenderPassRequirements {
            after_operation: RenderPassAfterOperation::None,
            continued: !self.render_passes.is_empty(),
        };
        self.render_passes.push(rpr);

        CompositeRenderingData {
            instructions: self.insts,
            render_passes: self.render_passes,
            required_backdrop_buffer_count: self.max_backdrop_buffer_count,
        }
    }

    fn draw_instance(&mut self, index: usize, backdrop_buffer_index: usize) {
        if let Some(&mut CompositeRenderingInstruction::DrawInstanceRange {
            ref mut index_range,
            backdrop_buffer,
        }) = self.insts.last_mut()
        {
            if index_range.end == index && backdrop_buffer == backdrop_buffer_index {
                // optimal pass: fuse
                index_range.end += 1;
                return;
            }
        }

        self.insts
            .push(CompositeRenderingInstruction::DrawInstanceRange {
                index_range: index..index + 1,
                backdrop_buffer: backdrop_buffer_index,
            });
    }

    fn insert_custom_render_commands(&mut self, token: CustomRenderToken) {
        // no dependency check
        self.insts
            .push(CompositeRenderingInstruction::InsertCustomRenderCommands(
                token,
            ));
    }

    fn set_clip(&mut self, rect: &[SafeF32; 4], config: &ClipConfig) {
        let clip_parameters = [
            rect[0],
            rect[1],
            rect[2],
            rect[3],
            config.left_softness,
            config.top_softness,
            config.right_softness,
            config.bottom_softness,
        ];
        if !self.clip_invalidated
            && self
                .active_clip_parameters
                .as_ref()
                .is_some_and(|x| x == &clip_parameters)
        {
            // same clip already active
            return;
        }

        // needs to change clip state...
        match self.insts.last_mut() {
            Some(x @ &mut CompositeRenderingInstruction::ClearClip) => {
                // replace clearclip
                *x = CompositeRenderingInstruction::SetClip {
                    shader_parameters: clip_parameters,
                };
            }
            Some(&mut CompositeRenderingInstruction::SetClip {
                ref shader_parameters,
            }) if &clip_parameters == shader_parameters => {
                // same clip, nop
            }
            Some(&mut CompositeRenderingInstruction::SetClip {
                ref mut shader_parameters,
            }) => {
                // overtake contiguous setclip
                *shader_parameters = clip_parameters;
            }
            _ => {
                // insert new setclip instruction
                self.insts.push(CompositeRenderingInstruction::SetClip {
                    shader_parameters: clip_parameters,
                });
            }
        }

        self.clip_invalidated = false;
        self.active_clip_parameters = Some(clip_parameters);
    }

    fn clear_clip(&mut self) {
        if self.clip_invalidated && self.active_clip_parameters.is_none() {
            // nothing clip activated
            return;
        }

        match self.insts.last_mut() {
            Some(&mut CompositeRenderingInstruction::ClearClip) => {
                // fuse, do nothing
            }
            Some(x @ &mut CompositeRenderingInstruction::SetClip { .. }) => {
                // clip set but no rendering occured, overtake
                *x = CompositeRenderingInstruction::ClearClip;
            }
            _ => {
                self.insts.push(CompositeRenderingInstruction::ClearClip);
            }
        }

        self.clip_invalidated = true;
        self.active_clip_parameters = None;
    }

    /// return: backdrop buffer index
    fn request_backdrop_blur(&mut self, stdev: SafeF32, rect: br::Rect2D) -> usize {
        if !rect_overlaps(&rect, &self.screen_rect) {
            // perfectly culled
            return 0;
        }

        if !self.backdrop_active {
            // first time layer
            self.backdrop_active = true;
            self.insts.extend([
                CompositeRenderingInstruction::GrabBackdrop,
                CompositeRenderingInstruction::GenerateBackdropBlur {
                    stdev,
                    dest_backdrop_buffer: 0,
                    rects: vec![rect],
                },
            ]);
            let rpr = RenderPassRequirements {
                after_operation: RenderPassAfterOperation::Grab,
                continued: !self.render_passes.is_empty(),
            };
            self.render_passes.push(rpr);
            self.clip_invalidated = true;
            self.max_backdrop_buffer_count = self
                .max_backdrop_buffer_count
                .max(self.last_free_backdrop_buffer);
            self.last_free_backdrop_buffer = 1;
            self.current_backdrop_overlap_rects.clear();
            self.active_backdrop_blur_index_for_stdev.clear();
            self.current_backdrop_overlap_rects.push(rect);
            self.active_backdrop_blur_index_for_stdev
                .insert(stdev, self.insts.len() - 1);

            return 0;
        }

        let overlaps = self
            .current_backdrop_overlap_rects
            .iter()
            .any(|x| rect_overlaps(&rect, x));

        if overlaps {
            // non-optimal pass: split to new layer
            self.insts.extend([
                CompositeRenderingInstruction::GrabBackdrop,
                CompositeRenderingInstruction::GenerateBackdropBlur {
                    stdev,
                    dest_backdrop_buffer: 0,
                    rects: vec![rect],
                },
            ]);
            let rpr = RenderPassRequirements {
                after_operation: RenderPassAfterOperation::Grab,
                continued: !self.render_passes.is_empty(),
            };
            self.render_passes.push(rpr);
            self.clip_invalidated = true;
            self.max_backdrop_buffer_count = self
                .max_backdrop_buffer_count
                .max(self.last_free_backdrop_buffer);
            self.last_free_backdrop_buffer = 1;
            self.current_backdrop_overlap_rects.clear();
            self.active_backdrop_blur_index_for_stdev.clear();
            self.current_backdrop_overlap_rects.push(rect);
            self.active_backdrop_blur_index_for_stdev
                .insert(stdev, self.insts.len() - 1);

            return 0;
        }

        // optimal pass: no overlapping layer: fuse or generate
        self.current_backdrop_overlap_rects.push(rect);

        if let Some(&ix) = self.active_backdrop_blur_index_for_stdev.get(&stdev) {
            // fuse
            let &mut CompositeRenderingInstruction::GenerateBackdropBlur {
                ref mut rects,
                dest_backdrop_buffer,
                ..
            } = &mut self.insts[ix]
            else {
                unreachable!();
            };

            rects.push(rect);
            dest_backdrop_buffer
        } else {
            // generate
            self.insts
                .push(CompositeRenderingInstruction::GenerateBackdropBlur {
                    rects: vec![rect],
                    dest_backdrop_buffer: self.last_free_backdrop_buffer,
                    stdev,
                });
            self.last_free_backdrop_buffer += 1;
            self.current_backdrop_overlap_rects.push(rect);
            self.active_backdrop_blur_index_for_stdev
                .insert(stdev, self.insts.len() - 1);

            self.last_free_backdrop_buffer - 1
        }
    }
}

pub struct CompositeTree {
    rects: Vec<CompositeRect>,
    unused: BTreeSet<usize>,
    dirty: bool,
    parameter_store: CompositeTreeParameterStore,
    custom_render_unused: BTreeSet<usize>,
    custom_render_last_id: usize,
}
impl CompositeTree {
    /// ルートノード
    pub const ROOT: CompositeTreeRef = CompositeTreeRef(0);

    pub fn new() -> Self {
        let mut rects = Vec::new();
        // root is filling rect
        rects.push(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            ..Default::default()
        });

        Self {
            rects,
            unused: BTreeSet::new(),
            dirty: false,
            parameter_store: CompositeTreeParameterStore {
                float_parameters: Vec::new(),
                float_values: Vec::new(),
                unused_float_parameters: BTreeSet::new(),
            },
            custom_render_unused: BTreeSet::new(),
            custom_render_last_id: 0,
        }
    }

    pub fn register(&mut self, data: CompositeRect) -> CompositeTreeRef {
        if let Some(x) = self.unused.pop_first() {
            self.rects[x] = data;
            return CompositeTreeRef(x);
        }

        self.rects.push(data);
        CompositeTreeRef(self.rects.len() - 1)
    }

    pub fn free(&mut self, index: CompositeTreeRef) {
        self.unused.insert(index.0);
    }

    pub fn acquire_custom_render_token(&mut self) -> CustomRenderToken {
        if let Some(x) = self.custom_render_unused.pop_first() {
            return CustomRenderToken(x);
        }

        let t = CustomRenderToken(self.custom_render_last_id);
        self.custom_render_last_id += 1;
        t
    }

    pub fn release_custom_render_token(&mut self, token: CustomRenderToken) {
        self.custom_render_unused.insert(token.0);
    }

    pub fn get(&self, index: CompositeTreeRef) -> &CompositeRect {
        &self.rects[index.0]
    }

    pub fn get_mut(&mut self, index: CompositeTreeRef) -> &mut CompositeRect {
        &mut self.rects[index.0]
    }

    pub fn mark_dirty(&mut self, index: CompositeTreeRef) {
        self.rects[index.0].dirty = true;
        self.dirty = true;
    }

    pub fn take_dirty(&mut self) -> bool {
        core::mem::replace(&mut self.dirty, false)
    }

    pub fn add_child(&mut self, parent: CompositeTreeRef, child: CompositeTreeRef) {
        if let Some(p) = self.rects[child.0].parent.replace(parent.0) {
            // unlink from old parent
            self.rects[p].children.retain(|&x| x != child.0);
        }

        self.rects[parent.0].children.push(child.0);
        self.dirty = true;
    }

    pub fn remove_child(&mut self, child: CompositeTreeRef) {
        if let Some(p) = self.rects[child.0].parent.take() {
            self.rects[p].children.retain(|&x| x != child.0);
            self.dirty = true;
        }
    }

    pub const fn parameter_store(&self) -> &CompositeTreeParameterStore {
        &self.parameter_store
    }

    pub const fn parameter_store_mut(&mut self) -> &mut CompositeTreeParameterStore {
        &mut self.parameter_store
    }

    /// return: bitmap count
    pub unsafe fn update(
        &mut self,
        size: br::Extent2D,
        current_sec: f32,
        tex_size: br::Extent2D,
        mapped_head: *mut core::ffi::c_void,
        event_bus: &AppEventBus,
    ) -> CompositeRenderingData {
        // let update_timer = std::time::Instant::now();

        self.parameter_store.evaluate_all(current_sec);

        let mut inst_builder = CompositeRenderingInstructionBuilder::new(size);
        let mut instance_slot_index = 0;
        let mut processes = vec![(
            0,
            (
                0.0,
                0.0,
                size.width as f32,
                size.height as f32,
                1.0,
                Matrix4::IDENTITY,
                None::<([SafeF32; 4], ClipConfig)>,
            ),
        )];
        while let Some((
            r,
            (
                effective_base_left,
                effective_base_top,
                effective_width,
                effective_height,
                parent_opacity,
                parent_matrix,
                active_clip,
            ),
        )) = processes.pop()
        {
            let r = &mut self.rects[r];
            r.dirty = false;
            let local_left =
                r.offset[0].evaluate(current_sec, &self.parameter_store) * r.base_scale_factor;
            let local_top =
                r.offset[1].evaluate(current_sec, &self.parameter_store) * r.base_scale_factor;
            let local_width =
                r.size[0].evaluate(current_sec, &self.parameter_store) * r.base_scale_factor;
            let local_height =
                r.size[1].evaluate(current_sec, &self.parameter_store) * r.base_scale_factor;

            let left = effective_base_left
                + (effective_width * r.relative_offset_adjustment[0])
                + local_left;
            let top = effective_base_top
                + (effective_height * r.relative_offset_adjustment[1])
                + local_top;
            let w = effective_width * r.relative_size_adjustment[0] + local_width;
            let h = effective_height * r.relative_size_adjustment[1] + local_height;

            let opacity = parent_opacity * r.opacity.evaluate(current_sec, &self.parameter_store);
            let matrix = parent_matrix.mul_mat4(
                Matrix4::translate(
                    left - effective_base_left + r.pivot[0] * w,
                    top - effective_base_top + r.pivot[1] * h,
                )
                .mul_mat4(Matrix4::scale(
                    r.scale_x.evaluate(current_sec, &self.parameter_store),
                    r.scale_y.evaluate(current_sec, &self.parameter_store),
                ))
                .mul_mat4(Matrix4::translate(-r.pivot[0] * w, -r.pivot[1] * h)),
            );

            r.offset[0].process_on_complete(current_sec, event_bus);
            r.offset[1].process_on_complete(current_sec, event_bus);
            r.size[0].process_on_complete(current_sec, event_bus);
            r.size[1].process_on_complete(current_sec, event_bus);
            r.opacity.process_on_complete(current_sec, event_bus);
            r.scale_x.process_on_complete(current_sec, event_bus);
            r.scale_y.process_on_complete(current_sec, event_bus);
            match r.composite_mode {
                CompositeMode::DirectSourceOver => (),
                CompositeMode::ColorTint(ref mut t) => {
                    t.process_on_complete(current_sec, event_bus)
                }
                CompositeMode::FillColor(ref mut t) => {
                    t.process_on_complete(current_sec, event_bus)
                }
                CompositeMode::ColorTintBackdropBlur(ref mut t, ref mut stdev) => {
                    t.process_on_complete(current_sec, event_bus);
                    stdev.process_on_complete(current_sec, event_bus);
                }
                CompositeMode::FillColorBackdropBlur(ref mut t, ref mut stdev) => {
                    t.process_on_complete(current_sec, event_bus);
                    stdev.process_on_complete(current_sec, event_bus);
                }
            }

            if let Some(t) = r.custom_render_token {
                // Custom Renderがある場合はそっちのみ
                inst_builder.insert_custom_render_commands(t);
            } else if r.has_bitmap {
                unsafe {
                    core::ptr::write(
                        mapped_head
                            .cast::<CompositeInstanceData>()
                            .add(instance_slot_index),
                        CompositeInstanceData {
                            pos_st: [w, h, 0.0, 0.0],
                            uv_st: [
                                ((r.texatlas_rect.right as f32 - r.texatlas_rect.left as f32)
                                    - 1.0)
                                    / tex_size.width as f32,
                                ((r.texatlas_rect.bottom as f32 - r.texatlas_rect.top as f32)
                                    - 1.0)
                                    / tex_size.height as f32,
                                (r.texatlas_rect.left as f32 + 0.5) / tex_size.width as f32,
                                (r.texatlas_rect.top as f32 + 0.5) / tex_size.height as f32,
                            ],
                            position_modifier_matrix: matrix.clone().transpose().0,
                            slice_borders: r.slice_borders,
                            tex_size_pixels_composite_mode_opacity: [
                                tex_size.width as _,
                                tex_size.height as _,
                                r.composite_mode.shader_mode_value(),
                                opacity,
                            ],
                            color_tint: match r.composite_mode {
                                CompositeMode::DirectSourceOver => [0.0; 4],
                                CompositeMode::ColorTint(ref t) => {
                                    t.evaluate(current_sec, &self.parameter_store)
                                }
                                CompositeMode::FillColor(ref t) => {
                                    t.evaluate(current_sec, &self.parameter_store)
                                }
                                CompositeMode::ColorTintBackdropBlur(ref t, _) => {
                                    t.evaluate(current_sec, &self.parameter_store)
                                }
                                CompositeMode::FillColorBackdropBlur(ref t, _) => {
                                    t.evaluate(current_sec, &self.parameter_store)
                                }
                            },
                            pos_x_animation_data: [0.0; 4],
                            pos_x_curve_control_points: [0.0; 4],
                            pos_y_animation_data: [0.0; 4],
                            pos_y_curve_control_points: [0.0; 4],
                            pos_width_animation_data: [0.0; 4],
                            pos_width_curve_control_points: [0.0; 4],
                            pos_height_animation_data: [0.0; 4],
                            pos_height_curve_control_points: [0.0; 4],
                        },
                    );
                }

                let backdrop_buffer_index = match r.composite_mode {
                    CompositeMode::ColorTintBackdropBlur(_, ref stdev)
                    | CompositeMode::FillColorBackdropBlur(_, ref stdev) => {
                        let stdev = stdev.evaluate(current_sec, &self.parameter_store);

                        if stdev > 0.0 {
                            inst_builder.request_backdrop_blur(
                                unsafe { SafeF32::new_unchecked(stdev) },
                                br::Rect2D {
                                    offset: br::Offset2D {
                                        x: left as _,
                                        y: top as _,
                                    },
                                    extent: br::Extent2D {
                                        width: w as _,
                                        height: h as _,
                                    },
                                },
                            )
                        } else {
                            0
                        }
                    }
                    // とりあえず0
                    _ => 0,
                };

                if let Some((clip_rect_px, clip_config)) = active_clip {
                    inst_builder.set_clip(&clip_rect_px, &clip_config);
                } else {
                    inst_builder.clear_clip();
                }

                inst_builder.draw_instance(instance_slot_index, backdrop_buffer_index);
                instance_slot_index += 1;
            }

            processes.extend(r.children.iter().rev().map(|&x| {
                (
                    x,
                    (
                        left,
                        top,
                        w,
                        h,
                        opacity,
                        matrix.clone(),
                        r.clip_child.map(|cc| {
                            (
                                [
                                    unsafe { SafeF32::new_unchecked(left) },
                                    unsafe { SafeF32::new_unchecked(top) },
                                    unsafe { SafeF32::new_unchecked(left + w) },
                                    unsafe { SafeF32::new_unchecked(top + h) },
                                ],
                                cc,
                            )
                        }),
                    ),
                )
            }));
        }

        // let update_time = update_timer.elapsed();
        // println!("instbuild({update_time:?}): {:?}", inst_builder.insts);

        inst_builder.build()
    }
}

pub struct CompositeRenderer<'subsystem> {
    gfx_device: &'subsystem Subsystem,
    rp_grabbed: br::RenderPassObject<&'subsystem Subsystem>,
    rp_final: br::RenderPassObject<&'subsystem Subsystem>,
    rp_continue_grabbed: br::RenderPassObject<&'subsystem Subsystem>,
    rp_continue_final: br::RenderPassObject<&'subsystem Subsystem>,
    fbs_grabbed: Vec<br::vk::VkFramebuffer>,
    fbs_final: Vec<br::vk::VkFramebuffer>,
    fbs_continue_grabbed: Vec<br::vk::VkFramebuffer>,
    fbs_continue_final: Vec<br::vk::VkFramebuffer>,
    sampler: br::SamplerObject<&'subsystem Subsystem>,
    _dsl_input: br::DescriptorSetLayoutObject<&'subsystem Subsystem>,
    dsl_input_backdrop: br::DescriptorSetLayoutObject<&'subsystem Subsystem>,
    pipeline_layout: br::PipelineLayoutObject<&'subsystem Subsystem>,
    pipeline_grabbed: br::PipelineObject<&'subsystem Subsystem>,
    pipeline_final: br::PipelineObject<&'subsystem Subsystem>,
    pipeline_continue_grabbed: br::PipelineObject<&'subsystem Subsystem>,
    pipeline_continue_final: br::PipelineObject<&'subsystem Subsystem>,
    grab_buffer: br::ImageViewObject<br::ImageObject<&'subsystem Subsystem>>,
    grab_buffer_memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    backdrop_buffers: Vec<br::ImageViewObject<br::ImageObject<&'subsystem Subsystem>>>,
    backdrop_buffer_memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    backdrop_blur_destination_fbs: Vec<br::vk::VkFramebuffer>,
    backdrop_buffers_invalidated: bool,
    input_backdrop_descriptor_pool: br::DescriptorPoolObject<&'subsystem Subsystem>,
    input_backdrop_descriptor_sets: Vec<br::DescriptorSet>,
    input_backdrop_descriptor_pool_capacity: usize,
    backdrop_fx_blur_processor: BackdropEffectBlurProcessor<'subsystem>,
    _fixed_descriptor_pool: br::DescriptorPoolObject<&'subsystem Subsystem>,
    alphamask_group_input_descriptor_set: br::DescriptorSet,
    blur_fixed_descriptor_sets: Vec<br::DescriptorSet>,
}
impl Drop for CompositeRenderer<'_> {
    fn drop(&mut self) {
        Self::release_all_framebuffers(self.gfx_device, &mut self.fbs_grabbed);
        Self::release_all_framebuffers(self.gfx_device, &mut self.fbs_final);
        Self::release_all_framebuffers(self.gfx_device, &mut self.fbs_continue_grabbed);
        Self::release_all_framebuffers(self.gfx_device, &mut self.fbs_continue_final);
        Self::release_all_framebuffers(self.gfx_device, &mut self.backdrop_blur_destination_fbs);
    }
}
impl<'subsystem> CompositeRenderer<'subsystem> {
    const INITIAL_BACKDROP_BUFFER_COUNT: usize = 16;
    const PIPELINE_VI_STATE: &'static br::PipelineVertexInputStateCreateInfo<'static> =
        &br::PipelineVertexInputStateCreateInfo::new(&[], &[]);
    const PIPELINE_IA_STATE: &'static br::PipelineInputAssemblyStateCreateInfo =
        &br::PipelineInputAssemblyStateCreateInfo::new(br::PrimitiveTopology::TriangleStrip);
    const PIPELINE_RASTER_STATE: &'static br::PipelineRasterizationStateCreateInfo<'static> =
        &br::PipelineRasterizationStateCreateInfo::new(
            br::PolygonMode::Fill,
            br::CullModeFlags::NONE,
            br::FrontFace::CounterClockwise,
        );
    const PIPELINE_BLEND_STATE: &'static br::PipelineColorBlendStateCreateInfo<'static> =
        &br::PipelineColorBlendStateCreateInfo::new(&[
            br::vk::VkPipelineColorBlendAttachmentState::PREMULTIPLIED,
        ]);

    pub fn new(base_sys: &mut AppBaseSystem<'subsystem>, rt: &PrimaryRenderTarget) -> Self {
        let rp_grabbed = base_sys
            .create_render_pass(&br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(rt.color_format())
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
            ))
            .unwrap();
        base_sys
            .subsystem
            .dbg_set_name(&rp_grabbed, c"CompositeRenderer::rp[grabbed]");
        let rp_final = base_sys
            .create_render_pass(&br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(rt.color_format())
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
            ))
            .unwrap();
        base_sys
            .subsystem
            .dbg_set_name(&rp_final, c"CompositeRenderer::rp[final]");
        let rp_continue_grabbed = base_sys
            .create_render_pass(&br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(rt.color_format())
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
            ))
            .unwrap();
        base_sys
            .subsystem
            .dbg_set_name(&rp_continue_grabbed, c"CompositeRenderer::rp[grabbed,cont]");
        let rp_continue_final = base_sys
            .create_render_pass(&br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(rt.color_format())
                    .with_layout_to(
                        br::ImageLayout::PresentSrc.from(br::ImageLayout::TransferSrcOpt),
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
                    br::PipelineStageFlags(0),
                )
                .of_memory(
                    br::AccessFlags::COLOR_ATTACHMENT.write,
                    br::AccessFlags::MEMORY.read,
                )
                .by_region()],
            ))
            .unwrap();
        base_sys
            .subsystem
            .dbg_set_name(&rp_continue_final, c"CompositeRenderer::rp[final,cont]");

        let mut fbs_grabbed = Vec::with_capacity(rt.backbuffer_count());
        let mut fbs_final = Vec::with_capacity(rt.backbuffer_count());
        let mut fbs_continue_grabbed = Vec::with_capacity(rt.backbuffer_count());
        let mut fbs_continue_final = Vec::with_capacity(rt.backbuffer_count());
        for bb in rt.backbuffer_views() {
            fbs_grabbed.push(
                br::FramebufferObject::new(
                    base_sys.subsystem,
                    &br::FramebufferCreateInfo::new(
                        &rp_grabbed,
                        &[bb.as_transparent_ref()],
                        rt.size.width,
                        rt.size.height,
                    ),
                )
                .unwrap(),
            );
            fbs_final.push(
                br::FramebufferObject::new(
                    base_sys.subsystem,
                    &br::FramebufferCreateInfo::new(
                        &rp_final,
                        &[bb.as_transparent_ref()],
                        rt.size.width,
                        rt.size.height,
                    ),
                )
                .unwrap(),
            );
            fbs_continue_grabbed.push(
                br::FramebufferObject::new(
                    base_sys.subsystem,
                    &br::FramebufferCreateInfo::new(
                        &rp_continue_grabbed,
                        &[bb.as_transparent_ref()],
                        rt.size.width,
                        rt.size.height,
                    ),
                )
                .unwrap(),
            );
            fbs_continue_final.push(
                br::FramebufferObject::new(
                    base_sys.subsystem,
                    &br::FramebufferCreateInfo::new(
                        &rp_continue_final,
                        &[bb.as_transparent_ref()],
                        rt.size.width,
                        rt.size.height,
                    ),
                )
                .unwrap(),
            );
        }

        let sampler =
            br::SamplerObject::new(base_sys.subsystem, &br::SamplerCreateInfo::new()).unwrap();

        let dsl_input = br::DescriptorSetLayoutObject::new(
            base_sys.subsystem,
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
        let dsl_input_backdrop = br::DescriptorSetLayoutObject::new(
            base_sys.subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
                .make_binding(0, 1)
                .only_for_fragment()]),
        )
        .unwrap();
        let pipeline_layout = br::PipelineLayoutObject::new(
            base_sys.subsystem,
            &br::PipelineLayoutCreateInfo::new(
                &[
                    dsl_input.as_transparent_ref(),
                    dsl_input_backdrop.as_transparent_ref(),
                ],
                COMPOSITE_PUSH_CONSTANT_RANGES,
            ),
        )
        .unwrap();

        let vsh = base_sys.require_shader("resources/composite.vert");
        let fsh = base_sys.require_shader("resources/composite.frag");
        let shader_stages = [
            vsh.on_stage(br::ShaderStage::Vertex, c"main"),
            fsh.on_stage(br::ShaderStage::Fragment, c"main"),
        ];
        let viewports = [rt
            .size
            .into_rect(br::Offset2D::ZERO)
            .make_viewport(0.0..1.0)];
        let scissors = [rt.size.into_rect(br::Offset2D::ZERO)];
        let vp_state = br::PipelineViewportStateCreateInfo::new_array(&viewports, &scissors);
        let [
            pipeline_grabbed,
            pipeline_final,
            pipeline_continue_grabbed,
            pipeline_continue_final,
        ] = base_sys
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    &pipeline_layout,
                    rp_grabbed.subpass(0),
                    &shader_stages,
                    Self::PIPELINE_VI_STATE,
                    Self::PIPELINE_IA_STATE,
                    &vp_state,
                    Self::PIPELINE_RASTER_STATE,
                    Self::PIPELINE_BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &pipeline_layout,
                    rp_final.subpass(0),
                    &shader_stages,
                    Self::PIPELINE_VI_STATE,
                    Self::PIPELINE_IA_STATE,
                    &vp_state,
                    Self::PIPELINE_RASTER_STATE,
                    Self::PIPELINE_BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &pipeline_layout,
                    rp_continue_grabbed.subpass(0),
                    &shader_stages,
                    Self::PIPELINE_VI_STATE,
                    Self::PIPELINE_IA_STATE,
                    &vp_state,
                    Self::PIPELINE_RASTER_STATE,
                    Self::PIPELINE_BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &pipeline_layout,
                    rp_continue_final.subpass(0),
                    &shader_stages,
                    Self::PIPELINE_VI_STATE,
                    Self::PIPELINE_IA_STATE,
                    &vp_state,
                    Self::PIPELINE_RASTER_STATE,
                    Self::PIPELINE_BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        let backdrop_buffers =
            Vec::<br::ImageViewObject<br::ImageObject<&Subsystem>>>::with_capacity(
                Self::INITIAL_BACKDROP_BUFFER_COUNT,
            );
        let backdrop_buffer_memory = br::DeviceMemoryObject::new(
            base_sys.subsystem,
            &br::MemoryAllocateInfo::new(10, base_sys.find_device_local_memory_index(!0).unwrap()),
        )
        .unwrap();
        let backdrop_blur_destination_fbs = Vec::with_capacity(Self::INITIAL_BACKDROP_BUFFER_COUNT);

        let mut grab_buffer = br::ImageObject::new(
            base_sys.subsystem,
            &br::ImageCreateInfo::new(rt.size, rt.color_format())
                .with_usage(br::ImageUsageFlags::SAMPLED | br::ImageUsageFlags::TRANSFER_DEST),
        )
        .unwrap();
        let grab_buffer_memory =
            base_sys.alloc_device_local_memory_for_requirements(&grab_buffer.requirements());
        grab_buffer.bind(&grab_buffer_memory, 0).unwrap();
        let grab_buffer = br::ImageViewBuilder::new(
            grab_buffer,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()
        .unwrap();

        let input_backdrop_descriptor_pool = br::DescriptorPoolObject::new(
            base_sys.subsystem,
            &br::DescriptorPoolCreateInfo::new(
                16,
                &[br::DescriptorType::CombinedImageSampler
                    .make_size(Self::INITIAL_BACKDROP_BUFFER_COUNT as _)],
            ),
        )
        .unwrap();
        let input_backdrop_descriptor_sets =
            Vec::<br::DescriptorSet>::with_capacity(Self::INITIAL_BACKDROP_BUFFER_COUNT);

        let backdrop_fx_blur_processor =
            BackdropEffectBlurProcessor::new(base_sys, rt.size, rt.color_format());

        let mut fixed_descriptor_pool = br::DescriptorPoolObject::new(
            base_sys.subsystem,
            &br::DescriptorPoolCreateInfo::new(
                (1 + backdrop_fx_blur_processor.fixed_descriptor_set_count()) as _,
                &[
                    br::DescriptorType::CombinedImageSampler.make_size(
                        (1 + backdrop_fx_blur_processor.fixed_descriptor_set_count()) as _,
                    ),
                    br::DescriptorType::UniformBuffer.make_size(1),
                    br::DescriptorType::StorageBuffer.make_size(1),
                ],
            ),
        )
        .unwrap();
        let [alphamask_group_input_descriptor_set] = fixed_descriptor_pool
            .alloc_array(&[dsl_input.as_transparent_ref()])
            .unwrap();
        let blur_fixed_descriptor_sets =
            backdrop_fx_blur_processor.alloc_fixed_descriptor_sets(&mut fixed_descriptor_pool);
        let mut descriptor_writes = vec![
            alphamask_group_input_descriptor_set.binding_at(0).write(
                br::DescriptorContents::storage_buffer(
                    base_sys.composite_instance_manager.buffer_transparent_ref(),
                    0..(core::mem::size_of::<CompositeInstanceData>() * 1024) as _,
                ),
            ),
            alphamask_group_input_descriptor_set.binding_at(1).write(
                br::DescriptorContents::uniform_buffer(
                    base_sys
                        .composite_instance_manager
                        .streaming_buffer_transparent_ref(),
                    0..core::mem::size_of::<CompositeStreamingData>() as _,
                ),
            ),
            alphamask_group_input_descriptor_set.binding_at(2).write(
                br::DescriptorContents::CombinedImageSampler(vec![
                    br::DescriptorImageInfo::new(
                        base_sys.mask_atlas_resource_transparent_ref(),
                        br::ImageLayout::ShaderReadOnlyOpt,
                    )
                    .with_sampler(&sampler),
                ]),
            ),
        ];
        backdrop_fx_blur_processor.write_input_descriptor_sets(
            &mut descriptor_writes,
            &grab_buffer,
            &blur_fixed_descriptor_sets,
        );
        base_sys
            .subsystem
            .update_descriptor_sets(&descriptor_writes, &[]);

        Self {
            gfx_device: base_sys.subsystem,
            rp_grabbed,
            rp_final,
            rp_continue_grabbed,
            rp_continue_final,
            fbs_grabbed: fbs_grabbed.into_iter().map(|x| x.unmanage().0).collect(),
            fbs_final: fbs_final.into_iter().map(|x| x.unmanage().0).collect(),
            fbs_continue_grabbed: fbs_continue_grabbed
                .into_iter()
                .map(|x| x.unmanage().0)
                .collect(),
            fbs_continue_final: fbs_continue_final
                .into_iter()
                .map(|x| x.unmanage().0)
                .collect(),
            sampler,
            _dsl_input: dsl_input,
            dsl_input_backdrop,
            pipeline_layout,
            pipeline_grabbed,
            pipeline_final,
            pipeline_continue_grabbed,
            pipeline_continue_final,
            grab_buffer,
            grab_buffer_memory,
            backdrop_buffers,
            backdrop_buffer_memory,
            backdrop_blur_destination_fbs,
            backdrop_buffers_invalidated: true,
            input_backdrop_descriptor_pool,
            input_backdrop_descriptor_sets,
            input_backdrop_descriptor_pool_capacity: Self::INITIAL_BACKDROP_BUFFER_COUNT,
            backdrop_fx_blur_processor,
            _fixed_descriptor_pool: fixed_descriptor_pool,
            alphamask_group_input_descriptor_set,
            blur_fixed_descriptor_sets,
        }
    }

    #[inline]
    fn release_all_framebuffers(
        gfx_device: &(impl br::VkHandle<Handle = br::vk::VkDevice> + ?Sized),
        fbs: &mut Vec<br::vk::VkFramebuffer>,
    ) {
        for x in fbs.drain(..) {
            unsafe {
                br::vkfn_wrapper::destroy_framebuffer(gfx_device.native_ptr(), x, None);
            }
        }
    }

    pub fn recreate_rt_resources<'s>(
        &'s mut self,
        base_sys: &mut AppBaseSystem<'subsystem>,
        rt: &PrimaryRenderTarget,
        descriptor_writes: &mut Vec<br::DescriptorSetWriteInfo<'s>>,
    ) {
        Self::release_all_framebuffers(self.gfx_device, &mut self.fbs_grabbed);
        Self::release_all_framebuffers(self.gfx_device, &mut self.fbs_final);
        Self::release_all_framebuffers(self.gfx_device, &mut self.fbs_continue_grabbed);
        Self::release_all_framebuffers(self.gfx_device, &mut self.fbs_continue_final);
        let mut fbs_grabbed = Vec::with_capacity(rt.backbuffer_count());
        let mut fbs_final = Vec::with_capacity(rt.backbuffer_count());
        let mut fbs_continue_grabbed = Vec::with_capacity(rt.backbuffer_count());
        let mut fbs_continue_final = Vec::with_capacity(rt.backbuffer_count());
        for bb in rt.backbuffer_views() {
            fbs_grabbed.push(
                br::FramebufferObject::new(
                    self.gfx_device,
                    &br::FramebufferCreateInfo::new(
                        &self.rp_grabbed,
                        &[bb.as_transparent_ref()],
                        rt.size.width,
                        rt.size.height,
                    ),
                )
                .unwrap(),
            );
            fbs_final.push(
                br::FramebufferObject::new(
                    self.gfx_device,
                    &br::FramebufferCreateInfo::new(
                        &self.rp_final,
                        &[bb.as_transparent_ref()],
                        rt.size.width,
                        rt.size.height,
                    ),
                )
                .unwrap(),
            );
            fbs_continue_grabbed.push(
                br::FramebufferObject::new(
                    self.gfx_device,
                    &br::FramebufferCreateInfo::new(
                        &self.rp_continue_grabbed,
                        &[bb.as_transparent_ref()],
                        rt.size.width,
                        rt.size.height,
                    ),
                )
                .unwrap(),
            );
            fbs_continue_final.push(
                br::FramebufferObject::new(
                    self.gfx_device,
                    &br::FramebufferCreateInfo::new(
                        &self.rp_continue_final,
                        &[bb.as_transparent_ref()],
                        rt.size.width,
                        rt.size.height,
                    ),
                )
                .unwrap(),
            );
        }

        self.backdrop_buffers_invalidated = true;

        unsafe {
            // release first
            core::ptr::drop_in_place(&mut self.grab_buffer);
            core::ptr::drop_in_place(&mut self.grab_buffer_memory);
        }
        let mut grab_buffer = br::ImageObject::new(
            self.gfx_device,
            &br::ImageCreateInfo::new(rt.size, rt.color_format())
                .with_usage(br::ImageUsageFlags::SAMPLED | br::ImageUsageFlags::TRANSFER_DEST),
        )
        .unwrap();
        let grab_buffer_memory =
            base_sys.alloc_device_local_memory_for_requirements(&grab_buffer.requirements());
        grab_buffer.bind(&grab_buffer_memory, 0).unwrap();
        let grab_buffer = br::ImageViewBuilder::new(
            grab_buffer,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()
        .unwrap();
        unsafe {
            core::ptr::write(&mut self.grab_buffer, grab_buffer);
            core::ptr::write(&mut self.grab_buffer_memory, grab_buffer_memory);
        }

        let vsh = base_sys.require_shader("resources/composite.vert");
        let fsh = base_sys.require_shader("resources/composite.frag");
        let shader_stages = [
            vsh.on_stage(br::ShaderStage::Vertex, c"main"),
            fsh.on_stage(br::ShaderStage::Fragment, c"main"),
        ];
        let viewports = [rt
            .size
            .into_rect(br::Offset2D::ZERO)
            .make_viewport(0.0..1.0)];
        let scissors = [rt.size.into_rect(br::Offset2D::ZERO)];
        let vp_state = br::PipelineViewportStateCreateInfo::new_array(&viewports, &scissors);
        let [
            pipeline_grabbed,
            pipeline_final,
            pipeline_continue_grabbed,
            pipeline_continue_final,
        ] = base_sys
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    &self.pipeline_layout,
                    self.rp_grabbed.subpass(0),
                    &shader_stages,
                    Self::PIPELINE_VI_STATE,
                    Self::PIPELINE_IA_STATE,
                    &vp_state,
                    Self::PIPELINE_RASTER_STATE,
                    Self::PIPELINE_BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &self.pipeline_layout,
                    self.rp_final.subpass(0),
                    &shader_stages,
                    Self::PIPELINE_VI_STATE,
                    Self::PIPELINE_IA_STATE,
                    &vp_state,
                    Self::PIPELINE_RASTER_STATE,
                    Self::PIPELINE_BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &self.pipeline_layout,
                    self.rp_continue_grabbed.subpass(0),
                    &shader_stages,
                    Self::PIPELINE_VI_STATE,
                    Self::PIPELINE_IA_STATE,
                    &vp_state,
                    Self::PIPELINE_RASTER_STATE,
                    Self::PIPELINE_BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &self.pipeline_layout,
                    self.rp_continue_final.subpass(0),
                    &shader_stages,
                    Self::PIPELINE_VI_STATE,
                    Self::PIPELINE_IA_STATE,
                    &vp_state,
                    Self::PIPELINE_RASTER_STATE,
                    Self::PIPELINE_BLEND_STATE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();
        self.pipeline_grabbed = pipeline_grabbed;
        self.pipeline_final = pipeline_final;
        self.pipeline_continue_grabbed = pipeline_continue_grabbed;
        self.pipeline_continue_final = pipeline_continue_final;

        self.backdrop_fx_blur_processor
            .recreate_rt_resources(base_sys, rt.size, rt.color_format());
        self.backdrop_fx_blur_processor.write_input_descriptor_sets(
            descriptor_writes,
            &self.grab_buffer,
            &self.blur_fixed_descriptor_sets,
        );

        self.fbs_grabbed
            .extend(fbs_grabbed.into_iter().map(|x| x.unmanage().0));
        self.fbs_final
            .extend(fbs_final.into_iter().map(|x| x.unmanage().0));
        self.fbs_continue_grabbed
            .extend(fbs_continue_grabbed.into_iter().map(|x| x.unmanage().0));
        self.fbs_continue_final
            .extend(fbs_continue_final.into_iter().map(|x| x.unmanage().0));
    }

    pub fn ready_input_backdrop_descriptor_sets(&mut self, required_count: usize) {
        if required_count == self.input_backdrop_descriptor_sets.len() {
            // no changes
            return;
        }

        if required_count > self.input_backdrop_descriptor_pool_capacity {
            // resize pool
            let object_count = required_count.max(1);

            self.input_backdrop_descriptor_pool = br::DescriptorPoolObject::new(
                self.gfx_device,
                &br::DescriptorPoolCreateInfo::new(
                    object_count as _,
                    &[br::DescriptorType::CombinedImageSampler.make_size(object_count as _)],
                ),
            )
            .unwrap();
            self.input_backdrop_descriptor_pool_capacity = object_count;
        } else {
            // just reset
            unsafe {
                self.input_backdrop_descriptor_pool.reset(0).unwrap();
            }
        }

        self.input_backdrop_descriptor_sets.clear();
        self.input_backdrop_descriptor_sets.extend(
            self.input_backdrop_descriptor_pool
                .alloc(
                    &core::iter::repeat_n(
                        self.dsl_input_backdrop.as_transparent_ref(),
                        required_count.max(1),
                    )
                    .collect::<Vec<_>>(),
                )
                .unwrap(),
        );
        self.backdrop_buffers_invalidated = true;
    }

    pub fn update_backdrop_resources(
        &mut self,
        base_sys: &AppBaseSystem<'subsystem>,
        rt: &PrimaryRenderTarget,
    ) -> bool {
        if !self.backdrop_buffers_invalidated {
            // no changes
            return false;
        }

        Self::release_all_framebuffers(self.gfx_device, &mut self.backdrop_blur_destination_fbs);
        self.backdrop_buffers.clear();
        unsafe {
            core::ptr::drop_in_place(&mut self.backdrop_buffer_memory);
        }
        let backdrop_count = self.input_backdrop_descriptor_sets.len();
        let mut image_objects = Vec::with_capacity(backdrop_count);
        let mut offsets = Vec::with_capacity(backdrop_count);
        let mut top = 0u64;
        let mut memory_index_mask = !0u32;
        for _ in 0..backdrop_count {
            let image = br::ImageObject::new(
                self.gfx_device,
                &br::ImageCreateInfo::new(rt.size, rt.color_format()).with_usage(
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
        let Some(memindex) = base_sys.find_device_local_memory_index(memory_index_mask) else {
            tracing::error!(
                memory_index_mask,
                "no suitable memory for composition backdrop buffers"
            );
            std::process::exit(1);
        };
        let backdrop_buffer_memory = br::DeviceMemoryObject::new(
            self.gfx_device,
            &br::MemoryAllocateInfo::new(top.max(64), memindex),
        )
        .unwrap();
        for (mut r, o) in image_objects.into_iter().zip(offsets.into_iter()) {
            r.bind(&backdrop_buffer_memory, o as _).unwrap();

            self.backdrop_buffers.push(
                br::ImageViewBuilder::new(
                    r,
                    br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
                )
                .create()
                .unwrap(),
            );
        }
        unsafe {
            core::ptr::write(&mut self.backdrop_buffer_memory, backdrop_buffer_memory);
        }

        self.backdrop_blur_destination_fbs
            .extend(self.backdrop_buffers.iter().map(|b| {
                br::FramebufferObject::new(
                    self.gfx_device,
                    &br::FramebufferCreateInfo::new(
                        self.backdrop_fx_blur_processor.final_render_pass(),
                        &[b.as_transparent_ref()],
                        rt.size.width,
                        rt.size.height,
                    ),
                )
                .unwrap()
                .unmanage()
                .0
            }));

        base_sys.subsystem.update_descriptor_sets(
            &self
                .backdrop_buffers
                .iter()
                .zip(self.input_backdrop_descriptor_sets.iter())
                .map(|(v, d)| {
                    d.binding_at(0)
                        .write(br::DescriptorContents::CombinedImageSampler(vec![
                            br::DescriptorImageInfo::new(v, br::ImageLayout::ShaderReadOnlyOpt)
                                .with_sampler(&self.sampler),
                        ]))
                })
                .collect::<Vec<_>>(),
            &[],
        );

        self.backdrop_buffers_invalidated = false;
        true
    }

    #[inline]
    pub fn default_backdrop_buffer(
        &self,
    ) -> &(impl br::VkHandle<Handle = br::vk::VkImage> + ?Sized) {
        self.backdrop_buffers[0].image()
    }

    pub fn select_subpass(
        &self,
        requirements: &RenderPassRequirements,
    ) -> br::SubpassRef<impl br::RenderPass + ?Sized> {
        match requirements {
            RenderPassRequirements {
                after_operation: RenderPassAfterOperation::None,
                continued: false,
            } => self.rp_final.subpass(0),
            RenderPassRequirements {
                after_operation: RenderPassAfterOperation::None,
                continued: true,
            } => self.rp_continue_final.subpass(0),
            RenderPassRequirements {
                after_operation: RenderPassAfterOperation::Grab,
                continued: false,
            } => self.rp_grabbed.subpass(0),
            RenderPassRequirements {
                after_operation: RenderPassAfterOperation::Grab,
                continued: true,
            } => self.rp_continue_grabbed.subpass(0),
        }
    }

    #[inline]
    pub fn subpass_final(&self) -> br::SubpassRef<impl br::RenderPass + ?Sized> {
        self.rp_final.subpass(0)
    }

    #[inline]
    pub fn subpass_continue_final(&self) -> br::SubpassRef<impl br::RenderPass + ?Sized> {
        self.rp_continue_final.subpass(0)
    }

    pub fn populate_commands<'x>(
        &self,
        mut rec: br::CmdRecord<'x>,
        render_data: &CompositeRenderingData,
        rt_size: br::Extent2D,
        rt_image: &(impl br::VkHandle<Handle = br::vk::VkImage> + ?Sized),
        backbuffer_index: usize,
        mut custom_render: impl FnMut(CustomRenderToken, br::CmdRecord<'x>) -> br::CmdRecord<'x>,
    ) -> br::CmdRecord<'x> {
        let render_region = rt_size.into_rect(br::Offset2D::ZERO);

        let mut in_render_pass = false;
        let mut rpt_pointer = 0;
        let mut pipeline_bound = false;

        for x in render_data.instructions.iter() {
            match x {
                &CompositeRenderingInstruction::DrawInstanceRange {
                    ref index_range,
                    backdrop_buffer,
                } => {
                    if !in_render_pass {
                        in_render_pass = true;

                        let (rp, fb);
                        match &render_data.render_passes[rpt_pointer] {
                            RenderPassRequirements {
                                continued: false,
                                after_operation: RenderPassAfterOperation::Grab,
                            } => {
                                rp = &self.rp_grabbed;
                                fb = self.fbs_grabbed[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: false,
                                after_operation: RenderPassAfterOperation::None,
                            } => {
                                rp = &self.rp_final;
                                fb = self.fbs_final[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: true,
                                after_operation: RenderPassAfterOperation::Grab,
                            } => {
                                rp = &self.rp_continue_grabbed;
                                fb = self.fbs_continue_grabbed[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: true,
                                after_operation: RenderPassAfterOperation::None,
                            } => {
                                rp = &self.rp_continue_final;
                                fb = self.fbs_continue_final[backbuffer_index];
                            }
                        };

                        rec = rec.inject(|r| {
                            inject_cmd_begin_render_pass2(
                                r,
                                self.gfx_device,
                                &br::RenderPassBeginInfo::new(
                                    rp,
                                    br::VkHandleRef::from_raw_ref(&fb),
                                    render_region,
                                    &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 1.0])],
                                ),
                                &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                            )
                        });
                    }
                    if !pipeline_bound {
                        pipeline_bound = true;

                        rec = rec
                            .bind_pipeline(
                                br::PipelineBindPoint::Graphics,
                                match &render_data.render_passes[rpt_pointer] {
                                    RenderPassRequirements {
                                        continued: false,
                                        after_operation: RenderPassAfterOperation::Grab,
                                    } => &self.pipeline_grabbed,
                                    RenderPassRequirements {
                                        continued: false,
                                        after_operation: RenderPassAfterOperation::None,
                                    } => &self.pipeline_final,
                                    RenderPassRequirements {
                                        continued: true,
                                        after_operation: RenderPassAfterOperation::Grab,
                                    } => &self.pipeline_continue_grabbed,
                                    RenderPassRequirements {
                                        continued: true,
                                        after_operation: RenderPassAfterOperation::None,
                                    } => &self.pipeline_continue_final,
                                },
                            )
                            .push_constant(
                                &self.pipeline_layout,
                                br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                                0,
                                &[rt_size.width as f32, rt_size.height as f32],
                            )
                            .bind_descriptor_sets(
                                br::PipelineBindPoint::Graphics,
                                &self.pipeline_layout,
                                0,
                                &[self.alphamask_group_input_descriptor_set],
                                &[],
                            );
                    }

                    rec = rec
                        .bind_descriptor_sets(
                            br::PipelineBindPoint::Graphics,
                            &self.pipeline_layout,
                            1,
                            &[self.input_backdrop_descriptor_sets[backdrop_buffer]],
                            &[],
                        )
                        .draw(4, index_range.len() as _, 0, index_range.start as _)
                }
                &CompositeRenderingInstruction::InsertCustomRenderCommands(token) => {
                    if !in_render_pass {
                        in_render_pass = true;

                        let (rp, fb);
                        match &render_data.render_passes[rpt_pointer] {
                            RenderPassRequirements {
                                continued: false,
                                after_operation: RenderPassAfterOperation::Grab,
                            } => {
                                rp = &self.rp_grabbed;
                                fb = self.fbs_grabbed[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: false,
                                after_operation: RenderPassAfterOperation::None,
                            } => {
                                rp = &self.rp_final;
                                fb = self.fbs_final[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: true,
                                after_operation: RenderPassAfterOperation::Grab,
                            } => {
                                rp = &self.rp_continue_grabbed;
                                fb = self.fbs_continue_grabbed[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: true,
                                after_operation: RenderPassAfterOperation::None,
                            } => {
                                rp = &self.rp_continue_final;
                                fb = self.fbs_continue_final[backbuffer_index];
                            }
                        };

                        rec = rec.inject(|r| {
                            inject_cmd_begin_render_pass2(
                                r,
                                self.gfx_device,
                                &br::RenderPassBeginInfo::new(
                                    rp,
                                    br::VkHandleRef::from_raw_ref(&fb),
                                    render_region,
                                    &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 1.0])],
                                ),
                                &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                            )
                        });
                    }

                    rec = custom_render(token, rec);

                    // 別のパイプラインをつかっている可能性があるのでいったん紐づいているのを無効化する
                    pipeline_bound = false;
                }
                &CompositeRenderingInstruction::SetClip {
                    ref shader_parameters,
                } => {
                    if !in_render_pass {
                        in_render_pass = true;

                        let (rp, fb);
                        match &render_data.render_passes[rpt_pointer] {
                            RenderPassRequirements {
                                continued: false,
                                after_operation: RenderPassAfterOperation::Grab,
                            } => {
                                rp = &self.rp_grabbed;
                                fb = self.fbs_grabbed[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: false,
                                after_operation: RenderPassAfterOperation::None,
                            } => {
                                rp = &self.rp_final;
                                fb = self.fbs_final[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: true,
                                after_operation: RenderPassAfterOperation::Grab,
                            } => {
                                rp = &self.rp_continue_grabbed;
                                fb = self.fbs_continue_grabbed[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: true,
                                after_operation: RenderPassAfterOperation::None,
                            } => {
                                rp = &self.rp_continue_final;
                                fb = self.fbs_continue_final[backbuffer_index];
                            }
                        };

                        rec = rec.inject(|r| {
                            inject_cmd_begin_render_pass2(
                                r,
                                self.gfx_device,
                                &br::RenderPassBeginInfo::new(
                                    rp,
                                    br::VkHandleRef::from_raw_ref(&fb),
                                    render_region,
                                    &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 1.0])],
                                ),
                                &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                            )
                        });
                    }
                    if !pipeline_bound {
                        pipeline_bound = true;

                        rec = rec
                            .bind_pipeline(
                                br::PipelineBindPoint::Graphics,
                                match &render_data.render_passes[rpt_pointer] {
                                    RenderPassRequirements {
                                        continued: false,
                                        after_operation: RenderPassAfterOperation::Grab,
                                    } => &self.pipeline_grabbed,
                                    RenderPassRequirements {
                                        continued: false,
                                        after_operation: RenderPassAfterOperation::None,
                                    } => &self.pipeline_final,
                                    RenderPassRequirements {
                                        continued: true,
                                        after_operation: RenderPassAfterOperation::Grab,
                                    } => &self.pipeline_continue_grabbed,
                                    RenderPassRequirements {
                                        continued: true,
                                        after_operation: RenderPassAfterOperation::None,
                                    } => &self.pipeline_continue_final,
                                },
                            )
                            .push_constant(
                                &self.pipeline_layout,
                                br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                                0,
                                &[rt_size.width as f32, rt_size.height as f32],
                            )
                            .bind_descriptor_sets(
                                br::PipelineBindPoint::Graphics,
                                &self.pipeline_layout,
                                0,
                                &[self.alphamask_group_input_descriptor_set],
                                &[],
                            );
                    }

                    rec = rec.push_constant(
                        &self.pipeline_layout,
                        br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
                        16,
                        &[
                            shader_parameters[0].value() / rt_size.width as f32,
                            shader_parameters[1].value() / rt_size.height as f32,
                            shader_parameters[2].value() / rt_size.width as f32,
                            shader_parameters[3].value() / rt_size.height as f32,
                            shader_parameters[4].value() / rt_size.width as f32,
                            shader_parameters[5].value() / rt_size.height as f32,
                            shader_parameters[6].value() / rt_size.width as f32,
                            shader_parameters[7].value() / rt_size.height as f32,
                        ],
                    );
                }
                &CompositeRenderingInstruction::ClearClip => {
                    if !in_render_pass {
                        in_render_pass = true;

                        let (rp, fb);
                        match &render_data.render_passes[rpt_pointer] {
                            RenderPassRequirements {
                                continued: false,
                                after_operation: RenderPassAfterOperation::Grab,
                            } => {
                                rp = &self.rp_grabbed;
                                fb = self.fbs_grabbed[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: false,
                                after_operation: RenderPassAfterOperation::None,
                            } => {
                                rp = &self.rp_final;
                                fb = self.fbs_final[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: true,
                                after_operation: RenderPassAfterOperation::Grab,
                            } => {
                                rp = &self.rp_continue_grabbed;
                                fb = self.fbs_continue_grabbed[backbuffer_index];
                            }
                            RenderPassRequirements {
                                continued: true,
                                after_operation: RenderPassAfterOperation::None,
                            } => {
                                rp = &self.rp_continue_final;
                                fb = self.fbs_continue_final[backbuffer_index];
                            }
                        };

                        rec = rec.inject(|r| {
                            inject_cmd_begin_render_pass2(
                                r,
                                self.gfx_device,
                                &br::RenderPassBeginInfo::new(
                                    rp,
                                    br::VkHandleRef::from_raw_ref(&fb),
                                    render_region,
                                    &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 1.0])],
                                ),
                                &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                            )
                        });
                    }
                    if !pipeline_bound {
                        pipeline_bound = true;

                        rec = rec
                            .bind_pipeline(
                                br::PipelineBindPoint::Graphics,
                                match &render_data.render_passes[rpt_pointer] {
                                    RenderPassRequirements {
                                        continued: false,
                                        after_operation: RenderPassAfterOperation::Grab,
                                    } => &self.pipeline_grabbed,
                                    RenderPassRequirements {
                                        continued: false,
                                        after_operation: RenderPassAfterOperation::None,
                                    } => &self.pipeline_final,
                                    RenderPassRequirements {
                                        continued: true,
                                        after_operation: RenderPassAfterOperation::Grab,
                                    } => &self.pipeline_continue_grabbed,
                                    RenderPassRequirements {
                                        continued: true,
                                        after_operation: RenderPassAfterOperation::None,
                                    } => &self.pipeline_continue_final,
                                },
                            )
                            .push_constant(
                                &self.pipeline_layout,
                                br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                                0,
                                &[rt_size.width as f32, rt_size.height as f32],
                            )
                            .bind_descriptor_sets(
                                br::PipelineBindPoint::Graphics,
                                &self.pipeline_layout,
                                0,
                                &[self.alphamask_group_input_descriptor_set],
                                &[],
                            );
                    }

                    rec = rec.push_constant(
                        &self.pipeline_layout,
                        br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
                        16,
                        &[0.0f32, 0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0],
                    );
                }
                CompositeRenderingInstruction::GrabBackdrop => {
                    rec = rec
                        .inject(|r| {
                            inject_cmd_end_render_pass2(
                                r,
                                self.gfx_device,
                                &br::SubpassEndInfo::new(),
                            )
                        })
                        .inject(|r| {
                            inject_cmd_pipeline_barrier_2(
                                r,
                                self.gfx_device,
                                &br::DependencyInfo::new(
                                    &[],
                                    &[],
                                    &[br::ImageMemoryBarrier2::new(
                                        self.grab_buffer.image(),
                                        br::ImageSubresourceRange::new(
                                            br::AspectMask::COLOR,
                                            0..1,
                                            0..1,
                                        ),
                                    )
                                    .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
                                ),
                            )
                        })
                        .copy_image(
                            rt_image,
                            br::ImageLayout::TransferSrcOpt,
                            self.grab_buffer.image(),
                            br::ImageLayout::TransferDestOpt,
                            &[br::ImageCopy {
                                srcSubresource: br::ImageSubresourceLayers::new(
                                    br::AspectMask::COLOR,
                                    0,
                                    0..1,
                                ),
                                dstSubresource: br::ImageSubresourceLayers::new(
                                    br::AspectMask::COLOR,
                                    0,
                                    0..1,
                                ),
                                srcOffset: br::Offset3D::ZERO,
                                dstOffset: br::Offset3D::ZERO,
                                extent: rt_size.with_depth(1),
                            }],
                        )
                        .inject(|r| {
                            inject_cmd_pipeline_barrier_2(
                                r,
                                self.gfx_device,
                                &br::DependencyInfo::new(
                                    &[],
                                    &[],
                                    &[br::ImageMemoryBarrier2::new(
                                        self.grab_buffer.image(),
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
                                ),
                            )
                        });
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
                    rec = self.backdrop_fx_blur_processor.populate_commands(
                        rec,
                        stdev,
                        br::VkHandleRef::from_raw_ref(
                            &self.backdrop_blur_destination_fbs[dest_backdrop_buffer],
                        ),
                        self.gfx_device,
                        rt_size,
                        &self.blur_fixed_descriptor_sets,
                    );
                }
            };
        }

        rec
    }
}

pub struct BackdropEffectBlurProcessor<'subsystem> {
    render_pass: br::RenderPassObject<&'subsystem Subsystem>,
    temporal_buffers: Vec<br::ImageViewObject<br::ImageObject<&'subsystem Subsystem>>>,
    temporal_buffer_memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    downsample_pass_fbs: Vec<br::vk::VkFramebuffer>,
    upsample_pass_fixed_fbs: Vec<br::vk::VkFramebuffer>,
    _sampler: br::SamplerObject<&'subsystem Subsystem>,
    input_dsl: br::DescriptorSetLayoutObject<&'subsystem Subsystem>,
    pipeline_layout: br::PipelineLayoutObject<&'subsystem Subsystem>,
    downsample_pipelines: Vec<br::PipelineObject<&'subsystem Subsystem>>,
    upsample_pipelines: Vec<br::PipelineObject<&'subsystem Subsystem>>,
}
impl Drop for BackdropEffectBlurProcessor<'_> {
    fn drop(&mut self) {
        self.clear_framebuffers();
    }
}
impl<'subsystem> BackdropEffectBlurProcessor<'subsystem> {
    #[tracing::instrument(name = "BackdropEffectBlurProcessor::new", skip(base_system))]
    pub fn new(
        base_system: &mut AppBaseSystem<'subsystem>,
        rt_size: br::Extent2D,
        rt_format: br::Format,
    ) -> Self {
        let render_pass = base_system
            .create_render_pass(&br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(rt_format)
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
            ))
            .unwrap();
        base_system.subsystem.dbg_set_name(
            &render_pass,
            c"Composite BackdropFx(Blur) ProcessRenderPass",
        );

        let mut temporal_buffers = Vec::with_capacity(BLUR_SAMPLE_STEPS);
        let temporal_buffer_memory =
            Self::create_temporal_buffers(base_system, rt_size, rt_format, &mut temporal_buffers);

        let (downsample_pass_fbs, upsample_pass_fixed_fbs) =
            Self::create_framebuffers(base_system, &temporal_buffers, &render_pass, rt_size);

        let sampler = match br::SamplerObject::new(
            base_system.subsystem,
            &br::SamplerCreateInfo::new()
                .filter(br::FilterMode::Linear, br::FilterMode::Linear)
                .addressing(
                    br::AddressingMode::MirroredRepeat,
                    br::AddressingMode::MirroredRepeat,
                    br::AddressingMode::MirroredRepeat,
                ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "blur sampler creation failed");
                std::process::abort();
            }
        };
        let input_dsl = match br::DescriptorSetLayoutObject::new(
            base_system.subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
                .make_binding(0, 1)
                .only_for_fragment()
                .with_immutable_samplers(&[sampler.as_transparent_ref()])]),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "blur input dsl creation failed");
                std::process::abort();
            }
        };

        let pipeline_layout = match br::PipelineLayoutObject::new(
            base_system.subsystem,
            &br::PipelineLayoutCreateInfo::new(
                &[input_dsl.as_transparent_ref()],
                &[br::PushConstantRange::for_type::<[f32; 3]>(
                    br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                )],
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "pipeline layout creation failed");
                std::process::abort();
            }
        };

        let (downsample_pipelines, upsample_pipelines) =
            Self::create_pipelines(base_system, rt_size, &pipeline_layout, &render_pass);

        Self {
            downsample_pass_fbs: downsample_pass_fbs
                .into_iter()
                .map(|x| x.unmanage().0)
                .collect(),
            upsample_pass_fixed_fbs: upsample_pass_fixed_fbs
                .into_iter()
                .map(|x| x.unmanage().0)
                .collect(),
            render_pass,
            temporal_buffers,
            temporal_buffer_memory,
            _sampler: sampler,
            input_dsl,
            pipeline_layout,
            downsample_pipelines,
            upsample_pipelines,
        }
    }

    fn create_pipelines(
        base_system: &mut AppBaseSystem<'subsystem>,
        rt_size: br::Extent2D,
        pipeline_layout: &(impl br::VkHandle<Handle = br::vk::VkPipelineLayout> + ?Sized),
        render_pass: &(impl br::RenderPass + ?Sized),
    ) -> (
        Vec<br::PipelineObject<&'subsystem Subsystem>>,
        Vec<br::PipelineObject<&'subsystem Subsystem>>,
    ) {
        let (downsample_vsh, downsample_fsh) = (
            base_system.require_shader("resources/dual_kawase_filter/downsample.vert"),
            base_system.require_shader("resources/dual_kawase_filter/downsample.frag"),
        );
        let (upsample_vsh, upsample_fsh) = (
            base_system.require_shader("resources/dual_kawase_filter/upsample.vert"),
            base_system.require_shader("resources/dual_kawase_filter/upsample.frag"),
        );
        let downsample_stages = [
            downsample_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
            downsample_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
        ];
        let upsample_stages = [
            upsample_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
            upsample_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
        ];

        let viewport_scissors = (0..=BLUR_SAMPLE_STEPS)
            .map(|lv| {
                let size = br::Extent2D {
                    width: rt_size.width >> lv,
                    height: rt_size.height >> lv,
                };

                (
                    [size.into_rect(br::Offset2D::ZERO).make_viewport(0.0..1.0)],
                    [size.into_rect(br::Offset2D::ZERO)],
                )
            })
            .collect::<Vec<_>>();
        let viewport_states = viewport_scissors
            .iter()
            .map(|(vp, sc)| br::PipelineViewportStateCreateInfo::new(vp, sc))
            .collect::<Vec<_>>();
        let downsample_pipelines = base_system
            .create_graphics_pipelines(
                &viewport_states[1..]
                    .iter()
                    .map(|vp_state| {
                        br::GraphicsPipelineCreateInfo::new(
                            &pipeline_layout,
                            render_pass.subpass(0),
                            &downsample_stages,
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
        let upsample_pipelines = base_system
            .create_graphics_pipelines(
                &viewport_states[..viewport_states.len() - 1]
                    .iter()
                    .map(|vp_state| {
                        br::GraphicsPipelineCreateInfo::new(
                            &pipeline_layout,
                            render_pass.subpass(0),
                            &upsample_stages,
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

        (downsample_pipelines, upsample_pipelines)
    }

    fn create_temporal_buffers(
        base_system: &mut AppBaseSystem<'subsystem>,
        rt_size: br::Extent2D,
        rt_format: br::Format,
        object_sink: &mut Vec<br::ImageViewObject<br::ImageObject<&'subsystem Subsystem>>>,
    ) -> br::DeviceMemoryObject<&'subsystem Subsystem> {
        let mut resources_offsets = Vec::with_capacity(BLUR_SAMPLE_STEPS);
        let mut top = 0;
        let mut memory_index_mask = !0u32;
        for lv in 1..=BLUR_SAMPLE_STEPS {
            let r = br::ImageObject::new(
                base_system.subsystem,
                &br::ImageCreateInfo::new(
                    br::Extent2D {
                        width: rt_size.width >> lv,
                        height: rt_size.height >> lv,
                    },
                    rt_format,
                )
                .with_usage(br::ImageUsageFlags::SAMPLED | br::ImageUsageFlags::COLOR_ATTACHMENT),
            )
            .unwrap();
            let req = r.requirements();
            assert!(req.alignment.is_power_of_two());
            let offset = (top + req.alignment - 1) & !(req.alignment - 1);

            top = offset + req.size;
            memory_index_mask &= req.memoryTypeBits;
            resources_offsets.push((r, offset));
        }
        let memory_object = base_system.alloc_device_local_memory(top, memory_index_mask);
        for (mut r, o) in resources_offsets {
            r.bind(&memory_object, o as _).unwrap();

            object_sink.push(
                br::ImageViewBuilder::new(
                    r,
                    br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
                )
                .create()
                .unwrap(),
            );
        }

        memory_object
    }

    /// returns: (downsample, upsample_fixed(only for temporal buffers))
    fn create_framebuffers<'r>(
        base_system: &mut AppBaseSystem<'subsystem>,
        temporal_buffers: &'r [br::ImageViewObject<br::ImageObject<&'subsystem Subsystem>>],
        render_pass: &(impl br::VkHandle<Handle = br::vk::VkRenderPass> + ?Sized),
        rt_size: br::Extent2D,
    ) -> (
        Vec<br::FramebufferObject<'r, &'subsystem Subsystem>>,
        Vec<br::FramebufferObject<'r, &'subsystem Subsystem>>,
    ) {
        let mut downsample_pass_fbs = Vec::with_capacity(temporal_buffers.len());
        let mut upsample_pass_fixed_fbs = Vec::with_capacity(temporal_buffers.len() - 1);
        for (n, b) in temporal_buffers.iter().enumerate() {
            let lv = n + 1;
            let bufsize = br::Extent2D {
                width: rt_size.width >> lv,
                height: rt_size.height >> lv,
            };

            downsample_pass_fbs.push(
                br::FramebufferObject::new(
                    base_system.subsystem,
                    &br::FramebufferCreateInfo::new(
                        render_pass,
                        &[b.as_transparent_ref()],
                        bufsize.width,
                        bufsize.height,
                    ),
                )
                .unwrap(),
            );
            if lv != temporal_buffers.len() {
                upsample_pass_fixed_fbs.push(
                    br::FramebufferObject::new(
                        base_system.subsystem,
                        &br::FramebufferCreateInfo::new(
                            render_pass,
                            &[b.as_transparent_ref()],
                            bufsize.width,
                            bufsize.height,
                        ),
                    )
                    .unwrap(),
                );
            }
        }

        (downsample_pass_fbs, upsample_pass_fixed_fbs)
    }

    fn clear_framebuffers(&mut self) {
        // assuming that Framebuffer and RenderPass are created from same Device
        for x in self.downsample_pass_fbs.drain(..) {
            unsafe {
                br::vkfn_wrapper::destroy_framebuffer(self.render_pass.device_handle(), x, None);
            }
        }
        for x in self.upsample_pass_fixed_fbs.drain(..) {
            unsafe {
                br::vkfn_wrapper::destroy_framebuffer(self.render_pass.device_handle(), x, None);
            }
        }
    }

    pub const fn fixed_descriptor_set_count(&self) -> usize {
        BLUR_SAMPLE_STEPS + 1
    }

    pub fn alloc_fixed_descriptor_sets(
        &self,
        dp: &mut (impl br::DescriptorPoolMut + ?Sized),
    ) -> Vec<br::DescriptorSet> {
        dp.alloc(
            &core::iter::repeat_n(
                self.input_dsl.as_transparent_ref(),
                self.fixed_descriptor_set_count(),
            )
            .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    pub fn write_input_descriptor_sets<'s>(
        &'s self,
        writes: &mut Vec<br::DescriptorSetWriteInfo<'s>>,
        first_input: &'s (impl br::VkHandle<Handle = br::vk::VkImageView> + ?Sized),
        descriptor_sets: &[br::DescriptorSet],
    ) {
        writes.reserve(1 + BLUR_SAMPLE_STEPS);
        self.write_first_input_descriptor_set(writes, first_input, descriptor_sets[0]);
        writes.extend((0..BLUR_SAMPLE_STEPS).map(|n| {
            descriptor_sets[n + 1].binding_at(0).write(
                br::DescriptorContents::CombinedImageSampler(vec![br::DescriptorImageInfo::new(
                    &self.temporal_buffers[n],
                    br::ImageLayout::ShaderReadOnlyOpt,
                )]),
            )
        }));
    }

    pub fn write_first_input_descriptor_set<'s>(
        &'s self,
        writes: &mut Vec<br::DescriptorSetWriteInfo<'s>>,
        first_input: &'s (impl br::VkHandle<Handle = br::vk::VkImageView> + ?Sized),
        descriptor_set: br::DescriptorSet,
    ) {
        writes.push(descriptor_set.binding_at(0).write(
            br::DescriptorContents::CombinedImageSampler(vec![br::DescriptorImageInfo::new(
                first_input,
                br::ImageLayout::ShaderReadOnlyOpt,
            )]),
        ));
    }

    pub const fn final_render_pass(&self) -> &(impl br::RenderPass + use<'subsystem>) {
        &self.render_pass
    }

    pub fn populate_commands<'x>(
        &self,
        mut rec: br::CmdRecord<'x>,
        mut stdev: SafeF32,
        dest_fb: &(impl br::VkHandle<Handle = br::vk::VkFramebuffer> + ?Sized),
        subsystem: &'subsystem Subsystem,
        rt_size: br::Extent2D,
        input_descriptor_sets: &[br::DescriptorSet],
    ) -> br::CmdRecord<'x> {
        let mut step_count = 0;
        // downsample
        for lv in 1..=BLUR_SAMPLE_STEPS {
            rec = rec
                .inject(|r| {
                    inject_cmd_begin_render_pass2(
                        r,
                        subsystem,
                        &br::RenderPassBeginInfo::new(
                            &self.render_pass,
                            &unsafe { br::VkHandleRef::dangling(self.downsample_pass_fbs[lv - 1]) },
                            br::Extent2D {
                                width: rt_size.width >> lv,
                                height: rt_size.height >> lv,
                            }
                            .into_rect(br::Offset2D::ZERO),
                            &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 0.0])],
                        ),
                        &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                    )
                })
                .bind_pipeline(
                    br::PipelineBindPoint::Graphics,
                    &self.downsample_pipelines[lv - 1],
                )
                .push_constant(
                    &self.pipeline_layout,
                    br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                    &[
                        ((rt_size.width >> (lv - 1)) as f32).recip(),
                        ((rt_size.height >> (lv - 1)) as f32).recip(),
                        stdev.value(),
                    ],
                )
                .bind_descriptor_sets(
                    br::PipelineBindPoint::Graphics,
                    &self.pipeline_layout,
                    0,
                    &[input_descriptor_sets[lv - 1]],
                    &[],
                )
                .draw(3, 1, 0, 0)
                .inject(|r| inject_cmd_end_render_pass2(r, subsystem, &br::SubpassEndInfo::new()));

            step_count += 1;
            stdev = unsafe { SafeF32::new_unchecked(stdev.value() / 2.0) };
            if stdev.value() < 0.5 {
                break;
            }
        }
        // upsample
        for lv in (0..step_count).rev() {
            rec = rec
                .inject(|r| {
                    inject_cmd_begin_render_pass2(
                        r,
                        subsystem,
                        &br::RenderPassBeginInfo::new(
                            &self.render_pass,
                            &if lv == 0 {
                                // final upsample
                                dest_fb.as_transparent_ref()
                            } else {
                                unsafe {
                                    br::VkHandleRef::dangling(self.upsample_pass_fixed_fbs[lv - 1])
                                }
                            },
                            br::Extent2D {
                                width: rt_size.width >> lv,
                                height: rt_size.height >> lv,
                            }
                            .into_rect(br::Offset2D::ZERO),
                            &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 0.0])],
                        ),
                        &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                    )
                })
                .bind_pipeline(
                    br::PipelineBindPoint::Graphics,
                    &self.upsample_pipelines[lv],
                )
                .push_constant(
                    &self.pipeline_layout,
                    br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                    &[
                        ((rt_size.width >> (lv + 1)) as f32).recip(),
                        ((rt_size.height >> (lv + 1)) as f32).recip(),
                        stdev.value(),
                    ],
                )
                .bind_descriptor_sets(
                    br::PipelineBindPoint::Graphics,
                    &self.pipeline_layout,
                    0,
                    &[input_descriptor_sets[lv + 1]],
                    &[],
                )
                .draw(3, 1, 0, 0)
                .inject(|r| inject_cmd_end_render_pass2(r, subsystem, &br::SubpassEndInfo::new()));

            stdev = unsafe { SafeF32::new_unchecked(stdev.value() * 2.0) };
        }

        rec
    }

    #[tracing::instrument(
        name = "BackdropEffectBlurProcessor::recreate_rt_resources",
        skip(self, base_system)
    )]
    pub fn recreate_rt_resources(
        &mut self,
        base_system: &mut AppBaseSystem<'subsystem>,
        rt_size: br::Extent2D,
        rt_format: br::Format,
    ) {
        self.clear_framebuffers();
        self.temporal_buffers.clear();
        unsafe {
            // release resources at first
            core::ptr::drop_in_place(&mut self.temporal_buffer_memory);
            core::ptr::write(
                &mut self.temporal_buffer_memory,
                Self::create_temporal_buffers(
                    base_system,
                    rt_size,
                    rt_format,
                    &mut self.temporal_buffers,
                ),
            );
        }

        let (downsample_pass_fbs, upsample_pass_fixed_fbs) = Self::create_framebuffers(
            base_system,
            &self.temporal_buffers,
            &self.render_pass,
            rt_size,
        );
        self.downsample_pass_fbs
            .extend(downsample_pass_fbs.into_iter().map(|x| x.unmanage().0));
        self.upsample_pass_fixed_fbs
            .extend(upsample_pass_fixed_fbs.into_iter().map(|x| x.unmanage().0));

        self.downsample_pipelines.clear();
        self.upsample_pipelines.clear();
        let (downsample_pipelines, upsample_pipelines) = Self::create_pipelines(
            base_system,
            rt_size,
            &self.pipeline_layout,
            &self.render_pass,
        );
        self.downsample_pipelines.extend(downsample_pipelines);
        self.upsample_pipelines.extend(upsample_pipelines);
    }
}

/// unbounded with gfx_device(must be externally managed)
pub struct UnboundedCompositionSurfaceAtlas {
    resource: br::vk::VkImage,
    resource_view: br::vk::VkImageView,
    memory: br::vk::VkDeviceMemory,
    residency_bitmap: Vec<u8>,
    format: br::Format,
    size: u32,
    region_manager: DynamicAtlasManager,
}
impl UnboundedCompositionSurfaceAtlas {
    pub unsafe fn drop_with_gfx_device(&mut self, gfx_device: &Subsystem) {
        unsafe {
            br::vkfn_wrapper::destroy_image_view(gfx_device.native_ptr(), self.resource_view, None);
            br::vkfn_wrapper::destroy_image(gfx_device.native_ptr(), self.resource, None);
            br::vkfn_wrapper::free_memory(gfx_device.native_ptr(), self.memory, None);
        }
    }

    // TODO: できればPhysical Deviceからとれる値をつかったほうがいい
    // 1024なら大抵は問題ないとは思うが...
    const GRANULARITY: u32 = 1024;

    pub fn new(subsystem: &Subsystem, size: u32, pixel_format: br::Format) -> Self {
        let bpp = match pixel_format {
            br::vk::VK_FORMAT_R8_UNORM => 1,
            _ => unimplemented!("bpp"),
        };

        let image = match br::ImageObject::new(
            subsystem,
            &br::ImageCreateInfo::new(br::Extent2D::spread1(size), pixel_format)
                .with_usage(
                    br::ImageUsageFlags::COLOR_ATTACHMENT
                        | br::ImageUsageFlags::SAMPLED
                        | br::ImageUsageFlags::TRANSFER_DEST,
                )
                .flags(br::ImageFlags::SPARSE_BINDING | br::ImageFlags::SPARSE_RESIDENCY),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create image");
                std::process::abort();
            }
        };
        let resource = match br::ImageViewBuilder::new(
            image,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()
        {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create image view");
                std::process::abort();
            }
        };

        assert!(size % Self::GRANULARITY == 0);
        let bitmap_div = size / Self::GRANULARITY;
        let mut residency_bitmap = vec![0; (bitmap_div * bitmap_div) as usize];
        tracing::debug!(
            size,
            granularity = Self::GRANULARITY,
            block_count = bitmap_div * bitmap_div,
            "ComositionSurfaceAtlas management parameters",
        );

        let image_memory_requirements = resource.image().sparse_requirements_alloc();
        for x in image_memory_requirements.iter() {
            tracing::debug!(?x, "image memory requirements");
        }

        let image_memory_requirements = resource.image().requirements();
        tracing::debug!(?image_memory_requirements, "image memory requirements");

        let memory_index = match subsystem
            .adapter_memory_info
            .find_device_local_index(image_memory_requirements.memoryTypeBits)
        {
            Some(x) => x,
            None => {
                tracing::error!(
                    memory_type_mask =
                        format!("0x{:08x}", image_memory_requirements.memoryTypeBits),
                    "No suitable memory for surface atlas"
                );
                std::process::abort();
            }
        };
        let memory = match br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(
                (Self::GRANULARITY * Self::GRANULARITY * bpp) as _,
                memory_index,
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(
                    size = Self::GRANULARITY * Self::GRANULARITY * bpp,
                    memory_index,
                    reason = ?e,
                    "Failed to allocate first memory block"
                );
                std::process::abort();
            }
        };

        if let Err(e) = unsafe {
            subsystem.bind_sparse_raw(
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
                            subresource: br::ImageSubresource::new(br::AspectMask::COLOR, 0, 0),
                            offset: br::Offset3D::ZERO,
                            extent: br::Extent2D::spread1(Self::GRANULARITY).with_depth(1),
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
        } {
            tracing::warn!(reason = ?e, "Failed to bind initial block");
        }
        residency_bitmap[0] = 0x01;

        let mut region_manager = DynamicAtlasManager::new();
        // free entire region
        region_manager.free(AtlasRect {
            left: 0,
            top: 0,
            right: Self::GRANULARITY,
            bottom: Self::GRANULARITY,
        });

        let (memory, _) = memory.unmanage();
        let (resource_view, resource) = resource.unmanage();
        let (resource, _, _, _, _) = resource.unmanage();

        Self {
            resource_view,
            resource,
            memory,
            residency_bitmap,
            size,
            format: pixel_format,
            region_manager,
        }
    }

    pub const fn resource_view_transparent_ref(&self) -> &br::VkHandleRef<br::vk::VkImageView> {
        br::VkHandleRef::from_raw_ref(&self.resource_view)
    }

    pub const fn image_transparent_ref(&self) -> &br::VkHandleRef<br::vk::VkImage> {
        br::VkHandleRef::from_raw_ref(&self.resource)
    }

    pub const fn size(&self) -> u32 {
        self.size
    }

    pub const fn format(&self) -> br::Format {
        self.format
    }

    pub const fn vk_extent(&self) -> br::Extent2D {
        br::Extent2D::spread1(self.size)
    }

    pub const fn uv_from_pixels(&self, pixels: f32) -> f32 {
        pixels / self.size as f32
    }

    #[tracing::instrument(skip(self), ret(level = tracing::Level::TRACE))]
    pub fn alloc(&mut self, required_width: u32, required_height: u32) -> AtlasRect {
        match self.region_manager.alloc(required_width, required_height) {
            Some(x) => x,
            None => {
                todo!("alloc new tile");
            }
        }
    }

    pub fn free(&mut self, rect: AtlasRect) {
        self.region_manager.free(rect);
    }
}

pub struct CompositionSurfaceAtlas<'d> {
    gfx_device: &'d Subsystem,
    raw: UnboundedCompositionSurfaceAtlas,
}
impl Drop for CompositionSurfaceAtlas<'_> {
    fn drop(&mut self) {
        unsafe {
            self.raw.drop_with_gfx_device(self.gfx_device);
        }
    }
}
impl<'d> CompositionSurfaceAtlas<'d> {
    #[tracing::instrument(skip(subsystem))]
    pub fn new(subsystem: &'d Subsystem, size: u32, pixel_format: br::vk::VkFormat) -> Self {
        Self {
            raw: UnboundedCompositionSurfaceAtlas::new(subsystem, size, pixel_format),
            gfx_device: subsystem,
        }
    }

    pub const fn unbound(self) -> UnboundedCompositionSurfaceAtlas {
        let raw = unsafe { core::ptr::read(&self.raw) };
        core::mem::forget(self);

        raw
    }
}
