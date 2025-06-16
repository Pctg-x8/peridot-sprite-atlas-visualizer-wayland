use std::{
    cell::Cell,
    os::fd::{AsRawFd, RawFd},
    rc::Rc,
};

use bedrock::{self as br, SurfaceCreateInfo};

use crate::{
    AppEvent, AppEventBus,
    hittest::CursorShape,
    platform::linux::input_event_codes::BTN_LEFT,
    thirdparty::wl::{self, WpCursorShapeDeviceV1Shape, WpCursorShapeManagerV1},
};

enum PointerOnSurface {
    None,
    Main { serial: u32 },
}
struct WaylandShellEventHandler<'a> {
    app_event_bus: &'a AppEventBus,
    ui_scale_factor: Rc<Cell<f32>>,
    pointer_on_surface: PointerOnSurface,
    main_surface_proxy_ptr: *mut wl::Surface,
}
impl wl::XdgSurfaceEventListener for WaylandShellEventHandler<'_> {
    fn configure(&mut self, _: &mut wl::XdgSurface, serial: u32) {
        self.app_event_bus
            .push(AppEvent::ToplevelWindowSurfaceConfigure { serial });
    }
}
impl wl::XdgToplevelEventListener for WaylandShellEventHandler<'_> {
    fn configure(&mut self, _: &mut wl::XdgToplevel, width: i32, height: i32, states: &[i32]) {
        self.app_event_bus.push(AppEvent::ToplevelWindowConfigure {
            width: width as _,
            height: height as _,
        });

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
        self.ui_scale_factor.set(factor as _);
        // 同じ値を適用することでdpi-awareになるらしい
        surface.set_buffer_scale(factor).unwrap();
        surface.commit().unwrap();
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

pub struct AppShell<'a> {
    shell_event_handler: Box<WaylandShellEventHandler<'a>>,
    display: wl::Display,
    surface: core::ptr::NonNull<wl::Surface>,
    xdg_surface: core::ptr::NonNull<wl::XdgSurface>,
    zxdg_exporter_v2: Option<core::ptr::NonNull<wl::ZxdgExporterV2>>,
    cursor_shape_device: core::ptr::NonNull<wl::WpCursorShapeDeviceV1>,
    frame_callback: core::ptr::NonNull<wl::Callback>,
}
impl<'a> AppShell<'a> {
    #[tracing::instrument(skip(events))]
    pub fn new(events: &'a AppEventBus) -> Self {
        let mut dp = wl::Display::connect().unwrap();
        let mut registry = dp.get_registry().unwrap();
        struct RegistryListener {
            compositor: Option<wl::Owned<wl::Compositor>>,
            xdg_wm_base: Option<wl::Owned<wl::XdgWmBase>>,
            seat: Option<wl::Owned<wl::Seat>>,
            cursor_shape_manager: Option<wl::Owned<WpCursorShapeManagerV1>>,
            zxdg_exporter_v2: Option<wl::Owned<wl::ZxdgExporterV2>>,
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

                if interface == c"wl_compositor" {
                    self.compositor = match registry.bind(name, version) {
                        Ok(x) => Some(x),
                        Err(e) => {
                            tracing::warn!(reason = ?e, "Failed to bind");
                            None
                        }
                    };
                }

                if interface == c"xdg_wm_base" {
                    self.xdg_wm_base = match registry.bind(name, version) {
                        Ok(x) => Some(x),
                        Err(e) => {
                            tracing::warn!(reason = ?e, "Failed to bind");
                            None
                        }
                    };
                }

                if interface == c"wl_seat" {
                    self.seat = match registry.bind(name, version) {
                        Ok(x) => Some(x),
                        Err(e) => {
                            tracing::warn!(reason = ?e, "Failed to bind");
                            None
                        }
                    };
                }

                if interface == c"wp_cursor_shape_manager_v1" {
                    self.cursor_shape_manager = match registry.bind(name, version) {
                        Ok(x) => Some(x),
                        Err(e) => {
                            tracing::warn!(reason = ?e, "Failed to bind");
                            None
                        }
                    };
                }

                if interface == c"zxdg_exporter_v2" {
                    self.zxdg_exporter_v2 = match registry.bind(name, version) {
                        Ok(x) => Some(x),
                        Err(e) => {
                            tracing::warn!(reason = ?e, "Failed to bind");
                            None
                        }
                    }
                }
            }

            fn global_remove(&mut self, _registry: &mut wl::Registry, name: u32) {
                tracing::warn!(name, "unimplemented: wl global remove");
            }
        }
        let mut rl = RegistryListener {
            compositor: None,
            xdg_wm_base: None,
            seat: None,
            cursor_shape_manager: None,
            zxdg_exporter_v2: None,
        };
        if let Err(e) = registry.add_listener(&mut rl) {
            tracing::warn!(target = "registry", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = dp.roundtrip() {
            tracing::warn!(reason = ?e, "Failed to roundtrip");
        }
        drop(registry);

        let (
            mut compositor,
            mut xdg_wm_base,
            mut seat,
            mut cursor_shape_manager,
            mut zxdg_exporter_v2,
        );
        match rl {
            RegistryListener {
                compositor: Some(compositor1),
                xdg_wm_base: Some(xdg_wm_base1),
                seat: Some(seat1),
                cursor_shape_manager: Some(cursor_shape_manager1),
                zxdg_exporter_v2: zxdg_exporter_v21,
            } => {
                compositor = compositor1;
                xdg_wm_base = xdg_wm_base1;
                seat = seat1;
                cursor_shape_manager = cursor_shape_manager1;
                zxdg_exporter_v2 = zxdg_exporter_v21;
            }
            rl => {
                if rl.compositor.is_none() {
                    tracing::error!(
                        interface = "wl_compositor",
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

        let mut wl_surface = match compositor.create_surface() {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create wl_surface");
                std::process::abort();
            }
        };
        let mut xdg_surface = match xdg_wm_base.get_xdg_surface(&mut wl_surface) {
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

        let mut frame = match wl_surface.frame() {
            Ok(x) => x,
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to request next frame");
                std::process::abort();
            }
        };

        let mut shell_event_handler = Box::new(WaylandShellEventHandler {
            app_event_bus: events,
            ui_scale_factor: Rc::new(Cell::new(2.0)),
            pointer_on_surface: PointerOnSurface::None,
            main_surface_proxy_ptr: wl_surface.as_raw() as _,
        });

        if let Err(e) = pointer.add_listener(&mut *shell_event_handler) {
            tracing::warn!(target = "pointer", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = xdg_surface.add_listener(&mut *shell_event_handler) {
            tracing::warn!(target = "xdg_surface", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = xdg_toplevel.add_listener(&mut *shell_event_handler) {
            tracing::warn!(target = "xdg_toplevel", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = wl_surface.add_listener(&mut *shell_event_handler) {
            tracing::warn!(target = "wl_surface", reason = ?e, "Failed to set listener");
        }
        if let Err(e) = frame.add_listener(&mut *shell_event_handler) {
            tracing::warn!(target = "frame", reason = ?e, "Failed to set listener");
        }

        if let Err(e) = wl_surface.commit() {
            tracing::warn!(reason = ?e, "Failed to commit wl_surface");
        }

        compositor.leak();
        xdg_wm_base.leak();
        seat.leak();
        cursor_shape_manager.leak();
        xdg_toplevel.leak();
        pointer.leak();

        Self {
            shell_event_handler,
            display: dp,
            surface: wl_surface.unwrap(),
            xdg_surface: xdg_surface.unwrap(),
            cursor_shape_device: cursor_shape_device.unwrap(),
            frame_callback: frame.unwrap(),
            zxdg_exporter_v2: zxdg_exporter_v2.map(|x| x.unwrap()),
        }
    }

    pub unsafe fn create_vulkan_surface(
        &mut self,
        instance: &impl br::Instance,
    ) -> br::Result<br::vk::VkSurfaceKHR> {
        unsafe {
            br::WaylandSurfaceCreateInfo::new(
                self.display.as_raw() as _,
                self.surface.as_ptr() as _,
            )
            .execute(instance, None)
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn flush(&mut self) {
        if let Err(e) = self.display.flush() {
            tracing::warn!(reason = ?e, "Failed to flush display events");
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn process_pending_events(&mut self) {
        if let Err(e) = self.display.dispatch() {
            tracing::warn!(reason = ?e, "Failed to dispatch display events");
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn prepare_read_events(&mut self) -> std::io::Result<()> {
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

    pub fn cancel_read_events(&mut self) {
        self.display.cancel_read();
    }

    #[tracing::instrument(skip(self))]
    pub fn read_and_process_events(&mut self) -> std::io::Result<()> {
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
    pub fn request_next_frame(&mut self) {
        self.frame_callback = match unsafe { self.surface.as_mut() }.frame() {
            Ok(cb) => cb.unwrap(),
            Err(e) => {
                tracing::warn!(reason = ?e, "Failed to request next frame");
                return;
            }
        };
        if let Err(e) =
            unsafe { self.frame_callback.as_mut() }.add_listener(&mut *self.shell_event_handler)
        {
            tracing::warn!(target = "frame_callback", reason = ?e, "Failed to set listener");
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn post_configure(&mut self, serial: u32) {
        tracing::trace!("ToplevelWindowSurfaceConfigure");

        if let Err(e) = unsafe { self.xdg_surface.as_mut() }.ack_configure(serial) {
            tracing::warn!(reason = ?e, "Failed to ack configure");
        }
    }

    #[tracing::instrument(skip(self))]
    pub fn set_cursor_shape(&mut self, enter_serial: u32, shape: CursorShape) {
        if let Err(e) = unsafe { self.cursor_shape_device.as_mut() }.set_shape(
            enter_serial,
            match shape {
                CursorShape::Default => WpCursorShapeDeviceV1Shape::Default,
                CursorShape::ResizeHorizontal => WpCursorShapeDeviceV1Shape::EwResize,
            },
        ) {
            tracing::warn!(reason = ?e, "Failed to set cursor shape");
        }
    }

    #[inline]
    pub fn ui_scale_factor(&self) -> f32 {
        self.shell_event_handler.ui_scale_factor.get()
    }
}
