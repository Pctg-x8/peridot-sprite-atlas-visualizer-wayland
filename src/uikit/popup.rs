use std::rc::Rc;

use bedrock::{self as br, RenderPass, ShaderModule, VkHandle};

use crate::{
    AppEvent, BLEND_STATE_SINGLE_NONE, IA_STATE_TRILIST, MS_STATE_EMPTY,
    RASTER_STATE_DEFAULT_FILL_NOCULL, RoundedRectConstants, VI_STATE_EMPTY, ViewInitContext,
    base_system::AppBaseSystem,
    composite::{
        AnimatableColor, AnimatableFloat, AnimationCurve, CompositeMode, CompositeRect,
        CompositeTree, CompositeTreeRef,
    },
    hittest::{HitTestTreeActionHandler, HitTestTreeData, HitTestTreeManager, HitTestTreeRef},
};

const POPUP_ANIMATION_DURATION: f32 = 0.2;
const POPUP_MASK_OPACITY: f32 = 0.125;
const POPUP_MASK_BLUR_POWER: f32 = 3.0;

pub struct MaskView {
    ct_root: CompositeTreeRef,
    ht_root: HitTestTreeRef,
}
impl MaskView {
    pub fn new(init: &mut ViewInitContext) -> Self {
        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            has_bitmap: true,
            composite_mode: CompositeMode::FillColor(AnimatableColor::Value([0.0, 0.0, 0.0, 0.0])),
            ..Default::default()
        });

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            width_adjustment_factor: 1.0,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });

        Self { ct_root, ht_root }
    }

    #[inline]
    pub fn bind_action_handler(
        &self,
        handler: &Rc<impl HitTestTreeActionHandler + 'static>,
        ht: &mut HitTestTreeManager,
    ) {
        ht.set_action_handler(self.ht_root, handler);
    }

    // TODO: このへんどうにかしたいかも
    pub const fn ct_root(&self) -> CompositeTreeRef {
        self.ct_root
    }

    pub const fn ht_root(&self) -> HitTestTreeRef {
        self.ht_root
    }

    #[inline(always)]
    pub fn is_sender(&self, sender: HitTestTreeRef) -> bool {
        self.ht_root == sender
    }

    pub fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        app_system.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    pub fn unmount_ht(&self, ht: &mut HitTestTreeManager) {
        ht.remove_child(self.ht_root);
    }

    pub fn unmount_visual(&self, ct: &mut CompositeTree) {
        ct.remove_child(self.ct_root);
    }

    pub fn show(&self, ct: &mut CompositeTree, current_sec: f32) {
        ct.get_mut(self.ct_root).composite_mode = CompositeMode::FillColorBackdropBlur(
            AnimatableColor::Animated {
                from_value: [0.0, 0.0, 0.0, 0.0],
                to_value: [0.0, 0.0, 0.0, POPUP_MASK_OPACITY],
                start_sec: current_sec,
                end_sec: current_sec + POPUP_ANIMATION_DURATION,
                curve: AnimationCurve::Linear,
                event_on_complete: None,
            },
            AnimatableFloat::Animated {
                from_value: 0.0,
                to_value: POPUP_MASK_BLUR_POWER,
                start_sec: current_sec,
                end_sec: current_sec + POPUP_ANIMATION_DURATION,
                curve: AnimationCurve::CubicBezier {
                    p1: (0.25, 0.5),
                    p2: (0.5, 1.0),
                },
                event_on_complete: None,
            },
        );

        ct.mark_dirty(self.ct_root);
    }

    pub fn hide(&self, ct: &mut CompositeTree, current_sec: f32, event_on_complete: AppEvent) {
        ct.get_mut(self.ct_root).composite_mode = CompositeMode::FillColorBackdropBlur(
            AnimatableColor::Animated {
                from_value: [0.0, 0.0, 0.0, POPUP_MASK_OPACITY],
                to_value: [0.0, 0.0, 0.0, 0.0],
                start_sec: current_sec,
                end_sec: current_sec + POPUP_ANIMATION_DURATION,
                curve: AnimationCurve::Linear,
                event_on_complete: Some(event_on_complete),
            },
            AnimatableFloat::Animated {
                from_value: POPUP_MASK_BLUR_POWER,
                to_value: 0.0,
                start_sec: current_sec,
                end_sec: current_sec + POPUP_ANIMATION_DURATION,
                curve: AnimationCurve::Linear,
                event_on_complete: None,
            },
        );

        ct.mark_dirty(self.ct_root);
    }
}

