use bedrock::{self as br, RenderPass, ShaderModule, VkHandle};
use std::{cell::Cell, rc::Rc};

use crate::{
    AppEvent, AppUpdateContext, BLEND_STATE_SINGLE_NONE, FillcolorRConstants, IA_STATE_TRILIST,
    MS_STATE_EMPTY, PresenterInitContext, RASTER_STATE_DEFAULT_FILL_NOCULL, VI_STATE_FLOAT2_ONLY,
    ViewInitContext,
    base_system::{
        AppBaseSystem, BufferMapMode, FontType, MemoryBoundBuffer, PixelFormat, RenderPassOptions,
        RenderTexture, RenderTextureFlags, RenderTextureOptions,
        scratch_buffer::{StagingScratchBufferManager, StagingScratchBufferMapMode},
    },
    composite::{
        AnimatableColor, AnimatableFloat, AnimationCurve, AtlasRect, CompositeMode, CompositeRect,
        CompositeTree, CompositeTreeRef,
    },
    hittest::{HitTestTreeActionHandler, HitTestTreeData, HitTestTreeRef, PointerActionArgs, Role},
    input::EventContinueControl,
};

#[derive(Debug, Clone, Copy)]
enum SystemCommand {
    Close,
    Minimize,
    Maximize,
    Restore,
}

struct SystemCommandButtonView {
    ct_root: CompositeTreeRef,
    ct_icon: CompositeTreeRef,
    ct_hover: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    icon_atlas_rect: Cell<AtlasRect>,
    hovering: Cell<bool>,
    pressing: Cell<bool>,
    is_dirty: Cell<bool>,
    cmd: Cell<SystemCommand>,
}
impl SystemCommandButtonView {
    const ICON_SIZE: f32 = 10.0;
    const WIDTH: f32 = 48.0;

