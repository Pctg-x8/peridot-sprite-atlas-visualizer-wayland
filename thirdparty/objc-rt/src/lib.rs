#![allow(non_camel_case_types)]

use core::ffi::*;
use ffi_common::FFIOpaqueStruct;
use std::{cell::UnsafeCell, collections::HashMap};

use crate::appkit::NSUInteger;

pub mod appkit;
pub mod coreanimation;
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
    pub unsafe fn objc_getRequiredClass(name: *const c_char) -> *mut Class;
    pub unsafe fn objc_msgSend();
    pub unsafe fn objc_msgSendSuper();
    pub unsafe fn objc_allocateClassPair(
        superclass: *mut Class,
        name: *const core::ffi::c_char,
        extra_bytes: usize,
    ) -> *mut Class;
    pub unsafe fn objc_registerClassPair(cls: *mut Class);

    pub unsafe fn sel_registerName(str: *const c_char) -> *mut Selector;

    pub unsafe fn class_addIvar(
        cls: *mut Class,
        name: *const core::ffi::c_char,
        size: usize,
        alignment: u8,
        types: *const core::ffi::c_char,
    ) -> BOOL;
    pub unsafe fn class_addMethod(
        cls: *mut Class,
        name: *const Selector,
        imp: IMP,
        types: *const c_char,
    ) -> BOOL;
    pub unsafe fn class_getProperty(cls: *mut Class, name: *const c_char) -> *mut Property;

    pub unsafe fn object_setInstanceVariable(
        obj: *mut Object,
        name: *const core::ffi::c_char,
        value: *mut core::ffi::c_void,
    ) -> *mut Ivar;
    pub unsafe fn object_getInstanceVariable(
        obj: *mut Object,
        name: *const core::ffi::c_char,
        outValue: *mut *mut core::ffi::c_void,
    ) -> *mut Ivar;
    pub unsafe fn object_getClass(obj: *mut Object) -> *mut Class;

    pub unsafe fn class_getInstanceVariable(cls: *mut Class, name: *const c_char) -> *mut Ivar;

    pub unsafe fn ivar_getOffset(v: *mut Ivar) -> isize;
}

pub type objc_uncaught_exception_handler = extern "C" fn(exception: *mut Object);

impl Super {
    #[inline(always)]
    pub unsafe fn send0(&self, sel: &Selector) {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Super, *const Selector),
            >(objc_msgSendSuper))(self as *const _, sel as *const _)
        }
    }

    #[inline(always)]
    pub unsafe fn send1<A>(&self, sel: &Selector, a: A) {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Super, *const Selector, A),
            >(objc_msgSendSuper))(self as *const _, sel as *const _, a)
        }
    }

    #[inline(always)]
    pub unsafe fn send1o<A>(&self, sel: &Selector, a: A) -> *mut Object {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Super, *const Selector, A) -> *mut Object,
            >(objc_msgSendSuper))(self as *const _, sel as *const _, a)
        }
    }
}

pub(crate) trait Sealed {}
impl Sealed for Class {}
impl Sealed for Object {}

