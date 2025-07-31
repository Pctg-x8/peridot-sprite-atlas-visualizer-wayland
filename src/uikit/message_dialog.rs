use std::rc::Rc;

use crate::{
    AppEvent, AppUpdateContext, PresenterInitContext, ViewInitContext,
    base_system::{AppBaseSystem, FontType},
    composite::{
        AnimatableColor, AnimatableFloat, CompositeMode, CompositeRect, CompositeTree,
        CompositeTreeRef,
    },
    hittest::{HitTestTreeActionHandler, HitTestTreeRef, PointerActionArgs},
    input::EventContinueControl,
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
        let text_atlas_rect = init.base_system.text_mask(FontType::UI, content).unwrap();

        let preferred_width =
            Self::FRAME_PADDING_H * 2.0 + text_atlas_rect.width() as f32 / init.ui_scale_factor;
        let preferred_height =
            Self::FRAME_PADDING_V * 2.0 + text_atlas_rect.height() as f32 / init.ui_scale_factor;

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(text_atlas_rect.width() as f32 / init.ui_scale_factor),
                AnimatableFloat::Value(text_atlas_rect.height() as f32 / init.ui_scale_factor),
            ],
            offset: [
                AnimatableFloat::Value(
                    -(text_atlas_rect.width() as f32 / init.ui_scale_factor) * 0.5,
                ),
                AnimatableFloat::Value(Self::FRAME_PADDING_V),
            ],
            relative_offset_adjustment: [0.5, 0.0],
            has_bitmap: true,
            texatlas_rect: text_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        Self {
            ct_root,
            preferred_width,
            preferred_height,
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
impl HitTestTreeActionHandler for ActionHandler {
    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
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
        _context: &mut AppUpdateContext,
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
        context: &mut AppUpdateContext,
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
        _context: &mut AppUpdateContext,
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
        context: &mut AppUpdateContext,
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
            init.for_view.base_system,
            mask_view.ct_root(),
            mask_view.ht_root(),
        );
        content_view.mount(
            frame_view.ct_root(),
            &mut init.for_view.base_system.composite_tree,
        );
        confirm_button.mount(
            init.for_view.base_system,
            frame_view.ct_root(),
            frame_view.ht_root(),
        );

        {
            let confirm_button_ct =
                confirm_button.ct_mut(&mut init.for_view.base_system.composite_tree);
            let confirm_button_ht = confirm_button.ht_mut(&mut init.for_view.base_system.hit_tree);

            confirm_button_ct.relative_offset_adjustment = [0.5, 0.0];
            confirm_button_ct.offset = [
                AnimatableFloat::Value(-0.5 * confirm_button.preferred_width()),
                AnimatableFloat::Value(content_view.preferred_height - 4.0),
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
            .bind_action_handler(&action_handler, &mut init.for_view.base_system.hit_tree);
        action_handler
            .frame_view
            .bind_action_handler(&action_handler, &mut init.for_view.base_system.hit_tree);
        action_handler
            .confirm_button
            .bind_action_handler(&action_handler, &mut init.for_view.base_system.hit_tree);

        Self { action_handler }
    }

    pub fn show(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
        current_sec: f32,
    ) {
        self.action_handler
            .mask_view
            .mount(app_system, ct_parent, ht_parent);
        self.action_handler
            .mask_view
            .show(&mut app_system.composite_tree, current_sec);
        self.action_handler
            .frame_view
            .show(&mut app_system.composite_tree, current_sec);
    }

    pub fn hide(&self, app_system: &mut AppBaseSystem, current_sec: f32) {
        self.action_handler
            .mask_view
            .unmount_ht(&mut app_system.hit_tree);
        self.action_handler.mask_view.hide(
            &mut app_system.composite_tree,
            current_sec,
            AppEvent::UIPopupUnmount {
                id: self.action_handler.popup_id,
            },
        );
        self.action_handler
            .frame_view
            .hide(&mut app_system.composite_tree, current_sec);
    }

    pub fn unmount(&self, ct: &mut CompositeTree) {
        self.action_handler.mask_view.unmount_visual(ct);
    }

    pub fn update(&self, ct: &mut CompositeTree, current_sec: f32) {
        self.action_handler.confirm_button.update(ct, current_sec);
    }
}
