use bitflags::bitflags;

use core::ffi::*;

use crate::{
    _NSConcreteStackBlock, AsObject, BLOCK_HAS_COPY_DISPOSE, BOOL, BlockDescriptorBase,
    BlockObjectBase, Class, ClosureBlockDescriptor, ClosureBlockObject, NSObject, Object, Owned,
    Receiver, Selector, closure1_block_invoke,
    corefoundation::{CGFloat, CGPoint, CGRect},
    foundation::{NSArray, NSArrayObject, NSDate, NSNotificationName, NSString},
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
    LeftMouseDown = 1,
    LeftMouseUp = 2,
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

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
    }
}
impl NSApplication {
    #[inline(always)]
    pub fn shared<'a>() -> &'a Self {
        unsafe {
            &*Class::require(c"NSApplication")
                .send0r::<*mut Object>(Selector::get_cached(c"sharedApplication"))
                .cast::<Self>()
        }
    }

    #[inline(always)]
    pub fn set_activation_policy(&self, policy: NSApplicationActivationPolicy) -> bool {
        unsafe {
            self.0.send1r::<_, BOOL>(
                Selector::get_cached(c"setActivationPolicy:"),
                policy as NSUInteger,
            ) != 0
        }
    }

    #[inline(always)]
    pub fn activate(&self) {
        unsafe {
            self.0.send0(Selector::get_cached(c"activate"));
        }
    }

    #[inline(always)]
    pub fn set_main_menu(&self, menu: &NSMenu) {
        unsafe {
            self.0.send1(
                Selector::get_cached(c"setMainMenu:"),
                menu.as_object() as *const _,
            );
        }
    }

    #[inline(always)]
    pub fn finish_launching(&self) {
        unsafe {
            self.0.send0(Selector::get_cached(c"finishLaunching"));
        }
    }

    #[inline(always)]
    pub fn run(&self) {
        unsafe {
            self.0.send0(Selector::get_cached(c"run"));
        }
    }

    #[inline(always)]
    pub fn next_event_matching_mask_until_date_in_mode_dequeue(
        &self,
        mask: NSEventMask,
        until: Option<&NSDate>,
        mode: &NSString,
        dequeue: bool,
    ) -> Option<Owned<NSEvent>> {
        unsafe {
            Owned::from_ptr(self.0.send4r(
                Selector::get_cached(c"nextEventMatchingMask:untilDate:inMode:dequeue:"),
                mask.bits(),
                until.map_or_else(core::ptr::null, |x| x.as_object() as *const _),
                mode,
                dequeue as BOOL,
            ))
        }
    }

    #[inline(always)]
    pub fn send_event(&self, event: &NSEvent) {
        unsafe {
            self.0.send1(
                Selector::get_cached(c"sendEvent:"),
                event.as_object() as *const _,
            );
        }
    }

    #[inline(always)]
    pub fn post_event(&self, event: Owned<NSEvent>, at_start: bool) {
        unsafe {
            self.0.send2(
                Selector::get_cached(c"postEvent:atStart:"),
                event.as_object() as *const _,
                if at_start { 1 } else { 0 } as BOOL,
            );
        }

        // moved into objc
        core::mem::forget(event);
    }

    #[inline(always)]
    pub fn update_windows(&self) {
        unsafe {
            self.0.send0(Selector::get_cached(c"updateWindows"));
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

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
    }
}
impl NSObject for NSMenu {}
impl NSMenu {
    pub fn new_with_title(title: &NSString) -> Owned<Self> {
        let inst = unsafe {
            Owned::from_ptr_unchecked(
                Class::require(c"NSMenu")
                    .send0r::<*mut Object>(Selector::get_cached(c"alloc"))
                    .cast::<Self>(),
            )
        };
        unsafe {
            inst.as_object().send1r::<_, *mut Object>(
                Selector::get_cached(c"initWithTitle:"),
                title.as_object() as *const _,
            );
        }

        inst
    }

