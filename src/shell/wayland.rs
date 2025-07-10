use std::{
    cell::{Cell, UnsafeCell},
    os::fd::{AsRawFd, RawFd},
    pin::Pin,
};

use bedrock::{self as br, SurfaceCreateInfo};

use crate::{
    AppEvent, AppEventBus,
    base_system::AppBaseSystem,
    hittest::CursorShape,
    input::PointerInputManager,
    platform::linux::{
        MemoryMapFlags, MemoryProtectionFlags, OpenFlags, TemporalSharedMemory,
        input_event_codes::BTN_LEFT,
    },
    thirdparty::wl::{self, WpCursorShapeDeviceV1Shape, WpCursorShapeManagerV1},
};

enum PointerOnSurface {
    None,
    Main { serial: u32 },
}
struct WaylandShellEventHandler<'a> {
    app_event_bus: &'a AppEventBus,
    cached_client_size: (u32, u32),
    ui_scale_factor: f32,
    pointer_on_surface: PointerOnSurface,
    main_surface_proxy_ptr: *mut wl::Surface,
    client_decoration: ClientDecorationResources,
}
impl wl::XdgWmBaseEventListener for WaylandShellEventHandler<'_> {
    fn ping(&mut self, wm_base: &mut wl::XdgWmBase, serial: u32) {
        if let Err(e) = wm_base.pong(serial) {
            tracing::warn!(reason = ?e, serial, "Failed to respond wm_base.ping");
        }
    }
}
impl wl::XdgSurfaceEventListener for WaylandShellEventHandler<'_> {
    fn configure(&mut self, _: &mut wl::XdgSurface, serial: u32) {
        self.app_event_bus
            .push(AppEvent::ToplevelWindowSurfaceConfigure { serial });
    }
}
impl wl::XdgToplevelEventListener for WaylandShellEventHandler<'_> {
    fn configure(
        &mut self,
        _: &mut wl::XdgToplevel,
        mut width: i32,
        mut height: i32,
        states: &[i32],
    ) {
        if width == 0 {
            width = self.cached_client_size.0 as i32 + 16;
        }
        if height == 0 {
            height = self.cached_client_size.1 as i32 + 16;
        }

        // subtract margins for decorations
        width -= 16;
        height -= 16;

        self.cached_client_size = (width as _, height as _);
        self.app_event_bus.push(AppEvent::ToplevelWindowConfigure {
            width: width as _,
            height: height as _,
        });
        self.client_decoration
            .adjust_for_main_surface_size(width, height);

        tracing::trace!(width, height, ?states, "configure");
    }

    fn close(&mut self, _: &mut wl::XdgToplevel) {
        self.app_event_bus.push(AppEvent::ToplevelWindowClose);
    }

    fn configure_bounds(&mut self, _toplevel: &mut wl::XdgToplevel, width: i32, height: i32) {
        tracing::trace!(width, height, "configure bounds");
    }

    fn wm_capabilities(&mut self, _toplevel: &mut wl::XdgToplevel, capabilities: &[i32]) {
        tracing::trace!(?capabilities, "wm capabilities");
    }
}
impl wl::SurfaceEventListener for WaylandShellEventHandler<'_> {
    fn enter(&mut self, _surface: &mut wl::Surface, _output: &mut wl::Output) {
        tracing::trace!("enter output");
    }

    fn leave(&mut self, _surface: &mut wl::Surface, _output: &mut wl::Output) {
        tracing::trace!("leave output");
    }

    fn preferred_buffer_scale(&mut self, surface: &mut wl::Surface, factor: i32) {
        tracing::trace!(factor, "preferred buffer scale");
        self.ui_scale_factor = factor as _;
        // 同じ値を適用することでdpi-awareになるらしい
        surface.set_buffer_scale(factor).unwrap();
        surface.commit().unwrap();
        self.client_decoration.set_buffer_scale(factor);
    }

    fn preferred_buffer_transform(&mut self, _surface: &mut wl::Surface, transform: u32) {
        tracing::trace!(transform, "preferred buffer transform");
    }
}
impl wl::PointerEventListener for WaylandShellEventHandler<'_> {
    fn enter(
        &mut self,
        _pointer: &mut wl::Pointer,
        serial: u32,
        surface: &mut wl::Surface,
        surface_x: wl::Fixed,
        surface_y: wl::Fixed,
    ) {
        self.pointer_on_surface = if core::ptr::addr_eq(surface, self.main_surface_proxy_ptr) {
            PointerOnSurface::Main { serial }
        } else {
            PointerOnSurface::None
        };

        match self.pointer_on_surface {
            PointerOnSurface::None => (),
            PointerOnSurface::Main { serial } => {
                self.app_event_bus.push(AppEvent::MainWindowPointerMove {
                    enter_serial: serial,
                    surface_x: surface_x.to_f32(),
                    surface_y: surface_y.to_f32(),
                })
            }
        }
    }

    fn leave(&mut self, _pointer: &mut wl::Pointer, _serial: u32, surface: &mut wl::Surface) {
        match self.pointer_on_surface {
            PointerOnSurface::None => (),
            PointerOnSurface::Main { .. } => {
                if core::ptr::addr_eq(surface, self.main_surface_proxy_ptr) {
                    self.pointer_on_surface = PointerOnSurface::None;
                }
            }
        };
    }

    fn motion(
        &mut self,
        _pointer: &mut wl::Pointer,
        _time: u32,
        surface_x: wl::Fixed,
        surface_y: wl::Fixed,
    ) {
        match self.pointer_on_surface {
            PointerOnSurface::None => (),
            PointerOnSurface::Main { serial } => {
                self.app_event_bus.push(AppEvent::MainWindowPointerMove {
                    enter_serial: serial,
                    surface_x: surface_x.to_f32(),
                    surface_y: surface_y.to_f32(),
                })
            }
        }
    }

    #[tracing::instrument(skip(self, _pointer), fields(state = state as u32))]
    fn button(
        &mut self,
        _pointer: &mut wl::Pointer,
        serial: u32,
        time: u32,
        button: u32,
        state: wl::PointerButtonState,
    ) {
        match self.pointer_on_surface {
            PointerOnSurface::None => (),
            PointerOnSurface::Main { serial } => {
                if button == BTN_LEFT && state == wl::PointerButtonState::Pressed {
                    self.app_event_bus
                        .push(AppEvent::MainWindowPointerLeftDown {
                            enter_serial: serial,
                        });
                } else if button == BTN_LEFT && state == wl::PointerButtonState::Released {
                    self.app_event_bus.push(AppEvent::MainWindowPointerLeftUp {
                        enter_serial: serial,
                    });
                }
            }
        }
    }

    fn axis(&mut self, _pointer: &mut wl::Pointer, time: u32, axis: u32, value: wl::Fixed) {
        tracing::trace!(time, axis, value = value.to_f32(), "axis");
    }

    fn frame(&mut self, _pointer: &mut wl::Pointer) {
        // do nothing
    }

    fn axis_source(&mut self, _pointer: &mut wl::Pointer, axis_source: u32) {
        tracing::trace!(axis_source, "axis source");
    }

    fn axis_stop(&mut self, _pointer: &mut wl::Pointer, _time: u32, axis: u32) {
        tracing::trace!(axis, "axis stop");
    }

    fn axis_discrete(&mut self, _pointer: &mut wl::Pointer, axis: u32, discrete: i32) {
        tracing::trace!(axis, discrete, "axis discrete");
    }

    fn axis_value120(&mut self, _pointer: &mut wl::Pointer, axis: u32, value120: i32) {
        tracing::trace!(axis, value120, "axis value120");
    }

    fn axis_relative_direction(&mut self, _pointer: &mut wl::Pointer, axis: u32, direction: u32) {
        tracing::trace!(axis, direction, "axis relative direction");
    }
}
impl wl::CallbackEventListener for WaylandShellEventHandler<'_> {
    fn done(&mut self, _callback: &mut wl::Callback, _data: u32) {
        self.app_event_bus.push(AppEvent::ToplevelWindowFrameTiming);
    }
}
impl wl::WpFractionalScaleV1EventListener for WaylandShellEventHandler<'_> {
    fn preferred_scale(&mut self, _object: &mut wl::WpFractionalScaleV1, scale: u32) {
        tracing::trace!(
            scale,
            scale_f = scale as f32 / 120.0,
            "preferred fractional scale"
        )
    }
}
impl wl::GtkShell1EventListener for WaylandShellEventHandler<'_> {
    fn capabilities(&mut self, sender: &mut wl::GtkShell1, capabilities: u32) {
        tracing::trace!(capabilities, "gtk_shell capabilities");
    }
}
impl wl::GtkSurface1EventListener for WaylandShellEventHandler<'_> {
    fn configure(&mut self, sender: &mut wl::GtkSurface1, states: &[u32]) {
        tracing::trace!(?states, "gtk_surface configure");
    }

    fn configure_edges(&mut self, sender: &mut wl::GtkSurface1, constraints: &[u32]) {
        tracing::trace!(?constraints, "gtk_surface configure edges");
    }
}