#[allow(private_bounds)]
pub trait Receiver: Sealed {
    #[inline(always)]
    unsafe fn send0(&self, sel: &Selector) {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Self, *const Selector),
            >(objc_msgSend))(self, sel)
        }
    }

    #[inline(always)]
    unsafe fn send0r<Ret>(&self, sel: &Selector) -> Ret {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Self, *const Selector) -> Ret,
            >(objc_msgSend))(self, sel)
        }
    }

    #[inline(always)]
    unsafe fn send1<A>(&self, sel: &Selector, a: A) {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Self, *const Selector, A),
            >(objc_msgSend))(self, sel, a)
        }
    }

    #[inline(always)]
    unsafe fn send1r<A, Ret>(&self, sel: &Selector, a: A) -> Ret {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Self, *const Selector, A) -> Ret,
            >(objc_msgSend))(self, sel, a)
        }
    }

    #[inline(always)]
    unsafe fn send2<A, B>(&self, sel: &Selector, a: A, b: B) {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Self, *const Selector, A, B),
            >(objc_msgSend))(self, sel, a, b)
        }
    }

    #[inline(always)]
    unsafe fn send2r<A, B, Ret>(&self, sel: &Selector, a: A, b: B) -> Ret {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Self, *const Selector, A, B) -> Ret,
            >(objc_msgSend))(self, sel, a, b)
        }
    }

    #[inline(always)]
    unsafe fn send3r<A, B, C, Ret>(&self, sel: &Selector, a: A, b: B, c: C) -> Ret {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Self, *const Selector, A, B, C) -> Ret,
            >(objc_msgSend))(self, sel, a, b, c)
        }
    }

    #[inline(always)]
    unsafe fn send4<A, B, C, D>(&self, sel: &Selector, a: A, b: B, c: C, d: D) {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Self, *const Selector, A, B, C, D),
            >(objc_msgSend))(self, sel, a, b, c, d)
        }
    }

    #[inline(always)]
    unsafe fn send4r<A, B, C, D, Ret>(&self, sel: &Selector, a: A, b: B, c: C, d: D) -> Ret {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Self, *const Selector, A, B, C, D) -> Ret,
            >(objc_msgSend))(self, sel, a, b, c, d)
        }
    }

    #[inline(always)]
    unsafe fn send5r<A, B, C, D, E, Ret>(
        &self,
        sel: &Selector,
        a: A,
        b: B,
        c: C,
        d: D,
        e: E,
    ) -> Ret {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(*const Self, *const Selector, A, B, C, D, E) -> Ret,
            >(objc_msgSend))(self, sel, a, b, c, d, e)
        }
    }

    #[inline(always)]
    unsafe fn send9r<A, B, C, D, E, F, G, H, I, Ret>(
        &self,
        sel: &Selector,
        a: A,
        b: B,
        c: C,
        d: D,
        e: E,
        f: F,
        g: G,
        h: H,
        i: I,
    ) -> Ret {
        unsafe {
            (core::mem::transmute::<
                unsafe extern "C" fn(),
                unsafe extern "C" fn(
                    *const Self,
                    *const Selector,
                    A,
                    B,
                    C,
                    D,
                    E,
                    F,
                    G,
                    H,
                    I,
                ) -> Ret,
            >(objc_msgSend))(self, sel, a, b, c, d, e, f, g, h, i)
        }
    }
}
impl Receiver for Class {}
impl Receiver for Object {}

impl Class {
    #[inline(always)]
    pub fn get<'a>(name: &CStr) -> Option<&'a mut Self> {
        unsafe { objc_getClass(name.as_ptr()).as_mut() }
    }

    #[inline(always)]
    pub fn require<'a>(name: &CStr) -> &'a mut Self {
        unsafe { &mut *objc_getRequiredClass(name.as_ptr()) }
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
    pub fn add_ivar(&mut self, name: &CStr, size: usize, alignment: u8, types: &CStr) -> bool {
        unsafe {
            class_addIvar(
                self as *mut _,
                name.as_ptr(),
                size,
                alignment,
                types.as_ptr(),
            ) != 0
        }
    }

    #[inline(always)]
    pub fn add_method(&mut self, name: &Selector, imp: IMP, types: &CStr) -> bool {
        unsafe { class_addMethod(self as *mut _, name as *const _, imp, types.as_ptr()) != 0 }
    }

    #[inline(always)]
    pub fn get_ivar(&self, name: &CStr) -> *mut Ivar {
        // TODO: 明示的にObjectが!Freezeであることをなんとか明示したい
        unsafe { class_getInstanceVariable(self as *const _ as *mut _, name.as_ptr()) }
    }
}

impl Object {
    #[inline(always)]
    pub unsafe fn ivar_ref_by_name<'x, T>(&'x self, name: &CStr) -> &'x T {
        unsafe {
            &*(self as *const Self)
                .byte_offset((*(*self.get_class()).get_ivar(name)).offset())
                .cast::<T>()
        }
    }

    #[inline(always)]
    pub unsafe fn ivar_ref_mut_by_name<'x, T>(&'x mut self, name: &CStr) -> &'x mut T {
        unsafe {
            &mut *(self as *mut Self)
                .byte_offset((*(*self.get_class()).get_ivar(name)).offset())
                .cast::<T>()
        }
    }

    #[inline(always)]
    pub fn get_class(&self) -> *mut Class {
        // TODO: 明示的にObjectが!Freezeであることをなんとか明示したい
        unsafe { object_getClass(self as *const _ as *mut _) }
    }
}

