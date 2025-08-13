use bitflags::bitflags;

use core::ffi::*;

use crate::{
    AsObject, BOOL, Class, NSObject, Object, Owned, Selector,
    corefoundation::{CGPoint, CGRect},
    foundation::{NSDate, NSString},
};

#[cfg(target_pointer_width = "64")]
pub type NSInteger = c_long;
#[cfg(target_pointer_width = "64")]
pub type NSUInteger = c_ulong;

bitflags! {
    pub struct NSWindowStyleMask: NSUInteger {
        const TITLED = 1 << 0;
        const CLOSABLE = 1 << 1;
        const MINIATURIZABLE = 1 << 2;
        const RESIZABLE = 1 << 3;
        const UNIFIED_TITLE_AND_TOOLBAR = 1 << 12;
    }
}

#[cfg_attr(target_pointer_width = "64", repr(u64))]
pub enum NSBackingStoreType {
    Buffered = 2,
}

#[cfg_attr(target_pointer_width = "64", repr(u64))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NSEventType {
    ApplicationDefined = 15,
    Periodic = 16,
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct NSEventMask : core::ffi::c_ulonglong {
        const ANY = core::ffi::c_ulonglong::MAX;
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct NSEventModifierFlags : NSUInteger {

    }
}

#[repr(transparent)]
pub struct NSApplication(Object);
impl AsObject for NSApplication {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSApplication {
    pub fn shared<'a>() -> &'a Self {
        let cls = Class::get(c"NSApplication").expect("no NSApplication");
        unsafe { &*(cls.send0o(Selector::get(c"sharedApplication")) as *mut Self) }
    }

    pub fn set_activation_policy(&self, policy: NSApplicationActivationPolicy) -> bool {
        unsafe {
            self.0
                .send1r::<_, BOOL>(Selector::get(c"setActivationPolicy:"), policy as NSUInteger)
                != 0
        }
    }

    pub fn set_main_menu(&self, menu: &NSMenu) {
        unsafe {
            self.0
                .send1(Selector::get(c"setMainMenu:"), menu.as_object() as *const _);
        }
    }

    pub fn finish_launching(&self) {
        unsafe {
            self.0.send0(Selector::get(c"finishLaunching"));
        }
    }

    pub fn run(&self) {
        unsafe {
            self.0.send0(Selector::get(c"run"));
        }
    }

    pub fn next_event_matching_mask_until_date_in_mode_dequeue(
        &self,
        mask: NSEventMask,
        until: Option<&NSDate>,
        mode: &NSString,
        dequeue: bool,
    ) -> Option<Owned<NSEvent>> {
        unsafe {
            Owned::from_ptr(self.0.send4v(
                Selector::get(c"nextEventMatchingMask:untilDate:inMode:dequeue:"),
                mask.bits(),
                until.map_or_else(core::ptr::null, |x| x.as_object() as *const _),
                mode,
                dequeue as BOOL,
            ))
        }
    }

    pub fn send_event(&self, event: &NSEvent) {
        unsafe {
            self.0
                .send1(Selector::get(c"sendEvent:"), event.as_object() as *const _);
        }
    }

    #[inline(always)]
    pub fn post_event(&self, event: &NSEvent, at_start: bool) {
        unsafe {
            self.0.send2(
                Selector::get(c"postEvent:atStart:"),
                event.as_object() as *const _,
                if at_start { 1 } else { 0 } as BOOL,
            );
        }
    }

    #[inline(always)]
    pub fn update_windows(&self) {
        unsafe {
            self.0.send0(Selector::get(c"updateWindows"));
        }
    }
}

#[cfg_attr(target_pointer_width = "64", repr(i64))]
pub enum NSApplicationActivationPolicy {
    Regular,
    Accessory,
    Prohibited,
}

#[repr(transparent)]
pub struct NSMenu(Object);
impl AsObject for NSMenu {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSMenu {}
impl NSMenu {
    pub fn new_with_title(title: &NSString) -> Owned<Self> {
        let inst = unsafe {
            Owned::from_ptr_unchecked(
                Class::require(c"NSMenu")
                    .send0r::<*mut Object>(Selector::get(c"alloc"))
                    .cast::<Self>(),
            )
        };
        unsafe {
            inst.as_object().send1r::<_, *mut Object>(
                Selector::get(c"initWithTitle:"),
                title.as_object() as *const _,
            );
        }

        inst
    }

