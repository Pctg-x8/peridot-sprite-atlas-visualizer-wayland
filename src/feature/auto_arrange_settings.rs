use std::{cell::Cell, rc::Rc};

use bedrock::{self as br, RenderPass, ShaderModule, VkHandle};

use crate::{
    AppEvent, BLEND_STATE_SINGLE_NONE, FillcolorRConstants, IA_STATE_TRILIST,
    RASTER_STATE_DEFAULT_FILL_NOCULL, VI_STATE_FLOAT2_ONLY, ViewInitContext,
    atlas::AtlasRect,
    base_system::{
        AppBaseSystem, DeviceLocalBuffer, FontType, PixelFormat, RenderTexture, RenderTextureFlags,
        RenderTextureOptions, inject_cmd_begin_render_pass2, inject_cmd_end_render_pass2,
        inject_cmd_pipeline_barrier_2,
    },
    composite::{
        AnimatableColor, AnimatableFloat, AnimationCurve, ClipConfig, CompositeMode, CompositeRect,
        CompositeTreeRef,
    },
    helper_types::SafeF32,
    hittest::{HitTestTreeActionHandler, HitTestTreeData, HitTestTreeRef},
    input::{EventContinueControl, FocusTargetToken},
    uikit::common_controls::CommonButtonView,
};

pub struct Presenter {
    id: uuid::Uuid,
    mask_view: crate::uikit::popup::MaskView,
    frame_view: crate::uikit::popup::CommonFrameView,
    title_label_view: TitleLabelView,
    action_handler: Rc<ActionHandler>,
}
impl crate::uikit::PopupPresenterSpawnable for Presenter {
    type SpawnArgs<'a> = ();

    fn new<'a>(
        init_context: &mut crate::PresenterInitContext,
        id: uuid::Uuid,
        _args: Self::SpawnArgs<'a>,
    ) -> Self {
        let mask_view = crate::uikit::popup::MaskView::new(&mut init_context.for_view);
        let frame_view =
            crate::uikit::popup::CommonFrameView::new(&mut init_context.for_view, 320.0, 480.0);
        let title_label_view = TitleLabelView::new(&mut init_context.for_view);
        let execute_button_view = CommonButtonView::new(&mut init_context.for_view, "Arrange");
        let cancel_button_view = CommonButtonView::new(&mut init_context.for_view, "Cancel");
        let allow_rotated_checkbox_view =
            LabelledCheckboxView::new(&mut init_context.for_view, "Allow rotation");
        let gap_input_field_view =
            LabelledInputFieldView::new(&mut init_context.for_view, "Gap", 4.0 * 6.0, "px");

        title_label_view.mount(init_context.for_view.base_system, frame_view.ct_root());
        execute_button_view.mount(
            init_context.for_view.base_system,
            frame_view.ct_root(),
            frame_view.ht_root(),
        );
        cancel_button_view.mount(
            init_context.for_view.base_system,
            frame_view.ct_root(),
            frame_view.ht_root(),
        );
        allow_rotated_checkbox_view.mount(
            init_context.for_view.base_system,
            frame_view.ct_root(),
            frame_view.ht_root(),
        );
        gap_input_field_view.mount(
            init_context.for_view.base_system,
            (frame_view.ct_root(), frame_view.ht_root()),
        );
        frame_view.mount(
            init_context.for_view.base_system,
            mask_view.ct_root(),
            mask_view.ht_root(),
        );

        execute_button_view.set_position(
            init_context.for_view.base_system,
            -16.0 - execute_button_view.preferred_width(),
            -16.0 - execute_button_view.preferred_height(),
        );
        execute_button_view.set_relative_offset_adjustments(
            init_context.for_view.base_system,
            1.0,
            1.0,
        );
        cancel_button_view.set_position(
            init_context.for_view.base_system,
            -16.0
                - execute_button_view.preferred_width()
                - 8.0
                - cancel_button_view.preferred_width(),
            -16.0 - cancel_button_view.preferred_height(),
        );
        cancel_button_view.set_relative_offset_adjustments(
            init_context.for_view.base_system,
            1.0,
            1.0,
        );
        allow_rotated_checkbox_view.set_position(init_context.for_view.base_system, 16.0, 48.0);
        gap_input_field_view.set_position(
            init_context.for_view.base_system,
            16.0,
            48.0 + 20.0 + 4.0,
        );

        let action_handler = Rc::new(ActionHandler {
            execute_button_view,
            cancel_button_view,
            allow_rotated_checkbox_view,
            gap_input_field_view,
            id,
        });
        mask_view.bind_action_handler(
            &action_handler,
            &mut init_context.for_view.base_system.hit_tree,
        );
        action_handler.execute_button_view.bind_action_handler(
            &action_handler,
            &mut init_context.for_view.base_system.hit_tree,
        );
        action_handler.cancel_button_view.bind_action_handler(
            &action_handler,
            &mut init_context.for_view.base_system.hit_tree,
        );
        action_handler
            .allow_rotated_checkbox_view
            .bind_action_handler(init_context.for_view.base_system, &action_handler);
        action_handler
            .gap_input_field_view
            .bind_action_handler(init_context.for_view.base_system, &action_handler);

        Self {
            id,
            mask_view,
            frame_view,
            title_label_view,
            action_handler,
        }
    }
}
impl crate::uikit::PopupPresenter for Presenter {
    fn show(
        &self,
        base_sys: &mut crate::base_system::AppBaseSystem,
        parents: (
            crate::composite::CompositeTreeRef,
            crate::hittest::HitTestTreeRef,
        ),
        current_sec: f32,
    ) {
        self.mask_view.mount(base_sys, parents.0, parents.1);
        self.mask_view
            .show(&mut base_sys.composite_tree, current_sec);
        self.frame_view
            .show(&mut base_sys.composite_tree, current_sec);
    }

