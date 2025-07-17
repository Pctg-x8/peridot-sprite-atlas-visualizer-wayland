use std::{
    cell::{Cell, UnsafeCell},
    os::fd::{AsRawFd, RawFd},
    pin::Pin,
};

use bedrock::{self as br, SurfaceCreateInfo};
use wayland::{self as wl, WpCursorShapeDeviceV1Shape, WpCursorShapeManagerV1};

use crate::{
    AppEvent, AppEventBus,
    base_system::AppBaseSystem,
    hittest::{CursorShape, Role},
    input::PointerInputManager,
    platform::linux::{
        MemoryMapFlags, MemoryProtectionFlags, OpenFlags, TemporalSharedMemory,
        input_event_codes::{BTN_LEFT, BTN_RIGHT},
    },
};

struct DataOfferSession {
    obj: wl::Owned<wl::DataOffer>,
    offered_mime_types: Vec<std::ffi::CString>,
    allowed_source_actions: wl::DataDeviceManagerDndAction,
}
impl wl::DataOfferEventListener for DataOfferSession {
    #[tracing::instrument(
        name = "<WaylandShellEventHandler as DataOfferEventListener>::offer",
        skip(self, sender)
    )]
    fn offer(&mut self, sender: &mut wl::DataOffer, mime_type: &core::ffi::CStr) {
        assert!(self.obj.ref_eq(sender), "different offer?");

        self.offered_mime_types.push(mime_type.into());
    }

    #[tracing::instrument(
        name = "<WaylandShellEventHandler as DataOfferEventListener>::source_action",
        skip(self, sender)
    )]
    fn source_actions(
        &mut self,
        sender: &mut wl::DataOffer,
        source_actions: wl::DataDeviceManagerDndAction,
    ) {
        assert!(self.obj.ref_eq(sender), "different offer?");

        self.allowed_source_actions = source_actions;
    }

    #[tracing::instrument(
        name = "<WaylandShellEventHandler as DataOfferEventListener>::action",
        skip(self, _sender)
    )]
    fn action(&mut self, _sender: &mut wl::DataOffer, dnd_action: wl::DataDeviceManagerDndAction) {
        tracing::trace!("action");
    }
}
impl DataOfferSession {
    fn is_offer(&self, other: &wl::DataOffer) -> bool {
        self.obj.ref_eq(other)
    }
}

enum PointerOnSurface {
    None,
    Main { serial: u32 },
    ResizeEdge { edge: wl::XdgToplevelResizeEdge },
}
struct WaylandShellEventHandler<'a, 'subsystem> {
    app_event_bus: &'a AppEventBus,
    cached_client_size_px: (u32, u32),
    buffer_scale: u32,
    ui_scale_factor: f32,
    pointer_on_surface: PointerOnSurface,
    main_surface_proxy_ptr: *mut wl::Surface,
    xdg_surface_proxy_ptr: *mut wl::XdgSurface,
    xdg_toplevel_proxy_ptr: *mut wl::XdgToplevel,
    primary_seat_ptr: *mut wl::Seat,
    client_decoration: Option<AppShellDecorator>,
    pointer_input_manager: Pin<Box<UnsafeCell<PointerInputManager>>>,
    base_system_ptr: *mut AppBaseSystem<'subsystem>,
    cursor_shape_device: *mut wl::WpCursorShapeDeviceV1,
    pointer_last_surface_pos: (wl::Fixed, wl::Fixed),
    tiled: bool,
    title_bar_last_click: Option<std::time::Instant>,
    active_data_offer: Option<Pin<Box<DataOfferSession>>>,
}
impl wl::XdgWmBaseEventListener for WaylandShellEventHandler<'_, '_> {
    fn ping(&mut self, wm_base: &mut wl::XdgWmBase, serial: u32) {
        if let Err(e) = wm_base.pong(serial) {
            tracing::warn!(reason = ?e, serial, "Failed to respond wm_base.ping");
        }
    }
}
impl wl::XdgSurfaceEventListener for WaylandShellEventHandler<'_, '_> {
    #[tracing::instrument(
        name = "<WaylandShellEventHandler as wl::XdgSurfaceEventListener>::configure",
        skip(self, sender)
    )]
    fn configure(&mut self, sender: &mut wl::XdgSurface, serial: u32) {
        if let Err(e) = sender.ack_configure(serial) {
            tracing::warn!(reason = ?e, "ack_configure failed");
        }
    }
}
impl wl::XdgToplevelEventListener for WaylandShellEventHandler<'_, '_> {
    #[tracing::instrument(
        name = "<WaylandShellEventHandler as XdgToplevelEventListener>::configure",
        skip(self, _toplevel)
    )]
    fn configure(
        &mut self,
        _toplevel: &mut wl::XdgToplevel,
        mut width: i32,
        mut height: i32,
        states: &[i32],
    ) {
        assert!(width >= 0, "negative width?");
        assert!(height >= 0, "negative height?");

        tracing::trace!("configure");
        let activated = states.contains(&4);
        self.tiled = states.iter().any(|&x| x == 5 || x == 6 || x == 7 || x == 8);

        if width == 0 {
            width = self.cached_client_size_px.0 as i32;
        }
        if height == 0 {
            height = self.cached_client_size_px.1 as i32;
        }

        let width_px = width as u32 * self.buffer_scale;
        let height_px = height as u32 * self.buffer_scale;

        self.cached_client_size_px = (width_px, height_px);
        self.app_event_bus.push(AppEvent::ToplevelWindowNewSize {
            width_px,
            height_px,
        });

        unsafe { &*self.xdg_surface_proxy_ptr }
            .set_window_geometry(0, 0, width, height)
            .unwrap();
        if let Some(ref deco) = self.client_decoration {
            deco.adjust_for_main_surface_size(width, height);
            if activated {
                deco.active();
            } else {
                deco.inactive();
            }
        }
        unsafe { &*self.main_surface_proxy_ptr }.commit().unwrap();
    }

    fn close(&mut self, _: &mut wl::XdgToplevel) {
        self.app_event_bus.push(AppEvent::ToplevelWindowClose);
    }

    #[tracing::instrument(
        name = "<WaylandShellEventHandler as XdgToplevelEventListener>::configure_bounds",
        skip(self, _toplevel)
    )]
    fn configure_bounds(&mut self, _toplevel: &mut wl::XdgToplevel, width: i32, height: i32) {
        tracing::trace!("configure bounds");
    }

    #[tracing::instrument(
        name = "<WaylandShellEventHandler as XdgToplevelEventListener>::wm_capabilities",
        skip(self, _toplevel)
    )]
    fn wm_capabilities(&mut self, _toplevel: &mut wl::XdgToplevel, capabilities: &[i32]) {
        tracing::trace!("wm capabilities");
    }
}
impl wl::SurfaceEventListener for WaylandShellEventHandler<'_, '_> {
    fn enter(&mut self, _surface: &mut wl::Surface, _output: &mut wl::Output) {
        tracing::trace!("enter output");
    }

