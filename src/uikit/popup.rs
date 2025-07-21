use std::{collections::HashMap, rc::Rc};

use crate::{
    AppEvent, PresenterInitContext, ViewInitContext,
    base_system::AppBaseSystem,
    composite::{
        AnimatableColor, AnimatableFloat, AnimationCurve, CompositeMode, CompositeRect,
        CompositeTree, CompositeTreeRef,
    },
    helper_types::SafeF32,
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
    const CORNER_RADIUS: SafeF32 = unsafe { SafeF32::new_unchecked(16.0) };

    pub fn new(init: &mut ViewInitContext, width: f32, height: f32) -> Self {
        let frame_image_atlas_rect = init
            .base_system
            .rounded_fill_rect_mask(
                unsafe { SafeF32::new_unchecked(init.ui_scale_factor.ceil()) },
                Self::CORNER_RADIUS,
            )
            .unwrap();
        let frame_border_image_atlas_rect = init
            .base_system
            .rounded_rect_mask(
                unsafe { SafeF32::new_unchecked(init.ui_scale_factor.ceil()) },
                Self::CORNER_RADIUS,
                unsafe { SafeF32::new_unchecked(1.0) },
            )
            .unwrap();

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
            slice_borders: [Self::CORNER_RADIUS.value() * init.ui_scale_factor.ceil(); 4],
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
            slice_borders: [Self::CORNER_RADIUS.value() * init.ui_scale_factor.ceil(); 4],
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

// TODO: message_dialog限定なのをあとでなおす
pub struct PopupManager {
    instance_by_id: HashMap<uuid::Uuid, super::message_dialog::Presenter>,
    hit_base_layer: HitTestTreeRef,
    composite_base_layer: CompositeTreeRef,
}
impl PopupManager {
    pub fn new(hit_base_layer: HitTestTreeRef, composite_base_layer: CompositeTreeRef) -> Self {
        Self {
            instance_by_id: HashMap::new(),
            hit_base_layer,
            composite_base_layer,
        }
    }

    pub fn spawn(
        &mut self,
        presenter_init_context: &mut PresenterInitContext,
        current_sec: f32,
        content: &str,
    ) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        let presenter = super::message_dialog::Presenter::new(presenter_init_context, id, content);
        presenter.show(
            presenter_init_context.for_view.base_system,
            self.composite_base_layer,
            self.hit_base_layer,
            current_sec,
        );
        // TODO: ここでRECOMPUTE_POINTER_ENTER相当の処理をしないといけない(ポインタを動かさないかぎりEnter状態が続くのでマスクを貫通できる)
        // クローズしたときも同じ

        self.instance_by_id.insert(id, presenter);
        id
    }

    pub fn close(&self, base_system: &mut AppBaseSystem, current_sec: f32, id: &uuid::Uuid) {
        let Some(inst) = self.instance_by_id.get(id) else {
            // no instance bound
            return;
        };

        inst.hide(base_system, current_sec);
    }

    pub fn remove(&mut self, base_system: &mut AppBaseSystem, id: &uuid::Uuid) {
        let Some(inst) = self.instance_by_id.remove(id) else {
            // no instance bound
            return;
        };

        inst.unmount(&mut base_system.composite_tree);
    }

    pub fn update(&mut self, base_system: &mut AppBaseSystem, current_sec: f32) {
        for x in self.instance_by_id.values() {
            x.update(&mut base_system.composite_tree, current_sec);
        }
    }
}