    fn update(&self, base_sys: &mut AppBaseSystem, current_sec: f32) {
        self.action_handler
            .execute_button_view
            .update(&mut base_sys.composite_tree, current_sec);
        self.action_handler
            .cancel_button_view
            .update(&mut base_sys.composite_tree, current_sec);
        self.action_handler
            .allow_rotated_checkbox_view
            .update(base_sys, current_sec);
        self.action_handler.gap_input_field_view.update(base_sys);
    }

    fn hide(&self, base_sys: &mut crate::base_system::AppBaseSystem, current_sec: f32) {
        self.mask_view.unmount_ht(&mut base_sys.hit_tree);
        self.mask_view.hide(
            &mut base_sys.composite_tree,
            current_sec,
            AppEvent::UIPopupUnmount { id: self.id },
        );
        self.frame_view
            .hide(&mut base_sys.composite_tree, current_sec);
    }

    fn unmount(&self, base_sys: &mut crate::base_system::AppBaseSystem) {
        self.mask_view.unmount_visual(&mut base_sys.composite_tree);
    }
}

struct ActionHandler {
    execute_button_view: CommonButtonView,
    cancel_button_view: CommonButtonView,
    allow_rotated_checkbox_view: LabelledCheckboxView,
    gap_input_field_view: LabelledInputFieldView,
    id: uuid::Uuid,
}
impl HitTestTreeActionHandler for ActionHandler {
    fn cursor_shape(
        &self,
        sender: crate::hittest::HitTestTreeRef,
        _context: &mut crate::AppUpdateContext,
    ) -> crate::hittest::CursorShape {
        if let Some(s) = self.execute_button_view.try_handle_cursor_shape(sender) {
            return s;
        }
        if let Some(s) = self.cancel_button_view.try_handle_cursor_shape(sender) {
            return s;
        }
        if let Some(s) = self.gap_input_field_view.try_handle_cursor_shape(sender) {
            return s;
        }

        crate::hittest::CursorShape::Default
    }

    fn keyboard_focus(&self, sender: HitTestTreeRef) -> Option<FocusTargetToken> {
        if let Some(x) = self.gap_input_field_view.try_handle_keyboard_focus(sender) {
            return Some(x);
        }

        None
    }

