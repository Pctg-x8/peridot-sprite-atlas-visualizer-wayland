//! Common Component(Standalone View)s

use bedrock::{self as br, CommandBufferMut, ImageChild, RenderPass, ShaderModule, VkHandle};
use std::{cell::Cell, rc::Rc};

use crate::{
    AppUpdateContext, BLEND_STATE_SINGLE_NONE, IA_STATE_TRILIST, MS_STATE_EMPTY,
    RASTER_STATE_DEFAULT_FILL_NOCULL, RoundedRectConstants, VI_STATE_EMPTY, ViewInitContext,
    composite::{
        AnimatableColor, AnimationData, CompositeMode, CompositeRect, CompositeTree,
        CompositeTreeRef,
    },
    hittest::{HitTestTreeActionHandler, HitTestTreeData, HitTestTreeManager, HitTestTreeRef},
    text::TextLayout,
};

pub struct CommonButtonView {
    ct_root: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    preferred_height: f32,
    hovering: Cell<bool>,
    pressing: Cell<bool>,
    is_dirty: Cell<bool>,
}
impl CommonButtonView {
    const PADDING_H: f32 = 24.0;
    const PADDING_V: f32 = 12.0;
    const CORNER_RADIUS: f32 = 12.0;

    #[tracing::instrument(name = "CommonButtonView::new", skip(init))]
    pub fn new(init: &mut ViewInitContext, label: &str) -> Self {
        let text_layout = TextLayout::build_simple(label, &mut init.fonts.ui_default);
        let text_atlas_rect = init
            .atlas
            .alloc(text_layout.width_px(), text_layout.height_px());
        let text_image_pixels =
            text_layout.build_stg_image_pixel_buffer(&mut init.staging_scratch_buffer);

        let render_size_px = ((Self::CORNER_RADIUS * 2.0 + 1.0) * init.ui_scale_factor) as u32;
        let frame_image_atlas_rect = init.atlas.alloc(render_size_px, render_size_px);
        let frame_border_image_atlas_rect = init.atlas.alloc(render_size_px, render_size_px);

        let render_pass = br::RenderPassObject::new(
            init.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(init.atlas.format())
                    .with_layout_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())
                    .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)],
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
            &init.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[init.atlas.resource().as_transparent_ref()],
                init.atlas.size(),
                init.atlas.size(),
            ),
        )
        .unwrap();

        let [pipeline, pipeline_border] = init
            .subsystem
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    init.subsystem.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.subsystem
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
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
                .multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    init.subsystem.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.subsystem
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.subsystem
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
                .multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        let mut cp = init
            .subsystem
            .create_transient_graphics_command_pool()
            .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            init.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(
                &br::CommandBufferBeginInfo::new().onetime_submit(),
                init.subsystem,
            )
            .unwrap()
        }
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[br::ImageMemoryBarrier2::new(
                init.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
        ))
        .inject(|r| {
            let (b, o) = init.staging_scratch_buffer.of(&text_image_pixels);

            r.copy_buffer_to_image(
                b,
                init.atlas.resource().image(),
                br::ImageLayout::TransferDestOpt,
                &[br::vk::VkBufferImageCopy {
                    bufferOffset: o,
                    bufferRowLength: text_layout.width_px(),
                    bufferImageHeight: text_layout.height_px(),
                    imageSubresource: br::ImageSubresourceLayers::new(
                        br::AspectMask::COLOR,
                        0,
                        0..1,
                    ),
                    imageOffset: text_atlas_rect.lt_offset().with_z(0),
                    imageExtent: text_atlas_rect.extent().with_depth(1),
                }],
            )
        })
        .begin_render_pass2(
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
        .end()
        .unwrap();
        init.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_root = init.composite_tree.register(CompositeRect {
            size: [
                Self::PADDING_H * 2.0 * init.ui_scale_factor + text_layout.width(),
                Self::PADDING_V * 2.0 * init.ui_scale_factor + text_layout.height(),
            ],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: frame_image_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([1.0, 1.0, 1.0, 0.0])),
            ..Default::default()
        });
        let ct_border = init.composite_tree.register(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: frame_border_image_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS * init.ui_scale_factor; 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([1.0, 1.0, 1.0, 0.25])),
            ..Default::default()
        });
        let ct_label = init.composite_tree.register(CompositeRect {
            offset: [-0.5 * text_layout.width(), -0.5 * text_layout.height()],
            size: [text_layout.width(), text_layout.height()],
            relative_offset_adjustment: [0.5, 0.5],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: text_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        init.composite_tree.add_child(ct_root, ct_border);
        init.composite_tree.add_child(ct_root, ct_label);

        let ht_root = init.ht.create(HitTestTreeData {
            width: Self::PADDING_H * 2.0 + text_layout.width() / init.ui_scale_factor,
            height: Self::PADDING_V * 2.0 + text_layout.height() / init.ui_scale_factor,
            ..Default::default()
        });

        Self {
            ct_root,
            ht_root,
            preferred_height: Self::PADDING_V * 2.0 + text_layout.height() / init.ui_scale_factor,
            hovering: Cell::new(false),
            pressing: Cell::new(false),
            is_dirty: Cell::new(false),
        }
    }

    pub const fn preferred_height(&self) -> f32 {
        self.preferred_height
    }

    #[inline]
    pub fn ct_mut<'c>(&self, ct: &'c mut CompositeTree) -> &'c mut CompositeRect {
        ct.get_mut(self.ct_root)
    }

    #[inline]
    pub fn ht_mut<'h, 'c, C>(
        &self,
        ht: &'h mut HitTestTreeManager<'c, C>,
    ) -> &'h mut HitTestTreeData<'c, C> {
        ht.get_data_mut(self.ht_root)
    }

    #[inline]
    pub fn bind_action_handler<'c, C>(
        &self,
        action_handler: &Rc<impl HitTestTreeActionHandler<'c, Context = C> + 'static>,
        ht: &mut HitTestTreeManager<'c, C>,
    ) {
        ht.set_action_handler(self.ht_root, action_handler);
    }

    #[inline]
    pub fn is_sender(&self, sender: HitTestTreeRef) -> bool {
        sender == self.ht_root
    }

    pub fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        composite_tree: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
    ) {
        composite_tree.add_child(ct_parent, self.ct_root);
        ht.add_child(ht_parent, self.ht_root);
    }

    pub fn update<ActionContext>(
        &self,
        ct: &mut CompositeTree,
        ht: &mut HitTestTreeManager<ActionContext>,
        current_sec: f32,
    ) {
        if !self.is_dirty.replace(false) {
            // not modified
            return;
        }

        let opacity = match (self.hovering.get(), self.pressing.get()) {
            (_, true) => 0.375,
            (true, _) => 0.25,
            _ => 0.0,
        };

        let current = match ct.get(self.ct_root).composite_mode {
            CompositeMode::ColorTint(ref x) => x.evaluate(current_sec, ct.parameter_store()),
            _ => unreachable!(),
        };
        ct.get_mut(self.ct_root).composite_mode =
            CompositeMode::ColorTint(AnimatableColor::Animated(
                current,
                AnimationData {
                    to_value: [1.0, 1.0, 1.0, opacity],
                    start_sec: current_sec,
                    end_sec: current_sec + 0.1,
                    curve_p1: (0.5, 0.0),
                    curve_p2: (0.5, 1.0),
                    event_on_complete: None,
                },
            ));
        ct.mark_dirty(self.ct_root);
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
