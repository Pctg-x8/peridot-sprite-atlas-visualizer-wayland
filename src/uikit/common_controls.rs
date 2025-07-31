//! Common Component(Standalone View)s

use std::{cell::Cell, rc::Rc};

use crate::{
    ViewInitContext,
    base_system::{AppBaseSystem, FontType},
    composite::{
        AnimatableColor, AnimatableFloat, AnimationCurve, CompositeMode, CompositeRect,
        CompositeTree, CompositeTreeRef,
    },
    helper_types::SafeF32,
    hittest::{
        CursorShape, HitTestTreeActionHandler, HitTestTreeData, HitTestTreeManager, HitTestTreeRef,
    },
};

pub struct CommonButtonView {
    ct_root: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    preferred_width: f32,
    preferred_height: f32,
    hovering: Cell<bool>,
    pressing: Cell<bool>,
    is_dirty: Cell<bool>,
}
impl CommonButtonView {
    const PADDING_H: f32 = 24.0;
    const PADDING_V: f32 = 12.0;
    const CORNER_RADIUS: SafeF32 = unsafe { SafeF32::new_unchecked(12.0) };

    #[tracing::instrument(name = "CommonButtonView::new", skip(init))]
    pub fn new(init: &mut ViewInitContext, label: &str) -> Self {
        let text_atlas_rect = init.base_system.text_mask(FontType::UI, label).unwrap();
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

        let preferred_width =
            Self::PADDING_H * 2.0 + text_atlas_rect.width() as f32 / init.ui_scale_factor;
        let preferred_height =
            Self::PADDING_V * 2.0 + text_atlas_rect.height() as f32 / init.ui_scale_factor;

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            base_scale_factor: init.ui_scale_factor,
            size: [
                AnimatableFloat::Value(preferred_width),
                AnimatableFloat::Value(preferred_height),
            ],
            has_bitmap: true,
            texatlas_rect: frame_image_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS.value() * init.ui_scale_factor.ceil(); 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([1.0, 1.0, 1.0, 0.0])),
            ..Default::default()
        });
        let ct_border = init.base_system.register_composite_rect(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            has_bitmap: true,
            texatlas_rect: frame_border_image_atlas_rect,
            slice_borders: [Self::CORNER_RADIUS.value() * init.ui_scale_factor.ceil(); 4],
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([1.0, 1.0, 1.0, 0.25])),
            ..Default::default()
        });
        let ct_label = init.base_system.register_composite_rect(CompositeRect {
            offset: [
                AnimatableFloat::Value(-0.5 * text_atlas_rect.width() as f32),
                AnimatableFloat::Value(-0.5 * text_atlas_rect.height() as f32),
            ],
            size: [
                AnimatableFloat::Value(text_atlas_rect.width() as f32),
                AnimatableFloat::Value(text_atlas_rect.height() as f32),
            ],
            relative_offset_adjustment: [0.5, 0.5],
            has_bitmap: true,
            texatlas_rect: text_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value([0.9, 0.9, 0.9, 1.0])),
            ..Default::default()
        });

        init.base_system
            .set_composite_tree_parent(ct_border, ct_root);
        init.base_system
            .set_composite_tree_parent(ct_label, ct_root);

        let ht_root = init.base_system.create_hit_tree(HitTestTreeData {
            width: preferred_width,
            height: preferred_height,
            ..Default::default()
        });

        Self {
            ct_root,
            ht_root,
            preferred_width,
            preferred_height,
            hovering: Cell::new(false),
            pressing: Cell::new(false),
            is_dirty: Cell::new(false),
        }
    }

    pub const fn preferred_width(&self) -> f32 {
        self.preferred_width
    }

    pub const fn preferred_height(&self) -> f32 {
        self.preferred_height
    }

    #[inline]
    pub fn ct_mut<'c>(&self, ct: &'c mut CompositeTree) -> &'c mut CompositeRect {
        ct.get_mut(self.ct_root)
    }

    #[inline]
    pub fn ht_mut<'h, 'ah>(
        &self,
        ht: &'h mut HitTestTreeManager<'ah>,
    ) -> &'h mut HitTestTreeData<'ah> {
        ht.get_data_mut(self.ht_root)
    }

    #[inline]
    pub fn bind_action_handler(
        &self,
        action_handler: &Rc<impl HitTestTreeActionHandler + 'static>,
        ht: &mut HitTestTreeManager,
    ) {
        ht.set_action_handler(self.ht_root, action_handler);
    }

    #[inline]
    pub fn is_sender(&self, sender: HitTestTreeRef) -> bool {
        sender == self.ht_root
    }

    pub fn mount(
        &self,
        app_system: &mut AppBaseSystem,
        ct_parent: CompositeTreeRef,
        ht_parent: HitTestTreeRef,
    ) {
        app_system.set_tree_parent((self.ct_root, self.ht_root), (ct_parent, ht_parent));
    }

    pub fn update(&self, ct: &mut CompositeTree, current_sec: f32) {
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
            CompositeMode::ColorTint(AnimatableColor::Animated {
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
        ct.mark_dirty(self.ct_root);
    }

    #[inline]
    pub fn try_handle_cursor_shape(&self, sender: HitTestTreeRef) -> Option<CursorShape> {
        if self.is_sender(sender) {
            Some(CursorShape::Pointer)
        } else {
            None
        }
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
