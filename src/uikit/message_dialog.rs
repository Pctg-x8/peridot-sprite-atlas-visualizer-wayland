use std::rc::Rc;

use bedrock::{self as br, CommandBufferMut, ImageChild};

use crate::{
    AppEvent, AppUpdateContext, PresenterInitContext, ViewInitContext,
    composite::{
        AnimatableColor, AnimatableFloat, CompositeMode, CompositeRect, CompositeTree,
        CompositeTreeRef,
    },
    hittest::{HitTestTreeActionHandler, HitTestTreeManager, HitTestTreeRef, PointerActionArgs},
    input::EventContinueControl,
    text::TextLayout,
};

use super::{common_controls::CommonButtonView, popup};

struct ContentView {
    ct_root: CompositeTreeRef,
    preferred_width: f32,
    preferred_height: f32,
}
impl ContentView {
    const FRAME_PADDING_H: f32 = 32.0;
    const FRAME_PADDING_V: f32 = 16.0;

    #[tracing::instrument(name = "ContentView::new", skip(init))]
    pub fn new(init: &mut ViewInitContext, content: &str) -> Self {
        let text_layout = TextLayout::build_simple(content, &mut init.fonts.ui_default);
        let text_atlas_rect = init
            .atlas
            .alloc(text_layout.width_px(), text_layout.height_px());
        let text_image_pixels =
            text_layout.build_stg_image_pixel_buffer(&mut init.staging_scratch_buffer);

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
        .pipeline_barrier_2(&br::DependencyInfo::new(
            &[],
            &[],
            &[br::ImageMemoryBarrier2::new(
                init.atlas.resource().image(),
                br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
            )
            .transit_from(br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt))
            .from(
                br::PipelineStageFlags2::COPY,
                br::AccessFlags2::TRANSFER.write,
            )
            .to(
                br::PipelineStageFlags2::FRAGMENT_SHADER,
                br::AccessFlags2::SHADER.read,
            )],
        ))
        .end()
        .unwrap();
        init.subsystem
            .sync_execute_graphics_commands(&[br::CommandBufferSubmitInfo::new(&cb)])
            .unwrap();

        let ct_root = init.composite_tree.register(CompositeRect {
            size: [
                AnimatableFloat::Value(text_layout.width()),
                AnimatableFloat::Value(text_layout.height()),
            ],
            offset: [
                AnimatableFloat::Value(-text_layout.width() * 0.5),
                AnimatableFloat::Value(Self::FRAME_PADDING_V * init.ui_scale_factor),
            ],
            relative_offset_adjustment: [0.5, 0.0],
            instance_slot_index: Some(init.composite_instance_manager.alloc()),
            texatlas_rect: text_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        Self {
            ct_root,
            preferred_width: Self::FRAME_PADDING_H * 2.0
                + text_layout.width() / init.ui_scale_factor,
            preferred_height: Self::FRAME_PADDING_V * 2.0
                + text_layout.height() / init.ui_scale_factor,
        }
    }

    pub fn mount(&self, ct_parent: CompositeTreeRef, ct: &mut CompositeTree) {
        ct.add_child(ct_parent, self.ct_root);
    }
}

struct ActionHandler {
    mask_view: popup::MaskView,
    frame_view: popup::CommonFrameView,
    confirm_button: CommonButtonView,
    popup_id: uuid::Uuid,
}
impl<'c> HitTestTreeActionHandler<'c> for ActionHandler {
    type Context = AppUpdateContext<'c>;

    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        _context: &mut Self::Context,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        if self.confirm_button.is_sender(sender) {
            self.confirm_button.on_hover();

            return EventContinueControl::STOP_PROPAGATION;
        }

        if self.frame_view.is_sender(sender) {
            return EventContinueControl::STOP_PROPAGATION;
        }