    const CLOSE_ICON_VERTICES: &'static [[f32; 2]] = &[
        [0.0 + 0.5 / Self::ICON_SIZE, 0.0 - 0.5 / Self::ICON_SIZE],
        [0.0 - 0.5 / Self::ICON_SIZE, 0.0 + 0.5 / Self::ICON_SIZE],
        [1.0 - 0.5 / Self::ICON_SIZE, 1.0 + 0.5 / Self::ICON_SIZE],
        [1.0 + 0.5 / Self::ICON_SIZE, 1.0 - 0.5 / Self::ICON_SIZE],
        [1.0 + 0.5 / Self::ICON_SIZE, 0.0 + 0.5 / Self::ICON_SIZE],
        [1.0 - 0.5 / Self::ICON_SIZE, 0.0 - 0.5 / Self::ICON_SIZE],
        [0.0 - 0.5 / Self::ICON_SIZE, 1.0 - 0.5 / Self::ICON_SIZE],
        [0.0 + 0.5 / Self::ICON_SIZE, 1.0 + 0.5 / Self::ICON_SIZE],
    ];
    const CLOSE_ICON_INDICES: &'static [u16] = &[0, 1, 2, 2, 3, 0, 4, 5, 6, 6, 7, 4];

    const MINIMIZE_ICON_VERTICES: &'static [[f32; 2]] = &[
        [0.0, 1.0 - 1.5 / Self::ICON_SIZE],
        [0.0, 1.0],
        [1.0, 1.0],
        [1.0, 1.0 - 1.5 / Self::ICON_SIZE],
    ];
    const MINIMIZE_ICON_INDICES: &'static [u16] = &[0, 1, 2, 2, 3, 0];

    const MAXIMIZE_ICON_VERTICES: &'static [[f32; 2]] = &[
        [0.0, 0.0],
        [0.0 + 1.5 / Self::ICON_SIZE, 0.0 + 1.5 / Self::ICON_SIZE],
        [1.0, 0.0],
        [1.0 - 1.5 / Self::ICON_SIZE, 0.0 + 1.5 / Self::ICON_SIZE],
        [1.0, 1.0],
        [1.0 - 1.5 / Self::ICON_SIZE, 1.0 - 1.5 / Self::ICON_SIZE],
        [0.0, 1.0],
        [0.0 + 1.5 / Self::ICON_SIZE, 1.0 - 1.5 / Self::ICON_SIZE],
    ];
    const MAXIMIZE_ICON_INDICES: &'static [u16] = &[
        0, 2, 3, 3, 1, 0, 2, 4, 5, 5, 3, 2, 4, 6, 7, 7, 5, 4, 6, 0, 1, 1, 7, 6,
    ];

    const fn select_vertices_indices(cmd: SystemCommand) -> (&'static [[f32; 2]], &'static [u16]) {
        match cmd {
            SystemCommand::Close => (Self::CLOSE_ICON_VERTICES, Self::CLOSE_ICON_INDICES),
            SystemCommand::Minimize => (Self::MINIMIZE_ICON_VERTICES, Self::MINIMIZE_ICON_INDICES),
            SystemCommand::Maximize => (Self::MAXIMIZE_ICON_VERTICES, Self::MAXIMIZE_ICON_INDICES),
            SystemCommand::Restore => unimplemented!(),
        }
    }

    fn render_icon(base_system: &AppBaseSystem, cmd: SystemCommand, atlas_rect: &AtlasRect) {
        let (vertices, indices) = Self::select_vertices_indices(cmd);
        let indices_offset = core::mem::size_of::<[f32; 2]>() * vertices.len();
        let bufsize = indices_offset + core::mem::size_of::<u16>() * indices.len();
        let mut buf = MemoryBoundBuffer::new_writable(
            base_system,
            bufsize,
            br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::INDEX_BUFFER,
        )
        .unwrap();
        let p = buf.map(0..bufsize, BufferMapMode::Write).unwrap();
        unsafe {
            p.addr_of_mut::<[f32; 2]>(0)
                .copy_from_nonoverlapping(vertices.as_ptr(), vertices.len());
            p.addr_of_mut::<u16>(indices_offset)
                .copy_from_nonoverlapping(indices.as_ptr(), indices.len());
        }
        p.unmap().unwrap();

        let icon_msaa_buf = RenderTexture::new(
            base_system,
            atlas_rect.extent(),
            PixelFormat::R8,
            &RenderTextureOptions {
                msaa_count: Some(4),
                flags: RenderTextureFlags::ALLOW_TRANSFER_SRC | RenderTextureFlags::NON_SAMPLED,
            },
        )
        .unwrap();

        let rp = br::RenderPassObject::new(
            base_system.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[icon_msaa_buf
                    .make_attachment_description()
                    .color_memory_op(br::LoadOp::Clear, br::StoreOp::Store)
                    .layout_transition(
                        br::ImageLayout::Undefined,
                        br::ImageLayout::TransferSrcOpt,
                    )],
                &[br::SubpassDescription2::new()
                    .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
                &[br::SubpassDependency2::new(
                    br::SubpassIndex::Internal(0),
                    br::SubpassIndex::External,
                )
                .of_memory(
                    br::AccessFlags::COLOR_ATTACHMENT.write,
                    br::AccessFlags::TRANSFER.read,
                )
                .of_execution(
                    br::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                    br::PipelineStageFlags::TRANSFER,
                )],
            ),
        )
        .unwrap();
        let fb = br::FramebufferObject::new(
            base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp,
                &[icon_msaa_buf.as_transparent_ref()],
                atlas_rect.width(),
                atlas_rect.height(),
            ),
        )
        .unwrap();

        let vsh = base_system.require_shader("resources/normalized_01_2d.vert");
        let fsh = base_system.require_shader("resources/fillcolor_r.frag");
        let [pipeline] = base_system
            .create_graphics_pipelines_array(&[br::GraphicsPipelineCreateInfo::new(
                base_system.require_empty_pipeline_layout(),
                rp.subpass(0),
                &[
                    vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                    fsh.on_stage(br::ShaderStage::Fragment, c"main")
                        .with_specialization_info(&br::SpecializationInfo::new(
                            &FillcolorRConstants { r: 1.0 },
                        )),
                ],
                VI_STATE_FLOAT2_ONLY,
                IA_STATE_TRILIST,
                &br::PipelineViewportStateCreateInfo::new_array(
                    &[atlas_rect
                        .extent()
                        .into_rect(br::Offset2D::ZERO)
                        .make_viewport(0.0..1.0)],
                    &[atlas_rect.extent().into_rect(br::Offset2D::ZERO)],
                ),
                RASTER_STATE_DEFAULT_FILL_NOCULL,
                BLEND_STATE_SINGLE_NONE,
            )
            .set_multisample_state(
                &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
            )])
            .unwrap();

        base_system
            .sync_execute_graphics_commands(|rec| {
                rec.begin_render_pass2(
                    &br::RenderPassBeginInfo::new(
                        &rp,
                        &fb,
                        icon_msaa_buf.render_region(),
                        &[br::ClearValue::color_f32([0.0; 4])],
                    ),
                    &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                )
                .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
                .bind_vertex_buffer_array(0, &[buf.as_transparent_ref()], &[0])
                .bind_index_buffer(&buf, indices_offset, br::IndexType::U16)
                .draw_indexed(indices.len() as _, 1, 0, 0, 0)
                .end_render_pass2(&br::SubpassEndInfo::new())
                .pipeline_barrier_2(&br::DependencyInfo::new(
                    &[],
                    &[],
                    &[base_system
                        .barrier_for_mask_atlas_resource()
                        .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
                ))
                .resolve_image(
                    icon_msaa_buf.as_image(),
                    br::ImageLayout::TransferSrcOpt,
                    base_system.mask_atlas_image_transparent_ref(),
                    br::ImageLayout::TransferDestOpt,
                    &[br::vk::VkImageResolve {
                        srcSubresource: br::ImageSubresourceLayers::new(
                            br::AspectMask::COLOR,
                            0,
                            0..1,
                        ),
                        srcOffset: br::Offset3D::ZERO,
                        dstSubresource: br::ImageSubresourceLayers::new(
                            br::AspectMask::COLOR,
                            0,
                            0..1,
                        ),
                        dstOffset: atlas_rect.lt_offset().with_z(0),
                        extent: atlas_rect.extent().with_depth(1),
                    }],
                )
                .pipeline_barrier_2(&br::DependencyInfo::new(
                    &[],
                    &[],
                    &[base_system
                        .barrier_for_mask_atlas_resource()
                        .transferring_layout(
                            br::ImageLayout::TransferDestOpt,
                            br::ImageLayout::ShaderReadOnlyOpt,
                        )
                        .from(
                            br::PipelineStageFlags2::RESOLVE,
                            br::AccessFlags2::TRANSFER.write,
                        )
                        .to(
                            br::PipelineStageFlags2::FRAGMENT_SHADER,
                            br::AccessFlags2::SHADER_SAMPLED_READ,
                        )],
                ))
            })
            .unwrap();
    }

    fn new(init: &mut ViewInitContext, right_offset: f32, init_cmd: SystemCommand) -> Self {
        let icon_size_px = (Self::ICON_SIZE * init.ui_scale_factor).trunc() as u32;
        let icon_atlas_rect = init
            .base_system
            .alloc_mask_atlas_rect(icon_size_px, icon_size_px);
        Self::render_icon(init.base_system, init_cmd, &icon_atlas_rect);

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            relative_offset_adjustment: [1.0, 0.0],
            offset: [
                AnimatableFloat::Value(-right_offset - Self::WIDTH),
                AnimatableFloat::Value(0.0),
            ],
            relative_size_adjustment: [0.0, 1.0],
            size: [
                AnimatableFloat::Value(Self::WIDTH),
                AnimatableFloat::Value(0.0),
            ],
            ..Default::default()
        });
        let ct_hover = init.base_system.register_composite_rect(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            has_bitmap: true,
            composite_mode: CompositeMode::FillColor(AnimatableColor::Value(match init_cmd {
                SystemCommand::Close => [1.0, 0.0, 0.0, 1.0],
                _ => [1.0, 1.0, 1.0, 0.5],
            })),
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });
        let ct_icon = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            offset: [
                AnimatableFloat::Value(-Self::ICON_SIZE * 0.5),
                AnimatableFloat::Value(-Self::ICON_SIZE * 0.5),
            ],
            relative_offset_adjustment: [0.5, 0.5],
            size: [
                AnimatableFloat::Value(Self::ICON_SIZE),
                AnimatableFloat::Value(Self::ICON_SIZE),
            ],
            has_bitmap: true,
            texatlas_rect: icon_atlas_rect.clone(),
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        init.base_system
            .set_composite_tree_parent(ct_hover, ct_root);
        init.base_system.set_composite_tree_parent(ct_icon, ct_root);

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            left: -right_offset - Self::WIDTH,
            left_adjustment_factor: 1.0,
            width: Self::WIDTH,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });

        Self {
            ct_root,
            ct_icon,
            ct_hover,
            ht_root,
            icon_atlas_rect: Cell::new(icon_atlas_rect),
            hovering: Cell::new(false),
            pressing: Cell::new(false),
            is_dirty: Cell::new(false),
            cmd: Cell::new(init_cmd),
        }
    }

    fn mount(
        &self,
        base_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        base_system.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    fn rescale(&self, base_system: &mut AppBaseSystem, ui_scale_factor: f32) {
        base_system.free_mask_atlas_rect(self.icon_atlas_rect.get());
        let icon_size_px = (Self::ICON_SIZE * ui_scale_factor).trunc() as u32;
        self.icon_atlas_rect
            .set(base_system.alloc_mask_atlas_rect(icon_size_px, icon_size_px));
        Self::render_icon(base_system, self.cmd.get(), &self.icon_atlas_rect.get());

        base_system
            .composite_tree
            .get_mut(self.ct_icon)
            .texatlas_rect = self.icon_atlas_rect.get();
        base_system
            .composite_tree
            .get_mut(self.ct_icon)
            .base_scale_factor = ui_scale_factor;
        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .base_scale_factor = ui_scale_factor;
        base_system.composite_tree.mark_dirty(self.ct_icon);
        base_system.composite_tree.mark_dirty(self.ct_root);
    }

    fn update(&self, ct: &mut CompositeTree, current_sec: f32) {
        if self.is_dirty.replace(false) {
            ct.get_mut(self.ct_hover).opacity = if self.hovering.get() {
                AnimatableFloat::Animated {
                    from_value: 0.0,
                    to_value: 1.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.1,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                }
            } else {
                AnimatableFloat::Animated {
                    from_value: 1.0,
                    to_value: 0.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.1,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                }
            };

            ct.mark_dirty(self.ct_hover);
        }
    }

    fn on_hover(&self) {
        self.hovering.set(true);
        self.is_dirty.set(true);
    }

    fn on_leave(&self) {
        self.hovering.set(false);
        self.pressing.set(false);
        self.is_dirty.set(true);
    }
}

