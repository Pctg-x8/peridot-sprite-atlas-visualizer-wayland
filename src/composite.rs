//! UI Rect Compositioning

use std::collections::BTreeSet;

use bedrock::{self as br, Image, ImageChild, MemoryBound, VkHandle, VulkanStructure};

use crate::subsystem::Subsystem;

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
    const fn shader_mode_value(&self) -> f32 {
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
    buffer: br::BufferObject<&'d Subsystem>,
    memory: br::DeviceMemoryObject<&'d Subsystem>,
    buffer_stg: br::BufferObject<&'d Subsystem>,
    memory_stg: br::DeviceMemoryObject<&'d Subsystem>,
    stg_mem_requires_flush: bool,
    capacity: usize,
    count: usize,
    free: BTreeSet<usize>,
}
impl<'d> CompositeInstanceManager<'d> {
    const INIT_CAP: usize = 1024;

    pub fn new(subsystem: &'d Subsystem) -> Self {
        let mut buffer = br::BufferObject::new(
            subsystem,
            &br::BufferCreateInfo::new(
                core::mem::size_of::<CompositeInstanceData>() * Self::INIT_CAP,
                br::BufferUsage::VERTEX_BUFFER.transfer_dest(),
            ),
        )
        .expect("Failed to create composite instance buffer");
        let buffer_mreq = buffer.requirements();
        let memory_index = subsystem
            .adapter_memory_info
            .find_device_local_index(buffer_mreq.memoryTypeBits)
            .expect("no suitable memory");
        let memory = br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(buffer_mreq.size, memory_index),
        )
        .expect("Failed to allocate composite instance data memory");
        buffer
            .bind(&memory, 0)
            .expect("Failed to bind buffer memory");

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

    pub const fn buffer_stg(&self) -> &impl br::Buffer {
        &self.buffer_stg
    }

    pub const fn buffer(&self) -> &impl br::Buffer {
        &self.buffer
    }

    pub const fn count(&self) -> usize {
        self.count
    }

    pub const fn memory_stg(&self) -> &impl br::DeviceMemory {
        &self.memory_stg
    }

    pub const fn memory_stg_exc(&mut self) -> &mut impl br::DeviceMemoryMut {
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

pub struct CompositeTree {
    rects: Vec<CompositeRect>,
    unused: BTreeSet<usize>,
    dirty: bool,
}
impl CompositeTree {
    /// ルートノード
    pub const ROOT: CompositeTreeRef = CompositeTreeRef(0);

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

    pub fn alloc(&mut self) -> CompositeTreeRef {
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

            return CompositeTreeRef(x);
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
                                    (r.texatlas_rect.right as f32 - r.texatlas_rect.left as f32)
                                        / tex_size.width as f32,
                                    (r.texatlas_rect.bottom as f32 - r.texatlas_rect.top as f32)
                                        / tex_size.height as f32,
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
    used_left: u32,
    used_top: u32,
    current_line_top: u32,
}
impl<'d> CompositionSurfaceAtlas<'d> {
    // TODO: できればPhysical Deviceからとれる値をつかったほうがいい
    // 1024なら大抵は問題ないとは思うが...
    const GRANULARITY: u32 = 1024;

    pub fn new(subsystem: &'d Subsystem, size: u32, pixel_format: br::vk::VkFormat) -> Self {
        let bpp = match pixel_format {
            br::vk::VK_FORMAT_R8_UNORM => 1,
            _ => unimplemented!("bpp"),
        };

        let image = br::ImageObject::new(
            subsystem,
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
        let resource = br::ImageViewBuilder::new(
            image,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
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

        let memory_index = subsystem
            .adapter_memory_info
            .find_device_local_index(image_memory_requirements.memoryTypeBits)
            .expect("no suitable memory for surface atlas");
        let memory = br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(
                (Self::GRANULARITY * Self::GRANULARITY * bpp) as _,
                memory_index,
            ),
        )
        .expect("Failed to allocate first memory block");

        unsafe {
            subsystem
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

    pub const fn resource(&self) -> &(impl br::ImageView + br::ImageChild) {
        &self.resource
    }

    pub const fn size(&self) -> u32 {
        self.size
    }

    pub const fn uv_from_pixels(&self, pixels: f32) -> f32 {
        pixels / self.size as f32
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