thread_local! {
    static SELECTOR_CACHE: UnsafeCell<HashMap<&'static core::ffi::CStr, &'static Selector>> = UnsafeCell::new(HashMap::new());
}

impl Selector {
    #[inline(always)]
    pub fn get<'a>(name: &CStr) -> &'a Self {
        unsafe { &*sel_registerName(name.as_ptr()) }
    }

    #[inline(always)]
    pub fn get_cached(name: &'static core::ffi::CStr) -> &'static Self {
        SELECTOR_CACHE
            .with(|c| unsafe { *(*c.get()).entry(name).or_insert_with(|| Self::get(name)) })
    }
}

impl Ivar {
    #[inline(always)]
    pub fn offset(&mut self) -> isize {
        unsafe { ivar_getOffset(self as *mut _) }
    }
}

pub trait AsObject {
    fn as_object(&self) -> &Object;
    fn as_object_mut(&mut self) -> &mut Object;

    #[inline(always)]
    unsafe fn ivar_ref_by_name<'x, T>(&'x self, name: &CStr) -> &'x T {
        unsafe { self.as_object().ivar_ref_by_name(name) }
    }

    #[inline(always)]
    unsafe fn ivar_ref_mut_by_name<'x, T>(&'x mut self, name: &CStr) -> &'x mut T {
        unsafe { self.as_object_mut().ivar_ref_mut_by_name(name) }
    }
}
impl AsObject for Object {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        self
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        self
    }
}

pub trait NSObject: AsObject {
    #[inline(always)]
    fn retain(&self) {
        unsafe {
            self.as_object()
                .send0r::<*mut Object>(Selector::get_cached(c"retain"));
        }
    }

    #[inline(always)]
    fn release(&self) {
        unsafe {
            self.as_object().send0(Selector::get_cached(c"release"));
        }
    }

    #[inline(always)]
    fn retain_count(&self) -> NSUInteger {
        unsafe {
            self.as_object()
                .send0r(Selector::get_cached(c"retainCount"))
        }
    }
}

#[repr(transparent)]
pub struct Owned<T: NSObject>(core::ptr::NonNull<T>);
unsafe impl<T: NSObject + Sync> Sync for Owned<T> {}
unsafe impl<T: NSObject + Send> Send for Owned<T> {}
impl<T: NSObject> Drop for Owned<T> {
    #[inline(always)]
    fn drop(&mut self) {
        tracing::debug!(target: "objc_rt::drop_checker", type_name = ?core::any::type_name::<T>(), retain_count = unsafe { self.0.as_ref().retain_count() }, "Dropping Objc object");
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
impl<T: NSObject> AsObject for Owned<T> {
    #[inline(always)]
    fn as_object(&self) -> &Object {
        unsafe { self.0.as_ref().as_object() }
    }

    #[inline(always)]
    fn as_object_mut(&mut self) -> &mut Object {
        unsafe { self.0.as_mut().as_object_mut() }
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

    pub unsafe fn retain_ptr_unchecked(ptr: *mut T) -> Self {
        let x = unsafe { core::ptr::NonNull::new_unchecked(ptr) };
        unsafe {
            x.as_ref().retain();
        }

        Self(x)
    }

    pub const fn leak(self) -> *mut T {
        let ptr = self.0.as_ptr();
        core::mem::forget(self);
        ptr
    }
}