    #[inline(always)]
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
                    Selector::get_cached(c"addItemWithTitle:action:keyEquivalent:"),
                    title.as_object() as *const _,
                    action.map_or_else(core::ptr::null, |x| x as *const _),
                    key_equivalent.as_object() as *const _,
                )
                .cast::<NSMenuItem>()
        }
    }

    #[inline(always)]
    pub fn set_submenu(&self, submenu: &NSMenu, for_item: &NSMenuItem) {
        unsafe {
            self.0.send2(
                Selector::get_cached(c"setSubmenu:forItem:"),
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

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
    }
}
impl NSObject for NSMenuItem {}
impl NSMenuItem {
    #[inline(always)]
    pub fn submenu_mut(&mut self) -> &mut NSMenu {
        unsafe {
            &mut *self
                .0
                .send0r::<*mut Object>(Selector::get_cached(c"submenu"))
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

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
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
                    Selector::get_cached(c"otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:"),
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
        unsafe { self.0.send0r(Selector::get_cached(c"type")) }
    }

    #[inline(always)]
    pub fn data1(&self) -> NSInteger {
        unsafe { self.0.send0r(Selector::get_cached(c"data1")) }
    }

    #[inline(always)]
    pub fn data2(&self) -> NSInteger {
        unsafe { self.0.send0r(Selector::get_cached(c"data2")) }
    }

    #[inline(always)]
    pub fn window(&self) -> Option<&NSWindow> {
        let p = unsafe {
            self.0
                .send0r::<*mut NSWindow>(Selector::get_cached(c"window"))
        };
        if p.is_null() {
            None
        } else {
            Some(unsafe { &*p })
        }
    }

    #[inline(always)]
    pub fn location_in_window(&self) -> CGPoint {
        unsafe { self.0.send0r(Selector::get_cached(c"locationInWindow")) }
    }

    #[inline(always)]
    pub fn button_number(&self) -> NSInteger {
        unsafe { self.0.send0r(Selector::get_cached(c"buttonNumber")) }
    }
}

#[repr(transparent)]
pub struct NSWindow(Object);
impl AsObject for NSWindow {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
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
        let this = unsafe {
            Owned::from_ptr_unchecked(
                Class::require(c"NSWindow")
                    .send0r::<*mut Object>(Selector::get_cached(c"alloc"))
                    .cast::<Self>(),
            )
        };
        unsafe {
            this.as_object().send4r::<_, _, _, _, *mut Object>(
                Selector::get_cached(c"initWithContentRect:styleMask:backing:defer:"),
                content_rect,
                style_mask.bits(),
                backing as NSUInteger,
                if defer { 1 } else { 0 } as core::ffi::c_char,
            )
        };

        this
    }

    #[inline(always)]
    pub fn make_key_and_order_front(&self, sender: *mut Object) {
        unsafe {
            self.0
                .send1(Selector::get_cached(c"makeKeyAndOrderFront:"), sender)
        }
    }

    #[inline(always)]
    pub fn center(&self) {
        unsafe { self.0.send0(Selector::get_cached(c"center")) }
    }

    #[inline(always)]
    pub fn set_title(&self, title: &NSString) {
        unsafe {
            self.0.send1(
                Selector::get_cached(c"setTitle:"),
                title.as_object() as *const _,
            )
        }
    }

    #[inline(always)]
    pub fn set_content_view(&self, content_view: &(impl NSView + ?Sized)) {
        unsafe {
            self.0.send1(
                Selector::get_cached(c"setContentView:"),
                content_view.as_object() as *const _,
            )
        }
    }

    #[inline(always)]
    pub fn backing_scale_factor(&self) -> CGFloat {
        unsafe { self.0.send0r(Selector::get_cached(c"backingScaleFactor")) }
    }

    #[inline(always)]
    pub fn set_accepts_mouse_moved_events(&self, accepts: bool) {
        unsafe {
            self.0.send1(
                Selector::get_cached(c"setAcceptsMouseMovedEvents:"),
                if accepts { 1 } else { 0 } as BOOL,
            );
        }
    }
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {
    pub static NSWindowDidChangeBackingPropertiesNotification: NSNotificationName;
}

#[repr(transparent)]
pub struct NSTrackingArea(Object);
impl AsObject for NSTrackingArea {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
    }
}
impl NSObject for NSTrackingArea {}
impl NSTrackingArea {
    pub fn new<Owner: AsObject>(
        rect: CGRect,
        options: NSTrackingAreaOptions,
        owner: &Owner,
        userinfo: Option<*mut Object>,
    ) -> Owned<Self> {
        let x = unsafe {
            Owned::from_ptr_unchecked(
                Class::require(c"NSTrackingArea")
                    .send0r::<*mut Object>(Selector::get_cached(c"alloc"))
                    .cast::<Self>(),
            )
        };
        unsafe {
            x.as_object().send4r::<_, _, _, _, *mut Object>(
                Selector::get_cached(c"initWithRect:options:owner:userInfo:"),
                rect,
                options.bits(),
                owner.as_object() as *const _ as *mut Object,
                userinfo.unwrap_or(core::ptr::null_mut()),
            );
        }

        x
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct NSTrackingAreaOptions : NSUInteger {
        const MOUSE_ENTERED_AND_EXITED = 0x01;
        const MOUSE_MOVED = 0x02;
        const ACTIVE_ALWAYS = 0x80;
        const ENABLED_DURING_MOUSE_DRAG = 0x400;
    }
}

#[repr(transparent)]
pub struct NSViewObject(Object);
impl AsObject for NSViewObject {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
    }
}
impl NSObject for NSViewObject {}
impl NSView for NSViewObject {}

pub trait NSView: NSObject {
    #[inline(always)]
    fn tracking_areas<'a>(&'a self) -> &'a impl NSArray<Item = NSTrackingArea> {
        unsafe {
            &*self
                .as_object()
                .send0r::<*mut Object>(Selector::get_cached(c"trackingAreas"))
                .cast::<NSArrayObject<NSTrackingArea>>()
        }
    }

    #[inline(always)]
    fn add_tracking_area(&self, area: Owned<NSTrackingArea>) {
        unsafe {
            self.as_object().send1(
                Selector::get_cached(c"addTrackingArea:"),
                area.as_object() as *const _,
            );
        }
        // object moved into objc
        core::mem::forget(area);
    }

    #[inline(always)]
    fn remove_tracking_area(&self, area: &NSTrackingArea) {
        unsafe {
            self.as_object().send1(
                Selector::get_cached(c"removeTrackingArea:"),
                area.as_object() as *const _,
            );
        }
    }

    #[inline(always)]
    fn bounds(&self) -> CGRect {
        unsafe { self.as_object().send0r(Selector::get_cached(c"bounds")) }
    }

    #[inline(always)]
    fn convert_point_from_view(&self, point: CGPoint, from_view: Option<&NSViewObject>) -> CGPoint {
        unsafe {
            self.as_object().send2r(
                Selector::get_cached(c"convertPoint:fromView:"),
                point,
                from_view.map_or_else(core::ptr::null, |x| x.as_object() as *const _),
            )
        }
    }

    #[inline(always)]
    fn layer(&self) -> Option<*mut Object> {
        let p = unsafe {
            self.as_object()
                .send0r::<*mut Object>(Selector::get_cached(c"layer"))
        };
        if p.is_null() { None } else { Some(p) }
    }

    #[inline(always)]
    unsafe fn layer_ensure_exists(&self) -> *mut Object {
        unsafe { self.as_object().send0r(Selector::get_cached(c"layer")) }
    }
}
impl<T: NSView> NSView for Owned<T> {}

