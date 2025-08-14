use std::{cell::UnsafeCell, pin::Pin};

use bedrock::{self as br, SurfaceCreateInfo};
use objc_rt::{
    self as objc, AsObject, Object, Receiver,
    appkit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSCursor, NSEvent,
        NSEventModifierFlags, NSEventType, NSMenu, NSTrackingArea, NSTrackingAreaOptions, NSView,
        NSWindow, NSWindowDidChangeBackingPropertiesNotification, NSWindowStyleMask,
    },
    coreanimation::{CADisplayLink, CALayer, CAMetalLayer},
    corefoundation::{CGFloat, CGPoint, CGRect, CGSize},
    foundation::{
        NSArray, NSDate, NSDefaultRunLoopMode, NSNotification, NSNotificationCenter, NSRunLoop,
        NSString,
    },
};

use crate::{
    AppEvent, AppEventBus, base_system::AppBaseSystem, hittest::CursorShape,
    input::PointerInputManager, subsystem::Subsystem,
};

pub struct AppShell<'event_bus, 'subsystem> {
    content_view: objc::Owned<MainNativeView<'event_bus>>,
    window_state_vars: Pin<Box<ShellWindowStateVars<'event_bus>>>,
    frame_timing_observation_thread: core::mem::ManuallyDrop<std::thread::JoinHandle<()>>,
    support_thread_termination_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _marker: core::marker::PhantomData<(&'event_bus AppEventBus, &'subsystem Subsystem)>,
}
impl Drop for AppShell<'_, '_> {
    fn drop(&mut self) {
        self.support_thread_termination_flag
            .store(true, std::sync::atomic::Ordering::Release);
        unsafe { core::mem::ManuallyDrop::take(&mut self.frame_timing_observation_thread) }
            .join()
            .expect("Failed to join frame timing observation thread");
    }
}
impl<'event_bus, 'subsystem> AppShell<'event_bus, 'subsystem> {
    pub fn new(
        events: &'event_bus AppEventBus,
        _base_system: &mut AppBaseSystem<'subsystem>,
    ) -> Self {
        let window_state_vars = Box::pin(ShellWindowStateVars {
            events,
            pointer_manager: UnsafeCell::new(PointerInputManager::new(640.0, 480.0)),
        });

        let nsapp = NSApplication::shared();
        // Note: バンドルするとこれがデフォルトになるので不要になるらしい しばらくバンドルしないので指定しておく
        nsapp.set_activation_policy(NSApplicationActivationPolicy::Regular);

        let main_menu = NSMenu::new_with_title(&NSString::from_utf8_string(c"app"));
        let appmenu = NSMenu::new_with_title(&NSString::from_utf8_string(c"Peridot"));
        appmenu.add_new_item(
            &NSString::from_utf8_string(c"Quit"),
            Some(objc::Selector::get(c"terminate:")),
            &NSString::from_utf8_string(c"q"),
        );
        let app_mi = main_menu.add_new_item(
            &NSString::from_utf8_string(c"Peridot"),
            None,
            &NSString::from_utf8_string(c""),
        );
        main_menu.set_submenu(&appmenu, app_mi);
        appmenu.leak();
        nsapp.set_main_menu(&main_menu);

        let w = NSWindow::new_with_content_rect_style_mask_backing_defer(
            CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: 640.0,
                    height: 480.0,
                },
            },
            NSWindowStyleMask::TITLED
                | NSWindowStyleMask::MINIATURIZABLE
                | NSWindowStyleMask::RESIZABLE
                | NSWindowStyleMask::CLOSABLE,
            NSBackingStoreType::Buffered,
            true,
        );
        w.set_title(&NSString::from_utf8_string(
            c"Peridot SpriteAtlas Visualizer/Editor",
        ));
        w.center();
        w.set_accepts_mouse_moved_events(true);

        let content_view = MainNativeView::new(window_state_vars.as_ref());
        w.set_content_view(&content_view);

        NSNotificationCenter::default().add_observer(
            &content_view,
            objc_rt::Selector::get(c"onWindowBackingPropertiesChanged"),
            Some(unsafe { NSWindowDidChangeBackingPropertiesNotification }),
            Some(&w),
        );

        let support_thread_termination_flag =
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let frame_timing_observation_thread = std::thread::Builder::new()
            .name("FrameTimingObservation".to_string())
            .spawn({
                let support_thread_termination_flag = support_thread_termination_flag.clone();
                let dp = CADisplayLink::new(
                    &content_view,
                    objc::Selector::get(c"doFrame") as *const _ as _,
                );
                move || {
                    let rl = NSRunLoop::current();
                    dp.add_to_run_loop(rl, unsafe { NSDefaultRunLoopMode });

                    while !support_thread_termination_flag
                        .load(std::sync::atomic::Ordering::Acquire)
                    {
                        rl.run_mode_before(
                            unsafe { NSDefaultRunLoopMode },
                            NSDate::distant_future(),
                        );
                    }
                }
            })
            .expect("Failed to spawn frame timing observation thread");

        w.make_key_and_order_front(core::ptr::null_mut());
        content_view.set_content_scale(w.backing_scale_factor());

        nsapp.activate();
        nsapp.finish_launching();

        Self {
            content_view,
            window_state_vars,
            frame_timing_observation_thread: core::mem::ManuallyDrop::new(
                frame_timing_observation_thread,
            ),
            support_thread_termination_flag,
            _marker: core::marker::PhantomData,
        }
    }

    pub const fn needs_window_command_buttons(&self) -> bool {
        // macosは必ずシステム描画のものをつかう
        false
    }

    pub unsafe fn create_vulkan_surface(
        &mut self,
        instance: &impl br::Instance,
    ) -> br::Result<br::vk::VkSurfaceKHR> {
        unsafe {
            br::MetalSurfaceCreateInfo::new(self.content_view.layer_ensure_exists() as _)
                .execute(instance, None)
        }
    }

    pub fn client_size(&self) -> (f32, f32) {
        // TODO: あとで実装する
        (640.0, 480.0)
    }

    pub fn client_size_pixels(&self) -> (u32, u32) {
        // TODO: あとで実装する
        (640 * 2, 480 * 2)
    }

    pub fn ui_scale_factor(&self) -> f32 {
        // TODO: あとで実装する とりあえず2固定
        2.0
    }

    pub fn flush(&mut self) {
        // do nothing for macos
    }

    pub fn request_next_frame(&self) {
        // TODO: これどうしよう
    }

    pub fn capture_pointer(&self) {
        // TODO
    }

    pub fn release_pointer(&self) {
        // TODO
    }

    pub fn server_side_decoration_provided(&self) -> bool {
        // macos always has server-side decoration
        true
    }

    pub fn is_tiled(&self) -> bool {
        // TODO
        false
    }

    pub fn close_safe(&self) {
        tracing::warn!("TODO: close_safe");
    }

    pub fn minimize(&self) {
        tracing::warn!("TODO: minimize");
    }

    pub fn toggle_maximize_restore(&self) {
        tracing::warn!("TODO: toggle_maximize_restore");
    }

    pub fn set_cursor_shape(&self, shape: CursorShape) {
        match shape {
            CursorShape::Default => {
                NSCursor::arrow().set();
            }
            CursorShape::Pointer => {
                NSCursor::pointing_hand().set();
            }
            CursorShape::IBeam => {
                NSCursor::ibeam().set();
            }
            CursorShape::ResizeHorizontal => {
                NSCursor::resize_left_right().set();
            }
        }
    }

    // このへんのwaylandべったりなやつなんとかしたい
    pub fn post_configure(&mut self, _serial: u32) {}

    pub fn pointer_input_manager(&self) -> &UnsafeCell<PointerInputManager> {
        &self.window_state_vars.pointer_manager
    }
}

