use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

use bedrock::{self as br, Instance, PhysicalDevice, ResolverInterface, VkHandle};
use freetype::FreeType;
use parking_lot::RwLock;

#[repr(transparent)]
pub struct SubsystemInstanceAccess(Subsystem);
impl br::VkHandle for SubsystemInstanceAccess {
    type Handle = br::vk::VkInstance;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.0.instance
    }
}
impl br::Instance for SubsystemInstanceAccess {}
impl br::InstanceExtensions for SubsystemInstanceAccess {
    fn set_debug_utils_object_name_ext_fn(&self) -> bedrock::vk::PFN_vkSetDebugUtilsObjectNameEXT {
        unsafe { self.native_ptr().load_function_unconstrainted() }
    }

    unsafe fn new_debug_utils_messenger_raw(
        &self,
        _info: &bedrock::DebugUtilsMessengerCreateInfo,
        _allocation_callbacks: Option<&bedrock::vk::VkAllocationCallbacks>,
    ) -> bedrock::Result<bedrock::vk::VkDebugUtilsMessengerEXT> {
        unimplemented!();
    }

    unsafe fn destroy_debug_utils_messenger_raw(
        &self,
        _obj: bedrock::vk::VkDebugUtilsMessengerEXT,
        _allocation_callbacks: Option<&bedrock::vk::VkAllocationCallbacks>,
    ) {
        unimplemented!();
    }

    fn create_debug_utils_messenger_ext_fn(
        &self,
    ) -> bedrock::vk::PFN_vkCreateDebugUtilsMessengerEXT {
        unimplemented!();
    }

    fn destroy_debug_utils_messenger_ext_fn(
        &self,
    ) -> bedrock::vk::PFN_vkDestroyDebugUtilsMessengerEXT {
        unimplemented!();
    }

    fn get_physical_device_external_fence_properties_khr_fn(
        &self,
    ) -> bedrock::vk::PFN_vkGetPhysicalDeviceExternalFencePropertiesKHR {
        unimplemented!();
    }
}

#[repr(transparent)]
struct SubsystemAdapterAccess(Subsystem);
impl br::VkHandle for SubsystemAdapterAccess {
    type Handle = br::vk::VkPhysicalDevice;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.0.adapter
    }
}
impl br::InstanceChild for SubsystemAdapterAccess {
    type ConcreteInstance = SubsystemInstanceAccess;

    #[inline(always)]
    fn instance(&self) -> &Self::ConcreteInstance {
        self.0.instance()
    }
}
impl br::PhysicalDevice for SubsystemAdapterAccess {}

