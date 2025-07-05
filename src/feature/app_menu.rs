use std::{cell::Cell, path::Path, rc::Rc};

use crate::{
    AppEvent, AppUpdateContext, BLEND_STATE_SINGLE_NONE, FillcolorRConstants, IA_STATE_TRIFAN,
    IA_STATE_TRILIST, MS_STATE_EMPTY, PresenterInitContext, RASTER_STATE_DEFAULT_FILL_NOCULL,
    RoundedRectConstants, VI_STATE_EMPTY, VI_STATE_FLOAT2_ONLY, ViewInitContext,
    base_system::AppBaseSystem,
    composite::{
        AnimatableColor, AnimatableFloat, AnimationData, CompositeMode, CompositeRect,
        CompositeTreeFloatParameterRef, CompositeTreeRef, FloatParameter,
    },
    hittest::{self, HitTestTreeActionHandler, HitTestTreeData, HitTestTreeRef},
    input::EventContinueControl,
    svg,
    text::TextLayout,
    trigger_cell::TriggerCell,
};

use bedrock::{
    self as br, Device, DeviceMemoryMut, ImageChild, MemoryBound, RenderPass, ShaderModule,
    VkHandle,
};

#[derive(Debug, Clone, Copy)]
pub enum Command {
    AddSprite,
    Save,
}

