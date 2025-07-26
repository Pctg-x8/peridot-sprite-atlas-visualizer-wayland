use std::{cell::Cell, path::Path, rc::Rc};

use crate::{
    AppEvent, AppUpdateContext, PresenterInitContext, ViewInitContext,
    base_system::{
        AppBaseSystem, FontType, scratch_buffer::StagingScratchBufferManager, svg::SinglePathSVG,
    },
    composite::{
        AnimatableColor, AnimatableFloat, AnimationCurve, CompositeMode, CompositeRect,
        CompositeTreeFloatParameterRef, CompositeTreeRef, FloatParameter,
    },
    helper_types::SafeF32,
    hittest::{self, HitTestTreeActionHandler, HitTestTreeData, HitTestTreeRef},
    input::EventContinueControl,
    trigger_cell::TriggerCell,
};

#[derive(Debug, Clone, Copy)]
pub enum Command {
    AddSprite,
    Open,
    Save,
}

struct CommandButtonView {
    ct_root: CompositeTreeRef,
    ct_icon: CompositeTreeRef,
    ct_label: CompositeTreeRef,
    ct_bg_alpha_rate_shown: CompositeTreeFloatParameterRef,
    ct_bg_alpha_rate_pointer: CompositeTreeFloatParameterRef,
    ht_root: HitTestTreeRef,
    icon_svg: SinglePathSVG,
    label: String,
    left: f32,
    show_delay_sec: f32,
    shown: TriggerCell<bool>,
    hovering: Cell<bool>,
    pressing: Cell<bool>,
    is_dirty: Cell<bool>,
    command: Command,
}
impl CommandButtonView {
    const ICON_SIZE: f32 = 24.0;
    const BUTTON_HEIGHT: f32 = Self::ICON_SIZE + 8.0 * 2.0;
    const HPADDING: f32 = 16.0;
    const ICON_LABEL_GAP: f32 = 4.0;

    const CONTENT_COLOR_SHOWN: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const CONTENT_COLOR_HIDDEN: [f32; 4] = [1.0, 1.0, 1.0, 0.0];