    fn on_pointer_enter(
        &self,
        sender: crate::hittest::HitTestTreeRef,
        _context: &mut crate::AppUpdateContext,
        _args: &crate::hittest::PointerActionArgs,
    ) -> crate::input::EventContinueControl {
        if self.execute_button_view.is_sender(sender) {
            self.execute_button_view.on_hover();
        }
        if self.cancel_button_view.is_sender(sender) {
            self.cancel_button_view.on_hover();
        }

        crate::input::EventContinueControl::STOP_PROPAGATION
    }

    fn on_pointer_leave(
        &self,
        sender: crate::hittest::HitTestTreeRef,
        _context: &mut crate::AppUpdateContext,
        _args: &crate::hittest::PointerActionArgs,
    ) -> crate::input::EventContinueControl {
        if self.execute_button_view.is_sender(sender) {
            self.execute_button_view.on_leave();
        }
        if self.cancel_button_view.is_sender(sender) {
            self.cancel_button_view.on_leave();
        }

        crate::input::EventContinueControl::STOP_PROPAGATION
    }

    fn on_pointer_move(
        &self,
        _sender: crate::hittest::HitTestTreeRef,
        _context: &mut crate::AppUpdateContext,
        _args: &crate::hittest::PointerActionArgs,
    ) -> crate::input::EventContinueControl {
        crate::input::EventContinueControl::STOP_PROPAGATION
    }

    fn on_pointer_down(
        &self,
        sender: crate::hittest::HitTestTreeRef,
        _context: &mut crate::AppUpdateContext,
        _args: &crate::hittest::PointerActionArgs,
    ) -> crate::input::EventContinueControl {
        if self.execute_button_view.is_sender(sender) {
            self.execute_button_view.on_press();
        }
        if self.cancel_button_view.is_sender(sender) {
            self.cancel_button_view.on_press();
        }

        crate::input::EventContinueControl::STOP_PROPAGATION
    }

    fn on_pointer_up(
        &self,
        sender: crate::hittest::HitTestTreeRef,
        _context: &mut crate::AppUpdateContext,
        _args: &crate::hittest::PointerActionArgs,
    ) -> crate::input::EventContinueControl {
        if self.execute_button_view.is_sender(sender) {
            self.execute_button_view.on_release();
        }
        if self.cancel_button_view.is_sender(sender) {
            self.cancel_button_view.on_release();
        }

        crate::input::EventContinueControl::STOP_PROPAGATION
    }

    fn on_click(
        &self,
        sender: crate::hittest::HitTestTreeRef,
        context: &mut crate::AppUpdateContext,
        _args: &crate::hittest::PointerActionArgs,
    ) -> crate::input::EventContinueControl {
        if self.execute_button_view.is_sender(sender) {
            context
                .event_queue
                .push(AppEvent::UIPopupClose { id: self.id });
            context
                .state
                .borrow_mut()
                .arrange(self.allow_rotated_checkbox_view.checked());
        }
        if self.cancel_button_view.is_sender(sender) {
            context
                .event_queue
                .push(AppEvent::UIPopupClose { id: self.id });
        }
        if let Some(c) = self.allow_rotated_checkbox_view.try_handle_on_click(sender) {
            return c;
        }

        crate::input::EventContinueControl::STOP_PROPAGATION
    }
}

struct TitleLabelView {
    ct_root: CompositeTreeRef,
}
impl TitleLabelView {
    const TEXT: &'static str = "Auto Arrange";
    const TOP_MARGIN: f32 = 16.0;
    const COLOR: [f32; 4] = [0.9, 0.9, 0.9, 1.0];

