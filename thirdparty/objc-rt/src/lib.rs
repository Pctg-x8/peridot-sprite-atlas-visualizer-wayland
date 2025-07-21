use core::ffi::*;
use ffi_common::FFIOpaqueStruct;

pub mod appkit;
pub mod corefoundation;
pub mod foundation;

FFIOpaqueStruct!(pub struct Class);

FFIOpaqueStruct!(pub struct Method);
FFIOpaqueStruct!(pub struct Ivar);
FFIOpaqueStruct!(pub struct Category);
FFIOpaqueStruct!(pub struct Property);
FFIOpaqueStruct!(pub struct Selector);
FFIOpaqueStruct!(pub struct Object);

pub type IMP = extern "C" fn();
pub type BOOL = core::ffi::c_char;

#[repr(C)]
pub struct Super {
    pub receiver: *mut Object,
    pub super_class: *mut Class,
}

#[link(name = "objc")]
unsafe extern "C" {
    pub unsafe fn objc_getClass(name: *const c_char) -> *mut Class;
    pub unsafe fn objc_msgSend();
    pub unsafe fn objc_msgSendSuper();
    pub unsafe fn objc_allocateClassPair(
        superclass: *mut Class,
        name: *const core::ffi::c_char,
        extra_bytes: usize,
    ) -> *mut Class;
    pub unsafe fn objc_registerClassPair(cls: *mut Class);

    pub unsafe fn sel_registerName(str: *const c_char) -> *mut Selector;

    pub unsafe fn class_addMethod(
        cls: *mut Class,
        name: *const Selector,
        imp: IMP,
        types: *const c_char,
    ) -> BOOL;
    pub unsafe fn class_getProperty(cls: *mut Class, name: *const c_char) -> *mut Property;
}

impl Super {
    #[inline(always)]
    pub fn send1o<A>(&self, sel: &Selector, a: A) -> *mut Object {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Super, *const Selector, A) -> *mut Object,
            >(objc_msgSendSuper))(self as *const _, sel as *const _, a)
        }
    }
}

impl Class {
    #[inline(always)]
    pub fn get<'a>(name: &CStr) -> Option<&'a mut Self> {
        unsafe { objc_getClass(name.as_ptr()).as_mut() }
    }

    #[inline(always)]
    pub unsafe fn allocate_pair<'a>(
        superclass: Option<&mut Class>,
        name: &CStr,
        extra_bytes: usize,
    ) -> Option<&'a mut Self> {
        unsafe {
            objc_allocateClassPair(
                superclass.map_or(core::ptr::null_mut(), |x| x as *mut _),
                name.as_ptr(),
                extra_bytes,
            )
            .as_mut()
        }
    }

    #[inline(always)]
    pub unsafe fn register_pair(&mut self) {
        unsafe { objc_registerClassPair(self as *mut _) }
    }

    #[inline(always)]
    pub fn add_method(&mut self, name: &Selector, imp: IMP, types: &CStr) -> bool {
        unsafe { class_addMethod(self as *mut _, name as *const _, imp, types.as_ptr()) != 0 }
    }

    #[inline(always)]
    pub unsafe fn send0(&self, sel: &Selector) {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Class, *const Selector),
            >(objc_msgSend))(self as *const _, sel)
        }
    }

    #[inline(always)]
    pub unsafe fn send0o(&self, sel: &Selector) -> *mut Object {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Class, *const Selector) -> *mut Object,
            >(objc_msgSend))(self as *const _, sel)
        }
    }

    #[inline(always)]
    pub unsafe fn send1o<A>(&self, sel: &Selector, a: A) -> *mut Object {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Class, *const Selector, A) -> *mut Object,
            >(objc_msgSend))(self as *const _, sel, a)
        }
    }
}