pub struct Subsystem {
    instance: br::vk::VkInstance,
    adapter: br::vk::VkPhysicalDevice,
    device: br::vk::VkDevice,
    pub adapter_memory_info: br::MemoryProperties,
    pub adapter_properties: br::PhysicalDeviceProperties,
    pub graphics_queue_family_index: u32,
    graphics_queue: br::vk::VkQueue,
    pipeline_cache: br::vk::VkPipelineCache,
    empty_pipeline_layout: OnceLock<br::VkHandleRef<'static, br::vk::VkPipelineLayout>>,
    pub ft: RwLock<FreeType>,
}
unsafe impl Sync for Subsystem {}
unsafe impl Send for Subsystem {}
impl Drop for Subsystem {
    fn drop(&mut self) {
        unsafe {
            match br::vkfn_wrapper::get_pipeline_cache_data_byte_length(
                self.device,
                self.pipeline_cache,
            ) {
                Ok(dl) => {
                    let mut sink = Vec::with_capacity(dl);
                    sink.set_len(dl);
                    match br::vkfn_wrapper::get_pipeline_cache_data(
                        self.device,
                        self.pipeline_cache,
                        &mut sink,
                    ) {
                        Ok(_) => match std::fs::write(".vk-pipeline-cache", &sink) {
                            Ok(_) => (),
                            Err(e) => {
                                eprintln!("persist pipeline cache failed: {e:?}");
                            }
                        },
                        Err(e) => {
                            eprintln!("get pipeline cache data failed: {e:?}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("get pipeline cache data length failed: {e:?}");
                }
            }

            if let Some(x) = self.empty_pipeline_layout.take() {
                br::vkfn::destroy_pipeline_layout(self.device, x.native_ptr(), core::ptr::null());
            }
            br::vkfn::destroy_pipeline_cache(self.device, self.pipeline_cache, core::ptr::null());
            br::vkfn::destroy_device(self.device, core::ptr::null());
            br::vkfn::destroy_instance(self.instance, core::ptr::null());
        }
    }
}
impl br::VkHandle for Subsystem {
    type Handle = br::vk::VkDevice;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.device
    }
}
impl br::InstanceChild for Subsystem {
    type ConcreteInstance = SubsystemInstanceAccess;

    #[inline(always)]
    fn instance(&self) -> &Self::ConcreteInstance {
        unsafe { core::mem::transmute(self) }
    }
}
impl br::Device for Subsystem {}
impl Subsystem {
    #[tracing::instrument(name = "Subsystem::init")]
    pub fn init() -> Self {
        let mut instance_layers =
            Vec::with_capacity(br::instance_layer_property_count().unwrap() as _);
        unsafe {
            instance_layers.set_len(instance_layers.capacity());
        }
        br::instance_layer_properties(&mut instance_layers).unwrap();
        for x in instance_layers.iter() {
            tracing::debug!(layer_name = ?x.layerName.as_cstr(), "vklayer");
        }
        let validation_layer_found = instance_layers
            .iter()
            .find(|x| x.layerName.as_cstr().unwrap() == c"VK_LAYER_KHRONOS_validation")
            .is_some();
        for x in br::instance_extension_properties(None).unwrap() {
            tracing::debug!(extension_name = ?x.extensionName.as_cstr(), version = x.specVersion, "vkext");
        }

        let mut instance_layers = Vec::new();
        if validation_layer_found {
            instance_layers.push(c"VK_LAYER_KHRONOS_validation".into());
        }

        let instance = match br::InstanceObject::new(&br::InstanceCreateInfo::new(
            &br::ApplicationInfo::new(
                c"Peridot SpriteAtlas Visualizer/Editor",
                br::Version::new(0, 0, 1, 0),
                c"",
                br::Version::new(0, 0, 0, 0),
            )
            .api_version(br::Version::new(0, 1, 4, 0)),
            &instance_layers,
            &[
                c"VK_KHR_surface".into(),
                #[cfg(feature = "platform-linux-wayland")]
                c"VK_KHR_wayland_surface".into(),
                #[cfg(feature = "platform-windows")]
                c"VK_KHR_win32_surface".into(),
                c"VK_EXT_debug_utils".into(),
            ],
        )) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create vk instance");
                std::process::abort();
            }
        };
        let adapter = match instance.iter_physical_devices() {
            Ok(mut xs) => match xs.next() {
                Some(x) => x,
                None => {
                    tracing::error!("No physical devices");
                    std::process::abort();
                }
            },
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to enumerate physical devices");
                std::process::abort();
            }
        };
        let adapter_queue_info = adapter.queue_family_properties_alloc();
        for (n, q) in adapter_queue_info.iter().enumerate() {
            let mut v = Vec::with_capacity(4);
            if q.queue_flags().has_any(br::QueueFlags::GRAPHICS) {
                v.push("Graphics");
            }
            if q.queue_flags().has_any(br::QueueFlags::COMPUTE) {
                v.push("Compute");
            }
            if q.queue_flags().has_any(br::QueueFlags::TRANSFER) {
                v.push("Transfer");
            }
            if q.queue_flags().has_any(br::QueueFlags::SPARSE_BINDING) {
                v.push("Sparse Binding");
            }

            tracing::debug!(index = n, count = q.queueCount, flags = ?v, "Queue");
        }
        let adapter_memory_info = adapter.memory_properties();
        for (n, p) in adapter_memory_info.types().iter().enumerate() {
            let h = &adapter_memory_info.heaps()[p.heapIndex as usize];

            let mut v = Vec::with_capacity(6);
            if p.property_flags()
                .has_any(br::MemoryPropertyFlags::DEVICE_LOCAL)
            {
                v.push("Device Local");
            }
            if p.property_flags()
                .has_any(br::MemoryPropertyFlags::HOST_VISIBLE)
            {
                v.push("Host Visible");
            }
            if p.property_flags()
                .has_any(br::MemoryPropertyFlags::HOST_COHERENT)
            {
                v.push("Host Coherent");
            }
            if p.property_flags()
                .has_any(br::MemoryPropertyFlags::HOST_CACHED)
            {
                v.push("Host Cached");
            }
            if p.property_flags()
                .has_any(br::MemoryPropertyFlags::LAZILY_ALLOCATED)
            {
                v.push("Lazy Allocated");
            }
            if p.property_flags()
                .has_any(br::MemoryPropertyFlags::PROTECTED)
            {
                v.push("Protected");
            }

            let mut hv = Vec::with_capacity(2);
            if h.flags().has_any(br::MemoryHeapFlags::DEVICE_LOCAL) {
                hv.push("Device Local");
            }
            if h.flags().has_any(br::MemoryHeapFlags::MULTI_INSTANCE) {
                hv.push("Multi Instance");
            }

            tracing::debug!(
                index = n,
                flags = ?v,
                heap.index = p.heapIndex,
                heap.flags = ?hv,
                heap.size = fmt_bytesize(h.size as _),
                "Memory Type",
            );
        }
        let adapter_properties = adapter.properties();
        let r8_image_format_properties = match adapter.image_format_properties(
            br::vk::VK_FORMAT_R8_UNORM,
            br::vk::VK_IMAGE_VIEW_TYPE_2D,
            br::vk::VK_IMAGE_TILING_OPTIMAL,
            br::ImageUsageFlags::COLOR_ATTACHMENT,
            br::ImageFlags::EMPTY,
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(reason = ?e, "Failed to get image format properties for VK_FORMAT_R8_UNORM");
                br::vk::VkImageFormatProperties {
                    maxExtent: br::Extent3D::spread1(0),
                    maxMipLevels: 0,
                    maxArrayLayers: 0,
                    sampleCounts: 0,
                    maxResourceSize: 0,
                }
            }
        };
        tracing::debug!(
            max_texture2d_size = adapter_properties.limits.maxImageDimension2D,
            r8_image_format_sample_count = r8_image_format_properties.sampleCounts,
            "adapter properties",
        );
        let adapter_sparse_image_format_props = adapter.sparse_image_format_properties_alloc(
            br::vk::VK_FORMAT_R8_UNORM,
            br::vk::VK_IMAGE_TYPE_2D,
            br::vk::VK_SAMPLE_COUNT_1_BIT,
            br::ImageUsageFlags::SAMPLED | br::ImageUsageFlags::COLOR_ATTACHMENT,
            br::vk::VK_IMAGE_TILING_OPTIMAL,
        );
        for x in adapter_sparse_image_format_props.iter() {
            tracing::debug!(
                image_granularity = ?x.imageGranularity,
                aspect_mask = format!("0x{:04x}", x.aspectMask),
                flags = format!("0x{:04x}", x.flags),
                "sparse image format property",
            );
        }
        let graphics_queue_family_index = adapter_queue_info
            .find_matching_index(br::QueueFlags::GRAPHICS)
            .unwrap();
        let device = br::DeviceObject::new(
            &adapter,
            &br::DeviceCreateInfo::new(
                &[br::DeviceQueueCreateInfo::new(
                    graphics_queue_family_index,
                    &[1.0],
                )],
                &[],
                &[c"VK_KHR_swapchain".into()],
            )
            .with_next(
                &br::PhysicalDeviceFeatures2::new(br::vk::VkPhysicalDeviceFeatures {
                    sparseBinding: true as _,
                    sparseResidencyImage2D: true as _,
                    ..unsafe { core::mem::MaybeUninit::zeroed().assume_init() }
                })
                .with_next(&mut br::vk::VkPhysicalDeviceSynchronization2Features {
                    sType:
                        <br::vk::VkPhysicalDeviceSynchronization2Features as br::TypedVulkanStructure>::TYPE,
                    pNext: core::ptr::null_mut(),
                    synchronization2: 1,
                }),
            ),
        )
        .unwrap();

        let pipeline_cache_path = std::path::Path::new(".vk-pipeline-cache");
        let pipeline_cache = if pipeline_cache_path.try_exists().is_ok_and(|x| x) {
            // try load from persistent
            match std::fs::read(&pipeline_cache_path) {
                Ok(blob) => {
                    tracing::info!("Recovering previous pipeline cache from file");
                    match br::PipelineCacheObject::new(
                        &device,
                        &br::PipelineCacheCreateInfo::new(&blob),
                    ) {
                        Ok(x) => x,
                        Err(e) => {
                            tracing::error!(reason = ?e, "Failed to create pipeline cachec object");
                            std::process::abort();
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(reason = ?e, "Failed to load pipeline cache");
                    match br::PipelineCacheObject::new(
                        &device,
                        &br::PipelineCacheCreateInfo::new(&[]),
                    ) {
                        Ok(x) => x,
                        Err(e) => {
                            tracing::error!(reason = ?e, "Failed to create pipeline cachec object");
                            std::process::abort();
                        }
                    }
                }
            }
        } else {
            tracing::info!("No previous pipeline cache file found");
            match br::PipelineCacheObject::new(&device, &br::PipelineCacheCreateInfo::new(&[])) {
                Ok(x) => x,
                Err(e) => {
                    tracing::error!(reason = ?e, "Failed to create pipeline cachec object");
                    std::process::abort();
                }
            }
        };

        let mut ft = match FreeType::new() {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to initialize FreeType");
                std::process::exit(1);
            }
        };
        let hinting = unsafe { ft.get_property::<u32>(c"cff", c"hinting-engine").unwrap() };
        let no_stem_darkening = unsafe {
            ft.get_property::<freetype::Bool>(c"cff", c"no-stem-darkening")
                .unwrap()
        };
        tracing::debug!(hinting, no_stem_darkening, "freetype cff properties");
        unsafe {
            ft.set_property(c"cff", c"no-stem-darkening", &(true as freetype::Bool))
                .unwrap();
        }

        let (pipeline_cache, _) = pipeline_cache.unmanage();
        let (device, _) = device.unmanage();
        let (adapter, _) = adapter.unmanage();
        let instance = instance.unmanage();

        Self {
            graphics_queue: unsafe {
                br::vkfn_wrapper::get_device_queue(device, graphics_queue_family_index, 0)
            },
            graphics_queue_family_index,
            device,
            adapter,
            instance,
            adapter_memory_info,
            adapter_properties,
            pipeline_cache,
            empty_pipeline_layout: OnceLock::new(),
            ft: RwLock::new(ft),
        }
    }

    pub const fn adapter(&self) -> &impl PhysicalDevice {
        unsafe { core::mem::transmute::<_, &SubsystemAdapterAccess>(self) }
    }

    #[tracing::instrument(skip(self), ret(level = tracing::Level::TRACE))]
    pub fn find_device_local_memory_index(&self, mask: u32) -> Option<u32> {
        self.adapter_memory_info.find_device_local_index(mask)
    }

    #[tracing::instrument(skip(self), ret(level = tracing::Level::TRACE))]
    pub fn find_host_visible_memory_index(&self, mask: u32) -> Option<u32> {
        self.adapter_memory_info.find_host_visible_index(mask)
    }

    #[tracing::instrument(skip(self), ret(level = tracing::Level::TRACE))]
    pub fn find_direct_memory_index(&self, mask: u32) -> Option<u32> {
        self.adapter_memory_info
            .types()
            .iter()
            .enumerate()
            .find_map(|(n, p)| {
                ((mask & (1 << n)) != 0
                    && p.property_flags().has_all(
                        br::MemoryPropertyFlags::DEVICE_LOCAL
                            | br::MemoryPropertyFlags::HOST_VISIBLE,
                    ))
                .then_some(n as _)
            })
    }

    #[inline]
    pub fn is_coherent_memory_type(&self, index: u32) -> bool {
        self.adapter_memory_info.is_coherent(index)
    }

    #[tracing::instrument(skip(self))]
    pub fn require_empty_pipeline_layout<'d>(
        &'d self,
    ) -> &'d impl br::VkHandle<Handle = br::vk::VkPipelineLayout> {
        self.empty_pipeline_layout.get_or_init(|| {
            match br::PipelineLayoutObject::new(self, &br::PipelineLayoutCreateInfo::new(&[], &[]))
            {
                Ok(x) => unsafe { br::VkHandleRef::dangling(x.unmanage().0) },
                Err(e) => {
                    tracing::error!(reason = ?e, "Failed to create required empty pipeline layout");
                    std::process::abort();
                }
            }
        })
    }

    #[tracing::instrument(skip(self, create_info_array), err(Display))]
    pub fn create_graphics_pipelines(
        &self,
        create_info_array: &[br::GraphicsPipelineCreateInfo],
    ) -> br::Result<Vec<br::PipelineObject<&Self>>> {
        br::Device::new_graphics_pipelines(
            self,
            create_info_array,
            Some(&unsafe { br::VkHandleRef::dangling(self.pipeline_cache) }),
        )
    }

    #[tracing::instrument(skip(self, create_info_array), err(Display))]
    pub fn create_graphics_pipelines_array<const N: usize>(
        &self,
        create_info_array: &[br::GraphicsPipelineCreateInfo; N],
    ) -> br::Result<[br::PipelineObject<&Self>; N]> {
        br::Device::new_graphics_pipeline_array(
            self,
            create_info_array,
            Some(&unsafe { br::VkHandleRef::dangling(self.pipeline_cache) }),
        )
    }

    #[tracing::instrument(skip(self), err(Display))]
    pub fn create_transient_graphics_command_pool(
        &self,
    ) -> br::Result<br::CommandPoolObject<&Self>> {
        br::CommandPoolObject::new(
            self,
            &br::CommandPoolCreateInfo::new(self.graphics_queue_family_index).transient(),
        )
    }

    #[tracing::instrument(skip(self, buffers), err(Display))]
    pub fn sync_execute_graphics_commands(
        &self,
        buffers: &[br::CommandBufferSubmitInfo],
    ) -> br::Result<()> {
        unsafe {
            br::vkfn_wrapper::queue_submit2(
                br::VkHandleRefMut::dangling(self.graphics_queue),
                &[br::SubmitInfo2::new(&[], buffers, &[])],
                None,
            )?;
            br::vkfn_wrapper::queue_wait_idle(self.graphics_queue)?;
        }

        Ok(())
    }

    #[tracing::instrument(skip(self, works, fence), err(Display))]
    pub fn submit_graphics_works(
        &self,
        works: &[br::SubmitInfo2],
        fence: Option<br::VkHandleRefMut<br::vk::VkFence>>,
    ) -> br::Result<()> {
        unsafe {
            br::vkfn_wrapper::queue_submit2(
                br::VkHandleRefMut::dangling(self.graphics_queue),
                works,
                fence,
            )
        }
    }

    #[tracing::instrument(skip(self, present_info), err(Display))]
    pub fn queue_present(&self, present_info: &br::PresentInfo) -> br::Result<()> {
        unsafe { br::vkfn_wrapper::queue_present(self.graphics_queue, present_info).map(drop) }
    }

    #[tracing::instrument(skip(self, infos, fence), err(Display))]
    pub unsafe fn bind_sparse_raw(
        &self,
        infos: &[br::vk::VkBindSparseInfo],
        fence: Option<br::VkHandleRefMut<br::vk::VkFence>>,
    ) -> br::Result<()> {
        unsafe {
            br::vkfn_wrapper::queue_bind_sparse(
                br::VkHandleRefMut::dangling(self.graphics_queue),
                infos,
                fence,
            )
        }
    }
}

