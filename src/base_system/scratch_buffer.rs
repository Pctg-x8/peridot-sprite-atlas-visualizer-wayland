use bedrock::{self as br, Device, MemoryBound, VkHandle};

use crate::subsystem::Subsystem;

pub struct StagingScratchBufferReservation {
    block_index: usize,
    offset: br::DeviceSize,
    size: br::DeviceSize,
}

/// Potentially resource leak
pub struct UnsafeStagingScratchBufferRawBlock {
    buffer: br::vk::VkBuffer,
    memory: br::vk::VkDeviceMemory,
    requires_explicit_flush: bool,
    next_suitable_index: Option<usize>,
    size: br::DeviceSize,
    top: br::DeviceSize,
}
unsafe impl Sync for UnsafeStagingScratchBufferRawBlock {}
unsafe impl Send for UnsafeStagingScratchBufferRawBlock {}
impl br::VkHandle for UnsafeStagingScratchBufferRawBlock {
    type Handle = br::vk::VkBuffer;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.buffer
    }
}
impl UnsafeStagingScratchBufferRawBlock {
    unsafe fn drop_with_gfx_device(&mut self, gfx_device: &Subsystem) {
        unsafe {
            br::vkfn_wrapper::free_memory(gfx_device.native_ptr(), self.memory, None);
            br::vkfn_wrapper::destroy_buffer(gfx_device.native_ptr(), self.buffer, None);
        }
    }

    fn new(gfx_device: &Subsystem, size: br::DeviceSize) -> Self {
        let mut buf = match br::BufferObject::new(
            gfx_device,
            &br::BufferCreateInfo::new(size as _, br::BufferUsage::TRANSFER_SRC),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create buffer object");
                std::process::abort();
            }
        };
        let mreq = buf.requirements();
        let Some(memindex) = gfx_device.find_host_visible_memory_index(mreq.memoryTypeBits) else {
            tracing::error!("No suitable memory");
            std::process::abort();
        };
        let is_coherent = gfx_device.is_coherent_memory_type(memindex);
        let mem = match br::DeviceMemoryObject::new(
            gfx_device,
            &br::MemoryAllocateInfo::new(mreq.size, memindex),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to allocate memory");
                std::process::abort();
            }
        };
        if let Err(e) = buf.bind(&mem, 0) {
            tracing::error!(reason = ?e, "Failed to bind memory to buffer");
            std::process::abort();
        }

        Self {
            buffer: buf.unmanage().0,
            memory: mem.unmanage().0,
            requires_explicit_flush: !is_coherent,
            next_suitable_index: None,
            size,
            top: 0,
        }
    }

    pub const fn reminder(&self) -> br::DeviceSize {
        self.size - self.top
    }

    pub const fn reserve(&mut self, size: br::DeviceSize) -> br::DeviceSize {
        let base = self.top;

        self.top = base + size;
        base
    }

    /// Safety: must be used with compatible gfx_device
    pub unsafe fn map<'s, 'g>(
        &'s self,
        gfx_device: &'g Subsystem,
        mode: StagingScratchBufferMapMode,
        range: core::ops::Range<br::DeviceSize>,
    ) -> br::Result<MappedStagingScratchBuffer<'s, 'g>> {
        let ptr = unsafe {
            br::vkfn_wrapper::map_memory(
                gfx_device.native_ptr(),
                self.memory,
                range.start,
                range.end - range.start,
                0,
            )?
        };
        if mode.is_read() && self.requires_explicit_flush {
            if let Err(e) = unsafe {
                gfx_device.invalidate_memory_range(&[br::MappedMemoryRange::new_raw(
                    self.memory,
                    range.start,
                    range.end - range.start,
                )])
            } {
                tracing::warn!(reason = ?e, "Failed to invalidate mapped memory range");
            }
        }

        Ok(MappedStagingScratchBuffer {
            gfx_device_ref: gfx_device,
            memory: self.memory,
            explicit_flush: self.requires_explicit_flush && mode.is_write(),
            ptr,
            range,
            _marker: core::marker::PhantomData,
        })
    }
}