struct ClientDecorationResources {
    shm: core::ptr::NonNull<wl::Shm>,
    shadow_corner_buf: core::ptr::NonNull<wl::Buffer>,
    shadow_straight_buf: core::ptr::NonNull<wl::Buffer>,
    shadow_lt_surface: core::ptr::NonNull<wl::Surface>,
    shadow_lb_surface: core::ptr::NonNull<wl::Surface>,
    shadow_lb_subsurface: core::ptr::NonNull<wl::Subsurface>,
    shadow_rt_surface: core::ptr::NonNull<wl::Surface>,
    shadow_rt_subsurface: core::ptr::NonNull<wl::Subsurface>,
    shadow_rb_surface: core::ptr::NonNull<wl::Surface>,
    shadow_rb_subsurface: core::ptr::NonNull<wl::Subsurface>,
    shadow_left_surface: core::ptr::NonNull<wl::Surface>,
    shadow_left_viewport: core::ptr::NonNull<wl::WpViewport>,
    shadow_right_surface: core::ptr::NonNull<wl::Surface>,
    shadow_right_subsurface: core::ptr::NonNull<wl::Subsurface>,
    shadow_right_viewport: core::ptr::NonNull<wl::WpViewport>,
    shadow_top_surface: core::ptr::NonNull<wl::Surface>,
    shadow_top_viewport: core::ptr::NonNull<wl::WpViewport>,
    shadow_bottom_surface: core::ptr::NonNull<wl::Surface>,
    shadow_bottom_subsurface: core::ptr::NonNull<wl::Subsurface>,
    shadow_bottom_viewport: core::ptr::NonNull<wl::WpViewport>,
}
impl ClientDecorationResources {
    pub fn new(
        compositor: &wl::Compositor,
        subcompositor: &wl::Subcompositor,
        shm: wl::Owned<wl::Shm>,
        viewporter: &wl::WpViewporter,
        main_surface: &wl::Surface,
    ) -> Self {
        let shadow_lt_surface = compositor.create_surface().unwrap();
        let shadow_lt_subsurface = subcompositor
            .get_subsurface(&shadow_lt_surface, main_surface)
            .unwrap();
        let shadow_lb_surface = compositor.create_surface().unwrap();
        let shadow_lb_subsurface = subcompositor
            .get_subsurface(&shadow_lb_surface, main_surface)
            .unwrap();
        let shadow_rt_surface = compositor.create_surface().unwrap();
        let shadow_rt_subsurface = subcompositor
            .get_subsurface(&shadow_rt_surface, main_surface)
            .unwrap();
        let shadow_rb_surface = compositor.create_surface().unwrap();
        let shadow_rb_subsurface = subcompositor
            .get_subsurface(&shadow_rb_surface, main_surface)
            .unwrap();

        shadow_rt_surface
            .set_buffer_transform(wl::OutputTransform::Rot90)
            .unwrap();
        shadow_rb_surface
            .set_buffer_transform(wl::OutputTransform::Rot180)
            .unwrap();
        shadow_lb_surface
            .set_buffer_transform(wl::OutputTransform::Rot270)
            .unwrap();

        shadow_lt_subsurface.set_position(-8, -8).unwrap();
        shadow_lt_subsurface.place_below(main_surface).unwrap();
        shadow_lb_subsurface.set_position(-8, 240 - 8).unwrap();
        shadow_lb_subsurface.place_below(main_surface).unwrap();
        shadow_rt_subsurface.set_position(320 - 8, -8).unwrap();
        shadow_rt_subsurface.place_below(main_surface).unwrap();
        shadow_rb_subsurface.set_position(320 - 8, 240 - 8).unwrap();
        shadow_rb_subsurface.place_below(main_surface).unwrap();

        let shadow_left_surface = compositor.create_surface().unwrap();
        let shadow_left_subsurface = subcompositor
            .get_subsurface(&shadow_left_surface, &main_surface)
            .unwrap();
        let shadow_left_viewport = viewporter.get_viewport(&shadow_left_surface).unwrap();
        let shadow_right_surface = compositor.create_surface().unwrap();
        let shadow_right_subsurface = subcompositor
            .get_subsurface(&shadow_right_surface, &main_surface)
            .unwrap();
        let shadow_right_viewport = viewporter.get_viewport(&shadow_right_surface).unwrap();
        let shadow_top_surface = compositor.create_surface().unwrap();
        let shadow_top_subsurface = subcompositor
            .get_subsurface(&shadow_top_surface, &main_surface)
            .unwrap();
        let shadow_top_viewport = viewporter.get_viewport(&shadow_top_surface).unwrap();
        let shadow_bottom_surface = compositor.create_surface().unwrap();
        let shadow_bottom_subsurface = subcompositor
            .get_subsurface(&shadow_bottom_surface, &main_surface)
            .unwrap();
        let shadow_bottom_viewport = viewporter.get_viewport(&shadow_bottom_surface).unwrap();

        shadow_left_viewport
            .set_source(0.0, 0.0, 16.0, 1.0)
            .unwrap();
        shadow_left_viewport.set_destination(16, 240 - 16).unwrap();
        shadow_right_viewport
            .set_source(0.0, 0.0, 16.0, 1.0)
            .unwrap();
        shadow_right_viewport.set_destination(16, 240 - 16).unwrap();
        shadow_top_viewport.set_source(0.0, 0.0, 1.0, 16.0).unwrap();
        shadow_top_viewport.set_destination(320 - 16, 16).unwrap();
        shadow_bottom_viewport
            .set_source(0.0, 0.0, 1.0, 16.0)
            .unwrap();
        shadow_bottom_viewport
            .set_destination(320 - 16, 16)
            .unwrap();

        shadow_right_surface
            .set_buffer_transform(wl::OutputTransform::Flipped)
            .unwrap();
        shadow_top_surface
            .set_buffer_transform(wl::OutputTransform::Rot90)
            .unwrap();
        shadow_bottom_surface
            .set_buffer_transform(wl::OutputTransform::Rot270)
            .unwrap();

        shadow_left_subsurface.set_position(-8, 8).unwrap();
        shadow_left_subsurface.place_below(main_surface).unwrap();
        shadow_right_subsurface.set_position(320 - 8, 8).unwrap();
        shadow_right_subsurface.place_below(main_surface).unwrap();
        shadow_top_subsurface.set_position(8, -8).unwrap();
        shadow_top_subsurface.place_below(main_surface).unwrap();
        shadow_bottom_subsurface.set_position(8, 240 - 8).unwrap();
        shadow_bottom_subsurface.place_below(main_surface).unwrap();

        let vertical_shadow_start_bytes: usize = 16 * 16 * 4;
        let shm_size = vertical_shadow_start_bytes + 16 * 1 * 4;
        let mut deco_shm_file_path_cstr = c"/peridot-sprite-atlas-visualizer-shell_deco_buf_XXXXXX"
            .to_owned()
            .into_bytes_with_nul();
        let deco_shm = 'try_shm_random_name: {
            // random name gen: https://wayland-book.com/surfaces/shared-memory.html
            for _ in 0..100 {
                let mut ts = core::mem::MaybeUninit::uninit();
                if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, ts.as_mut_ptr()) } < 0 {
                    continue;
                }
                let ts = unsafe { ts.assume_init_ref() };
                let mut r = ts.tv_nsec;
                for p in 0..6 {
                    let fplen = deco_shm_file_path_cstr.len();
                    deco_shm_file_path_cstr[fplen - p - 2] =
                        b'A' + (r & 15) as u8 + (r & 16) as u8 * 2;
                    r >>= 5;
                }

