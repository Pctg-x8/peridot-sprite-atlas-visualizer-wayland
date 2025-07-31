use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use bedrock::{
    self as br, DescriptorPoolMut, Device, DeviceMemoryMut, ImageChild, MemoryBound, ShaderModule,
    VkHandle, VkObject,
};
use image::EncodableLayout;
use parking_lot::RwLock;

use crate::{
    AppEvent, AppUpdateContext, BLEND_STATE_SINGLE_NONE, BLEND_STATE_SINGLE_PREMULTIPLIED,
    IA_STATE_TRILIST, IA_STATE_TRISTRIP, MS_STATE_EMPTY, PresenterInitContext,
    RASTER_STATE_DEFAULT_FILL_NOCULL, VI_STATE_EMPTY, VI_STATE_FLOAT4_ONLY, ViewInitContext,
    app_state::{AppState, SpriteInfo},
    atlas::{AtlasRect, DynamicAtlasManager},
    base_system::{
        AppBaseSystem, inject_cmd_pipeline_barrier_2,
        scratch_buffer::{
            FlippableStagingScratchBufferGroup, StagingScratchBuffer, StagingScratchBufferMapMode,
        },
    },
    bg_worker::{BackgroundWork, BackgroundWorkerEnqueueAccess},
    composite::{
        AnimatableColor, AnimatableFloat, AnimationCurve, CompositeMode, CompositeRect,
        CompositeTree, CompositeTreeFloatParameterRef, CompositeTreeRef, CustomRenderToken,
        FloatParameter,
    },
    coordinate::SizePixels,
    helper_types::SafeF32,
    hittest::{HitTestTreeActionHandler, HitTestTreeData, HitTestTreeRef, PointerActionArgs},
    input::EventContinueControl,
    quadtree::QuadTree,
    subsystem::Subsystem,
};

pub struct Presenter<'subsystem> {
    sprites_dirty: Rc<Cell<bool>>,
    action_handler: Rc<ActionHandler<'subsystem>>,
}
impl<'subsystem> Presenter<'subsystem> {
    pub fn new(
        init: &mut PresenterInitContext<'_, '_, '_, 'subsystem>,
        rendered_pass: br::SubpassRef<impl br::RenderPass + ?Sized>,
        main_buffer_size: br::Extent2D,
    ) -> Self {
        let grid_view = GridView::new(
            &mut init.for_view,
            rendered_pass,
            main_buffer_size,
            SizePixels {
                width: 32,
                height: 32,
            },
        );
        let marker_view = CurrentSelectedSpriteMarkerView::new(&mut init.for_view);
        let sprites_dirty = Rc::new(Cell::new(false));

        marker_view.mount(
            grid_view.ct_root,
            &mut init.for_view.base_system.composite_tree,
        );

        let action_handler = Rc::new(ActionHandler {
            sprites_qt: RefCell::new(QuadTree::new()),
            sprite_rects_cached: RefCell::new(Vec::new()),
            current_selected_sprite_marker_view: marker_view,
            grid_view,
            drag_state: RefCell::new(DragState::None),
        });
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(action_handler.grid_view.ht_root, &action_handler);

        init.app_state.register_atlas_size_view_feedback({
            let action_handler = Rc::downgrade(&action_handler);

            move |size| {
                let Some(action_handler) = action_handler.upgrade() else {
                    // app teardown-ed
                    return;
                };

                action_handler
                    .grid_view
                    .renderer
                    .borrow_mut()
                    .set_atlas_size(*size);
            }
        });
        init.app_state.register_sprites_view_feedback({
            let sprites_dirty = Rc::downgrade(&sprites_dirty);
            let action_handler = Rc::downgrade(&action_handler);
            let mut last_selected_index = None;

            move |sprites| {
                let Some(sprites_dirty) = sprites_dirty.upgrade() else {
                    // app teardown-ed
                    return;
                };
                let Some(action_handler) = action_handler.upgrade() else {
                    // app teardown-ed
                    return;
                };

                sprites_dirty.set(true);
                action_handler.update_sprite_rects(sprites);

                // TODO: Model的には複数選択できる形にしてるけどViewはどうしようか......
                let selected_index = sprites.iter().position(|x| x.selected);
                if selected_index != last_selected_index {
                    last_selected_index = selected_index;
                    if let Some(x) = selected_index {
                        action_handler.current_selected_sprite_marker_view.focus(
                            sprites[x].left as _,
                            sprites[x].top as _,
                            sprites[x].width as _,
                            sprites[x].height as _,
                        );
                    } else {
                        action_handler.current_selected_sprite_marker_view.hide();
                    }
                }
            }
        });

        Self {
            sprites_dirty,
            action_handler,
        }
    }

    #[inline]
    pub fn mount(&self, base_sys: &mut AppBaseSystem, parents: (CompositeTreeRef, HitTestTreeRef)) {
        self.action_handler.grid_view.mount(base_sys, parents);
    }

    #[inline]
    pub fn rescale(&self, base_sys: &mut AppBaseSystem, ui_scale_factor: f32) {
        self.action_handler
            .current_selected_sprite_marker_view
            .rescale(base_sys, ui_scale_factor);
    }

    #[inline]
    pub fn update(&self, base_sys: &mut AppBaseSystem, current_sec: f32) {
        self.action_handler
            .current_selected_sprite_marker_view
            .update(&mut base_sys.composite_tree, current_sec);
    }

    pub fn sync_with_app_state(
        &self,
        app_state: &AppState<'subsystem>,
        bg_worker_enqueue: &BackgroundWorkerEnqueueAccess<'subsystem>,
        staging_scratch_buffers: &std::sync::Arc<
            RwLock<FlippableStagingScratchBufferGroup<'subsystem>>,
        >,
    ) {
        if self.sprites_dirty.replace(false) {
            self.action_handler
                .grid_view
                .renderer
                .borrow()
                .update_sprites(
                    app_state.sprites(),
                    bg_worker_enqueue,
                    staging_scratch_buffers,
                );
        }
    }

    #[inline]
    pub fn set_offset(&self, x: f32, y: f32) {
        self.action_handler
            .grid_view
            .renderer
            .borrow_mut()
            .set_offset(x, y);
    }

    #[inline]
    pub fn recreate_render_resources(
        &self,
        app_system: &AppBaseSystem<'subsystem>,
        rendered_pass: br::SubpassRef<impl br::RenderPass + ?Sized>,
        main_buffer_size: br::Extent2D,
    ) {
        self.action_handler
            .grid_view
            .renderer
            .borrow_mut()
            .recreate(app_system, rendered_pass, main_buffer_size);
    }

    #[inline]
    pub fn needs_update(&self) -> bool {
        self.action_handler.grid_view.renderer.borrow().is_dirty()
    }

