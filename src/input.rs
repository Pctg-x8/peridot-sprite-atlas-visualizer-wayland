use bitflags::bitflags;

use crate::hittest::{CursorShape, HitTestTreeManager, HitTestTreeRef, PointerActionArgs};

bitflags! {
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct EventContinueControl: usize {
        const STOP_PROPAGATION = 1 << 0;
        const CAPTURE_ELEMENT = 1 << 1;
        const RELEASE_CAPTURE_ELEMENT = 1 << 2;
        const RECOMPUTE_POINTER_ENTER = 1 << 3;
    }
}

enum PointerFocusState {
    None,
    Entering(HitTestTreeRef),
    Capturing(HitTestTreeRef),
}

pub struct PointerInputManager {
    last_client_pointer_pos: Option<(f32, f32)>,
    pointer_focus: PointerFocusState,
    click_base_client_pointer_pos: Option<(f32, f32)>,
}
impl PointerInputManager {
    const CLICK_DETECTION_MAX_DISTANCE: f32 = 4.0;

    pub fn new() -> Self {
        PointerInputManager {
            last_client_pointer_pos: None,
            pointer_focus: PointerFocusState::None,
            click_base_client_pointer_pos: None,
        }
    }

    fn handle_mouse_enter_leave<ActionContext>(
        &mut self,
        client_x: f32,
        client_y: f32,
        client_width: f32,
        client_height: f32,
        ht: &mut HitTestTreeManager<ActionContext>,
        action_context: &mut ActionContext,
        ht_root: HitTestTreeRef,
    ) {
        let new_hit = ht.test(
            action_context,
            ht_root,
            client_x,
            client_y,
            0.0,
            0.0,
            client_width,
            client_height,
        );
        if let PointerFocusState::Entering(ht_ref) = self.pointer_focus {
            if Some(ht_ref) != new_hit {
                // entering changed
                let mut p = Some(ht_ref);
                while let Some(ht_ref) = p {
                    let flags = ht.get_data(ht_ref).action_handler().map_or(
                        EventContinueControl::empty(),
                        |h| {
                            h.on_pointer_leave(
                                ht_ref,
                                action_context,
                                ht,
                                PointerActionArgs {
                                    client_x,
                                    client_y,
                                    client_width,
                                    client_height,
                                },
                            )
                        },
                    );
                    if flags.contains(EventContinueControl::STOP_PROPAGATION) {
                        break;
                    }

                    p = ht.parent_of(ht_ref);
                }

                if let Some(ht_ref) = new_hit {
                    let mut p = Some(ht_ref);
                    while let Some(ht_ref) = p {
                        let flags = ht.get_data(ht_ref).action_handler().map_or(
                            EventContinueControl::empty(),
                            |h| {
                                h.on_pointer_enter(
                                    ht_ref,
                                    action_context,
                                    ht,
                                    PointerActionArgs {
                                        client_x,
                                        client_y,
                                        client_width,
                                        client_height,
                                    },
                                )
                            },
                        );
                        if flags.contains(EventContinueControl::STOP_PROPAGATION) {
                            break;
                        }

                        p = ht.parent_of(ht_ref);
                    }
                }
            }
        }

        self.pointer_focus = match new_hit {
            None => PointerFocusState::None,
            Some(ht_ref) => PointerFocusState::Entering(ht_ref),
        };
    }

    pub fn handle_mouse_move<ActionContext>(
        &mut self,
        client_x: f32,
        client_y: f32,
        client_width: f32,
        client_height: f32,
        ht: &mut HitTestTreeManager<ActionContext>,
        action_context: &mut ActionContext,
        ht_root: HitTestTreeRef,
    ) {
        self.last_client_pointer_pos = Some((client_x, client_y));

        if let Some((cbx, cby)) = self.click_base_client_pointer_pos {
            let d_sq = (client_x - cbx).powi(2) + (client_y - cby).powi(2);

            if d_sq >= Self::CLICK_DETECTION_MAX_DISTANCE.powi(2) {
                // 動きすぎたのでクリック状態を解除
                self.click_base_client_pointer_pos = None;
            }
        }

        if let PointerFocusState::Capturing(ht_ref) = self.pointer_focus {
            // キャプチャ中の要素があればそれにだけ流す
            if let Some(h) = ht.get_data(ht_ref).action_handler() {
                h.on_pointer_move(
                    ht_ref,
                    action_context,
                    ht,
                    PointerActionArgs {
                        client_x,
                        client_y,
                        client_width,
                        client_height,
                    },
                );
            }

            return;
        }

        self.handle_mouse_enter_leave(
            client_x,
            client_y,
            client_width,
            client_height,
            ht,
            action_context,
            ht_root,
        );

        let mut p = match self.pointer_focus {
            PointerFocusState::Entering(ht_ref) => Some(ht_ref),
            _ => None,
        };
        while let Some(ht_ref) = p {
            let flags =
                ht.get_data(ht_ref)
                    .action_handler()
                    .map_or(EventContinueControl::empty(), |h| {
                        h.on_pointer_move(
                            ht_ref,
                            action_context,
                            ht,
                            PointerActionArgs {
                                client_x,
                                client_y,
                                client_width,
                                client_height,
                            },
                        )
                    });
            if flags.contains(EventContinueControl::RECOMPUTE_POINTER_ENTER) {
                self.handle_mouse_enter_leave(
                    client_x,
                    client_y,
                    client_width,
                    client_height,
                    ht,
                    action_context,
                    ht_root,
                );
            }
            if flags.contains(EventContinueControl::STOP_PROPAGATION) {
                break;
            }

            p = ht.parent_of(ht_ref);
        }
    }

