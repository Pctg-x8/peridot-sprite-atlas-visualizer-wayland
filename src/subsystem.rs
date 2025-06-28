use std::{
    cell::{OnceCell, RefCell},
    collections::{HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
};

use bedrock::{
    self as br, Device, Instance, MemoryBound, PhysicalDevice, ResolverInterface, VkHandle,
};

#[derive(Debug, thiserror::Error)]
pub enum LoadShaderError {
    #[error(transparent)]
    Vk(#[from] br::vk::VkResult),
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

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

#[repr(transparent)]
pub struct SubsystemShaderModuleRef<'a>(br::VkHandleRef<'a, br::vk::VkShaderModule>);
impl br::VkHandle for SubsystemShaderModuleRef<'_> {
    type Handle = br::vk::VkShaderModule;

    #[inline]
    fn native_ptr(&self) -> Self::Handle {
        self.0.native_ptr()
    }
}
impl br::ShaderModule for SubsystemShaderModuleRef<'_> {}

pub struct Subsystem {
    instance: br::vk::VkInstance,
    adapter: br::vk::VkPhysicalDevice,
    device: br::vk::VkDevice,
    pub adapter_memory_info: br::MemoryProperties,
    pub adapter_properties: br::PhysicalDeviceProperties,
    pub graphics_queue_family_index: u32,
    graphics_queue: br::vk::VkQueue,
    pipeline_cache: br::vk::VkPipelineCache,
    empty_pipeline_layout: OnceCell<br::VkHandleRef<'static, br::vk::VkPipelineLayout>>,
    loaded_shader_modules: RefCell<HashMap<PathBuf, br::vk::VkShaderModule>>,
}
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

            for (_, v) in self.loaded_shader_modules.get_mut().drain() {
                br::vkfn::destroy_shader_module(self.device, v, core::ptr::null());
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
    #[tracing::instrument]
    pub fn init() -> Self {
        for x in br::instance_extension_properties(None).unwrap() {
            tracing::debug!(extension_name = ?x.extensionName.as_cstr(), version = x.specVersion, "vkext");
        }

        let instance = match br::InstanceObject::new(&br::InstanceCreateInfo::new(
            &br::ApplicationInfo::new(
                c"Peridot SpriteAtlas Visualizer/Editor",
                br::Version::new(0, 0, 1, 0),
                c"",
                br::Version::new(0, 0, 0, 0),
            )
            .api_version(br::Version::new(0, 1, 4, 0)),
            &[c"VK_LAYER_KHRONOS_validation".into()],
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
                    ..Default::default()
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
            empty_pipeline_layout: OnceCell::new(),
            loaded_shader_modules: RefCell::new(HashMap::new()),
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

    #[tracing::instrument(skip(self), fields(path = %path.as_ref().display()), err(Display))]
    pub fn load_shader<'d>(
        &'d self,
        path: impl AsRef<Path>,
    ) -> Result<SubsystemShaderModuleRef<'d>, LoadShaderError> {
        if let Some(&loaded) = self.loaded_shader_modules.borrow().get(path.as_ref()) {
            return Ok(SubsystemShaderModuleRef(unsafe {
                br::VkHandleRef::dangling(loaded)
            }));
        }

        tracing::info!("Loading fresh shader");
        let obj = br::ShaderModuleObject::new(
            self,
            &br::ShaderModuleCreateInfo::new(&load_spv_file(&path)?),
        )?
        .unmanage()
        .0;
        self.loaded_shader_modules
            .borrow_mut()
            .insert(path.as_ref().to_owned(), obj);
        Ok(SubsystemShaderModuleRef(unsafe {
            br::VkHandleRef::dangling(obj)
        }))
    }

    #[tracing::instrument(skip(self), fields(path = %path.as_ref().display()))]
    pub fn require_shader<'d>(&'d self, path: impl AsRef<Path>) -> SubsystemShaderModuleRef<'d> {
        match self.load_shader(path) {
            Ok(x) => x,
            Err(_) => panic!("could not load required shader"),
        }
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

pub struct StagingScratchBufferReservation {
    block_index: usize,
    offset: br::DeviceSize,
    size: br::DeviceSize,
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
    _marker: core::marker::PhantomData<&'r mut StagingScratchBuffer<'g>>,
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

pub struct StagingScratchBuffer<'g> {
    next_suitable_index: Option<usize>,
    gfx_device_ref: &'g Subsystem,
    buffer: br::vk::VkBuffer,
    memory: br::vk::VkDeviceMemory,
    requires_explicit_flush: bool,
    size: br::DeviceSize,
    top: br::DeviceSize,
}
unsafe impl Sync for StagingScratchBuffer<'_> {}
unsafe impl Send for StagingScratchBuffer<'_> {}
impl Drop for StagingScratchBuffer<'_> {
    fn drop(&mut self) {
        unsafe {
            br::vkfn_wrapper::free_memory(self.gfx_device_ref.native_ptr(), self.memory, None);
            br::vkfn_wrapper::destroy_buffer(self.gfx_device_ref.native_ptr(), self.buffer, None);
        }
    }
}
impl br::VkHandle for StagingScratchBuffer<'_> {
    type Handle = br::vk::VkBuffer;

    #[inline(always)]
    fn native_ptr(&self) -> Self::Handle {
        self.buffer
    }
}
impl<'g> StagingScratchBuffer<'g> {
    #[tracing::instrument(name = "StagingScratchBuffer::new", skip(gfx_device))]
    pub fn new(gfx_device: &'g Subsystem, size: br::DeviceSize) -> Self {
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
            next_suitable_index: None,
            gfx_device_ref: gfx_device,
            buffer: buf.unmanage().0,
            memory: mem.unmanage().0,
            requires_explicit_flush: !is_coherent,
            size,
            top: 0,
        }
    }

    pub const fn reminder(&self) -> br::DeviceSize {
        self.size - self.top
    }

    pub fn reserve(&mut self, size: br::DeviceSize) -> br::DeviceSize {
        let base = self.top;

        self.top = base + size;
        base
    }

    pub fn map<'s>(
        &'s self,
        mode: StagingScratchBufferMapMode,
        range: core::ops::Range<br::DeviceSize>,
    ) -> br::Result<MappedStagingScratchBuffer<'s, 'g>> {
        let ptr = unsafe {
            br::vkfn_wrapper::map_memory(
                self.gfx_device_ref.native_ptr(),
                self.memory,
                range.start,
                range.end - range.start,
                0,
            )?
        };
        if mode.is_read() && self.requires_explicit_flush {
            if let Err(e) = unsafe {
                self.gfx_device_ref
                    .invalidate_memory_range(&[br::MappedMemoryRange::new_raw(
                        self.memory,
                        range.start,
                        range.end - range.start,
                    )])
            } {
                tracing::warn!(reason = ?e, "Failed to invalidate mapped memory range");
            }
        }

        Ok(MappedStagingScratchBuffer {
            gfx_device_ref: self.gfx_device_ref,
            memory: self.memory,
            explicit_flush: self.requires_explicit_flush && mode.is_write(),
            ptr,
            range,
            _marker: core::marker::PhantomData,
        })
    }
}

