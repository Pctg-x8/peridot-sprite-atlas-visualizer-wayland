use std::collections::BTreeSet;

use crate::input::EventContinueControl;

pub struct HitTestTreeData<ActionContext> {
    pub left: f32,
    pub top: f32,
    pub left_adjustment_factor: f32,
    pub top_adjustment_factor: f32,
    pub width: f32,
    pub height: f32,
    pub width_adjustment_factor: f32,
    pub height_adjustment_factor: f32,
    pub action_handler:
        Option<std::rc::Weak<dyn HitTestTreeActionHandler<Context = ActionContext>>>,
}
impl<ActionContext> Default for HitTestTreeData<ActionContext> {
    #[inline]
    fn default() -> Self {
        Self {
            left: 0.0,
            top: 0.0,
            left_adjustment_factor: 0.0,
            top_adjustment_factor: 0.0,
            width: 0.0,
            height: 0.0,
            width_adjustment_factor: 0.0,
            height_adjustment_factor: 0.0,
            action_handler: None,
        }
    }
}
impl<ActionContext> HitTestTreeData<ActionContext> {
    #[inline]
    pub fn action_handler(
        &self,
    ) -> Option<std::rc::Rc<dyn HitTestTreeActionHandler<Context = ActionContext>>> {
        self.action_handler
            .as_ref()
            .and_then(std::rc::Weak::upgrade)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HitTestTreeRef(usize);

struct HitTestTreeRelationData {
    parent: Option<usize>,
    children: Vec<usize>,
}

pub struct HitTestTreeManager<ActionContext> {
    data: Vec<HitTestTreeData<ActionContext>>,
    relations: Vec<HitTestTreeRelationData>,
    free_index: BTreeSet<usize>,
}
impl<ActionContext> HitTestTreeManager<ActionContext> {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            relations: Vec::new(),
            free_index: BTreeSet::new(),
        }
    }

    pub fn create(&mut self, data: HitTestTreeData<ActionContext>) -> HitTestTreeRef {
        if let Some(x) = self.free_index.pop_first() {
            self.data[x] = data;
            self.relations[x].parent = None;
            self.relations[x].children.clear();

            return HitTestTreeRef(x);
        }

        self.data.push(data);
        self.relations.push(HitTestTreeRelationData {
            parent: None,
            children: Vec::new(),
        });

        HitTestTreeRef(self.data.len() - 1)
    }

    #[inline]
    pub fn get_data(&self, r: HitTestTreeRef) -> &HitTestTreeData<ActionContext> {
        &self.data[r.0]
    }

    #[inline]
    pub fn get_data_mut(&mut self, r: HitTestTreeRef) -> &mut HitTestTreeData<ActionContext> {
        &mut self.data[r.0]
    }

    #[inline]
    pub fn parent_of(&self, r: HitTestTreeRef) -> Option<HitTestTreeRef> {
        self.relations
            .get(r.0)
            .and_then(|r| r.parent)
            .map(HitTestTreeRef)
    }

    pub fn add_child(&mut self, parent: HitTestTreeRef, child: HitTestTreeRef) {
        if let Some(old_parent) = self.relations[child.0].parent.replace(parent.0) {
            // 古い親から外す
            self.relations[old_parent]
                .children
                .retain(|&x| x != child.0);
        }

        self.relations[parent.0].children.push(child.0);
    }

    pub fn remove_child(&mut self, child: HitTestTreeRef) {
        let Some(p) = self.relations[child.0].parent.take() else {
            // 親なし
            return;
        };

        self.relations[p].children.retain(|&x| x != child.0);
    }

    pub fn dump(&self, root: HitTestTreeRef) {
        fn rec<ActionContext>(this: &HitTestTreeManager<ActionContext>, r: usize, indent: usize) {
            for _ in 0..indent {
                print!("  ");
            }

            let HitTestTreeData {
                left,
                top,
                left_adjustment_factor,
                top_adjustment_factor,
                width,
                height,
                width_adjustment_factor,
                height_adjustment_factor,
                ..
            } = this.data[r];
            println!(
                "#{r}: (x{left_adjustment_factor}+{left}, x{top_adjustment_factor}+{top}) size (x{width_adjustment_factor}+{width}, x{height_adjustment_factor}+{height})"
            );

            for &c in &this.relations[r].children {
                rec(this, c, indent + 1);
            }
        }

        rec(self, root.0, 0);
    }

