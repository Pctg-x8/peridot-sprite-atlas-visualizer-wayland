//! UI Rect Compositioning

use std::collections::{BTreeSet, HashMap};

use bedrock::{self as br, Image, ImageChild, MemoryBound, TypedVulkanStructure, VkHandle};

use crate::{AppEvent, AppEventBus, mathext::Matrix4, subsystem::Subsystem};

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

pub enum AnimatableFloat {
    Value(f32),
    Animated(f32, AnimationData<f32>),
}
impl AnimatableFloat {
    pub fn evaluate(&self, current_sec: f32) -> f32 {
        match self {
            &Self::Value(x) => x,
            &Self::Animated(from_value, ref a) => {
                lerp(a.interpolate(current_sec), from_value, a.to_value)
            }
        }
    }

    fn process_on_complete(&mut self, current_sec: f32, q: &AppEventBus) {
        match self {
            &mut Self::Animated(_, ref mut a) if a.end_sec <= current_sec => {
                if let Some(e) = a.event_on_complete.take() {
                    q.push(e);
                }
            }
            _ => (),
        }
    }
}

pub enum AnimatableColor {
    Value([f32; 4]),
    Expression(Box<dyn Fn(&CompositeTreeParameterStore) -> [f32; 4]>),
    Animated([f32; 4], AnimationData<[f32; 4]>),
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
            &Self::Animated(from_value, ref a) => {
                lerp4(a.interpolate(current_sec), from_value, a.to_value)
            }
        }
    }

    fn process_on_complete(&mut self, current_sec: f32, q: &AppEventBus) {
        match self {
            &mut Self::Animated(_, ref mut a) if a.end_sec <= current_sec => {
                if let Some(e) = a.event_on_complete.take() {
                    q.push(e);
                }
            }
            _ => (),
        }
    }
}

pub struct AnimationData<T> {
    pub start_sec: f32,
    pub end_sec: f32,
    pub to_value: T,
    pub curve_p1: (f32, f32),
    pub curve_p2: (f32, f32),
    pub event_on_complete: Option<AppEvent>,
}
impl<T> AnimationData<T> {
    fn interpolate(&self, current_sec: f32) -> f32 {
        // out of limits
        if current_sec < self.start_sec {
            return 0.0;
        }
        if current_sec > self.end_sec {
            return 1.0;
        }

        let x = (current_sec - self.start_sec) / (self.end_sec - self.start_sec);

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
        let a = self.curve_p1.0 * 3.0 - self.curve_p2.0 * 3.0 + 1.0;
        let b = self.curve_p2.0 * 3.0 - self.curve_p1.0 * 6.0;
        let c = self.curve_p1.0 * 3.0;
        let d = -x;

        let t = if a == 0.0 {
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
                        let t2 = 2.0 * r.cbrt() * ((phi + core::f32::consts::TAU) / 3.0).cos()
                            - a1 / 3.0;
                        let t3 =
                            3.0 * r.cbrt() * ((phi + core::f32::consts::TAU * 2.0) / 3.0).cos()
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
        (self.curve_p1.1 * 3.0 - self.curve_p2.1 * 3.0 + 1.0) * t.powi(3)
            + (self.curve_p2.1 * 3.0 - self.curve_p1.1 * 6.0) * t.powi(2)
            + self.curve_p1.1 * 3.0 * t
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
    pub opacity: AnimatableFloat,
    pub pivot: [f32; 2],
    pub scale_x: AnimatableFloat,
    pub scale_y: AnimatableFloat,
    pub animation_data_left: Option<AnimationData<f32>>,
    pub animation_data_top: Option<AnimationData<f32>>,
    pub animation_data_width: Option<AnimationData<f32>>,
    pub animation_data_height: Option<AnimationData<f32>>,
    pub dirty: bool,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}
impl Default for CompositeRect {
    fn default() -> Self {
        Self {
            instance_slot_index: None,
            offset: [0.0, 0.0],
            size: [0.0, 0.0],
            relative_offset_adjustment: [0.0, 0.0],
            relative_size_adjustment: [0.0, 0.0],
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
            animation_data_left: None,
            animation_data_top: None,
            animation_data_width: None,
            animation_data_height: None,
            parent: None,
            children: Vec::new(),
        }
    }
}

pub struct CompositeInstanceManager<'d> {
    buffer: br::BufferObject<&'d Subsystem>,
    memory: br::DeviceMemoryObject<&'d Subsystem>,
    streaming_buffer: br::BufferObject<&'d Subsystem>,
    streaming_memory: br::DeviceMemoryObject<&'d Subsystem>,
    streaming_memory_requires_flush: bool,
    buffer_stg: br::BufferObject<&'d Subsystem>,
    memory_stg: br::DeviceMemoryObject<&'d Subsystem>,
    stg_mem_requires_flush: bool,
    capacity: usize,
    count: usize,
    free: BTreeSet<usize>,
}
impl<'d> CompositeInstanceManager<'d> {
    const INIT_CAP: usize = 1024;