struct MenuButtonView {
    ct_root: CompositeTreeRef,
    ct_bg: CompositeTreeRef,
    ct_icon: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    icon_atlas_rect: Cell<AtlasRect>,
    hovering: Cell<bool>,
    pressing: Cell<bool>,
    is_dirty: Cell<bool>,
}
impl MenuButtonView {
    const ICON_VERTICES: &'static [[f32; 2]] = &[
        [-1.0, -1.0],
        [1.0, -1.0],
        [-1.0, -1.0 + 2.0 / 5.0],
        [1.0, -1.0 + 2.0 / 5.0],
        [-1.0, -1.0 + 4.0 / 5.0],
        [1.0, -1.0 + 4.0 / 5.0],
        [-1.0, -1.0 + 6.0 / 5.0],
        [1.0, -1.0 + 6.0 / 5.0],
        [-1.0, -1.0 + 8.0 / 5.0],
        [1.0, -1.0 + 8.0 / 5.0],
        [-1.0, -1.0 + 10.0 / 5.0],
        [1.0, -1.0 + 10.0 / 5.0],
    ];
    const ICON_INDICES: &'static [u16] = &[0, 1, 2, 2, 3, 1, 4, 5, 6, 6, 7, 5, 8, 9, 10, 10, 11, 9];
    const ICON_SIZE: f32 = 10.0;

    fn render_icon(base_system: &AppBaseSystem, atlas_rect: &AtlasRect) {
        let size = core::mem::size_of::<[f32; 2]>() * Self::ICON_VERTICES.len()
            + core::mem::size_of::<u16>() * Self::ICON_INDICES.len();
        let mut vbuf = MemoryBoundBuffer::new_writable(
            base_system,
            size,
            br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::INDEX_BUFFER,
        )
        .unwrap();
        let ptr = vbuf.map(0..size, BufferMapMode::Write).unwrap();
        unsafe {
            ptr.addr_of_mut::<[f32; 2]>(0)
                .copy_from_nonoverlapping(Self::ICON_VERTICES.as_ptr(), Self::ICON_VERTICES.len());
            ptr.addr_of_mut::<u16>(core::mem::size_of::<[f32; 2]>() * Self::ICON_VERTICES.len())
                .copy_from_nonoverlapping(Self::ICON_INDICES.as_ptr(), Self::ICON_INDICES.len());
        }
        ptr.unmap().unwrap();

        let rp = base_system
            .render_to_mask_atlas_pass(RenderPassOptions::empty())
            .unwrap();
        let fb = br::FramebufferObject::new(
            base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp,
                &[base_system
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                base_system.mask_atlas_size(),
                base_system.mask_atlas_size(),
            ),
        )
        .unwrap();

        let [pipeline] = base_system
            .create_graphics_pipelines_array(&[br::GraphicsPipelineCreateInfo::new(
                base_system.require_empty_pipeline_layout(),
                rp.subpass(0),
                &[
                    base_system
                        .require_shader("resources/notrans.vert")
                        .on_stage(br::ShaderStage::Vertex, c"main"),
                    base_system
                        .require_shader("resources/fillcolor_r.frag")
                        .on_stage(br::ShaderStage::Fragment, c"main")
                        .with_specialization_info(&br::SpecializationInfo::new(
                            &FillcolorRConstants { r: 1.0 },
                        )),
                ],
                VI_STATE_FLOAT2_ONLY,
                IA_STATE_TRILIST,
                &br::PipelineViewportStateCreateInfo::new_array(
                    &[atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                    &[atlas_rect.vk_rect()],
                ),
                RASTER_STATE_DEFAULT_FILL_NOCULL,
                BLEND_STATE_SINGLE_NONE,
            )
            .set_multisample_state(MS_STATE_EMPTY)])
            .unwrap();

        base_system
            .sync_execute_graphics_commands(|rec| {
                rec.begin_render_pass2(
                    &br::RenderPassBeginInfo::new(
                        &rp,
                        &fb,
                        atlas_rect.vk_rect(),
                        &[br::ClearValue::color_f32([0.0; 4])],
                    ),
                    &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                )
                .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
                .bind_vertex_buffer_array(0, &[vbuf.as_transparent_ref()], &[0])
                .bind_index_buffer(
                    &vbuf,
                    core::mem::size_of::<[f32; 2]>() * Self::ICON_VERTICES.len(),
                    br::IndexType::U16,
                )
                .draw_indexed(Self::ICON_INDICES.len() as _, 1, 0, 0, 0)
                .end_render_pass2(&br::SubpassEndInfo::new())
            })
            .unwrap();
    }

    #[tracing::instrument(name = "MenuButtonView::new", skip(init))]
    fn new(init: &mut ViewInitContext, height: f32) -> Self {
        let icon_atlas_rect = init.base_system.alloc_mask_atlas_rect(
            (Self::ICON_SIZE * init.ui_scale_factor) as _,
            (Self::ICON_SIZE * init.ui_scale_factor) as _,
        );
        Self::render_icon(init.base_system, &icon_atlas_rect);

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(height),
                AnimatableFloat::Value(height),
            ],
            ..Default::default()
        });
        let ct_bg = init.base_system.register_composite_rect(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            has_bitmap: true,
            composite_mode: CompositeMode::FillColor(AnimatableColor::Value([1.0, 1.0, 1.0, 0.0])),
            ..Default::default()
        });
        let ct_icon = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(Self::ICON_SIZE),
                AnimatableFloat::Value(Self::ICON_SIZE),
            ],
            offset: [
                AnimatableFloat::Value(-Self::ICON_SIZE * 0.5),
                AnimatableFloat::Value(-Self::ICON_SIZE * 0.5),
            ],
            relative_offset_adjustment: [0.5, 0.5],
            has_bitmap: true,
            texatlas_rect: icon_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        init.base_system.set_composite_tree_parent(ct_bg, ct_root);
        init.base_system.set_composite_tree_parent(ct_icon, ct_root);

        let ht_root = init.base_system.hit_tree.create(HitTestTreeData {
            width: height,
            height,
            ..Default::default()
        });

        Self {
            ct_root,
            ct_bg,
            ct_icon,
            ht_root,
            icon_atlas_rect: Cell::new(icon_atlas_rect),
            hovering: Cell::new(false),
            pressing: Cell::new(false),
            is_dirty: Cell::new(false),
        }
    }

    fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        app_system.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    fn rescale(&self, base_system: &mut AppBaseSystem, ui_scale_factor: f32) {
        base_system.free_mask_atlas_rect(self.icon_atlas_rect.get());
        self.icon_atlas_rect.set(base_system.alloc_mask_atlas_rect(
            (Self::ICON_SIZE * ui_scale_factor) as _,
            (Self::ICON_SIZE * ui_scale_factor) as _,
        ));
        Self::render_icon(base_system, &self.icon_atlas_rect.get());

        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .texatlas_rect = self.icon_atlas_rect.get();
        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .base_scale_factor = ui_scale_factor;
        base_system
            .composite_tree
            .get_mut(self.ct_icon)
            .base_scale_factor = ui_scale_factor;
        base_system.composite_tree.mark_dirty(self.ct_root);
        base_system.composite_tree.mark_dirty(self.ct_icon);
    }

    fn update(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        if !self.is_dirty.replace(false) {
            // not modified
            return;
        }

        let opacity = match (self.hovering.get(), self.pressing.get()) {
            (_, true) => 0.375,
            (true, _) => 0.25,
            _ => 0.0,
        };

        let current = match composite_tree.get(self.ct_bg).composite_mode {
            CompositeMode::FillColor(ref x) => {
                x.evaluate(current_sec, composite_tree.parameter_store())
            }
            _ => unreachable!(),
        };
        composite_tree.get_mut(self.ct_bg).composite_mode =
            CompositeMode::FillColor(AnimatableColor::Animated {
                from_value: current,
                to_value: [1.0, 1.0, 1.0, opacity],
                start_sec: current_sec,
                end_sec: current_sec + 0.1,
                curve: AnimationCurve::CubicBezier {
                    p1: (0.5, 0.0),
                    p2: (0.5, 1.0),
                },
                event_on_complete: None,
            });
        composite_tree.mark_dirty(self.ct_bg);
    }

    pub fn on_hover(&self) {
        self.hovering.set(true);
        self.is_dirty.set(true);
    }

    pub fn on_leave(&self) {
        self.hovering.set(false);
        // はずれたらpressingもなかったことにする
        self.pressing.set(false);

        self.is_dirty.set(true);
    }

    pub fn on_press(&self) {
        self.pressing.set(true);
        self.is_dirty.set(true);
    }

    pub fn on_release(&self) {
        self.pressing.set(false);
        self.is_dirty.set(true);
    }
}