    fn new(init: &mut ViewInitContext) -> Self {
        let label_atlas_rect = init
            .base_system
            .text_mask(FontType::UI, Self::TEXT)
            .unwrap();

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(label_atlas_rect.width() as f32 / init.ui_scale_factor),
                AnimatableFloat::Value(label_atlas_rect.height() as f32 / init.ui_scale_factor),
            ],
            offset: [
                AnimatableFloat::Value(
                    -0.5 * label_atlas_rect.width() as f32 / init.ui_scale_factor,
                ),
                AnimatableFloat::Value(Self::TOP_MARGIN),
            ],
            relative_offset_adjustment: [0.5, 0.0],
            has_bitmap: true,
            texatlas_rect: label_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value(Self::COLOR)),
            ..Default::default()
        });

        Self { ct_root }
    }

    fn mount(&self, base_sys: &mut AppBaseSystem, parent: CompositeTreeRef) {
        base_sys.set_composite_tree_parent(self.ct_root, parent);
    }

    fn rescale(&self, base_sys: &mut AppBaseSystem, ui_scale_factor: f32) {
        base_sys.free_mask_atlas_rect(self.ct_root.entity(&base_sys.composite_tree).texatlas_rect);
        let label_atlas_rect = base_sys.text_mask(FontType::UI, Self::TEXT).unwrap();

        let ct = self
            .ct_root
            .entity_mut_dirtified(&mut base_sys.composite_tree);
        ct.texatlas_rect = label_atlas_rect;
        ct.base_scale_factor = ui_scale_factor;
    }
}

pub struct LabelledInputFieldView {
    ct_root: CompositeTreeRef,
    ct_cursor: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    ht_field: HitTestTreeRef,
    focus_token: FocusTargetToken,
    focused_render: Cell<bool>,
}
impl LabelledInputFieldView {
    const MARGIN_H_LABEL_FIELD: f32 = 4.0;
    const MARGIN_H_FIELD_UNIT: f32 = 2.0;
    const FIELD_UNDERLINE_THICKNESS: f32 = 1.0;

