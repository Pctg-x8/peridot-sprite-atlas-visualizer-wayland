mod wl;

use std::{io::Read, path::Path, rc::Rc};

use bedrock::{
    self as br, CommandBufferMut, CommandPoolMut, DescriptorPoolMut, Device, DeviceMemoryMut,
    Fence, FenceMut, ImageSubresourceSlice, Instance, MemoryBound, PhysicalDevice, QueueMut,
    RenderPass, Swapchain, VkHandle, VkHandleMut, VulkanStructure,
};

pub enum AppEvent {
    ToplevelWindowConfigure { width: u32, height: u32 },
    ToplevelWindowSurfaceConfigure { serial: u32 },
    ToplevelWindowClose,
    ToplevelWindowFrameTiming,
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

pub struct SpriteListPaneView<'d> {
    pub frame_image:
        br::ImageViewObject<br::ImageObject<&'d br::DeviceObject<&'d br::InstanceObject>>>,
    frame_image_mem: br::DeviceMemoryObject<&'d br::DeviceObject<&'d br::InstanceObject>>,
}
impl<'d> SpriteListPaneView<'d> {
    const CORNER_RADIUS: f32 = 24.0;

    pub fn new(
        device: &'d br::DeviceObject<&'d br::InstanceObject>,
        memory_types: &[br::vk::VkMemoryType],
        graphics_queue_family_index: u32,
        graphics_queue: &mut impl br::QueueMut,
        bitmap_scale: u32,
    ) -> Self {
        let render_size_px = ((Self::CORNER_RADIUS * 2.0 + 1.0) * bitmap_scale as f32) as u32;
        let mut frame_image = br::ImageObject::new(
            device,
            &br::ImageCreateInfo::new(
                br::vk::VkExtent2D {
                    width: render_size_px,
                    height: render_size_px,
                },
                br::vk::VK_FORMAT_R8_UNORM,
            )
            .sampled()
            .as_color_attachment(),
        )
        .unwrap();

        let frame_image_memreq = frame_image.requirements();
        let frame_image_mem = br::DeviceMemoryObject::new(
            device,
            &br::MemoryAllocateInfo::new(
                frame_image_memreq.size,
                memory_types
                    .iter()
                    .enumerate()
                    .find(|(n, t)| {
                        (frame_image_memreq.memoryTypeBits & (1 << n)) != 0
                            && (t.propertyFlags & br::vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) != 0
                    })
                    .unwrap()
                    .0 as _,
            ),
        )
        .unwrap();
        frame_image.bind(&frame_image_mem, 0).unwrap();

        let frame_image = frame_image
            .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
            .view_builder()
            .create()
            .unwrap();

        let render_pass = br::RenderPassObject::new(
            device,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(br::vk::VK_FORMAT_R8_UNORM)
                    .layout_transition(
                        br::ImageLayout::Undefined,
                        br::ImageLayout::ShaderReadOnlyOpt,
                    )
                    .color_memory_op(br::LoadOp::Clear, br::StoreOp::Store)],
                &[
                    br::SubpassDescription2::new().colors(&[br::AttachmentReference2::color(
                        0,
                        br::ImageLayout::ColorAttachmentOpt,
                    )]),
                ],
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
            ),
        )
        .unwrap();
        let framebuffer = br::FramebufferObject::new(
            device,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[frame_image.as_transparent_ref()],
                render_size_px,
                render_size_px,
            ),
        )
        .unwrap();

        let vsh = br::ShaderModuleObject::new(
            device,
            &br::ShaderModuleCreateInfo::new(&load_spv_file("resources/filltri.vert").unwrap()),
        )
        .unwrap();
        let fsh = br::ShaderModuleObject::new(
            device,
            &br::ShaderModuleCreateInfo::new(
                &load_spv_file("resources/rounded_rect.frag").unwrap(),
            ),
        )
        .unwrap();

        let pipeline_layout =
            br::PipelineLayoutObject::new(device, &br::PipelineLayoutCreateInfo::new(&[], &[]))
                .unwrap();
        let [pipeline] = device
            .new_graphics_pipeline_array(
                &[br::GraphicsPipelineCreateInfo::new(
                    &pipeline_layout,
                    render_pass.subpass(0),
                    &[
                        br::PipelineShaderStage::new(br::ShaderStage::Vertex, &vsh, c"main"),
                        br::PipelineShaderStage::new(br::ShaderStage::Fragment, &fsh, c"main"),
                    ],
                    &br::PipelineVertexInputStateCreateInfo::new(&[], &[]),
                    &br::PipelineInputAssemblyStateCreateInfo::new(
                        br::PrimitiveTopology::TriangleList,
                    ),
                    &br::PipelineViewportStateCreateInfo::new(
                        &[br::vk::VkViewport {
                            x: 0.0,
                            y: 0.0,
                            width: render_size_px as _,
                            height: render_size_px as _,
                            minDepth: 0.0,
                            maxDepth: 1.0,
                        }],
                        &[br::vk::VkRect2D {
                            offset: br::vk::VkOffset2D::ZERO,
                            extent: br::vk::VkExtent2D {
                                width: render_size_px,
                                height: render_size_px,
                            },
                        }],
                    ),
                    &br::PipelineRasterizationStateCreateInfo::new(
                        br::PolygonMode::Fill,
                        br::CullModeFlags::NONE,
                        br::FrontFace::CounterClockwise,
                    ),
                    &br::PipelineColorBlendStateCreateInfo::new(&[
                        br::vk::VkPipelineColorBlendAttachmentState::NOBLEND,
                    ]),
                )
                .multisample_state(&br::PipelineMultisampleStateCreateInfo::new())],
                None::<&br::PipelineCacheObject<&'d br::DeviceObject<&'d br::InstanceObject>>>,
            )
            .unwrap();

        let mut cp = br::CommandPoolObject::new(
            device,
            &br::CommandPoolCreateInfo::new(graphics_queue_family_index).transient(),
        )
        .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            device,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe { cb.begin(device).unwrap() }
            .begin_render_pass_2(
                &br::RenderPassBeginInfo::new(
                    &render_pass,
                    &framebuffer,
                    br::vk::VkRect2D {
                        offset: br::vk::VkOffset2D::ZERO,
                        extent: br::vk::VkExtent2D {
                            width: render_size_px,
                            height: render_size_px,
                        },
                    },
                    &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 0.0])],
                ),
                &br::vk::VkSubpassBeginInfo::new(br::vk::VK_SUBPASS_CONTENTS_INLINE),
            )
            .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
            .draw(3, 1, 0, 0)
            .end_render_pass_2(&br::vk::VkSubpassEndInfo::new())
            .end()
            .unwrap();

        graphics_queue
            .submit2(
                &[br::SubmitInfo2::new(
                    &[],
                    &[br::CommandBufferSubmitInfo::new(&cb)],
                    &[],
                )],
                None,
            )
            .unwrap();
        graphics_queue.wait().unwrap();

        Self {
            frame_image,
            frame_image_mem,
        }
    }
}

