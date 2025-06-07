use std::{
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

use fontconfig::{
    FC_FAMILY, FC_FILE, FC_INDEX, FC_WEIGHT, FcConfig, FcConfigGetCurrent, FcConfigSubstitute,
    FcDefaultSubstitute, FcFalse, FcFontSet, FcFontSetDestroy, FcFontSort, FcInit, FcMatchPattern,
    FcPattern, FcPatternAddInteger, FcPatternAddString, FcPatternCreate, FcPatternDestroy,
    FcPatternGetInteger, FcPatternGetString, FcPatternReference, FcResult, FcResultMatch,
    FcResultNoId, FcResultNoMatch, FcResultOutOfMemory, FcResultTypeMismatch, FcTrue,
};

pub fn init() {
    unsafe {
        FcInit();
    }
}

#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Pattern = FcMatchPattern,
}

#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorResultValue {
    NoMatch,
    TypeMismatch,
    NoId,
    OutOfMemory,
    Unknown(FcResult),
}
impl ErrorResultValue {
    const fn from_fc_result(x: FcResult) -> Result<(), Self> {
        if x == FcResultMatch {
            return Ok(());
        }
        if x == FcResultNoMatch {
            return Err(Self::NoMatch);
        }
        if x == FcResultTypeMismatch {
            return Err(Self::TypeMismatch);
        }
        if x == FcResultNoId {
            return Err(Self::NoId);
        }
        if x == FcResultOutOfMemory {
            return Err(Self::OutOfMemory);
        }

        Err(Self::Unknown(x))
    }
}

#[repr(transparent)]
pub struct Config(FcConfig);
impl Config {
    pub fn current<'a>() -> Option<&'a mut Self> {
        let ptr = unsafe { FcConfigGetCurrent() };

        unsafe { (ptr as *mut Self).as_mut() }
    }

    pub fn substitute(&mut self, pat: &mut Pattern, kind: MatchKind) {
        unsafe {
            FcConfigSubstitute(self as *mut _ as _, pat as *mut _ as _, kind as _);
        }
    }

    pub fn sort(
        &mut self,
        pat: &mut Pattern,
        trim: bool,
    ) -> Result<Owned<FontSet>, ErrorResultValue> {
        let mut result = MaybeUninit::uninit();
        let ptr = unsafe {
            FcFontSort(
                self as *mut _ as _,
                pat as *mut _ as _,
                if trim { FcTrue } else { FcFalse },
                core::ptr::null_mut(),
                result.as_mut_ptr(),
            )
        };

        ErrorResultValue::from_fc_result(unsafe { result.assume_init() })
            .map(|_| Owned(unsafe { core::ptr::NonNull::new_unchecked(ptr as _) }))
    }
}

pub trait FcObjectDroppable {
    unsafe fn drop_internal(this: *mut Self);
}
pub trait FcObjectCloneable {
    unsafe fn clone_internal(this: *mut Self);
}

#[repr(transparent)]
pub struct Owned<T: FcObjectDroppable>(core::ptr::NonNull<T>);
impl<T: FcObjectDroppable> Drop for Owned<T> {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe { T::drop_internal(self.0.as_ptr()) }
    }
}
impl<T: FcObjectCloneable + FcObjectDroppable> Clone for Owned<T> {
    #[inline(always)]
    fn clone(&self) -> Self {
        unsafe {
            T::clone_internal(self.0.as_ptr());
        }

        Self(self.0)
    }
}
impl<T: FcObjectDroppable> Deref for Owned<T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}
impl<T: FcObjectDroppable> DerefMut for Owned<T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

#[repr(transparent)]
pub struct Pattern(FcPattern);
impl FcObjectDroppable for Pattern {
    unsafe fn drop_internal(this: *mut Self) {
        unsafe { FcPatternDestroy(this as _) }
    }
}
impl FcObjectCloneable for Pattern {
    unsafe fn clone_internal(this: *mut Self) {
        unsafe { FcPatternReference(this as _) }
    }
}
impl Pattern {
    pub fn new() -> Owned<Self> {
        let ptr = unsafe { FcPatternCreate() };

        Owned(unsafe { core::ptr::NonNull::new_unchecked(ptr as _) })
    }

    pub fn add_string(&mut self, object: &core::ffi::CStr, value: &core::ffi::CStr) {
        unsafe {
            FcPatternAddString(self as *mut _ as _, object.as_ptr(), value.as_ptr() as _);
        }
    }

    pub fn add_integer(&mut self, object: &core::ffi::CStr, value: core::ffi::c_int) {
        unsafe {
            FcPatternAddInteger(self as *mut _ as _, object.as_ptr(), value);
        }
    }

    pub fn add_family_name(&mut self, value: &core::ffi::CStr) {
        self.add_string(FC_FAMILY, value)
    }

    pub fn add_weight(&mut self, weight: core::ffi::c_int) {
        self.add_integer(FC_WEIGHT, weight)
    }

    pub fn default_substitute(&mut self) {
        unsafe {
            FcDefaultSubstitute(self as *mut _ as _);
        }
    }

    pub fn get_string(
        &self,
        object: &core::ffi::CStr,
        id: core::ffi::c_int,
    ) -> Result<&core::ffi::CStr, ErrorResultValue> {
        let mut ptr = MaybeUninit::uninit();
        let r = unsafe {
            FcPatternGetString(self as *const _ as _, object.as_ptr(), id, ptr.as_mut_ptr())
        };

        ErrorResultValue::from_fc_result(r)
            .map(|_| unsafe { core::ffi::CStr::from_ptr(ptr.assume_init() as _) })
    }

    pub fn get_integer(
        &self,
        object: &core::ffi::CStr,
        id: core::ffi::c_int,
    ) -> Result<core::ffi::c_int, ErrorResultValue> {
        let mut v = MaybeUninit::uninit();
        let r = unsafe {
            FcPatternGetInteger(self as *const _ as _, object.as_ptr(), id, v.as_mut_ptr())
        };

        ErrorResultValue::from_fc_result(r).map(|_| unsafe { v.assume_init() })
    }

    #[inline]
    pub fn get_file_path(&self, id: core::ffi::c_int) -> Option<&core::ffi::CStr> {
        match self.get_string(FC_FILE, id) {
            Ok(x) => Some(x),
            Err(ErrorResultValue::NoId) => None,
            e => panic!("get_file_path failed: {e:?}"),
        }
    }

    #[inline]
    pub fn get_face_index(&self, id: core::ffi::c_int) -> Option<core::ffi::c_int> {
        match self.get_integer(FC_INDEX, id) {
            Ok(x) => Some(x),
            Err(ErrorResultValue::NoId) => None,
            e => panic!("get_face_index failed: {e:?}"),
        }
    }
}

#[repr(transparent)]
pub struct FontSet(FcFontSet);
impl FcObjectDroppable for FontSet {
    unsafe fn drop_internal(this: *mut Self) {
        unsafe {
            FcFontSetDestroy(this as _);
        }
    }
}
impl FontSet {
    pub fn fonts(&self) -> &[&Pattern] {
        unsafe { core::slice::from_raw_parts_mut(self.0.fonts as _, self.0.nfont as _) }
    }
}