impl Object {
    #[inline(always)]
    pub unsafe fn send0(&self, sel: &Selector) {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Object, *const Selector),
            >(objc_msgSend))(self as *const _, sel)
        }
    }

    #[inline(always)]
    pub unsafe fn send0o(&self, sel: &Selector) -> *mut Object {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Object, *const Selector) -> *mut Object,
            >(objc_msgSend))(self as *const _, sel)
        }
    }

    #[inline(always)]
    pub unsafe fn send0v<Ret>(&self, sel: &Selector) -> Ret {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Object, *const Selector) -> Ret,
            >(objc_msgSend))(self as *const _, sel)
        }
    }

    #[inline(always)]
    pub unsafe fn send1<A>(&self, sel: &Selector, a: A) {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Object, *const Selector, A),
            >(objc_msgSend))(self as *const _, sel as *const _, a)
        }
    }

    #[inline(always)]
    pub unsafe fn send1o<A>(&self, sel: &Selector, a: A) -> *mut Object {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Object, *const Selector, A) -> *mut Object,
            >(objc_msgSend))(self as *const _, sel as *const _, a)
        }
    }

    #[inline(always)]
    pub unsafe fn send2o<A, B>(&self, sel: &Selector, a: A, b: B) -> *mut Object {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Object, *const Selector, A, B) -> *mut Object,
            >(objc_msgSend))(self as *const _, sel as *const _, a, b)
        }
    }

    #[inline(always)]
    pub unsafe fn send4o<A, B, C, D>(&self, sel: &Selector, a: A, b: B, c: C, d: D) -> *mut Object {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Object, *const Selector, A, B, C, D) -> *mut Object,
            >(objc_msgSend))(self as *const _, sel as *const _, a, b, c, d)
        }
    }

    #[inline(always)]
    pub unsafe fn send5o<A, B, C, D, E>(
        &self,
        sel: &Selector,
        a: A,
        b: B,
        c: C,
        d: D,
        e: E,
    ) -> *mut Object {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Object, *const Selector, A, B, C, D, E) -> *mut Object,
            >(objc_msgSend))(self as *const _, sel as *const _, a, b, c, d, e)
        }
    }
}

impl Selector {
    pub fn get<'a>(name: &CStr) -> &'a Self {
        unsafe { &*sel_registerName(name.as_ptr()) }
    }
}

pub trait AsObject {
    fn as_object(&self) -> &Object;
}

pub trait NSObject: AsObject {
    #[inline(always)]
    fn retain(&self) {
        unsafe {
            self.as_object().send0o(Selector::get(c"retain"));
        }
    }

    #[inline(always)]
    fn release(&self) {
        unsafe {
            self.as_object().send0(Selector::get(c"release"));
        }
    }
}

#[repr(transparent)]
pub struct Owned<T: NSObject>(core::ptr::NonNull<T>);
impl<T: NSObject> Drop for Owned<T> {
    #[inline(always)]
    fn drop(&mut self) {
        tracing::debug!(target: "objc_rt::drop_checker", type_name = ?core::any::type_name::<T>(), "Dropping Objc object");
        unsafe {
            self.0.as_ref().release();
        }
    }
}
impl<T: NSObject> Clone for Owned<T> {
    #[inline(always)]
    fn clone(&self) -> Self {
        unsafe {
            self.0.as_ref().retain();
        }

        Self(self.0)
    }
}
impl<T: NSObject> core::ops::Deref for Owned<T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}
impl<T: NSObject> core::ops::DerefMut for Owned<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}
impl<T: NSObject + core::fmt::Debug> core::fmt::Debug for Owned<T> {
    #[inline(always)]
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        T::fmt(self, f)
    }
}
impl<T: NSObject> Owned<T> {
    pub const unsafe fn from_ptr_unchecked(ptr: *mut T) -> Self {
        Self(unsafe { core::ptr::NonNull::new_unchecked(ptr) })
    }

    pub const unsafe fn from_ptr(ptr: *mut T) -> Option<Self> {
        match core::ptr::NonNull::new(ptr) {
            Some(x) => Some(Self(x)),
            None => None,
        }
    }
}