struct BaseView {
    height: f32,
    ct_root: CompositeTreeRef,
    ct_title: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    text_atlas_rect: Cell<AtlasRect>,
}
impl BaseView {
    const TITLE_SPACING: f32 = 16.0;
    const TITLE_LEFT_OFFSET: f32 = 48.0;

    #[tracing::instrument(name = "BaseView::new", skip(ctx))]
    fn new(ctx: &mut ViewInitContext) -> Self {
        let title = "Peridot SpriteAtlas Visualizer/Editor";
        let text_atlas_rect = ctx
            .base_system
            .text_mask(ctx.staging_scratch_buffer, FontType::UI, title)
            .unwrap();
        let bg_atlas_rect = ctx.base_system.alloc_mask_atlas_rect(1, 2);

        let height =
            text_atlas_rect.height() as f32 / ctx.ui_scale_factor + Self::TITLE_SPACING * 2.0;

        let bg_stg_image_pixels = ctx.staging_scratch_buffer.reserve(2);
        let ptr = ctx
            .staging_scratch_buffer
            .map(&bg_stg_image_pixels, StagingScratchBufferMapMode::Write)
            .unwrap();
        unsafe {
            ptr.addr_of_mut::<u8>(0).write(0xff);
            ptr.addr_of_mut::<u8>(1).write(0x00);
        }
        drop(ptr);

        ctx.base_system
            .sync_execute_graphics_commands(|rec| {
                rec.pipeline_barrier_2(&br::DependencyInfo::new(
                    &[],
                    &[],
                    &[ctx
                        .base_system
                        .barrier_for_mask_atlas_resource()
                        .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
                ))
                .inject(|r| {
                    let (b, o) = ctx.staging_scratch_buffer.of(&bg_stg_image_pixels);

                    r.copy_buffer_to_image(
                        b,
                        &ctx.base_system.mask_atlas_image_transparent_ref(),
                        br::ImageLayout::TransferDestOpt,
                        &[br::vk::VkBufferImageCopy {
                            bufferOffset: o,
                            bufferRowLength: 1,
                            bufferImageHeight: 2,
                            imageSubresource: br::ImageSubresourceLayers::new(
                                br::AspectMask::COLOR,
                                0,
                                0..1,
                            ),
                            imageOffset: bg_atlas_rect.lt_offset().with_z(0),
                            imageExtent: bg_atlas_rect.extent().with_depth(1),
                        }],
                    )
                })
                .pipeline_barrier_2(&br::DependencyInfo::new(
                    &[],
                    &[],
                    &[ctx
                        .base_system
                        .barrier_for_mask_atlas_resource()
                        .from(
                            br::PipelineStageFlags2::COPY,
                            br::AccessFlags2::TRANSFER.write,
                        )
                        .to(
                            br::PipelineStageFlags2::FRAGMENT_SHADER,
                            br::AccessFlags2::SHADER_SAMPLED_READ,
                        )
                        .transit_from(
                            br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt),
                        )],
                ))
            })
            .unwrap();

        let ct_root = ctx.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: ctx.ui_scale_factor,
            relative_size_adjustment: [1.0, 0.0],
            size: [AnimatableFloat::Value(0.0), AnimatableFloat::Value(height)],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.0, 0.0, 0.0, 0.25])),
            texatlas_rect: bg_atlas_rect,
            has_bitmap: true,
            ..Default::default()
        });
        let ct_title = ctx.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: ctx.ui_scale_factor,
            size: [
                AnimatableFloat::Value(text_atlas_rect.width() as f32 / ctx.ui_scale_factor),
                AnimatableFloat::Value(text_atlas_rect.height() as f32 / ctx.ui_scale_factor),
            ],
            offset: [
                AnimatableFloat::Value(Self::TITLE_LEFT_OFFSET),
                AnimatableFloat::Value(Self::TITLE_SPACING),
            ],
            texatlas_rect: text_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            has_bitmap: true,
            ..Default::default()
        });

        ctx.base_system.set_composite_tree_parent(ct_title, ct_root);

        let ht_root = ctx.base_system.create_hit_tree(HitTestTreeData {
            height,
            width_adjustment_factor: 1.0,
            ..Default::default()
        });

        Self {
            height,
            ct_root,
            ct_title,
            ht_root,
            text_atlas_rect: Cell::new(text_atlas_rect),
        }
    }

    fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        app_system.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    fn rescale(
        &self,
        base_system: &mut AppBaseSystem,
        staging_scratch_buffer: &mut StagingScratchBufferManager,
        ui_scale_factor: f32,
    ) {
        base_system.free_mask_atlas_rect(self.text_atlas_rect.get());
        let title = "Peridot SpriteAtlas Visualizer/Editor";
        self.text_atlas_rect.set(
            base_system
                .text_mask(staging_scratch_buffer, FontType::UI, title)
                .unwrap(),
        );

        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .base_scale_factor = ui_scale_factor;
        base_system
            .composite_tree
            .get_mut(self.ct_title)
            .base_scale_factor = ui_scale_factor;
        base_system
            .composite_tree
            .get_mut(self.ct_title)
            .texatlas_rect = self.text_atlas_rect.get();
        base_system.composite_tree.mark_dirty(self.ct_root);
        base_system.composite_tree.mark_dirty(self.ct_title);
    }
}