    #[inline]
    pub fn process_dirty_data<'x>(
        &self,
        subsystem: &'subsystem Subsystem,
        staging_scratch_buffer: &StagingScratchBuffer<'subsystem>,
        rec: br::CmdRecord<'x>,
    ) -> br::CmdRecord<'x> {
        self.action_handler
            .grid_view
            .renderer
            .borrow_mut()
            .process_dirty_data(subsystem, staging_scratch_buffer, rec)
    }

    #[inline]
    pub fn handle_custom_render<'x>(
        &self,
        token: &CustomRenderToken,
        rt_size: br::Extent2D,
        rec: br::CmdRecord<'x>,
    ) -> br::CmdRecord<'x> {
        if &self.action_handler.grid_view.custom_render_token == token {
            return self
                .action_handler
                .grid_view
                .renderer
                .borrow()
                .render_commands(rt_size, rec);
        }

        rec
    }
}

enum DragState {
    None,
    Grid {
        base_x_pixels: f32,
        base_y_pixels: f32,
        drag_start_client_x_pixels: f32,
        drag_start_client_y_pixels: f32,
    },
    Sprite {
        index: usize,
        base_x_pixels: f32,
        base_y_pixels: f32,
        base_width_pixels: f32,
        base_height_pixels: f32,
        drag_start_client_x_pixels: f32,
        drag_start_client_y_pixels: f32,
    },
}

struct ActionHandler<'subsystem> {
    sprites_qt: RefCell<QuadTree>,
    sprite_rects_cached: RefCell<Vec<(u32, u32, u32, u32)>>,
    current_selected_sprite_marker_view: CurrentSelectedSpriteMarkerView,
    grid_view: GridView<'subsystem>,
    drag_state: RefCell<DragState>,
}
impl<'subsystem> ActionHandler<'subsystem> {
    fn update_sprite_rects(&self, sprites: &[SpriteInfo]) {
        let mut sprite_rects_locked = self.sprite_rects_cached.borrow_mut();
        let mut sprites_qt_locked = self.sprites_qt.borrow_mut();

        while sprite_rects_locked.len() > sprites.len() {
            // 削除分
            let n = sprite_rects_locked.len() - 1;
            let old = sprite_rects_locked.pop().unwrap();
            let (index, level) = QuadTree::rect_index_and_level(old.0, old.1, old.2, old.3);

            sprites_qt_locked.element_index_for_region[level][index as usize].remove(&n);
        }
        for (n, (old, new)) in sprite_rects_locked
            .iter_mut()
            .zip(sprites.iter())
            .enumerate()
        {
            // 移動分
            if old.0 == new.left
                && old.1 == new.top
                && old.2 == new.right()
                && old.3 == new.bottom()
            {
                // 座標変化なし
                continue;
            }

            let (old_index, old_level) = QuadTree::rect_index_and_level(old.0, old.1, old.2, old.3);
            let (new_index, new_level) =
                QuadTree::rect_index_and_level(new.left, new.top, new.right(), new.bottom());
            *old = (new.left, new.top, new.right(), new.bottom());

            if old_level == new_level && old_index == new_index {
                // 所属ブロックに変化なし
                continue;
            }

            sprites_qt_locked.element_index_for_region[old_level][old_index as usize].remove(&n);
            sprites_qt_locked.bind(new_level, new_index, n);
        }
        let new_base = sprite_rects_locked.len();
        for (n, new) in sprites.iter().enumerate().skip(new_base) {
            // 追加分
            let (index, level) =
                QuadTree::rect_index_and_level(new.left, new.top, new.right(), new.bottom());
            sprites_qt_locked.bind(level, index, n);
            sprite_rects_locked.push((new.left, new.top, new.right(), new.bottom()));
        }
    }
}
impl<'c> HitTestTreeActionHandler for ActionHandler<'c> {
    fn on_pointer_down(
        &self,
        _sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        args: &PointerActionArgs,
    ) -> EventContinueControl {
        let [cx, cy] = self.grid_view.renderer.borrow().offset();
        let pointing_x = args.client_x * context.ui_scale_factor - cx;
        let pointing_y = args.client_y * context.ui_scale_factor - cy;

        let state_locked = context.state.borrow();
        let sprite_drag_target_index =
            state_locked
                .selected_sprites_with_index()
                .rev()
                .find(|(_, x)| {
                    x.left as f32 <= pointing_x
                        && pointing_x <= x.right() as f32
                        && x.top as f32 <= pointing_y
                        && pointing_y <= x.bottom() as f32
                });
        if let Some((sprite_drag_target_index, target_sprite_ref)) = sprite_drag_target_index {
            // 選択中のスプライトの上で操作が開始された
            self.current_selected_sprite_marker_view.hide();
            *self.drag_state.borrow_mut() = DragState::Sprite {
                index: sprite_drag_target_index,
                base_x_pixels: target_sprite_ref.left as f32,
                base_y_pixels: target_sprite_ref.top as f32,
                base_width_pixels: target_sprite_ref.width as f32,
                base_height_pixels: target_sprite_ref.height as f32,
                drag_start_client_x_pixels: args.client_x * context.ui_scale_factor,
                drag_start_client_y_pixels: args.client_y * context.ui_scale_factor,
            };
        } else {
            *self.drag_state.borrow_mut() = DragState::Grid {
                base_x_pixels: cx,
                base_y_pixels: cy,
                drag_start_client_x_pixels: args.client_x * context.ui_scale_factor,
                drag_start_client_y_pixels: args.client_y * context.ui_scale_factor,
            };
        }

        EventContinueControl::CAPTURE_ELEMENT
    }

    fn on_pointer_move(
        &self,
        _sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        args: &PointerActionArgs,
    ) -> EventContinueControl {
        match &*self.drag_state.borrow() {
            DragState::None => (),
            DragState::Grid {
                base_x_pixels,
                base_y_pixels,
                drag_start_client_x_pixels,
                drag_start_client_y_pixels,
            } => {
                let dx = args.client_x * context.ui_scale_factor - drag_start_client_x_pixels;
                let dy = args.client_y * context.ui_scale_factor - drag_start_client_y_pixels;
                let ox = base_x_pixels + dx;
                let oy = base_y_pixels + dy;

                self.grid_view.renderer.borrow_mut().set_offset(ox, oy);
                self.current_selected_sprite_marker_view
                    .set_view_offset(ox, oy);

                return EventContinueControl::STOP_PROPAGATION;
            }
            &DragState::Sprite {
                index,
                base_x_pixels,
                base_y_pixels,
                drag_start_client_x_pixels,
                drag_start_client_y_pixels,
                ..
            } => {
                let (dx, dy) = (
                    (args.client_x * context.ui_scale_factor) - drag_start_client_x_pixels,
                    (args.client_y * context.ui_scale_factor) - drag_start_client_y_pixels,
                );
                let (sx, sy) = (
                    (base_x_pixels + dx).max(0.0) as u32,
                    (base_y_pixels + dy).max(0.0) as u32,
                );
                self.grid_view
                    .renderer
                    .borrow_mut()
                    .update_sprite_offset(index, sx as _, sy as _);

                return EventContinueControl::STOP_PROPAGATION;
            }
        }

        EventContinueControl::empty()
    }

