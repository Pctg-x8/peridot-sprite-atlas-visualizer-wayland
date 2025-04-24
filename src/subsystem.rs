use std::{io::Read, path::Path};

use bedrock::{self as br, Instance, PhysicalDevice};

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
}
impl Drop for Subsystem {
    fn drop(&mut self) {
        unsafe {
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
    pub fn init() -> Self {
        for x in br::instance_extension_properties(None).unwrap() {
            println!(
                "vkext {:?} version {}",
                x.extensionName.as_cstr().unwrap(),
                x.specVersion,
            );
        }

        let instance = br::InstanceObject::new(&br::InstanceCreateInfo::new(
            &br::ApplicationInfo::new(
                c"Peridot SpriteAtlas Visualizer",
                br::Version::new(0, 0, 1, 0),
                c"",
                br::Version::new(0, 0, 0, 0),
            )
            .api_version(br::Version::new(0, 1, 4, 0)),
            &[c"VK_LAYER_KHRONOS_validation".into()],
            &[c"VK_KHR_surface".into(), c"VK_KHR_wayland_surface".into()],
        ))
        .unwrap();
        let adapter = instance
            .iter_physical_devices()
            .expect("Failed to iterate physical devices")
            .next()
            .expect("no physical devices");
        let adapter_queue_info = adapter.queue_family_properties_alloc();
        for (n, q) in adapter_queue_info.iter().enumerate() {
            let mut v = Vec::with_capacity(4);
            if q.queue_flags().has(br::QueueFlags::GRAPHICS) {
                v.push("Graphics");
            }
            if q.queue_flags().has(br::QueueFlags::COMPUTE) {
                v.push("Compute");
            }
            if q.queue_flags().has(br::QueueFlags::TRANSFER) {
                v.push("Transfer");
            }
            if q.queue_flags().has(br::QueueFlags::SPARSE_BINDING) {
                v.push("Sparse Binding");
            }

            println!("Queue #{n}: x{} {}", q.queueCount, v.join(" / "));
        }
        let adapter_memory_info = adapter.memory_properties();
        for (n, p) in adapter_memory_info.types().iter().enumerate() {
            let h = &adapter_memory_info.heaps()[p.heapIndex as usize];

            let mut v = Vec::with_capacity(6);
            if p.property_flags()
                .has(br::MemoryPropertyFlags::DEVICE_LOCAL)
            {
                v.push("Device Local");
            }
            if p.property_flags()
                .has(br::MemoryPropertyFlags::HOST_VISIBLE)
            {
                v.push("Host Visible");
            }
            if p.property_flags()
                .has(br::MemoryPropertyFlags::HOST_COHERENT)
            {
                v.push("Host Coherent");
            }
            if p.property_flags().has(br::MemoryPropertyFlags::HOST_CACHED) {
                v.push("Host Cached");
            }
            if p.property_flags()
                .has(br::MemoryPropertyFlags::LAZILY_ALLOCATED)
            {
                v.push("Lazy Allocated");
            }
            if p.property_flags().has(br::MemoryPropertyFlags::PROTECTED) {
                v.push("Protected");
            }

            let mut hv = Vec::with_capacity(2);
            if h.flags().has(br::MemoryHeapFlags::DEVICE_LOCAL) {
                hv.push("Device Local");
            }
            if h.flags().has(br::MemoryHeapFlags::MULTI_INSTANCE) {
                hv.push("Multi Instance");
            }

            println!(
                "Memory Type #{n}: {} heap #{} ({}) size {}",
                v.join(" / "),
                p.heapIndex,
                hv.join(" / "),
                fmt_bytesize(h.size as _)
            );
        }
        let adapter_properties = adapter.properties();
        println!(
            "max texture2d size: {}",
            adapter_properties.limits.maxImageDimension2D
        );
        let adapter_sparse_image_format_props = adapter.sparse_image_format_properties_alloc(
            br::vk::VK_FORMAT_R8_UNORM,
            br::vk::VK_IMAGE_TYPE_2D,
            br::vk::VK_SAMPLE_COUNT_1_BIT,
            br::ImageUsageFlags::SAMPLED | br::ImageUsageFlags::COLOR_ATTACHMENT,
            br::vk::VK_IMAGE_TILING_OPTIMAL,
        );
        for x in adapter_sparse_image_format_props.iter() {
            println!(
                "sparse image format property: {:?} 0x{:04x} 0x{:04x}",
                x.imageGranularity, x.aspectMask, x.flags
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
                        <br::vk::VkPhysicalDeviceSynchronization2Features as br::VulkanStructure>::TYPE,
                    pNext: core::ptr::null_mut(),
                    synchronization2: 1,
                }),
            ),
        )
        .unwrap();

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
        }
    }

    pub const fn adapter(&self) -> &impl PhysicalDevice {
        unsafe { core::mem::transmute::<_, &SubsystemAdapterAccess>(self) }
    }

    #[inline]
    pub fn load_shader<'d>(
        &'d self,
        path: impl AsRef<Path>,
    ) -> br::Result<br::ShaderModuleObject<&'d Self>> {
        br::ShaderModuleObject::new(
            self,
            &br::ShaderModuleCreateInfo::new(&load_spv_file(path).unwrap()),
        )
    }

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

    pub fn queue_present(&self, present_info: &br::PresentInfo) -> br::Result<()> {
        unsafe { br::vkfn_wrapper::queue_present(self.graphics_queue, present_info).map(drop) }
    }

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
