use std::path::Path;

use crate::{
    composite::{
        AtlasRect, CompositeInstanceManager, CompositeRect, CompositeTree, CompositeTreeRef,
        CompositionSurfaceAtlas, UnboundedCompositeInstanceManager,
        UnboundedCompositionSurfaceAtlas,
    },
    hittest::{HitTestTreeData, HitTestTreeManager, HitTestTreeRef},
    subsystem::{Subsystem, SubsystemShaderModuleRef},
};

use bedrock::{self as br, CommandBufferMut};

pub struct FontSet {
    pub ui_default: freetype::Owned<freetype::Face>,
}

pub struct AppBaseSystem<'subsystem> {
    pub subsystem: &'subsystem Subsystem,
    pub atlas: UnboundedCompositionSurfaceAtlas,
    pub composite_tree: CompositeTree,
    pub composite_instance_manager: UnboundedCompositeInstanceManager,
    pub hit_tree: HitTestTreeManager<'subsystem>,
    pub fonts: FontSet,
}
impl Drop for AppBaseSystem<'_> {
    fn drop(&mut self) {
        unsafe {
            self.composite_instance_manager
                .drop_with_gfx_device(&self.subsystem);
            self.atlas.drop_with_gfx_device(&self.subsystem);
        }
    }
}
impl<'subsystem> AppBaseSystem<'subsystem> {
    pub fn new(subsystem: &'subsystem Subsystem) -> Self {
        // initialize font systems
        #[cfg(unix)]
        fontconfig::init();

        let (primary_face_path, primary_face_index);
        #[cfg(unix)]
        {
            let mut fc_pat = fontconfig::Pattern::new();
            fc_pat.add_family_name(c"system-ui");
            fc_pat.add_weight(80);
            fontconfig::Config::current()
                .unwrap()
                .substitute(&mut fc_pat, fontconfig::MatchKind::Pattern);
            fc_pat.default_substitute();
            let fc_set = fontconfig::Config::current()
                .unwrap()
                .sort(&mut fc_pat, true)
                .unwrap();
            let mut primary_face_info = None;
            for &f in fc_set.fonts() {
                let file_path = f.get_file_path(0).unwrap();
                let index = f.get_face_index(0).unwrap();

                tracing::debug!(?file_path, index, "match font");

                if primary_face_info.is_none() {
                    primary_face_info = Some((file_path.to_owned(), index));
                }
            }
            let Some((path, index)) = primary_face_info else {
                tracing::error!("No UI face found");
                std::process::exit(1);
            };
            primary_face_path = path;
            primary_face_index = index;
        }
        // TODO: mock
        #[cfg(windows)]
        {
            primary_face_path = c"C:\\Windows\\Fonts\\YuGothR.ttc";
            primary_face_index = 0;
        }

        let ft_face = match subsystem
            .ft
            .write()
            .new_face(&primary_face_path, primary_face_index as _)
        {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create ft face");
                std::process::exit(1);
            }
        };

        let composite_instance_buffer = CompositeInstanceManager::new(subsystem);
        let composition_alphamask_surface_atlas = CompositionSurfaceAtlas::new(
            subsystem,
            subsystem.adapter_properties.limits.maxImageDimension2D,
            br::vk::VK_FORMAT_R8_UNORM,
        );