    pub fn new(init: &mut ViewInitContext, label: &str, value_size: f32, unit: &str) -> Self {
        let label_atlas_rect = init.base_system.text_mask(FontType::UI, label).unwrap();
        let value_atlas_rect = init.base_system.text_mask(FontType::UI, "0").unwrap();
        let unit_atlas_rect = init.base_system.text_mask(FontType::UI, unit).unwrap();

        let preferred_width = label_atlas_rect.width() as f32 / init.ui_scale_factor
            + Self::MARGIN_H_LABEL_FIELD
            + value_size
            + Self::MARGIN_H_FIELD_UNIT
            + unit_atlas_rect.width() as f32 / init.ui_scale_factor;
        let preferred_height = 16.0;
        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(preferred_width),
                AnimatableFloat::Value(preferred_height),
            ],
            ..Default::default()
        });
        let ct_label = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(label_atlas_rect.width() as f32 / init.ui_scale_factor),
                AnimatableFloat::Value(label_atlas_rect.height() as f32 / init.ui_scale_factor),
            ],
            // TODO: baseline alignment
            offset: [
                AnimatableFloat::Value(0.0),
                AnimatableFloat::Value(
                    -0.5 * label_atlas_rect.height() as f32 / init.ui_scale_factor,
                ),
            ],
            relative_offset_adjustment: [0.0, 0.5],
            has_bitmap: true,
            texatlas_rect: label_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });
        let ct_value = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(value_atlas_rect.width() as f32 / init.ui_scale_factor),
                AnimatableFloat::Value(value_atlas_rect.height() as f32 / init.ui_scale_factor),
            ],
            offset: [
                AnimatableFloat::Value(
                    -0.5 * value_atlas_rect.width() as f32 / init.ui_scale_factor,
                ),
                AnimatableFloat::Value(
                    -0.5 * value_atlas_rect.height() as f32 / init.ui_scale_factor,
                ),
            ],
            relative_offset_adjustment: [0.5, 0.5],
            has_bitmap: true,
            texatlas_rect: value_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });
        let ct_unit = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(unit_atlas_rect.width() as f32 / init.ui_scale_factor),
                AnimatableFloat::Value(unit_atlas_rect.height() as f32 / init.ui_scale_factor),
            ],
            // TODO: baseline alignment
            offset: [
                AnimatableFloat::Value(-(unit_atlas_rect.width() as f32) / init.ui_scale_factor),
                AnimatableFloat::Value(
                    -0.5 * unit_atlas_rect.height() as f32 / init.ui_scale_factor,
                ),
            ],
            relative_offset_adjustment: [1.0, 0.5],
            has_bitmap: true,
            texatlas_rect: unit_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });
        let ct_field = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(
                    -(label_atlas_rect.width() as f32 / init.ui_scale_factor
                        + Self::MARGIN_H_LABEL_FIELD
                        + unit_atlas_rect.width() as f32 / init.ui_scale_factor
                        + Self::MARGIN_H_FIELD_UNIT),
                ),
                AnimatableFloat::Value(0.0),
            ],
            relative_size_adjustment: [1.0, 1.0],
            offset: [
                AnimatableFloat::Value(
                    label_atlas_rect.width() as f32 / init.ui_scale_factor
                        + Self::MARGIN_H_LABEL_FIELD,
                ),
                AnimatableFloat::Value(0.0),
            ],
            clip_child: Some(ClipConfig {
                left_softness: SafeF32::ZERO,
                top_softness: SafeF32::ZERO,
                right_softness: SafeF32::ZERO,
                bottom_softness: SafeF32::ZERO,
            }),
            ..Default::default()
        });
        let ct_field_underline = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(0.0),
                AnimatableFloat::Value(Self::FIELD_UNDERLINE_THICKNESS),
            ],
            relative_size_adjustment: [1.0, 0.0],
            offset: [
                AnimatableFloat::Value(0.0),
                AnimatableFloat::Value(-Self::FIELD_UNDERLINE_THICKNESS),
            ],
            relative_offset_adjustment: [0.0, 1.0],
            has_bitmap: true,
            composite_mode: CompositeMode::FillColor(AnimatableColor::Value([
                0.75, 0.75, 0.75, 1.0,
            ])),
            ..Default::default()
        });
        let ct_cursor = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [AnimatableFloat::Value(1.0), AnimatableFloat::Value(12.0)],
            offset: [AnimatableFloat::Value(0.0), AnimatableFloat::Value(-6.0)],
            relative_offset_adjustment: [0.0, 0.5],
            has_bitmap: true,
            composite_mode: CompositeMode::FillColor(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });

        init.base_system
            .set_composite_tree_parent(ct_field_underline, ct_field);
        init.base_system
            .set_composite_tree_parent(ct_value, ct_field);
        init.base_system
            .set_composite_tree_parent(ct_cursor, ct_value);
        init.base_system
            .set_composite_tree_parent(ct_label, ct_root);
        init.base_system
            .set_composite_tree_parent(ct_field, ct_root);
        init.base_system.set_composite_tree_parent(ct_unit, ct_root);

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            width: preferred_width,
            height: preferred_height,
            ..Default::default()
        });
        let ht_field = init.base_system.create_hit_tree(HitTestTreeData {
            left: label_atlas_rect.width() as f32 / init.ui_scale_factor
                + Self::MARGIN_H_LABEL_FIELD,
            width: -(label_atlas_rect.width() as f32 / init.ui_scale_factor
                + Self::MARGIN_H_LABEL_FIELD
                + unit_atlas_rect.width() as f32 / init.ui_scale_factor
                + Self::MARGIN_H_FIELD_UNIT),
            width_adjustment_factor: 1.0,
            height_adjustment_factor: 1.0,
            ..Default::default()
        });

        init.base_system.set_hit_tree_parent(ht_field, ht_root);

        let focus_token = init.base_system.keyboard_focus_manager.acquire_token();

        Self {
            ct_root,
            ct_cursor,
            ht_root,
            ht_field,
            focus_token,
            focused_render: Cell::new(false),
        }
    }

    pub fn mount(&self, base_sys: &mut AppBaseSystem, parents: (CompositeTreeRef, HitTestTreeRef)) {
        base_sys.set_tree_parent((self.ct_root, self.ht_root), parents);
    }

    pub fn update(&self, base_sys: &mut AppBaseSystem) {
        let focused = base_sys.keyboard_focus_manager.has_focus(&self.focus_token);
        if focused != self.focused_render.get() {
            self.focused_render.set(focused);

            self.ct_cursor
                .entity_mut_dirtified(&mut base_sys.composite_tree)
                .opacity = AnimatableFloat::Value(if focused { 1.0 } else { 0.0 });
        }
    }

    pub fn bind_action_handler<'subsystem>(
        &self,
        base_sys: &mut AppBaseSystem<'subsystem>,
        handler: &Rc<impl HitTestTreeActionHandler + 'subsystem>,
    ) {
        base_sys.hit_tree.set_action_handler(self.ht_root, handler);
        base_sys.hit_tree.set_action_handler(self.ht_field, handler);
    }

    pub fn set_position(&self, base_sys: &mut AppBaseSystem, x: f32, y: f32) {
        self.ct_root
            .entity_mut_dirtified(&mut base_sys.composite_tree)
            .offset = [AnimatableFloat::Value(x), AnimatableFloat::Value(y)];
        base_sys.hit_tree.get_data_mut(self.ht_root).left = x;
        base_sys.hit_tree.get_data_mut(self.ht_root).top = y;
    }

    pub fn try_handle_cursor_shape(
        &self,
        sender: HitTestTreeRef,
    ) -> Option<crate::hittest::CursorShape> {
        if sender == self.ht_field {
            return Some(crate::hittest::CursorShape::IBeam);
        }

        None
    }

    pub fn try_handle_keyboard_focus(&self, sender: HitTestTreeRef) -> Option<FocusTargetToken> {
        if sender == self.ht_root || sender == self.ht_field {
            return Some(self.focus_token);
        }

        None
    }
}

