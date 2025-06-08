use bedrock::{
    self as br, CommandBufferMut, Device, DeviceMemoryMut, ImageChild, MemoryBound, RenderPass,
    ShaderModule, VkHandle,
};
use std::{cell::Cell, rc::Rc};

use crate::{
    AppUpdateContext, BLEND_STATE_SINGLE_NONE, FillcolorRConstants, IA_STATE_TRILIST,
    MS_STATE_EMPTY, PresenterInitContext, RASTER_STATE_DEFAULT_FILL_NOCULL, VI_STATE_FLOAT2_ONLY,
    ViewInitContext,
    composite::{
        AnimatableColor, AnimationData, CompositeMode, CompositeRect, CompositeTree,
        CompositeTreeRef,
    },
    hittest::{
        HitTestTreeActionHandler, HitTestTreeData, HitTestTreeManager, HitTestTreeRef,
        PointerActionArgs,
    },
    input::EventContinueControl,
    subsystem::StagingScratchBufferMapMode,
    text::TextLayout,
};

struct MenuButtonView {
    ct_root: CompositeTreeRef,
    ct_bg: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    hovering: Cell<bool>,
    pressing: Cell<bool>,
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

    #[tracing::instrument(name = "MenuButtonView::new", skip(init))]
    pub fn new(init: &mut ViewInitContext, height: f32) -> Self {
        let icon_atlas_rect = init.atlas.alloc(
            (Self::ICON_SIZE * init.ui_scale_factor) as _,
            (Self::ICON_SIZE * init.ui_scale_factor) as _,
        );

        let mut vbuf = br::BufferObject::new(
            init.subsystem,
            &br::BufferCreateInfo::new(
                core::mem::size_of::<[f32; 2]>() * Self::ICON_VERTICES.len()
                    + core::mem::size_of::<u16>() * Self::ICON_INDICES.len(),
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::INDEX_BUFFER,
            ),
        )
        .unwrap();
        let req = vbuf.requirements();
        let memindex = init
            .subsystem
            .find_direct_memory_index(req.memoryTypeBits)
            .expect("no suitable memory");
        let mut mem = br::DeviceMemoryObject::new(
            init.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        vbuf.bind(&mem, 0).unwrap();

        let h = mem.native_ptr();
        let requires_flush = !init.subsystem.is_coherent_memory_type(memindex);
        let ptr = mem.map(0..req.size as _).unwrap();
        unsafe {
            core::ptr::copy_nonoverlapping(
                Self::ICON_VERTICES.as_ptr(),
                ptr.addr_of_mut::<[f32; 2]>(0),
                Self::ICON_VERTICES.len(),
            );
            core::ptr::copy_nonoverlapping(
                Self::ICON_INDICES.as_ptr(),
                ptr.addr_of_mut::<u16>(
                    core::mem::size_of::<[f32; 2]>() * Self::ICON_VERTICES.len(),
                ),
                Self::ICON_INDICES.len(),
            );
        }
        if requires_flush {
            unsafe {
                init.subsystem
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(h, 0, req.size)])
                    .unwrap();
            }
        }
        unsafe {
            mem.unmap();
        }