    #[tracing::instrument(name = "AppMenuButtonView::new", skip(init), fields(icon_path = %icon_path.as_ref().display()))]
    fn new(
        init: &mut ViewInitContext,
        label: &str,
        icon_path: impl AsRef<Path>,
        left: f32,
        top: f32,
        show_delay_sec: f32,
        command: Command,
    ) -> Self {
        let icon_svg = SinglePathSVG::load(icon_path);

        let bg_atlas_rect = init
            .base_system
            .rounded_fill_rect_mask(
                unsafe { SafeF32::new_unchecked(init.ui_scale_factor) },
                unsafe { SafeF32::new_unchecked(Self::BUTTON_HEIGHT / 2.0) },
            )
            .unwrap();
        let icon_atlas_rect = init
            .base_system
            .rasterize_svg(
                (Self::ICON_SIZE * init.ui_scale_factor).ceil() as _,
                (Self::ICON_SIZE * init.ui_scale_factor).ceil() as _,
                &icon_svg,
            )
            .unwrap();
        let label_atlas_rect = init
            .base_system
            .text_mask(init.staging_scratch_buffer, FontType::UI, label)
            .unwrap();

        let width = (Self::ICON_SIZE + Self::ICON_LABEL_GAP + Self::HPADDING * 2.0)
            + label_atlas_rect.width() as f32 / init.ui_scale_factor;

        let ct_bg_alpha_rate_shown = init
            .base_system
            .composite_tree
            .parameter_store_mut()
            .alloc_float(FloatParameter::Value(0.0));
        let ct_bg_alpha_rate_pointer = init
            .base_system
            .composite_tree
            .parameter_store_mut()
            .alloc_float(FloatParameter::Value(0.0));
        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            offset: [AnimatableFloat::Value(left), AnimatableFloat::Value(top)],
            size: [
                AnimatableFloat::Value(width),
                AnimatableFloat::Value(Self::BUTTON_HEIGHT),
            ],
            has_bitmap: true,
            texatlas_rect: bg_atlas_rect,
            slice_borders: [Self::BUTTON_HEIGHT * 0.5 * init.ui_scale_factor; 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Expression(Box::new(
                move |ps| {
                    let opacity = ps.float_value(ct_bg_alpha_rate_shown) * 0.25
                        + ps.float_value(ct_bg_alpha_rate_pointer) * 0.25;

                    [1.0, 1.0, 1.0, opacity]
                },
            ))),
            ..Default::default()
        });
        let ct_icon = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(Self::ICON_SIZE),
                AnimatableFloat::Value(Self::ICON_SIZE),
            ],
            offset: [
                AnimatableFloat::Value(Self::HPADDING),
                AnimatableFloat::Value(-Self::ICON_SIZE * 0.5),
            ],
            relative_offset_adjustment: [0.0, 0.5],
            has_bitmap: true,
            texatlas_rect: icon_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value(
                Self::CONTENT_COLOR_HIDDEN,
            )),
            ..Default::default()
        });
        let ct_label = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(label_atlas_rect.width() as f32 / init.ui_scale_factor),
                AnimatableFloat::Value(label_atlas_rect.height() as f32 / init.ui_scale_factor),
            ],
            offset: [
                AnimatableFloat::Value(Self::HPADDING + Self::ICON_SIZE + Self::ICON_LABEL_GAP),
                AnimatableFloat::Value(
                    -(label_atlas_rect.height() as f32 / init.ui_scale_factor) * 0.5,
                ),
            ],
            relative_offset_adjustment: [0.0, 0.5],
            has_bitmap: true,
            texatlas_rect: label_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value(
                Self::CONTENT_COLOR_HIDDEN,
            )),
            ..Default::default()
        });

        init.base_system.set_composite_tree_parent(ct_icon, ct_root);
        init.base_system
            .set_composite_tree_parent(ct_label, ct_root);

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            left,
            top,
            width,
            height: Self::BUTTON_HEIGHT,
            ..Default::default()
        });

        Self {
            ct_root,
            ct_icon,
            ct_label,
            ct_bg_alpha_rate_shown,
            ct_bg_alpha_rate_pointer,
            ht_root,
            icon_svg,
            label: label.into(),
            left,
            show_delay_sec,
            shown: TriggerCell::new(false),
            hovering: Cell::new(false),
            pressing: Cell::new(false),
            is_dirty: Cell::new(false),
            command,
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
        base_system
            .free_mask_atlas_rect(base_system.composite_tree.get(self.ct_root).texatlas_rect);
        base_system
            .free_mask_atlas_rect(base_system.composite_tree.get(self.ct_icon).texatlas_rect);
        base_system
            .free_mask_atlas_rect(base_system.composite_tree.get(self.ct_label).texatlas_rect);

        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .texatlas_rect = base_system
            .rounded_fill_rect_mask(unsafe { SafeF32::new_unchecked(ui_scale_factor) }, unsafe {
                SafeF32::new_unchecked(Self::BUTTON_HEIGHT / 2.0)
            })
            .unwrap();
        base_system
            .composite_tree
            .get_mut(self.ct_icon)
            .texatlas_rect = base_system
            .rasterize_svg(
                (Self::ICON_SIZE * ui_scale_factor).ceil() as _,
                (Self::ICON_SIZE * ui_scale_factor).ceil() as _,
                &self.icon_svg,
            )
            .unwrap();
        base_system
            .composite_tree
            .get_mut(self.ct_label)
            .texatlas_rect = base_system
            .text_mask(staging_scratch_buffer, FontType::UI, &self.label)
            .unwrap();

        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .slice_borders = [Self::BUTTON_HEIGHT * 0.5 * ui_scale_factor; 4];

        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .base_scale_factor = ui_scale_factor;
        base_system
            .composite_tree
            .get_mut(self.ct_icon)
            .base_scale_factor = ui_scale_factor;
        base_system
            .composite_tree
            .get_mut(self.ct_label)
            .base_scale_factor = ui_scale_factor;
        base_system.composite_tree.mark_dirty(self.ct_root);
        base_system.composite_tree.mark_dirty(self.ct_icon);
        base_system.composite_tree.mark_dirty(self.ct_label);
    }

    fn update(&self, app_system: &mut AppBaseSystem, current_sec: f32) {
        if let Some(shown) = self.shown.get_if_triggered() {
            if shown {
                app_system.composite_tree.parameter_store_mut().set_float(
                    self.ct_bg_alpha_rate_shown,
                    FloatParameter::Animated {
                        from_value: 0.0,
                        to_value: 1.0,
                        start_sec: current_sec + self.show_delay_sec,
                        end_sec: current_sec + self.show_delay_sec + 0.25,
                        curve: AnimationCurve::Linear,
                        event_on_complete: None,
                    },
                );
                app_system
                    .composite_tree
                    .get_mut(self.ct_icon)
                    .composite_mode = CompositeMode::ColorTint(AnimatableColor::Animated {
                    start_sec: current_sec + self.show_delay_sec,
                    end_sec: current_sec + self.show_delay_sec + 0.25,
                    from_value: Self::CONTENT_COLOR_HIDDEN,
                    to_value: Self::CONTENT_COLOR_SHOWN,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                });
                app_system
                    .composite_tree
                    .get_mut(self.ct_label)
                    .composite_mode = CompositeMode::ColorTint(AnimatableColor::Animated {
                    start_sec: current_sec + self.show_delay_sec,
                    end_sec: current_sec + self.show_delay_sec + 0.25,
                    from_value: Self::CONTENT_COLOR_HIDDEN,
                    to_value: Self::CONTENT_COLOR_SHOWN,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                });
                app_system.composite_tree.get_mut(self.ct_root).offset[0] =
                    AnimatableFloat::Animated {
                        start_sec: current_sec + self.show_delay_sec,
                        end_sec: current_sec + self.show_delay_sec + 0.25,
                        from_value: self.left + 8.0,
                        to_value: self.left,
                        curve: AnimationCurve::CubicBezier {
                            p1: (0.5, 0.5),
                            p2: (0.5, 1.0),
                        },
                        event_on_complete: None,
                    };

                app_system.composite_tree.mark_dirty(self.ct_root);
                app_system.composite_tree.mark_dirty(self.ct_icon);
                app_system.composite_tree.mark_dirty(self.ct_label);
            } else {
                app_system.composite_tree.parameter_store_mut().set_float(
                    self.ct_bg_alpha_rate_shown,
                    FloatParameter::Animated {
                        from_value: 1.0,
                        to_value: 0.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        curve: AnimationCurve::Linear,
                        event_on_complete: None,
                    },
                );
                app_system
                    .composite_tree
                    .get_mut(self.ct_icon)
                    .composite_mode = CompositeMode::ColorTint(AnimatableColor::Animated {
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    from_value: Self::CONTENT_COLOR_SHOWN,
                    to_value: Self::CONTENT_COLOR_HIDDEN,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                });
                app_system
                    .composite_tree
                    .get_mut(self.ct_label)
                    .composite_mode = CompositeMode::ColorTint(AnimatableColor::Animated {
                    start_sec: current_sec,
                    end_sec: current_sec + 0.25,
                    from_value: Self::CONTENT_COLOR_SHOWN,
                    to_value: Self::CONTENT_COLOR_HIDDEN,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                });

                app_system.composite_tree.mark_dirty(self.ct_icon);
                app_system.composite_tree.mark_dirty(self.ct_label);
            }
        }

        if self.is_dirty.replace(false) {
            let current = app_system
                .composite_tree
                .parameter_store()
                .evaluate_float(self.ct_bg_alpha_rate_pointer, current_sec);
            let target = match (self.hovering.get(), self.pressing.get()) {
                (true, true) => 1.0,
                (false, _) => 0.0,
                _ => 0.5,
            };

            app_system.composite_tree.parameter_store_mut().set_float(
                self.ct_bg_alpha_rate_pointer,
                FloatParameter::Animated {
                    from_value: current,
                    to_value: target,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.1,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                },
            );
        }
    }

    pub fn show(&self) {
        self.shown.set(true);
    }

    pub fn hide(&self) {
        self.shown.set(false);
    }

    pub fn on_pointer_enter(&self) {
        self.hovering.set(true);
        self.is_dirty.set(true);
    }

    pub fn on_pointer_leave(&self) {
        // はなれた際はpressingもなかったことにする
        self.hovering.set(false);
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
    ct_root: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    shown: TriggerCell<bool>,
}
impl BaseView {
    #[tracing::instrument(name = "AppMenuBaseView::new", skip(init))]
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