struct CommandButtonView {
    ct_root: CompositeTreeRef,
    ct_icon: CompositeTreeRef,
    ct_label: CompositeTreeRef,
    ct_bg_alpha_rate_shown: CompositeTreeFloatParameterRef,
    ct_bg_alpha_rate_pointer: CompositeTreeFloatParameterRef,
    ht_root: HitTestTreeRef,
    left: f32,
    top: f32,
    show_delay_sec: f32,
    ui_scale_factor: f32,
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
    pub fn new(
        init: &mut ViewInitContext,
        label: &str,
        icon_path: impl AsRef<Path>,
        left: f32,
        top: f32,
        show_delay_sec: f32,
        command: Command,
    ) -> Self {
        let label_layout = TextLayout::build_simple(label, &mut init.base_system.fonts.ui_default);

        let bg_atlas_rect = init.base_system.alloc_mask_atlas_rect(
            ((Self::BUTTON_HEIGHT + 1.0) * init.ui_scale_factor) as u32,
            ((Self::BUTTON_HEIGHT + 1.0) * init.ui_scale_factor) as u32,
        );
        let icon_atlas_rect = init.base_system.alloc_mask_atlas_rect(
            (Self::ICON_SIZE * init.ui_scale_factor) as _,
            (Self::ICON_SIZE * init.ui_scale_factor) as _,
        );
        let label_atlas_rect = init
            .base_system
            .alloc_mask_atlas_rect(label_layout.width_px(), label_layout.height_px());

        let icon_svg_content = match std::fs::read_to_string(icon_path) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to load icon svg");
                std::process::exit(1);
            }
        };
        let mut reader = quick_xml::Reader::from_str(&icon_svg_content);
        let mut svg_path_commands = Vec::new();
        let mut viewbox = None;
        loop {
            match reader.read_event().unwrap() {
                quick_xml::events::Event::Start(x) => {
                    println!("xml start: {x:?}");
                    println!("  name {:?}", x.name());
                    for a in x.attributes().with_checks(false) {
                        let a = a.unwrap();
                        println!("  attr {:?} = {:?}", a.key, a.unescape_value());
                    }

                    if x.name().0 == b"svg" {
                        let viewbox_value = &x
                            .attributes()
                            .with_checks(false)
                            .find(|x| x.as_ref().is_ok_and(|x| x.key.0 == b"viewBox"))
                            .unwrap()
                            .unwrap()
                            .value;
                        viewbox = Some(svg::ViewBox::from_str_bytes(viewbox_value));
                    }
                }
                quick_xml::events::Event::End(x) => {
                    println!("xml end: {x:?}");
                    println!("  name {:?}", x.name());
                }
                quick_xml::events::Event::Empty(x) => {
                    println!("xml empty: {x:?}");
                    println!("  name {:?}", x.name());
                    for a in x.attributes().with_checks(false) {
                        let a = a.unwrap();
                        println!("  attr {:?} = {:?}", a.key, a.unescape_value());
                    }

                    if x.name().0 == b"path" {
                        let path_data = &x
                            .attributes()
                            .with_checks(false)
                            .find(|x| x.as_ref().is_ok_and(|x| x.key.0 == b"d"))
                            .unwrap()
                            .unwrap()
                            .value;
                        for x in svg::InstructionParser::new_bytes(path_data) {
                            println!("  path inst: {x:?}");
                            svg_path_commands.push(x);
                        }
                    }
                }
                quick_xml::events::Event::Text(x) => println!("xml text: {x:?}"),
                quick_xml::events::Event::CData(x) => println!("xml cdata: {x:?}"),
                quick_xml::events::Event::Comment(x) => println!("xml comment: {x:?}"),
                quick_xml::events::Event::Decl(x) => println!("xml decl: {x:?}"),
                quick_xml::events::Event::PI(x) => println!("xml pi: {x:?}"),
                quick_xml::events::Event::DocType(x) => println!("xml doctype: {x:?}"),
                quick_xml::events::Event::Eof => {
                    println!("eof");
                    break;
                }
            }
        }

        let viewbox = viewbox.unwrap();

        // rasterize icon svg
        let mut stencil_buffer = br::ImageObject::new(
            init.base_system.subsystem,
            &br::ImageCreateInfo::new(icon_atlas_rect.extent(), br::vk::VK_FORMAT_S8_UINT)
                .as_depth_stencil_attachment()
                .as_transient_attachment()
                .sample_counts(4),
        )
        .unwrap();
        let stencil_buffer_mem = init
            .base_system
            .alloc_device_local_memory_for_requirements(&stencil_buffer.requirements());
        stencil_buffer.bind(&stencil_buffer_mem, 0).unwrap();
        let stencil_buffer = br::ImageViewBuilder::new(
            stencil_buffer,
            br::ImageSubresourceRange::new(br::AspectMask::STENCIL, 0..1, 0..1),
        )
        .create()
        .unwrap();

        let mut ms_color_buffer = br::ImageObject::new(
            init.base_system.subsystem,
            &br::ImageCreateInfo::new(icon_atlas_rect.extent(), br::vk::VK_FORMAT_R8_UNORM)
                .as_color_attachment()
                .usage_with(br::ImageUsageFlags::TRANSFER_SRC)
                .sample_counts(4),
        )
        .unwrap();
        let ms_color_buffer_mem = init
            .base_system
            .alloc_device_local_memory_for_requirements(&ms_color_buffer.requirements());
        ms_color_buffer.bind(&ms_color_buffer_mem, 0).unwrap();
        let ms_color_buffer = br::ImageViewBuilder::new(
            ms_color_buffer,
            br::ImageSubresourceRange::new(br::AspectMask::COLOR, 0..1, 0..1),
        )
        .create()
        .unwrap();

        let render_pass = br::RenderPassObject::new(
            init.base_system.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[
                    br::AttachmentDescription2::new(br::vk::VK_FORMAT_R8_UNORM)
                        .color_memory_op(br::LoadOp::Clear, br::StoreOp::DontCare)
                        .with_layout_to(br::ImageLayout::TransferSrcOpt.from_undefined())
                        .samples(4),
                    br::AttachmentDescription2::new(br::vk::VK_FORMAT_S8_UINT)
                        .stencil_memory_op(br::LoadOp::Clear, br::StoreOp::DontCare)
                        .with_layout_to(br::ImageLayout::DepthStencilReadOnlyOpt.from_undefined())
                        .samples(4),
                ],
                &[
                    br::SubpassDescription2::new()
                        .depth_stencil(&br::AttachmentReference2::depth_stencil_attachment_opt(1)),
                    br::SubpassDescription2::new()
                        .colors(&[br::AttachmentReference2::color_attachment_opt(0)])
                        .depth_stencil(&br::AttachmentReference2::depth_stencil_readonly_opt(1)),
                ],
                &[
                    br::SubpassDependency2::new(
                        br::SubpassIndex::Internal(0),
                        br::SubpassIndex::Internal(1),
                    )
                    .by_region()
                    .of_memory(
                        br::AccessFlags::DEPTH_STENCIL_ATTACHMENT.write,
                        br::AccessFlags::DEPTH_STENCIL_ATTACHMENT.read,
                    )
                    .of_execution(
                        br::PipelineStageFlags::LATE_FRAGMENT_TESTS,
                        br::PipelineStageFlags::EARLY_FRAGMENT_TESTS,
                    ),
                    br::SubpassDependency2::new(
                        br::SubpassIndex::Internal(1),
                        br::SubpassIndex::External,
                    )
                    .by_region()
                    .of_memory(
                        br::AccessFlags::COLOR_ATTACHMENT.write,
                        br::AccessFlags::TRANSFER.read,
                    )
                    .of_execution(
                        br::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                        br::PipelineStageFlags::TRANSFER,
                    ),
                ],
            ),
        )
        .unwrap();
        let fb = br::FramebufferObject::new(
            init.base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &render_pass,
                &[
                    ms_color_buffer.as_transparent_ref(),
                    stencil_buffer.as_transparent_ref(),
                ],
                icon_atlas_rect.width(),
                icon_atlas_rect.height(),
            ),
        )
        .unwrap();

        let round_rect_rp = br::RenderPassObject::new(
            init.base_system.subsystem,
            &br::RenderPassCreateInfo2::new(
                &[
                    br::AttachmentDescription2::new(init.base_system.mask_atlas_format())
                        .color_memory_op(br::LoadOp::DontCare, br::StoreOp::Store)
                        .with_layout_to(br::ImageLayout::TransferDestOpt.from_undefined()),
                ],
                &[br::SubpassDescription2::new()
                    .colors(&[br::AttachmentReference2::color_attachment_opt(0)])],
                &[],
            ),
        )
        .unwrap();
        let round_rect_fb = br::FramebufferObject::new(
            init.base_system.subsystem,
            &br::FramebufferCreateInfo::new(
                &round_rect_rp,
                &[init
                    .base_system
                    .mask_atlas_resource_transparent_ref()
                    .as_transparent_ref()],
                init.base_system.mask_atlas_size(),
                init.base_system.mask_atlas_size(),
            ),
        )
        .unwrap();

        let local_viewports = [icon_atlas_rect
            .extent()
            .into_rect(br::Offset2D::ZERO)
            .make_viewport(0.0..1.0)];
        let local_scissor_rects = [icon_atlas_rect.extent().into_rect(br::Offset2D::ZERO)];
        let vp_state_local =
            br::PipelineViewportStateCreateInfo::new_array(&local_viewports, &local_scissor_rects);
        let sop_invert_always = br::vk::VkStencilOpState {
            failOp: br::StencilOp::Invert as _,
            passOp: br::StencilOp::Invert as _,
            depthFailOp: br::StencilOp::Invert as _,
            compareOp: br::CompareOp::Always as _,
            compareMask: 0,
            writeMask: 0x01,
            reference: 0x01,
        };
        let sop_testonly_equal_1 = br::vk::VkStencilOpState {
            failOp: br::StencilOp::Keep as _,
            passOp: br::StencilOp::Keep as _,
            depthFailOp: br::StencilOp::Keep as _,
            compareOp: br::CompareOp::Equal as _,
            compareMask: 0x01,
            reference: 0x01,
            writeMask: 0,
        };
        let [
            round_rect_pipeline,
            first_stencil_shape_pipeline,
            curve_stencil_shape_pipeline,
            colorize_pipeline,
        ] = init
            .base_system
            .create_graphics_pipelines_array(&[
                // round rect pipeline
                br::GraphicsPipelineCreateInfo::new(
                    init.base_system.require_empty_pipeline_layout(),
                    round_rect_rp.subpass(0),
                    &[
                        init.base_system
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.base_system
                            .require_shader("resources/rounded_rect.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &RoundedRectConstants {
                                    corner_radius: Self::BUTTON_HEIGHT * 0.5,
                                },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &br::PipelineViewportStateCreateInfo::new_array(
                        &[bg_atlas_rect.vk_rect().make_viewport(0.0..1.0)],
                        &[bg_atlas_rect.vk_rect()],
                    ),
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_multisample_state(MS_STATE_EMPTY),
                // first stencil shape pipeline
                br::GraphicsPipelineCreateInfo::new(
                    init.base_system.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.base_system
                            .require_shader("resources/normalized_01_2d.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.base_system
                            .require_shader("resources/stencil_only.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    VI_STATE_FLOAT2_ONLY,
                    IA_STATE_TRIFAN,
                    &vp_state_local,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_depth_stencil_state(
                    &br::PipelineDepthStencilStateCreateInfo::new()
                        .stencil_test(true)
                        .stencil_state_back(sop_invert_always.clone())
                        .stencil_state_front(sop_invert_always.clone()),
                )
                .set_multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
                ),
                // curve stencil shape
                br::GraphicsPipelineCreateInfo::new(
                    init.base_system.require_empty_pipeline_layout(),
                    render_pass.subpass(0),
                    &[
                        init.base_system
                            .require_shader("resources/normalized_01_2d_with_uv.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.base_system
                            .require_shader("resources/stencil_loop_blinn_curve.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main"),
                    ],
                    &br::PipelineVertexInputStateCreateInfo::new(
                        &[br::VertexInputBindingDescription::per_vertex_typed::<
                            [f32; 4],
                        >(0)],
                        &[
                            br::VertexInputAttributeDescription {
                                location: 0,
                                binding: 0,
                                format: br::vk::VK_FORMAT_R32G32_SFLOAT,
                                offset: 0,
                            },
                            br::VertexInputAttributeDescription {
                                location: 1,
                                binding: 0,
                                format: br::vk::VK_FORMAT_R32G32_SFLOAT,
                                offset: core::mem::size_of::<[f32; 2]>() as _,
                            },
                        ],
                    ),
                    IA_STATE_TRILIST,
                    &vp_state_local,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_depth_stencil_state(
                    &br::PipelineDepthStencilStateCreateInfo::new()
                        .stencil_test(true)
                        .stencil_state_back(sop_invert_always.clone())
                        .stencil_state_front(sop_invert_always),
                )
                .set_multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new().rasterization_samples(4),
                ),
                // colorize pipeline
                br::GraphicsPipelineCreateInfo::new(
                    init.base_system.require_empty_pipeline_layout(),
                    render_pass.subpass(1),
                    &[
                        init.base_system
                            .require_shader("resources/filltri.vert")
                            .on_stage(br::ShaderStage::Vertex, c"main"),
                        init.base_system
                            .require_shader("resources/fillcolor_r.frag")
                            .on_stage(br::ShaderStage::Fragment, c"main")
                            .with_specialization_info(&br::SpecializationInfo::new(
                                &FillcolorRConstants { r: 1.0 },
                            )),
                    ],
                    VI_STATE_EMPTY,
                    IA_STATE_TRILIST,
                    &vp_state_local,
                    RASTER_STATE_DEFAULT_FILL_NOCULL,
                    BLEND_STATE_SINGLE_NONE,
                )
                .set_depth_stencil_state(
                    &br::PipelineDepthStencilStateCreateInfo::new()
                        .stencil_test(true)
                        .stencil_state_back(sop_testonly_equal_1.clone())
                        .stencil_state_front(sop_testonly_equal_1),
                )
                .set_multisample_state(
                    &br::PipelineMultisampleStateCreateInfo::new()
                        .rasterization_samples(4)
                        .enable_alpha_to_coverage(),
                ),
            ])
            .unwrap();

        let mut trifan_points = Vec::<[f32; 2]>::new();
        let mut trifan_point_ranges = Vec::new();
        let mut curve_triangle_points = Vec::<[f32; 4]>::new();
        let mut cubic_last_control_point = None::<(f32, f32)>;
        let mut quadratic_last_control_point = None::<(f32, f32)>;
        let mut current_figure_first_index = None;
        let mut p = (0.0f32, 0.0f32);
        for x in svg_path_commands.iter() {
            match x {
                &svg::Instruction::Move { absolute, x, y } => {
                    if current_figure_first_index.is_some() {
                        panic!("not closed last figure");
                    }

                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p = if absolute { (x, y) } else { (p.0 + x, p.1 + y) };
                    current_figure_first_index = Some(trifan_points.len());

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &svg::Instruction::Line { absolute, x, y } => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p = if absolute { (x, y) } else { (p.0 + x, p.1 + y) };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &svg::Instruction::HLine { absolute, x } => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p.0 = if absolute { x } else { p.0 + x };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &svg::Instruction::VLine { absolute, y } => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p.1 = if absolute { y } else { p.1 + y };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &svg::Instruction::BezierCurve {
                    absolute,
                    c1_x,
                    c1_y,
                    c2_x,
                    c2_y,
                    x,
                    y,
                } => {
                    let figure = lyon_geom::CubicBezierSegment {
                        from: lyon_geom::point(p.0, p.1),
                        ctrl1: if absolute {
                            lyon_geom::point(c1_x, c1_y)
                        } else {
                            lyon_geom::point(p.0 + c1_x, p.1 + c1_y)
                        },
                        ctrl2: if absolute {
                            lyon_geom::point(c2_x, c2_y)
                        } else {
                            lyon_geom::point(p.0 + c2_x, p.1 + c2_y)
                        },
                        to: if absolute {
                            lyon_geom::point(x, y)
                        } else {
                            lyon_geom::point(p.0 + x, p.1 + y)
                        },
                    };

                    figure.for_each_quadratic_bezier(1.0, &mut |q| {
                        curve_triangle_points.extend([
                            [
                                viewbox.translate_x(q.from.x),
                                viewbox.translate_y(q.from.y),
                                0.0,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.ctrl.x),
                                viewbox.translate_y(q.ctrl.y),
                                0.5,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.to.x),
                                viewbox.translate_y(q.to.y),
                                1.0,
                                1.0,
                            ],
                        ]);

                        // TODO: おなじ位置の頂点を出力する場合があるのでもうちょい最適化したいかも
                        trifan_points
                            .push([viewbox.translate_x(q.from.x), viewbox.translate_y(q.from.y)]);
                        trifan_points
                            .push([viewbox.translate_x(q.to.x), viewbox.translate_y(q.to.y)]);
                    });

                    cubic_last_control_point = Some((figure.ctrl2.x, figure.ctrl2.y));
                    quadratic_last_control_point = None;
                    p = (figure.to.x, figure.to.y);
                }
                &svg::Instruction::SequentialBezierCurve {
                    absolute,
                    c2_x,
                    c2_y,
                    x,
                    y,
                } => {
                    let figure = lyon_geom::CubicBezierSegment {
                        from: lyon_geom::point(p.0, p.1),
                        ctrl1: if let Some((lc2_x, lc2_y)) = cubic_last_control_point {
                            let d = (p.0 - lc2_x, p.1 - lc2_y);
                            lyon_geom::point(p.0 + d.0, p.1 + d.1)
                        } else {
                            lyon_geom::point(p.0, p.1)
                        },
                        ctrl2: if absolute {
                            lyon_geom::point(c2_x, c2_y)
                        } else {
                            lyon_geom::point(p.0 + c2_x, p.1 + c2_y)
                        },
                        to: if absolute {
                            lyon_geom::point(x, y)
                        } else {
                            lyon_geom::point(p.0 + x, p.1 + y)
                        },
                    };

                    figure.for_each_quadratic_bezier(1.0, &mut |q| {
                        curve_triangle_points.extend([
                            [
                                viewbox.translate_x(q.from.x),
                                viewbox.translate_y(q.from.y),
                                0.0,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.ctrl.x),
                                viewbox.translate_y(q.ctrl.y),
                                0.5,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.to.x),
                                viewbox.translate_y(q.to.y),
                                1.0,
                                1.0,
                            ],
                        ]);

                        // TODO: おなじ位置の頂点を出力する場合があるのでもうちょい最適化したい
                        trifan_points
                            .push([viewbox.translate_x(q.from.x), viewbox.translate_y(q.from.y)]);
                        trifan_points
                            .push([viewbox.translate_x(q.to.x), viewbox.translate_y(q.to.y)]);
                    });

                    cubic_last_control_point = Some((figure.ctrl2.x, figure.ctrl2.y));
                    quadratic_last_control_point = None;
                    p = (figure.to.x, figure.to.y);
                }
                &svg::Instruction::QuadraticBezierCurve {
                    absolute,
                    cx,
                    cy,
                    x,
                    y,
                } => {
                    curve_triangle_points.extend([
                        [viewbox.translate_x(p.0), viewbox.translate_y(p.1), 0.0, 0.0],
                        if absolute {
                            [viewbox.translate_x(cx), viewbox.translate_y(cy), 0.5, 0.0]
                        } else {
                            [
                                viewbox.translate_x(p.0 + cx),
                                viewbox.translate_y(p.1 + cy),
                                0.5,
                                0.0,
                            ]
                        },
                        if absolute {
                            [viewbox.translate_x(x), viewbox.translate_y(y), 1.0, 1.0]
                        } else {
                            [
                                viewbox.translate_x(p.0 + x),
                                viewbox.translate_y(p.1 + y),
                                1.0,
                                1.0,
                            ]
                        },
                    ]);
                    cubic_last_control_point = None;
                    quadratic_last_control_point = Some(if absolute {
                        (cx, cy)
                    } else {
                        (p.0 + cx, p.1 + cy)
                    });
                    p = if absolute { (x, y) } else { (p.0 + x, p.1 + y) };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &svg::Instruction::SequentialQuadraticBezierCurve { absolute, x, y } => {
                    let (cx, cy) = if let Some((lcx, lcy)) = quadratic_last_control_point {
                        let d = (p.0 - lcx, p.1 - lcy);
                        (p.0 + d.0, p.1 + d.1)
                    } else {
                        p
                    };

                    curve_triangle_points.extend([
                        [viewbox.translate_x(p.0), viewbox.translate_y(p.1), 0.0, 0.0],
                        [viewbox.translate_x(cx), viewbox.translate_y(cy), 0.5, 0.0],
                        if absolute {
                            [viewbox.translate_x(x), viewbox.translate_y(y), 1.0, 1.0]
                        } else {
                            [
                                viewbox.translate_x(p.0 + x),
                                viewbox.translate_y(p.1 + y),
                                1.0,
                                1.0,
                            ]
                        },
                    ]);
                    cubic_last_control_point = None;
                    quadratic_last_control_point = Some((cx, cy));
                    p = if absolute { (x, y) } else { (p.0 + x, p.1 + y) };

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
                &svg::Instruction::Arc {
                    absolute,
                    rx,
                    ry,
                    angle,
                    large_arc_flag,
                    sweep_flag,
                    x,
                    y,
                } => {
                    let figure = lyon_geom::SvgArc {
                        from: lyon_geom::point(p.0, p.1),
                        to: if absolute {
                            lyon_geom::point(x, y)
                        } else {
                            lyon_geom::point(p.0 + x, p.1 + y)
                        },
                        radii: lyon_geom::vector(rx, ry),
                        x_rotation: lyon_geom::Angle::degrees(angle),
                        flags: lyon_geom::ArcFlags {
                            large_arc: large_arc_flag,
                            sweep: sweep_flag,
                        },
                    };

                    figure.for_each_quadratic_bezier(&mut |q| {
                        curve_triangle_points.extend([
                            [
                                viewbox.translate_x(q.from.x),
                                viewbox.translate_y(q.from.y),
                                0.0,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.ctrl.x),
                                viewbox.translate_y(q.ctrl.y),
                                0.5,
                                0.0,
                            ],
                            [
                                viewbox.translate_x(q.to.x),
                                viewbox.translate_y(q.to.y),
                                1.0,
                                1.0,
                            ],
                        ]);

                        // TODO: おなじ位置の頂点を出力する場合があるのでもうちょい最適化したい
                        trifan_points
                            .push([viewbox.translate_x(q.from.x), viewbox.translate_y(q.from.y)]);
                        trifan_points
                            .push([viewbox.translate_x(q.to.x), viewbox.translate_y(q.to.y)]);
                    });

                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    p = (figure.to.x, figure.to.y);
                }
                &svg::Instruction::Close => {
                    cubic_last_control_point = None;
                    quadratic_last_control_point = None;
                    let x = current_figure_first_index.take().unwrap();
                    let p = (trifan_points[x][0], trifan_points[x][1]);
                    trifan_point_ranges.push(x..trifan_points.len());

                    trifan_points.push([viewbox.translate_x(p.0), viewbox.translate_y(p.1)]);
                }
            }
        }
        if let Some(x) = current_figure_first_index {
            // unprocessed final figure
            trifan_point_ranges.push(x..trifan_points.len());
        }

        let curve_triangle_points_offset = trifan_points.len() * core::mem::size_of::<[f32; 2]>();
        let mut vbuf = br::BufferObject::new(
            init.base_system.subsystem,
            &br::BufferCreateInfo::new(
                curve_triangle_points_offset
                    + curve_triangle_points.len() * core::mem::size_of::<[f32; 4]>(),
                br::BufferUsage::VERTEX_BUFFER,
            ),
        )
        .unwrap();
        let req = vbuf.requirements();
        let memindex = init
            .base_system
            .find_direct_memory_index(req.memoryTypeBits)
            .unwrap();
        let mut mem = br::DeviceMemoryObject::new(
            init.base_system.subsystem,
            &br::MemoryAllocateInfo::new(req.size, memindex),
        )
        .unwrap();
        vbuf.bind(&mem, 0).unwrap();
        let h = mem.native_ptr();
        let requires_flush = !init.base_system.is_coherent_memory_type(memindex);
        let ptr = mem.map(0..req.size as _).unwrap();
        unsafe {
            core::ptr::copy_nonoverlapping(
                trifan_points.as_ptr(),
                ptr.addr_of_mut(0),
                trifan_points.len(),
            );
            core::ptr::copy_nonoverlapping(
                curve_triangle_points.as_ptr(),
                ptr.addr_of_mut(curve_triangle_points_offset),
                curve_triangle_points.len(),
            );
        }
        if requires_flush {
            unsafe {
                init.base_system
                    .subsystem
                    .flush_mapped_memory_ranges(&[br::MappedMemoryRange::new_raw(h, 0, req.size)])
                    .unwrap();
            }
        }
        unsafe {
            mem.unmap();
        }

        let label_image_pixels =
            label_layout.build_stg_image_pixel_buffer(init.staging_scratch_buffer);

        init.base_system
            .sync_execute_graphics_commands(|rec| {
                rec.begin_render_pass2(
                    &br::RenderPassBeginInfo::new(
                        &round_rect_rp,
                        &round_rect_fb,
                        bg_atlas_rect.vk_rect(),
                        &[],
                    ),
                    &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                )
                .bind_pipeline(br::PipelineBindPoint::Graphics, &round_rect_pipeline)
                .draw(3, 1, 0, 0)
                .end_render_pass2(&br::SubpassEndInfo::new())
                .begin_render_pass2(
                    &br::RenderPassBeginInfo::new(
                        &render_pass,
                        &fb,
                        icon_atlas_rect.extent().into_rect(br::Offset2D::ZERO),
                        &[
                            br::ClearValue::color_f32([0.0; 4]),
                            br::ClearValue::depth_stencil(1.0, 0),
                        ],
                    ),
                    &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                )
                .bind_pipeline(
                    br::PipelineBindPoint::Graphics,
                    &first_stencil_shape_pipeline,
                )
                .bind_vertex_buffer_array(0, &[vbuf.as_transparent_ref()], &[0])
                .inject(|r| {
                    trifan_point_ranges
                        .into_iter()
                        .fold(r, |r, vr| r.draw(vr.len() as _, 1, vr.start as _, 0))
                })
                .inject(|r| {
                    if curve_triangle_points.is_empty() {
                        // no curves
                        return r;
                    }

                    r.bind_pipeline(
                        br::PipelineBindPoint::Graphics,
                        &curve_stencil_shape_pipeline,
                    )
                    .bind_vertex_buffer_array(
                        0,
                        &[vbuf.as_transparent_ref()],
                        &[curve_triangle_points_offset as _],
                    )
                    .draw(curve_triangle_points.len() as _, 1, 0, 0)
                })
                .next_subpass2(
                    &br::SubpassBeginInfo::new(br::SubpassContents::Inline),
                    &br::SubpassEndInfo::new(),
                )
                .bind_pipeline(br::PipelineBindPoint::Graphics, &colorize_pipeline)
                .draw(3, 1, 0, 0)
                .end_render_pass2(&br::SubpassEndInfo::new())
                .pipeline_barrier_2(&br::DependencyInfo::new(
                    &[],
                    &[],
                    &[init
                        .base_system
                        .barrier_for_mask_atlas_resource()
                        .transit_to(br::ImageLayout::TransferDestOpt.from_undefined())
                        .of_execution(
                            br::PipelineStageFlags2(0),
                            br::PipelineStageFlags2::RESOLVE,
                        )],
                ))
                .resolve_image(
                    ms_color_buffer.image(),
                    br::ImageLayout::TransferSrcOpt,
                    &init.base_system.mask_atlas_image_transparent_ref(),
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
                        dstOffset: icon_atlas_rect.lt_offset().with_z(0),
                        extent: icon_atlas_rect.extent().with_depth(1),
                    }],
                )
                .inject(|r| {
                    let (b, o) = init.staging_scratch_buffer.of(&label_image_pixels);

                    r.copy_buffer_to_image(
                        b,
                        &init.base_system.mask_atlas_image_transparent_ref(),
                        br::ImageLayout::TransferDestOpt,
                        &[br::vk::VkBufferImageCopy {
                            bufferOffset: o,
                            bufferRowLength: label_layout.width_px(),
                            bufferImageHeight: label_layout.height_px(),
                            imageSubresource: br::ImageSubresourceLayers::new(
                                br::AspectMask::COLOR,
                                0,
                                0..1,
                            ),
                            imageOffset: label_atlas_rect.lt_offset().with_z(0),
                            imageExtent: label_atlas_rect.extent().with_depth(1),
                        }],
                    )
                })
                .pipeline_barrier_2(&br::DependencyInfo::new(
                    &[],
                    &[],
                    &[init
                        .base_system
                        .barrier_for_mask_atlas_resource()
                        .transit_from(
                            br::ImageLayout::TransferDestOpt.to(br::ImageLayout::ShaderReadOnlyOpt),
                        )
                        .of_memory(
                            br::AccessFlags2::TRANSFER.write,
                            br::AccessFlags2::SHADER.read,
                        )
                        .of_execution(
                            br::PipelineStageFlags2::RESOLVE,
                            br::PipelineStageFlags2::FRAGMENT_SHADER,
                        )],
                ))
            })
            .unwrap();
        drop((
            round_rect_pipeline,
            first_stencil_shape_pipeline,
            curve_stencil_shape_pipeline,
            colorize_pipeline,
            mem,
            vbuf,
            round_rect_fb,
            round_rect_rp,
            fb,
            render_pass,
            ms_color_buffer_mem,
            ms_color_buffer,
            stencil_buffer_mem,
            stencil_buffer,
        ));

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
            offset: [
                AnimatableFloat::Value(left * init.ui_scale_factor),
                AnimatableFloat::Value(top * init.ui_scale_factor),
            ],
            size: [
                AnimatableFloat::Value(
                    (Self::ICON_SIZE + Self::ICON_LABEL_GAP + Self::HPADDING * 2.0)
                        * init.ui_scale_factor
                        + label_layout.width(),
                ),
                AnimatableFloat::Value(Self::BUTTON_HEIGHT * init.ui_scale_factor),
            ],
            instance_slot_index: Some(0),
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
            size: [
                AnimatableFloat::Value(Self::ICON_SIZE * init.ui_scale_factor),
                AnimatableFloat::Value(Self::ICON_SIZE * init.ui_scale_factor),
            ],
            offset: [
                AnimatableFloat::Value(Self::HPADDING * init.ui_scale_factor),
                AnimatableFloat::Value(-Self::ICON_SIZE * 0.5 * init.ui_scale_factor),
            ],
            relative_offset_adjustment: [0.0, 0.5],
            instance_slot_index: Some(0),
            texatlas_rect: icon_atlas_rect,
            composite_mode: CompositeMode::ColorTint(AnimatableColor::Value(
                Self::CONTENT_COLOR_HIDDEN,
            )),
            ..Default::default()
        });
        let ct_label = init.base_system.register_composite_rect(CompositeRect {
            size: [
                AnimatableFloat::Value(label_layout.width()),
                AnimatableFloat::Value(label_layout.height()),
            ],
            offset: [
                AnimatableFloat::Value(
                    (Self::HPADDING + Self::ICON_SIZE + Self::ICON_LABEL_GAP)
                        * init.ui_scale_factor,
                ),
                AnimatableFloat::Value(-label_layout.height() * 0.5),
            ],
            relative_offset_adjustment: [0.0, 0.5],
            instance_slot_index: Some(0),
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
            width: (Self::ICON_SIZE + Self::ICON_LABEL_GAP + Self::HPADDING * 2.0)
                + label_layout.width() / init.ui_scale_factor,
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
            left,
            top,
            show_delay_sec,
            ui_scale_factor: init.ui_scale_factor,
            shown: TriggerCell::new(false),
            hovering: Cell::new(false),
            pressing: Cell::new(false),
            is_dirty: Cell::new(false),
            command,
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
                app_system.composite_tree.parameter_store_mut().set_float(
                    self.ct_bg_alpha_rate_shown,
                    FloatParameter::Animated(
                        0.0,
                        AnimationData {
                            to_value: 1.0,
                            start_sec: current_sec + self.show_delay_sec,
                            end_sec: current_sec + self.show_delay_sec + 0.25,
                            curve_p1: (0.5, 0.5),
                            curve_p2: (0.5, 0.5),
                            event_on_complete: None,
                        },
                    ),
                );
                app_system
                    .composite_tree
                    .get_mut(self.ct_icon)
                    .composite_mode = CompositeMode::ColorTint(AnimatableColor::Animated(
                    Self::CONTENT_COLOR_HIDDEN,
                    AnimationData {
                        start_sec: current_sec + self.show_delay_sec,
                        end_sec: current_sec + self.show_delay_sec + 0.25,
                        to_value: Self::CONTENT_COLOR_SHOWN,
                        curve_p1: (0.5, 0.5),
                        curve_p2: (0.5, 0.5),
                        event_on_complete: None,
                    },
                ));
                app_system
                    .composite_tree
                    .get_mut(self.ct_label)
                    .composite_mode = CompositeMode::ColorTint(AnimatableColor::Animated(
                    Self::CONTENT_COLOR_HIDDEN,
                    AnimationData {
                        start_sec: current_sec + self.show_delay_sec,
                        end_sec: current_sec + self.show_delay_sec + 0.25,
                        to_value: Self::CONTENT_COLOR_SHOWN,
                        curve_p1: (0.5, 0.5),
                        curve_p2: (0.5, 0.5),
                        event_on_complete: None,
                    },
                ));
                // TODO: ここでui_scale_factor適用するとui_scale_factorがかわったときにアニメーションが破綻するので別のところにおいたほうがよさそう(CompositeTreeで位置計算するときに適用する)
                app_system.composite_tree.get_mut(self.ct_root).offset[0] =
                    AnimatableFloat::Animated(
                        (self.left + 8.0) * self.ui_scale_factor,
                        AnimationData {
                            start_sec: current_sec + self.show_delay_sec,
                            end_sec: current_sec + self.show_delay_sec + 0.25,
                            to_value: self.left * self.ui_scale_factor,
                            curve_p1: (0.5, 0.5),
                            curve_p2: (0.5, 1.0),
                            event_on_complete: None,
                        },
                    );

                app_system.composite_tree.mark_dirty(self.ct_root);
                app_system.composite_tree.mark_dirty(self.ct_icon);
                app_system.composite_tree.mark_dirty(self.ct_label);
            } else {
                app_system.composite_tree.parameter_store_mut().set_float(
                    self.ct_bg_alpha_rate_shown,
                    FloatParameter::Animated(
                        1.0,
                        AnimationData {
                            to_value: 0.0,
                            start_sec: current_sec,
                            end_sec: current_sec + 0.25,
                            curve_p1: (0.5, 0.5),
                            curve_p2: (0.5, 0.5),
                            event_on_complete: None,
                        },
                    ),
                );
                app_system
                    .composite_tree
                    .get_mut(self.ct_icon)
                    .composite_mode = CompositeMode::ColorTint(AnimatableColor::Animated(
                    Self::CONTENT_COLOR_SHOWN,
                    AnimationData {
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        to_value: Self::CONTENT_COLOR_HIDDEN,
                        curve_p1: (0.5, 0.5),
                        curve_p2: (0.5, 0.5),
                        event_on_complete: None,
                    },
                ));
                app_system
                    .composite_tree
                    .get_mut(self.ct_label)
                    .composite_mode = CompositeMode::ColorTint(AnimatableColor::Animated(
                    Self::CONTENT_COLOR_SHOWN,
                    AnimationData {
                        start_sec: current_sec,
                        end_sec: current_sec + 0.25,
                        to_value: Self::CONTENT_COLOR_HIDDEN,
                        curve_p1: (0.5, 0.5),
                        curve_p2: (0.5, 0.5),
                        event_on_complete: None,
                    },
                ));

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
                FloatParameter::Animated(
                    current,
                    AnimationData {
                        to_value: target,
                        start_sec: current_sec,
                        end_sec: current_sec + 0.1,
                        curve_p1: (0.5, 0.5),
                        curve_p2: (0.5, 0.5),
                        event_on_complete: None,
                    },
                ),
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
            instance_slot_index: Some(0),
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
                    AnimatableColor::Animated(
                        [0.0, 0.0, 0.0, 0.0],
                        AnimationData {
                            start_sec: current_sec,
                            end_sec: current_sec + 0.25,
                            to_value: [0.0, 0.0, 0.0, 0.25],
                            curve_p1: (0.5, 0.5),
                            curve_p2: (0.5, 0.5),
                            event_on_complete: None,
                        },
                    ),
                    AnimatableFloat::Animated(
                        0.0,
                        AnimationData {
                            to_value: 3.0,
                            start_sec: current_sec,
                            end_sec: current_sec + 0.25,
                            curve_p1: (0.5, 0.5),
                            curve_p2: (0.5, 0.5),
                            event_on_complete: None,
                        },
                    ),
                );
                app_system.composite_tree.mark_dirty(self.ct_root);
            } else {
                app_system
                    .composite_tree
                    .get_mut(self.ct_root)
                    .composite_mode = CompositeMode::FillColorBackdropBlur(
                    AnimatableColor::Animated(
                        [0.0, 0.0, 0.0, 0.25],
                        AnimationData {
                            start_sec: current_sec,
                            end_sec: current_sec + 0.25,
                            to_value: [0.0, 0.0, 0.0, 0.0],
                            curve_p1: (0.5, 0.5),
                            curve_p2: (0.5, 0.5),
                            event_on_complete: None,
                        },
                    ),
                    AnimatableFloat::Animated(
                        3.0,
                        AnimationData {
                            to_value: 0.0,
                            start_sec: current_sec,
                            end_sec: current_sec + 0.25,
                            curve_p1: (0.5, 0.5),
                            curve_p2: (0.5, 0.5),
                            event_on_complete: None,
                        },
                    ),
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
    item_views: Vec<Rc<CommandButtonView>>,
    shown: Cell<bool>,
}
impl HitTestTreeActionHandler for ActionHandler {
    fn hit_active(&self, _sender: HitTestTreeRef, _context: &AppUpdateContext) -> bool {
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
                // TODO: click action
                match v.command {
                    Command::AddSprite => {
                        println!("Add Sprite");
                        context.event_queue.push(AppEvent::AppMenuRequestAddSprite);
                    }
                    Command::Save => {
                        println!("Save");
                        context.event_queue.push(AppEvent::UIMessageDialogRequest {
                            content: "Save not implemented".into(),
                        });
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
}

pub struct Presenter {
    base_view: Rc<BaseView>,
    action_handler: Rc<ActionHandler>,
}
impl Presenter {
    pub fn new(init: &mut PresenterInitContext, header_height: f32) -> Self {
        let base_view = Rc::new(BaseView::new(&mut init.for_view));
        let add_button = Rc::new(CommandButtonView::new(
            &mut init.for_view,
            "Add Sprite",
            "resources/icons/add.svg",
            64.0,
            header_height + 32.0,
            0.0,
            Command::AddSprite,
        ));
        let save_button = Rc::new(CommandButtonView::new(
            &mut init.for_view,
            "Save",
            "resources/icons/save.svg",
            64.0,
            header_height + 32.0 + CommandButtonView::BUTTON_HEIGHT + 16.0,
            0.05,
            Command::Save,
        ));

        add_button.mount(
            init.for_view.base_system,
            base_view.ct_root,
            base_view.ht_root,
        );
        save_button.mount(
            init.for_view.base_system,
            base_view.ct_root,
            base_view.ht_root,
        );

        let action_handler = Rc::new(ActionHandler {
            base_view: base_view.clone(),
            item_views: vec![add_button.clone(), save_button.clone()],
            shown: Cell::new(false),
        });

        init.app_state.register_visible_menu_view_feedback({
            let base_view = Rc::downgrade(&base_view);
            let action_handler = Rc::downgrade(&action_handler);
            let add_button = Rc::downgrade(&add_button);
            let save_button = Rc::downgrade(&save_button);

            move |visible| {
                let Some(base_view) = base_view.upgrade() else {
                    // app teardown-ed
                    return;
                };
                let Some(action_handler) = action_handler.upgrade() else {
                    // app teardown-ed
                    return;
                };
                let Some(add_button) = add_button.upgrade() else {
                    // app teardown-ed
                    return;
                };
                let Some(save_button) = save_button.upgrade() else {
                    // app teardown-ed
                    return;
                };

                if visible {
                    base_view.show();
                    add_button.show();
                    save_button.show();
                } else {
                    base_view.hide();
                    add_button.hide();
                    save_button.hide();
                }

                action_handler.shown.set(visible);
            }
        });
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(base_view.ht_root, &action_handler);
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(add_button.ht_root, &action_handler);
        init.for_view
            .base_system
            .hit_tree
            .set_action_handler(save_button.ht_root, &action_handler);

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

    pub fn update(&self, app_system: &mut AppBaseSystem, current_sec: f32) {
        self.base_view.update(app_system, current_sec);
        for v in self.action_handler.item_views.iter() {
            v.update(app_system, current_sec);
        }
    }
}
