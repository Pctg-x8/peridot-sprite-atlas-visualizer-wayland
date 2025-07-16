//! UI Rect Compositioning

use std::collections::{BTreeSet, HashMap};

use bedrock::{self as br, Image, ImageChild, MemoryBound, TypedVulkanStructure, VkHandle};

use crate::{AppEvent, AppEventBus, helper_types::SafeF32, mathext::Matrix4, subsystem::Subsystem};

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

    pub fn sync_buffer<'cb, E: 'cb>(&self, cr: br::CmdRecord<'cb, E>) -> br::CmdRecord<'cb, E> {
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
        if !self.clip_invalidated && self.active_clip_parameters.is_none() {
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

            if r.has_bitmap {
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

#[derive(Debug, Clone, Copy)]
pub struct AtlasRect {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}
impl AtlasRect {
    pub const fn width(&self) -> u32 {
        self.right.abs_diff(self.left)
    }

    pub const fn height(&self) -> u32 {
        self.bottom.abs_diff(self.top)
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

/// unbounded with gfx_device(must be externally managed)
pub struct UnboundedCompositionSurfaceAtlas {
    resource: br::vk::VkImage,
    resource_view: br::vk::VkImageView,
    memory: br::vk::VkDeviceMemory,
    residency_bitmap: Vec<u8>,
    format: br::Format,
    size: u32,
    used_left: u32,
    used_top: u32,
    current_line_top: u32,
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
                .as_color_attachment()
                .sampled()
                .transfer_dest()
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
            used_left: 0,
            used_top: 0,
            current_line_top: 0,
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
        if self.used_left + required_width > Self::GRANULARITY {
            tracing::trace!(
                left = Self::GRANULARITY - self.used_left,
                "wrapping occured"
            );

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

    pub fn free(&mut self, rect: AtlasRect) {
        tracing::warn!(?rect, "TODO: free atlas rect");
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
