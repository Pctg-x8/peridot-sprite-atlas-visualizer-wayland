use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
};

use bedrock::{
    self as br, DescriptorPoolMut, Device, DeviceMemoryMut, ImageChild, MemoryBound, ShaderModule,
    VkHandle, VkObject,
};
use image::EncodableLayout;
use parking_lot::RwLock;

use crate::{
    BLEND_STATE_SINGLE_NONE, BLEND_STATE_SINGLE_PREMULTIPLIED, BufferedStagingScratchBuffer,
    IA_STATE_TRILIST, IA_STATE_TRISTRIP, MS_STATE_EMPTY, RASTER_STATE_DEFAULT_FILL_NOCULL,
    VI_STATE_EMPTY, VI_STATE_FLOAT4_ONLY,
    app_state::SpriteInfo,
    base_system::AppBaseSystem,
    bg_worker::{BackgroundWork, BackgroundWorkerEnqueueAccess},
    coordinate::SizePixels,
    subsystem::{
        StagingScratchBufferManager, StagingScratchBufferMapMode, Subsystem,
        SubsystemShaderModuleRef,
    },
};

#[repr(C)]
struct GridParams {
    pub offset: [f32; 2],
    pub size: [f32; 2],
}

struct LoadedSpriteSourceAtlas<'subsystem> {
    resource: br::ImageViewObject<br::ImageObject<&'subsystem Subsystem>>,
    memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    left: u32,
    top: u32,
    max_height: u32,
}
impl<'subsystem> LoadedSpriteSourceAtlas<'subsystem> {
    const SIZE: u32 = 4096;

    #[tracing::instrument(skip(base_system), fields(size = Self::SIZE))]
    fn new(base_system: &AppBaseSystem<'subsystem>, format: br::Format) -> Self {
        let mut resource = br::ImageObject::new(
            base_system.subsystem,
            &br::ImageCreateInfo::new(br::Extent2D::spread1(Self::SIZE), format)
                .sampled()
                .transfer_dest()
                .usage_with(br::ImageUsageFlags::TRANSFER_SRC),
        )
        .unwrap();
        resource
            .set_name(Some(c"Loaded Sprite Source Atlas"))
            .unwrap();
        let req = resource.requirements();
        let memindex = base_system
            .find_device_local_memory_index(req.memoryTypeBits)
            .unwrap();
        let memory = br::DeviceMemoryObject::new(
            base_system.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        resource.bind(&memory, 0).unwrap();
        let resource = br::ImageViewBuilder::new(
            resource,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()
        .unwrap();

        Self {
            resource,
            memory,
            left: 0,
            top: 0,
            max_height: 0,
        }
    }

    fn alloc(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if width > Self::SIZE || self.top + height > Self::SIZE {
            // でかすぎ
            return None;
        }

        if self.left + width <= Self::SIZE {
            // まだ横にはいる
            let p = (self.left, self.top);
            self.left += width;
            self.max_height = self.max_height.max(height);

            return Some(p);
        }

        // 改行
        let p = (self.left, self.top);
        self.top += self.max_height;
        if self.top + height > Self::SIZE {
            // 結局はいらん
            return None;
        }
        self.left = width;
        self.max_height = height;
        Some(p)
    }

    // TODO: 解放処理どうする？
}

#[repr(C)]
struct SpriteInstance {
    pos_st: [f32; 4],
    uv_st: [f32; 4],
}

struct SpriteInstanceBuffers<'subsystem> {
    subsystem: &'subsystem Subsystem,
    buffer: br::BufferObject<&'subsystem Subsystem>,
    memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    stg_buffer: br::BufferObject<&'subsystem Subsystem>,
    stg_memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    stg_requires_flush: bool,
    is_dirty: bool,
    capacity: br::DeviceSize,
}
impl<'subsystem> SpriteInstanceBuffers<'subsystem> {
    const BUCKET_SIZE: br::DeviceSize = 64;

