use bitflags::bitflags;

use crate::{
    AsObject, BOOL, Class, NSObject, Object, Owned, Selector,
    appkit::{NSInteger, NSUInteger},
};

#[repr(C)]
pub struct NSString(Object);
impl AsObject for NSString {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSString {}
impl core::fmt::Debug for NSString {
    #[inline]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.utf8_string(), f)
    }
}
impl NSString {
    pub fn from_utf8_string(x: &core::ffi::CStr) -> Owned<Self> {
        unsafe {
            Owned::from_ptr_unchecked(
                Class::get(c"NSString")
                    .expect("no NSString class")
                    .send1o(Selector::get(c"stringWithUTF8String:"), x.as_ptr())
                    as *mut Self,
            )
        }
    }

    #[inline]
    pub fn utf8_string(&self) -> &core::ffi::CStr {
        unsafe { core::ffi::CStr::from_ptr(self.0.send0r(Selector::get(c"UTF8String"))) }
    }
}

#[repr(transparent)]
pub struct NSError(Object);
impl AsObject for NSError {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSError {}
impl core::fmt::Debug for NSError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "NSError {{ code: {}, domain: {:?}, localized_description: {:?} }}",
            self.code(),
            self.domain(),
            self.localized_description()
        )
    }
}
impl NSError {
    #[inline]
    pub fn code(&self) -> NSInteger {
        unsafe { self.0.send0r(Selector::get(c"code")) }
    }

    #[inline]
    pub fn domain(&self) -> Owned<NSString> {
        unsafe {
            Owned::from_ptr_unchecked(
                self.0.send0r::<*mut Object>(Selector::get(c"domain")) as *mut NSString
            )
        }
    }

    #[inline]
    pub fn localized_description(&self) -> Owned<NSString> {
        unsafe {
            Owned::from_ptr_unchecked(
                self.0
                    .send0r::<*mut Object>(Selector::get(c"localizedDescription"))
                    as *mut NSString,
            )
        }
    }
}

#[repr(transparent)]
pub struct NSURL(Object);
impl AsObject for NSURL {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSURL {}
impl NSURL {
    pub fn absolute_string(&self) -> Owned<NSString> {
        unsafe {
            Owned::from_ptr_unchecked(
                self.0
                    .send0r::<*mut NSString>(Selector::get(c"absoluteString")),
            )
        }
    }

    pub fn file_system_representation(&self) -> &core::ffi::CStr {
        unsafe {
            core::ffi::CStr::from_ptr(
                self.0
                    .send0r::<*const core::ffi::c_char>(Selector::get(c"fileSystemRepresentation")),
            )
        }
    }
}

#[repr(transparent)]
pub struct NSRunLoop(Object);
impl AsObject for NSRunLoop {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSRunLoop {}
impl NSRunLoop {
    #[inline]
    pub fn main<'a>() -> &'a mut Self {
        unsafe {
            &mut *Class::require(c"NSRunLoop")
                .send0o(Selector::get(c"mainRunLoop"))
                .cast::<Self>()
        }
    }
    #[inline]
    pub fn current<'a>() -> &'a mut Self {
        unsafe {
            &mut *Class::require(c"NSRunLoop")
                .send0o(Selector::get(c"currentRunLoop"))
                .cast::<Self>()
        }
    }

    #[inline(always)]
    pub fn run(&mut self) {
        unsafe { self.0.send0(Selector::get(c"run")) }
    }

    #[inline]
    pub fn run_mode_before(&mut self, mode: NSRunLoopMode, before_date: &mut NSDate) -> bool {
        unsafe {
            self.0.send2r::<_, _, BOOL>(
                Selector::get(c"runMode:beforeDate:"),
                (*mode).as_object() as *const _,
                before_date.as_object() as *const _,
            ) != 0
        }
    }

    #[inline]
    pub fn run_until(&mut self, date: &mut NSDate) {
        unsafe {
            self.0.send1(
                Selector::get(c"runUntilDate:"),
                date.as_object() as *const _,
            )
        }
    }
}

pub type NSRunLoopMode = *mut NSString;
unsafe extern "C" {
    pub static NSDefaultRunLoopMode: NSRunLoopMode;
}

#[repr(u32)]
pub enum NSSearchPathDirectory {
    CachesDirectory = 13,
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct NSSearchPathDomainMask : NSUInteger {
        const UserDomainMask = 1 ;
        const LocalDomainMask = 2;
        const NetworkDomainMask = 4;
        const SystemDomainMask = 8;
        const NSAllDomainsMask = 0x0fff;
    }
}

#[repr(transparent)]
pub struct NSFileManager(Object);
impl AsObject for NSFileManager {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSFileManager {}
impl NSFileManager {
    pub fn default() -> Owned<Self> {
        unsafe {
            Owned::from_ptr_unchecked(
                Class::get(c"NSFileManager")
                    .expect("no NSFileManager class")
                    .send0o(Selector::get(c"defaultManager")) as *mut Self,
            )
        }
    }

