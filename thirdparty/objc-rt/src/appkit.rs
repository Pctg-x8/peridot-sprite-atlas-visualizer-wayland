use bitflags::bitflags;

use core::ffi::*;

use crate::{
    AsObject, BOOL, Class, NSObject, Object, Owned, Selector,
    corefoundation::CGRect,
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

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct NSEventMask : core::ffi::c_ulonglong {
        const ANY = core::ffi::c_ulonglong::MAX;
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
    pub fn window(&self) -> Option<&NSWindow> {
        let p = unsafe { self.0.send0v::<*mut NSWindow>(Selector::get(c"window")) };
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