// simple tlsf allocator: first-level 16bits second-level 8bits
struct DeviceLocalScratchBufferManager {
    max_block_size: br::DeviceSize,
    allowed_size_mask: br::DeviceSize,
    top_force_zero_bit_count: u32,
    effective_bit_count: u32,
    first_level_freeblock_residency_bit: u16,
    second_level_freeblock_residency_bit: [u8; 16],
    freeblock_offsets: [br::DeviceSize; 16 * 8],
    addr_to_block_size: HashMap<br::DeviceSize, br::DeviceSize>,
    addr_to_prev_block: HashMap<br::DeviceSize, br::DeviceSize>,
    addr_to_next_free_block: HashMap<br::DeviceSize, br::DeviceSize>,
    used_addrs: HashSet<br::DeviceSize>,
}
impl DeviceLocalScratchBufferManager {
    pub fn new(max_block_size: br::DeviceSize) -> Self {
        let mut second_level_freeblock_residency_bit = [0; 16];
        second_level_freeblock_residency_bit[15] = 1 << 7;

        // 4mb = 4 * 1024 * 1024 -> 2^22
        // 8bits/8bits = 8bits + 3bits = 11bits left 11bits(2048)
        // 11bits -> 2^11 = 2048
        // 16bits/8bits = 16bits + 3bits = 19bits left 3bits(8)

        Self {
            max_block_size,
            allowed_size_mask: max_block_size - 1,
            top_force_zero_bit_count: (max_block_size - 1).leading_zeros(),
            effective_bit_count: (max_block_size - 1).trailing_ones(),
            first_level_freeblock_residency_bit: 1 << 15,
            second_level_freeblock_residency_bit,
            freeblock_offsets: [0; 16 * 8],
            addr_to_block_size: HashMap::new(),
            addr_to_prev_block: HashMap::new(),
            addr_to_next_free_block: HashMap::new(),
            used_addrs: HashSet::new(),
        }
    }