                match TemporalSharedMemory::create(
                    unsafe {
                        std::ffi::CString::from_vec_with_nul_unchecked(deco_shm_file_path_cstr)
                    },
                    OpenFlags::READ_WRITE | OpenFlags::EXCLUSIVE,
                    0o600,
                ) {
                    Ok(x) => {
                        break 'try_shm_random_name x;
                    }
                    Err((e, name)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        // name conflict: retry
                        deco_shm_file_path_cstr = name.into_bytes_with_nul();
                        continue;
                    }
                    Err((e, _)) => {
                        tracing::error!(reason = ?e, "Failed to create shm");
                        std::process::abort();
                    }
                }
            }

            tracing::error!("shm already exists(try limit reached)");
            std::process::abort();
        };
        if let Err(e) = deco_shm.truncate(shm_size as _) {
            tracing::error!(reason = ?e, "Failed to set deco shm file size");
            std::process::abort();
        }
        let deco_shm_mapped = deco_shm
            .mmap_random(
                0..shm_size,
                MemoryProtectionFlags::READ | MemoryProtectionFlags::WRITE,
                MemoryMapFlags::SHARED,
            )
            .unwrap();
        unsafe {
            for y in 0..16 {
                for x in 0..16 {
                    let (x1, y1) = ((16 - x) as f32, (16 - y) as f32);
                    let d = (x1 * x1 + y1 * y1).sqrt();
                    deco_shm_mapped.ptr_of::<[u8; 4]>().add(x + y * 16).write([
                        0,
                        0,
                        0,
                        (255.0 * (1.0 - d / 16.0).powf(1.0)) as _,
                    ]);
                }
            }
        }

        let deco_shm_pool = shm.create_pool(&deco_shm, shm_size as _).unwrap();
        let shadow_corner_buf = deco_shm_pool
            .create_buffer(0, 16, 16, 16 * 4, wl::ShmFormat::ARGB8888)
            .unwrap();
        let shadow_straight_buf = deco_shm_pool
            .create_buffer(
                vertical_shadow_start_bytes as _,
                16,
                1,
                16 * 4,
                wl::ShmFormat::ARGB8888,
            )
            .unwrap();

        shadow_lt_subsurface.leak();
        shadow_left_subsurface.leak();
        shadow_top_subsurface.leak();

        Self {
            shm: shm.unwrap(),
            shadow_corner_buf: shadow_corner_buf.unwrap(),
            shadow_straight_buf: shadow_straight_buf.unwrap(),
            shadow_lt_surface: shadow_lt_surface.unwrap(),
            shadow_lb_surface: shadow_lb_surface.unwrap(),
            shadow_lb_subsurface: shadow_lb_subsurface.unwrap(),
            shadow_rt_surface: shadow_rt_surface.unwrap(),
            shadow_rt_subsurface: shadow_rt_subsurface.unwrap(),
            shadow_rb_surface: shadow_rb_surface.unwrap(),
            shadow_rb_subsurface: shadow_rb_subsurface.unwrap(),
            shadow_left_surface: shadow_left_surface.unwrap(),
            shadow_left_viewport: shadow_left_viewport.unwrap(),
            shadow_right_surface: shadow_right_surface.unwrap(),
            shadow_right_subsurface: shadow_right_subsurface.unwrap(),
            shadow_right_viewport: shadow_right_viewport.unwrap(),
            shadow_top_surface: shadow_top_surface.unwrap(),
            shadow_top_viewport: shadow_top_viewport.unwrap(),
            shadow_bottom_surface: shadow_bottom_surface.unwrap(),
            shadow_bottom_subsurface: shadow_bottom_subsurface.unwrap(),
            shadow_bottom_viewport: shadow_bottom_viewport.unwrap(),
        }
    }

    pub fn set_buffer_scale(&mut self, scale: i32) {
        // recreate pix buffer
        let vertical_shadow_start_bytes: usize = 16 * scale as usize * 16 * scale as usize * 4;
        let shm_size = vertical_shadow_start_bytes + 16 * scale as usize * 1 * scale as usize * 4;
        let mut deco_shm_file_path_cstr = c"/peridot-sprite-atlas-visualizer-shell_deco_buf_XXXXXX"
            .to_owned()
            .into_bytes_with_nul();
        let deco_shm = 'try_shm_random_name: {
            // random name gen: https://wayland-book.com/surfaces/shared-memory.html
            for _ in 0..100 {
                let mut ts = core::mem::MaybeUninit::uninit();
                if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, ts.as_mut_ptr()) } < 0 {
                    continue;
                }
                let ts = unsafe { ts.assume_init_ref() };
                let mut r = ts.tv_nsec;
                for p in 0..6 {
                    let fplen = deco_shm_file_path_cstr.len();
                    deco_shm_file_path_cstr[fplen - p - 2] =
                        b'A' + (r & 15) as u8 + (r & 16) as u8 * 2;
                    r >>= 5;
                }

                match TemporalSharedMemory::create(
                    unsafe {
                        std::ffi::CString::from_vec_with_nul_unchecked(deco_shm_file_path_cstr)
                    },
                    OpenFlags::READ_WRITE | OpenFlags::EXCLUSIVE,
                    0o600,
                ) {
                    Ok(x) => {
                        break 'try_shm_random_name x;
                    }
                    Err((e, name)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        // name conflict: retry
                        deco_shm_file_path_cstr = name.into_bytes_with_nul();
                        continue;
                    }
                    Err((e, _)) => {
                        tracing::error!(reason = ?e, "Failed to create shm");
                        std::process::abort();
                    }
                }
            }

            tracing::error!("shm already exists(try limit reached)");
            std::process::abort();
        };
        if let Err(e) = deco_shm.truncate(shm_size as _) {
            tracing::error!(reason = ?e, "Failed to set deco shm file size");
            std::process::abort();
        }
        let deco_shm_mapped = deco_shm
            .mmap_random(
                0..shm_size,
                MemoryProtectionFlags::READ | MemoryProtectionFlags::WRITE,
                MemoryMapFlags::SHARED,
            )
            .unwrap();
        unsafe {
            for y in 0..16 * scale as usize {
                for x in 0..16 * scale as usize {
                    let (x1, y1) = (
                        (16 * scale as usize - x) as f32,
                        (16 * scale as usize - y) as f32,
                    );
                    let d = (x1 * x1 + y1 * y1).sqrt();
                    deco_shm_mapped
                        .ptr_of::<[u8; 4]>()
                        .add(x + y * 16 * scale as usize)
                        .write([
                            0,
                            0,
                            0,
                            (255.0 * (1.0 - d / (16.0 * scale as f32)).powf(1.0)) as _,
                        ]);
                }
            }
            for y in 0..scale as usize {
                for x in 0..16 * scale as usize {
                    deco_shm_mapped
                        .ptr()
                        .byte_add(vertical_shadow_start_bytes)
                        .cast::<[u8; 4]>()
                        .add(x + y * 16 * scale as usize)
                        .write([
                            0,
                            0,
                            0,
                            (255.0 * (x as f32 / (16.0 * scale as f32)).powf(1.0)) as _,
                        ]);
                }
            }
        }

        let deco_shm_pool = unsafe { self.shm.as_ref() }
            .create_pool(&deco_shm, shm_size as _)
            .unwrap();
        self.shadow_corner_buf = deco_shm_pool
            .create_buffer(
                0,
                16 * scale,
                16 * scale,
                16 * scale * 4,
                wl::ShmFormat::ARGB8888,
            )
            .unwrap()
            .unwrap();
        self.shadow_straight_buf = deco_shm_pool
            .create_buffer(
                vertical_shadow_start_bytes as _,
                16 * scale,
                1 * scale,
                16 * scale * 4,
                wl::ShmFormat::ARGB8888,
            )
            .unwrap()
            .unwrap();
        self.show();

        unsafe { self.shadow_lt_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_lt_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_lb_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_lb_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_left_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_left_surface.as_ref() }
            .commit()
            .unwrap();
        unsafe { self.shadow_right_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_right_surface.as_ref() }
            .commit()
            .unwrap();
        unsafe { self.shadow_top_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_top_surface.as_ref() }
            .commit()
            .unwrap();
        unsafe { self.shadow_bottom_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_bottom_surface.as_ref() }
            .commit()
            .unwrap();
    }

    pub fn show(&self) {
        unsafe { self.shadow_lt_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_corner_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_lt_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_lb_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_corner_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_lb_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_corner_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_corner_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_left_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_straight_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_left_surface.as_ref() }
            .commit()
            .unwrap();
        unsafe { self.shadow_right_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_straight_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_right_surface.as_ref() }
            .commit()
            .unwrap();
        unsafe { self.shadow_top_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_straight_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_top_surface.as_ref() }
            .commit()
            .unwrap();
        unsafe { self.shadow_bottom_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_straight_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_bottom_surface.as_ref() }
            .commit()
            .unwrap();
    }

    pub fn adjust_for_main_surface_size(&self, width: i32, height: i32) {
        // corner positioning
        unsafe { self.shadow_lb_subsurface.as_ref() }
            .set_position(-8, height - 8)
            .unwrap();
        unsafe { self.shadow_rt_subsurface.as_ref() }
            .set_position(width - 8, -8)
            .unwrap();
        unsafe { self.shadow_rb_subsurface.as_ref() }
            .set_position(width - 8, height - 8)
            .unwrap();

        // ltrb stretch
        unsafe { self.shadow_left_viewport.as_ref() }
            .set_destination(16, height - 16)
            .unwrap();
        unsafe { self.shadow_right_viewport.as_ref() }
            .set_destination(16, height - 16)
            .unwrap();
        unsafe { self.shadow_top_viewport.as_ref() }
            .set_destination(width - 16, 16)
            .unwrap();
        unsafe { self.shadow_bottom_viewport.as_ref() }
            .set_destination(width - 16, 16)
            .unwrap();

        // rb positioning
        unsafe { self.shadow_right_subsurface.as_ref() }
            .set_position(width - 8, 8)
            .unwrap();
        unsafe { self.shadow_bottom_subsurface.as_ref() }
            .set_position(8, height - 8)
            .unwrap();

        // commit
        unsafe { self.shadow_lb_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_left_surface.as_ref() }
            .commit()
            .unwrap();
        unsafe { self.shadow_right_surface.as_ref() }
            .commit()
            .unwrap();
        unsafe { self.shadow_top_surface.as_ref() }
            .commit()
            .unwrap();
        unsafe { self.shadow_bottom_surface.as_ref() }
            .commit()
            .unwrap();
    }
}