    fn on_pointer_up(
        &self,
        _sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        args: &PointerActionArgs,
    ) -> EventContinueControl {
        match self.drag_state.replace(DragState::None) {
            DragState::None => (),
            DragState::Grid {
                base_x_pixels,
                base_y_pixels,
                drag_start_client_x_pixels,
                drag_start_client_y_pixels,
            } => {
                let dx = args.client_x * context.ui_scale_factor - drag_start_client_x_pixels;
                let dy = args.client_y * context.ui_scale_factor - drag_start_client_y_pixels;
                let ox = base_x_pixels + dx;
                let oy = base_y_pixels + dy;

                self.grid_view.renderer.borrow_mut().set_offset(ox, oy);
                self.current_selected_sprite_marker_view
                    .set_view_offset(ox, oy);

                return EventContinueControl::STOP_PROPAGATION
                    | EventContinueControl::RELEASE_CAPTURE_ELEMENT;
            }
            DragState::Sprite {
                index,
                base_x_pixels,
                base_y_pixels,
                base_width_pixels,
                base_height_pixels,
                drag_start_client_x_pixels,
                drag_start_client_y_pixels,
            } => {
                let (dx, dy) = (
                    (args.client_x * context.ui_scale_factor) - drag_start_client_x_pixels,
                    (args.client_y * context.ui_scale_factor) - drag_start_client_y_pixels,
                );
                let (sx, sy) = (
                    (base_x_pixels + dx).max(0.0) as u32,
                    (base_y_pixels + dy).max(0.0) as u32,
                );
                context.state.borrow_mut().set_sprite_offset(index, sx, sy);

                // 選択インデックスが変わるわけではないのでここで選択枠Viewを復帰させる
                self.current_selected_sprite_marker_view.focus(
                    sx as _,
                    sy as _,
                    base_width_pixels,
                    base_height_pixels,
                );

                return EventContinueControl::STOP_PROPAGATION
                    | EventContinueControl::RELEASE_CAPTURE_ELEMENT;
            }
        }

        EventContinueControl::empty()
    }

    fn on_click(
        &self,
        _sender: HitTestTreeRef,
        context: &mut AppUpdateContext,
        args: &PointerActionArgs,
    ) -> EventContinueControl {
        let [cx, cy] = self.grid_view.renderer.borrow().offset();
        let x = args.client_x * context.ui_scale_factor - cx;
        let y = args.client_y * context.ui_scale_factor - cy;

        let mut max_index = None;
        for n in self
            .sprites_qt
            .borrow()
            .iter_possible_element_indices(x as _, y as _)
        {
            let (l, t, r, b) = self.sprite_rects_cached.borrow()[n];
            if l as f32 <= x && x <= r as f32 && t as f32 <= y && y <= b as f32 {
                // 大きいインデックスのものが最前面にいるのでmaxをとる
                max_index = Some(max_index.map_or(n, |x: usize| x.max(n)));
            }
        }

        if let Some(mx) = max_index {
            context
                .event_queue
                .push(AppEvent::SelectSprite { index: mx });
        } else {
            context.event_queue.push(AppEvent::DeselectSprite);
        }

        EventContinueControl::STOP_PROPAGATION
    }
}

struct GridView<'d> {
    ct_root: CompositeTreeRef,
    ht_root: HitTestTreeRef,
    custom_render_token: CustomRenderToken,
    renderer: RefCell<Renderer<'d>>,
}
impl<'d> GridView<'d> {
    fn new(
        init: &mut ViewInitContext<'_, '_, 'd>,
        rendered_pass: br::SubpassRef<impl br::RenderPass + ?Sized>,
        main_buffer_size: br::Extent2D,
        init_atlas_size: SizePixels,
    ) -> Self {
        let renderer = Renderer::new(
            init.base_system,
            rendered_pass,
            main_buffer_size,
            init_atlas_size,
        );

        let custom_render_token = init
            .base_system
            .composite_tree
            .acquire_custom_render_token();
        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            relative_size_adjustment: [1.0, 1.0],
            custom_render_token: Some(custom_render_token),
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
            custom_render_token,
            renderer: RefCell::new(renderer),
        }
    }

    #[inline(always)]
    fn mount(&self, base_system: &mut AppBaseSystem, parents: (CompositeTreeRef, HitTestTreeRef)) {
        base_system.set_tree_parent((self.ct_root, self.ht_root), parents);
    }
}

#[repr(C)]
struct GridParams {
    pub offset: [f32; 2],
    pub size: [f32; 2],
}

struct Renderer<'d> {
    _sprite_sampler: br::SamplerObject<&'d Subsystem>,
    pub param_buffer: br::BufferObject<&'d Subsystem>,
    _param_buffer_memory: br::DeviceMemoryObject<&'d Subsystem>,
    current_params_data: GridParams,
    param_is_dirty: bool,
    atlas_size: SizePixels,
    bg_vertex_buffer_is_dirty: bool,
    pub bg_vertex_buffer: br::BufferObject<&'d Subsystem>,
    _bg_vertex_buffer_memory: br::DeviceMemoryObject<&'d Subsystem>,
    pub render_pipeline_layout: br::PipelineLayoutObject<&'d Subsystem>,
    pub render_pipeline: br::PipelineObject<&'d Subsystem>,
    pub bg_render_pipeline: br::PipelineObject<&'d Subsystem>,
    _dsl_param: br::DescriptorSetLayoutObject<&'d Subsystem>,
    _dsl_sprite_instance: br::DescriptorSetLayoutObject<&'d Subsystem>,
    _dp: br::DescriptorPoolObject<&'d Subsystem>,
    pub ds_param: br::DescriptorSet,
    ds_sprite_instance: br::DescriptorSet,
    loaded_sprite_source_atlas: RefCell<LoadedSpriteSourceAtlas<'d>>,
    sprite_instance_buffers: RefCell<SpriteInstanceBuffers<'d>>,
    sprite_atlas_rect_by_path: RefCell<HashMap<PathBuf, (u32, u32, u32, u32)>>,
    sprite_instance_render_pipeline_layout: br::PipelineLayoutObject<&'d Subsystem>,
    sprite_instance_render_pipeline: br::PipelineObject<&'d Subsystem>,
    sprite_count: Cell<usize>,
    sprite_image_copies: Arc<RwLock<HashMap<usize, Vec<br::vk::VkBufferImageCopy>>>>,
}
impl<'d> Renderer<'d> {
    const SPRITES_RENDER_PIPELINE_VI_STATE: &'static br::PipelineVertexInputStateCreateInfo<
        'static,
    > = &br::PipelineVertexInputStateCreateInfo::new(
        &[br::VertexInputBindingDescription::per_instance_typed::<
            [f32; 8],
        >(0)],
        &[
            br::VertexInputAttributeDescription {
                location: 0,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: 0,
            },
            br::VertexInputAttributeDescription {
                location: 1,
                binding: 0,
                format: br::vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                offset: (core::mem::size_of::<f32>() * 4) as _,
            },
        ],
    );