// alloc-only buffer resource manager: using tlsf algorithm for best-fit buffer block finding.
pub struct StagingScratchBufferManager<'g> {
    buffer_blocks: Vec<StagingScratchBuffer<'g>>,
    first_level_free_residency_bit: u64,
    second_level_free_residency_bit: [u16; StagingScratchBufferManager::FIRST_LEVEL_MAX_COUNT],
    suitable_block_head_index: [usize;
        StagingScratchBufferManager::SECOND_LEVEL_ENTRY_COUNT
            * StagingScratchBufferManager::FIRST_LEVEL_MAX_COUNT],
    total_reserve_amount: br::DeviceSize,
}
impl<'g> StagingScratchBufferManager<'g> {
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

    #[tracing::instrument(name = "StagingScratchBufferManager::new", skip(gfx_device))]
    pub fn new(gfx_device: &'g Subsystem) -> Self {
        let mut this = Self {
            buffer_blocks: vec![StagingScratchBuffer::new(gfx_device, Self::BLOCK_SIZE)],
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

    pub fn reset(&mut self) {
        // TODO: ここbuffer blockの再利用はどうするかあとで考える
        self.buffer_blocks.shrink_to(1);
        self.buffer_blocks[0].next_suitable_index = None;
        self.first_level_free_residency_bit = 0;
        self.second_level_free_residency_bit.fill(0);
        self.total_reserve_amount = 0;

        let (f, s) = Self::level_indices(Self::BLOCK_SIZE);
        self.chain_free_block(f, s, 0);
    }

    // #[tracing::instrument(ret(level = tracing::Level::DEBUG))]
    fn level_indices(size: br::DeviceSize) -> (u8, u8) {
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

    pub fn of<'s>(
        &'s self,
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

    pub fn buffer_of<'s>(
        &'s self,
        index: usize,
    ) -> &'s (impl br::VkHandle<Handle = br::vk::VkBuffer> + use<'g>) {
        &self.buffer_blocks[index]
    }

    pub fn map<'s>(
        &'s mut self,
        reservation: &StagingScratchBufferReservation,
        mode: StagingScratchBufferMapMode,
    ) -> br::Result<MappedStagingScratchBuffer<'s, 'g>> {
        self.buffer_blocks[reservation.block_index].map(
            mode,
            reservation.offset..(reservation.offset + reservation.size),
        )
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

fn load_spv_file(path: impl AsRef<Path>) -> std::io::Result<Vec<u32>> {
    let mut f = std::fs::File::open(path)?;
    let byte_length = f.metadata()?.len();
    assert!((byte_length & 0x03) == 0);
    let mut words = vec![0u32; byte_length as usize >> 2];
    f.read_exact(unsafe {
        core::slice::from_raw_parts_mut(words.as_mut_ptr() as *mut u8, words.len() << 2)
    })?;

    Ok(words)
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