pub struct AppShell<'a, 'subsystem> {
    shell_event_handler: Box<UnsafeCell<WaylandShellEventHandler<'a>>>,
    display: wl::Display,
    content_surface: core::ptr::NonNull<wl::Surface>,
    xdg_surface: core::ptr::NonNull<wl::XdgSurface>,
    zxdg_exporter_v2: Option<core::ptr::NonNull<wl::ZxdgExporterV2>>,
    cursor_shape_device: core::ptr::NonNull<wl::WpCursorShapeDeviceV1>,
    frame_callback: Cell<core::ptr::NonNull<wl::Callback>>,
    _gtk_surface: Option<core::ptr::NonNull<wl::GtkSurface1>>,
    pub pointer_input_manager: Pin<Box<UnsafeCell<PointerInputManager>>>,
    _marker: core::marker::PhantomData<*mut AppBaseSystem<'subsystem>>,
}
impl<'a, 'subsystem> AppShell<'a, 'subsystem> {
    #[tracing::instrument(skip(events, _base_sys))]
    pub fn new(events: &'a AppEventBus, _base_sys: *mut AppBaseSystem<'subsystem>) -> Self {
        let dp = wl::Display::connect().unwrap();
        let mut registry = dp.get_registry().unwrap();
        struct RegistryListener {
            compositor: Option<wl::Owned<wl::Compositor>>,
            subcompositor: Option<wl::Owned<wl::Subcompositor>>,
            xdg_wm_base: Option<wl::Owned<wl::XdgWmBase>>,
            seat: Option<wl::Owned<wl::Seat>>,
            cursor_shape_manager: Option<wl::Owned<WpCursorShapeManagerV1>>,
            zxdg_exporter_v2: Option<wl::Owned<wl::ZxdgExporterV2>>,
            fractional_scale_manager_v1: Option<wl::Owned<wl::WpFractionalScaleManagerV1>>,
            gtk_shell1: Option<wl::Owned<wl::GtkShell1>>,
            shm: Option<wl::Owned<wl::Shm>>,
            viewporter: Option<wl::Owned<wl::WpViewporter>>,
        }
        impl wl::RegistryListener for RegistryListener {
            #[tracing::instrument(name = "RegistryListener::global", skip(self, registry))]
            fn global(
                &mut self,
                registry: &mut wl::Registry,
                name: u32,
                interface: &core::ffi::CStr,
                version: u32,
            ) {
                tracing::debug!("wl global");

                #[inline]
                fn try_bind<T: wl::Interface>(
                    registry: &mut wl::Registry,
                    name: u32,
                    version: u32,
                ) -> Option<wl::Owned<T>> {
                    match registry.bind(name, version) {
                        Ok(x) => Some(x),
                        Err(e) => {
                            tracing::warn!(reason = ?e, "Failed to bind");
                            None
                        }
                    }
                }

                if interface == c"wl_compositor" {
                    self.compositor = try_bind(registry, name, version);
                } else if interface == c"wl_subcompositor" {
                    self.subcompositor = try_bind(registry, name, version);
                } else if interface == c"xdg_wm_base" {
                    self.xdg_wm_base = try_bind(registry, name, version);
                } else if interface == c"wl_seat" {
                    self.seat = try_bind(registry, name, version);
                } else if interface == c"wp_cursor_shape_manager_v1" {
                    self.cursor_shape_manager = try_bind(registry, name, version);
                } else if interface == c"zxdg_exporter_v2" {
                    self.zxdg_exporter_v2 = try_bind(registry, name, version);
                } else if interface == c"wp_fractional_scale_manager_v1" {
                    self.fractional_scale_manager_v1 = try_bind(registry, name, version);
                } else if interface == c"gtk_shell1" {
                    self.gtk_shell1 = try_bind(registry, name, version);
                } else if interface == c"wl_shm" {
                    self.shm = try_bind(registry, name, version);
                } else if interface == c"wp_viewporter" {
                    self.viewporter = try_bind(registry, name, version);
                }
            }

            fn global_remove(&mut self, _registry: &mut wl::Registry, name: u32) {
                tracing::warn!(name, "unimplemented: wl global remove");
            }
        }
        let mut rl = RegistryListener {
            compositor: None,
            subcompositor: None,
            xdg_wm_base: None,
            seat: None,
            cursor_shape_manager: None,
            zxdg_exporter_v2: None,
            fractional_scale_manager_v1: None,
            gtk_shell1: None,
            shm: None,
            viewporter: None,
        };
        if let Err(e) = registry.add_listener(&mut rl) {
            tracing::warn!(target = "registry", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = dp.roundtrip() {
            tracing::warn!(reason = ?e, "Failed to roundtrip");
        }
        drop(registry);

        let (
            compositor,
            subcompositor,
            mut xdg_wm_base,
            mut seat,
            cursor_shape_manager,
            zxdg_exporter_v2,
            fractional_scale_manager_v1,
            mut gtk_shell1,
            shm,
            viewporter,
        );
        match rl {
            RegistryListener {
                compositor: Some(compositor1),
                subcompositor: Some(subcompositor1),
                xdg_wm_base: Some(xdg_wm_base1),
                seat: Some(seat1),
                cursor_shape_manager: Some(cursor_shape_manager1),
                zxdg_exporter_v2: zxdg_exporter_v21,
                fractional_scale_manager_v1: fractional_scale_manager_v11,
                gtk_shell1: gtk_shell11,
                shm: Some(shm1),
                viewporter: Some(viewporter1),
            } => {
                compositor = compositor1;
                subcompositor = subcompositor1;
                xdg_wm_base = xdg_wm_base1;
                seat = seat1;
                cursor_shape_manager = cursor_shape_manager1;
                zxdg_exporter_v2 = zxdg_exporter_v21;
                fractional_scale_manager_v1 = fractional_scale_manager_v11;
                gtk_shell1 = gtk_shell11;
                shm = shm1;
                viewporter = viewporter1;
            }
            rl => {
                if rl.compositor.is_none() {
                    tracing::error!(
                        interface = "wl_compositor",
                        "Missing required wayland interface"
                    );
                }
                if rl.subcompositor.is_none() {
                    tracing::error!(
                        interface = "wl_subcompositor",
                        "Missing required wayland interface"
                    );
                }
                if rl.xdg_wm_base.is_none() {
                    tracing::error!(
                        interface = "xdg_wm_base",
                        "Missing required wayland interface"
                    );
                }
                if rl.seat.is_none() {
                    tracing::error!(interface = "wl_seat", "Missing required wayland interface");
                }
                if rl.cursor_shape_manager.is_none() {
                    tracing::error!(
                        interface = "wp_cursor_shape_manager_v1",
                        "Missing required wayland interface"
                    );
                }
                if rl.shm.is_none() {
                    tracing::error!(interface = "wl_shm", "Missing required wayland interface");
                }
                if rl.viewporter.is_none() {
                    tracing::error!(
                        interface = "wp_viewporter",
                        "Missing required wayland interface"
                    );
                }

                std::process::abort();
            }
        }

        struct SeatListener {
            pointer: Option<wl::Owned<wl::Pointer>>,
        }
        impl wl::SeatEventListener for SeatListener {
            fn capabilities(&mut self, seat: &mut wl::Seat, capabilities: u32) {
                tracing::debug!(capabilities = format!("0x{capabilities:04x}"), "seat event");

                if (capabilities & 0x01) != 0 {
                    // pointer
                    self.pointer = match seat.get_pointer() {
                        Ok(x) => Some(x),
                        Err(e) => {
                            tracing::warn!(reason = ?e, "Failed to get pointer");
                            None
                        }
                    };
                }
            }

            fn name(&mut self, _seat: &mut wl::Seat, name: &core::ffi::CStr) {
                tracing::debug!(?name, "seat event");
            }
        }
        let mut seat_listener = SeatListener { pointer: None };
        if let Err(e) = seat.add_listener(&mut seat_listener) {
            tracing::warn!(target = "seat", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = dp.roundtrip() {
            tracing::warn!(reason = ?e, "Failed to roundtrip");
        }

        let mut pointer = match seat_listener {
            SeatListener { pointer: Some(p) } => p,
            _ => {
                tracing::error!("No pointer from seat");
                std::process::abort();
            }
        };
        let cursor_shape_device = match cursor_shape_manager.get_pointer(&mut pointer) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to get cursor shape device");
                std::process::abort();
            }
        };

        let mut main_surface = match compositor.create_surface() {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create main surface");
                std::process::exit(1);
            }
        };
        let mut xdg_surface = match xdg_wm_base.get_xdg_surface(&main_surface) {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to get xdg surface");
                std::process::abort();
            }
        };
        let mut xdg_toplevel = match xdg_surface.get_toplevel() {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to get xdg toplevel");
                std::process::abort();
            }
        };
        if let Err(e) = xdg_toplevel.set_app_id(c"io.ct2.peridot.tools.sprite_atlas") {
            tracing::warn!(reason = ?e, "Failed to set app id");
        }
        if let Err(e) = xdg_toplevel.set_title(c"Peridot SpriteAtlas Visualizer/Editor") {
            tracing::warn!(reason = ?e, "Failed to set app title");
        }