pub struct StagingScratchBufferBlock<'g> {
    gfx_device_ref: &'g Subsystem,
    raw: UnsafeStagingScratchBufferRawBlock,
}
impl Drop for StagingScratchBufferBlock<'_> {
    fn drop(&mut self) {
        unsafe {
            self.raw.drop_with_gfx_device(self.gfx_device_ref);
        }
    }
}
impl br::VkHandle for StagingScratchBufferBlock<'_> {
    type Handle = br::vk::VkBuffer;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.raw.native_ptr()
    }
}
impl<'g> StagingScratchBufferBlock<'g> {
    #[tracing::instrument(name = "StagingScratchBuffer::new", skip(gfx_device))]
    pub fn new(gfx_device: &'g Subsystem, size: br::DeviceSize) -> Self {
        Self {
            gfx_device_ref: gfx_device,
            raw: UnsafeStagingScratchBufferRawBlock::new(gfx_device, size),
        }
    }

    #[inline(always)]
    pub const fn reminder(&self) -> br::DeviceSize {
        self.raw.reminder()
    }

    #[inline(always)]
    pub const fn reserve(&mut self, size: br::DeviceSize) -> br::DeviceSize {
        self.raw.reserve(size)
    }

    #[inline(always)]
    pub fn map<'s>(
        &'s self,
        mode: StagingScratchBufferMapMode,
        range: core::ops::Range<br::DeviceSize>,
    ) -> br::Result<MappedStagingScratchBuffer<'s, 'g>> {
        unsafe { self.raw.map(self.gfx_device_ref, mode, range) }
    }
}

/// alloc-only buffer resource manager: using tlsf algorithm for best-fit buffer block finding.
///
/// Safety: must be used with compatible gfx_device
pub struct UnsafeStagingScratchBufferRaw {
    buffer_blocks: Vec<UnsafeStagingScratchBufferRawBlock>,
    first_level_free_residency_bit: u64,
    second_level_free_residency_bit: [u16; UnsafeStagingScratchBufferRaw::FIRST_LEVEL_MAX_COUNT],
    suitable_block_head_index: [usize;
        UnsafeStagingScratchBufferRaw::SECOND_LEVEL_ENTRY_COUNT
            * UnsafeStagingScratchBufferRaw::FIRST_LEVEL_MAX_COUNT],
    total_reserve_amount: br::DeviceSize,
}
impl UnsafeStagingScratchBufferRaw {
    pub unsafe fn drop_with_gfx_device(&mut self, gfx_device: &Subsystem) {
        for b in &mut self.buffer_blocks {
            unsafe {
                b.drop_with_gfx_device(gfx_device);
            }
        }
    }

    const BLOCK_SIZE: br::DeviceSize = 4 * 1024 * 1024;
    const MIN_ALLOC_GRANULARITY: br::DeviceSize = 64;
    const SECOND_LEVEL_BIT_COUNT: u32 = 4;

    const LOWER_BIT_COUNT: u32 = (Self::MIN_ALLOC_GRANULARITY - 1).trailing_ones();
    const SECOND_LEVEL_ENTRY_COUNT: usize = (1 << Self::SECOND_LEVEL_BIT_COUNT) as _;
    const FIRST_LEVEL_BIT_COUNT: u32 = core::mem::size_of::<br::DeviceSize>() as u32 * 8
        - Self::SECOND_LEVEL_BIT_COUNT
        - Self::LOWER_BIT_COUNT;
    const FIRST_LEVEL_MAX_COUNT: usize = ((Self::BLOCK_SIZE - 1)
        >> (Self::LOWER_BIT_COUNT + Self::SECOND_LEVEL_BIT_COUNT))
        .trailing_ones() as usize
        + 2;

