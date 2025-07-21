use bedrock::{self as br, DescriptorPoolMut, Device, RenderPass, ShaderModule, VkHandle};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use crate::{
    AppUpdateContext, BLEND_STATE_SINGLE_NONE, FillcolorRConstants, IA_STATE_TRILIST,
    MS_STATE_EMPTY, PresenterInitContext, RASTER_STATE_DEFAULT_FILL_NOCULL, VI_STATE_EMPTY,
    VI_STATE_FLOAT2_ONLY, ViewInitContext,
    base_system::{
        AppBaseSystem, BufferMapMode, FontType, MemoryBoundBuffer, PixelFormat, RenderPassOptions,
        RenderTexture, RenderTextureFlags, RenderTextureOptions, create_render_pass2,
        inject_cmd_begin_render_pass2, inject_cmd_end_render_pass2, inject_cmd_pipeline_barrier_2,
        scratch_buffer::StagingScratchBufferManager,
    },
    composite::{
        AnimatableColor, AnimatableFloat, AnimationCurve, AtlasRect, ClipConfig, CompositeMode,
        CompositeRect, CompositeTree, CompositeTreeRef,
    },
    const_subpass_description_2_single_color_write_only,
    helper_types::SafeF32,
    hittest::{
        CursorShape, HitTestTreeActionHandler, HitTestTreeData, HitTestTreeRef, PointerActionArgs,
    },
    input::EventContinueControl,
    trigger_cell::TriggerCell,
};

struct ToggleButtonView {
    icon_atlas_rect: AtlasRect,
    ct_root: CompositeTreeRef,
    ct_icon: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    hovering: Cell<bool>,
    pressing: Cell<bool>,
    is_dirty: Cell<bool>,
    place_inner: TriggerCell<bool>,
}
impl ToggleButtonView {
    const SIZE: f32 = 20.0;
    const ICON_SIZE: f32 = 7.0;
    const ICON_THICKNESS: f32 = 1.5;

    const ICON_VERTICES: &'static [[f32; 2]] = const {
        let top_left: (f32, f32) = (0.4 - (0.5 * Self::ICON_THICKNESS / Self::ICON_SIZE), 0.0);
        let top_right: (f32, f32) = (0.4 + (0.5 * Self::ICON_THICKNESS / Self::ICON_SIZE), 0.0);
        let middle_left: (f32, f32) = (0.0, 0.5);
        let middle_right: (f32, f32) = (Self::ICON_THICKNESS / Self::ICON_SIZE, 0.5);
        let bottom_left: (f32, f32) = (0.4 - (0.5 * Self::ICON_THICKNESS / Self::ICON_SIZE), 1.0);
        let bottom_right: (f32, f32) = (0.4 + (0.5 * Self::ICON_THICKNESS / Self::ICON_SIZE), 1.0);