        if self.mask_view.is_sender(sender) {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_leave(
        &self,
        sender: HitTestTreeRef,
        _context: &mut Self::Context,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        if self.confirm_button.is_sender(sender) {
            self.confirm_button.on_leave();

            return EventContinueControl::STOP_PROPAGATION;
        }

        if self.frame_view.is_sender(sender) {
            return EventContinueControl::STOP_PROPAGATION;
        }

        if self.mask_view.is_sender(sender) {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        if self.confirm_button.is_sender(sender) {
            self.confirm_button.on_press();

            return EventContinueControl::STOP_PROPAGATION;
        }

        if self.frame_view.is_sender(sender) {
            return EventContinueControl::STOP_PROPAGATION;
        }

        if self.mask_view.is_sender(sender) {
            context
                .event_queue
                .push(AppEvent::UIPopupClose { id: self.popup_id });
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        sender: HitTestTreeRef,
        _context: &mut Self::Context,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        if self.confirm_button.is_sender(sender) {
            self.confirm_button.on_release();

            return EventContinueControl::STOP_PROPAGATION;
        }

        if self.frame_view.is_sender(sender) {
            return EventContinueControl::STOP_PROPAGATION;
        }

        if self.mask_view.is_sender(sender) {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_click(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        _args: &PointerActionArgs,
    ) -> EventContinueControl {
        if self.confirm_button.is_sender(sender) {
            context
                .event_queue
                .push(AppEvent::UIPopupClose { id: self.popup_id });

            return EventContinueControl::STOP_PROPAGATION;
        }

        if self.frame_view.is_sender(sender) {
            return EventContinueControl::STOP_PROPAGATION;
        }

        if self.mask_view.is_sender(sender) {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }
}

pub struct Presenter {
    action_handler: Rc<ActionHandler>,
}
impl Presenter {
    pub fn new(init: &mut PresenterInitContext, popup_id: uuid::Uuid, content: &str) -> Self {
        let content_view = ContentView::new(&mut init.for_view, content);
        let confirm_button = CommonButtonView::new(&mut init.for_view, "OK");
        let frame_view = popup::CommonFrameView::new(
            &mut init.for_view,
            content_view.preferred_width,
            content_view.preferred_height + 4.0 + confirm_button.preferred_height(),
        );
        let mask_view = popup::MaskView::new(&mut init.for_view);

        frame_view.mount(
            mask_view.ct_root(),
            init.for_view.composite_tree,
            mask_view.ht_root(),
            init.for_view.ht,
        );
        content_view.mount(frame_view.ct_root(), init.for_view.composite_tree);
        confirm_button.mount(
            frame_view.ct_root(),
            init.for_view.composite_tree,
            frame_view.ht_root(),
            init.for_view.ht,
        );

        {
            let confirm_button_ct = confirm_button.ct_mut(init.for_view.composite_tree);
            let confirm_button_ht = confirm_button.ht_mut(init.for_view.ht);

            confirm_button_ct.relative_offset_adjustment = [0.5, 0.0];
            confirm_button_ct.offset = [
                AnimatableFloat::Value(-0.5 * confirm_button.preferred_width()),
                AnimatableFloat::Value(
                    (content_view.preferred_height - 4.0) * init.for_view.ui_scale_factor,
                ),
            ];
            confirm_button_ht.left_adjustment_factor = 0.5;
            confirm_button_ht.left = -0.5 * confirm_button_ht.width;
            confirm_button_ht.top = content_view.preferred_height - 4.0;
        }

        let action_handler = Rc::new(ActionHandler {
            mask_view,
            frame_view,
            confirm_button,
            popup_id,
        });
        action_handler
            .mask_view
            .bind_action_handler(&action_handler, init.for_view.ht);
        action_handler
            .frame_view
            .bind_action_handler(&action_handler, init.for_view.ht);
        action_handler
            .confirm_button
            .bind_action_handler(&action_handler, init.for_view.ht);

        Self { action_handler }
    }

    pub fn show(
        &self,
        ct_parent: CompositeTreeRef,
        ct: &mut CompositeTree,
        ht_parent: HitTestTreeRef,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
        current_sec: f32,
    ) {
        self.action_handler
            .mask_view
            .mount(ct_parent, ct, ht_parent, ht);
        self.action_handler.mask_view.show(ct, current_sec);
        self.action_handler.frame_view.show(ct, current_sec);
    }

    pub fn hide(
        &self,
        ct: &mut CompositeTree,
        ht: &mut HitTestTreeManager<AppUpdateContext<'_>>,
        current_sec: f32,
    ) {
        self.action_handler.mask_view.unmount_ht(ht);
        self.action_handler.mask_view.hide(
            ct,
            current_sec,
            AppEvent::UIPopupUnmount {
                id: self.action_handler.popup_id,
            },
        );
        self.action_handler.frame_view.hide(ct, current_sec);
    }

    pub fn unmount(&self, ct: &mut CompositeTree) {
        self.action_handler.mask_view.unmount_visual(ct);
    }

    pub fn update<ActionContext>(
        &self,
        ct: &mut CompositeTree,
        ht: &mut HitTestTreeManager<ActionContext>,
        current_sec: f32,
    ) {
        self.action_handler
            .confirm_button
            .update(ct, ht, current_sec);
    }
}