    pub fn new(gfx_device: &Subsystem) -> Self {
        let mut this = Self {
            buffer_blocks: vec![UnsafeStagingScratchBufferRawBlock::new(
                gfx_device,
                Self::BLOCK_SIZE,
            )],
            first_level_free_residency_bit: 0,
            second_level_free_residency_bit: [0; Self::FIRST_LEVEL_MAX_COUNT],
            suitable_block_head_index: [0; Self::SECOND_LEVEL_ENTRY_COUNT
                * Self::FIRST_LEVEL_MAX_COUNT],
            total_reserve_amount: 0,
        };

        let (f, s) = Self::level_indices(Self::BLOCK_SIZE);
        this.chain_free_block(f, s, 0);
        this
    }

    pub const fn total_reserved_amount(&self) -> br::DeviceSize {
        self.total_reserve_amount
    }

    fn reset(&mut self) {
        // TODO: ここbuffer blockの再利用はどうするかあとで考える
        self.buffer_blocks.shrink_to(1);
        self.buffer_blocks[0].next_suitable_index = None;
        self.first_level_free_residency_bit = 0;
        self.second_level_free_residency_bit.fill(0);
        self.total_reserve_amount = 0;

        let (f, s) = Self::level_indices(Self::BLOCK_SIZE);
        self.chain_free_block(f, s, 0);
    }

    const fn level_indices(size: br::DeviceSize) -> (u8, u8) {
        const fn const_min_u32(a: u32, b: u32) -> u32 {
            if a < b { a } else { b }
        }

        let f = Self::FIRST_LEVEL_BIT_COUNT
            - const_min_u32(size.leading_zeros(), Self::FIRST_LEVEL_BIT_COUNT);
        let s = if f == 0 {
            // minimum sizes
            size >> Self::LOWER_BIT_COUNT
        } else {
            (size >> (Self::LOWER_BIT_COUNT + f - 1)) - (1 << Self::SECOND_LEVEL_BIT_COUNT)
        };

        (f as u8, s as u8)
    }

    const fn level_to_index(fli: u8, sli: u8) -> usize {
        fli as usize * Self::SECOND_LEVEL_ENTRY_COUNT + sli as usize
    }

    fn evict_level(&mut self, fli: u8, sli: u8) {
        let new_residency_bits = self.second_level_free_residency_bit[fli as usize] & !(1 << sli);
        self.second_level_free_residency_bit[fli as usize] = new_residency_bits;
        if new_residency_bits == 0 {
            // second levelが全部埋まった
            self.first_level_free_residency_bit &= !(1 << fli);
        }
    }

    fn resident_level(&mut self, fli: u8, sli: u8, value: usize) {
        self.second_level_free_residency_bit[fli as usize] |= 1 << sli;
        // かならずできるはず
        self.first_level_free_residency_bit |= 1 << fli;

        self.suitable_block_head_index[Self::level_to_index(fli, sli)] = value;
    }

    const fn residential_value(&self, fli: u8, sli: u8) -> Option<usize> {
        if (self.second_level_free_residency_bit[fli as usize] & (1 << sli)) != 0 {
            Some(self.suitable_block_head_index[Self::level_to_index(fli, sli)])
        } else {
            None
        }
    }

    const fn least_second_level_resident_index(&self, fli: u8, lowest_sli: u8) -> Option<u8> {
        let p = (self.second_level_free_residency_bit[fli as usize] & (u16::MAX << lowest_sli))
            .trailing_zeros() as usize;

        if p < Self::SECOND_LEVEL_ENTRY_COUNT {
            Some(p as _)
        } else {
            None
        }
    }

    const fn least_first_level_resident_index(&self, lowest_fli: u8) -> Option<u8> {
        let p = (self.first_level_free_residency_bit & (u64::MAX << lowest_fli)).trailing_zeros()
            as usize;

        if p < Self::FIRST_LEVEL_MAX_COUNT {
            Some(p as _)
        } else {
            None
        }
    }

    fn find_fit_free_block_index(&self, fli: &mut u8, sli: &mut u8) -> Option<usize> {
        if let Some(resident_sli) = self.least_second_level_resident_index(*fli, *sli) {
            // found
            *sli = resident_sli;

            return Some(self.suitable_block_head_index[Self::level_to_index(*fli, *sli)]);
        }

        if let Some(resident_fli) = self.least_first_level_resident_index(*fli) {
            // found in larger first level
            *fli = resident_fli;
            *sli = self
                .least_second_level_resident_index(*fli, 0)
                .expect("resident fli found but no sli?");

            return Some(self.suitable_block_head_index[Self::level_to_index(*fli, *sli)]);
        }

        None
    }