    #[tracing::instrument(skip(app_system, rendered_pass))]
    fn new(
        app_system: &AppBaseSystem<'d>,
        rendered_pass: br::SubpassRef<impl br::RenderPass + ?Sized>,
        main_buffer_size: br::Extent2D,
        init_atlas_size: SizePixels,
    ) -> Self {
        let sprite_sampler =
            br::SamplerObject::new(app_system.subsystem, &br::SamplerCreateInfo::new()).unwrap();

        let mut param_buffer = match br::BufferObject::new(
            app_system.subsystem,
            &br::BufferCreateInfo::new_for_type::<GridParams>(
                br::BufferUsage::UNIFORM_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create param buffer");
                std::process::abort();
            }
        };
        let mreq = param_buffer.requirements();
        let Some(memindex) = app_system.find_device_local_memory_index(mreq.memoryTypeBits) else {
            tracing::error!("No suitable memory for param buffer");
            std::process::abort();
        };
        let param_buffer_memory = match br::DeviceMemoryObject::new(
            app_system.subsystem,
            &br::MemoryAllocateInfo::new(mreq.size, memindex),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to allocate param buffer memory");
                std::process::abort();
            }
        };
        if let Err(e) = param_buffer.bind(&param_buffer_memory, 0) {
            tracing::warn!(reason = ?e, "Failed to bind param buffer memory");
        }