    #[tracing::instrument(skip(subsystem))]
    fn new(subsystem: &'subsystem Subsystem) -> Self {
        let capacity = Self::BUCKET_SIZE;
        let byte_length = capacity as usize * core::mem::size_of::<SpriteInstance>();

        let mut buffer = br::BufferObject::new(
            subsystem,
            &br::BufferCreateInfo::new(
                byte_length,
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        )
        .unwrap();
        let req = buffer.requirements();
        let memindex = subsystem
            .find_device_local_memory_index(req.memoryTypeBits)
            .unwrap();
        let memory = br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        buffer.bind(&memory, 0).unwrap();

        let mut stg_buffer = br::BufferObject::new(
            subsystem,
            &br::BufferCreateInfo::new(byte_length, br::BufferUsage::TRANSFER_SRC),
        )
        .unwrap();
        let req = stg_buffer.requirements();
        let memindex = subsystem
            .find_host_visible_memory_index(req.memoryTypeBits)
            .unwrap();
        let stg_memory = br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        stg_buffer.bind(&stg_memory, 0).unwrap();
        let stg_requires_flush = !subsystem.is_coherent_memory_type(memindex);

        Self {
            subsystem,
            buffer,
            memory,
            stg_buffer,
            stg_memory,
            stg_requires_flush,
            is_dirty: false,
            capacity,
        }
    }

    /// return: true if resized
    #[tracing::instrument(skip(self), fields(required_capacity))]
    fn require_capacity(&mut self, element_count: br::DeviceSize) -> bool {
        let required_capacity = (element_count + Self::BUCKET_SIZE - 1) & !(Self::BUCKET_SIZE - 1);
        tracing::Span::current().record("required_capacity", required_capacity);

        if self.capacity >= required_capacity {
            // enough
            return false;
        }

        // realloc
        self.capacity = required_capacity;
        let byte_length = self.capacity as usize * core::mem::size_of::<SpriteInstance>();

        self.buffer = br::BufferObject::new(
            self.subsystem,
            &br::BufferCreateInfo::new(
                byte_length,
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        )
        .unwrap();
        let req = self.buffer.requirements();
        let memindex = self
            .subsystem
            .find_device_local_memory_index(req.memoryTypeBits)
            .unwrap();
        self.memory = br::DeviceMemoryObject::new(
            self.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        self.buffer.bind(&self.memory, 0).unwrap();

        self.stg_buffer = br::BufferObject::new(
            self.subsystem,
            &br::BufferCreateInfo::new(byte_length, br::BufferUsage::TRANSFER_SRC),
        )
        .unwrap();
        let req = self.stg_buffer.requirements();
        let memindex = self
            .subsystem
            .find_host_visible_memory_index(req.memoryTypeBits)
            .unwrap();
        self.stg_memory = br::DeviceMemoryObject::new(
            self.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        self.stg_buffer.bind(&self.stg_memory, 0).unwrap();
        self.stg_requires_flush = !self.subsystem.is_coherent_memory_type(memindex);

        true
    }
}

pub struct EditingAtlasRenderer<'d> {
    _sprite_sampler: br::SamplerObject<&'d Subsystem>,
    pub param_buffer: br::BufferObject<&'d Subsystem>,
    _param_buffer_memory: br::DeviceMemoryObject<&'d Subsystem>,
    current_params_data: GridParams,
    param_is_dirty: bool,
    atlas_size: SizePixels,
    bg_vertex_buffer_is_dirty: bool,
    pub bg_vertex_buffer: br::BufferObject<&'d Subsystem>,
    _bg_vertex_buffer_memory: br::DeviceMemoryObject<&'d Subsystem>,
    grid_vsh: SubsystemShaderModuleRef<'d>,
    grid_fsh: SubsystemShaderModuleRef<'d>,
    bg_vsh: SubsystemShaderModuleRef<'d>,
    bg_fsh: SubsystemShaderModuleRef<'d>,
    pub render_pipeline_layout: br::PipelineLayoutObject<&'d Subsystem>,
    pub render_pipeline: br::PipelineObject<&'d Subsystem>,
    pub bg_render_pipeline: br::PipelineObject<&'d Subsystem>,
    _dsl_param: br::DescriptorSetLayoutObject<&'d Subsystem>,
    _dsl_sprite_instance: br::DescriptorSetLayoutObject<&'d Subsystem>,
    _dp: br::DescriptorPoolObject<&'d Subsystem>,
    pub ds_param: br::DescriptorSet,
    ds_sprite_instance: br::DescriptorSet,
    loaded_sprite_source_atlas: RefCell<LoadedSpriteSourceAtlas<'d>>,
    sprite_instance_buffers: RefCell<SpriteInstanceBuffers<'d>>,
    sprite_atlas_rect_by_path: RefCell<HashMap<PathBuf, (u32, u32, u32, u32)>>,
    sprite_instance_vsh: SubsystemShaderModuleRef<'d>,
    sprite_instance_fsh: SubsystemShaderModuleRef<'d>,
    sprite_instance_render_pipeline_layout: br::PipelineLayoutObject<&'d Subsystem>,
    sprite_instance_render_pipeline: br::PipelineObject<&'d Subsystem>,
    sprite_count: Cell<usize>,
    sprite_image_copies: Arc<RwLock<HashMap<usize, Vec<br::vk::VkBufferImageCopy>>>>,
}
impl<'d> EditingAtlasRenderer<'d> {
    #[tracing::instrument(skip(app_system, rendered_pass))]
    pub fn new<'app_system>(
        app_system: &'app_system AppBaseSystem<'d>,
        rendered_pass: br::SubpassRef<impl br::RenderPass>,
        main_buffer_size: br::Extent2D,
        init_atlas_size: SizePixels,
    ) -> Self
    where
        'd: 'app_system,
    {
        let sprite_sampler =
            br::SamplerObject::new(app_system.subsystem, &br::SamplerCreateInfo::new()).unwrap();

        let mut param_buffer = match br::BufferObject::new(
            app_system.subsystem,
            &br::BufferCreateInfo::new_for_type::<GridParams>(
                br::BufferUsage::UNIFORM_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create param buffer");
                std::process::abort();
            }
        };
        let mreq = param_buffer.requirements();
        let Some(memindex) = app_system.find_device_local_memory_index(mreq.memoryTypeBits) else {
            tracing::error!("No suitable memory for param buffer");
            std::process::abort();
        };
        let param_buffer_memory = match br::DeviceMemoryObject::new(
            app_system.subsystem,
            &br::MemoryAllocateInfo::new(mreq.size, memindex),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to allocate param buffer memory");
                std::process::abort();
            }
        };
        if let Err(e) = param_buffer.bind(&param_buffer_memory, 0) {
            tracing::warn!(reason = ?e, "Failed to bind param buffer memory");
        }

        let mut bg_vertex_buffer = match br::BufferObject::new(
            app_system.subsystem,
            &br::BufferCreateInfo::new_for_type::<[[f32; 4]; 4]>(
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create bg vertex buffer");
                std::process::abort();
            }
        };
        let mreq = bg_vertex_buffer.requirements();
        let Some(memindex) = app_system.find_device_local_memory_index(mreq.memoryTypeBits) else {
            tracing::error!("No suitable memory for bg vertex buffer");
            std::process::abort();
        };
        let bg_vertex_buffer_memory = match br::DeviceMemoryObject::new(
            app_system.subsystem,
            &br::MemoryAllocateInfo::new(mreq.size, memindex),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to allocate bg vertex buffer memory");
                std::process::abort();
            }
        };
        if let Err(e) = bg_vertex_buffer.bind(&bg_vertex_buffer_memory, 0) {
            tracing::warn!(reason = ?e, "Failed to bind bg vertex buffer memory");
        }

        let dsl_param = match br::DescriptorSetLayoutObject::new(
            app_system.subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[
                br::DescriptorType::UniformBuffer.make_binding(0, 1)
            ]),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create param descriptor set layout");
                std::process::abort();
            }
        };
        let dsl_sprite_instance = match br::DescriptorSetLayoutObject::new(
            app_system.subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
                .make_binding(0, 1)
                .with_immutable_samplers(&[sprite_sampler.as_transparent_ref()])]),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create sprite instance descriptor set layout");
                std::process::exit(1);
            }
        };

        let vsh = app_system.require_shader("resources/filltri.vert");
        let fsh = app_system.require_shader("resources/grid.frag");
        let bg_vsh = app_system.require_shader("resources/atlas_bg.vert");
        let bg_fsh = app_system.require_shader("resources/atlas_bg.frag");
        let sprite_instance_vsh = app_system.require_shader("resources/sprite_instance.vert");
        let sprite_instance_fsh = app_system.require_shader("resources/sprite_instance.frag");

        let render_pipeline_layout = match br::PipelineLayoutObject::new(
            app_system.subsystem,
            &br::PipelineLayoutCreateInfo::new(
                &[dsl_param.as_transparent_ref()],
                &[br::PushConstantRange::for_type::<[f32; 2]>(
                    br::vk::VK_SHADER_STAGE_FRAGMENT_BIT | br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                )],
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create pipeline layout");
                std::process::abort();
            }
        };
        let sprite_instance_render_pipeline_layout = match br::PipelineLayoutObject::new(
            app_system.subsystem,
            &br::PipelineLayoutCreateInfo::new(
                &[
                    dsl_param.as_transparent_ref(),
                    dsl_sprite_instance.as_transparent_ref(),
                ],
                &[br::PushConstantRange::for_type::<[f32; 2]>(
                    br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                )],
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create sprite instance pipeline layout");
                std::process::exit(1);
            }
        };
        let [
            render_pipeline,
            bg_render_pipeline,
            sprite_instance_render_pipeline,
        ] = app_system
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    &render_pipeline_layout,
                    rendered_pass,
                    &[
                        vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[main_buffer_size
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[main_buffer_size.into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &render_pipeline_layout,
                    rendered_pass,
                    &[
                        bg_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        bg_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_FLOAT4_ONLY,
                    IA_STATE_TRISTRIP,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[main_buffer_size
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[main_buffer_size.into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &sprite_instance_render_pipeline_layout,
                    rendered_pass,
                    &[
                        sprite_instance_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        sprite_instance_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    &br::PipelineVertexInputStateCreateInfo::new(
                        &[br::VertexInputBindingDescription::per_instance_typed::<
                            [f32; 8],
                        >(0)],
                        &[
                            br::VertexInputAttributeDescription {
                                location: 0,
                                binding: 0,
                                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                                offset: 0,
                            },
                            br::VertexInputAttributeDescription {
                                location: 1,
                                binding: 0,
                                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                                offset: (core::mem::size_of::<f32>() * 4) as _,
                            },
                        ],
                    ),
                    IA_STATE_TRISTRIP,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[main_buffer_size
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[main_buffer_size.into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_PREMULTIPLIED,
                )
                .multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        let loaded_sprite_source_atlas =
            LoadedSpriteSourceAtlas::new(app_system, br::vk::VK_FORMAT_R8G8B8A8_UNORM);
        let sprite_instance_buffers = SpriteInstanceBuffers::new(app_system.subsystem);

        let mut dp = match br::DescriptorPoolObject::new(
            app_system.subsystem,
            &br::DescriptorPoolCreateInfo::new(
                2,
                &[
                    br::DescriptorType::UniformBuffer.make_size(1),
                    br::DescriptorType::CombinedImageSampler.make_size(1),
                ],
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create descriptor pool");
                std::process::abort();
            }
        };
        let [ds_param, ds_sprite_instance] = match dp.alloc_array(&[
            dsl_param.as_transparent_ref(),
            dsl_sprite_instance.as_transparent_ref(),
        ]) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to allocate descriptor sets");
                std::process::abort();
            }
        };
        app_system.subsystem.update_descriptor_sets(
            &[
                ds_param
                    .binding_at(0)
                    .write(br::DescriptorContents::uniform_buffer(
                        &param_buffer,
                        0..core::mem::size_of::<GridParams>() as _,
                    )),
                ds_sprite_instance.binding_at(0).write(
                    br::DescriptorContents::combined_image_sampler(
                        &loaded_sprite_source_atlas.resource,
                        br::ImageLayout::ShaderReadOnlyOpt,
                    ),
                ),
            ],
            &[],
        );

        Self {
            _sprite_sampler: sprite_sampler,
            param_buffer,
            _param_buffer_memory: param_buffer_memory,
            bg_vertex_buffer,
            _bg_vertex_buffer_memory: bg_vertex_buffer_memory,
            current_params_data: GridParams {
                offset: [0.0, 0.0],
                size: [64.0, 64.0],
            },
            param_is_dirty: true,
            atlas_size: init_atlas_size,
            bg_vertex_buffer_is_dirty: true,
            _dsl_param: dsl_param,
            _dsl_sprite_instance: dsl_sprite_instance,
            _dp: dp,
            ds_param,
            ds_sprite_instance,
            grid_vsh: vsh,
            grid_fsh: fsh,
            bg_vsh,
            bg_fsh,
            render_pipeline_layout,
            render_pipeline,
            bg_render_pipeline,
            loaded_sprite_source_atlas: RefCell::new(loaded_sprite_source_atlas),
            sprite_instance_buffers: RefCell::new(sprite_instance_buffers),
            sprite_atlas_rect_by_path: RefCell::new(HashMap::new()),
            sprite_instance_vsh,
            sprite_instance_fsh,
            sprite_instance_render_pipeline_layout,
            sprite_instance_render_pipeline,
            sprite_count: Cell::new(0),
            sprite_image_copies: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn update_sprite_offset(&self, index: usize, left_pixels: f32, top_pixels: f32) {
        let mut buffers_mref = self.sprite_instance_buffers.borrow_mut();

        let h = buffers_mref.stg_memory.native_ptr();
        let cap = buffers_mref.capacity;
        let p = buffers_mref
            .stg_memory
            .map(0..(cap as usize * core::mem::size_of::<SpriteInstance>()))
            .unwrap();
        self.sprite_image_copies.write().clear();
        unsafe {
            let instance_ptr =
                p.addr_of_mut::<SpriteInstance>(index * core::mem::size_of::<SpriteInstance>());
            core::ptr::addr_of_mut!((*instance_ptr).pos_st[2]).write(left_pixels);
            core::ptr::addr_of_mut!((*instance_ptr).pos_st[3]).write(top_pixels);
        }
        if buffers_mref.stg_requires_flush {
            unsafe {
                buffers_mref
                    .subsystem
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                        h,
                        0,
                        cap * core::mem::size_of::<SpriteInstance>() as u64,
                    )])
                    .unwrap();
            }
        }
        unsafe {
            buffers_mref.stg_memory.unmap();
        }

        buffers_mref.is_dirty = true;
    }

    #[tracing::instrument(skip(self, sprites, bg_worker_access, staging_scratch_buffers))]
    pub fn update_sprites(
        &self,
        sprites: &[SpriteInfo],
        bg_worker_access: &BackgroundWorkerEnqueueAccess<'d>,
        staging_scratch_buffers: &std::sync::Weak<RwLock<BufferedStagingScratchBuffer<'d>>>,
    ) {
        let mut buffers_mref = self.sprite_instance_buffers.borrow_mut();
        let mut rects_mref = self.sprite_atlas_rect_by_path.borrow_mut();
        let mut atlas_mref = self.loaded_sprite_source_atlas.borrow_mut();

        buffers_mref.require_capacity(sprites.len() as _);
        if !sprites.is_empty() {
            let h = buffers_mref.stg_memory.native_ptr();
            let p = buffers_mref
                .stg_memory
                .map(0..sprites.len() * core::mem::size_of::<SpriteInstance>())
                .unwrap();
            self.sprite_image_copies.write().clear();
            for (n, x) in sprites.iter().enumerate() {
                let (ox, oy) = match rects_mref.entry(x.source_path.clone()) {
                    std::collections::hash_map::Entry::Occupied(o) => {
                        let &(ox, oy, _, _) = o.get();

                        (ox, oy)
                    }
                    std::collections::hash_map::Entry::Vacant(v) => {
                        let Some((ox, oy)) = atlas_mref.alloc(x.width, x.height) else {
                            tracing::error!(path = ?x.source_path, width = x.width, height = x.height, "no space for sprite(TODO: add page or resize atlas...)");
                            continue;
                        };
                        v.insert((ox, oy, x.width, x.height));

                        bg_worker_access.enqueue(BackgroundWork::LoadSpriteSource(
                            x.source_path.clone(),
                            Box::new({
                                let sprite_image_copies = Arc::downgrade(&self.sprite_image_copies);
                                let staging_scratch_buffers = staging_scratch_buffers.clone();
                                let &SpriteInfo { width, height, .. } = x;

                                move |path, di| {
                                    let Some(sprite_image_copies) = sprite_image_copies.upgrade()
                                    else {
                                        // component teardown-ed
                                        return;
                                    };
                                    let Some(staging_scratch_buffers) =
                                        staging_scratch_buffers.upgrade()
                                    else {
                                        // app teardown-ed
                                        return;
                                    };

                                    // TODO: hdr
                                    let img_formatted = di.to_rgba8();
                                    let img_bytes = img_formatted.as_bytes();

                                    let mut staging_scratch_buffer =
                                        parking_lot::RwLockWriteGuard::map(
                                            staging_scratch_buffers.write(),
                                            |x| x.active_buffer_mut(),
                                        );
                                    let mut copies_locked = sprite_image_copies.write();
                                    let r = staging_scratch_buffer.reserve(img_bytes.len() as _);
                                    let p = staging_scratch_buffer
                                        .map(&r, StagingScratchBufferMapMode::Write)
                                        .unwrap();
                                    unsafe {
                                        p.addr_of_mut::<u8>(0).copy_from_nonoverlapping(
                                            img_bytes.as_ptr(),
                                            img_bytes.len(),
                                        );
                                    }
                                    drop(p);
                                    let (bx, o) = staging_scratch_buffer.of_index(&r);
                                    copies_locked.entry(bx).or_insert_with(Vec::new).push(
                                        br::vk::VkBufferImageCopy {
                                            bufferOffset: o,
                                            bufferRowLength: img_formatted.width(),
                                            bufferImageHeight: img_formatted.height(),
                                            imageSubresource: br::ImageSubresourceLayers::new(
                                                br::AspectMask::COLOR,
                                                0,
                                                0..1,
                                            ),
                                            imageOffset: br::Offset3D::new(ox as _, oy as _, 0),
                                            imageExtent: br::Extent3D::new(width, height, 1),
                                        },
                                    );

                                    tracing::info!(?path, ox, oy, "LoadSpriteComplete");
                                }
                            }),
                        ));

                        (ox, oy)
                    }
                };

                unsafe {
                    let instance_ptr =
                        p.addr_of_mut::<SpriteInstance>(n * core::mem::size_of::<SpriteInstance>());
                    core::ptr::addr_of_mut!((*instance_ptr).pos_st).write([
                        x.width as f32,
                        x.height as f32,
                        x.left as f32,
                        x.top as f32,
                    ]);
                    core::ptr::addr_of_mut!((*instance_ptr).uv_st).write([
                        x.width as f32 / LoadedSpriteSourceAtlas::SIZE as f32,
                        x.height as f32 / LoadedSpriteSourceAtlas::SIZE as f32,
                        ox as f32 / LoadedSpriteSourceAtlas::SIZE as f32,
                        oy as f32 / LoadedSpriteSourceAtlas::SIZE as f32,
                    ]);
                }
            }
            if buffers_mref.stg_requires_flush {
                unsafe {
                    buffers_mref
                        .subsystem
                        .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                            h,
                            0,
                            (sprites.len() * core::mem::size_of::<SpriteInstance>()) as _,
                        )])
                        .unwrap();
                }
            }
            unsafe {
                buffers_mref.stg_memory.unmap();
            }

            buffers_mref.is_dirty = true;
        }

        self.sprite_count.set(sprites.len());
    }

    pub const fn offset(&self) -> [f32; 2] {
        self.current_params_data.offset
    }

    pub fn set_offset(&mut self, x: f32, y: f32) {
        self.current_params_data.offset = [x, y];
        self.param_is_dirty = true;
    }

    pub fn set_atlas_size(&mut self, size: SizePixels) {
        self.atlas_size = size;
        self.bg_vertex_buffer_is_dirty = true;
    }

    pub fn is_dirty(&self) -> bool {
        self.param_is_dirty
            || self.bg_vertex_buffer_is_dirty
            || self.sprite_instance_buffers.borrow().is_dirty
            || !self.sprite_image_copies.read().is_empty()
    }

    pub fn process_dirty_data<'c, E>(
        &mut self,
        staging_scratch_buffer: &StagingScratchBufferManager<'d>,
        rec: br::CmdRecord<'c, E>,
    ) -> br::CmdRecord<'c, E> {
        if !self.is_dirty() {
            return rec;
        }

        self.param_is_dirty = false;
        self.bg_vertex_buffer_is_dirty = false;
        let mut loaded_sprite_atlas_image_barrier_needed = false;
        rec.update_buffer(
            &self.param_buffer,
            0,
            core::mem::size_of::<GridParams>() as _,
            &self.current_params_data,
        )
        .update_buffer(
            &self.bg_vertex_buffer,
            0,
            core::mem::size_of::<[[f32; 4]; 4]>() as _,
            &[
                [0.0f32, 0.0, 0.0, 1.0],
                [self.atlas_size.width as f32, 0.0, 0.0, 1.0],
                [0.0f32, self.atlas_size.height as f32, 0.0, 1.0],
                [
                    self.atlas_size.width as f32,
                    self.atlas_size.height as f32,
                    0.0,
                    1.0,
                ],
            ],
        )
        .inject(|r| {
            let buffers_mref = self.sprite_instance_buffers.get_mut();
            if !buffers_mref.is_dirty {
                return r;
            }
            buffers_mref.is_dirty = false;

            r.copy_buffer(
                &buffers_mref.stg_buffer,
                &buffers_mref.buffer,
                &[br::BufferCopy::mirror(
                    0,
                    (self.sprite_count.get() * core::mem::size_of::<SpriteInstance>()) as _,
                )],
            )
        })
        .inject(|r| {
            let atlas_ref = self.loaded_sprite_source_atlas.borrow();
            let mut copies_mref = self.sprite_image_copies.write();
            if copies_mref.is_empty() {
                // no copies needed
                return r;
            }

            loaded_sprite_atlas_image_barrier_needed = true;
            copies_mref.drain().fold(
                r.pipeline_barrier_2(&br::DependencyInfo::new(
                    &[],
                    &[],
                    &[br::ImageMemoryBarrier2::new(
                        atlas_ref.resource.image(),
                        br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
                    )
                    .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
                )),
                |r, (bi, cps)| {
                    r.copy_buffer_to_image(
                        staging_scratch_buffer.buffer_of(bi),
                        atlas_ref.resource.image(),
                        br::ImageLayout::TransferDestOpt,
                        &cps,
                    )
                },
            )
        })
        .inject(|r| {
            let atlas_ref = self.loaded_sprite_source_atlas.borrow();
            let mut image_memory_barriers = Vec::with_capacity(8);
            if loaded_sprite_atlas_image_barrier_needed {
                image_memory_barriers.push(
                    br::ImageMemoryBarrier2::new(
                        atlas_ref.resource.image(),
                        br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
                    )
                    .transit_from(
                        br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt),
                    )
                    .from(
                        br::PipelineStageFlags2::COPY,
                        br::AccessFlags2::TRANSFER.write,
                    )
                    .to(
                        br::PipelineStageFlags2::FRAGMENT_SHADER,
                        br::AccessFlags2::SHADER.read,
                    ),
                );
            }

            r.pipeline_barrier_2(&br::DependencyInfo::new(
                &[br::MemoryBarrier2::new()
                    .from(
                        br::PipelineStageFlags2::COPY,
                        br::AccessFlags2::TRANSFER.write,
                    )
                    .to(
                        br::PipelineStageFlags2::FRAGMENT_SHADER
                            | br::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
                        br::AccessFlags2::SHADER.read | br::AccessFlags2::VERTEX_ATTRIBUTE_READ,
                    )],
                &[],
                &image_memory_barriers,
            ))
        })
    }

    pub fn recreate(
        &mut self,
        device: &'d Subsystem,
        rendered_pass: br::SubpassRef<impl br::RenderPass>,
        main_buffer_size: br::Extent2D,
    ) {
        let [
            render_pipeline,
            bg_render_pipeline,
            sprite_instance_render_pipeline,
        ] = device
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    &self.render_pipeline_layout,
                    rendered_pass,
                    &[
                        self.grid_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        self.grid_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[main_buffer_size
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[main_buffer_size.into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &self.render_pipeline_layout,
                    rendered_pass,
                    &[
                        self.bg_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        self.bg_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_FLOAT4_ONLY,
                    IA_STATE_TRISTRIP,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[main_buffer_size
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[main_buffer_size.into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &self.sprite_instance_render_pipeline_layout,
                    rendered_pass,
                    &[
                        self.sprite_instance_vsh
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        self.sprite_instance_fsh
                            .on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    &br::PipelineVertexInputStateCreateInfo::new(
                        &[br::VertexInputBindingDescription::per_instance_typed::<
                            [f32; 8],
                        >(0)],
                        &[
                            br::VertexInputAttributeDescription {
                                location: 0,
                                binding: 0,
                                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                                offset: 0,
                            },
                            br::VertexInputAttributeDescription {
                                location: 1,
                                binding: 0,
                                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                                offset: (core::mem::size_of::<f32>() * 4) as _,
                            },
                        ],
                    ),
                    IA_STATE_TRISTRIP,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[main_buffer_size
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[main_buffer_size.into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_PREMULTIPLIED,
                )
                .multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        self.render_pipeline = render_pipeline;
        self.bg_render_pipeline = bg_render_pipeline;
        self.sprite_instance_render_pipeline = sprite_instance_render_pipeline;
    }

    pub fn render_commands<'cb, E>(
        &self,
        sc_size: br::Extent2D,
        rec: br::CmdRecord<'cb, E>,
    ) -> br::CmdRecord<'cb, E> {
        rec.bind_pipeline(br::PipelineBindPoint::Graphics, &self.render_pipeline)
            .push_constant(
                &self.render_pipeline_layout,
                br::vk::VK_SHADER_STAGE_FRAGMENT_BIT | br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                0,
                &[sc_size.width as f32, sc_size.height as f32],
            )
            .bind_descriptor_sets(
                br::PipelineBindPoint::Graphics,
                &self.render_pipeline_layout,
                0,
                &[self.ds_param],
                &[],
            )
            .draw(3, 1, 0, 0)
            .bind_pipeline(br::PipelineBindPoint::Graphics, &self.bg_render_pipeline)
            .bind_vertex_buffer_array(0, &[self.bg_vertex_buffer.as_transparent_ref()], &[0])
            .draw(4, 1, 0, 0)
            .inject(|r| {
                let inst_count = self.sprite_count.get();

                if inst_count <= 0 {
                    // no sprites drawn
                    return r;
                }

                r.bind_pipeline(
                    br::PipelineBindPoint::Graphics,
                    &self.sprite_instance_render_pipeline,
                )
                .bind_descriptor_sets(
                    br::PipelineBindPoint::Graphics,
                    &self.sprite_instance_render_pipeline_layout,
                    0,
                    &[self.ds_param, self.ds_sprite_instance],
                    &[],
                )
                .push_constant(
                    &self.sprite_instance_render_pipeline_layout,
                    br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                    &[sc_size.width as f32, sc_size.height as f32],
                )
                .bind_vertex_buffer_array(
                    0,
                    &[self
                        .sprite_instance_buffers
                        .borrow()
                        .buffer
                        .as_transparent_ref()],
                    &[0],
                )
                .draw(4, inst_count as _, 0, 0)
            })
    }
}
