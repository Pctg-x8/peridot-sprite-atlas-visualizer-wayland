use std::cell::UnsafeCell;

use bedrock::{self as br, SurfaceCreateInfo};
use objc_rt::{
    self as objc, AsObject,
    appkit::{NSBackingStoreType, NSUInteger, NSWindowStyleMask},
    corefoundation::{CGPoint, CGRect, CGSize},
    foundation::NSString,
};

use crate::{
    AppEventBus, base_system::AppBaseSystem, hittest::CursorShape, input::PointerInputManager,
    subsystem::Subsystem,
};

#[repr(transparent)]
struct NSApplication(objc::Object);
impl NSApplication {
    pub fn shared<'a>() -> &'a Self {
        let cls = objc::Class::get(c"NSApplication").expect("no NSApplication");
        unsafe { &*(cls.send0o(objc::Selector::get(c"sharedApplication")) as *mut Self) }
    }

    pub fn run(&self) {
        unsafe {
            self.0.send0(objc::Selector::get(c"run"));
        }
    }
}

#[repr(transparent)]
pub struct NSWindow(objc::Object);
impl objc::AsObject for NSWindow {
    #[inline(always)]
    fn as_object(&self) -> &objc::Object {
        &self.0
    }
}
impl objc::NSObject for NSWindow {}
impl NSWindow {
    pub fn new_with_content_rect_style_mask_backing_defer(
        content_rect: CGRect,
        style_mask: NSWindowStyleMask,
        backing: NSBackingStoreType,
        defer: bool,
    ) -> objc::Owned<Self> {
        let cls = objc::Class::get(c"NSWindow").expect("no NSWindow class");
        let this = unsafe { cls.send0o(objc::Selector::get(c"alloc")) };
        unsafe {
            (*this).send4o(
                objc::Selector::get(c"initWithContentRect:styleMask:backing:defer:"),
                content_rect,
                style_mask.bits(),
                backing as NSUInteger,
                if defer { 1 } else { 0 } as core::ffi::c_char,
            )
        };

        unsafe { objc::Owned::from_ptr_unchecked(this as *mut Self) }
    }

    #[inline(always)]
    pub fn make_key_and_order_front(&self, sender: *mut objc::Object) {
        unsafe {
            self.0
                .send1(objc::Selector::get(c"makeKeyAndOrderFront:"), sender)
        }
    }

    #[inline(always)]
    pub fn center(&self) {
        unsafe { self.0.send0(objc::Selector::get(c"center")) }
    }

    #[inline(always)]
    pub fn set_title(&self, title: &NSString) {
        unsafe {
            self.0.send1(
                objc::Selector::get(c"setTitle:"),
                title.as_object() as *const _,
            )
        }
    }

    #[inline(always)]
    pub fn set_content_view(&self, content_view: *mut objc::Object) {
        unsafe {
            self.0
                .send1(objc::Selector::get(c"setContentView:"), content_view)
        }
    }
}

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
                objc::Class::get(c"CAMetalLayer")
                    .expect("no CAMetalLayer class")
                    .send0o(objc::Selector::get(c"layer")) as *mut Self,
            )
        }
    }
}

pub struct AppShell<'event_bus, 'subsystem> {
    layer: objc::Owned<CAMetalLayer>,
    pointer_manager: UnsafeCell<PointerInputManager>,
    _marker: core::marker::PhantomData<(&'event_bus AppEventBus, &'subsystem Subsystem)>,
}
impl<'event_bus, 'subsystem> AppShell<'event_bus, 'subsystem> {
    pub fn new(
        events: &'event_bus AppEventBus,
        _base_system: &mut AppBaseSystem<'subsystem>,
    ) -> Self {
        let nsapp = NSApplication::shared();
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
                | NSWindowStyleMask::CLOSABLE
                | NSWindowStyleMask::UNIFIED_TITLE_AND_TOOLBAR,
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
            extern "C" fn init_with_frame_and_layer(
                this: *mut objc::Object,
                _cmd: *const objc::Selector,
                frame: CGRect,
                layer: *mut objc::Object,
            ) -> *mut objc::Object {
                objc::Super {
                    receiver: this,
                    super_class: objc::Class::get(c"NSView").expect("no NSView class") as *mut _,
                }
                .send1o(objc::Selector::get(c"initWithFrame:"), frame.clone());

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
            custom_view_class.register_pair();
        }

        let content_view = unsafe { custom_view_class.send0o(objc::Selector::get(c"alloc")) };
        let content_view = unsafe {
            (*content_view).send2o(
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
        w.set_content_view(content_view);

        w.make_key_and_order_front(core::ptr::null_mut());

        let pointer_input_manager = PointerInputManager::new();

        Self {
            layer,
            pointer_manager: UnsafeCell::new(pointer_input_manager),
            _marker: core::marker::PhantomData,
        }
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

    pub fn process_pending_events(&mut self) {
        // TODO: ここもあとで macosでwaylandと似たようなやり方できたっけな......
        NSApplication::shared().run();
    }

    pub fn request_next_frame(&mut self) {
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

    // このへんのwaylandべったりなやつなんとかしたい
    pub fn post_configure(&mut self, _serial: u32) {}
    pub fn set_cursor_shape(&mut self, _enter_serial: u32, _shape: CursorShape) {}

    pub fn pointer_input_manager(&self) -> &UnsafeCell<PointerInputManager> {
        &self.pointer_manager
    }
}

#[cfg(target_os = "macos")]
#[link(name = "AppKit", kind = "framework")]
unsafe extern "system" {}