        let mut bg_vertex_buffer = match br::BufferObject::new(
            app_system.subsystem,
            &br::BufferCreateInfo::new_for_type::<[[f32; 4]; 4]>(
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create bg vertex buffer");
                std::process::abort();
            }
        };
        let mreq = bg_vertex_buffer.requirements();
        let Some(memindex) = app_system.find_device_local_memory_index(mreq.memoryTypeBits) else {
            tracing::error!("No suitable memory for bg vertex buffer");
            std::process::abort();
        };
        let bg_vertex_buffer_memory = match br::DeviceMemoryObject::new(
            app_system.subsystem,
            &br::MemoryAllocateInfo::new(mreq.size, memindex),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to allocate bg vertex buffer memory");
                std::process::abort();
            }
        };
        if let Err(e) = bg_vertex_buffer.bind(&bg_vertex_buffer_memory, 0) {
            tracing::warn!(reason = ?e, "Failed to bind bg vertex buffer memory");
        }

        let dsl_param = match br::DescriptorSetLayoutObject::new(
            app_system.subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[
                br::DescriptorType::UniformBuffer.make_binding(0, 1)
            ]),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create param descriptor set layout");
                std::process::abort();
            }
        };
        let dsl_sprite_instance = match br::DescriptorSetLayoutObject::new(
            app_system.subsystem,
            &br::DescriptorSetLayoutCreateInfo::new(&[br::DescriptorType::CombinedImageSampler
                .make_binding(0, 1)
                .with_immutable_samplers(&[sprite_sampler.as_transparent_ref()])]),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create sprite instance descriptor set layout");
                std::process::exit(1);
            }
        };

        let vsh = app_system.require_shader("resources/filltri.vert");
        let fsh = app_system.require_shader("resources/grid.frag");
        let bg_vsh = app_system.require_shader("resources/atlas_bg.vert");
        let bg_fsh = app_system.require_shader("resources/atlas_bg.frag");
        let sprite_instance_vsh = app_system.require_shader("resources/sprite_instance.vert");
        let sprite_instance_fsh = app_system.require_shader("resources/sprite_instance.frag");

        let render_pipeline_layout = match br::PipelineLayoutObject::new(
            app_system.subsystem,
            &br::PipelineLayoutCreateInfo::new(
                &[dsl_param.as_transparent_ref()],
                &[br::PushConstantRange::for_type::<[f32; 2]>(
                    br::vk::VK_SHADER_STAGE_FRAGMENT_BIT | br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                )],
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create pipeline layout");
                std::process::abort();
            }
        };
        let sprite_instance_render_pipeline_layout = match br::PipelineLayoutObject::new(
            app_system.subsystem,
            &br::PipelineLayoutCreateInfo::new(
                &[
                    dsl_param.as_transparent_ref(),
                    dsl_sprite_instance.as_transparent_ref(),
                ],
                &[br::PushConstantRange::for_type::<[f32; 2]>(
                    br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                )],
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create sprite instance pipeline layout");
                std::process::exit(1);
            }
        };

        let main_viewports = [main_buffer_size
            .into_rect(br::Offset2D::ZERO)
            .make_viewport(0.0..1.0)];
        let main_scissors = [main_buffer_size.into_rect(br::Offset2D::ZERO)];
        let main_viewport_state =
            br::PipelineViewportStateCreateInfo::new_array(&main_viewports, &main_scissors);

        let [
            render_pipeline,
            bg_render_pipeline,
            sprite_instance_render_pipeline,
        ] = app_system
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    &render_pipeline_layout,
                    rendered_pass,
                    &[
                        vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &main_viewport_state,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &render_pipeline_layout,
                    rendered_pass,
                    &[
                        bg_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        bg_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_FLOAT4_ONLY,
                    IA_STATE_TRISTRIP,
                    &main_viewport_state,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &sprite_instance_render_pipeline_layout,
                    rendered_pass,
                    &[
                        sprite_instance_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        sprite_instance_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    Self::SPRITES_RENDER_PIPELINE_VI_STATE,
                    IA_STATE_TRISTRIP,
                    &main_viewport_state,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_PREMULTIPLIED,
                )
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        let loaded_sprite_source_atlas =
            LoadedSpriteSourceAtlas::new(app_system, br::vk::VK_FORMAT_R8G8B8A8_UNORM);
        let sprite_instance_buffers = SpriteInstanceBuffers::new(app_system.subsystem);

        let mut dp = match br::DescriptorPoolObject::new(
            app_system.subsystem,
            &br::DescriptorPoolCreateInfo::new(
                2,
                &[
                    br::DescriptorType::UniformBuffer.make_size(1),
                    br::DescriptorType::CombinedImageSampler.make_size(1),
                ],
            ),
        ) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create descriptor pool");
                std::process::abort();
            }
        };
        let [ds_param, ds_sprite_instance] = match dp.alloc_array(&[
            dsl_param.as_transparent_ref(),
            dsl_sprite_instance.as_transparent_ref(),
        ]) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to allocate descriptor sets");
                std::process::abort();
            }
        };
        app_system.subsystem.update_descriptor_sets(
            &[
                ds_param
                    .binding_at(0)
                    .write(br::DescriptorContents::uniform_buffer(
                        &param_buffer,
                        0..core::mem::size_of::<GridParams>() as _,
                    )),
                ds_sprite_instance.binding_at(0).write(
                    br::DescriptorContents::combined_image_sampler(
                        &loaded_sprite_source_atlas.resource,
                        br::ImageLayout::ShaderReadOnlyOpt,
                    ),
                ),
            ],
            &[],
        );

        Self {
            _sprite_sampler: sprite_sampler,
            param_buffer,
            _param_buffer_memory: param_buffer_memory,
            bg_vertex_buffer,
            _bg_vertex_buffer_memory: bg_vertex_buffer_memory,
            current_params_data: GridParams {
                offset: [0.0, 0.0],
                size: [64.0, 64.0],
            },
            param_is_dirty: true,
            atlas_size: init_atlas_size,
            bg_vertex_buffer_is_dirty: true,
            _dsl_param: dsl_param,
            _dsl_sprite_instance: dsl_sprite_instance,
            _dp: dp,
            ds_param,
            ds_sprite_instance,
            render_pipeline_layout,
            render_pipeline,
            bg_render_pipeline,
            loaded_sprite_source_atlas: RefCell::new(loaded_sprite_source_atlas),
            sprite_instance_buffers: RefCell::new(sprite_instance_buffers),
            sprite_atlas_rect_by_path: RefCell::new(HashMap::new()),
            sprite_instance_render_pipeline_layout,
            sprite_instance_render_pipeline,
            sprite_count: Cell::new(0),
            sprite_image_copies: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn update_sprite_offset(&self, index: usize, left_pixels: f32, top_pixels: f32) {
        let mut buffers_mref = self.sprite_instance_buffers.borrow_mut();

        let h = buffers_mref.stg_memory.native_ptr();
        let cap = buffers_mref.capacity;
        let p = buffers_mref
            .stg_memory
            .map(0..(cap as usize * core::mem::size_of::<SpriteInstance>()))
            .unwrap();
        self.sprite_image_copies.write().clear();
        unsafe {
            let instance_ptr =
                p.addr_of_mut::<SpriteInstance>(index * core::mem::size_of::<SpriteInstance>());
            core::ptr::addr_of_mut!((*instance_ptr).pos_st[2]).write(left_pixels);
            core::ptr::addr_of_mut!((*instance_ptr).pos_st[3]).write(top_pixels);
        }
        if buffers_mref.stg_requires_flush {
            unsafe {
                buffers_mref
                    .subsystem
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                        h,
                        0,
                        cap * core::mem::size_of::<SpriteInstance>() as u64,
                    )])
                    .unwrap();
            }
        }
        unsafe {
            buffers_mref.stg_memory.unmap();
        }

        buffers_mref.is_dirty = true;
    }

    #[tracing::instrument(skip(self, sprites, bg_worker_access, staging_scratch_buffers))]
    fn update_sprites(
        &self,
        sprites: &[SpriteInfo],
        bg_worker_access: &BackgroundWorkerEnqueueAccess<'d>,
        staging_scratch_buffers: &std::sync::Arc<RwLock<FlippableStagingScratchBufferGroup<'d>>>,
    ) {
        let mut buffers_mref = self.sprite_instance_buffers.borrow_mut();
        let mut rects_mref = self.sprite_atlas_rect_by_path.borrow_mut();
        let mut atlas_mref = self.loaded_sprite_source_atlas.borrow_mut();

        buffers_mref.require_capacity(sprites.len() as _);
        if !sprites.is_empty() {
            let h = buffers_mref.stg_memory.native_ptr();
            let p = buffers_mref
                .stg_memory
                .map(0..sprites.len() * core::mem::size_of::<SpriteInstance>())
                .unwrap();
            self.sprite_image_copies.write().clear();
            for (n, x) in sprites.iter().enumerate() {
                let (ox, oy) = match rects_mref.entry(x.source_path.clone()) {
                    std::collections::hash_map::Entry::Occupied(o) => {
                        let &(ox, oy, _, _) = o.get();

                        (ox, oy)
                    }
                    std::collections::hash_map::Entry::Vacant(v) => {
                        let Some(r) = atlas_mref.alloc(x.width, x.height) else {
                            tracing::error!(path = ?x.source_path, width = x.width, height = x.height, "no space for sprite(TODO: add page or resize atlas...)");
                            continue;
                        };
                        v.insert((r.left, r.top, x.width, x.height));
                        let (ox, oy) = (r.left, r.top);

                        bg_worker_access.enqueue(BackgroundWork::LoadSpriteSource(
                            x.source_path.clone(),
                            Box::new({
                                let sprite_image_copies = Arc::downgrade(&self.sprite_image_copies);
                                let staging_scratch_buffers =
                                    std::sync::Arc::downgrade(staging_scratch_buffers);
                                let &SpriteInfo { width, height, .. } = x;

                                move |path, di| {
                                    let Some(sprite_image_copies) = sprite_image_copies.upgrade()
                                    else {
                                        // component teardown-ed
                                        return;
                                    };
                                    let Some(staging_scratch_buffers) =
                                        staging_scratch_buffers.upgrade()
                                    else {
                                        // app teardown-ed
                                        return;
                                    };

                                    // TODO: hdr
                                    let img_formatted = di.to_rgba8();
                                    let img_bytes = img_formatted.as_bytes();

                                    let mut staging_scratch_buffer =
                                        parking_lot::RwLockWriteGuard::map(
                                            staging_scratch_buffers.write(),
                                            |x| x.active_buffer_mut(),
                                        );
                                    let mut copies_locked = sprite_image_copies.write();
                                    let r = staging_scratch_buffer.reserve(img_bytes.len() as _);
                                    let p = staging_scratch_buffer
                                        .map(&r, StagingScratchBufferMapMode::Write)
                                        .unwrap();
                                    unsafe {
                                        p.addr_of_mut::<u8>(0).copy_from_nonoverlapping(
                                            img_bytes.as_ptr(),
                                            img_bytes.len(),
                                        );
                                    }
                                    drop(p);
                                    let (bx, o) = staging_scratch_buffer.of_index(&r);
                                    copies_locked.entry(bx).or_insert_with(Vec::new).push(
                                        br::vk::VkBufferImageCopy {
                                            bufferOffset: o,
                                            bufferRowLength: img_formatted.width(),
                                            bufferImageHeight: img_formatted.height(),
                                            imageSubresource: br::ImageSubresourceLayers::new(
                                                br::AspectMask::COLOR,
                                                0,
                                                0..1,
                                            ),
                                            imageOffset: br::Offset3D::new(ox as _, oy as _, 0),
                                            imageExtent: br::Extent3D::new(width, height, 1),
                                        },
                                    );

                                    tracing::info!(?path, ox, oy, "LoadSpriteComplete");
                                }
                            }),
                        ));

                        (ox, oy)
                    }
                };

                unsafe {
                    let instance_ptr =
                        p.addr_of_mut::<SpriteInstance>(n * core::mem::size_of::<SpriteInstance>());
                    core::ptr::addr_of_mut!((*instance_ptr).pos_st).write([
                        x.width as f32,
                        x.height as f32,
                        x.left as f32,
                        x.top as f32,
                    ]);
                    core::ptr::addr_of_mut!((*instance_ptr).uv_st).write([
                        x.width as f32 / LoadedSpriteSourceAtlas::SIZE as f32,
                        x.height as f32 / LoadedSpriteSourceAtlas::SIZE as f32,
                        ox as f32 / LoadedSpriteSourceAtlas::SIZE as f32,
                        oy as f32 / LoadedSpriteSourceAtlas::SIZE as f32,
                    ]);
                }
            }
            if buffers_mref.stg_requires_flush {
                unsafe {
                    buffers_mref
                        .subsystem
                        .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(
                            h,
                            0,
                            (sprites.len() * core::mem::size_of::<SpriteInstance>()) as _,
                        )])
                        .unwrap();
                }
            }
            unsafe {
                buffers_mref.stg_memory.unmap();
            }

            buffers_mref.is_dirty = true;
        }

        self.sprite_count.set(sprites.len());
    }

    const fn offset(&self) -> [f32; 2] {
        self.current_params_data.offset
    }

    fn set_offset(&mut self, x: f32, y: f32) {
        self.current_params_data.offset = [x, y];
        self.param_is_dirty = true;
    }

    fn set_atlas_size(&mut self, size: SizePixels) {
        self.atlas_size = size;
        self.bg_vertex_buffer_is_dirty = true;
    }

    fn is_dirty(&self) -> bool {
        self.param_is_dirty
            || self.bg_vertex_buffer_is_dirty
            || self.sprite_instance_buffers.borrow().is_dirty
            || !self.sprite_image_copies.read().is_empty()
    }

    fn process_dirty_data<'c>(
        &mut self,
        subsystem: &Subsystem,
        staging_scratch_buffer: &StagingScratchBuffer<'d>,
        rec: br::CmdRecord<'c>,
    ) -> br::CmdRecord<'c> {
        if !self.is_dirty() {
            return rec;
        }

        self.param_is_dirty = false;
        self.bg_vertex_buffer_is_dirty = false;
        let mut loaded_sprite_atlas_image_barrier_needed = false;
        rec.update_buffer_exact(&self.param_buffer, 0, &self.current_params_data)
            .update_buffer_exact(
                &self.bg_vertex_buffer,
                0,
                &[
                    [0.0f32, 0.0, 0.0, 1.0],
                    [self.atlas_size.width as f32, 0.0, 0.0, 1.0],
                    [0.0f32, self.atlas_size.height as f32, 0.0, 1.0],
                    [
                        self.atlas_size.width as f32,
                        self.atlas_size.height as f32,
                        0.0,
                        1.0,
                    ],
                ],
            )
            .inject(|r| {
                let buffers_mref = self.sprite_instance_buffers.get_mut();
                if !buffers_mref.is_dirty {
                    return r;
                }
                buffers_mref.is_dirty = false;

                r.copy_buffer(
                    &buffers_mref.stg_buffer,
                    &buffers_mref.buffer,
                    &[br::BufferCopy::mirror(
                        0,
                        (self.sprite_count.get() * core::mem::size_of::<SpriteInstance>()) as _,
                    )],
                )
            })
            .inject(|r| {
                let atlas_ref = self.loaded_sprite_source_atlas.borrow();
                let mut copies_mref = self.sprite_image_copies.write();
                if copies_mref.is_empty() {
                    // no copies needed
                    return r;
                }

                loaded_sprite_atlas_image_barrier_needed = true;
                copies_mref.drain().fold(
                    r.inject(|r| {
                        inject_cmd_pipeline_barrier_2(
                            r,
                            subsystem,
                            &br::DependencyInfo::new(
                                &[],
                                &[],
                                &[br::ImageMemoryBarrier2::new(
                                    atlas_ref.resource.image(),
                                    br::ImageSubresourceRange::new(
                                        br::AspectMask::COLOR,
                                        0..1,
                                        0..1,
                                    ),
                                )
                                .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())],
                            ),
                        )
                    }),
                    |r, (bi, cps)| {
                        r.copy_buffer_to_image(
                            staging_scratch_buffer.buffer_of(bi),
                            atlas_ref.resource.image(),
                            br::ImageLayout::TransferDestOpt,
                            &cps,
                        )
                    },
                )
            })
            .inject(|r| {
                let atlas_ref = self.loaded_sprite_source_atlas.borrow();
                let mut image_memory_barriers = Vec::with_capacity(8);
                if loaded_sprite_atlas_image_barrier_needed {
                    image_memory_barriers.push(
                        br::ImageMemoryBarrier2::new(
                            atlas_ref.resource.image(),
                            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
                        )
                        .transit_from(
                            br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt),
                        )
                        .from(
                            br::PipelineStageFlags2::COPY,
                            br::AccessFlags2::TRANSFER.write,
                        )
                        .to(
                            br::PipelineStageFlags2::FRAGMENT_SHADER,
                            br::AccessFlags2::SHADER.read,
                        ),
                    );
                }

                inject_cmd_pipeline_barrier_2(
                    r,
                    subsystem,
                    &br::DependencyInfo::new(
                        &[br::MemoryBarrier2::new()
                            .from(
                                br::PipelineStageFlags2::COPY,
                                br::AccessFlags2::TRANSFER.write,
                            )
                            .to(
                                br::PipelineStageFlags2::FRAGMENT_SHADER
                                    | br::PipelineStageFlags2::VERTEX_ATTRIBUTE_INPUT,
                                br::AccessFlags2::SHADER.read
                                    | br::AccessFlags2::VERTEX_ATTRIBUTE_READ,
                            )],
                        &[],
                        &image_memory_barriers,
                    ),
                )
            })
    }

    fn recreate(
        &mut self,
        app_system: &AppBaseSystem<'d>,
        rendered_pass: br::SubpassRef<impl br::RenderPass + ?Sized>,
        main_buffer_size: br::Extent2D,
    ) {
        let grid_vsh = app_system.require_shader("resources/filltri.vert");
        let grid_fsh = app_system.require_shader("resources/grid.frag");
        let bg_vsh = app_system.require_shader("resources/atlas_bg.vert");
        let bg_fsh = app_system.require_shader("resources/atlas_bg.frag");
        let sprite_instance_vsh = app_system.require_shader("resources/sprite_instance.vert");
        let sprite_instance_fsh = app_system.require_shader("resources/sprite_instance.frag");

        let main_viewport = [main_buffer_size
            .into_rect(br::Offset2D::ZERO)
            .make_viewport(0.0..1.0)];
        let main_scissor_rect = [main_buffer_size.into_rect(br::Offset2D::ZERO)];
        let main_viewport_state =
            br::PipelineViewportStateCreateInfo::new_array(&main_viewport, &main_scissor_rect);

        let [
            render_pipeline,
            bg_render_pipeline,
            sprite_instance_render_pipeline,
        ] = app_system
            .create_graphics_pipelines_array(&[
                br::GraphicsPipelineCreateInfo::new(
                    &self.render_pipeline_layout,
                    rendered_pass,
                    &[
                        grid_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        grid_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &main_viewport_state,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &self.render_pipeline_layout,
                    rendered_pass,
                    &[
                        bg_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        bg_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_FLOAT4_ONLY,
                    IA_STATE_TRISTRIP,
                    &main_viewport_state,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                br::GraphicsPipelineCreateInfo::new(
                    &self.sprite_instance_render_pipeline_layout,
                    rendered_pass,
                    &[
                        sprite_instance_vsh.on_stage(br::ShaderStage::Vertex, c"main"),
                        sprite_instance_fsh.on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    Self::SPRITES_RENDER_PIPELINE_VI_STATE,
                    IA_STATE_TRISTRIP,
                    &main_viewport_state,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_PREMULTIPLIED,
                )
                .set_multisample_state(MS_STATE_EMPTY),
            ])
            .unwrap();

        self.render_pipeline = render_pipeline;
        self.bg_render_pipeline = bg_render_pipeline;
        self.sprite_instance_render_pipeline = sprite_instance_render_pipeline;
    }

    fn render_commands<'cb>(
        &self,
        sc_size: br::Extent2D,
        rec: br::CmdRecord<'cb>,
    ) -> br::CmdRecord<'cb> {
        rec.bind_pipeline(br::PipelineBindPoint::Graphics, &self.render_pipeline)
            .push_constant(
                &self.render_pipeline_layout,
                br::vk::VK_SHADER_STAGE_FRAGMENT_BIT | br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                0,
                &[sc_size.width as f32, sc_size.height as f32],
            )
            .bind_descriptor_sets(
                br::PipelineBindPoint::Graphics,
                &self.render_pipeline_layout,
                0,
                &[self.ds_param],
                &[],
            )
            .draw(3, 1, 0, 0)
            .bind_pipeline(br::PipelineBindPoint::Graphics, &self.bg_render_pipeline)
            .bind_vertex_buffer_array(0, &[self.bg_vertex_buffer.as_transparent_ref()], &[0])
            .draw(4, 1, 0, 0)
            .inject(|r| {
                let inst_count = self.sprite_count.get();

                if inst_count <= 0 {
                    // no sprites drawn
                    return r;
                }

                r.bind_pipeline(
                    br::PipelineBindPoint::Graphics,
                    &self.sprite_instance_render_pipeline,
                )
                .bind_descriptor_sets(
                    br::PipelineBindPoint::Graphics,
                    &self.sprite_instance_render_pipeline_layout,
                    0,
                    &[self.ds_param, self.ds_sprite_instance],
                    &[],
                )
                .push_constant(
                    &self.sprite_instance_render_pipeline_layout,
                    br::vk::VK_SHADER_STAGE_VERTEX_BIT,
                    0,
                    &[sc_size.width as f32, sc_size.height as f32],
                )
                .bind_vertex_buffer_array(
                    0,
                    &[self
                        .sprite_instance_buffers
                        .borrow()
                        .buffer
                        .as_transparent_ref()],
                    &[0],
                )
                .draw(4, inst_count as _, 0, 0)
            })
    }
}

struct LoadedSpriteSourceAtlas<'subsystem> {
    resource: br::ImageViewObject<br::ImageObject<&'subsystem Subsystem>>,
    memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    region_manager: DynamicAtlasManager,
}
impl<'subsystem> LoadedSpriteSourceAtlas<'subsystem> {
    const SIZE: u32 = 4096;