    #[tracing::instrument(skip(subsystem))]
    pub fn new(subsystem: &'d Subsystem) -> Self {
        let mut buffer = br::BufferObject::new(
            subsystem,
            &br::BufferCreateInfo::new(
                core::mem::size_of::<CompositeInstanceData>() * Self::INIT_CAP,
                br::BufferUsage::STORAGE_BUFFER.transfer_dest(),
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
            &self.buffer_stg,
            &self.buffer,
            &[br::BufferCopy::mirror(
                0,
                (core::mem::size_of::<CompositeInstanceData>() * 1024) as _,
            )],
        )
    }

    pub const fn buffer_stg<'b>(&'b self) -> &'b (impl br::Buffer + use<'d>) {
        &self.buffer_stg
    }

    pub const fn buffer<'b>(&'b self) -> &'b (impl br::Buffer + use<'d>) {
        &self.buffer
    }

    pub const fn streaming_buffer<'b>(&'b self) -> &'b (impl br::Buffer + use<'d>) {
        &self.streaming_buffer
    }

    pub const fn streaming_memory_exc<'b>(
        &'b mut self,
    ) -> &'b mut (impl br::DeviceMemoryMut + use<'d>) {
        &mut self.streaming_memory
    }

    pub const fn streaming_memory_requires_flush(&self) -> bool {
        self.streaming_memory_requires_flush
    }

    pub const fn count(&self) -> usize {
        self.count
    }

    pub const fn memory_stg<'b>(&'b self) -> &'b (impl br::DeviceMemory + use<'d>) {
        &self.memory_stg
    }

    pub const fn memory_stg_exc<'b>(&'b mut self) -> &'b mut (impl br::DeviceMemoryMut + use<'d>) {
        &mut self.memory_stg
    }

    pub const fn memory_stg_requires_explicit_flush(&self) -> bool {
        self.stg_mem_requires_flush
    }

    pub const fn range_all(&self) -> core::ops::Range<usize> {
        0..core::mem::size_of::<CompositeInstanceData>() * self.count
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CompositeTreeRef(usize);

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CompositeTreeFloatParameterRef(usize);

pub struct CompositeTreeParameterStore {
    float_parameters: Vec<AnimatableFloat>,
    float_values: Vec<f32>,
    unused_float_parameters: BTreeSet<usize>,
}
impl CompositeTreeParameterStore {
    pub fn alloc_float(&mut self, init: AnimatableFloat) -> CompositeTreeFloatParameterRef {
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

    pub fn set_float(&mut self, r: CompositeTreeFloatParameterRef, a: AnimatableFloat) {
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

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct SafeF32(f32);
// SafeF32 never gets NaN
impl Eq for SafeF32 {}
impl Ord for SafeF32 {
    #[inline(always)]
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        unsafe { self.partial_cmp(other).unwrap_unchecked() }
    }
}
impl std::hash::Hash for SafeF32 {
    #[inline(always)]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_ne_bytes().hash(state)
    }
}
impl SafeF32 {
    pub const unsafe fn new_unchecked(v: f32) -> Self {
        Self(v)
    }

    pub const fn new(v: f32) -> Option<Self> {
        if v.is_nan() {
            None
        } else {
            Some(unsafe { Self::new_unchecked(v) })
        }
    }

    pub const fn value(&self) -> f32 {
        self.0
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
}
impl CompositeRenderingInstructionBuilder {
    fn new(screen_size: br::Extent2D) -> Self {
        Self {
            insts: Vec::new(),
            render_passes: Vec::new(),
            last_free_backdrop_buffer: 0,
            active_backdrop_blur_index_for_stdev: HashMap::new(),
            current_backdrop_overlap_rects: Vec::new(),
            backdrop_active: false,
            max_backdrop_buffer_count: 0,
            screen_rect: screen_size.into_rect(br::Offset2D::ZERO),
        }
    }

    fn into_data(mut self) -> CompositeRenderingData {
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
        mapped_ptr: &br::MappedMemory<'_, impl br::DeviceMemoryMut + ?Sized>,
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
            ),
        )) = processes.pop()
        {
            let r = &mut self.rects[r];
            r.dirty = false;
            let local_left = match r.animation_data_left {
                None => r.offset[0],
                Some(ref mut x) => {
                    let rate = x.interpolate(current_sec);
                    if rate >= 1.0 {
                        if let Some(e) = x.event_on_complete.take() {
                            event_bus.push(e);
                        }
                    }

                    r.offset[0] + (x.to_value - r.offset[0]) * rate
                }
            };
            let local_top = match r.animation_data_top {
                None => r.offset[1],
                Some(ref mut x) => {
                    let rate = x.interpolate(current_sec);
                    if rate >= 1.0 {
                        if let Some(e) = x.event_on_complete.take() {
                            event_bus.push(e);
                        }
                    }

                    r.offset[1] + (x.to_value - r.offset[1]) * rate
                }
            };
            let local_width = match r.animation_data_width {
                None => r.size[0],
                Some(ref mut x) => {
                    let rate = x.interpolate(current_sec);
                    if rate >= 1.0 {
                        if let Some(e) = x.event_on_complete.take() {
                            event_bus.push(e);
                        }
                    }

                    r.size[0] + (x.to_value - r.size[0]) * rate
                }
            };
            let local_height = match r.animation_data_height {
                None => r.size[1],
                Some(ref mut x) => {
                    let rate = x.interpolate(current_sec);
                    if rate >= 1.0 {
                        if let Some(e) = x.event_on_complete.take() {
                            event_bus.push(e);
                        }
                    }

                    r.size[1] + (x.to_value - r.size[1]) * rate
                }
            };

            let left = effective_base_left
                + (effective_width * r.relative_offset_adjustment[0])
                + local_left;
            let top = effective_base_top
                + (effective_height * r.relative_offset_adjustment[1])
                + local_top;
            let w = effective_width * r.relative_size_adjustment[0] + local_width;
            let h = effective_height * r.relative_size_adjustment[1] + local_height;
            let opacity = parent_opacity * r.opacity.evaluate(current_sec);
            let matrix = parent_matrix.mul_mat4(
                Matrix4::translate(
                    left - effective_base_left + r.pivot[0] * w,
                    top - effective_base_top + r.pivot[1] * h,
                )
                .mul_mat4(Matrix4::scale(
                    r.scale_x.evaluate(current_sec),
                    r.scale_y.evaluate(current_sec),
                ))
                .mul_mat4(Matrix4::translate(-r.pivot[0] * w, -r.pivot[1] * h)),
            );

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

            if let Some(_) = r.instance_slot_index {
                unsafe {
                    core::ptr::write(
                        mapped_ptr.get_mut(
                            core::mem::size_of::<CompositeInstanceData>() * instance_slot_index,
                        ),
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
                        let stdev = stdev.evaluate(current_sec);

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

                inst_builder.draw_instance(instance_slot_index, backdrop_buffer_index);
                instance_slot_index += 1;
            }

            processes.extend(
                r.children
                    .iter()
                    .rev()
                    .map(|&x| (x, (left, top, w, h, opacity, matrix.clone()))),
            );
        }

        // let update_time = update_timer.elapsed();
        // println!("instbuild({update_time:?}): {:?}", inst_builder.insts);

        inst_builder.into_data()
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

pub struct CompositionSurfaceAtlas<'d> {
    resource: br::ImageViewObject<br::ImageObject<&'d Subsystem>>,
    memory: br::DeviceMemoryObject<&'d Subsystem>,
    residency_bitmap: Vec<u8>,
    size: u32,
    format: br::vk::VkFormat,
    used_left: u32,
    used_top: u32,
    current_line_top: u32,
}
impl<'d> CompositionSurfaceAtlas<'d> {
    // TODO: できればPhysical Deviceからとれる値をつかったほうがいい
    // 1024なら大抵は問題ないとは思うが...
    const GRANULARITY: u32 = 1024;

    #[tracing::instrument(skip(subsystem))]
    pub fn new(subsystem: &'d Subsystem, size: u32, pixel_format: br::vk::VkFormat) -> Self {
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

        Self {
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

    pub const fn resource<'s>(&'s self) -> &'s (impl br::ImageView + br::ImageChild + use<'d>) {
        &self.resource
    }

    pub const fn size(&self) -> u32 {
        self.size
    }

    pub const fn format(&self) -> br::vk::VkFormat {
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
}