    pub fn handle_mouse_left_down<ActionContext>(
        &mut self,
        client_x: f32,
        client_y: f32,
        client_width: f32,
        client_height: f32,
        ht: &mut HitTestTreeManager<ActionContext>,
        action_context: &mut ActionContext,
        ht_root: HitTestTreeRef,
    ) {
        self.click_base_client_pointer_pos = Some((client_x, client_y));

        match self.pointer_focus {
            PointerFocusState::Capturing(ht_ref) => {
                let flags = ht.get_data(ht_ref).action_handler().map_or(
                    EventContinueControl::empty(),
                    |h| {
                        h.on_pointer_down(
                            ht_ref,
                            action_context,
                            ht,
                            PointerActionArgs {
                                client_x,
                                client_y,
                                client_width,
                                client_height,
                            },
                        )
                    },
                );
                if flags.contains(EventContinueControl::RECOMPUTE_POINTER_ENTER) {
                    self.handle_mouse_enter_leave(
                        client_x,
                        client_y,
                        client_width,
                        client_height,
                        ht,
                        action_context,
                        ht_root,
                    );
                }
                if flags.contains(EventContinueControl::RELEASE_CAPTURE_ELEMENT) {
                    // TODO: release native pointer capture here
                    self.pointer_focus = PointerFocusState::Entering(ht_ref);
                    self.handle_mouse_enter_leave(
                        client_x,
                        client_y,
                        client_width,
                        client_height,
                        ht,
                        action_context,
                        ht_root,
                    );
                }
            }
            PointerFocusState::Entering(ht_ref) => {
                let mut p = Some(ht_ref);
                while let Some(ht_ref) = p {
                    let flags = ht.get_data(ht_ref).action_handler().map_or(
                        EventContinueControl::empty(),
                        |h| {
                            h.on_pointer_down(
                                ht_ref,
                                action_context,
                                ht,
                                PointerActionArgs {
                                    client_x,
                                    client_y,
                                    client_width,
                                    client_height,
                                },
                            )
                        },
                    );
                    if flags.contains(EventContinueControl::RECOMPUTE_POINTER_ENTER) {
                        self.handle_mouse_enter_leave(
                            client_x,
                            client_y,
                            client_width,
                            client_height,
                            ht,
                            action_context,
                            ht_root,
                        );
                    }
                    if flags.contains(EventContinueControl::CAPTURE_ELEMENT) {
                        self.pointer_focus = PointerFocusState::Capturing(ht_ref);
                        // TODO: capture native pointer here
                    }
                    if flags.contains(EventContinueControl::STOP_PROPAGATION) {
                        break;
                    }

                    p = ht.parent_of(ht_ref);
                }
            }
            PointerFocusState::None => (),
        }
    }