        Self {
            ct_root,
            ht_root,
            shown: TriggerCell::new(false),
        }
    }

    pub fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        app_system.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    pub fn update(&self, app_system: &mut AppBaseSystem, current_sec: f32) {
        if let Some(shown) = self.shown.get_if_triggered() {
            if shown {
                app_system
                    .composite_tree
                    .get_mut(self.ct_root)
                    .composite_mode = CompositeMode::FillColorBackdropBlur(
                    AnimatableColor::Animated {
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        from_value: [0.0, 0.0, 0.0, 0.0],
                        to_value: [0.0, 0.0, 0.0, 0.25],
                        curve: AnimationCurve::Linear,
                        event_on_complete: None,
                    },
                    AnimatableFloat::Animated {
                        from_value: 0.0,
                        to_value: 3.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        curve: AnimationCurve::Linear,
                        event_on_complete: None,
                    },
                );
                app_system.composite_tree.mark_dirty(self.ct_root);
            } else {
                app_system
                    .composite_tree
                    .get_mut(self.ct_root)
                    .composite_mode = CompositeMode::FillColorBackdropBlur(
                    AnimatableColor::Animated {
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        from_value: [0.0, 0.0, 0.0, 0.25],
                        to_value: [0.0, 0.0, 0.0, 0.0],
                        curve: AnimationCurve::Linear,
                        event_on_complete: None,
                    },
                    AnimatableFloat::Animated {
                        from_value: 3.0,
                        to_value: 0.0,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        curve: AnimationCurve::Linear,
                        event_on_complete: None,
                    },
                );
                app_system.composite_tree.mark_dirty(self.ct_root);
            }
        }
    }

    pub fn show(&self) {
        self.shown.set(true);
    }

    pub fn hide(&self) {
        self.shown.set(false);
    }
}

