use std::{cell::UnsafeCell, pin::Pin};

use bedrock::{self as br, SurfaceCreateInfo};
use objc_rt::{
    self as objc, AsObject, NSObject, Owned,
    appkit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask,
        NSEventModifierFlags, NSEventType, NSMenu, NSWindow, NSWindowStyleMask,
    },
    coreanimation::CADisplayLink,
    corefoundation::{CGPoint, CGRect, CGSize},
    foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop, NSString},
};

use crate::{
    AppEvent, AppEventBus, base_system::AppBaseSystem, hittest::CursorShape,
    input::PointerInputManager, subsystem::Subsystem,
};

#[repr(transparent)]
pub struct CAMetalLayer(objc::Object);
impl objc::AsObject for CAMetalLayer {
    #[inline(always)]
    fn as_object(&self) -> &objc::Object {
        &self.0
    }
}
impl objc::NSObject for CAMetalLayer {}
impl CAMetalLayer {
    pub fn new() -> Option<objc::Owned<Self>> {
        unsafe {
            objc::Owned::from_ptr(
                objc::Class::require(c"CAMetalLayer").send0o(objc::Selector::get(c"layer"))
                    as *mut Self,
            )
        }
    }
}

struct ShellWindowStateVars<'event_bus> {
    events: &'event_bus AppEventBus,
}

pub struct AppShell<'event_bus, 'subsystem> {
    layer: objc::Owned<CAMetalLayer>,
    pointer_manager: UnsafeCell<PointerInputManager>,
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
        let pointer_input_manager = PointerInputManager::new(640.0, 480.0);

        let window_state_vars = Box::pin(ShellWindowStateVars { events });

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

        let layer = CAMetalLayer::new().expect("Failed to create CAMetalLayer");

        let custom_view_class = unsafe {
            objc::Class::allocate_pair(
                Some(objc::Class::get(c"NSView").expect("no NSView class")),
                c"AppShellView",
                0,
            )
            .expect("Failed to register AppShellView class")
        };
        unsafe {
            custom_view_class.add_ivar(
                c"stateVars",
                core::mem::size_of::<*const ShellWindowStateVars>(),
                core::mem::align_of::<*const ShellWindowStateVars>() as _,
                c"^v",
            );
            custom_view_class.add_ivar(
                c"doRenderEventCached",
                core::mem::size_of::<*mut NSEvent>(),
                core::mem::align_of::<*mut NSEvent>() as _,
                c"@",
            );

            extern "C" fn init_with_frame_and_layer(
                this: *mut objc::Object,
                _cmd: *const objc::Selector,
                frame: CGRect,
                layer: *mut objc::Object,
            ) -> *mut objc::Object {
                unsafe {
                    objc::Super {
                        receiver: this,
                        super_class: objc::Class::get(c"NSView").expect("no NSView class")
                            as *mut _,
                    }
                    .send1o(objc::Selector::get(c"initWithFrame:"), frame.clone());
                }

                println!("init AppShellView with frame {frame:?}");

                unsafe {
                    (*this).send1(objc::Selector::get(c"setLayer:"), layer);
                    (*this).send1(objc::Selector::get(c"setWantsLayer:"), 1 as objc::BOOL);
                }

                this
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
                unsafe {
                    objc::Super {
                        receiver: this,
                        super_class: objc::Class::get(c"NSView").expect("no NSView class")
                            as *mut _,
                    }
                    .send1(objc::Selector::get(c"setFrameSize:"), size);
                }

                tracing::info!(?size, "resize view");
                let state_vars = unsafe {
                    &**(*this).get_ivar_by_name::<*const ShellWindowStateVars>(c"stateVars")
                };
                state_vars.events.push(AppEvent::ToplevelWindowNewSize {
                    width_px: size.width as _,
                    height_px: size.height as _,
                });
            }
            extern "C" fn do_frame(
                this: *mut objc::Object,
                _cmd: *const objc::Selector,
                dp: *mut CADisplayLink,
            ) {
                // let state_vars = unsafe {
                //     &**(*this).get_ivar_by_name::<*const ShellWindowStateVars>(c"stateVars")
                // };
                // state_vars.events.push(AppEvent::ToplevelWindowFrameTiming);
                // let cached_ptr =
                //     unsafe { *(*this).get_ivar_by_name::<*mut NSEvent>(c"doRenderEventCached") };
                // if cached_ptr.is_null() {
                let e = NSEvent::new_other(
                    NSEventType::Periodic,
                    CGPoint { x: 0.0, y: 0.0 },
                    NSEventModifierFlags::empty(),
                    unsafe { (*dp).timestamp() },
                    0,
                    None,
                    0,
                    1000,
                    34645351683,
                );
                // unsafe {
                //     (*this).set_ivar_by_name(c"doRenderEventCached", e.clone().leak());
                // }
                NSApplication::shared().post_event(&e, true);
                e.leak();
                // } else {
                //     unsafe {
                //         (*cached_ptr).retain();
                //     }
                //     NSApplication::shared().post_event(unsafe { &*cached_ptr }, true);
                // }
            }

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
                >(init_with_frame_and_layer),
                c"@@:{CGRect={CGPoint=dd}{CGSize=dd}}@",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"wantsUpdateLayer"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector) -> objc::BOOL,
                    objc::IMP,
                >(wants_update_layer),
                c"B@:",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"setFrameSize:"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, CGSize),
                    objc::IMP,
                >(set_frame_size),
                c"v@:{CGSize=dd}",
            );
            custom_view_class.add_method(
                objc::Selector::get(c"doFrame"),
                core::mem::transmute::<
                    extern "C" fn(*mut objc::Object, *const objc::Selector, *mut CADisplayLink),
                    objc::IMP,
                >(do_frame),
                c"v@:",
            );
            custom_view_class.register_pair();
        }

        let content_view = unsafe { custom_view_class.send0o(objc::Selector::get(c"alloc")) };
        let content_view: *mut objc::Object = unsafe {
            (*content_view).send2v(
                objc::Selector::get(c"initWithFrame:layer:"),
                CGRect {
                    origin: CGPoint { x: 0.0, y: 0.0 },
                    size: CGSize {
                        width: 640.0,
                        height: 480.0,
                    },
                },
                layer.as_object(),
            )
        };
        unsafe {
            (*content_view)
                .set_ivar_by_name(c"stateVars", &*window_state_vars.as_ref() as *const _);
            (*content_view)
                .set_ivar_by_name::<*mut NSEvent>(c"doRenderEventCached", core::ptr::null_mut());
        }
        w.set_content_view(content_view);

        let support_thread_termination_flag =
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let frame_timing_observation_thread = std::thread::Builder::new()
            .name("FrameTimingObservation".to_string())
            .spawn({
                let support_thread_termination_flag = support_thread_termination_flag.clone();
                let dp = CADisplayLink::new(
                    content_view,
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
        nsapp.finish_launching();

        Self {
            layer,
            pointer_manager: UnsafeCell::new(pointer_input_manager),
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
            br::MetalSurfaceCreateInfo::new(self.layer.as_object() as *const _ as _)
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
        tracing::warn!("TODO: set cursor shape");
    }

    // このへんのwaylandべったりなやつなんとかしたい
    pub fn post_configure(&mut self, _serial: u32) {}

    pub fn pointer_input_manager(&self) -> &UnsafeCell<PointerInputManager> {
        &self.pointer_manager
    }
}

#[cfg(target_os = "macos")]
#[link(name = "AppKit", kind = "framework")]
unsafe extern "system" {}