struct ShellWindowStateVars<'event_bus> {
    events: &'event_bus AppEventBus,
    pointer_manager: UnsafeCell<PointerInputManager>,
}

#[repr(transparent)]
struct MainNativeView<'event_bus>(
    objc::Object,
    core::marker::PhantomData<*const ShellWindowStateVars<'event_bus>>,
);
impl objc_rt::AsObject for MainNativeView<'_> {
    #[inline(always)]
    fn as_object(&self) -> &objc::Object {
        &self.0
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut objc::Object {
        &mut self.0
    }
}
impl objc_rt::NSObject for MainNativeView<'_> {}
impl objc_rt::appkit::NSView for MainNativeView<'_> {}
impl<'event_bus> MainNativeView<'event_bus> {
    fn new(state_vars: Pin<&ShellWindowStateVars<'event_bus>>) -> objc::Owned<Self> {
        let custom_view_class = unsafe {
            objc::Class::allocate_pair(Some(objc::Class::require(c"NSView")), c"AppShellView", 0)
                .expect("Failed to register AppShellView class")
        };
        Self::register_ivars(custom_view_class);
        unsafe {
            custom_view_class.add_method(
                objc::Selector::get(c"initWithFrame:layer:"),
                core::mem::transmute::<
                    extern "C" fn(
                        *mut objc::Object,
                        *const objc::Selector,
                        CGRect,
                        *mut objc::Object,
                    ) -> *mut objc::Object,
                    objc::IMP,
                >(Self::init_with_frame_and_layer),
                c"@@:{CGRect={CGPoint=dd}{CGSize=dd}}@",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"wantsUpdateLayer"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector) -> objc::BOOL,
                    objc::IMP,
                >(Self::wants_update_layer),
                c"B@:",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"setFrameSize:"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, CGSize),
                    objc::IMP,
                >(Self::set_frame_size),
                c"v@:{CGSize=dd}",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"updateTrackingAreas"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector),
                    objc::IMP,
                >(Self::update_tracking_areas),
                c"v@:",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"doFrame"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, *mut CADisplayLink),
                    objc::IMP,
                >(Self::do_frame),
                c"v@:",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"onWindowBackingPropertiesChanged"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, *mut NSNotification),
                    objc::IMP,
                >(Self::on_window_backing_properties_changed),
                c"v@:@",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"mouseDown:"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, *mut NSEvent),
                    objc::IMP,
                >(Self::mouse_down),
                c"v@:@",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"mouseUp:"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, *mut NSEvent),
                    objc::IMP,
                >(Self::mouse_up),
                c"v@:@",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"mouseMoved:"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, *mut NSEvent),
                    objc::IMP,
                >(Self::mouse_moved),
                c"v@:@",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"mouseDragged:"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, *mut NSEvent),
                    objc::IMP,
                >(Self::mouse_dragged),
                c"v@:@",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"mouseEntered:"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, *mut NSEvent),
                    objc::IMP,
                >(Self::mouse_entered),
                c"v@:@",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"mouseExited:"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, *mut NSEvent),
                    objc::IMP,
                >(Self::mouse_exited),
                c"v@:@",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"cursorUpdate:"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, *mut NSEvent),
                    objc::IMP,
                >(Self::mouse_exited),
                c"v@:@",
            );
            custom_view_class.register_pair();
        }

        let layer = CAMetalLayer::new().expect("Failed to create CAMetalLayer");
        let content_view: *mut Object =
            unsafe { custom_view_class.send0r(objc::Selector::get(c"alloc")) };
        let content_view: *mut Object = unsafe {
            (*content_view).send2r(
                objc::Selector::get(c"initWithFrame:layer:"),
                CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGSize {
                        width: 640.0 / 2.0,
                        height: 480.0 / 2.0,
                    },
                },
                layer.as_object(),
            )
        };
        Self::init_ivars(unsafe { &mut *content_view }, state_vars);

        unsafe { objc::Owned::from_ptr_unchecked(content_view.cast::<Self>()) }
    }

    pub fn set_content_scale(&self, scale: CGFloat) {
        unsafe {
            (*self.layer_ensure_exists().cast::<CAMetalLayer>()).set_contents_scale(scale);
        }
    }

    #[inline(always)]
    fn super_nsview(&mut self) -> objc::Super {
        objc::Super {
            receiver: &mut self.0,
            super_class: objc::Class::require(c"NSView"),
        }
    }

    fn register_ivars(cls: &mut objc::Class) {
        cls.add_ivar(
            c"stateVars",
            core::mem::size_of::<*const ShellWindowStateVars>(),
            core::mem::align_of::<*const ShellWindowStateVars>() as _,
            c"^v",
        );
        cls.add_ivar(
            c"doRenderEventCached",
            core::mem::size_of::<*mut NSEvent>(),
            core::mem::align_of::<*mut NSEvent>() as _,
            c"@",
        );
    }

    fn init_ivars(obj: &mut objc::Object, state_vars: Pin<&ShellWindowStateVars<'event_bus>>) {
        unsafe {
            *obj.ivar_ref_mut_by_name::<*const ShellWindowStateVars>(c"stateVars") =
                state_vars.get_ref() as *const _;
            *obj.ivar_ref_mut_by_name::<*mut NSEvent>(c"doRenderEventCached") =
                core::ptr::null_mut();
        }
    }

    #[inline(always)]
    fn state_vars(&self) -> &ShellWindowStateVars {
        unsafe {
            &**self
                .0
                .ivar_ref_by_name::<*const ShellWindowStateVars>(c"stateVars")
        }
    }

    #[inline(always)]
    fn do_render_event_cached_mut(&mut self) -> &mut *mut NSEvent {
        unsafe {
            self.0
                .ivar_ref_mut_by_name::<*mut NSEvent>(c"doRenderEventCached")
        }
    }

    extern "C" fn init_with_frame_and_layer(
        this: *mut objc::Object,
        _cmd: *const objc::Selector,
        frame: CGRect,
        layer: *mut objc::Object,
    ) -> *mut objc::Object {
        let this = unsafe { &mut *this.cast::<Self>() };

        unsafe {
            this.super_nsview()
                .send1o(objc::Selector::get(c"initWithFrame:"), frame.clone());
        }

        unsafe {
            this.as_object()
                .send1(objc::Selector::get(c"setLayer:"), layer);
            this.as_object()
                .send1(objc::Selector::get(c"setWantsLayer:"), 1 as objc::BOOL);
        }

        this.as_object_mut()
    }

    extern "C" fn wants_update_layer(
        _this: *mut objc::Object,
        _cmd: *const objc::Selector,
    ) -> objc::BOOL {
        1
    }

    extern "C" fn set_frame_size(
        this: *mut objc::Object,
        _cmd: *const objc::Selector,
        size: CGSize,
    ) {
        let this = unsafe { &mut *this.cast::<Self>() };

        unsafe {
            this.super_nsview()
                .send1(objc::Selector::get(c"setFrameSize:"), size);
        }

        let state_vars = this.state_vars();
        // TODO: *2.0はあとでちゃんとした値にする
        state_vars.events.push(AppEvent::ToplevelWindowNewSize {
            width_px: (size.width * 2.0) as _,
            height_px: (size.height * 2.0) as _,
        });
        unsafe {
            (*state_vars.pointer_manager.get())
                .set_client_size((size.width * 2.0) as _, (size.height * 2.0) as _);
        }
    }

    extern "C" fn update_tracking_areas(this: *mut objc::Object, _sel: *const objc::Selector) {
        let this = unsafe { &mut *this.cast::<Self>() };

        unsafe {
            this.super_nsview()
                .send0(objc::Selector::get(c"updateTrackingAreas"));
        }

        let tracking_areas = this.tracking_areas();
        for n in 0..tracking_areas.count() {
            this.remove_tracking_area(tracking_areas.object_at_index(n));
        }

        let current_bounds = this.bounds();
        if current_bounds.size.width as i64 == 0 || current_bounds.size.height as i64 == 0 {
            // zero sized
            return;
        }
        this.add_tracking_area(NSTrackingArea::new(
            current_bounds,
            NSTrackingAreaOptions::MOUSE_ENTERED_AND_EXITED
                | NSTrackingAreaOptions::MOUSE_MOVED
                | NSTrackingAreaOptions::ACTIVE_ALWAYS,
            this,
            None,
        ));
    }

    extern "C" fn do_frame(
        this: *mut objc::Object,
        _cmd: *const objc::Selector,
        dp: *mut CADisplayLink,
    ) {
        let this = unsafe { &mut *this.cast::<Self>() };

        let eptr = this.do_render_event_cached_mut();
        if eptr.is_null() {
            *eptr = NSEvent::new_other(
                NSEventType::Periodic,
                CGPoint { x: 0.0, y: 0.0 },
                NSEventModifierFlags::empty(),
                unsafe { (*dp).timestamp() },
                0,
                None,
                0,
                0,
                0,
            )
            .leak();
        }

        NSApplication::shared()
            .post_event(unsafe { objc_rt::Owned::retain_ptr_unchecked(*eptr) }, true);
    }

    extern "C" fn on_window_backing_properties_changed(
        _this: *mut objc::Object,
        _cmd: *const objc::Selector,
        _notification: *mut NSNotification,
    ) {
        println!("TODO: backing properties changed");
    }

    extern "C" fn mouse_down(
        this: *mut objc::Object,
        _cmd: *const objc::Selector,
        e: *mut NSEvent,
    ) {
        let this = unsafe { &mut *this.cast::<Self>() };

        let p = unsafe { (*e).location_in_window() };
        let mut pv = this.convert_point_from_view(p, None);

        // flip y
        pv.y = this.bounds().size.height - pv.y;

        let stv = this.state_vars();
        stv.events.push(AppEvent::MainWindowPointerMove {
            surface_x: pv.x as _,
            surface_y: pv.y as _,
        });
        stv.events.push(AppEvent::MainWindowPointerLeftDown);
    }

    extern "C" fn mouse_up(this: *mut objc::Object, _cmd: *const objc::Selector, e: *mut NSEvent) {
        let this = unsafe { &mut *this.cast::<Self>() };

        let p = unsafe { (*e).location_in_window() };
        let mut pv = this.convert_point_from_view(p, None);

        // flip y
        pv.y = this.bounds().size.height - pv.y;

        let stv = this.state_vars();
        stv.events.push(AppEvent::MainWindowPointerMove {
            surface_x: pv.x as _,
            surface_y: pv.y as _,
        });
        stv.events.push(AppEvent::MainWindowPointerLeftUp);
    }

    extern "C" fn mouse_moved(
        this: *mut objc::Object,
        _cmd: *const objc::Selector,
        e: *mut NSEvent,
    ) {
        let this = unsafe { &mut *this.cast::<Self>() };

        let p = unsafe { (*e).location_in_window() };
        let mut pv = this.convert_point_from_view(p, None);

        // flip y
        pv.y = this.bounds().size.height - pv.y;

        this.state_vars()
            .events
            .push(AppEvent::MainWindowPointerMove {
                surface_x: pv.x as _,
                surface_y: pv.y as _,
            });
    }

    extern "C" fn mouse_dragged(
        this: *mut objc::Object,
        _cmd: *const objc::Selector,
        e: *mut NSEvent,
    ) {
        let this = unsafe { &mut *this.cast::<Self>() };

        let p = unsafe { (*e).location_in_window() };
        let mut pv = this.convert_point_from_view(p, None);

        // flip y
        pv.y = this.bounds().size.height - pv.y;

        this.state_vars()
            .events
            .push(AppEvent::MainWindowPointerMove {
                surface_x: pv.x as _,
                surface_y: pv.y as _,
            });
    }

    extern "C" fn mouse_entered(
        this: *mut objc::Object,
        _cmd: *const objc::Selector,
        e: *mut NSEvent,
    ) {
        println!("mouse entered");

        let this = unsafe { &mut *this.cast::<Self>() };

        let p = unsafe { (*e).location_in_window() };
        let mut pv = this.convert_point_from_view(p, None);

        // flip y
        pv.y = this.bounds().size.height - pv.y;

        this.state_vars()
            .events
            .push(AppEvent::MainWindowPointerMove {
                surface_x: pv.x as _,
                surface_y: pv.y as _,
            });
    }

    extern "C" fn mouse_exited(
        this: *mut objc::Object,
        _cmd: *const objc::Selector,
        e: *mut NSEvent,
    ) {
        println!("mouse exited");

        let this = unsafe { &mut *this.cast::<Self>() };

        let p = unsafe { (*e).location_in_window() };
        let mut pv = this.convert_point_from_view(p, None);

        // flip y
        pv.y = this.bounds().size.height - pv.y;

        this.state_vars()
            .events
            .push(AppEvent::MainWindowPointerMove {
                surface_x: pv.x as _,
                surface_y: pv.y as _,
            });
    }
}