    fn level_indices(&self, size: br::DeviceSize) -> (u8, u8) {
        let lower = (self.effective_bit_count - 16 - 3) as u8;

        let f = (15
            - ((size & self.allowed_size_mask).leading_zeros() - self.top_force_zero_bit_count)
                .min(15)) as u8;
        let s = ((size >> (lower + f)) & 0x07) as u8;

        (f, s)
    }

    fn evict_level_freeblock(&mut self, fli: u8, sli: u8) {
        let new_freeblock_residency_bits =
            self.second_level_freeblock_residency_bit[fli as usize] & !(1 << sli);
        self.second_level_freeblock_residency_bit[fli as usize] = new_freeblock_residency_bits;
        if new_freeblock_residency_bits == 0 {
            // second levelが全部埋まった
            self.first_level_freeblock_residency_bit &= !(1 << fli);
        }
    }

    fn resident_level_freeblock(&mut self, fli: u8, sli: u8, addr: br::DeviceSize) {
        self.second_level_freeblock_residency_bit[fli as usize] |= 1 << sli;
        // かならずできるはず
        self.first_level_freeblock_residency_bit |= 1 << fli;

        self.freeblock_offsets[(fli * 8 + sli) as usize] = addr;
    }

    fn residential_free_block_addr(&self, fli: u8, sli: u8) -> Option<br::DeviceSize> {
        if (self.second_level_freeblock_residency_bit[fli as usize] & (1 << sli)) != 0 {
            Some(self.freeblock_offsets[(fli * 8 + sli) as usize])
        } else {
            None
        }
    }