        Self {
            atlas: composition_alphamask_surface_atlas.unbound(),
            composite_tree: CompositeTree::new(),
            composite_instance_manager: composite_instance_buffer.unbound(),
            hit_tree: HitTestTreeManager::new(),
            fonts: FontSet {
                ui_default: ft_face,
            },
            subsystem,
        }
    }

    pub fn rescale_fonts(&mut self, scale: f32) {
        if let Err(e) =
            self.fonts
                .ui_default
                .set_char_size((10.0 * 64.0) as _, 0, (96.0 * scale) as _, 0)
        {
            tracing::warn!(reason = ?e, "Failed to set char size");
        }
    }

    pub const fn mask_atlas_format(&self) -> br::Format {
        self.atlas.format()
    }

    pub const fn mask_atlas_size(&self) -> u32 {
        self.atlas.size()
    }

    pub const fn mask_atlas_resource_transparent_ref(
        &self,
    ) -> &br::VkHandleRef<br::vk::VkImageView> {
        self.atlas.resource_view_transparent_ref()
    }

    pub const fn mask_atlas_image_transparent_ref(&self) -> &br::VkHandleRef<br::vk::VkImage> {
        self.atlas.image_transparent_ref()
    }

    #[inline(always)]
    pub fn alloc_mask_atlas_rect(
        &mut self,
        required_width: u32,
        required_height: u32,
    ) -> AtlasRect {
        self.atlas.alloc(required_width, required_height)
    }

    #[inline(always)]
    pub fn barrier_for_mask_atlas_resource(&self) -> br::ImageMemoryBarrier2 {
        br::ImageMemoryBarrier2::new(
            self.atlas.image_transparent_ref(),
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
    }

    #[inline(always)]
    pub fn register_composite_rect(&mut self, rect: CompositeRect) -> CompositeTreeRef {
        self.composite_tree.register(rect)
    }

    #[inline(always)]
    pub fn set_composite_tree_parent(&mut self, child: CompositeTreeRef, parent: CompositeTreeRef) {
        self.composite_tree.add_child(parent, child);
    }

    #[inline(always)]
    pub fn create_hit_tree(&mut self, init: HitTestTreeData<'subsystem>) -> HitTestTreeRef {
        self.hit_tree.create(init)
    }

    #[inline(always)]
    pub fn set_hit_tree_parent(&mut self, child: HitTestTreeRef, parent: HitTestTreeRef) {
        self.hit_tree.add_child(parent, child);
    }

    #[inline]
    pub fn set_tree_parent(
        &mut self,
        child: (CompositeTreeRef, HitTestTreeRef),
        parent: (CompositeTreeRef, HitTestTreeRef),
    ) {
        self.set_composite_tree_parent(child.0, parent.0);
        self.set_hit_tree_parent(child.1, parent.1);
    }

    #[inline(always)]
    pub fn find_direct_memory_index(&self, index_mask: u32) -> Option<u32> {
        self.subsystem.find_direct_memory_index(index_mask)
    }

    #[inline(always)]
    pub fn find_device_local_memory_index(&self, index_mask: u32) -> Option<u32> {
        self.subsystem.find_device_local_memory_index(index_mask)
    }

    #[inline(always)]
    pub fn find_host_visible_memory_index(&self, index_mask: u32) -> Option<u32> {
        self.subsystem.find_host_visible_memory_index(index_mask)
    }

    #[inline(always)]
    pub fn is_coherent_memory_type(&self, index: u32) -> bool {
        self.subsystem.is_coherent_memory_type(index)
    }

    #[inline(always)]
    pub fn require_shader(&self, path: impl AsRef<Path>) -> SubsystemShaderModuleRef<'subsystem> {
        self.subsystem.require_shader(path)
    }

    #[inline(always)]
    pub fn require_empty_pipeline_layout(
        &self,
    ) -> &impl br::VkHandle<Handle = br::vk::VkPipelineLayout> {
        self.subsystem.require_empty_pipeline_layout()
    }

    #[inline(always)]
    pub fn create_graphics_pipelines_array<const N: usize>(
        &self,
        create_info_array: &[br::GraphicsPipelineCreateInfo; N],
    ) -> br::Result<[br::PipelineObject<&'subsystem Subsystem>; N]> {
        self.subsystem
            .create_graphics_pipelines_array(create_info_array)
    }

    #[inline(always)]
    pub fn create_transient_graphics_command_pool(&self) -> br::Result<impl br::CommandPoolMut> {
        self.subsystem.create_transient_graphics_command_pool()
    }

    pub fn sync_execute_graphics_commands(
        &self,
        rec: impl for<'e> FnOnce(br::CmdRecord<'e, Subsystem>) -> br::CmdRecord<'e, Subsystem>,
    ) -> br::Result<()> {
        let mut cp = self.create_transient_graphics_command_pool()?;
        let [mut cb] = br::CommandBufferObject::alloc_array(
            self.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )?;
        rec(unsafe {
            cb.begin(
                &br::CommandBufferBeginInfo::new().onetime_submit(),
                self.subsystem,
            )?
        })
        .end()?;

        self.sync_execute_graphics_command_buffers(&[br::CommandBufferSubmitInfo::new(&cb)])
    }

    #[inline(always)]
    pub fn sync_execute_graphics_command_buffers(
        &self,
        buffers: &[br::CommandBufferSubmitInfo],
    ) -> br::Result<()> {
        self.subsystem.sync_execute_graphics_commands(buffers)
    }

    #[tracing::instrument(skip(self), fields(memory_type_index))]
    pub fn alloc_device_local_memory(
        &self,
        size: br::DeviceSize,
        memory_type_index_mask: u32,
    ) -> br::DeviceMemoryObject<&'subsystem Subsystem> {
        let Some(memindex) = self.find_device_local_memory_index(memory_type_index_mask) else {
            tracing::error!("no suitable memory");
            std::process::exit(1);
        };
        tracing::Span::current().record("memory_type_index", memindex);

        match br::DeviceMemoryObject::new(
            self.subsystem,
            &br::MemoryAllocateInfo::new(size, memindex),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to allocate device local memory");
                std::process::exit(1);
            }
        }
    }

    #[inline(always)]
    pub fn alloc_device_local_memory_for_requirements(
        &self,
        req: &br::vk::VkMemoryRequirements,
    ) -> br::DeviceMemoryObject<&'subsystem Subsystem> {
        self.alloc_device_local_memory(req.size, req.memoryTypeBits)
    }
}