    fn unchain_free_block(&mut self, fli: u8, sli: u8, index: usize) {
        self.evict_level(fli, sli);
        if let Some(next) = self.buffer_blocks[index].next_suitable_index {
            // set next free block as resident of this level
            self.resident_level(fli, sli, next);
        }
    }

    fn chain_free_block(&mut self, fli: u8, sli: u8, index: usize) {
        if let Some(current) = self.residential_value(fli, sli) {
            // chain head of existing list
            self.buffer_blocks[index].next_suitable_index = Some(current);
        }

        self.resident_level(fli, sli, index);
    }

    pub fn reserve(&mut self, size: br::DeviceSize) -> StagingScratchBufferReservation {
        // roundup
        let size = (size + (Self::MIN_ALLOC_GRANULARITY - 1)) & !(Self::MIN_ALLOC_GRANULARITY - 1);
        let (mut fli, mut sli) = Self::level_indices(size);

        let Some(block_index) = self.find_fit_free_block_index(&mut fli, &mut sli) else {
            todo!("out of memory. allocate new one");
        };
        self.unchain_free_block(fli, sli, block_index);

        let offset = self.buffer_blocks[block_index].reserve(size);
        if self.buffer_blocks[block_index].reminder() > 0 {
            // のこりは再登録
            let (new_f, new_s) = Self::level_indices(self.buffer_blocks[block_index].reminder());
            self.chain_free_block(new_f, new_s, block_index);
        }

        self.total_reserve_amount += size;

        StagingScratchBufferReservation {
            block_index,
            offset,
            size,
        }
    }

    pub fn of<'s, 'g>(
        &'s self,
        _gfx_device: &'g Subsystem, // phantom
        reservation: &StagingScratchBufferReservation,
    ) -> (
        &'s (impl br::VkHandle<Handle = br::vk::VkBuffer> + use<'g>),
        br::DeviceSize,
    ) {
        (
            &self.buffer_blocks[reservation.block_index],
            reservation.offset,
        )
    }

    pub fn of_index(
        &self,
        reservation: &StagingScratchBufferReservation,
    ) -> (usize, br::DeviceSize) {
        (reservation.block_index, reservation.offset)
    }

    pub fn buffer_of<'s, 'g>(
        &'s self,
        _gfx_device: &'g Subsystem, // phantom
        index: usize,
    ) -> &'s (impl br::VkHandle<Handle = br::vk::VkBuffer> + use<'g>) {
        &self.buffer_blocks[index]
    }

    pub unsafe fn map<'s, 'g>(
        &'s mut self,
        gfx_device: &'g Subsystem,
        reservation: &StagingScratchBufferReservation,
        mode: StagingScratchBufferMapMode,
    ) -> br::Result<MappedStagingScratchBuffer<'s, 'g>> {
        unsafe {
            self.buffer_blocks[reservation.block_index].map(
                gfx_device,
                mode,
                reservation.offset..(reservation.offset + reservation.size),
            )
        }
    }
}

pub struct FlippableStagingScratchBufferGroup<'subsystem> {
    buffers: Vec<StagingScratchBuffer<'subsystem>>,
    active_index: usize,
}
impl<'subsystem> FlippableStagingScratchBufferGroup<'subsystem> {
    pub fn new(subsystem: &'subsystem Subsystem, count: usize) -> Self {
        Self {
            buffers: core::iter::repeat_with(|| StagingScratchBuffer::new(subsystem))
                .take(count)
                .collect(),
            active_index: 0,
        }
    }

    pub fn flip_next_and_ready(&mut self) {
        self.active_index = (self.active_index + 1) % self.buffers.len();
        self.buffers[self.active_index].reset();
    }