    pub fn translate_client_to_tree_local(
        &self,
        target: HitTestTreeRef,
        client_x: f32,
        client_y: f32,
        client_width: f32,
        client_height: f32,
    ) -> (f32, f32, f32, f32) {
        let d = &self.data[target.0];
        match self.relations[target.0].parent {
            None => {
                // parent = clientなので直接計算する
                let effective_left = client_width * d.left_adjustment_factor + d.left;
                let effective_top = client_height * d.top_adjustment_factor + d.top;

                (
                    client_x - effective_left,
                    client_y - effective_top,
                    client_width * d.width_adjustment_factor + d.width,
                    client_height * d.height_adjustment_factor + d.height,
                )
            }
            Some(p) => {
                // 親でいっかい計算して、その中のローカル座標として計算する
                let (
                    parent_local_x,
                    parent_local_y,
                    parent_effective_width,
                    parent_effective_height,
                ) = self.translate_client_to_tree_local(
                    HitTestTreeRef(p),
                    client_x,
                    client_y,
                    client_width,
                    client_height,
                );
                let effective_left = parent_effective_width * d.left_adjustment_factor + d.left;
                let effective_top = parent_effective_height * d.top_adjustment_factor + d.top;

                (
                    parent_local_x - effective_left,
                    parent_local_y - effective_top,
                    parent_effective_width * d.width_adjustment_factor + d.width,
                    parent_effective_height * d.height_adjustment_factor + d.height,
                )
            }
        }
    }

    pub fn test(
        &self,
        context: &ActionContext,
        root: HitTestTreeRef,
        global_x: f32,
        global_y: f32,
        parent_global_left: f32,
        parent_global_top: f32,
        parent_effective_width: f32,
        parent_effective_height: f32,
    ) -> Option<HitTestTreeRef> {
        let d = &self.data[root.0];
        if !d
            .action_handler
            .as_ref()
            .and_then(std::rc::Weak::upgrade)
            .map_or(true, |x| x.hit_active(root, context))
        {
            // hit disabled
            return None;
        }

        let (global_left, global_top, effective_width, effective_height) = (
            parent_global_left + parent_effective_width * d.left_adjustment_factor + d.left,
            parent_global_top + parent_effective_height * d.top_adjustment_factor + d.top,
            parent_effective_width * d.width_adjustment_factor + d.width,
            parent_effective_height * d.height_adjustment_factor + d.height,
        );
        let (global_right, global_bottom) =
            (global_left + effective_width, global_top + effective_height);

        // 後ろにあるほうが上なので優先して見る
        if let Some(t) = self.relations[root.0].children.iter().rev().find_map(|&c| {
            self.test(
                context,
                HitTestTreeRef(c),
                global_x,
                global_y,
                global_left,
                global_top,
                effective_width,
                effective_height,
            )
        }) {
            // 子にヒット
            return Some(t);
        }

        if global_left <= global_x
            && global_x <= global_right
            && global_top <= global_y
            && global_y <= global_bottom
        {
            // 自分にヒット
            return Some(root);
        }

        // なににもヒットしなかった
        None
    }
}

#[derive(Clone, Copy)]
pub enum CursorShape {
    Default,
    ResizeHorizontal,
}

pub struct PointerActionArgs {
    pub client_x: f32,
    pub client_y: f32,
    pub client_width: f32,
    pub client_height: f32,
}

pub trait HitTestTreeActionHandler {
    type Context;

    #[allow(unused_variables)]
    #[inline]
    fn hit_active(&self, sender: HitTestTreeRef, context: &Self::Context) -> bool {
        true
    }

    #[allow(unused_variables)]
    #[inline]
    fn cursor_shape(&self, sender: HitTestTreeRef, context: &mut Self::Context) -> CursorShape {
        CursorShape::Default
    }

    #[allow(unused_variables)]
    fn on_pointer_enter(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        args: PointerActionArgs,
    ) -> EventContinueControl {
        EventContinueControl::empty()
    }

    #[allow(unused_variables)]
    fn on_pointer_leave(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        args: PointerActionArgs,
    ) -> EventContinueControl {
        EventContinueControl::empty()
    }

    #[allow(unused_variables)]
    fn on_pointer_move(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        args: PointerActionArgs,
    ) -> EventContinueControl {
        EventContinueControl::empty()
    }

    #[allow(unused_variables)]
    fn on_pointer_down(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        args: PointerActionArgs,
    ) -> EventContinueControl {
        EventContinueControl::empty()
    }

    #[allow(unused_variables)]
    fn on_pointer_up(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        args: PointerActionArgs,
    ) -> EventContinueControl {
        EventContinueControl::empty()
    }

    #[allow(unused_variables)]
    fn on_click(
        &self,
        sender: HitTestTreeRef,
        context: &mut Self::Context,
        ht: &mut HitTestTreeManager<Self::Context>,
        args: PointerActionArgs,
    ) -> EventContinueControl {
        EventContinueControl::empty()
    }
}