    fn find_fit_free_block_head(&self, fli: &mut u8, sli: &mut u8) -> Option<br::DeviceSize> {
        let residency_bit_pos = (self.second_level_freeblock_residency_bit[*fli as usize]
            & (0xff << *sli))
            .trailing_zeros()
            + 1;
        if residency_bit_pos < 8 {
            // found
            *sli = residency_bit_pos as _;

            return Some(self.freeblock_offsets[(*fli * 8 + *sli) as usize]);
        }

        let residency_bit_pos =
            (self.first_level_freeblock_residency_bit & (0xffff << *fli)).trailing_zeros() + 1;
        if residency_bit_pos < 16 {
            // found in first level
            *fli = residency_bit_pos as _;
            let second_level_least_residency =
                self.second_level_freeblock_residency_bit[*fli as usize].trailing_zeros() + 1;
            assert!(second_level_least_residency < 8);
            *sli = second_level_least_residency as _;

            return Some(self.freeblock_offsets[(*fli * 8 + *sli) as usize]);
        }

        None
    }

    fn unchain_free_block(&mut self, addr: br::DeviceSize) {
        let (f, s) = self.level_indices(self.addr_to_block_size[&addr]);

        self.evict_level_freeblock(f, s);
        if let Some(a) = self.addr_to_next_free_block.remove(&addr) {
            // set next free block as resident of this level
            self.resident_level_freeblock(f, s, a);
        }

        self.used_addrs.insert(addr);
    }