#[repr(transparent)]
pub struct NSCursor(Object);
impl AsObject for NSCursor {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
    }
}
impl NSObject for NSCursor {}
impl NSCursor {
    #[inline(always)]
    pub fn current<'a>() -> &'a mut Self {
        unsafe {
            &mut *Class::require(c"NSCursor")
                .send0r::<*mut Object>(Selector::get_cached(c"currentCursor"))
                .cast::<Self>()
        }
    }

    #[inline(always)]
    pub fn arrow<'a>() -> &'a mut Self {
        unsafe {
            &mut *Class::require(c"NSCursor")
                .send0r::<*mut Object>(Selector::get_cached(c"arrowCursor"))
                .cast::<Self>()
        }
    }

    #[inline(always)]
    pub fn ibeam<'a>() -> &'a mut Self {
        unsafe {
            &mut *Class::require(c"NSCursor")
                .send0r::<*mut Object>(Selector::get_cached(c"IBeamCursor"))
                .cast::<Self>()
        }
    }

    #[inline(always)]
    pub fn pointing_hand<'a>() -> &'a mut Self {
        unsafe {
            &mut *Class::require(c"NSCursor")
                .send0r::<*mut Object>(Selector::get_cached(c"pointingHandCursor"))
                .cast::<Self>()
        }
    }

    #[inline(always)]
    pub fn resize_left_right<'a>() -> &'a mut Self {
        unsafe {
            &mut *Class::require(c"NSCursor")
                .send0r::<*mut Object>(Selector::get_cached(c"resizeLeftRightCursor"))
                .cast::<Self>()
        }
    }