    pub fn add_new_item<'a>(
        &'a self,
        title: &NSString,
        action: Option<&Selector>,
        key_equivalent: &NSString,
    ) -> &'a NSMenuItem {
        unsafe {
            &*self
                .0
                .send3r::<_, _, _, *mut Object>(
                    Selector::get(c"addItemWithTitle:action:keyEquivalent:"),
                    title.as_object() as *const _,
                    action.map_or_else(core::ptr::null, |x| x as *const _),
                    key_equivalent.as_object() as *const _,
                )
                .cast::<NSMenuItem>()
        }
    }

    pub fn set_submenu(&self, submenu: &NSMenu, for_item: &NSMenuItem) {
        unsafe {
            self.0.send2(
                Selector::get(c"setSubmenu:forItem:"),
                submenu.as_object() as *const _,
                for_item.as_object() as *const _,
            );
        }
    }
}

#[repr(transparent)]
pub struct NSMenuItem(Object);
impl AsObject for NSMenuItem {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSMenuItem {}
impl NSMenuItem {
    #[inline(always)]
    pub fn submenu_mut(&mut self) -> &mut NSMenu {
        unsafe {
            &mut *self
                .0
                .send0r::<*mut Object>(Selector::get(c"submenu"))
                .cast::<NSMenu>()
        }
    }
}

#[repr(transparent)]
pub struct NSEvent(Object);
impl AsObject for NSEvent {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSEvent {}
impl NSEvent {
    #[inline(always)]
    pub fn new_other(
        r#type: NSEventType,
        location: CGPoint,
        modifier_flags: NSEventModifierFlags,
        timestamp: f64,
        window_number: NSInteger,
        context: Option<*mut Object>, // NSGraphicsContext
        subtype: core::ffi::c_short,
        data1: NSInteger,
        data2: NSInteger,
    ) -> Owned<Self> {
        unsafe {
            Owned::from_ptr_unchecked(
                Class::require(c"NSEvent").send9r(
                    Selector::get(c"otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:"),
                    r#type as NSUInteger,
                    location,
                    modifier_flags.bits(),
                    timestamp,
                    window_number,
                    context.unwrap_or(core::ptr::null_mut()),
                    subtype,
                    data1,
                    data2,
                ),
            )
        }
    }

    #[inline(always)]
    pub fn r#type(&self) -> NSEventType {
        unsafe { self.0.send0r(Selector::get(c"type")) }
    }

    #[inline(always)]
    pub fn data1(&self) -> NSInteger {
        unsafe { self.0.send0r(Selector::get(c"data1")) }
    }

    #[inline(always)]
    pub fn data2(&self) -> NSInteger {
        unsafe { self.0.send0r(Selector::get(c"data2")) }
    }

    #[inline(always)]
    pub fn window(&self) -> Option<&NSWindow> {
        let p = unsafe { self.0.send0r::<*mut NSWindow>(Selector::get(c"window")) };
        if p.is_null() {
            None
        } else {
            Some(unsafe { &*p })
        }
    }
}

#[repr(transparent)]
pub struct NSWindow(Object);
impl AsObject for NSWindow {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSWindow {}
impl NSWindow {
    pub fn new_with_content_rect_style_mask_backing_defer(
        content_rect: CGRect,
        style_mask: NSWindowStyleMask,
        backing: NSBackingStoreType,
        defer: bool,
    ) -> Owned<Self> {
        let cls = Class::get(c"NSWindow").expect("no NSWindow class");
        let this = unsafe { cls.send0o(Selector::get(c"alloc")) };
        unsafe {
            (*this).send4o(
                Selector::get(c"initWithContentRect:styleMask:backing:defer:"),
                content_rect,
                style_mask.bits(),
                backing as NSUInteger,
                if defer { 1 } else { 0 } as core::ffi::c_char,
            )
        };

        unsafe { Owned::from_ptr_unchecked(this as *mut Self) }
    }

    #[inline(always)]
    pub fn make_key_and_order_front(&self, sender: *mut Object) {
        unsafe {
            self.0
                .send1(Selector::get(c"makeKeyAndOrderFront:"), sender)
        }
    }

    #[inline(always)]
    pub fn center(&self) {
        unsafe { self.0.send0(Selector::get(c"center")) }
    }

    #[inline(always)]
    pub fn set_title(&self, title: &NSString) {
        unsafe {
            self.0
                .send1(Selector::get(c"setTitle:"), title.as_object() as *const _)
        }
    }

    #[inline(always)]
    pub fn set_content_view(&self, content_view: *mut Object) {
        unsafe {
            self.0
                .send1(Selector::get(c"setContentView:"), content_view)
        }
    }
}