pub struct CommonFrameView {
    ct_root: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    height: f32,
    ui_scale_factor: f32,
}
impl CommonFrameView {
    const CORNER_RADIUS: f32 = 16.0;

    pub fn new(init: &mut ViewInitContext, width: f32, height: f32) -> Self {
        let render_size_px =
            ((Self::CORNER_RADIUS * 2.0 + 1.0) * init.ui_scale_factor.ceil()) as u32;
        let frame_image_atlas_rect = init
            .base_system
            .alloc_mask_atlas_rect(render_size_px, render_size_px);
        let frame_border_image_atlas_rect = init
            .base_system
            .alloc_mask_atlas_rect(render_size_px, render_size_px);

        let render_pass = br::RenderPassObject::new(
            &init.base_system.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[
                    br::AttachmentDescription2::new(init.base_system.mask_atlas_format())
                        .with_layout_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())
                        .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store),
                ],
                &[br::SubpassDescription2::new()
                    .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
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
            &init.base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[init
                    .base_system
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                init.base_system.mask_atlas_size(),
                init.base_system.mask_atlas_size(),
            ),
        )
        .unwrap();

        let [pipeline, pipeline_border] = init
            .base_system
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    init.base_system.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.base_system
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.base_system
                            .require_shader("resources/rounded_rect.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &RoundedRectConstants {
                                    corner_radius: Self::CORNER_RADIUS,
                                },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[frame_image_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                        &[frame_image_atlas_rect.vk_rect()],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    init.base_system.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.base_system
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.base_system
                            .require_shader("resources/rounded_rect_border.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &RoundedRectConstants {
                                    corner_radius: Self::CORNER_RADIUS,
                                },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new(
                        &[frame_border_image_atlas_rect
                            .vk_rect()
                            .make_viewport(0.0..1.0)],
                        &[frame_border_image_atlas_rect.vk_rect()],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        init.base_system
            .sync_execute_graphics_commands(|rec| {
                rec.begin_render_pass2(
                    &br::RenderPassBeginInfo::new(
                        &render_pass,
                        &framebuffer,
                        frame_image_atlas_rect.vk_rect(),
                        &[br::ClearValue::color_f32([0.0; 4])],
                    ),
                    &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                )
                .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
                .draw(3, 1, 0, 0)
                .end_render_pass2(&br::SubpassEndInfo::new())
                .begin_render_pass2(
                    &br::RenderPassBeginInfo::new(
                        &render_pass,
                        &framebuffer,
                        frame_border_image_atlas_rect.vk_rect(),
                        &[br::ClearValue::color_f32([0.0; 4])],
                    ),
                    &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                )
                .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline_border)
                .draw(3, 1, 0, 0)
                .end_render_pass2(&br::SubpassEndInfo::new())
            })
            .unwrap();
        drop((pipeline, pipeline_border, framebuffer, render_pass));

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            offset: [
                AnimatableFloat::Value(-width * 0.5 * init.ui_scale_factor),
                AnimatableFloat::Value(-height * 0.5 * init.ui_scale_factor),
            ],
            relative_offset_adjustment: [0.5, 0.5],
            size: [
                AnimatableFloat::Value(width * init.ui_scale_factor),
                AnimatableFloat::Value(height * init.ui_scale_factor),
            ],
            has_bitmap: true,
            texatlas_rect: frame_image_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor.ceil(); 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.0, 0.0, 0.0, 1.0])),
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });
        let ct_border = init.base_system.register_composite_rect(CompositeRect {
            offset: [
                AnimatableFloat::Value(-width * 0.5 * init.ui_scale_factor),
                AnimatableFloat::Value(-height * 0.5 * init.ui_scale_factor),
            ],
            relative_offset_adjustment: [0.5, 0.5],
            size: [
                AnimatableFloat::Value(width * init.ui_scale_factor),
                AnimatableFloat::Value(height * init.ui_scale_factor),
            ],
            has_bitmap: true,
            texatlas_rect: frame_border_image_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor.ceil(); 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([
                0.25, 0.25, 0.25, 1.0,
            ])),
            ..Default::default()
        });

        init.base_system
            .set_composite_tree_parent(ct_border, ct_root);

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            left: -width * 0.5,
            top: -height * 0.5,
            left_adjustment_factor: 0.5,
            top_adjustment_factor: 0.5,
            width,
            height,
            ..Default::default()
        });

        Self {
            ct_root,
            ht_root,
            height,
            ui_scale_factor: init.ui_scale_factor,
        }
    }

    #[inline]
    pub fn bind_action_handler(
        &self,
        handler: &Rc<impl HitTestTreeActionHandler + 'static>,
        ht: &mut HitTestTreeManager,
    ) {
        ht.set_action_handler(self.ht_root, handler);
    }

    // TODO: このへんどうにかしたいかも
    pub const fn ct_root(&self) -> CompositeTreeRef {
        self.ct_root
    }

    pub const fn ht_root(&self) -> HitTestTreeRef {
        self.ht_root
    }

    #[inline(always)]
    pub fn is_sender(&self, sender: HitTestTreeRef) -> bool {
        self.ht_root == sender
    }

    pub fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        app_system.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    pub fn show(&self, ct: &mut CompositeTree, current_sec: f32) {
        ct.get_mut(self.ct_root).opacity = AnimatableFloat::Animated {
            from_value: 0.0,
            to_value: 1.0,
            start_sec: current_sec,
            end_sec: current_sec + POPUP_ANIMATION_DURATION,
            curve: AnimationCurve::Linear,
            event_on_complete: None,
        };
        ct.get_mut(self.ct_root).offset[1] = AnimatableFloat::Animated {
            from_value: (-0.5 * self.height + 8.0) * self.ui_scale_factor,
            to_value: (-0.5 * self.height) * self.ui_scale_factor,
            start_sec: current_sec,
            end_sec: current_sec + POPUP_ANIMATION_DURATION,
            curve: AnimationCurve::CubicBezier {
                p1: (0.25, 0.5),
                p2: (0.5, 0.9),
            },
            event_on_complete: None,
        };
        ct.get_mut(self.ct_root).scale_x = AnimatableFloat::Animated {
            from_value: 0.9,
            to_value: 1.0,
            start_sec: current_sec,
            end_sec: current_sec + POPUP_ANIMATION_DURATION,
            curve: AnimationCurve::CubicBezier {
                p1: (0.25, 0.5),
                p2: (0.5, 0.9),
            },
            event_on_complete: None,
        };
        ct.get_mut(self.ct_root).scale_y = AnimatableFloat::Animated {
            from_value: 0.9,
            to_value: 1.0,
            start_sec: current_sec,
            end_sec: current_sec + POPUP_ANIMATION_DURATION,
            curve: AnimationCurve::CubicBezier {
                p1: (0.25, 0.5),
                p2: (0.5, 0.9),
            },
            event_on_complete: None,
        };

        ct.mark_dirty(self.ct_root);
    }

    pub fn hide(&self, ct: &mut CompositeTree, current_sec: f32) {
        ct.get_mut(self.ct_root).opacity = AnimatableFloat::Animated {
            from_value: 1.0,
            to_value: 0.0,
            start_sec: current_sec,
            end_sec: current_sec + POPUP_ANIMATION_DURATION,
            curve: AnimationCurve::Linear,
            event_on_complete: None,
        };
        ct.get_mut(self.ct_root).offset[1] = AnimatableFloat::Animated {
            from_value: (-0.5 * self.height) * self.ui_scale_factor,
            to_value: (-0.5 * self.height + 8.0) * self.ui_scale_factor,
            start_sec: current_sec,
            end_sec: current_sec + POPUP_ANIMATION_DURATION,
            curve: AnimationCurve::CubicBezier {
                p1: (0.25, 0.5),
                p2: (0.5, 0.9),
            },
            event_on_complete: None,
        };
        ct.get_mut(self.ct_root).scale_x = AnimatableFloat::Animated {
            from_value: 1.0,
            to_value: 0.9,
            start_sec: current_sec,
            end_sec: current_sec + POPUP_ANIMATION_DURATION,
            curve: AnimationCurve::CubicBezier {
                p1: (0.25, 0.5),
                p2: (0.5, 0.9),
            },
            event_on_complete: None,
        };
        ct.get_mut(self.ct_root).scale_y = AnimatableFloat::Animated {
            from_value: 1.0,
            to_value: 0.9,
            start_sec: current_sec,
            end_sec: current_sec + POPUP_ANIMATION_DURATION,
            curve: AnimationCurve::CubicBezier {
                p1: (0.25, 0.5),
                p2: (0.5, 0.9),
            },
            event_on_complete: None,
        };

        ct.mark_dirty(self.ct_root);
    }
}