        // client decoration: backdrop shadow
        // TODO: detect whether need this(if xdg_decoration is not provided, then needs client decoration)
        let client_decoration_resources = ClientDecorationResources::new(
            &compositor,
            &subcompositor,
            shm,
            &viewporter,
            &main_surface,
        );
        client_decoration_resources.show();

        let mut frame = match main_surface.frame() {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to request next frame");
                std::process::abort();
            }
        };

        let mut gtk_surface = if let Some(ref x) = gtk_shell1 {
            match x.get_gtk_surface(&main_surface) {
                Ok(x) => Some(x),
                Err(e) => {
                    tracing::warn!(reason = ?e, "Failed to create gtk_surface1");
                    None
                }
            }
        } else {
            None
        };

        if let Some(ref x) = gtk_surface {
            x.present(0).unwrap();
        }

        let mut shell_event_handler = Box::new(UnsafeCell::new(WaylandShellEventHandler {
            app_event_bus: events,
            // 現時点ではわからないので適当な値を設定
            cached_client_size: (640, 480),
            ui_scale_factor: 2.0,
            pointer_on_surface: PointerOnSurface::None,
            main_surface_proxy_ptr: main_surface.as_raw() as _,
            client_decoration: client_decoration_resources,
        }));

        if let Err(e) = pointer.add_listener(shell_event_handler.get_mut()) {
            tracing::warn!(target = "pointer", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = xdg_surface.add_listener(shell_event_handler.get_mut()) {
            tracing::warn!(target = "xdg_surface", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = xdg_toplevel.add_listener(shell_event_handler.get_mut()) {
            tracing::warn!(target = "xdg_toplevel", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = xdg_wm_base.add_listener(shell_event_handler.get_mut()) {
            tracing::warn!(target = "xdg_wm_base", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = main_surface.add_listener(shell_event_handler.get_mut()) {
            tracing::warn!(target = "wl_surface", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = frame.add_listener(shell_event_handler.get_mut()) {
            tracing::warn!(target = "frame", reason = ?e, "Failed to set listener");
        }

        'optin_fractional_scale: {
            let Some(ref m) = fractional_scale_manager_v1 else {
                // no wp_fractional_scale_manager_v1
                break 'optin_fractional_scale;
            };

            let Ok(mut fs) = m.get_fractional_scale(&main_surface) else {
                // errored(logged via tracing::instrument)
                break 'optin_fractional_scale;
            };

            if let Err(e) = fs.add_listener(shell_event_handler.get_mut()) {
                tracing::warn!(target = "fractional_scale", reason = ?e, "Failed to set listener");
            }

            fs.leak();
        }

        if let Some(ref mut x) = gtk_shell1
            && let Err(e) = x.add_listener(shell_event_handler.get_mut())
        {
            tracing::warn!(target = "gtk_shell1", reason = ?e, "Failed to set listener");
        }
        if let Some(ref mut x) = gtk_surface
            && let Err(e) = x.add_listener(shell_event_handler.get_mut())
        {
            tracing::warn!(target = "gtk_surface1", reason = ?e, "Failed to set listener");
        }

        if let Err(e) = main_surface.commit() {
            tracing::warn!(reason = ?e, "Failed to commit wl_surface");
        }

        let pointer_input_manager = Box::pin(UnsafeCell::new(PointerInputManager::new()));

        compositor.leak();
        subcompositor.leak();
        viewporter.leak();
        xdg_wm_base.leak();
        seat.leak();
        cursor_shape_manager.leak();
        xdg_toplevel.leak();
        pointer.leak();
        if let Some(x) = fractional_scale_manager_v1 {
            x.leak();
        }
        if let Some(x) = gtk_shell1 {
            x.leak();
        }

        Self {
            shell_event_handler,
            display: dp,
            content_surface: main_surface.unwrap(),
            xdg_surface: xdg_surface.unwrap(),
            cursor_shape_device: cursor_shape_device.unwrap(),
            frame_callback: Cell::new(frame.unwrap()),
            zxdg_exporter_v2: zxdg_exporter_v2.map(|x| x.unwrap()),
            _gtk_surface: gtk_surface.map(|x| x.unwrap()),
            pointer_input_manager,
            _marker: core::marker::PhantomData,
        }
    }

    pub fn sync(&self) {
        if let Err(e) = self.display.roundtrip() {
            tracing::warn!(reason = ?e, "wayland display roundtrip failed");
        }
    }

    pub const fn is_floating_window(&self) -> bool {
        // TODO: detect floating/tiling window system
        false
    }

    pub const fn server_side_decoration_provided(&self) -> bool {
        // Ubuntu(Mutter/GNOME) has not server side decoration(no zxdg_decoration_manager_v1 provided)
        // TODO: detect this
        false
    }

    pub unsafe fn create_vulkan_surface(
        &mut self,
        instance: &impl br::Instance,
    ) -> br::Result<br::vk::VkSurfaceKHR> {
        unsafe {
            br::WaylandSurfaceCreateInfo::new(
                self.display.as_raw() as _,
                self.content_surface.as_ptr() as _,
            )
            .execute(instance, None)
        }
    }

    pub fn client_size(&self) -> (f32, f32) {
        let ui_scale_factor = unsafe { (*self.shell_event_handler.get()).ui_scale_factor };
        let (w, h) = unsafe { (*self.shell_event_handler.get()).cached_client_size };

        (w as f32 / ui_scale_factor, h as f32 / ui_scale_factor)
    }

    #[tracing::instrument(skip(self))]
    pub fn flush(&self) {
        if let Err(e) = self.display.flush() {
            tracing::warn!(reason = ?e, "Failed to flush display events");
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn prepare_read_events(&self) -> std::io::Result<()> {
        loop {
            match self.display.prepare_read() {
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if let Err(e) = self.display.dispatch_pending() {
                        tracing::error!(reason = ?e, "Failed to dispatch pending events");
                        return Err(e);
                    }
                }
                Err(e) => {
                    tracing::error!(reason = ?e, "Failed to prepare reading events");
                    return Err(e);
                }
            }
        }

        if let Err(e) = self.display.flush() {
            tracing::error!(reason = ?e, "Failed to flush outgoing events");
            return Err(e);
        }

        Ok(())
    }

    #[inline(always)]
    pub fn display_fd(&self) -> RawFd {
        self.display.as_raw_fd()
    }

    pub fn cancel_read_events(&self) {
        self.display.cancel_read();
    }

    #[tracing::instrument(skip(self))]
    pub fn read_and_process_events(&self) -> std::io::Result<()> {
        if let Err(e) = self.display.read_events() {
            tracing::error!(reason = ?e, "Failed to read events");
            return Err(e);
        }

        if let Err(e) = self.display.dispatch_pending() {
            tracing::warn!(reason = ?e, "Failed to dispatch incoming events");
        }

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub fn request_next_frame(&self) {
        let mut next_callback = match unsafe { self.content_surface.as_ref() }.frame() {
            Ok(cb) => cb,
            Err(e) => {
                tracing::warn!(reason = ?e, "Failed to request next frame");
                return;
            }
        };
        if let Err(e) = next_callback.add_listener(unsafe { &mut *self.shell_event_handler.get() })
        {
            tracing::warn!(target = "frame_callback", reason = ?e, "Failed to set listener");
        }

        self.frame_callback.set(next_callback.unwrap());
    }

    #[tracing::instrument(skip(self))]
    pub fn post_configure(&self, serial: u32) {
        tracing::trace!("ToplevelWindowSurfaceConfigure");

        if let Err(e) = unsafe { self.xdg_surface.as_ref() }.ack_configure(serial) {
            tracing::warn!(reason = ?e, "Failed to ack configure");
        }
    }

    pub fn capture_pointer(&self) {
        /* do nothing currently(maybe requires on floating-window system) */
    }

    pub fn release_pointer(&self) {
        /* do nothing currently(maybe requires on floating-window system) */
    }

    pub fn close_safe(&self) {
        // do nothing for wayland
    }

    pub fn minimize(&self) {
        // do nothing currently(maybe requires on floating-window system)
    }

    pub fn maximize(&self) {
        // do nothing currently(maybe requires on floating-window system)
    }

    #[tracing::instrument(skip(self))]
    pub fn set_cursor_shape(&self, enter_serial: u32, shape: CursorShape) {
        if let Err(e) = unsafe { self.cursor_shape_device.as_ref() }.set_shape(
            enter_serial,
            match shape {
                CursorShape::Default => WpCursorShapeDeviceV1Shape::Default,
                CursorShape::ResizeHorizontal => WpCursorShapeDeviceV1Shape::EwResize,
            },
        ) {
            tracing::warn!(reason = ?e, "Failed to set cursor shape");
        }
    }

    // wayland specific functionality
    pub fn try_export_toplevel(&self) -> Option<wl::Owned<wl::ZxdgExportedV2>> {
        let Some(ref x) = self.zxdg_exporter_v2 else {
            tracing::warn!("No zxdg_exporter_v2 found on the system");
            return None;
        };

        match unsafe { x.as_ref() }.export_toplevel(unsafe { self.content_surface.as_ref() }) {
            Ok(x) => Some(x),
            Err(e) => {
                tracing::warn!(reason = ?e, "Failed to get exported toplevel");
                None
            }
        }
    }

    #[inline]
    pub fn ui_scale_factor(&self) -> f32 {
        unsafe { (*self.shell_event_handler.get()).ui_scale_factor }
    }
}