#[repr(C)]
pub struct CompositeInstanceData {
    pub pos_st: [f32; 4],
    pub uv_st: [f32; 4],
    pub slice_borders: [f32; 4],
    pub tex_size_pixels_composite_mode: [f32; 4],
    pub color_tint: [f32; 4],
}

fn main() {
    let (app_event_sender, app_event_receiver) = std::sync::mpsc::channel();

    let mut dp = wl::Display::connect().expect("Failed to connect to wayland display");
    let mut registry = dp.get_registry().expect("Failed to get global registry");
    struct RegistryListener {
        compositor: Option<wl::Owned<wl::Compositor>>,
        xdg_wm_base: Option<wl::Owned<wl::XdgWmBase>>,
    }
    impl wl::RegistryListener for RegistryListener {
        fn global(
            &mut self,
            registry: &mut wl::Registry,
            name: u32,
            interface: &core::ffi::CStr,
            version: u32,
        ) {
            println!("wl global: {name} {interface:?} {version}");

            if interface == c"wl_compositor" {
                self.compositor = Some(
                    registry
                        .bind(name, version)
                        .expect("Failed to bind compositor interface"),
                );
            }

            if interface == c"xdg_wm_base" {
                self.xdg_wm_base = Some(
                    registry
                        .bind(name, version)
                        .expect("Failed to bind xdg wm base interface"),
                );
            }
        }

        fn global_remove(&mut self, _registry: &mut wl::Registry, name: u32) {
            println!("wl global remove: {name}");
        }
    }
    let mut rl = RegistryListener {
        compositor: None,
        xdg_wm_base: None,
    };
    registry
        .add_listener(&mut rl)
        .expect("Failed to register listener");
    dp.roundtrip().expect("Failed to roundtrip events");

    let mut compositor = rl.compositor.expect("no wl_compositor");
    let mut xdg_wm_base = rl.xdg_wm_base.expect("no xdg_wm_base");
    let mut wl_surface = compositor
        .create_surface()
        .expect("Failed to create wl_surface");
    let mut xdg_surface = xdg_wm_base
        .get_xdg_surface(&mut wl_surface)
        .expect("Failed to get xdg surface");
    let mut xdg_toplevel = xdg_surface
        .get_toplevel()
        .expect("Failed to get xdg toplevel");
    xdg_toplevel
        .set_app_id(c"io.ct2.peridot.tools.sprite_atlas")
        .expect("Failed to set app id");
    xdg_toplevel
        .set_title(c"Peridot SpriteAtlas Visualizer/Editor")
        .expect("Failed to set title");

    struct ToplevelSurfaceEventsHandler {
        app_event_sender: std::sync::mpsc::Sender<AppEvent>,
    }
    impl wl::XdgSurfaceEventListener for ToplevelSurfaceEventsHandler {
        fn configure(&mut self, _: &mut wl::XdgSurface, serial: u32) {
            self.app_event_sender
                .send(AppEvent::ToplevelWindowSurfaceConfigure { serial })
                .unwrap();
        }
    }
    struct ToplevelWindowEventsHandler {
        app_event_sender: std::sync::mpsc::Sender<AppEvent>,
    }
    impl wl::XdgToplevelEventListener for ToplevelWindowEventsHandler {
        fn configure(&mut self, _: &mut wl::XdgToplevel, width: i32, height: i32, states: &[i32]) {
            self.app_event_sender
                .send(AppEvent::ToplevelWindowConfigure {
                    width: width as _,
                    height: height as _,
                })
                .unwrap();

            println!(
                "configure: {width} {height} {states:?} th: {:?}",
                std::thread::current().id()
            );
        }

        fn close(&mut self, _: &mut wl::XdgToplevel) {
            self.app_event_sender
                .send(AppEvent::ToplevelWindowClose)
                .unwrap();
        }

        fn configure_bounds(&mut self, toplevel: &mut wl::XdgToplevel, width: i32, height: i32) {
            println!(
                "configure bounds: {width} {height} th: {:?}",
                std::thread::current().id()
            );
        }

        fn wm_capabilities(&mut self, toplevel: &mut wl::XdgToplevel, capabilities: &[i32]) {
            println!(
                "wm capabilities: {capabilities:?} th: {:?}",
                std::thread::current().id()
            );
        }
    }
    let mut tseh = ToplevelSurfaceEventsHandler {
        app_event_sender: app_event_sender.clone(),
    };
    let mut tweh = ToplevelWindowEventsHandler {
        app_event_sender: app_event_sender.clone(),
    };
    xdg_surface
        .add_listener(&mut tseh)
        .expect("Failed to register toplevel surface event");
    xdg_toplevel
        .add_listener(&mut tweh)
        .expect("Failed to register toplevel window event");

    struct SurfaceEvents {
        optimal_buffer_scale: u32,
    }
    impl wl::SurfaceEventListener for SurfaceEvents {
        fn enter(&mut self, surface: &mut wl::Surface, output: &mut wl::Output) {
            println!("enter output");
        }

        fn leave(&mut self, surface: &mut wl::Surface, output: &mut wl::Output) {
            println!("leave output");
        }

        fn preferred_buffer_scale(&mut self, surface: &mut wl::Surface, factor: i32) {
            println!("preferred buffer scale: {factor}");
            self.optimal_buffer_scale = factor as _;
        }

        fn preferred_buffer_transform(&mut self, surface: &mut wl::Surface, transform: u32) {
            println!("preferred buffer transform: {transform}");
        }
    }
    let mut surface_events = SurfaceEvents {
        optimal_buffer_scale: 1,
    };
    wl_surface.add_listener(&mut surface_events).unwrap();

    wl_surface.commit().expect("Failed to commit surface");
    dp.roundtrip().expect("Failed to sync");

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
    let adapter_memory_info = adapter.memory_properties();
    let adapter_properties = adapter.properties();
    println!(
        "max texture2d size: {}",
        adapter_properties.limits.maxImageDimension2D
    );
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
            &br::PhysicalDeviceFeatures2::new(br::vk::VkPhysicalDeviceFeatures::default())
                .with_next(&mut br::vk::VkPhysicalDeviceSynchronization2Features {
                    sType:
                        <br::vk::VkPhysicalDeviceSynchronization2Features as VulkanStructure>::TYPE,
                    pNext: core::ptr::null_mut(),
                    synchronization2: 1,
                }),
        ),
    )
    .unwrap();
    let mut graphics_queue = device.queue(graphics_queue_family_index, 0);

    let surface = unsafe {
        br::SurfaceObject::new(
            &adapter,
            &br::vk::VkWaylandSurfaceCreateInfoKHR::new(dp.as_raw() as _, wl_surface.as_raw() as _),
        )
        .unwrap()
    };
    let surface_caps = adapter.surface_capabilities(&surface).unwrap();
    let surface_formats = adapter.surface_formats_alloc(&surface).unwrap();
    let sc_transform = if surface_caps
        .supported_transforms()
        .has(br::SurfaceTransformFlags::IDENTITY)
    {
        br::SurfaceTransformFlags::IDENTITY.bits()
    } else {
        surface_caps.currentTransform
    };
    let sc_composite_alpha = if surface_caps
        .supported_composite_alpha()
        .has(br::CompositeAlphaFlags::OPAQUE)
    {
        br::CompositeAlphaFlags::OPAQUE.bits()
    } else {
        br::CompositeAlphaFlags::INHERIT.bits()
    };
    let sc_format = surface_formats
        .iter()
        .find(|x| {
            x.format == br::vk::VK_FORMAT_R8G8B8A8_UNORM
                && x.colorSpace == br::vk::VK_COLOR_SPACE_SRGB_NONLINEAR_KHR
        })
        .unwrap()
        .clone();
    let mut sc_size = br::vk::VkExtent2D {
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
    let mut sc = Rc::new(
        br::SwapchainBuilder::new(
            &surface,
            2,
            sc_format.clone(),
            sc_size,
            br::ImageUsageFlags::COLOR_ATTACHMENT,
        )
        .pre_transform(sc_transform)
        .composite_alpha(sc_composite_alpha)
        .create(&device)
        .unwrap(),
    );

    let sprite_list_pane_view = SpriteListPaneView::new(
        &device,
        adapter_memory_info.types(),
        graphics_queue_family_index,
        &mut graphics_queue,
        surface_events.optimal_buffer_scale,
    );

    let mut composite_instance_buffer = br::BufferObject::new(
        &device,
        &br::BufferCreateInfo::new_for_type::<[CompositeInstanceData; 1024]>(
            br::BufferUsage::TRANSFER_DEST.vertex_buffer(),
        ),
    )
    .unwrap();
    let composite_instance_buffer_memreq = composite_instance_buffer.requirements();
    let device_local_memblock = br::DeviceMemoryObject::new(
        &device,
        &br::MemoryAllocateInfo::new(
            composite_instance_buffer_memreq.size,
            adapter_memory_info
                .types()
                .iter()
                .enumerate()
                .find(|(n, x)| {
                    (composite_instance_buffer_memreq.memoryTypeBits & (1 << n)) != 0
                        && (x.propertyFlags & br::vk::VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT) != 0
                })
                .map(|(n, _)| n)
                .unwrap() as _,
        ),
    )
    .unwrap();
    composite_instance_buffer
        .bind(&device_local_memblock, 0)
        .unwrap();

    let mut composite_instance_buffer_stg = br::BufferObject::new(
        &device,
        &br::BufferCreateInfo::new_for_type::<[CompositeInstanceData; 1024]>(
            br::BufferUsage::TRANSFER_SRC,
        ),
    )
    .unwrap();
    let composite_instance_buffer_stg_memreq = composite_instance_buffer_stg.requirements();
    let stg_mem = adapter_memory_info
        .types()
        .iter()
        .enumerate()
        .find(|(n, x)| {
            (composite_instance_buffer_memreq.memoryTypeBits & (1 << n)) != 0
                && (x.propertyFlags & br::vk::VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT) != 0
        })
        .unwrap();
    let stg_mem_requires_flush =
        (stg_mem.1.propertyFlags & br::vk::VK_MEMORY_PROPERTY_HOST_COHERENT_BIT) == 0;
    let mut stg_memblock = br::DeviceMemoryObject::new(
        &device,
        &br::MemoryAllocateInfo::new(composite_instance_buffer_stg_memreq.size, stg_mem.0 as _),
    )
    .unwrap();
    composite_instance_buffer_stg
        .bind(&stg_memblock, 0)
        .unwrap();

    let stg_memblock_nptr = stg_memblock.native_ptr();
    let ptr = stg_memblock
        .map(0..core::mem::size_of::<CompositeInstanceData>())
        .unwrap();
    unsafe {
        core::ptr::write(
            ptr.get_mut(0),
            CompositeInstanceData {
                pos_st: [
                    128.0 * surface_events.optimal_buffer_scale as f32,
                    160.0 * surface_events.optimal_buffer_scale as f32,
                    100.0 * surface_events.optimal_buffer_scale as f32,
                    100.0 * surface_events.optimal_buffer_scale as f32,
                ],
                uv_st: [1.0, 1.0, 0.0, 0.0],
                slice_borders: [
                    SpriteListPaneView::CORNER_RADIUS * surface_events.optimal_buffer_scale as f32,
                    SpriteListPaneView::CORNER_RADIUS * surface_events.optimal_buffer_scale as f32,
                    SpriteListPaneView::CORNER_RADIUS * surface_events.optimal_buffer_scale as f32,
                    SpriteListPaneView::CORNER_RADIUS * surface_events.optimal_buffer_scale as f32,
                ],
                tex_size_pixels_composite_mode: [
                    (SpriteListPaneView::CORNER_RADIUS * 2.0 + 1.0)
                        * surface_events.optimal_buffer_scale as f32,
                    (SpriteListPaneView::CORNER_RADIUS * 2.0 + 1.0)
                        * surface_events.optimal_buffer_scale as f32,
                    1.0,
                    0.0,
                ],
                color_tint: [1.0, 1.0, 1.0, 0.5],
            },
        );
    }
    if stg_mem_requires_flush {
        unsafe {
            device
                .flush_mapped_memory_ranges(&[br::vk::VkMappedMemoryRange {
                    sType: br::vk::VkMappedMemoryRange::TYPE,
                    pNext: core::ptr::null(),
                    memory: stg_memblock_nptr,
                    offset: 0,
                    size: core::mem::size_of::<CompositeInstanceData>() as _,
                }])
                .unwrap();
        }
    }
    ptr.end();
    let mut composite_instance_buffer_dirty = true;

    let main_rp = br::RenderPassObject::new(
        &device,
        &br::RenderPassCreateInfo2::new(
            &[br::AttachmentDescription2::new(sc_format.format)
                .layout_transition(br::ImageLayout::Undefined, br::ImageLayout::PresentSrc)
                .color_memory_op(br::LoadOp::Clear, br::StoreOp::Store)],
            &[
                br::SubpassDescription2::new().colors(&[br::AttachmentReference2::color(
                    0,
                    br::ImageLayout::ColorAttachmentOpt,
                )]),
            ],
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

    let mut backbuffer_views = sc
        .images_alloc()
        .unwrap()
        .into_iter()
        .map(|bb| {
            bb.clone_parent()
                .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
                .view_builder()
                .create()
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut main_fbs = backbuffer_views
        .iter()
        .map(|bb| {
            br::FramebufferObject::new(
                &device,
                &br::FramebufferCreateInfo::new(
                    &main_rp,
                    &[bb.as_transparent_ref()],
                    sc_size.width,
                    sc_size.height,
                ),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();

    let composite_sampler = br::SamplerObject::new(&device, &br::SamplerCreateInfo::new()).unwrap();

    let composite_vsh_byte_length = std::fs::metadata("resources/composite.vert").unwrap().len();
    assert_eq!(composite_vsh_byte_length & 0x03, 0);
    let mut composite_vsh_words = vec![0u32; composite_vsh_byte_length as usize >> 2];
    std::fs::File::open("resources/composite.vert")
        .unwrap()
        .read_exact(unsafe {
            core::slice::from_raw_parts_mut(
                composite_vsh_words.as_mut_ptr() as *mut u8,
                composite_vsh_words.len() << 2,
            )
        })
        .unwrap();
    let composite_vsh = br::ShaderModuleObject::new(
        &device,
        &br::ShaderModuleCreateInfo::new(&composite_vsh_words),
    )
    .unwrap();

    let composite_fsh_byte_length = std::fs::metadata("resources/composite.frag").unwrap().len();
    assert_eq!(composite_fsh_byte_length & 0x03, 0);
    let mut composite_fsh_words = vec![0u32; composite_fsh_byte_length as usize >> 2];
    std::fs::File::open("resources/composite.frag")
        .unwrap()
        .read_exact(unsafe {
            core::slice::from_raw_parts_mut(
                composite_fsh_words.as_mut_ptr() as *mut u8,
                composite_fsh_words.len() << 2,
            )
        })
        .unwrap();
    let composite_fsh = br::ShaderModuleObject::new(
        &device,
        &br::ShaderModuleCreateInfo::new(&composite_fsh_words),
    )
    .unwrap();

    let composite_fsh_input_layout = br::DescriptorSetLayoutObject::new(
        &device,
        &br::DescriptorSetLayoutCreateInfo::new(&[
            br::DescriptorType::CombinedImageSampler.make_binding(0, 1)
        ]),
    )
    .unwrap();
    let mut descriptor_pool = br::DescriptorPoolObject::new(
        &device,
        &br::DescriptorPoolCreateInfo::new(
            1,
            &[br::DescriptorType::CombinedImageSampler.make_size(1)],
        ),
    )
    .unwrap();
    let [composite_tex_descriptor] = descriptor_pool
        .alloc_array(&[composite_fsh_input_layout.as_transparent_ref()])
        .unwrap();
    device.update_descriptor_sets(
        &[composite_tex_descriptor.binding_at(0).write(
            br::DescriptorContents::CombinedImageSampler(vec![
                br::DescriptorImageInfo::new(
                    &sprite_list_pane_view.frame_image,
                    br::ImageLayout::ShaderReadOnlyOpt,
                )
                .with_sampler(&composite_sampler),
            ]),
        )],
        &[],
    );

    let composite_pipeline_layout = br::PipelineLayoutObject::new(
        &device,
        &br::PipelineLayoutCreateInfo::new(
            &[composite_fsh_input_layout.as_transparent_ref()],
            &[br::vk::VkPushConstantRange::for_type::<[f32; 2]>(
                br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                0,
            )],
        ),
    )
    .unwrap();
    let composite_vbinds =
        [br::vk::VkVertexInputBindingDescription::per_instance_typed::<CompositeInstanceData>(0)];
    let composite_vinput = br::PipelineVertexInputStateCreateInfo::new(
        &composite_vbinds,
        &[
            br::vk::VkVertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: core::mem::offset_of!(CompositeInstanceData, pos_st) as _,
            },
            br::vk::VkVertexInputAttributeDescription {
                location: 1,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: core::mem::offset_of!(CompositeInstanceData, uv_st) as _,
            },
            br::vk::VkVertexInputAttributeDescription {
                location: 2,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: core::mem::offset_of!(CompositeInstanceData, slice_borders) as _,
            },
            br::vk::VkVertexInputAttributeDescription {
                location: 3,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: core::mem::offset_of!(CompositeInstanceData, tex_size_pixels_composite_mode)
                    as _,
            },
            br::vk::VkVertexInputAttributeDescription {
                location: 4,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: core::mem::offset_of!(CompositeInstanceData, color_tint) as _,
            },
        ],
    );

    let [mut composite_pipeline] = device
        .new_graphics_pipeline_array(
            &[br::GraphicsPipelineCreateInfo::new(
                &composite_pipeline_layout,
                main_rp.subpass(0),
                &[
                    br::PipelineShaderStage::new(br::ShaderStage::Vertex, &composite_vsh, c"main"),
                    br::PipelineShaderStage::new(
                        br::ShaderStage::Fragment,
                        &composite_fsh,
                        c"main",
                    ),
                ],
                &composite_vinput,
                &br::PipelineInputAssemblyStateCreateInfo::new(
                    br::PrimitiveTopology::TriangleStrip,
                ),
                &br::PipelineViewportStateCreateInfo::new(
                    &[br::vk::VkViewport {
                        x: 0.0,
                        y: 0.0,
                        width: sc_size.width as _,
                        height: sc_size.height as _,
                        minDepth: 0.0,
                        maxDepth: 1.0,
                    }],
                    &[br::vk::VkRect2D {
                        offset: br::vk::VkOffset2D::ZERO,
                        extent: sc_size,
                    }],
                ),
                &br::PipelineRasterizationStateCreateInfo::new(
                    br::PolygonMode::Fill,
                    br::CullModeFlags::NONE,
                    br::FrontFace::CounterClockwise,
                ),
                &br::PipelineColorBlendStateCreateInfo::new(&[
                    br::vk::VkPipelineColorBlendAttachmentState::PREMULTIPLIED,
                ]),
            )
            .multisample_state(&br::PipelineMultisampleStateCreateInfo::new())],
            None::<&br::PipelineCacheObject<&br::DeviceObject<&br::InstanceObject>>>,
        )
        .unwrap();

    let mut main_cp = br::CommandPoolObject::new(
        &device,
        &br::CommandPoolCreateInfo::new(graphics_queue_family_index),
    )
    .unwrap();
    let mut main_cbs = br::CommandBufferObject::alloc(
        &device,
        &br::CommandBufferAllocateInfo::new(
            &mut main_cp,
            main_fbs.len() as _,
            br::CommandBufferLevel::Primary,
        ),
    )
    .unwrap();

    for (cb, fb) in main_cbs.iter_mut().zip(main_fbs.iter()) {
        unsafe { cb.begin(&device).unwrap() }
            .begin_render_pass_2(
                &br::RenderPassBeginInfo::new(
                    &main_rp,
                    fb,
                    br::vk::VkRect2D {
                        offset: br::vk::VkOffset2D::ZERO,
                        extent: sc_size,
                    },
                    &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 1.0])],
                ),
                &br::vk::VkSubpassBeginInfo::new(br::SubpassContents::Inline as _),
            )
            .bind_pipeline(br::PipelineBindPoint::Graphics, &composite_pipeline)
            .bind_vertex_buffer_array(0, &[composite_instance_buffer.as_transparent_ref()], &[0])
            .push_constant(
                &composite_pipeline_layout,
                br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                0,
                &[sc_size.width as f32, sc_size.height as f32],
            )
            .bind_descriptor_sets(
                br::PipelineBindPoint::Graphics,
                &composite_pipeline_layout,
                0,
                &[composite_tex_descriptor],
                &[],
            )
            .draw(4, 1, 0, 0)
            .end_render_pass_2(&br::vk::VkSubpassEndInfo::new())
            .end()
            .unwrap();
    }

    let mut update_cp = br::CommandPoolObject::new(
        &device,
        &br::CommandPoolCreateInfo::new(graphics_queue_family_index),
    )
    .unwrap();
    let [mut update_cb] = br::CommandBufferObject::alloc_array(
        &device,
        &br::CommandBufferFixedCountAllocateInfo::new(
            &mut update_cp,
            br::CommandBufferLevel::Primary,
        ),
    )
    .unwrap();

    let mut acquire_completion =
        br::SemaphoreObject::new(&device, &br::SemaphoreCreateInfo::new()).unwrap();
    let render_completion =
        br::SemaphoreObject::new(&device, &br::SemaphoreCreateInfo::new()).unwrap();
    let mut last_render_command_fence =
        br::FenceObject::new(&device, &br::FenceCreateInfo::new(0)).unwrap();
    let mut last_rendering;
    let mut last_update_command_fence =
        br::FenceObject::new(&device, &br::FenceCreateInfo::new(0)).unwrap();
    let mut last_updating = false;

    struct FrameCallback {
        app_event_sender: std::sync::mpsc::Sender<AppEvent>,
    }
    impl wl::CallbackEventListener for FrameCallback {
        fn done(&mut self, _: &mut wl::Callback, _: u32) {
            self.app_event_sender
                .send(AppEvent::ToplevelWindowFrameTiming)
                .unwrap();
        }
    }
    let mut frame_callback = FrameCallback {
        app_event_sender: app_event_sender.clone(),
    };

    let mut frame = wl_surface.frame().expect("Failed to request next frame");
    frame
        .add_listener(&mut frame_callback)
        .expect("Failed to set frame callback");

    // fire initial update/render
    if core::mem::replace(&mut composite_instance_buffer_dirty, false) {
        unsafe { update_cb.begin(&device).unwrap() }
            .copy_buffer(
                &composite_instance_buffer_stg,
                &composite_instance_buffer,
                &[br::BufferCopy::mirror(
                    0,
                    (core::mem::size_of::<CompositeInstanceData>() * 1024) as _,
                )],
            )
            .pipeline_barrier_2(&br::DependencyInfo::new(
                &[br::MemoryBarrier2::new()
                    .of_memory(
                        br::AccessFlags2::TRANSFER.write,
                        br::AccessFlags2::VERTEX_ATTRIBUTE_READ,
                    )
                    .of_execution(
                        br::PipelineStageFlags2::COPY,
                        br::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
                    )],
                &[],
                &[],
            ))
            .end()
            .unwrap();
        graphics_queue
            .submit2(
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
    let next = sc
        .acquire_next(
            None,
            br::CompletionHandlerMut::Queue(acquire_completion.as_transparent_ref_mut()),
        )
        .unwrap();
    graphics_queue
        .submit2(
            &[br::SubmitInfo2::new(
                &[br::SemaphoreSubmitInfo::new(&acquire_completion).on_color_attachment_output()],
                &[br::CommandBufferSubmitInfo::new(&main_cbs[next as usize])],
                &[br::SemaphoreSubmitInfo::new(&render_completion).on_color_attachment_output()],
            )],
            Some(last_render_command_fence.as_transparent_ref_mut()),
        )
        .unwrap();
    last_rendering = true;
    graphics_queue
        .present(&br::PresentInfo::new(
            &[render_completion.as_transparent_ref()],
            &[sc.as_transparent_ref()],
            &[next],
            &mut [br::vk::VkResult(0)],
        ))
        .unwrap();

    dp.flush().unwrap();
    let mut t = std::time::Instant::now();
    let mut frame_resize_request = None;
    'app: loop {
        dp.dispatch().expect("Failed to dispatch");
        while let Ok(e) = app_event_receiver.try_recv() {
            match e {
                AppEvent::ToplevelWindowClose => break 'app,
                AppEvent::ToplevelWindowFrameTiming => {
                    let dt = t.elapsed();
                    t = std::time::Instant::now();
                    // print!("frame {dt:?}\n");

                    if last_rendering {
                        last_render_command_fence.wait().unwrap();
                        last_rendering = false;
                    }

                    if core::mem::replace(&mut composite_instance_buffer_dirty, false) {
                        if last_updating {
                            last_update_command_fence.wait().unwrap();
                            last_updating = false;
                        }

                        last_update_command_fence.reset().unwrap();
                        unsafe { update_cb.begin(&device).unwrap() }
                            .copy_buffer(
                                &composite_instance_buffer_stg,
                                &composite_instance_buffer,
                                &[br::BufferCopy::mirror(
                                    0,
                                    (core::mem::size_of::<CompositeInstanceData>() * 1024) as _,
                                )],
                            )
                            .pipeline_barrier_2(&br::DependencyInfo::new(
                                &[br::MemoryBarrier2::new()
                                    .of_memory(
                                        br::AccessFlags2::TRANSFER.write,
                                        br::AccessFlags2::VERTEX_ATTRIBUTE_READ,
                                    )
                                    .of_execution(
                                        br::PipelineStageFlags2::COPY,
                                        br::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
                                    )],
                                &[],
                                &[],
                            ))
                            .end()
                            .unwrap();
                        graphics_queue
                            .submit2(
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

                    last_render_command_fence.reset().unwrap();
                    let next = sc
                        .acquire_next(
                            None,
                            br::CompletionHandlerMut::Queue(
                                acquire_completion.as_transparent_ref_mut(),
                            ),
                        )
                        .unwrap();
                    graphics_queue
                        .submit2(
                            &[br::SubmitInfo2::new(
                                &[br::SemaphoreSubmitInfo::new(&acquire_completion)
                                    .on_color_attachment_output()],
                                &[br::CommandBufferSubmitInfo::new(&main_cbs[next as usize])],
                                &[br::SemaphoreSubmitInfo::new(&render_completion)
                                    .on_color_attachment_output()],
                            )],
                            Some(last_render_command_fence.as_transparent_ref_mut()),
                        )
                        .unwrap();
                    last_rendering = true;
                    graphics_queue
                        .present(&br::PresentInfo::new(
                            &[render_completion.as_transparent_ref()],
                            &[sc.as_transparent_ref()],
                            &[next],
                            &mut [br::vk::VkResult(0)],
                        ))
                        .unwrap();

                    frame = wl_surface.frame().expect("Failed to request next frame");
                    frame
                        .add_listener(&mut frame_callback)
                        .expect("Failed to set frame callback");
                }
                AppEvent::ToplevelWindowConfigure { width, height } => {
                    println!("ToplevelWindowConfigure {width} {height}");
                    frame_resize_request = Some((width, height));
                }
                AppEvent::ToplevelWindowSurfaceConfigure { serial } => {
                    if let Some((w, h)) = frame_resize_request.take() {
                        if w != sc_size.width || h != sc_size.height {
                            println!("frame resize: {w} {h}");

                            sc_size.width = w;
                            sc_size.height = h;

                            if last_rendering {
                                last_render_command_fence.wait().unwrap();
                                last_rendering = false;
                            }

                            unsafe {
                                main_cp
                                    .reset(br::vk::VK_COMMAND_POOL_RESET_RELEASE_RESOURCES_BIT)
                                    .unwrap();
                            }
                            drop(main_fbs);
                            drop(backbuffer_views);
                            drop(sc);
                            sc = Rc::new(
                                br::SwapchainBuilder::new(
                                    &surface,
                                    2,
                                    sc_format.clone(),
                                    sc_size,
                                    br::ImageUsageFlags::COLOR_ATTACHMENT,
                                )
                                .pre_transform(sc_transform)
                                .composite_alpha(sc_composite_alpha)
                                .create(&device)
                                .unwrap(),
                            );

                            backbuffer_views = sc
                                .images_alloc()
                                .unwrap()
                                .into_iter()
                                .map(|bb| {
                                    bb.clone_parent()
                                        .subresource_range(br::AspectMask::COLOR, 0..1, 0..1)
                                        .view_builder()
                                        .create()
                                        .unwrap()
                                })
                                .collect::<Vec<_>>();
                            main_fbs = backbuffer_views
                                .iter()
                                .map(|bb| {
                                    br::FramebufferObject::new(
                                        &device,
                                        &br::FramebufferCreateInfo::new(
                                            &main_rp,
                                            &[bb.as_transparent_ref()],
                                            sc_size.width,
                                            sc_size.height,
                                        ),
                                    )
                                    .unwrap()
                                })
                                .collect::<Vec<_>>();

                            let [composite_pipeline1] = device
                                .new_graphics_pipeline_array(
                                    &[br::GraphicsPipelineCreateInfo::new(
                                        &composite_pipeline_layout,
                                        main_rp.subpass(0),
                                        &[
                                            br::PipelineShaderStage::new(br::ShaderStage::Vertex, &composite_vsh, c"main"),
                                            br::PipelineShaderStage::new(
                                                br::ShaderStage::Fragment,
                                                &composite_fsh,
                                                c"main",
                                            ),
                                        ],
                                        &composite_vinput,
                                        &br::PipelineInputAssemblyStateCreateInfo::new(
                                            br::PrimitiveTopology::TriangleStrip,
                                        ),
                                        &br::PipelineViewportStateCreateInfo::new(
                                            &[br::vk::VkViewport {
                                                x: 0.0,
                                                y: 0.0,
                                                width: sc_size.width as _,
                                                height: sc_size.height as _,
                                                minDepth: 0.0,
                                                maxDepth: 1.0,
                                            }],
                                            &[br::vk::VkRect2D {
                                                offset: br::vk::VkOffset2D::ZERO,
                                                extent: sc_size,
                                            }],
                                        ),
                                        &br::PipelineRasterizationStateCreateInfo::new(
                                            br::PolygonMode::Fill,
                                            br::CullModeFlags::NONE,
                                            br::FrontFace::CounterClockwise,
                                        ),
                                        &br::PipelineColorBlendStateCreateInfo::new(&[
                                            br::vk::VkPipelineColorBlendAttachmentState::PREMULTIPLIED,
                                        ]),
                                    )
                                    .multisample_state(&br::PipelineMultisampleStateCreateInfo::new())],
                                    None::<&br::PipelineCacheObject<&br::DeviceObject<&br::InstanceObject>>>,
                                )
                                .unwrap();
                            composite_pipeline = composite_pipeline1;

                            for (cb, fb) in main_cbs.iter_mut().zip(main_fbs.iter()) {
                                unsafe { cb.begin(&device).unwrap() }
                                    .begin_render_pass_2(
                                        &br::RenderPassBeginInfo::new(
                                            &main_rp,
                                            fb,
                                            br::vk::VkRect2D {
                                                offset: br::vk::VkOffset2D::ZERO,
                                                extent: sc_size,
                                            },
                                            &[br::ClearValue::color_f32([0.0, 0.0, 0.0, 1.0])],
                                        ),
                                        &br::vk::VkSubpassBeginInfo::new(
                                            br::SubpassContents::Inline as _,
                                        ),
                                    )
                                    .bind_pipeline(
                                        br::PipelineBindPoint::Graphics,
                                        &composite_pipeline,
                                    )
                                    .bind_vertex_buffer_array(
                                        0,
                                        &[composite_instance_buffer.as_transparent_ref()],
                                        &[0],
                                    )
                                    .push_constant(
                                        &composite_pipeline_layout,
                                        br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                                        0,
                                        &[sc_size.width as f32, sc_size.height as f32],
                                    )
                                    .bind_descriptor_sets(
                                        br::PipelineBindPoint::Graphics,
                                        &composite_pipeline_layout,
                                        0,
                                        &[composite_tex_descriptor],
                                        &[],
                                    )
                                    .draw(4, 1, 0, 0)
                                    .end_render_pass_2(&br::vk::VkSubpassEndInfo::new())
                                    .end()
                                    .unwrap();
                            }
                        }
                    }

                    println!("ToplevelWindowSurfaceConfigure {serial}");
                    xdg_surface
                        .ack_configure(serial)
                        .expect("Failed to ack configure");
                }
            }
        }
    }

    unsafe {
        device.wait().unwrap();
    }
}