const fn o(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

pub struct LabelledCheckboxView {
    ct_root: CompositeTreeRef,
    ct_box_outer: CompositeTreeRef,
    ct_box_check: CompositeTreeRef,
    ct_label: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    label: String,
    checked: Cell<bool>,
    checked_rendered: Cell<bool>,
}
impl LabelledCheckboxView {
    const CHECKICON_SIZE: f32 = 10.0;
    const CHECKICON_THICKNESS: f32 = 3.0;
    const CHECKICON_VERTICES: &'static [[f32; 2]] = &[
        o(
            [0.0, 0.5],
            [0.0, -Self::CHECKICON_THICKNESS * 0.5 / Self::CHECKICON_SIZE],
        ),
        o(
            [0.4, 0.85],
            [0.0, -Self::CHECKICON_THICKNESS * 0.5 / Self::CHECKICON_SIZE],
        ),
        o(
            [1.0, 0.1],
            [0.0, -Self::CHECKICON_THICKNESS * 0.5 / Self::CHECKICON_SIZE],
        ),
        o(
            [0.0, 0.5],
            [0.0, Self::CHECKICON_THICKNESS * 0.5 / Self::CHECKICON_SIZE],
        ),
        o(
            [0.4, 0.85],
            [0.0, Self::CHECKICON_THICKNESS * 0.5 / Self::CHECKICON_SIZE],
        ),
        o(
            [1.0, 0.1],
            [0.0, Self::CHECKICON_THICKNESS * 0.5 / Self::CHECKICON_SIZE],
        ),
    ];
    const CHECKICON_INDICES: &'static [u16] = &[0, 3, 1, 3, 1, 4, 1, 2, 4, 2, 4, 5];

    fn gen_checkicon_surface(base_sys: &mut AppBaseSystem, scale: f32) -> AtlasRect {
        let size_px = (Self::CHECKICON_SIZE * scale).ceil() as u32;
        let atlas_rect = base_sys.alloc_mask_atlas_rect(size_px, size_px);

        let msaa_tempbuf = RenderTexture::new(
            base_sys,
            br::Extent2D::spread1(size_px),
            PixelFormat::R8,
            &RenderTextureOptions {
                msaa_count: Some(4),
                flags: RenderTextureFlags::NON_SAMPLED | RenderTextureFlags::ALLOW_TRANSFER_SRC,
            },
        )
        .unwrap();

        let rp = base_sys
            .create_render_pass(&br::RenderPassCreateInfo2::new(
                &[msaa_tempbuf
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
            ))
            .unwrap();
        let fb = br::FramebufferObject::new(
            base_sys.subsystem,
            &br::FramebufferCreateInfo::new(
                &rp,
                &[msaa_tempbuf.as_transparent_ref()],
                size_px,
                size_px,
            ),
        )
        .unwrap();

        let vsh = base_sys.require_shader("resources/normalized_01_2d.vert");
        let fsh = base_sys.require_shader("resources/fillcolor_r.frag");
        let [pipeline] = base_sys
            .create_graphics_pipelines_array(&[br::GraphicsPipelineCreateInfo::new(
                base_sys.require_empty_pipeline_layout(),
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
                    &[br::Extent2D::spread1(size_px)
                        .into_rect(br::Offset2D::ZERO)
                        .make_viewport(0.0..1.0)],
                    &[br::Extent2D::spread1(size_px).into_rect(br::Offset2D::ZERO)],
                ),
                RASTER_STATE_DEFAULT_FILL_NOCULL,
                BLEND_STATE_SINGLE_NONE,
            )
            .set_multisample_state(
                &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
            )])
            .unwrap();

        let index_offset = Self::CHECKICON_VERTICES.len() * core::mem::size_of::<[f32; 2]>();
        let drawbuf = DeviceLocalBuffer::new(
            base_sys,
            index_offset + Self::CHECKICON_INDICES.len() * core::mem::size_of::<u16>(),
            br::BufferUsage::VERTEX_BUFFER
                | br::BufferUsage::INDEX_BUFFER
                | br::BufferUsage::TRANSFER_DEST,
        )
        .unwrap();

        base_sys
            .sync_execute_graphics_commands(|rec| {
                rec.update_buffer_slice(&drawbuf, 0, Self::CHECKICON_VERTICES)
                    .update_buffer_slice(&drawbuf, index_offset as _, Self::CHECKICON_INDICES)
                    .inject(|r| {
                        inject_cmd_pipeline_barrier_2(
                            r,
                            base_sys.subsystem,
                            &br::DependencyInfo::new(
                                &[br::MemoryBarrier2::new()
                                    .from(
                                        br::PipelineStageFlags2::COPY,
                                        br::AccessFlags2::TRANSFER.write,
                                    )
                                    .to(
                                        br::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT
                                            | br::PipelineStageFlags2::INDEX_INPUT,
                                        br::AccessFlags2::VERTEX_ATTRIBUTE_READ
                                            | br::AccessFlags2::INDEX_READ,
                                    )],
                                &[],
                                &[],
                            ),
                        )
                    })
                    .inject(|r| {
                        inject_cmd_begin_render_pass2(
                            r,
                            base_sys.subsystem,
                            &br::RenderPassBeginInfo::new(
                                &rp,
                                &fb,
                                br::Extent2D::spread1(size_px).into_rect(br::Offset2D::ZERO),
                                &[br::ClearValue::color_f32([0.0; 4])],
                            ),
                            &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                        )
                    })
                    .bind_pipeline(br::PipelineBindPoint::Graphics, &pipeline)
                    .bind_vertex_buffer_array(0, &[drawbuf.as_transparent_ref()], &[0])
                    .bind_index_buffer(&drawbuf, index_offset, br::IndexType::U16)
                    .draw_indexed(Self::CHECKICON_INDICES.len() as _, 1, 0, 0, 0)
                    .inject(|r| {
                        inject_cmd_end_render_pass2(
                            r,
                            base_sys.subsystem,
                            &br::SubpassEndInfo::new(),
                        )
                    })
                    .resolve_image(
                        msaa_tempbuf.as_image(),
                        br::ImageLayout::TransferSrcOpt,
                        base_sys.mask_atlas_image_transparent_ref(),
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
                    .inject(|r| {
                        inject_cmd_pipeline_barrier_2(
                            r,
                            base_sys.subsystem,
                            &br::DependencyInfo::new(
                                &[],
                                &[],
                                &[base_sys
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

        atlas_rect
    }

    pub fn new(init: &mut ViewInitContext, label: &str) -> Self {
        let label_atlas_rect = init.base_system.text_mask(FontType::UI, label).unwrap();
        let border_atlas_rect = init
            .base_system
            .rect_mask(
                unsafe { SafeF32::new_unchecked(init.ui_scale_factor) },
                unsafe { SafeF32::new_unchecked(1.0) },
            )
            .unwrap();
        let checkicon_atlas_rect =
            Self::gen_checkicon_surface(init.base_system, init.ui_scale_factor);

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(
                    16.0 + 4.0 + label_atlas_rect.width() as f32 / init.ui_scale_factor,
                ),
                AnimatableFloat::Value(16.0),
            ],
            ..Default::default()
        });
        let ct_box_outer = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [AnimatableFloat::Value(16.0), AnimatableFloat::Value(16.0)],
            has_bitmap: true,
            texatlas_rect: border_atlas_rect,
            slice_borders: [(1.0 * init.ui_scale_factor).ceil(); 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });
        let ct_box_check = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            relative_size_adjustment: [1.0, 1.0],
            offset: [AnimatableFloat::Value(3.0), AnimatableFloat::Value(3.0)],
            size: [AnimatableFloat::Value(-6.0), AnimatableFloat::Value(-6.0)],
            has_bitmap: true,
            texatlas_rect: checkicon_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([1.0, 1.0, 1.0, 1.0])),
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });
        let ct_label = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(label_atlas_rect.width() as f32 / init.ui_scale_factor),
                AnimatableFloat::Value(label_atlas_rect.height() as f32 / init.ui_scale_factor),
            ],
            offset: [
                AnimatableFloat::Value(16.0 + 4.0),
                AnimatableFloat::Value(
                    -0.5 * label_atlas_rect.height() as f32 / init.ui_scale_factor,
                ),
            ],
            relative_offset_adjustment: [0.0, 0.5],
            has_bitmap: true,
            texatlas_rect: label_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        init.base_system
            .set_composite_tree_parent(ct_box_outer, ct_root);
        init.base_system
            .set_composite_tree_parent(ct_box_check, ct_box_outer);
        init.base_system
            .set_composite_tree_parent(ct_label, ct_root);

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            width: 16.0 + 4.0 + label_atlas_rect.width() as f32 / init.ui_scale_factor,
            height: 16.0,
            ..Default::default()
        });

        Self {
            ct_root,
            ct_box_outer,
            ct_box_check,
            ct_label,
            ht_root,
            label: label.into(),
            checked: Cell::new(false),
            checked_rendered: Cell::new(false),
        }
    }

    pub fn mount(
        &self,
        base_sys: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        base_sys.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    pub fn bind_action_handler<'subsystem>(
        &self,
        base_sys: &mut AppBaseSystem<'subsystem>,
        handler: &Rc<impl HitTestTreeActionHandler + 'subsystem>,
    ) {
        base_sys.hit_tree.set_action_handler(self.ht_root, handler);
    }

    pub fn update(&self, base_sys: &mut AppBaseSystem, current_sec: f32) {
        if self.checked.get() != self.checked_rendered.get() {
            let c = self.checked.get();
            self.ct_box_check
                .entity_mut_dirtified(&mut base_sys.composite_tree)
                .opacity = AnimatableFloat::Animated {
                from_value: if c { 0.0 } else { 1.0 },
                to_value: if c { 1.0 } else { 0.0 },
                start_sec: current_sec,
                end_sec: current_sec + 0.1,
                curve: AnimationCurve::Linear,
                event_on_complete: None,
            };
            self.checked_rendered.set(c);
        }
    }

    pub fn try_handle_on_click(&self, sender: HitTestTreeRef) -> Option<EventContinueControl> {
        if sender == self.ht_root {
            self.toggle();
            return Some(EventContinueControl::STOP_PROPAGATION);
        }

        None
    }

    pub fn set_position(&self, base_sys: &mut AppBaseSystem, x: f32, y: f32) {
        let ct = self
            .ct_root
            .entity_mut_dirtified(&mut base_sys.composite_tree);
        ct.offset = [AnimatableFloat::Value(x), AnimatableFloat::Value(y)];
        base_sys.hit_tree.get_data_mut(self.ht_root).left = x;
        base_sys.hit_tree.get_data_mut(self.ht_root).top = y;
    }

    #[inline]
    pub fn toggle(&self) {
        self.checked.update(|x| !x);
    }

    pub const fn checked(&self) -> bool {
        self.checked.get()
    }
}