        &[
            [top_left.0 * 2.0 - 1.0, top_left.1 * 2.0 - 1.0],
            [top_right.0 * 2.0 - 1.0, top_right.1 * 2.0 - 1.0],
            [middle_left.0 * 2.0 - 1.0, middle_left.1 * 2.0 - 1.0],
            [middle_right.0 * 2.0 - 1.0, middle_right.1 * 2.0 - 1.0],
            [bottom_left.0 * 2.0 - 1.0, bottom_left.1 * 2.0 - 1.0],
            [bottom_right.0 * 2.0 - 1.0, bottom_right.1 * 2.0 - 1.0],
            [(top_left.0 + 0.6) * 2.0 - 1.0, top_left.1 * 2.0 - 1.0],
            [(top_right.0 + 0.6) * 2.0 - 1.0, top_right.1 * 2.0 - 1.0],
            [(middle_left.0 + 0.6) * 2.0 - 1.0, middle_left.1 * 2.0 - 1.0],
            [
                (middle_right.0 + 0.6) * 2.0 - 1.0,
                middle_right.1 * 2.0 - 1.0,
            ],
            [(bottom_left.0 + 0.6) * 2.0 - 1.0, bottom_left.1 * 2.0 - 1.0],
            [
                (bottom_right.0 + 0.6) * 2.0 - 1.0,
                bottom_right.1 * 2.0 - 1.0,
            ],
        ]
    };
    const ICON_INDICES: &'static [u16] = &[
        0u16, 1, 2, 2, 1, 3, 2, 3, 4, 4, 3, 5, 6, 7, 8, 8, 7, 9, 8, 9, 10, 10, 9, 11,
    ];

    fn render_icon_circle(
        base_system: &mut AppBaseSystem,
        icon_atlas_rect: &AtlasRect,
        circle_atlas_rect: &AtlasRect,
    ) {
        let bufsize = Self::ICON_VERTICES.len() * core::mem::size_of::<[f32; 2]>()
            + Self::ICON_INDICES.len() * core::mem::size_of::<u16>();
        let mut buf = MemoryBoundBuffer::new_writable(
            base_system,
            bufsize,
            br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::INDEX_BUFFER,
        )
        .unwrap();
        let ptr = buf.map(0..bufsize, BufferMapMode::Write).unwrap();
        unsafe {
            ptr.addr_of_mut::<[f32; 2]>(0)
                .copy_from_nonoverlapping(Self::ICON_VERTICES.as_ptr(), Self::ICON_VERTICES.len());
            ptr.addr_of_mut::<u16>(Self::ICON_VERTICES.len() * core::mem::size_of::<[f32; 2]>())
                .copy_from_nonoverlapping(Self::ICON_INDICES.as_ptr(), Self::ICON_INDICES.len());
        }
        ptr.unmap().unwrap();

        let msaa_buffer = RenderTexture::new(
            base_system,
            icon_atlas_rect.extent(),
            PixelFormat::R8,
            &RenderTextureOptions {
                msaa_count: Some(4),
                flags: RenderTextureFlags::ALLOW_TRANSFER_SRC | RenderTextureFlags::NON_SAMPLED,
            },
        )
        .unwrap();

        let rp = create_render_pass2(
            base_system.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[msaa_buffer
                    .make_attachment_description()
                    .color_memory_op(br::LoadOp::Clear, br::StoreOp::Store)
                    .layout_transition(
                        br::ImageLayout::Undefined,
                        br::ImageLayout::TransferSrcOpt,
                    )],
                &[const_subpass_description_2_single_color_write_only::<0>()],
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
                &[msaa_buffer.as_transparent_ref()],
                icon_atlas_rect.width(),
                icon_atlas_rect.height(),
            ),
        )
        .unwrap();
        let rp_direct = base_system
            .render_to_mask_atlas_pass(RenderPassOptions::FULL_PIXEL_RENDER)
            .unwrap();
        let fb_direct = br::FramebufferObject::new(
            base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp_direct,
                &[base_system
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                base_system.mask_atlas_size(),
                base_system.mask_atlas_size(),
            ),
        )
        .unwrap();

        #[derive(br::SpecializationConstants)]
        struct CircleFragmentShaderParams {
            #[constant_id = 0]
            pub softness: f32,
        }
        let [pipeline, pipeline_circle] = base_system
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
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
                    &br::PipelineViewportStateCreateInfo::new(
                        &[icon_atlas_rect
                            .extent()
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[icon_atlas_rect.extent().into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
                ),
                br::GraphicsPipelineCreateInfo::new(
                    base_system.require_empty_pipeline_layout(),
                    rp_direct.subpass(0),
                    &[
                        base_system
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        base_system
                            .require_shader("resources/aa_circle.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &CircleFragmentShaderParams { softness: 0.0 },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[circle_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                        &[circle_atlas_rect.vk_rect()],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        base_system
            .sync_execute_graphics_commands(|rec| {
                rec.inject(|r| {
                    inject_cmd_begin_render_pass2(
                        r,
                        base_system.subsystem,
                        &br::RenderPassBeginInfo::new(
                            &rp,
                            &fb,
                            msaa_buffer.render_region(),
                            &[br::ClearValue::color_f32([0.0; 4])],
                        ),
                        &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                    )
                })
                .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
                .bind_vertex_buffer_array(0, &[buf.as_transparent_ref()], &[0])
                .bind_index_buffer(
                    &buf,
                    Self::ICON_VERTICES.len() * core::mem::size_of::<[f32; 2]>(),
                    br::IndexType::U16,
                )
                .draw_indexed(Self::ICON_INDICES.len() as _, 1, 0, 0, 0)
                .inject(|r| {
                    inject_cmd_end_render_pass2(
                        r,
                        base_system.subsystem,
                        &br::SubpassEndInfo::new(),
                    )
                })
                .inject(|r| {
                    inject_cmd_begin_render_pass2(
                        r,
                        base_system.subsystem,
                        &br::RenderPassBeginInfo::new(
                            &rp_direct,
                            &fb_direct,
                            circle_atlas_rect.vk_rect(),
                            &[br::ClearValue::color_f32([0.0; 4])],
                        ),
                        &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                    )
                })
                .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline_circle)
                .draw(3, 1, 0, 0)
                .inject(|r| {
                    inject_cmd_end_render_pass2(
                        r,
                        base_system.subsystem,
                        &br::SubpassEndInfo::new(),
                    )
                })
                .inject(|r| {
                    inject_cmd_pipeline_barrier_2(
                        r,
                        base_system.subsystem,
                        &br::DependencyInfo::new(
                            &[],
                            &[],
                            &[base_system
                                .barrier_for_mask_atlas_resource()
                                .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
                        ),
                    )
                })
                .resolve_image(
                    msaa_buffer.as_image(),
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
                        dstOffset: icon_atlas_rect.lt_offset().with_z(0),
                        extent: icon_atlas_rect.extent().with_depth(1),
                    }],
                )
                .inject(|r| {
                    inject_cmd_pipeline_barrier_2(
                        r,
                        base_system.subsystem,
                        &br::DependencyInfo::new(
                            &[],
                            &[],
                            &[base_system
                                .barrier_for_mask_atlas_resource()
                                .from(
                                    br::PipelineStageFlags2::RESOLVE,
                                    br::AccessFlags2::TRANSFER.write,
                                )
                                .to(
                                    br::PipelineStageFlags2::FRAGMENT_SHADER,
                                    br::AccessFlags2::SHADER_SAMPLED_READ,
                                )
                                .transit_from(
                                    br::ImageLayout::TransferDestOpt
                                        .to(br::ImageLayout::ShaderReadOnlyOpt),
                                )],
                        ),
                    )
                })
            })
            .unwrap();
    }

    #[tracing::instrument(name = "SpriteListToggleButtonView::new", skip(init))]
    fn new(init: &mut ViewInitContext) -> Self {
        let icon_size_px = (Self::ICON_SIZE * init.ui_scale_factor).ceil() as u32;
        let icon_atlas_rect = init
            .base_system
            .alloc_mask_atlas_rect(icon_size_px, icon_size_px);
        let circle_atlas_rect = init.base_system.alloc_mask_atlas_rect(
            (Self::SIZE * init.ui_scale_factor) as _,
            (Self::SIZE * init.ui_scale_factor) as _,
        );
        Self::render_icon_circle(init.base_system, &icon_atlas_rect, &circle_atlas_rect);

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(Self::SIZE),
                AnimatableFloat::Value(Self::SIZE),
            ],
            offset: [
                AnimatableFloat::Value(-Self::SIZE - 8.0),
                AnimatableFloat::Value(8.0),
            ],
            relative_offset_adjustment: [1.0, 0.0],
            has_bitmap: true,
            texatlas_rect: circle_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([1.0, 1.0, 1.0, 0.0])),
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

        init.base_system.set_composite_tree_parent(ct_icon, ct_root);

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            width: Self::SIZE,
            height: Self::SIZE,
            top: 8.0,
            left: -Self::SIZE - 8.0,
            left_adjustment_factor: 1.0,
            ..Default::default()
        });

        Self {
            icon_atlas_rect,
            ct_root,
            ct_icon,
            ht_root,
            hovering: Cell::new(false),
            pressing: Cell::new(false),
            is_dirty: Cell::new(false),
            place_inner: TriggerCell::new(true),
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

    fn rescale(&self, base_system: &mut AppBaseSystem, ui_scale_factor: SafeF32) {
        base_system.free_mask_atlas_rect(
            self.ct_root
                .entity(&base_system.composite_tree)
                .texatlas_rect,
        );
        base_system.free_mask_atlas_rect(
            self.ct_icon
                .entity(&base_system.composite_tree)
                .texatlas_rect,
        );

        let icon_size_px = (Self::ICON_SIZE * ui_scale_factor.value()).ceil() as u32;
        let icon_atlas_rect = base_system.alloc_mask_atlas_rect(icon_size_px, icon_size_px);
        let circle_atlas_rect = base_system.alloc_mask_atlas_rect(
            (Self::SIZE * ui_scale_factor.value()) as _,
            (Self::SIZE * ui_scale_factor.value()) as _,
        );
        Self::render_icon_circle(base_system, &icon_atlas_rect, &circle_atlas_rect);

        let cr = self
            .ct_root
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.texatlas_rect = circle_atlas_rect;
        cr.base_scale_factor = ui_scale_factor.value();
        let cr = self
            .ct_icon
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.texatlas_rect = icon_atlas_rect;
        cr.base_scale_factor = ui_scale_factor.value();
    }

    fn update(&self, app_system: &mut AppBaseSystem, current_sec: f32) {
        if let Some(place_inner) = self.place_inner.get_if_triggered() {
            if place_inner {
                app_system.composite_tree.get_mut(self.ct_root).offset[0] =
                    AnimatableFloat::Animated {
                        from_value: 8.0,
                        to_value: -Self::SIZE - 8.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        curve: AnimationCurve::CubicBezier {
                            p1: (0.25, 0.8),
                            p2: (0.5, 1.0),
                        },
                        event_on_complete: None,
                    };
                app_system.composite_tree.mark_dirty(self.ct_root);
                app_system.hit_tree.get_data_mut(self.ht_root).left = -Self::SIZE - 8.0;
            } else {
                app_system.composite_tree.get_mut(self.ct_root).offset[0] =
                    AnimatableFloat::Animated {
                        from_value: -Self::SIZE - 8.0,
                        to_value: 8.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        curve: AnimationCurve::CubicBezier {
                            p1: (0.25, 0.8),
                            p2: (0.5, 1.0),
                        },
                        event_on_complete: None,
                    };
                app_system.composite_tree.mark_dirty(self.ct_root);
                app_system.hit_tree.get_data_mut(self.ht_root).left = 8.0;
            }

            let ct_icon = app_system.composite_tree.get_mut(self.ct_icon);
            ct_icon.texatlas_rect = self.icon_atlas_rect.clone();
            if !place_inner {
                // flip icon when placed outer
                core::mem::swap(
                    &mut ct_icon.texatlas_rect.left,
                    &mut ct_icon.texatlas_rect.right,
                );
            }

            app_system.composite_tree.mark_dirty(self.ct_icon);
        }

        if !self.is_dirty.replace(false) {
            // not modified
            return;
        }

        let opacity = match (self.hovering.get(), self.pressing.get()) {
            (_, true) => 0.375,
            (true, _) => 0.25,
            _ => 0.0,
        };

        let current = match app_system.composite_tree.get(self.ct_root).composite_mode {
            CompositeMode::ColorTint(ref x) => {
                x.evaluate(current_sec, app_system.composite_tree.parameter_store())
            }
            _ => unreachable!(),
        };
        app_system
            .composite_tree
            .get_mut(self.ct_root)
            .composite_mode = CompositeMode::ColorTint(AnimatableColor::Animated {
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
        app_system.composite_tree.mark_dirty(self.ct_root);
    }

    fn place_inner(&self) {
        self.place_inner.set(true);
    }

    fn place_outer(&self) {
        self.place_inner.set(false);
    }

    fn on_hover(&self) {
        self.hovering.set(true);
        self.is_dirty.set(true);
    }

    fn on_leave(&self) {
        self.hovering.set(false);
        // はずれたらpressingもなかったことにする
        self.pressing.set(false);
        self.is_dirty.set(true);
    }

    fn on_press(&self) {
        self.pressing.set(true);
        self.is_dirty.set(true);
    }

    fn on_release(&self) {
        self.pressing.set(false);
        self.is_dirty.set(true);
    }
}

struct CellView {
    ct_root: CompositeTreeRef,
    ct_bg: CompositeTreeRef,
    ct_bg_selected: CompositeTreeRef,
    ct_label_clip: CompositeTreeRef,
    ct_label: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    label: RefCell<String>,
    top: Cell<f32>,
    hovering: TriggerCell<bool>,
    bound_sprite_index: Cell<usize>,
}
impl CellView {
    const CORNER_RADIUS: SafeF32 = unsafe { SafeF32::new_unchecked(8.0) };
    const MARGIN_H: f32 = 16.0;
    const HEIGHT: f32 = 24.0;
    const LABEL_MARGIN_H: f32 = 8.0;
    const LABEL_OVERFLOW_SOFTCLIP: f32 = 16.0;

    #[tracing::instrument(name = "SpriteListCellView::new", skip(init))]
    fn new(
        init: &mut ViewInitContext,
        init_label: &str,
        init_top: f32,
        init_sprite_index: usize,
    ) -> Self {
        let label_atlas_rect = init
            .base_system
            .text_mask(init.staging_scratch_buffer, FontType::UI, init_label)
            .unwrap();
        let bg_atlas_rect = init
            .base_system
            .rounded_fill_rect_mask(
                unsafe { SafeF32::new_unchecked(init.ui_scale_factor) },
                Self::CORNER_RADIUS,
            )
            .unwrap();

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            offset: [
                AnimatableFloat::Value(Self::MARGIN_H),
                AnimatableFloat::Value(init_top),
            ],
            relative_size_adjustment: [1.0, 0.0],
            size: [
                AnimatableFloat::Value(-Self::MARGIN_H * 2.0),
                AnimatableFloat::Value(Self::HEIGHT),
            ],
            ..Default::default()
        });
        let ct_label_clip = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            offset: [
                AnimatableFloat::Value(Self::LABEL_MARGIN_H),
                AnimatableFloat::Value(
                    -(label_atlas_rect.height() as f32 / init.ui_scale_factor) * 0.5,
                ),
            ],
            relative_offset_adjustment: [0.0, 0.5],
            size: [
                AnimatableFloat::Value(-Self::LABEL_MARGIN_H * 2.0),
                AnimatableFloat::Value(label_atlas_rect.height() as f32 / init.ui_scale_factor),
            ],
            relative_size_adjustment: [1.0, 0.0],
            clip_child: Some(ClipConfig {
                left_softness: unsafe { SafeF32::new_unchecked(0.0) },
                top_softness: unsafe { SafeF32::new_unchecked(0.0) },
                right_softness: unsafe {
                    SafeF32::new_unchecked(Self::LABEL_OVERFLOW_SOFTCLIP * init.ui_scale_factor)
                },
                bottom_softness: unsafe { SafeF32::new_unchecked(0.0) },
            }),
            ..Default::default()
        });
        let ct_label = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            relative_size_adjustment: [0.0, 1.0],
            size: [
                AnimatableFloat::Value(label_atlas_rect.width() as f32 / init.ui_scale_factor),
                AnimatableFloat::Value(0.0),
            ],
            has_bitmap: true,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            texatlas_rect: label_atlas_rect,
            ..Default::default()
        });
        let ct_bg = init.base_system.register_composite_rect(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            has_bitmap: true,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([
                1.0, 1.0, 1.0, 0.125,
            ])),
            texatlas_rect: bg_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS.value() * init.ui_scale_factor; 4],
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });
        let ct_bg_selected = init.base_system.register_composite_rect(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            has_bitmap: true,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.6, 0.8, 1.0, 0.25])),
            texatlas_rect: bg_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS.value() * init.ui_scale_factor; 4],
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });

        init.base_system
            .set_composite_tree_parent(ct_bg_selected, ct_root);
        init.base_system.set_composite_tree_parent(ct_bg, ct_root);
        init.base_system
            .set_composite_tree_parent(ct_label, ct_label_clip);
        init.base_system
            .set_composite_tree_parent(ct_label_clip, ct_root);

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            left: Self::MARGIN_H,
            top: init_top,
            width_adjustment_factor: 1.0,
            width: -Self::MARGIN_H * 2.0,
            height: Self::HEIGHT,
            ..Default::default()
        });

        Self {
            ct_root,
            ct_label_clip,
            ct_label,
            ct_bg,
            ct_bg_selected,
            ht_root,
            label: RefCell::new(init_label.into()),
            top: Cell::new(init_top),
            hovering: TriggerCell::new(false),
            bound_sprite_index: Cell::new(init_sprite_index),
        }
    }

    fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
        base_system: &mut AppBaseSystem,
    ) {
        base_system.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    fn rescale(
        &self,
        base_system: &mut AppBaseSystem,
        staging_scratch_buffer: &mut StagingScratchBufferManager,
        ui_scale_factor: SafeF32,
    ) {
        base_system
            .free_mask_atlas_rect(self.ct_bg.entity(&base_system.composite_tree).texatlas_rect);
        base_system.free_mask_atlas_rect(
            self.ct_label
                .entity(&base_system.composite_tree)
                .texatlas_rect,
        );

        let label_atlas_rect = base_system
            .text_mask(staging_scratch_buffer, FontType::UI, &self.label.borrow())
            .unwrap();
        let bg_atlas_rect = base_system
            .rounded_fill_rect_mask(ui_scale_factor, Self::CORNER_RADIUS)
            .unwrap();

        let cr = self
            .ct_root
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.base_scale_factor = ui_scale_factor.value();
        let cr = self
            .ct_label
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.texatlas_rect = label_atlas_rect;
        cr.base_scale_factor = ui_scale_factor.value();
        let cr = self
            .ct_label_clip
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.base_scale_factor = ui_scale_factor.value();
        cr.clip_child = Some(ClipConfig {
            left_softness: unsafe { SafeF32::new_unchecked(0.0) },
            top_softness: unsafe { SafeF32::new_unchecked(0.0) },
            right_softness: unsafe {
                SafeF32::new_unchecked(Self::LABEL_OVERFLOW_SOFTCLIP * ui_scale_factor.value())
            },
            bottom_softness: unsafe { SafeF32::new_unchecked(0.0) },
        });
        let cr = self
            .ct_bg
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.texatlas_rect = bg_atlas_rect;
        cr.slice_borders = [Self::CORNER_RADIUS.value() * ui_scale_factor.value(); 4];
        let cr = self
            .ct_bg_selected
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.texatlas_rect = bg_atlas_rect;
        cr.slice_borders = [Self::CORNER_RADIUS.value() * ui_scale_factor.value(); 4];
    }

    fn update(&self, base_system: &mut AppBaseSystem, current_sec: f32) {
        if let Some(hovering) = self.hovering.get_if_triggered() {
            base_system.composite_tree.get_mut(self.ct_bg).opacity = if hovering {
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
        }
    }

    fn unmount(&self, base_system: &mut AppBaseSystem) {
        base_system.composite_tree.remove_child(self.ct_root);
        base_system.hit_tree.remove_child(self.ht_root);
    }

    fn on_hover(&self) {
        self.hovering.set(true);
    }

    fn on_leave(&self) {
        self.hovering.set(false);
    }

    fn on_select(&self, ct: &mut CompositeTree) {
        ct.get_mut(self.ct_bg_selected).opacity = AnimatableFloat::Value(1.0);
    }

    fn on_deselect(&self, ct: &mut CompositeTree) {
        ct.get_mut(self.ct_bg_selected).opacity = AnimatableFloat::Value(0.0);
    }

    fn set_top(&self, top: f32, base_system: &mut AppBaseSystem) {
        self.ct_root
            .entity_mut_dirtified(&mut base_system.composite_tree)
            .offset[1] = AnimatableFloat::Value(top);
        base_system.hit_tree.get_data_mut(self.ht_root).top = top;
        self.top.set(top);
    }

    fn set_label(
        &self,
        label: &str,
        base_system: &mut AppBaseSystem,
        staging_scratch_buffer: &mut StagingScratchBufferManager,
    ) {
        if label == &self.label.borrow() as &str {
            // no changes
            return;
        }

        base_system.free_mask_atlas_rect(
            self.ct_label
                .entity(&base_system.composite_tree)
                .texatlas_rect,
        );

        let label_atlas_rect = base_system
            .text_mask(staging_scratch_buffer, FontType::UI, label)
            .unwrap();

        let cr = self
            .ct_label
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.texatlas_rect = label_atlas_rect;

        self.label.replace(label.into());
    }

    fn bind_sprite_index(&self, index: usize) {
        self.bound_sprite_index.set(index);
    }
}

struct FrameView {
    ct_root: CompositeTreeRef,
    ct_title: CompositeTreeRef,
    ct_title_blurred: CompositeTreeRef,
    ht_frame: HitTestTreeRef,
    ht_resize_area: HitTestTreeRef,
    width: Cell<f32>,
    shown: TriggerCell<bool>,
    ui_scale_factor: Cell<f32>,
    is_dirty: Cell<bool>,
}
impl FrameView {
    const CORNER_RADIUS: SafeF32 = unsafe { SafeF32::new_unchecked(24.0) };
    const BLUR_AMOUNT_ONEDIR: u32 = 8;
    const FLOATING_MARGIN: f32 = 8.0;
    const INIT_WIDTH: f32 = 320.0;
    const RESIZE_AREA_WIDTH: f32 = 8.0;

    fn gen_blurry_title(
        base_system: &mut AppBaseSystem,
        title_atlas_rect: &AtlasRect,
        blurred_atlas_rect: &AtlasRect,
        blur_pixels: u32,
    ) {
        let work_rt = RenderTexture::new(
            base_system,
            blurred_atlas_rect.extent(),
            PixelFormat::R8,
            &Default::default(),
        )
        .unwrap();

        let render_pass = base_system
            .render_to_mask_atlas_pass(RenderPassOptions::FULL_PIXEL_RENDER)
            .unwrap();
        let framebuffer = br::FramebufferObject::new(
            base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[base_system
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                base_system.mask_atlas_size(),
                base_system.mask_atlas_size(),
            ),
        )
        .unwrap();
        let title_blurred_work_framebuffer = br::FramebufferObject::new(
            base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[work_rt.as_transparent_ref()],
                blurred_atlas_rect.width(),
                blurred_atlas_rect.height(),
            ),
        )
        .unwrap();

        let vsh_blur = base_system.require_shader("resources/filltri_uvmod.vert");
        let fsh_blur = base_system.require_shader("resources/blit_axis_convolved.frag");
        #[derive(br::SpecializationConstants)]
        struct ConvolutionFragmentShaderParams {
            #[constant_id = 0]
            max_count: u32,
        }

        let smp =
            br::SamplerObject::new(base_system.subsystem, &br::SamplerCreateInfo::new()).unwrap();
        let dsl_tex1 = br::DescriptorSetLayoutObject::new(
            base_system.subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
                .make_binding(0, 1)
                .with_immutable_samplers(&[smp.as_transparent_ref()])]),
        )
        .unwrap();
        let mut dp = br::DescriptorPoolObject::new(
            base_system.subsystem,
            &br::DescriptorPoolCreateInfo::new(
                2,
                &[br::DescriptorType::CombinedImageSampler.make_size(2)],
            ),
        )
        .unwrap();
        let [ds_title, ds_title2] = dp
            .alloc_array(&[dsl_tex1.as_transparent_ref(), dsl_tex1.as_transparent_ref()])
            .unwrap();
        base_system.subsystem.update_descriptor_sets(
            &[
                ds_title
                    .binding_at(0)
                    .write(br::DescriptorContents::CombinedImageSampler(vec![
                        br::DescriptorImageInfo::new(
                            &base_system.mask_atlas_resource_transparent_ref(),
                            br::ImageLayout::ShaderReadOnlyOpt,
                        ),
                    ])),
                ds_title2
                    .binding_at(0)
                    .write(br::DescriptorContents::CombinedImageSampler(vec![
                        br::DescriptorImageInfo::new(&work_rt, br::ImageLayout::ShaderReadOnlyOpt),
                    ])),
            ],
            &[],
        );

        let blur_pipeline_layout = br::PipelineLayoutObject::new(
            base_system.subsystem,
            &br::PipelineLayoutCreateInfo::new(
                &[dsl_tex1.as_transparent_ref()],
                &[
                    br::vk::VkPushConstantRange::for_type::<[f32; 4]>(
                        br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                        0,
                    ),
                    br::vk::VkPushConstantRange::new(
                        br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
                        core::mem::size_of::<[f32; 4]>() as _
                            ..(core::mem::size_of::<[f32; 4]>()
                                + core::mem::size_of::<[f32; 4]>()
                                + core::mem::size_of::<[f32; 2]>()
                                + (core::mem::size_of::<f32>() * blur_pixels as usize))
                                as _,
                    ),
                ],
            ),
        )
        .unwrap();
        let [pipeline_blur1, pipeline_blur] = base_system
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    &blur_pipeline_layout,
                    render_pass.subpass(0),
                    &[
                        vsh_blur.on_stage(br::ShaderStage::Vertex, c"main"),
                        fsh_blur
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &ConvolutionFragmentShaderParams {
                                    max_count: blur_pixels,
                                },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[blurred_atlas_rect
                            .extent()
                            .into_rect(br::Offset2D::ZERO)
                            .make_viewport(0.0..1.0)],
                        &[blurred_atlas_rect.extent().into_rect(br::Offset2D::ZERO)],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &blur_pipeline_layout,
                    render_pass.subpass(0),
                    &[
                        vsh_blur.on_stage(br::ShaderStage::Vertex, c"main"),
                        fsh_blur
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &ConvolutionFragmentShaderParams {
                                    max_count: blur_pixels,
                                },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[blurred_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                        &[blurred_atlas_rect.vk_rect()],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        fn gauss_distrib(x: f32, p: f32) -> f32 {
            (core::f32::consts::TAU * p.powi(2)).sqrt().recip()
                * (-x.powi(2) / (2.0 * p.powi(2))).exp()
        }
        let mut fsh_h_params = vec![0.0f32; blur_pixels as usize + 6];
        let mut fsh_v_params = vec![0.0f32; blur_pixels as usize + 6];
        // uv_limit
        fsh_h_params[0] = base_system.atlas.uv_from_pixels(title_atlas_rect.left as _);
        fsh_h_params[1] = base_system.atlas.uv_from_pixels(title_atlas_rect.top as _);
        fsh_h_params[2] = base_system
            .atlas
            .uv_from_pixels(title_atlas_rect.right as _);
        fsh_h_params[3] = base_system
            .atlas
            .uv_from_pixels(title_atlas_rect.bottom as _);
        fsh_v_params[2] = 1.0;
        fsh_v_params[3] = 1.0;
        // uv_step
        fsh_h_params[4] = 1.0 / base_system.mask_atlas_size() as f32;
        fsh_v_params[5] = 1.0 / blurred_atlas_rect.height() as f32;
        // factors
        let mut t = 0.0;
        for n in 0..blur_pixels as usize {
            let v = gauss_distrib(n as f32, blur_pixels as f32 / 3.0);
            fsh_h_params[n + 6] = v;
            fsh_v_params[n + 6] = v;

            t += v;
        }
        for n in 0..blur_pixels as usize {
            fsh_h_params[n + 6] /= t;
            fsh_v_params[n + 6] /= t;
        }

        base_system
            .sync_execute_graphics_commands(|rec| {
                rec.inject(|r| {
                    inject_cmd_begin_render_pass2(
                        r,
                        base_system.subsystem,
                        &br::RenderPassBeginInfo::new(
                            &render_pass,
                            &title_blurred_work_framebuffer,
                            work_rt.render_region(),
                            &[br::ClearValue::color_f32([0.0; 4])],
                        ),
                        &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                    )
                })
                .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline_blur1)
                .bind_descriptor_sets(
                    br::PipelineBindPoint::Graphics,
                    &blur_pipeline_layout,
                    0,
                    &[ds_title],
                    &[],
                )
                .push_constant(
                    &blur_pipeline_layout,
                    br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                    &[
                        base_system
                            .atlas
                            .uv_from_pixels((title_atlas_rect.width() + blur_pixels * 2 + 1) as _),
                        base_system
                            .atlas
                            .uv_from_pixels((title_atlas_rect.height() + blur_pixels * 2 + 1) as _),
                        base_system
                            .atlas
                            .uv_from_pixels(title_atlas_rect.left as f32 - blur_pixels as f32),
                        base_system
                            .atlas
                            .uv_from_pixels(title_atlas_rect.top as f32 - blur_pixels as f32),
                    ],
                )
                .push_constant_slice(
                    &blur_pipeline_layout,
                    br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
                    core::mem::size_of::<[f32; 4]>() as _,
                    &fsh_h_params,
                )
                .draw(3, 1, 0, 0)
                .inject(|r| {
                    inject_cmd_end_render_pass2(
                        r,
                        base_system.subsystem,
                        &br::SubpassEndInfo::new(),
                    )
                })
                .inject(|r| {
                    inject_cmd_begin_render_pass2(
                        r,
                        base_system.subsystem,
                        &br::RenderPassBeginInfo::new(
                            &render_pass,
                            &framebuffer,
                            blurred_atlas_rect.vk_rect(),
                            &[br::ClearValue::color_f32([0.0; 4])],
                        ),
                        &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                    )
                })
                .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline_blur)
                .bind_descriptor_sets(
                    br::PipelineBindPoint::Graphics,
                    &blur_pipeline_layout,
                    0,
                    &[ds_title2],
                    &[],
                )
                .push_constant(
                    &blur_pipeline_layout,
                    br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                    &[1.0f32, 1.0, 0.0, 0.0],
                )
                .push_constant_slice(
                    &blur_pipeline_layout,
                    br::vk::VK_SHADER_STAGE_FRAGMENT_BIT,
                    core::mem::size_of::<[f32; 4]>() as _,
                    &fsh_v_params,
                )
                .draw(3, 1, 0, 0)
                .inject(|r| {
                    inject_cmd_end_render_pass2(
                        r,
                        base_system.subsystem,
                        &br::SubpassEndInfo::new(),
                    )
                })
            })
            .unwrap();
    }

    #[tracing::instrument(name = "SpriteListPaneView::new", skip(init))]
    fn new(init: &mut ViewInitContext, header_height: f32) -> Self {
        let frame_image_atlas_rect = init
            .base_system
            .rounded_fill_rect_mask(
                unsafe { SafeF32::new_unchecked(init.ui_scale_factor) },
                Self::CORNER_RADIUS,
            )
            .unwrap();
        let title_atlas_rect = init
            .base_system
            .text_mask(init.staging_scratch_buffer, FontType::UI, "Sprites")
            .unwrap();

        let title_blur_pixels =
            (Self::BLUR_AMOUNT_ONEDIR as f32 * init.ui_scale_factor).ceil() as _;
        let title_blurred_atlas_rect = init.base_system.alloc_mask_atlas_rect(
            title_atlas_rect.width() + (title_blur_pixels * 2 + 1),
            title_atlas_rect.height() + (title_blur_pixels * 2 + 1),
        );
        Self::gen_blurry_title(
            init.base_system,
            &title_atlas_rect,
            &title_blurred_atlas_rect,
            title_blur_pixels,
        );

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            offset: [
                AnimatableFloat::Value(Self::FLOATING_MARGIN),
                AnimatableFloat::Value(header_height),
            ],
            size: [
                AnimatableFloat::Value(Self::INIT_WIDTH),
                AnimatableFloat::Value(-(header_height + Self::FLOATING_MARGIN)),
            ],
            relative_size_adjustment: [0.0, 1.0],
            has_bitmap: true,
            texatlas_rect: frame_image_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS.value() * init.ui_scale_factor; 4],
            composite_mode: CompositeMode::ColorTintBackdropBlur(
                AnimatableColor::Value([1.0, 1.0, 1.0, 0.0625]),
                AnimatableFloat::Value(3.0),
            ),
            ..Default::default()
        });
        let ct_title_blurred = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            has_bitmap: true,
            offset: [
                AnimatableFloat::Value(
                    -(title_blurred_atlas_rect.width() as f32 / init.ui_scale_factor * 0.5),
                ),
                AnimatableFloat::Value(12.0 - Self::BLUR_AMOUNT_ONEDIR as f32),
            ],
            relative_offset_adjustment: [0.5, 0.0],
            size: [
                AnimatableFloat::Value(
                    title_blurred_atlas_rect.width() as f32 / init.ui_scale_factor,
                ),
                AnimatableFloat::Value(
                    title_blurred_atlas_rect.height() as f32 / init.ui_scale_factor,
                ),
            ],
            texatlas_rect: title_blurred_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.7, 0.7, 0.7, 1.0])),
            ..Default::default()
        });
        let ct_title = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            has_bitmap: true,
            offset: [
                AnimatableFloat::Value(
                    -(title_atlas_rect.width() as f32 / init.ui_scale_factor * 0.5),
                ),
                AnimatableFloat::Value(12.0),
            ],
            relative_offset_adjustment: [0.5, 0.0],
            size: [
                AnimatableFloat::Value(title_atlas_rect.width() as f32 / init.ui_scale_factor),
                AnimatableFloat::Value(title_atlas_rect.height() as f32 / init.ui_scale_factor),
            ],
            texatlas_rect: title_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        init.base_system
            .set_composite_tree_parent(ct_title_blurred, ct_root);
        init.base_system
            .set_composite_tree_parent(ct_title, ct_root);

        let ht_frame = init.base_system.create_hit_tree(HitTestTreeData {
            top: header_height,
            left: Self::FLOATING_MARGIN,
            width: Self::INIT_WIDTH,
            height: -Self::FLOATING_MARGIN - header_height,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });
        let ht_resize_area = init.base_system.create_hit_tree(HitTestTreeData {
            left: -Self::RESIZE_AREA_WIDTH * 0.5,
            left_adjustment_factor: 1.0,
            width: Self::RESIZE_AREA_WIDTH,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });

        init.base_system
            .set_hit_tree_parent(ht_resize_area, ht_frame);

        Self {
            ct_root,
            ct_title,
            ct_title_blurred,
            ht_frame,
            ht_resize_area,
            width: Cell::new(Self::INIT_WIDTH),
            shown: TriggerCell::new(true),
            ui_scale_factor: Cell::new(init.ui_scale_factor as _),
            is_dirty: Cell::new(false),
        }
    }

    fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        app_system.set_tree_parent((self.ct_root, self.ht_frame), (ct_parent, ht_parent));
    }

    fn rescale(
        &self,
        base_system: &mut AppBaseSystem,
        staging_scratch_buffer: &mut StagingScratchBufferManager,
        ui_scale_factor: SafeF32,
    ) {
        base_system.free_mask_atlas_rect(
            self.ct_root
                .entity(&base_system.composite_tree)
                .texatlas_rect,
        );
        base_system.free_mask_atlas_rect(
            self.ct_title
                .entity(&base_system.composite_tree)
                .texatlas_rect,
        );
        base_system.free_mask_atlas_rect(
            self.ct_title_blurred
                .entity(&base_system.composite_tree)
                .texatlas_rect,
        );

        let frame_atlas_rect = base_system
            .rounded_fill_rect_mask(ui_scale_factor, Self::CORNER_RADIUS)
            .unwrap();
        let title_atlas_rect = base_system
            .text_mask(staging_scratch_buffer, FontType::UI, "Sprites")
            .unwrap();
        let title_blur_pixels =
            (Self::BLUR_AMOUNT_ONEDIR as f32 * ui_scale_factor.value()).ceil() as _;
        let title_blurred_atlas_rect = base_system.alloc_mask_atlas_rect(
            title_atlas_rect.width() + (title_blur_pixels * 2 + 1),
            title_atlas_rect.height() + (title_blur_pixels * 2 + 1),
        );
        Self::gen_blurry_title(
            base_system,
            &title_atlas_rect,
            &title_blurred_atlas_rect,
            title_blur_pixels,
        );

        let cr = self
            .ct_root
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.texatlas_rect = frame_atlas_rect;
        cr.slice_borders = [Self::CORNER_RADIUS.value() * ui_scale_factor.value(); 4];
        cr.base_scale_factor = ui_scale_factor.value();
        let cr = self
            .ct_title
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.texatlas_rect = title_atlas_rect;
        cr.base_scale_factor = ui_scale_factor.value();
        let cr = self
            .ct_title_blurred
            .entity_mut_dirtified(&mut base_system.composite_tree);
        cr.texatlas_rect = title_blurred_atlas_rect;
        cr.base_scale_factor = ui_scale_factor.value();
    }

    fn update(&self, app_system: &mut AppBaseSystem, current_sec: f32) {
        if let Some(shown) = self.shown.get_if_triggered() {
            if shown {
                app_system.composite_tree.get_mut(self.ct_root).offset[0] =
                    AnimatableFloat::Animated {
                        from_value: -self.width.get() * self.ui_scale_factor.get(),
                        to_value: Self::FLOATING_MARGIN * self.ui_scale_factor.get(),
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        curve: AnimationCurve::CubicBezier {
                            p1: (0.4, 1.25),
                            p2: (0.5, 1.0),
                        },
                        event_on_complete: None,
                    };
                app_system.composite_tree.mark_dirty(self.ct_root);
                app_system.hit_tree.get_data_mut(self.ht_frame).left = Self::FLOATING_MARGIN;
            } else {
                app_system.composite_tree.get_mut(self.ct_root).offset[0] =
                    AnimatableFloat::Animated {
                        from_value: Self::FLOATING_MARGIN * self.ui_scale_factor.get(),
                        to_value: -self.width.get() * self.ui_scale_factor.get(),
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        curve: AnimationCurve::CubicBezier {
                            p1: (0.4, 1.25),
                            p2: (0.5, 1.0),
                        },
                        event_on_complete: None,
                    };
                app_system.composite_tree.mark_dirty(self.ct_root);
                app_system.hit_tree.get_data_mut(self.ht_frame).left = -self.width.get();
            }
        }

        if !self.is_dirty.replace(false) {
            // no modification
            return;
        }

        let width = self.width.get();
        self.ct_root
            .entity_mut_dirtified(&mut app_system.composite_tree)
            .size[0] = AnimatableFloat::Value(width);
        app_system.hit_tree.get_data_mut(self.ht_frame).width = width;
    }

    fn set_width(&self, width: f32) {
        self.width.set(width);
        self.is_dirty.set(true);
    }

    fn show(&self) {
        self.shown.set(true);
    }

    fn hide(&self) {
        self.shown.set(false);
    }
}