    pub fn handle_mouse_left_up<ActionContext>(
        &mut self,
        client_x: f32,
        client_y: f32,
        client_width: f32,
        client_height: f32,
        ht: &mut HitTestTreeManager<ActionContext>,
        action_context: &mut ActionContext,
        ht_root: HitTestTreeRef,
    ) {
        match self.pointer_focus {
            PointerFocusState::Capturing(ht_ref) => {
                let flags = ht.get_data(ht_ref).action_handler().map_or(
                    EventContinueControl::empty(),
                    |h| {
                        h.on_pointer_up(
                            ht_ref,
                            action_context,
                            ht,
                            PointerActionArgs {
                                client_x,
                                client_y,
                                client_width,
                                client_height,
                            },
                        )
                    },
                );
                if flags.contains(EventContinueControl::RECOMPUTE_POINTER_ENTER) {
                    self.handle_mouse_enter_leave(
                        client_x,
                        client_y,
                        client_width,
                        client_height,
                        ht,
                        action_context,
                        ht_root,
                    );
                }
                if flags.contains(EventContinueControl::RELEASE_CAPTURE_ELEMENT) {
                    // TODO: release native pointer capture here
                    self.pointer_focus = PointerFocusState::Entering(ht_ref);
                    self.handle_mouse_enter_leave(
                        client_x,
                        client_y,
                        client_width,
                        client_height,
                        ht,
                        action_context,
                        ht_root,
                    );
                }
            }
            PointerFocusState::Entering(ht_ref) => {
                let mut p = Some(ht_ref);
                while let Some(ht_ref) = p {
                    let flags = ht.get_data(ht_ref).action_handler().map_or(
                        EventContinueControl::empty(),
                        |h| {
                            h.on_pointer_up(
                                ht_ref,
                                action_context,
                                ht,
                                PointerActionArgs {
                                    client_x,
                                    client_y,
                                    client_width,
                                    client_height,
                                },
                            )
                        },
                    );
                    if flags.contains(EventContinueControl::RECOMPUTE_POINTER_ENTER) {
                        self.handle_mouse_enter_leave(
                            client_x,
                            client_y,
                            client_width,
                            client_height,
                            ht,
                            action_context,
                            ht_root,
                        );
                    }
                    if flags.contains(EventContinueControl::CAPTURE_ELEMENT) {
                        self.pointer_focus = PointerFocusState::Capturing(ht_ref);
                        // TODO: capture native pointer here
                    }
                    if flags.contains(EventContinueControl::STOP_PROPAGATION) {
                        break;
                    }

                    p = ht.parent_of(ht_ref);
                }
            }
            PointerFocusState::None => (),
        }

        if self.click_base_client_pointer_pos.take().is_some() {
            // クリック判定持続してた
            match self.pointer_focus {
                PointerFocusState::Capturing(ht_ref) => {
                    let flags = ht.get_data(ht_ref).action_handler().map_or(
                        EventContinueControl::empty(),
                        |h| {
                            h.on_click(
                                ht_ref,
                                action_context,
                                ht,
                                PointerActionArgs {
                                    client_x,
                                    client_y,
                                    client_width,
                                    client_height,
                                },
                            )
                        },
                    );
                    if flags.contains(EventContinueControl::RECOMPUTE_POINTER_ENTER) {
                        self.handle_mouse_enter_leave(
                            client_x,
                            client_y,
                            client_width,
                            client_height,
                            ht,
                            action_context,
                            ht_root,
                        );
                    }
                    if flags.contains(EventContinueControl::RELEASE_CAPTURE_ELEMENT) {
                        // TODO: release native pointer capture here
                        self.pointer_focus = PointerFocusState::Entering(ht_ref);
                        self.handle_mouse_enter_leave(
                            client_x,
                            client_y,
                            client_width,
                            client_height,
                            ht,
                            action_context,
                            ht_root,
                        );
                    }
                }
                PointerFocusState::Entering(ht_ref) => {
                    let mut p = Some(ht_ref);
                    while let Some(ht_ref) = p {
                        let flags = ht.get_data(ht_ref).action_handler().map_or(
                            EventContinueControl::empty(),
                            |h| {
                                h.on_click(
                                    ht_ref,
                                    action_context,
                                    ht,
                                    PointerActionArgs {
                                        client_x,
                                        client_y,
                                        client_width,
                                        client_height,
                                    },
                                )
                            },
                        );
                        if flags.contains(EventContinueControl::RECOMPUTE_POINTER_ENTER) {
                            self.handle_mouse_enter_leave(
                                client_x,
                                client_y,
                                client_width,
                                client_height,
                                ht,
                                action_context,
                                ht_root,
                            );
                        }
                        if flags.contains(EventContinueControl::CAPTURE_ELEMENT) {
                            self.pointer_focus = PointerFocusState::Capturing(ht_ref);
                            // TODO: capture native pointer here
                        }
                        if flags.contains(EventContinueControl::STOP_PROPAGATION) {
                            break;
                        }

                        p = ht.parent_of(ht_ref);
                    }
                }
                PointerFocusState::None => (),
            }
        }
    }

    pub fn cursor_shape<ActionContext>(
        &self,
        ht: &mut HitTestTreeManager<ActionContext>,
        action_context: &mut ActionContext,
    ) -> CursorShape {
        match self.pointer_focus {
            PointerFocusState::Capturing(ht_ref) => ht
                .get_data(ht_ref)
                .action_handler()
                .map_or(CursorShape::Default, |h| {
                    h.cursor_shape(ht_ref, action_context)
                }),
            PointerFocusState::Entering(ht_ref) => {
                let mut p = Some(ht_ref);
                while let Some(ht_ref) = p {
                    if let Some(cursor_shape) = ht
                        .get_data(ht_ref)
                        .action_handler()
                        .map(|h| h.cursor_shape(ht_ref, action_context))
                    {
                        return cursor_shape;
                    }

                    p = ht.parent_of(ht_ref);
                }

                // fallback
                CursorShape::Default
            }
            PointerFocusState::None => CursorShape::Default,
        }
    }
}