struct ActionHandler {
    menu_button_view: MenuButtonView,
    system_command_button_views: Vec<SystemCommandButtonView>,
}
impl HitTestTreeActionHandler for ActionHandler {
    fn role(&self, sender: HitTestTreeRef) -> Option<Role> {
        for x in self.system_command_button_views.iter() {
            if sender == x.ht_root {
                return match x.cmd.get() {
                    SystemCommand::Close => Some(Role::CloseButton),
                    SystemCommand::Minimize => Some(Role::MinimizeButton),
                    SystemCommand::Maximize => Some(Role::MaximizeButton),
                    SystemCommand::Restore => Some(Role::RestoreButton),
                };
            }
        }

        if sender == self.menu_button_view.ht_root {
            return Some(crate::hittest::Role::ForceClient);
        }

        return Some(crate::hittest::Role::TitleBar);
    }

    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.menu_button_view.ht_root {
            self.menu_button_view.on_hover();
            return EventContinueControl::STOP_PROPAGATION;
        }

        for x in self.system_command_button_views.iter() {
            if sender == x.ht_root {
                x.on_hover();
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        EventContinueControl::empty()
    }

    fn on_pointer_leave(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.menu_button_view.ht_root {
            self.menu_button_view.on_leave();
            return EventContinueControl::STOP_PROPAGATION;
        }

        for x in self.system_command_button_views.iter() {
            if sender == x.ht_root {
                x.on_leave();
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        EventContinueControl::empty()
    }

    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        for x in self.system_command_button_views.iter() {
            if sender == x.ht_root {
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.menu_button_view.ht_root {
            self.menu_button_view.on_press();
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        for x in self.system_command_button_views.iter() {
            if sender == x.ht_root {
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.menu_button_view.ht_root {
            self.menu_button_view.on_release();
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_click(
        &self,
        sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        for x in self.system_command_button_views.iter() {
            if sender == x.ht_root {
                match x.cmd.get() {
                    SystemCommand::Close => {
                        context.event_queue.push(AppEvent::ToplevelWindowClose);
                    }
                    SystemCommand::Minimize => {
                        context
                            .event_queue
                            .push(AppEvent::ToplevelWindowMinimizeRequest);
                    }
                    SystemCommand::Maximize => {
                        context
                            .event_queue
                            .push(AppEvent::ToplevelWindowMaximizeRequest);
                    }
                    SystemCommand::Restore => (),
                }
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.menu_button_view.ht_root {
            context.event_queue.push(AppEvent::AppMenuToggle);
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }
}

pub struct Presenter {
    base_view: BaseView,
    action_handler: Rc<ActionHandler>,
}
impl Presenter {
    pub fn new(init: &mut PresenterInitContext) -> Self {
        let base_view = BaseView::new(&mut init.for_view);
        let menu_button_view = MenuButtonView::new(&mut init.for_view, base_view.height);
        let close_button_view =
            SystemCommandButtonView::new(&mut init.for_view, 0.0, SystemCommand::Close);
        let maximize_restore_button_view = SystemCommandButtonView::new(
            &mut init.for_view,
            SystemCommandButtonView::WIDTH,
            SystemCommand::Maximize,
        );
        let minimize_button_view = SystemCommandButtonView::new(
            &mut init.for_view,
            SystemCommandButtonView::WIDTH * 2.0,
            SystemCommand::Minimize,
        );

        menu_button_view.mount(
            init.for_view.base_system,
            base_view.ct_root,
            base_view.ht_root,
        );
        close_button_view.mount(
            init.for_view.base_system,
            base_view.ct_root,
            base_view.ht_root,
        );
        maximize_restore_button_view.mount(
            init.for_view.base_system,
            base_view.ct_root,
            base_view.ht_root,
        );
        minimize_button_view.mount(
            init.for_view.base_system,
            base_view.ct_root,
            base_view.ht_root,
        );

        let action_handler = Rc::new(ActionHandler {
            menu_button_view,
            system_command_button_views: vec![
                close_button_view,
                maximize_restore_button_view,
                minimize_button_view,
            ],
        });
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(base_view.ht_root, &action_handler);
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(action_handler.menu_button_view.ht_root, &action_handler);
        for x in action_handler.system_command_button_views.iter() {
            init.for_view
                .base_system
                .hit_tree
                .set_action_handler(x.ht_root, &action_handler);
        }

        Self {
            base_view,
            action_handler,
        }
    }

    pub fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        self.base_view.mount(app_system, ct_parent, ht_parent);
    }

    pub fn rescale(
        &self,
        base_system: &mut AppBaseSystem,
        staging_scratch_buffer: &mut StagingScratchBufferManager,
        ui_scale_factor: f32,
    ) {
        self.base_view
            .rescale(base_system, staging_scratch_buffer, ui_scale_factor);
        self.action_handler
            .menu_button_view
            .rescale(base_system, ui_scale_factor);
        for v in self.action_handler.system_command_button_views.iter() {
            v.rescale(base_system, ui_scale_factor);
        }
    }

    pub fn update(&self, ct: &mut CompositeTree, current_sec: f32) {
        self.action_handler.menu_button_view.update(ct, current_sec);
        for x in self.action_handler.system_command_button_views.iter() {
            x.update(ct, current_sec);
        }
    }

    pub const fn height(&self) -> f32 {
        self.base_view.height
    }
}