    fn leave(&mut self, _surface: &mut wl::Surface, _output: &mut wl::Output) {
        tracing::trace!("leave output");
    }

    fn preferred_buffer_scale(&mut self, surface: &mut wl::Surface, factor: i32) {
        assert!(factor > 0, "negative or zero scale factor?");

        tracing::trace!(factor, "preferred buffer scale");
        self.buffer_scale = factor as _;
        // 同じ値を適用することでdpi-awareになるらしい
        surface.set_buffer_scale(factor).unwrap();
        surface.commit().unwrap();
        if let Some(ref mut deco) = self.client_decoration {
            deco.set_buffer_scale(factor);
        }

        self.ui_scale_factor = factor as _;
    }

    fn preferred_buffer_transform(&mut self, _surface: &mut wl::Surface, transform: u32) {
        tracing::trace!(transform, "preferred buffer transform");
    }
}
impl wl::PointerEventListener for WaylandShellEventHandler<'_, '_> {
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
        } else if let Some(ref deco) = self.client_decoration
            && let Some(e) = deco.resize_edge(surface)
        {
            PointerOnSurface::ResizeEdge { edge: e }
        } else {
            PointerOnSurface::None
        };

        match self.pointer_on_surface {
            PointerOnSurface::None => {}
            PointerOnSurface::ResizeEdge { edge } => {
                unsafe { &*self.cursor_shape_device }
                    .set_shape(
                        serial,
                        match edge {
                            wl::XdgToplevelResizeEdge::Top | wl::XdgToplevelResizeEdge::Bottom => {
                                wl::WpCursorShapeDeviceV1Shape::NsResize
                            }
                            wl::XdgToplevelResizeEdge::Left | wl::XdgToplevelResizeEdge::Right => {
                                wl::WpCursorShapeDeviceV1Shape::EwResize
                            }
                            wl::XdgToplevelResizeEdge::TopLeft => {
                                wl::WpCursorShapeDeviceV1Shape::NwResize
                            }
                            wl::XdgToplevelResizeEdge::BottomLeft => {
                                wl::WpCursorShapeDeviceV1Shape::SwResize
                            }
                            wl::XdgToplevelResizeEdge::TopRight => {
                                wl::WpCursorShapeDeviceV1Shape::NeResize
                            }
                            wl::XdgToplevelResizeEdge::BottomRight => {
                                wl::WpCursorShapeDeviceV1Shape::SeResize
                            }
                        },
                    )
                    .unwrap();
            }
            PointerOnSurface::Main { serial } => {
                self.app_event_bus.push(AppEvent::MainWindowPointerMove {
                    enter_serial: serial,
                    surface_x: surface_x.to_f32() * self.buffer_scale as f32 / self.ui_scale_factor,
                    surface_y: surface_y.to_f32() * self.buffer_scale as f32 / self.ui_scale_factor,
                })
            }
        }
    }

    fn leave(&mut self, _pointer: &mut wl::Pointer, _serial: u32, surface: &mut wl::Surface) {
        match self.pointer_on_surface {
            PointerOnSurface::None => (),
            PointerOnSurface::ResizeEdge { .. } => {
                self.pointer_on_surface = PointerOnSurface::None;
            }
            PointerOnSurface::Main { .. } => {
                if core::ptr::addr_eq(surface, self.main_surface_proxy_ptr) {
                    // TODO: notify pointer_leave for currently entering element
                    self.pointer_on_surface = PointerOnSurface::None;
                }
            }
        }
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
            PointerOnSurface::ResizeEdge { .. } => (),
            PointerOnSurface::Main { serial } => {
                // TODO: pointer recognition
                self.pointer_last_surface_pos = (surface_x, surface_y);

                self.app_event_bus.push(AppEvent::MainWindowPointerMove {
                    enter_serial: serial,
                    surface_x: surface_x.to_f32() * self.buffer_scale as f32 / self.ui_scale_factor,
                    surface_y: surface_y.to_f32() * self.buffer_scale as f32 / self.ui_scale_factor,
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
            PointerOnSurface::ResizeEdge { edge } => {
                if button == BTN_LEFT && state == wl::PointerButtonState::Pressed {
                    unsafe { &*self.xdg_toplevel_proxy_ptr }
                        .resize(unsafe { &*self.primary_seat_ptr }, serial, edge)
                        .unwrap();
                }
            }
            PointerOnSurface::Main {
                serial: enter_serial,
            } => {
                if button == BTN_LEFT && state == wl::PointerButtonState::Pressed {
                    // TODO: detect whether floating window system and client side decorated
                    let role = self
                        .pointer_input_manager
                        .get_mut()
                        .role_focus(unsafe { &(*self.base_system_ptr).hit_tree });
                    match role {
                        Some(Role::TitleBar) => {
                            if let Some(lc) = self.title_bar_last_click.take()
                                && lc.elapsed() <= std::time::Duration::from_millis(400)
                            {
                                // double click
                                self.app_event_bus
                                    .push(AppEvent::ToplevelWindowMaximizeRequest);
                            } else {
                                unsafe { &*self.xdg_toplevel_proxy_ptr }
                                    .r#move(unsafe { &*self.primary_seat_ptr }, serial)
                                    .unwrap();
                                self.title_bar_last_click = Some(std::time::Instant::now());
                            }

                            return;
                        }
                        _ => (),
                    }

                    self.app_event_bus
                        .push(AppEvent::MainWindowPointerLeftDown { enter_serial });
                } else if button == BTN_LEFT && state == wl::PointerButtonState::Released {
                    self.app_event_bus
                        .push(AppEvent::MainWindowPointerLeftUp { enter_serial });
                } else if button == BTN_RIGHT && state == wl::PointerButtonState::Pressed {
                    // TODO: detect whether floating window system and client side decorated
                    let role = self
                        .pointer_input_manager
                        .get_mut()
                        .role_focus(unsafe { &(*self.base_system_ptr).hit_tree });
                    match role {
                        Some(Role::TitleBar) => {
                            unsafe { &*self.xdg_toplevel_proxy_ptr }
                                .show_window_menu(
                                    unsafe { &*self.primary_seat_ptr },
                                    serial,
                                    self.pointer_last_surface_pos.0.to_f32() as _,
                                    self.pointer_last_surface_pos.1.to_f32() as _,
                                )
                                .unwrap();
                            return;
                        }
                        _ => (),
                    }
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
impl wl::CallbackEventListener for WaylandShellEventHandler<'_, '_> {
    fn done(&mut self, _callback: &mut wl::Callback, _data: u32) {
        self.app_event_bus.push(AppEvent::ToplevelWindowFrameTiming);
    }
}
impl wl::WpFractionalScaleV1EventListener for WaylandShellEventHandler<'_, '_> {
    fn preferred_scale(&mut self, _object: &mut wl::WpFractionalScaleV1, scale: u32) {
        self.ui_scale_factor = scale as f32 / 120.0;
    }
}
impl wl::GtkShell1EventListener for WaylandShellEventHandler<'_, '_> {
    fn capabilities(&mut self, _sender: &mut wl::GtkShell1, capabilities: u32) {
        tracing::trace!(capabilities, "gtk_shell capabilities");
    }
}
impl wl::GtkSurface1EventListener for WaylandShellEventHandler<'_, '_> {
    fn configure(&mut self, _sender: &mut wl::GtkSurface1, states: &[u32]) {
        tracing::trace!(?states, "gtk_surface configure");
    }

    fn configure_edges(&mut self, _sender: &mut wl::GtkSurface1, constraints: &[u32]) {
        tracing::trace!(?constraints, "gtk_surface configure edges");
    }
}
impl wl::ZxdgToplevelDecorationV1EventListener for WaylandShellEventHandler<'_, '_> {
    #[tracing::instrument(
        name = "<WaylandShellEventHandler as ZxdgToplevelDecorationV1EventListener>::configure",
        skip(self, sender),
        fields(mode = mode as u32)
    )]
    fn configure(
        &mut self,
        sender: &mut wl::ZxdgToplevelDecorationV1,
        mode: wl::ZxdgToplevelDecorationMode,
    ) {
        if mode == wl::ZxdgToplevelDecorationMode::ServerSide {
            sender
                .set_mode(wl::ZxdgToplevelDecorationMode::ServerSide)
                .unwrap();
            return;
        }

        todo!("non server side decoration support");
    }
}
impl wl::DataDeviceEventListener for WaylandShellEventHandler<'_, '_> {
    #[tracing::instrument(
        name = "<WaylandShellEventHandler as DataDeviceEventListener>::data_offer",
        skip(self, _sender, id)
    )]
    fn data_offer(&mut self, _sender: &mut wl::DataDevice, id: wl::Owned<wl::DataOffer>) {
        let mut session = Box::pin(DataOfferSession {
            obj: id,
            offered_mime_types: Vec::new(),
            allowed_source_actions: wl::DataDeviceManagerDndAction::NONE,
        });
        if let Err(_) = unsafe {
            session
                .obj
                .copy_ptr()
                .as_mut()
                .add_listener(session.as_mut().get_mut())
        } {
            tracing::warn!("Failed to set wl_data_offer listener");
        }
        self.active_data_offer = Some(session);
    }

    #[tracing::instrument(
        name = "<WaylandShellEventHandler as DataDeviceEventListener>::enter",
        skip(self, _sender, surface, id)
    )]
    fn enter(
        &mut self,
        _sender: &mut wl::DataDevice,
        serial: u32,
        surface: &wl::Surface,
        x: wl::Fixed,
        y: wl::Fixed,
        id: Option<&wl::DataOffer>,
    ) {
        tracing::trace!("enter");

        if !core::ptr::addr_eq(self.main_surface_proxy_ptr, surface) {
            // not entering to main surface
            self.active_data_offer = None;
            return;
        }

        let Some(ref offer_session) = self.active_data_offer else {
            // no data_offer session
            return;
        };

        if id.is_none_or(|id| !offer_session.is_offer(id)) {
            // different offer or null offer
            return;
        }

        let Some(accepted_mime) = offer_session
            .offered_mime_types
            .iter()
            .find(|x| x.as_c_str() == c"text/uri-list")
        else {
            tracing::warn!(offered_mime_types = ?offer_session.offered_mime_types, "cannot accept any of offerred mime types");
            self.active_data_offer = None;
            return;
        };

        if let Err(e) = offer_session.obj.accept(serial, Some(accepted_mime)) {
            tracing::warn!(reason = ?e, ?accepted_mime, "Failed to accept the mime");
            self.active_data_offer = None;
            return;
        }

        if !offer_session
            .allowed_source_actions
            .contains(wl::DataDeviceManagerDndAction::COPY)
        {
            tracing::warn!(allowed_source_actions = ?offer_session.allowed_source_actions, "copy operation is not allowed for this source");
            self.active_data_offer = None;
            return;
        }

        if let Err(e) = offer_session.obj.set_actions(
            offer_session.allowed_source_actions,
            wl::DataDeviceManagerDndAction::COPY,
        ) {
            tracing::warn!(reason = ?e, "Failed to set dnd action");
            self.active_data_offer = None;
            return;
        }

        self.app_event_bus.push(AppEvent::UIShowDragAndDropOverlay);
    }

    #[tracing::instrument(
        name = "<WaylandShellEventHandler as DataDeviceEventListener>::leave",
        skip(self, _sender)
    )]
    fn leave(&mut self, _sender: &mut wl::DataDevice) {
        tracing::trace!("leave");

        // drop active offer session
        self.active_data_offer = None;
        self.app_event_bus.push(AppEvent::UIHideDragAndDropOverlay);
    }

    #[tracing::instrument(
        name = "<WaylandShellEventHandler as DataDeviceEventListener>::motion",
        skip(self, _sender)
    )]
    fn motion(&mut self, _sender: &mut wl::DataDevice, time: u32, x: wl::Fixed, y: wl::Fixed) {
        tracing::trace!("motion");
    }

    #[tracing::instrument(
        name = "<WaylandShellEventHandler as DataDeviceEventListener>::drop",
        skip(self, sender)
    )]
    fn drop(&mut self, sender: &mut wl::DataDevice) {
        tracing::trace!("drop");

        let Some(offer_session) = self.active_data_offer.take() else {
            // not in an offer session
            return;
        };

        let Some(accepted_mime) = offer_session
            .offered_mime_types
            .iter()
            .find(|x| x.as_c_str() == c"text/uri-list")
        else {
            tracing::warn!(offered_mime_types = ?offer_session.offered_mime_types, "cannot accept any of offerred mime types");
            self.active_data_offer = None;
            return;
        };

        let mut pipefd = [0 as core::ffi::c_int; 2];
        let r = unsafe { libc::pipe(pipefd.as_mut_ptr()) };
        if r < 0 {
            tracing::warn!(reason = ?std::io::Error::last_os_error(), "Failed to create pipe");
            return;
        }
        let [readfd, writefd] = pipefd;

        if let Err(e) = offer_session.obj.receive(accepted_mime, &writefd) {
            tracing::warn!(reason = ?e, "Failed to request receive");
            unsafe {
                libc::close(readfd);
                libc::close(writefd);
            }
            return;
        }
        unsafe {
            libc::close(writefd);
        }

        let r = unsafe { wl::ffi::wl_display_roundtrip(sender.display()) };
        println!("rt {r}");

        let mut received = Vec::<u8>::new();
        let mut readbuf = vec![0u8; 8192];
        loop {
            let b = unsafe { libc::read(readfd, readbuf.as_mut_ptr() as *mut _, readbuf.len()) };
            if b <= 0 {
                break;
            }

            received.extend(&readbuf[..b as usize]);
        }

        println!("dnd receive: {}", unsafe {
            str::from_utf8_unchecked(&received)
        });

        unsafe {
            libc::close(readfd);
        }

        if let Err(e) = offer_session.obj.finish() {
            tracing::warn!(reason = ?e, "Failed to emit finish");
            return;
        }

        self.app_event_bus
            .push(AppEvent::AddSpritesByUriList(unsafe {
                str::from_utf8_unchecked(&received)
                    .split("\r\n")
                    .filter(|x| !x.starts_with("#"))
                    .filter(|x| !x.is_empty())
                    .map(|x| std::ffi::CString::new(x).unwrap())
                    .collect::<Vec<_>>()
            }));
    }

    #[tracing::instrument(
        name = "<WaylandShellEventHandler as DataDeviceEventListener>::selection",
        skip(self, _sender, id)
    )]
    fn selection(&mut self, _sender: &mut wl::DataDevice, id: Option<&wl::DataOffer>) {
        tracing::trace!("selection");

        if let (Some(offer), Some(active_offer)) = (id, self.active_data_offer.as_ref())
            && active_offer.is_offer(offer)
        {
            // drop active offer
            self.active_data_offer = None;
        }
    }
}