    #[tracing::instrument(skip(base_system), fields(size = Self::SIZE))]
    fn new(base_system: &AppBaseSystem<'subsystem>, format: br::Format) -> Self {
        let mut resource = br::ImageObject::new(
            base_system.subsystem,
            &br::ImageCreateInfo::new(br::Extent2D::spread1(Self::SIZE), format).with_usage(
                br::ImageUsageFlags::SAMPLED
                    | br::ImageUsageFlags::TRANSFER_DEST
                    | br::ImageUsageFlags::TRANSFER_SRC,
            ),
        )
        .unwrap();
        resource
            .set_name(Some(c"Loaded Sprite Source Atlas"))
            .unwrap();
        let req = resource.requirements();
        let memindex = base_system
            .find_device_local_memory_index(req.memoryTypeBits)
            .unwrap();
        let memory = br::DeviceMemoryObject::new(
            base_system.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        resource.bind(&memory, 0).unwrap();
        let resource = br::ImageViewBuilder::new(
            resource,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()
        .unwrap();

        let mut region_manager = DynamicAtlasManager::new();
        region_manager.free(AtlasRect {
            left: 0,
            top: 0,
            right: Self::SIZE,
            bottom: Self::SIZE,
        });

        Self {
            resource,
            memory,
            region_manager,
        }
    }

    fn alloc(&mut self, width: u32, height: u32) -> Option<AtlasRect> {
        if width > Self::SIZE || height > Self::SIZE {
            // でかすぎ
            return None;
        }

        self.region_manager.alloc(width, height)
    }

    fn free(&mut self, rect: AtlasRect) {
        self.region_manager.free(rect);
    }
}

#[repr(C)]
struct SpriteInstance {
    pos_st: [f32; 4],
    uv_st: [f32; 4],
}

struct SpriteInstanceBuffers<'subsystem> {
    subsystem: &'subsystem Subsystem,
    buffer: br::BufferObject<&'subsystem Subsystem>,
    memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    stg_buffer: br::BufferObject<&'subsystem Subsystem>,
    stg_memory: br::DeviceMemoryObject<&'subsystem Subsystem>,
    stg_requires_flush: bool,
    is_dirty: bool,
    capacity: br::DeviceSize,
}
impl<'subsystem> SpriteInstanceBuffers<'subsystem> {
    const BUCKET_SIZE: br::DeviceSize = 64;