    fn chain_free_block(&mut self, addr: br::DeviceSize) {
        let (f, s) = self.level_indices(self.addr_to_block_size[&addr]);

        if let Some(a) = self.residential_free_block_addr(f, s) {
            // chain head of existing list
            self.addr_to_next_free_block.insert(addr, a);
        }

        self.resident_level_freeblock(f, s, addr);
        self.used_addrs.remove(&addr);
    }

    pub fn alloc(&mut self, size: br::DeviceSize) -> br::DeviceSize {
        let (mut fli, mut sli) = self.level_indices(size);

        let head_addr = self
            .find_fit_free_block_head(&mut fli, &mut sli)
            .expect("out of memory");
        self.unchain_free_block(head_addr);

        if self.addr_to_block_size[&head_addr] > size {
            // 必要分より大きいので分割
            let new_free_block_size = self.addr_to_block_size[&head_addr] - size;
            let new_free_block_addr = head_addr + size;
            self.addr_to_block_size.insert(head_addr, size);
            self.addr_to_block_size
                .insert(new_free_block_addr, new_free_block_size);

            self.chain_free_block(new_free_block_addr);
        }

        self.used_addrs.insert(head_addr);
        return head_addr;
    }

    pub fn free(&mut self, addr: br::DeviceSize) {
        let mut size = self.addr_to_block_size[&addr];

        if !self.used_addrs.contains(&(addr + size)) {
            // merge with next unused block
            self.unchain_free_block(addr + size);
            let next_block_size = unsafe {
                self.addr_to_block_size
                    .remove(&(addr + size))
                    .unwrap_unchecked()
            };

            size += next_block_size;
            self.addr_to_block_size.insert(addr, size);
        }

        self.chain_free_block(addr);
    }
}

fn fmt_bytesize(x: usize) -> String {
    if x < 1000 {
        return format!("{x}bytes");
    }

    let (mut suffix, mut x) = ("KB", x as f64 / 1024.0);

    if x >= 1000.0 {
        suffix = "MB";
        x /= 1024.0;
    }

    if x >= 1000.0 {
        suffix = "GB";
        x /= 1024.0;
    }

    if x >= 1000.0 {
        suffix = "TB";
        x /= 1024.0;
    }

    format!("{x:.3} {suffix}")
}