    #[inline(always)]
    pub fn set(&self) {
        unsafe {
            self.0.send0(Selector::get_cached(c"set"));
        }
    }
}

#[repr(transparent)]
pub struct NSSavePanelObject(Object);
impl AsObject for NSSavePanelObject {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
    }
}
impl NSObject for NSSavePanelObject {}
impl NSSavePanel for NSSavePanelObject {}

pub trait NSSavePanel: NSObject {
    #[inline(always)]
    fn new() -> Owned<NSSavePanelObject> {
        unsafe {
            Owned::from_ptr_unchecked(
                Class::require(c"NSOpenPanel")
                    .send0r::<*mut Object>(Selector::get_cached(c"savePanel"))
                    .cast::<NSSavePanelObject>(),
            )
        }
    }

    fn begin_sheet_modal_for_window<F: Fn(NSModalResponse)>(
        &self,
        w: &NSWindow,
        completion_handler: F,
    ) {
        extern "C" fn copy_helper_impl<F>(
            dst: *mut ClosureBlockObject<F>,
            src: *const ClosureBlockObject<F>,
        ) {
            unsafe {
                core::ptr::copy(&(*src).heading, &mut (*dst).heading, 1);
                core::ptr::write(&mut (*dst).closure, (*src).closure.clone());
            }
        }
        extern "C" fn dispose_helper_impl<F>(src: *mut ClosureBlockObject<F>) {
            unsafe { core::ptr::drop_in_place(&mut (*src).closure) }
        }

        let block_lit: ClosureBlockObject<F> = ClosureBlockObject {
            heading: BlockObjectBase {
                isa: unsafe { &_NSConcreteStackBlock as _ },
                flags: BLOCK_HAS_COPY_DISPOSE,
                reserved: 0,
                invoke: Some(unsafe {
                    core::mem::transmute::<
                        extern "C" fn(*mut ClosureBlockObject<F>, NSModalResponse),
                        _,
                    >(closure1_block_invoke)
                }),
                descriptor: &const {
                    ClosureBlockDescriptor {
                        heading: BlockDescriptorBase {
                            reserved: 0,
                            size: core::mem::size_of::<ClosureBlockObject<F>>() as _,
                        },
                        copy_helper: Some(copy_helper_impl::<F>),
                        dispose_helper: Some(dispose_helper_impl::<F>),
                    }
                },
            },
            closure: std::rc::Rc::new(completion_handler),
        };

        unsafe {
            self.as_object().send2(
                Selector::get_cached(c"beginSheetModalForWindow:completionHandler:"),
                w.as_object() as *const _,
                block_lit,
            )
        }
    }
}

pub type NSModalResponse = NSInteger;

#[repr(transparent)]
pub struct NSOpenPanelObject(Object);
impl AsObject for NSOpenPanelObject {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        &mut self.0
    }
}
impl NSObject for NSOpenPanelObject {}
impl NSSavePanel for NSOpenPanelObject {}
impl NSOpenPanel for NSOpenPanelObject {}
impl NSOpenPanelObject {
    #[inline(always)]
    pub fn new() -> Owned<Self> {
        unsafe {
            Owned::from_ptr_unchecked(
                Class::require(c"NSOpenPanel")
                    .send0r::<*mut Object>(Selector::get_cached(c"openPanel"))
                    .cast::<Self>(),
            )
        }
    }
}

pub trait NSOpenPanel: NSSavePanel {
    #[inline(always)]
    fn set_can_choose_files(&mut self, flag: bool) {
        unsafe {
            self.as_object_mut().send1(
                Selector::get_cached(c"setCanChooseFiles:"),
                if flag { 1 } else { 0 } as BOOL,
            )
        }
    }

    #[inline(always)]
    fn set_can_choose_directories(&mut self, flag: bool) {
        unsafe {
            self.as_object_mut().send1(
                Selector::get_cached(c"setCanChooseDirectories:"),
                if flag { 1 } else { 0 } as BOOL,
            )
        }
    }

    #[inline(always)]
    fn set_allows_multiple_selection(&mut self, flag: bool) {
        unsafe {
            self.as_object_mut().send1(
                Selector::get_cached(c"setAllowsMultipleSelection:"),
                if flag { 1 } else { 0 } as BOOL,
            )
        }
    }
}