    #[tracing::instrument(skip(subsystem))]
    fn new(subsystem: &'subsystem Subsystem) -> Self {
        let capacity = Self::BUCKET_SIZE;
        let byte_length = capacity as usize * core::mem::size_of::<SpriteInstance>();

        let mut buffer = br::BufferObject::new(
            subsystem,
            &br::BufferCreateInfo::new(
                byte_length,
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        )
        .unwrap();
        let req = buffer.requirements();
        let memindex = subsystem
            .find_device_local_memory_index(req.memoryTypeBits)
            .unwrap();
        let memory = br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        buffer.bind(&memory, 0).unwrap();

        let mut stg_buffer = br::BufferObject::new(
            subsystem,
            &br::BufferCreateInfo::new(byte_length, br::BufferUsage::TRANSFER_SRC),
        )
        .unwrap();
        let req = stg_buffer.requirements();
        let memindex = subsystem
            .find_host_visible_memory_index(req.memoryTypeBits)
            .unwrap();
        let stg_memory = br::DeviceMemoryObject::new(
            subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        stg_buffer.bind(&stg_memory, 0).unwrap();
        let stg_requires_flush = !subsystem.is_coherent_memory_type(memindex);

        Self {
            subsystem,
            buffer,
            memory,
            stg_buffer,
            stg_memory,
            stg_requires_flush,
            is_dirty: false,
            capacity,
        }
    }

    /// return: true if resized
    #[tracing::instrument(skip(self), fields(required_capacity))]
    fn require_capacity(&mut self, element_count: br::DeviceSize) -> bool {
        let required_capacity = (element_count + Self::BUCKET_SIZE - 1) & !(Self::BUCKET_SIZE - 1);
        tracing::Span::current().record("required_capacity", required_capacity);

        if self.capacity >= required_capacity {
            // enough
            return false;
        }

        // realloc
        self.capacity = required_capacity;
        let byte_length = self.capacity as usize * core::mem::size_of::<SpriteInstance>();

        self.buffer = br::BufferObject::new(
            self.subsystem,
            &br::BufferCreateInfo::new(
                byte_length,
                br::BufferUsage::VERTEX_BUFFER | br::BufferUsage::TRANSFER_DEST,
            ),
        )
        .unwrap();
        let req = self.buffer.requirements();
        let memindex = self
            .subsystem
            .find_device_local_memory_index(req.memoryTypeBits)
            .unwrap();
        self.memory = br::DeviceMemoryObject::new(
            self.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        self.buffer.bind(&self.memory, 0).unwrap();

        self.stg_buffer = br::BufferObject::new(
            self.subsystem,
            &br::BufferCreateInfo::new(byte_length, br::BufferUsage::TRANSFER_SRC),
        )
        .unwrap();
        let req = self.stg_buffer.requirements();
        let memindex = self
            .subsystem
            .find_host_visible_memory_index(req.memoryTypeBits)
            .unwrap();
        self.stg_memory = br::DeviceMemoryObject::new(
            self.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        self.stg_buffer.bind(&self.stg_memory, 0).unwrap();
        self.stg_requires_flush = !self.subsystem.is_coherent_memory_type(memindex);

        true
    }
}

enum CurrentSelectedSpriteTrigger {
    Focus {
        global_x_pixels: f32,
        global_y_pixels: f32,
        width_pixels: f32,
        height_pixels: f32,
    },
    Hide,
}
struct CurrentSelectedSpriteMarkerView {
    ct_root: CompositeTreeRef,
    global_x_param: CompositeTreeFloatParameterRef,
    global_y_param: CompositeTreeFloatParameterRef,
    view_offset_x_param: CompositeTreeFloatParameterRef,
    view_offset_y_param: CompositeTreeFloatParameterRef,
    focus_trigger: Cell<Option<CurrentSelectedSpriteTrigger>>,
    view_offset_x: Cell<f32>,
    view_offset_y: Cell<f32>,
}
impl CurrentSelectedSpriteMarkerView {
    const CORNER_RADIUS: SafeF32 = unsafe { SafeF32::new_unchecked(4.0) };
    const THICKNESS: SafeF32 = unsafe { SafeF32::new_unchecked(2.0) };
    const COLOR: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

    fn new(init: &mut ViewInitContext) -> Self {
        let border_image_atlas_rect = init
            .base_system
            .rounded_rect_mask(
                unsafe { SafeF32::new_unchecked(init.ui_scale_factor) },
                Self::CORNER_RADIUS,
                Self::THICKNESS,
            )
            .unwrap();

        let global_x_param = init
            .base_system
            .composite_tree
            .parameter_store_mut()
            .alloc_float(FloatParameter::Value(0.0));
        let global_y_param = init
            .base_system
            .composite_tree
            .parameter_store_mut()
            .alloc_float(FloatParameter::Value(0.0));
        let view_offset_x_param = init
            .base_system
            .composite_tree
            .parameter_store_mut()
            .alloc_float(FloatParameter::Value(0.0));
        let view_offset_y_param = init
            .base_system
            .composite_tree
            .parameter_store_mut()
            .alloc_float(FloatParameter::Value(0.0));

        let ct_root = init.base_system.register_composite_rect(CompositeRect {
            offset: [
                AnimatableFloat::Expression(Box::new(move |store| {
                    store.float_value(global_x_param) + store.float_value(view_offset_x_param)
                })),
                AnimatableFloat::Expression(Box::new(move |store| {
                    store.float_value(global_y_param) + store.float_value(view_offset_y_param)
                })),
            ],
            has_bitmap: true,
            slice_borders: [Self::CORNER_RADIUS.value() * init.ui_scale_factor; 4],
            texatlas_rect: border_image_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value(Self::COLOR)),
            opacity: AnimatableFloat::Value(0.0),
            ..Default::default()
        });

        Self {
            ct_root,
            global_x_param,
            global_y_param,
            view_offset_x_param,
            view_offset_y_param,
            focus_trigger: Cell::new(None),
            view_offset_x: Cell::new(0.0),
            view_offset_y: Cell::new(0.0),
        }
    }

    fn mount(&self, ct_parent: CompositeTreeRef, ct: &mut CompositeTree) {
        ct.add_child(ct_parent, self.ct_root);
    }

    fn rescale(&self, base_system: &mut AppBaseSystem, ui_scale_factor: f32) {
        base_system
            .free_mask_atlas_rect(base_system.composite_tree.get(self.ct_root).texatlas_rect);
        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .texatlas_rect = base_system
            .rounded_rect_mask(
                unsafe { SafeF32::new_unchecked(ui_scale_factor) },
                Self::CORNER_RADIUS,
                Self::THICKNESS,
            )
            .unwrap();

        base_system
            .composite_tree
            .get_mut(self.ct_root)
            .slice_borders = [Self::CORNER_RADIUS.value() * ui_scale_factor; 4];
        base_system.composite_tree.mark_dirty(self.ct_root);
    }

    fn update(&self, ct: &mut CompositeTree, current_sec: f32) {
        match self.focus_trigger.replace(None) {
            None => (),
            Some(CurrentSelectedSpriteTrigger::Focus {
                global_x_pixels,
                global_y_pixels,
                width_pixels,
                height_pixels,
            }) => {
                ct.parameter_store_mut()
                    .set_float(self.global_x_param, FloatParameter::Value(global_x_pixels));
                ct.parameter_store_mut()
                    .set_float(self.global_y_param, FloatParameter::Value(global_y_pixels));
                ct.get_mut(self.ct_root).size = [
                    AnimatableFloat::Value(width_pixels),
                    AnimatableFloat::Value(height_pixels),
                ];

                ct.get_mut(self.ct_root).scale_x = AnimatableFloat::Animated {
                    from_value: 1.3,
                    to_value: 1.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.15,
                    curve: AnimationCurve::CubicBezier {
                        p1: (0.0, 0.0),
                        p2: (0.0, 1.0),
                    },
                    event_on_complete: None,
                };
                ct.get_mut(self.ct_root).scale_y = AnimatableFloat::Animated {
                    from_value: 1.3,
                    to_value: 1.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.15,
                    curve: AnimationCurve::CubicBezier {
                        p1: (0.0, 0.0),
                        p2: (0.0, 1.0),
                    },
                    event_on_complete: None,
                };

                ct.get_mut(self.ct_root).opacity = AnimatableFloat::Animated {
                    from_value: 0.0,
                    to_value: 1.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.15,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                };
            }
            Some(CurrentSelectedSpriteTrigger::Hide) => {
                ct.get_mut(self.ct_root).opacity = AnimatableFloat::Animated {
                    from_value: 1.0,
                    to_value: 0.0,
                    start_sec: current_sec,
                    end_sec: current_sec + 0.15,
                    curve: AnimationCurve::Linear,
                    event_on_complete: None,
                };
            }
        }

        ct.parameter_store_mut().set_float(
            self.view_offset_x_param,
            FloatParameter::Value(self.view_offset_x.get()),
        );
        ct.parameter_store_mut().set_float(
            self.view_offset_y_param,
            FloatParameter::Value(self.view_offset_y.get()),
        );

        ct.mark_dirty(self.ct_root);
    }

    fn focus(&self, x_pixels: f32, y_pixels: f32, width_pixels: f32, height_pixels: f32) {
        self.focus_trigger
            .set(Some(CurrentSelectedSpriteTrigger::Focus {
                global_x_pixels: x_pixels,
                global_y_pixels: y_pixels,
                width_pixels,
                height_pixels,
            }));
    }

    fn hide(&self) {
        self.focus_trigger
            .set(Some(CurrentSelectedSpriteTrigger::Hide));
    }

    fn set_view_offset(&self, offset_x_pixels: f32, offset_y_pixels: f32) {
        self.view_offset_x.set(offset_x_pixels);
        self.view_offset_y.set(offset_y_pixels);
    }
}