pub struct AppShell<'a, 'subsystem> {
    shell_event_handler: Box<UnsafeCell<WaylandShellEventHandler<'a, 'subsystem>>>,
    display: wl::Display,
    content_surface: core::ptr::NonNull<wl::Surface>,
    xdg_surface: core::ptr::NonNull<wl::XdgSurface>,
    xdg_toplevel: core::ptr::NonNull<wl::XdgToplevel>,
    zxdg_exporter_v2: Option<core::ptr::NonNull<wl::ZxdgExporterV2>>,
    cursor_shape_device: core::ptr::NonNull<wl::WpCursorShapeDeviceV1>,
    frame_callback: Cell<core::ptr::NonNull<wl::Callback>>,
    _gtk_surface: Option<core::ptr::NonNull<wl::GtkSurface1>>,
    data_device: Option<core::ptr::NonNull<wl::DataDevice>>,
    has_server_side_decoration: bool,
}
impl<'a, 'subsystem> AppShell<'a, 'subsystem> {
    #[tracing::instrument(skip(events, base_sys))]
    pub fn new(events: &'a AppEventBus, base_sys: *mut AppBaseSystem<'subsystem>) -> Self {
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
            zxdg_decoration_manager_v1: Option<wl::Owned<wl::ZxdgDecorationManagerV1>>,
            data_device_manager: Option<wl::Owned<wl::DataDeviceManager>>,
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
                } else if interface == c"zxdg_decoration_manager_v1" {
                    self.zxdg_decoration_manager_v1 = try_bind(registry, name, version);
                } else if interface == c"wl_data_device_manager" {
                    self.data_device_manager = try_bind(registry, name, version);
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
            zxdg_decoration_manager_v1: None,
            data_device_manager: None,
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
            zxdg_decoration_manager_v1,
            data_device_manager,
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
                zxdg_decoration_manager_v1: zxdg_decoration_manager_v11,
                data_device_manager: data_device_manager1,
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
                zxdg_decoration_manager_v1 = zxdg_decoration_manager_v11;
                data_device_manager = data_device_manager1;
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
        xdg_toplevel.set_min_size(160, 120).unwrap();

        let app_decorator;
        if zxdg_decoration_manager_v1.is_none() {
            // client decoration: backdrop shadow
            let deco = AppShellDecorator::new(
                &compositor,
                &subcompositor,
                shm,
                &viewporter,
                &main_surface,
            );
            deco.active();
            app_decorator = Some(deco);
        } else {
            app_decorator = None;
        }

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

        let mut data_device = if let Some(ref m) = data_device_manager {
            match m.get_data_device(&seat) {
                Ok(x) => Some(x),
                Err(e) => {
                    tracing::warn!(reason = ?e, "Failed to get data device");
                    None
                }
            }
        } else {
            None
        };

        let pointer_input_manager = Box::pin(UnsafeCell::new(PointerInputManager::new()));

        let mut shell_event_handler = Box::new(UnsafeCell::new(WaylandShellEventHandler {
            app_event_bus: events,
            // 現時点ではわからないので適当な値を設定
            cached_client_size_px: (640, 480),
            buffer_scale: 1,
            ui_scale_factor: 1.0,
            pointer_on_surface: PointerOnSurface::None,
            main_surface_proxy_ptr: main_surface.as_raw() as _,
            xdg_surface_proxy_ptr: xdg_surface.as_raw() as _,
            xdg_toplevel_proxy_ptr: xdg_toplevel.as_raw() as _,
            primary_seat_ptr: seat.as_raw() as _,
            client_decoration: app_decorator,
            pointer_input_manager,
            base_system_ptr: base_sys,
            cursor_shape_device: cursor_shape_device.as_raw() as _,
            pointer_last_surface_pos: (
                wl::Fixed::from_f32_lossy(0.0),
                wl::Fixed::from_f32_lossy(0.0),
            ),
            tiled: false,
            title_bar_last_click: None,
            active_data_offer: None,
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
        if let Some(ref mut x) = data_device
            && let Err(e) = x.add_listener(shell_event_handler.get_mut())
        {
            tracing::warn!(target = "wl_data_device", reason = ?e, "Failed to set listener");
        }

        'optin_decoration: {
            let Some(ref m) = zxdg_decoration_manager_v1 else {
                // no zxdg_decoration_manager_v1
                break 'optin_decoration;
            };

            let Ok(mut x) = m.get_toplevel_decoration(&xdg_toplevel) else {
                // errored(logged via tracing::instrument)
                break 'optin_decoration;
            };

            if let Err(e) = x.add_listener(shell_event_handler.get_mut()) {
                tracing::warn!(target = "zxdg_toplevel_decoration", reason = ?e, "Failed to set listener");
            }

            x.leak();
        }

        if let Err(e) = main_surface.commit() {
            tracing::warn!(reason = ?e, "Failed to commit wl_surface");
        }

        if let Err(e) = dp.roundtrip() {
            tracing::warn!(reason = ?e, "Failed to final roundtrip");
        }

        compositor.leak();
        subcompositor.leak();
        viewporter.leak();
        xdg_wm_base.leak();
        seat.leak();
        cursor_shape_manager.leak();
        pointer.leak();
        if let Some(x) = fractional_scale_manager_v1 {
            x.leak();
        }
        if let Some(x) = gtk_shell1 {
            x.leak();
        }
        if let Some(x) = data_device_manager {
            x.leak();
        }
        let has_server_side_decoration = zxdg_decoration_manager_v1.is_some();
        if let Some(zxdg_decoration_manager_v1) = zxdg_decoration_manager_v1 {
            zxdg_decoration_manager_v1.leak();
        }

        Self {
            shell_event_handler,
            display: dp,
            content_surface: main_surface.unwrap(),
            xdg_surface: xdg_surface.unwrap(),
            xdg_toplevel: xdg_toplevel.unwrap(),
            cursor_shape_device: cursor_shape_device.unwrap(),
            frame_callback: Cell::new(frame.unwrap()),
            zxdg_exporter_v2: zxdg_exporter_v2.map(|x| x.unwrap()),
            _gtk_surface: gtk_surface.map(|x| x.unwrap()),
            data_device: data_device.map(|x| x.unwrap()),
            has_server_side_decoration,
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
        self.has_server_side_decoration
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
        let (w, h) = unsafe { (*self.shell_event_handler.get()).cached_client_size_px };

        (w as f32 / ui_scale_factor, h as f32 / ui_scale_factor)
    }

    pub fn client_size_pixels(&self) -> (u32, u32) {
        let (w, h) = unsafe { (*self.shell_event_handler.get()).cached_client_size_px };

        (w, h)
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
        if let Err(e) = unsafe { self.xdg_toplevel.as_ref() }.set_minimized() {
            tracing::warn!(reason = ?e, "Failed to call set_minimized");
        }
    }

    pub fn toggle_maximize_restore(&self) {
        if self.is_tiled() {
            if let Err(e) = unsafe { self.xdg_toplevel.as_ref() }.unset_maximized() {
                tracing::warn!(reason = ?e, "Failed to call unset_maximized");
            }
        } else {
            if let Err(e) = unsafe { self.xdg_toplevel.as_ref() }.set_maximized() {
                tracing::warn!(reason = ?e, "Failed to call set_maximized");
            }
        }
    }

    pub fn is_tiled(&self) -> bool {
        unsafe { &*self.shell_event_handler.get() }.tiled
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

    #[inline]
    pub fn pointer_input_manager(&self) -> &UnsafeCell<PointerInputManager> {
        unsafe { &(*self.shell_event_handler.get()).pointer_input_manager }
    }
}

fn try_create_shm_random_suffix(
    prefix: String,
) -> Result<Option<TemporalSharedMemory>, std::io::Error> {
    let mut path_cstr_bytes = prefix.into_bytes();
    path_cstr_bytes.extend(b"XXXXXX\0");
    // random name gen: https://wayland-book.com/surfaces/shared-memory.html
    for _ in 0..100 {
        let mut ts = core::mem::MaybeUninit::uninit();
        if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, ts.as_mut_ptr()) } < 0 {
            continue;
        }
        let ts = unsafe { ts.assume_init_ref() };
        let mut r = ts.tv_nsec;
        for p in 0..6 {
            let fplen = path_cstr_bytes.len();
            path_cstr_bytes[fplen - p - 2] = b'A' + (r & 15) as u8 + (r & 16) as u8 * 2;
            r >>= 5;
        }

        match TemporalSharedMemory::create(
            unsafe { std::ffi::CString::from_vec_with_nul_unchecked(path_cstr_bytes) },
            OpenFlags::READ_WRITE | OpenFlags::EXCLUSIVE,
            0o600,
        ) {
            Ok(x) => {
                return Ok(Some(x));
            }
            Err((e, name)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // name conflict: retry
                path_cstr_bytes = name.into_bytes_with_nul();
                continue;
            }
            Err((e, _)) => {
                return Err(e);
            }
        }
    }

    tracing::error!("shm creation failed(similar names already exists, try limit reached)");
    Ok(None)
}

struct AppShellDecorator {
    compositor: core::ptr::NonNull<wl::Compositor>,
    shm: core::ptr::NonNull<wl::Shm>,
    deco_shm: TemporalSharedMemory,
    straight_shadow_start_bytes: usize,
    shm_size: usize,
    buffer_scale: i32,
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
impl AppShellDecorator {
    const SHADOW_SIZE: usize = 16;
    const INPUT_SIZE: usize = 8;

    pub fn new(
        compositor: &wl::Owned<wl::Compositor>,
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
            .set_source(0.0, 0.0, Self::SHADOW_SIZE as f32 * 2.0, 1.0)
            .unwrap();
        shadow_right_viewport
            .set_source(0.0, 0.0, Self::SHADOW_SIZE as f32 * 2.0, 1.0)
            .unwrap();
        shadow_top_viewport
            .set_source(0.0, 0.0, 1.0, Self::SHADOW_SIZE as f32 * 2.0)
            .unwrap();
        shadow_bottom_viewport
            .set_source(0.0, 0.0, 1.0, Self::SHADOW_SIZE as f32 * 2.0)
            .unwrap();

        // fixed configurations
        shadow_rt_surface
            .set_buffer_transform(wl::OutputTransform::Rot90)
            .unwrap();
        shadow_rb_surface
            .set_buffer_transform(wl::OutputTransform::Rot180)
            .unwrap();
        shadow_lb_surface
            .set_buffer_transform(wl::OutputTransform::Rot270)
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

        shadow_lt_subsurface.place_below(main_surface).unwrap();
        shadow_lb_subsurface.place_below(main_surface).unwrap();
        shadow_rt_subsurface.place_below(main_surface).unwrap();
        shadow_rb_subsurface.place_below(main_surface).unwrap();
        shadow_left_subsurface.place_below(main_surface).unwrap();
        shadow_right_subsurface.place_below(main_surface).unwrap();
        shadow_top_subsurface.place_below(main_surface).unwrap();
        shadow_bottom_subsurface.place_below(main_surface).unwrap();

        // fixed position
        shadow_lt_subsurface
            .set_position(-(Self::SHADOW_SIZE as i32), -(Self::SHADOW_SIZE as i32))
            .unwrap();
        shadow_left_subsurface
            .set_position(-(Self::SHADOW_SIZE as i32), Self::SHADOW_SIZE as i32)
            .unwrap();
        shadow_top_subsurface
            .set_position(Self::SHADOW_SIZE as i32, -(Self::SHADOW_SIZE as i32))
            .unwrap();

        let vertical_shadow_start_bytes: usize = Self::SHADOW_SIZE * 2 * Self::SHADOW_SIZE * 2 * 4;
        let shm_size = vertical_shadow_start_bytes + Self::SHADOW_SIZE * 2 * 1 * 4;
        let deco_shm = Self::create_shm_or_abort(shm_size);

        let deco_shm_pool = shm.create_pool(&deco_shm, shm_size as _).unwrap();
        let shadow_corner_buf = deco_shm_pool
            .create_buffer(
                0,
                (Self::SHADOW_SIZE * 2) as _,
                (Self::SHADOW_SIZE * 2) as _,
                (Self::SHADOW_SIZE * 2) as i32 * 4,
                wl::ShmFormat::ARGB8888,
            )
            .unwrap();
        let shadow_straight_buf = deco_shm_pool
            .create_buffer(
                vertical_shadow_start_bytes as _,
                (Self::SHADOW_SIZE * 2) as _,
                1,
                (Self::SHADOW_SIZE * 2) as i32 * 4,
                wl::ShmFormat::ARGB8888,
            )
            .unwrap();

        shadow_lt_surface
            .attach(Some(&shadow_corner_buf), 0, 0)
            .unwrap();
        shadow_lt_surface.commit().unwrap();
        shadow_lb_surface
            .attach(Some(&shadow_corner_buf), 0, 0)
            .unwrap();
        shadow_lb_surface.commit().unwrap();
        shadow_rb_surface
            .attach(Some(&shadow_corner_buf), 0, 0)
            .unwrap();
        shadow_rb_surface.commit().unwrap();
        shadow_rt_surface
            .attach(Some(&shadow_corner_buf), 0, 0)
            .unwrap();
        shadow_rt_surface.commit().unwrap();
        shadow_left_surface
            .attach(Some(&shadow_straight_buf), 0, 0)
            .unwrap();
        shadow_left_surface.commit().unwrap();
        shadow_right_surface
            .attach(Some(&shadow_straight_buf), 0, 0)
            .unwrap();
        shadow_right_surface.commit().unwrap();
        shadow_top_surface
            .attach(Some(&shadow_straight_buf), 0, 0)
            .unwrap();
        shadow_top_surface.commit().unwrap();
        shadow_bottom_surface
            .attach(Some(&shadow_straight_buf), 0, 0)
            .unwrap();
        shadow_bottom_surface.commit().unwrap();

        shadow_lt_subsurface.leak();
        shadow_left_subsurface.leak();
        shadow_top_subsurface.leak();

        Self {
            compositor: unsafe { compositor.copy_ptr() },
            shm: shm.unwrap(),
            deco_shm,
            straight_shadow_start_bytes: vertical_shadow_start_bytes,
            shm_size,
            buffer_scale: 1,
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
        self.buffer_scale = scale;

        // recreate pix buffer
        self.straight_shadow_start_bytes =
            (Self::SHADOW_SIZE * 2) * scale as usize * (Self::SHADOW_SIZE * 2) * scale as usize * 4;
        self.shm_size = self.straight_shadow_start_bytes
            + (Self::SHADOW_SIZE * 2) * scale as usize * 1 * scale as usize * 4;
        self.deco_shm = Self::create_shm_or_abort(self.shm_size);
        let deco_shm_pool = unsafe { self.shm.as_ref() }
            .create_pool(&self.deco_shm, self.shm_size as _)
            .unwrap();
        self.shadow_corner_buf = deco_shm_pool
            .create_buffer(
                0,
                (Self::SHADOW_SIZE * 2) as i32 * scale,
                (Self::SHADOW_SIZE * 2) as i32 * scale,
                (Self::SHADOW_SIZE * 2) as i32 * scale * 4,
                wl::ShmFormat::ARGB8888,
            )
            .unwrap()
            .unwrap();
        self.shadow_straight_buf = deco_shm_pool
            .create_buffer(
                self.straight_shadow_start_bytes as _,
                (Self::SHADOW_SIZE * 2) as i32 * scale,
                1 * scale,
                (Self::SHADOW_SIZE * 2) as i32 * scale * 4,
                wl::ShmFormat::ARGB8888,
            )
            .unwrap()
            .unwrap();

        unsafe { self.shadow_lt_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_corner_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_lb_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_corner_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_corner_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_corner_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_left_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_straight_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_right_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_straight_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_top_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_straight_buf.as_ref() }), 0, 0)
            .unwrap();
        unsafe { self.shadow_bottom_surface.as_ref() }
            .attach(Some(unsafe { self.shadow_straight_buf.as_ref() }), 0, 0)
            .unwrap();

        unsafe { self.shadow_lt_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_lb_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_left_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_right_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_top_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();
        unsafe { self.shadow_bottom_surface.as_ref() }
            .set_buffer_scale(scale)
            .unwrap();

        unsafe { self.shadow_lt_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_lb_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }.commit().unwrap();
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

    fn render_content(&self, base_alpha_u8: f32) {
        let mapped = self
            .deco_shm
            .mmap_random(
                0..self.shm_size,
                MemoryProtectionFlags::READ | MemoryProtectionFlags::WRITE,
                MemoryMapFlags::SHARED,
            )
            .unwrap();

        let render_size_px = Self::SHADOW_SIZE * 2 * self.buffer_scale as usize;

        let pixels = mapped.ptr_of::<[u8; 4]>();
        for y in 0..render_size_px {
            for x in 0..render_size_px {
                let (x1, y1) = ((render_size_px - x) as f32, (render_size_px - y) as f32);
                let dn = (x1 * x1 + y1 * y1).sqrt() / render_size_px as f32;
                let a = (1.0 - dn).clamp(0.0, 1.0).powf(2.0);

                unsafe {
                    pixels
                        .add(x + y * render_size_px)
                        .write([0, 0, 0, (base_alpha_u8 * a) as _]);
                }
            }
        }

        let pixels = unsafe {
            mapped
                .ptr_of::<[u8; 4]>()
                .byte_add(self.straight_shadow_start_bytes)
        };
        for y in 0..self.buffer_scale as usize {
            for x in 0..render_size_px {
                let a = (x as f32 / (render_size_px as f32)).powf(2.0);

                unsafe {
                    pixels
                        .add(x + y * render_size_px)
                        .write([0, 0, 0, (base_alpha_u8 * a) as _]);
                }
            }
        }
    }

    fn refresh_surface_contents(&self) {
        unsafe { self.shadow_lt_surface.as_ref() }
            .damage(0, 0, i32::MAX, i32::MAX)
            .unwrap();
        unsafe { self.shadow_lb_surface.as_ref() }
            .damage(0, 0, i32::MAX, i32::MAX)
            .unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }
            .damage(0, 0, i32::MAX, i32::MAX)
            .unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }
            .damage(0, 0, i32::MAX, i32::MAX)
            .unwrap();
        unsafe { self.shadow_left_surface.as_ref() }
            .damage(0, 0, i32::MAX, i32::MAX)
            .unwrap();
        unsafe { self.shadow_right_surface.as_ref() }
            .damage(0, 0, i32::MAX, i32::MAX)
            .unwrap();
        unsafe { self.shadow_top_surface.as_ref() }
            .damage(0, 0, i32::MAX, i32::MAX)
            .unwrap();
        unsafe { self.shadow_bottom_surface.as_ref() }
            .damage(0, 0, i32::MAX, i32::MAX)
            .unwrap();

        unsafe { self.shadow_lt_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_lb_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }.commit().unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }.commit().unwrap();
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

    pub fn active(&self) {
        self.render_content(255.0);
        self.refresh_surface_contents();
    }

    pub fn inactive(&self) {
        self.render_content(128.0);
        self.refresh_surface_contents();
    }

    pub fn adjust_for_main_surface_size(&self, width: i32, height: i32) {
        // corner positioning
        unsafe { self.shadow_lb_subsurface.as_ref() }
            .set_position(
                -(Self::SHADOW_SIZE as i32),
                height - (Self::SHADOW_SIZE as i32),
            )
            .unwrap();
        unsafe { self.shadow_rt_subsurface.as_ref() }
            .set_position(
                width - (Self::SHADOW_SIZE as i32),
                -(Self::SHADOW_SIZE as i32),
            )
            .unwrap();
        unsafe { self.shadow_rb_subsurface.as_ref() }
            .set_position(
                width - (Self::SHADOW_SIZE as i32),
                height - (Self::SHADOW_SIZE as i32),
            )
            .unwrap();

        // ltrb stretch
        unsafe { self.shadow_left_viewport.as_ref() }
            .set_destination(
                (Self::SHADOW_SIZE * 2) as i32,
                height - (Self::SHADOW_SIZE * 2) as i32,
            )
            .unwrap();
        unsafe { self.shadow_right_viewport.as_ref() }
            .set_destination(
                (Self::SHADOW_SIZE * 2) as i32,
                height - (Self::SHADOW_SIZE * 2) as i32,
            )
            .unwrap();
        unsafe { self.shadow_top_viewport.as_ref() }
            .set_destination(
                width - (Self::SHADOW_SIZE * 2) as i32,
                (Self::SHADOW_SIZE * 2) as i32,
            )
            .unwrap();
        unsafe { self.shadow_bottom_viewport.as_ref() }
            .set_destination(
                width - (Self::SHADOW_SIZE * 2) as i32,
                (Self::SHADOW_SIZE * 2) as i32,
            )
            .unwrap();

        // rb positioning
        unsafe { self.shadow_right_subsurface.as_ref() }
            .set_position(width - (Self::SHADOW_SIZE as i32), Self::SHADOW_SIZE as i32)
            .unwrap();
        unsafe { self.shadow_bottom_subsurface.as_ref() }
            .set_position(
                Self::SHADOW_SIZE as i32,
                height - (Self::SHADOW_SIZE as i32),
            )
            .unwrap();

        // input region
        let r = unsafe { self.compositor.as_ref() }.create_region().unwrap();
        r.add(
            (Self::SHADOW_SIZE - Self::INPUT_SIZE) as _,
            0,
            Self::INPUT_SIZE as _,
            height,
        )
        .unwrap();
        unsafe { self.shadow_left_surface.as_ref() }
            .set_input_region(Some(&r))
            .unwrap();
        let r = unsafe { self.compositor.as_ref() }.create_region().unwrap();
        r.add(Self::SHADOW_SIZE as _, 0, Self::INPUT_SIZE as _, height)
            .unwrap();
        unsafe { self.shadow_right_surface.as_ref() }
            .set_input_region(Some(&r))
            .unwrap();
        let r = unsafe { self.compositor.as_ref() }.create_region().unwrap();
        r.add(
            0,
            (Self::SHADOW_SIZE - Self::INPUT_SIZE) as _,
            width,
            Self::INPUT_SIZE as _,
        )
        .unwrap();
        unsafe { self.shadow_top_surface.as_ref() }
            .set_input_region(Some(&r))
            .unwrap();
        let r = unsafe { self.compositor.as_ref() }.create_region().unwrap();
        r.add(0, Self::SHADOW_SIZE as _, width, Self::INPUT_SIZE as _)
            .unwrap();
        unsafe { self.shadow_bottom_surface.as_ref() }
            .set_input_region(Some(&r))
            .unwrap();
        let r = unsafe { self.compositor.as_ref() }.create_region().unwrap();
        r.add(
            (Self::SHADOW_SIZE - Self::INPUT_SIZE) as _,
            (Self::SHADOW_SIZE - Self::INPUT_SIZE) as _,
            (Self::INPUT_SIZE + Self::SHADOW_SIZE) as _,
            (Self::INPUT_SIZE + Self::SHADOW_SIZE) as _,
        )
        .unwrap();
        unsafe { self.shadow_lt_surface.as_ref() }
            .set_input_region(Some(&r))
            .unwrap();
        let r = unsafe { self.compositor.as_ref() }.create_region().unwrap();
        r.add(
            (Self::SHADOW_SIZE - Self::INPUT_SIZE) as _,
            0,
            (Self::INPUT_SIZE + Self::SHADOW_SIZE) as _,
            (Self::INPUT_SIZE + Self::SHADOW_SIZE) as _,
        )
        .unwrap();
        unsafe { self.shadow_lb_surface.as_ref() }
            .set_input_region(Some(&r))
            .unwrap();
        let r = unsafe { self.compositor.as_ref() }.create_region().unwrap();
        r.add(
            0,
            (Self::SHADOW_SIZE - Self::INPUT_SIZE) as _,
            (Self::INPUT_SIZE + Self::SHADOW_SIZE) as _,
            (Self::INPUT_SIZE + Self::SHADOW_SIZE) as _,
        )
        .unwrap();
        unsafe { self.shadow_rt_surface.as_ref() }
            .set_input_region(Some(&r))
            .unwrap();
        let r = unsafe { self.compositor.as_ref() }.create_region().unwrap();
        r.add(
            0,
            0,
            (Self::INPUT_SIZE + Self::SHADOW_SIZE) as _,
            (Self::INPUT_SIZE + Self::SHADOW_SIZE) as _,
        )
        .unwrap();
        unsafe { self.shadow_rb_surface.as_ref() }
            .set_input_region(Some(&r))
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

    pub fn resize_edge(&self, enter_surface: &wl::Surface) -> Option<wl::XdgToplevelResizeEdge> {
        if core::ptr::addr_eq(enter_surface, self.shadow_left_surface.as_ptr()) {
            return Some(wl::XdgToplevelResizeEdge::Left);
        }
        if core::ptr::addr_eq(enter_surface, self.shadow_right_surface.as_ptr()) {
            return Some(wl::XdgToplevelResizeEdge::Right);
        }
        if core::ptr::addr_eq(enter_surface, self.shadow_top_surface.as_ptr()) {
            return Some(wl::XdgToplevelResizeEdge::Top);
        }
        if core::ptr::addr_eq(enter_surface, self.shadow_bottom_surface.as_ptr()) {
            return Some(wl::XdgToplevelResizeEdge::Bottom);
        }
        if core::ptr::addr_eq(enter_surface, self.shadow_lt_surface.as_ptr()) {
            return Some(wl::XdgToplevelResizeEdge::TopLeft);
        }
        if core::ptr::addr_eq(enter_surface, self.shadow_lb_surface.as_ptr()) {
            return Some(wl::XdgToplevelResizeEdge::BottomLeft);
        }
        if core::ptr::addr_eq(enter_surface, self.shadow_rt_surface.as_ptr()) {
            return Some(wl::XdgToplevelResizeEdge::TopRight);
        }
        if core::ptr::addr_eq(enter_surface, self.shadow_rb_surface.as_ptr()) {
            return Some(wl::XdgToplevelResizeEdge::BottomRight);
        }

        None
    }

    #[tracing::instrument(name = "ClientDecorationResources::create_shm_or_abort")]
    fn create_shm_or_abort(size: usize) -> TemporalSharedMemory {
        let shm = match try_create_shm_random_suffix(
            "/peridot-sprite-atlas-visualizer-shell_deco_buf_".into(),
        ) {
            Ok(Some(x)) => x,
            Ok(None) => {
                std::process::abort();
            }
            Err(e) => {
                tracing::error!(reason = ?e, "Failed to create shm");
                std::process::abort();
            }
        };
        if let Err(e) = shm.truncate(size as _) {
            tracing::error!(reason = ?e, "Failed to set shm file size");
            std::process::abort();
        }

        shm
    }
}