struct ActionHandler {
    view: Rc<FrameView>,
    toggle_button_view: Rc<ToggleButtonView>,
    cell_views: RefCell<Vec<CellView>>,
    ht_resize_area: HitTestTreeRef,
    resize_state: Cell<Option<(f32, f32)>>,
    shown: Cell<bool>,
}
impl HitTestTreeActionHandler for ActionHandler {
    fn cursor_shape(&self, sender: HitTestTreeRef, _context: &mut AppUpdateContext) -> CursorShape {
        if sender == self.ht_resize_area && self.shown.get() {
            return CursorShape::ResizeHorizontal;
        }

        CursorShape::Default
    }

    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view.on_hover();

            return EventContinueControl::STOP_PROPAGATION;
        }

        for v in self.cell_views.borrow().iter() {
            if sender == v.ht_root {
                v.on_hover();
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
        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view.on_leave();

            return EventContinueControl::STOP_PROPAGATION;
        }

        for v in self.cell_views.borrow().iter() {
            if sender == v.ht_root {
                v.on_leave();
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        EventContinueControl::empty()
    }

    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        args: &PointerActionArgs,
    ) -> EventContinueControl {
        if self.shown.get() {
            if sender == self.view.ht_frame {
                // guard fallback
                return EventContinueControl::STOP_PROPAGATION;
            }

            if sender == self.ht_resize_area {
                self.resize_state
                    .set(Some((self.view.width.get(), args.client_x)));

                return EventContinueControl::CAPTURE_ELEMENT
                    | EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view.on_press();

            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_move(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        args: &PointerActionArgs,
    ) -> EventContinueControl {
        if self.shown.get() {
            if sender == self.view.ht_frame {
                // guard fallback
                return EventContinueControl::STOP_PROPAGATION;
            }

            if sender == self.ht_resize_area {
                if let Some((base_width, base_cx)) = self.resize_state.get() {
                    let w = (base_width + (args.client_x - base_cx)).max(16.0);
                    self.view.set_width(w);

                    return EventContinueControl::STOP_PROPAGATION;
                }
            }
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        args: &PointerActionArgs,
    ) -> EventContinueControl {
        if self.shown.get() {
            if sender == self.view.ht_frame {
                // guard fallback
                return EventContinueControl::STOP_PROPAGATION;
            }

            if sender == self.ht_resize_area {
                if let Some((base_width, base_cx)) = self.resize_state.replace(None) {
                    let w = (base_width + (args.client_x - base_cx)).max(16.0);
                    self.view.set_width(w);

                    return EventContinueControl::RELEASE_CAPTURE_ELEMENT;
                }
            }
        }

        if sender == self.toggle_button_view.ht_root {
            self.toggle_button_view.on_release();

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
        if self.shown.get() && sender == self.view.ht_frame {
            // guard fallback
            return EventContinueControl::STOP_PROPAGATION;
        }

        if sender == self.toggle_button_view.ht_root {
            let show = !self.shown.get();
            self.shown.set(show);

            if show {
                self.view.show();
                self.toggle_button_view.place_inner();
            } else {
                self.view.hide();
                self.toggle_button_view.place_outer();
            }

            return EventContinueControl::STOP_PROPAGATION
                | EventContinueControl::RECOMPUTE_POINTER_ENTER;
        }

        for v in self.cell_views.borrow().iter() {
            if sender == v.ht_root {
                context
                    .state
                    .borrow_mut()
                    .select_sprite(v.bound_sprite_index.get());
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        EventContinueControl::empty()
    }
}

pub struct Presenter {
    view: Rc<FrameView>,
    needs_rebuild_list_cells: Rc<Cell<bool>>,
    sprite_list_contents: Rc<RefCell<Vec<(String, bool)>>>,
    ui_scale_factor: f32,
    ht_action_handler: Rc<ActionHandler>,
}
impl Presenter {
    pub fn new(init: &mut PresenterInitContext, header_height: f32) -> Self {
        let view = Rc::new(FrameView::new(&mut init.for_view, header_height));
        let toggle_button_view = Rc::new(ToggleButtonView::new(&mut init.for_view));

        toggle_button_view.mount(init.for_view.base_system, view.ct_root, view.ht_frame);

        let needs_rebuild_list_cells = Rc::new(Cell::new(false));
        let sprite_list_contents = Rc::new(RefCell::new(Vec::new()));
        init.app_state.register_sprites_view_feedback({
            let sprite_list_contents = Rc::downgrade(&sprite_list_contents);
            let needs_rebuild_list_cells = Rc::downgrade(&needs_rebuild_list_cells);

            move |sprites| {
                let Some(needs_rebuild_list_cells) = needs_rebuild_list_cells.upgrade() else {
                    // presenter teardown-ed
                    return;
                };
                let Some(sprite_list_contents) = sprite_list_contents.upgrade() else {
                    // presenter teardown-ed
                    return;
                };

                sprite_list_contents.borrow_mut().clear();
                sprite_list_contents
                    .borrow_mut()
                    .extend(sprites.iter().map(|x| (x.name.clone(), x.selected)));
                needs_rebuild_list_cells.set(true);
            }
        });

        let ht_action_handler = Rc::new(ActionHandler {
            view: view.clone(),
            toggle_button_view: toggle_button_view.clone(),
            cell_views: RefCell::new(Vec::new()),
            ht_resize_area: view.ht_resize_area,
            resize_state: Cell::new(None),
            shown: Cell::new(true),
        });
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(view.ht_frame, &ht_action_handler);
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(view.ht_resize_area, &ht_action_handler);
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(toggle_button_view.ht_root, &ht_action_handler);

        Self {
            view,
            needs_rebuild_list_cells,
            sprite_list_contents,
            ui_scale_factor: init.for_view.ui_scale_factor,
            ht_action_handler,
        }
    }

    pub fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        self.view.mount(app_system, ct_parent, ht_parent);
    }

    pub fn rescale(
        &mut self,
        base_system: &mut AppBaseSystem,
        staging_scratch_buffer: &mut StagingScratchBufferManager,
        ui_scale_factor: SafeF32,
    ) {
        self.ui_scale_factor = ui_scale_factor.value();

        self.view
            .rescale(base_system, staging_scratch_buffer, ui_scale_factor);
        self.ht_action_handler
            .toggle_button_view
            .rescale(base_system, ui_scale_factor);
        for v in self.ht_action_handler.cell_views.borrow().iter() {
            v.rescale(base_system, staging_scratch_buffer, ui_scale_factor);
        }
    }

    pub fn update<'r, 'base_system, 'subsystem>(
        &mut self,
        app_system: &'base_system mut AppBaseSystem<'subsystem>,
        current_sec: f32,
        staging_scratch_buffer: &'r mut StagingScratchBufferManager<'subsystem>,
    ) {
        self.ht_action_handler.view.update(app_system, current_sec);
        self.ht_action_handler
            .toggle_button_view
            .update(app_system, current_sec);

        if self.needs_rebuild_list_cells.replace(false) {
            let sprite_list_contents = self.sprite_list_contents.borrow();
            let visible_contents = &sprite_list_contents[..];
            let mut cell_views = self.ht_action_handler.cell_views.borrow_mut();
            for (n, &(ref c, sel)) in visible_contents.iter().enumerate() {
                if cell_views.len() == n {
                    // create new one
                    let new_cell = CellView::new(
                        &mut ViewInitContext {
                            base_system: app_system,
                            staging_scratch_buffer,
                            ui_scale_factor: self.ui_scale_factor,
                        },
                        c,
                        32.0 + n as f32 * CellView::HEIGHT,
                        n,
                    );
                    new_cell.mount(
                        self.ht_action_handler.view.ct_root,
                        self.ht_action_handler.view.ht_frame,
                        app_system,
                    );
                    app_system
                        .hit_tree
                        .set_action_handler(new_cell.ht_root, &self.ht_action_handler);
                    if sel {
                        new_cell.on_select(&mut app_system.composite_tree);
                    }

                    cell_views.push(new_cell);
                    continue;
                }

                // reuse existing
                cell_views[n].set_top(32.0 + n as f32 * CellView::HEIGHT, app_system);
                cell_views[n].set_label(c, app_system, staging_scratch_buffer);
                cell_views[n].bind_sprite_index(n);
                if sel {
                    cell_views[n].on_select(&mut app_system.composite_tree);
                } else {
                    cell_views[n].on_deselect(&mut app_system.composite_tree);
                }
            }
        }

        for v in self.ht_action_handler.cell_views.borrow().iter() {
            v.update(app_system, current_sec);
        }
    }
}