    pub fn url_for_directory(
        &self,
        directory: NSSearchPathDirectory,
        domain_mask: NSSearchPathDomainMask,
        appropriate_for_url: Option<&NSURL>,
        create: bool,
    ) -> Result<Owned<NSURL>, Owned<NSError>> {
        let mut error = core::mem::MaybeUninit::<*mut NSError>::uninit();
        let url = unsafe {
            self.0.send5o(
                Selector::get(c"URLForDirectory:inDomain:appropriateForURL:create:error:"),
                directory as NSUInteger,
                domain_mask.bits(),
                appropriate_for_url.map_or_else(core::ptr::null, |x| x.as_object() as *const _),
                if create { 1 } else { 0 } as BOOL,
                error.as_mut_ptr(),
            )
        };

        if !url.is_null() {
            Ok(unsafe { Owned::from_ptr_unchecked(url as *mut _) })
        } else {
            Err(unsafe { Owned::from_ptr_unchecked(error.assume_init()) })
        }
    }
}

#[repr(transparent)]
pub struct NSDate(Object);
impl AsObject for NSDate {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSDate {}
impl NSDate {
    #[inline]
    pub fn distant_past<'a>() -> &'a mut Self {
        unsafe {
            &mut *Class::require(c"NSDate")
                .send0o(Selector::get(c"distantPast"))
                .cast::<Self>()
        }
    }

    #[inline]
    pub fn distant_future<'a>() -> &'a mut Self {
        unsafe {
            &mut *Class::require(c"NSDate")
                .send0o(Selector::get(c"distantFuture"))
                .cast::<Self>()
        }
    }
}

#[repr(transparent)]
pub struct NSNotification(Object);
impl AsObject for NSNotification {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSNotification {}

#[repr(transparent)]
pub struct NSNotificationCenter(Object);
impl AsObject for NSNotificationCenter {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl NSObject for NSNotificationCenter {}
impl NSNotificationCenter {
    #[inline(always)]
    pub fn default<'a>() -> &'a mut Self {
        unsafe {
            &mut *Class::require(c"NSNotificationCenter")
                .send0r::<*mut Object>(Selector::get(c"defaultCenter"))
                .cast::<Self>()
        }
    }

    #[inline(always)]
    pub fn add_observer<Observer: AsObject, Sender: AsObject>(
        &self,
        observer: &Observer,
        selector: &Selector,
        name: Option<NSNotificationName>,
        object: Option<&Sender>,
    ) {
        unsafe {
            self.0.send4(
                Selector::get(c"addObserver:selector:name:object:"),
                observer.as_object() as *const _,
                selector as *const _,
                name.map_or_else(core::ptr::null, |x| x as *const _),
                object.map_or_else(core::ptr::null, |x| x.as_object() as *const _),
            );
        }
    }
}

pub type NSNotificationName = *mut NSString;

#[repr(transparent)]
pub struct NSArrayObject<T: NSObject>(Object, core::marker::PhantomData<T>);
impl<T: NSObject> AsObject for NSArrayObject<T> {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        &self.0
    }
}
impl<T: NSObject> NSObject for NSArrayObject<T> {}
impl<T: NSObject> NSArray for NSArrayObject<T> {
    type Item = T;
}

pub trait NSArray: NSObject {
    type Item: NSObject;

    #[inline(always)]
    fn count(&self) -> NSUInteger {
        unsafe { self.as_object().send0r(Selector::get(c"count")) }
    }

    #[inline(always)]
    fn object_at_index<'a>(&'a self, index: NSUInteger) -> &'a Self::Item {
        unsafe {
            &*self
                .as_object()
                .send1r::<_, *mut Object>(Selector::get(c"objectAtIndex:"), index)
                .cast::<Self::Item>()
        }
    }
}

pub type NSUncaughtExceptionHandler = extern "C" fn(exception: *mut Object);
unsafe extern "C" {
    pub fn NSSetUncaughtExceptionHandler(handler: NSUncaughtExceptionHandler);
}