struct ActionHandler {
    base_view: Rc<BaseView>,
    item_views: Rc<[CommandButtonView]>,
    shown: Cell<bool>,
}
impl HitTestTreeActionHandler for ActionHandler {
    fn hit_active(&self, _sender: HitTestTreeRef) -> bool {
        self.shown.get()
    }

    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        for v in self.item_views.iter() {
            if sender == v.ht_root {
                v.on_pointer_enter();
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.base_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_leave(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        for v in self.item_views.iter() {
            if sender == v.ht_root {
                v.on_pointer_leave();
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.base_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        _args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        for v in self.item_views.iter() {
            if sender == v.ht_root {
                v.on_press();
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.base_view.ht_root {
            context.event_queue.push(AppEvent::AppMenuToggle);

            return EventContinueControl::STOP_PROPAGATION
                | EventContinueControl::RECOMPUTE_POINTER_ENTER;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_move(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        if sender == self.base_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
        _args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        for v in self.item_views.iter() {
            if sender == v.ht_root {
                v.on_release();
                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.base_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn on_click(
        &self,
        sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        _args: &hittest::PointerActionArgs,
    ) -> EventContinueControl {
        for v in self.item_views.iter() {
            if sender == v.ht_root {
                match v.command {
                    Command::AddSprite => {
                        context.event_queue.push(AppEvent::AppMenuRequestAddSprite);
                    }
                    Command::Open => {
                        context.event_queue.push(AppEvent::AppMenuRequestOpen);
                    }
                    Command::Save => {
                        context.event_queue.push(AppEvent::AppMenuRequestSave);
                    }
                }

                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        if sender == self.base_view.ht_root {
            return EventContinueControl::STOP_PROPAGATION;
        }

        EventContinueControl::empty()
    }

    fn cursor_shape(
        &self,
        sender: HitTestTreeRef,
        _context: &mut AppUpdateContext,
    ) -> hittest::CursorShape {
        if self.item_views.iter().any(|x| x.ht_root == sender) {
            return hittest::CursorShape::Pointer;
        }

        hittest::CursorShape::Default
    }
}

pub struct Presenter {
    base_view: Rc<BaseView>,
    action_handler: Rc<ActionHandler>,
}
impl Presenter {
    pub fn new(init: &mut PresenterInitContext, header_height: f32) -> Self {
        let base_view = Rc::new(BaseView::new(&mut init.for_view));
        let item_views: Rc<[CommandButtonView]> = Rc::new([
            CommandButtonView::new(
                &mut init.for_view,
                "Add Sprite",
                "resources/icons/add.svg",
                64.0,
                header_height + 32.0,
                0.0,
                Command::AddSprite,
            ),
            CommandButtonView::new(
                &mut init.for_view,
                "Open",
                "resources/icons/open.svg",
                64.0,
                header_height + 32.0 + CommandButtonView::BUTTON_HEIGHT + 16.0,
                0.05,
                Command::Open,
            ),
            CommandButtonView::new(
                &mut init.for_view,
                "Save",
                "resources/icons/save.svg",
                64.0,
                header_height + 32.0 + (CommandButtonView::BUTTON_HEIGHT + 16.0) * 2.0,
                0.05 * 2.0,
                Command::Save,
            ),
        ]);

        for v in item_views.iter() {
            v.mount(
                init.for_view.base_system,
                base_view.ct_root,
                base_view.ht_root,
            );
        }

        let action_handler = Rc::new(ActionHandler {
            base_view: base_view.clone(),
            item_views,
            shown: Cell::new(false),
        });

        init.app_state.register_visible_menu_view_feedback({
            let base_view = Rc::downgrade(&base_view);
            let item_views = Rc::downgrade(&action_handler.item_views);
            let action_handler = Rc::downgrade(&action_handler);

            move |visible| {
                let Some(base_view) = base_view.upgrade() else {
                    // app teardown-ed
                    return;
                };
                let Some(action_handler) = action_handler.upgrade() else {
                    // app teardown-ed
                    return;
                };
                let Some(item_views) = item_views.upgrade() else {
                    // app teardown-ed
                    return;
                };

                if visible {
                    base_view.show();
                    for v in item_views.iter() {
                        v.show();
                    }
                } else {
                    base_view.hide();
                    for v in item_views.iter() {
                        v.hide();
                    }
                }

                action_handler.shown.set(visible);
            }
        });
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(base_view.ht_root, &action_handler);
        for v in action_handler.item_views.iter() {
            init.for_view
                .base_system
                .hit_tree
                .set_action_handler(v.ht_root, &action_handler);
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
        for v in self.action_handler.item_views.iter() {
            v.rescale(base_system, staging_scratch_buffer, ui_scale_factor);
        }
    }

    pub fn update(&self, app_system: &mut AppBaseSystem, current_sec: f32) {
        self.base_view.update(app_system, current_sec);
        for v in self.action_handler.item_views.iter() {
            v.update(app_system, current_sec);
        }
    }
}