        let rp = br::RenderPassObject::new(
            init.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[br::AttachmentDescription2::new(init.atlas.format())
                    .color_memory_op(br::LoadOp::Clear, br::StoreOp::Store)
                    .with_layout_to(br::ImageLayout::ShaderReadOnlyOpt.from_undefined())],
                &[br::SubpassDescription2::new()
                    .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
                &[br::SubpassDependency2::new(
                    br::SubpassIndex::Internal(0),
                    br::SubpassIndex::External,
                )
                .by_region()
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
        let fb = br::FramebufferObject::new(
            init.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp,
                &[init.atlas.resource().as_transparent_ref()],
                init.atlas.size(),
                init.atlas.size(),
            ),
        )
        .unwrap();

        let [pipeline] = init
            .subsystem
            .create_graphics_pipelines_array(&[br::GraphicsPipelineCreateInfo::new(
                init.subsystem.require_empty_pipeline_layout(),
                rp.subpass(0),
                &[
                    init.subsystem
                        .require_shader("resources/notrans.vert")
                        .on_stage(br::ShaderStage::Vertex, c"main"),
                    init.subsystem
                        .require_shader("resources/fillcolor_r.frag")
                        .on_stage(br::ShaderStage::Fragment, c"main")
                        .with_specialization_info(&br::SpecializationInfo::new(
                            &FillcolorRConstants { r: 1.0 },
                        )),
                ],
                VI_STATE_FLOAT2_ONLY,
                IA_STATE_TRILIST,
                &br::PipelineViewportStateCreateInfo::new_array(
                    &[icon_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                    &[icon_atlas_rect.vk_rect()],
                ),
                RASTER_STATE_DEFAULT_FILL_NOCULL,
                BLEND_STATE_SINGLE_NONE,
            )
            .multisample_state(MS_STATE_EMPTY)])
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
        .begin_render_pass2(
            &br::RenderPassBeginInfo::new(
                &rp,
                &fb,
                icon_atlas_rect.vk_rect(),
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
        .end()
        .unwrap();
        init.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_root = init.composite_tree.register(CompositeRect {
            size: [height * init.ui_scale_factor, height * init.ui_scale_factor],
            ..Default::default()
        });
        let ct_bg = init.composite_tree.register(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            composite_mode: CompositeMode::FillColor(AnimatableColor::Value([1.0, 1.0, 1.0, 0.0])),
            ..Default::default()
        });
        let ct_icon = init.composite_tree.register(CompositeRect {
            size: [
                Self::ICON_SIZE * init.ui_scale_factor,
                Self::ICON_SIZE * init.ui_scale_factor,
            ],
            offset: [
                -Self::ICON_SIZE * 0.5 * init.ui_scale_factor,
                -Self::ICON_SIZE * 0.5 * init.ui_scale_factor,
            ],
            relative_offset_adjustment: [0.5, 0.5],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: icon_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        init.composite_tree.add_child(ct_root, ct_bg);
        init.composite_tree.add_child(ct_root, ct_icon);

        let ht_root = init.ht.create(HitTestTreeData {
            width: height,
            height,
            ..Default::default()
        });

        Self {
            ct_root,
            ct_bg,
            ht_root,
            hovering: Cell::new(false),
            pressing: Cell::new(false),
        }
    }

    pub fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        ct: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
    ) {
        ct.add_child(ct_parent, self.ct_root);
        ht.add_child(ht_parent, self.ht_root);
    }

    fn update_button_bg_opacity(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        let opacity = match (self.hovering.get(), self.pressing.get()) {
            (_, true) => 0.375,
            (true, _) => 0.25,
            _ => 0.0,
        };

        let current = match composite_tree.get(self.ct_bg).composite_mode {
            CompositeMode::FillColor(ref x) => {
                x.compute(current_sec, composite_tree.parameter_store())
            }
            _ => unreachable!(),
        };
        composite_tree.get_mut(self.ct_bg).composite_mode =
            CompositeMode::FillColor(AnimatableColor::Animated(
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
        composite_tree.mark_dirty(self.ct_bg);
    }

    pub fn on_hover(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        self.hovering.set(true);
        self.update_button_bg_opacity(composite_tree, current_sec);
    }

    pub fn on_leave(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        self.hovering.set(false);
        // はずれたらpressingもなかったことにする
        self.pressing.set(false);

        self.update_button_bg_opacity(composite_tree, current_sec);
    }

    pub fn on_press(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        self.pressing.set(true);
        self.update_button_bg_opacity(composite_tree, current_sec);
    }

    pub fn on_release(&self, composite_tree: &mut CompositeTree, current_sec: f32) {
        self.pressing.set(false);
        self.update_button_bg_opacity(composite_tree, current_sec);
    }
}

struct BaseView {
    height: f32,
    ct_root: CompositeTreeRef,
    ht_root: HitTestTreeRef,
}
impl BaseView {
    const TITLE_SPACING: f32 = 16.0;
    const TITLE_LEFT_OFFSET: f32 = 48.0;

    #[tracing::instrument(name = "BaseView::new", skip(ctx))]
    pub fn new(ctx: &mut ViewInitContext) -> Self {
        let title = "Peridot SpriteAtlas Visualizer/Editor";
        let text_layout = TextLayout::build_simple(title, &mut ctx.fonts.ui_default);
        let text_atlas_rect = ctx
            .atlas
            .alloc(text_layout.width_px(), text_layout.height_px());
        let bg_atlas_rect = ctx.atlas.alloc(1, 2);

        let height = text_layout.height() / ctx.ui_scale_factor + Self::TITLE_SPACING * 2.0;

        let text_stg_image_pixels =
            text_layout.build_stg_image_pixel_buffer(ctx.staging_scratch_buffer);
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

        let mut cp = ctx
            .subsystem
            .create_transient_graphics_command_pool()
            .unwrap();
        let [mut cb] = br::CommandBufferObject::alloc_array(
            &ctx.subsystem,
            &br::CommandBufferFixedCountAllocateInfo::new(&mut cp, br::CommandBufferLevel::Primary),
        )
        .unwrap();
        unsafe {
            cb.begin(
                &br::CommandBufferBeginInfo::new().onetime_submit(),
                &ctx.subsystem,
            )
            .unwrap()
        }
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[br::ImageMemoryBarrier2::new(
                ctx.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
        ))
        .inject(|r| {
            let (tb, to) = ctx.staging_scratch_buffer.of(&text_stg_image_pixels);
            let (b, o) = ctx.staging_scratch_buffer.of(&bg_stg_image_pixels);

            // TODO: ここ使うリソースいっしょだったらバッチするようにしたい
            r.copy_buffer_to_image(
                tb,
                ctx.atlas.resource().image(),
                br::ImageLayout::TransferDestOpt,
                &[br::vk::VkBufferImageCopy {
                    bufferOffset: to,
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
            .copy_buffer_to_image(
                b,
                ctx.atlas.resource().image(),
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
            &[br::ImageMemoryBarrier2::new(
                ctx.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .from(
                br::PipelineStageFlags2::COPY,
                br::AccessFlags2::TRANSFER.write,
            )
            .to(
                br::PipelineStageFlags2::FRAGMENT_SHADER,
                br::AccessFlags2::SHADER_SAMPLED_READ,
            )
            .transit_from(br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt))],
        ))
        .end()
        .unwrap();
        ctx.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_root = ctx.composite_tree.register(CompositeRect {
            relative_size_adjustment: [1.0, 0.0],
            size: [0.0, height * ctx.ui_scale_factor],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.0, 0.0, 0.0, 0.25])),
            texatlas_rect: bg_atlas_rect,
            instance_slot_index: Some(ctx.composite_instance_manager.alloc()),
            ..Default::default()
        });
        let ct_title = ctx.composite_tree.register(CompositeRect {
            size: [text_layout.width(), text_layout.height()],
            offset: [
                Self::TITLE_LEFT_OFFSET * ctx.ui_scale_factor,
                Self::TITLE_SPACING * ctx.ui_scale_factor,
            ],
            texatlas_rect: text_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            instance_slot_index: Some(ctx.composite_instance_manager.alloc()),
            ..Default::default()
        });

        ctx.composite_tree.add_child(ct_root, ct_title);

        let ht_root = ctx.ht.create(HitTestTreeData {
            height,
            width_adjustment_factor: 1.0,
            ..Default::default()
        });

        Self {
            height,
            ct_root,
            ht_root,
        }
    }

    pub fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        ct: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
    ) {
        ct.add_child(ct_parent, self.ct_root);
        ht.add_child(ht_parent, self.ht_root);
    }
}

struct ActionHandler {
    menu_button_view: MenuButtonView,
}
impl<'c> HitTestTreeActionHandler<'c> for ActionHandler {
    type Context = AppUpdateContext<'c>;

    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.menu_button_view.ht_root {
            self.menu_button_view.on_hover(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_leave(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.menu_button_view.ht_root {
            self.menu_button_view.on_leave(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.menu_button_view.ht_root {
            self.menu_button_view.on_press(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _ht: &mut HitTestTreeManager<Self::Context>,
        _args: PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.menu_button_view.ht_root {
            self.menu_button_view.on_release(
                &mut context.for_view_feedback.composite_tree,
                context.for_view_feedback.current_sec,
            );
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_click(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        _args: PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.menu_button_view.ht_root {
            context
                .state
                .toggle_menu(&mut context.for_view_feedback, ht);
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }
}

pub struct Presenter {
    base_view: BaseView,
    _action_handler: Rc<ActionHandler>,
}
impl Presenter {
    pub fn new(init: &mut PresenterInitContext) -> Self {
        let base_view = BaseView::new(&mut init.for_view);
        let menu_button_view = MenuButtonView::new(&mut init.for_view, base_view.height);

        menu_button_view.mount(
            base_view.ct_root,
            init.for_view.composite_tree,
            base_view.ht_root,
            init.for_view.ht,
        );

        let action_handler = Rc::new(ActionHandler { menu_button_view });
        init.for_view
            .ht
            .set_action_handler(action_handler.menu_button_view.ht_root, &action_handler);

        Self {
            base_view,
            _action_handler: action_handler,
        }
    }

    pub fn mount(
        &self,
        ct_parent: CompositeTreeRef,
        ct: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
    ) {
        self.base_view.mount(ct_parent, ct, ht_parent, ht);
    }

    pub const fn height(&self) -> f32 {
        self.base_view.height
    }
}