    pub fn active_buffer<'s>(&'s self) -> &'s StagingScratchBuffer<'subsystem> {
        &self.buffers[self.active_index]
    }

    pub fn active_buffer_mut<'s>(&'s mut self) -> &'s mut StagingScratchBuffer<'subsystem> {
        &mut self.buffers[self.active_index]
    }
}

// alloc-only buffer resource manager: using tlsf algorithm for best-fit buffer block finding.
pub struct StagingScratchBuffer<'g> {
    gfx_device_ref: &'g Subsystem,
    raw: UnsafeStagingScratchBufferRaw,
}
impl Drop for StagingScratchBuffer<'_> {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            self.raw.drop_with_gfx_device(self.gfx_device_ref);
        }
    }
}
impl<'g> StagingScratchBuffer<'g> {
    #[tracing::instrument(name = "StagingScratchBuffer::new", skip(gfx_device))]
    pub fn new(gfx_device: &'g Subsystem) -> Self {
        Self {
            raw: UnsafeStagingScratchBufferRaw::new(gfx_device),
            gfx_device_ref: gfx_device,
        }
    }

    #[inline(always)]
    pub const fn total_reserved_amount(&self) -> br::DeviceSize {
        self.raw.total_reserved_amount()
    }

    #[inline(always)]
    fn reset(&mut self) {
        self.raw.reset();
    }

    #[inline(always)]
    pub fn reserve(&mut self, size: br::DeviceSize) -> StagingScratchBufferReservation {
        self.raw.reserve(size)
    }

    #[inline(always)]
    pub fn of<'s>(
        &'s self,
        reservation: &StagingScratchBufferReservation,
    ) -> (
        &'s (impl br::VkHandle<Handle = br::vk::VkBuffer> + use<'g>),
        br::DeviceSize,
    ) {
        self.raw.of(self.gfx_device_ref, reservation)
    }

    #[inline(always)]
    pub fn of_index(
        &self,
        reservation: &StagingScratchBufferReservation,
    ) -> (usize, br::DeviceSize) {
        self.raw.of_index(reservation)
    }

    #[inline(always)]
    pub fn buffer_of<'s>(
        &'s self,
        index: usize,
    ) -> &'s (impl br::VkHandle<Handle = br::vk::VkBuffer> + use<'g>) {
        self.raw.buffer_of(self.gfx_device_ref, index)
    }

    #[inline(always)]
    pub fn map<'s>(
        &'s mut self,
        reservation: &StagingScratchBufferReservation,
        mode: StagingScratchBufferMapMode,
    ) -> br::Result<MappedStagingScratchBuffer<'s, 'g>> {
        unsafe { self.raw.map(self.gfx_device_ref, reservation, mode) }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StagingScratchBufferMapMode {
    Read,
    Write,
    ReadWrite,
}
impl StagingScratchBufferMapMode {
    const fn is_read(&self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    const fn is_write(&self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

pub struct MappedStagingScratchBuffer<'r, 'g> {
    gfx_device_ref: &'g Subsystem,
    memory: br::vk::VkDeviceMemory,
    range: core::ops::Range<br::DeviceSize>,
    explicit_flush: bool,
    ptr: *mut core::ffi::c_void,
    _marker: core::marker::PhantomData<&'r mut StagingScratchBufferBlock<'g>>,
}
impl Drop for MappedStagingScratchBuffer<'_, '_> {
    fn drop(&mut self) {
        if self.explicit_flush {
            if let Err(e) = unsafe {
                self.gfx_device_ref
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                        self.memory,
                        self.range.start,
                        self.range.end - self.range.start,
                    )])
            } {
                tracing::warn!(reason = ?e, "Failed to flush mapped memory ranges");
            }
        }

        unsafe {
            br::vkfn_wrapper::unmap_memory(self.gfx_device_ref.native_ptr(), self.memory);
        }
    }
}
impl MappedStagingScratchBuffer<'_, '_> {
    pub const unsafe fn addr_of_mut<T>(&self, offset: usize) -> *mut T {
        unsafe { self.ptr.byte_add(offset).cast() }
    }
}
