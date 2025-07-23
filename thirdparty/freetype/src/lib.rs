use std::{
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

use bitflags::bitflags;
use freetype2::{
    errors::FT_Error_String,
    modapi::{FT_Property_Get, FT_Property_Set},
    *,
};

pub use freetype2::FT_Bool as Bool;
pub use freetype2::FT_GlyphSlotRec as GlyphSlotRec;
pub use freetype2::FT_Vector as Vector;

#[repr(transparent)]
pub struct FreeType(core::ptr::NonNull<FT_LibraryRec>);
impl Drop for FreeType {
    fn drop(&mut self) {
        let r = unsafe { FT_Done_FreeType(self.0.as_ptr()) };
        if r != 0 {
            eprintln!("FreeType Done error: {r:?}");
        }
    }
}
impl FreeType {
    pub fn new() -> Result<Self, FT_Error> {
        let mut ptr = MaybeUninit::uninit();
        let r = unsafe { FT_Init_FreeType(ptr.as_mut_ptr()) };
        if r != 0 {
            return Err(r);
        }

        Ok(unsafe { Self(core::ptr::NonNull::new_unchecked(ptr.assume_init())) })
    }

    pub fn new_face(
        &mut self,
        path: &core::ffi::CStr,
        face_index: FT_Long,
    ) -> Result<Owned<Face>, FT_Error> {
        let mut ptr = MaybeUninit::uninit();
        let r =
            unsafe { FT_New_Face(self.0.as_ptr(), path.as_ptr(), face_index, ptr.as_mut_ptr()) };
        if r != 0 {
            return Err(r);
        }

        Ok(unsafe { Owned(core::ptr::NonNull::new_unchecked(ptr.assume_init() as _)) })
    }

    pub unsafe fn get_property<T>(
        &self,
        module_name: &core::ffi::CStr,
        property_name: &core::ffi::CStr,
    ) -> Result<T, FT_Error> {
        let mut sink = core::mem::MaybeUninit::uninit();
        let r = unsafe {
            FT_Property_Get(
                self.0.as_ptr(),
                module_name.as_ptr(),
                property_name.as_ptr(),
                sink.as_mut_ptr() as _,
            )
        };
        if r != 0 {
            return Err(r);
        }

        Ok(unsafe { sink.assume_init() })
    }

    pub unsafe fn set_property<T>(
        &mut self,
        module_name: &core::ffi::CStr,
        property_name: &core::ffi::CStr,
        value: &T,
    ) -> Result<(), FT_Error> {
        let r = unsafe {
            FT_Property_Set(
                self.0.as_ptr(),
                module_name.as_ptr(),
                property_name.as_ptr(),
                value as *const _ as _,
            )
        };
        if r != 0 {
            return Err(r);
        }

        Ok(())
    }
}

pub trait FtObjectDroppable {
    unsafe fn drop_internal(this: *mut Self);
}
pub trait FtObjectRefCounted {
    unsafe fn create_reference(this: *mut Self);
}

#[repr(transparent)]
pub struct Owned<T: FtObjectDroppable>(core::ptr::NonNull<T>);
impl<T: FtObjectDroppable> Drop for Owned<T> {
    fn drop(&mut self) {
        unsafe { T::drop_internal(self.0.as_ptr()) }
    }
}
impl<T: FtObjectDroppable + FtObjectRefCounted> Clone for Owned<T> {
    fn clone(&self) -> Self {
        unsafe {
            T::create_reference(self.0.as_ptr());
        }

        Self(self.0)
    }
}
impl<T: FtObjectDroppable> Deref for Owned<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}
impl<T: FtObjectDroppable> DerefMut for Owned<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct LoadFlags : i32 {
        const DEFAULT = FT_LOAD_DEFAULT;
    }
}

#[repr(i32)]
#[derive(Debug, Clone, Copy)]
pub enum RenderMode {
    Normal = FT_RENDER_MODE_NORMAL,
    Light = FT_RENDER_MODE_LIGHT,
    Mono = FT_RENDER_MODE_MONO,
    LCD = FT_RENDER_MODE_LCD,
    LCDV = FT_RENDER_MODE_LCD_V,
}

#[repr(transparent)]
pub struct Face(FT_FaceRec);
impl FtObjectDroppable for Face {
    unsafe fn drop_internal(this: *mut Self) {
        let r = unsafe { FT_Done_Face(this as _) };
        if r != 0 {
            eprintln!("FreeType Face Done error: {r}");
        }
    }
}
impl FtObjectRefCounted for Face {
    unsafe fn create_reference(this: *mut Self) {
        let r = unsafe { FT_Reference_Face(this as _) };
        if r != 0 {
            panic!("FreeType Face Reference error: {r}");
        }
    }
}
impl Face {
    pub const fn as_raw(&mut self) -> FT_Face {
        self as *mut _ as _
    }

    pub const fn ascender_pixels(&self) -> f64 {
        unsafe { (*self.0.size).metrics.ascender as f64 / 64.0 }
    }

    pub const fn height_pixels(&self) -> f64 {
        unsafe { (*self.0.size).metrics.height as f64 / 64.0 }
    }

    pub fn set_char_size(
        &mut self,
        char_width: FT_F26Dot6,
        char_height: FT_F26Dot6,
        horz_resolution: FT_UInt,
        vert_resolution: FT_UInt,
    ) -> Result<(), FT_Error> {
        let r = unsafe {
            FT_Set_Char_Size(
                self as *mut _ as _,
                char_width,
                char_height,
                horz_resolution,
                vert_resolution,
            )
        };
        if r != 0 {
            return Err(r);
        }

        Ok(())
    }

    #[inline]
    pub fn set_transform(&mut self, matrix: Option<&FT_Matrix>, delta: Option<&FT_Vector>) {
        unsafe {
            FT_Set_Transform(
                self as *mut _ as _,
                match matrix {
                    Some(x) => x as *const _ as _,
                    None => core::ptr::null_mut(),
                },
                match delta {
                    Some(x) => x as *const _ as _,
                    None => core::ptr::null_mut(),
                },
            )
        }
    }

    #[inline]
    pub fn load_glyph(&mut self, glyph: FT_UInt, load_flags: LoadFlags) -> Result<(), Error> {
        match unsafe { FT_Load_Glyph(self as *mut _ as _, glyph, load_flags.bits()) } {
            0 => Ok(()),
            r => Err(Error(r)),
        }
    }

    #[inline]
    pub fn render_glyph(&mut self, render_mode: RenderMode) -> Result<(), Error> {
        let r = unsafe { FT_Render_Glyph(self.0.glyph, render_mode as _) };
        if r != 0 {
            return Err(Error(r));
        }

        Ok(())
    }

    pub const fn glyph_slot(&self) -> Option<&FT_GlyphSlotRec> {
        unsafe { self.0.glyph.as_ref() }
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Error(FT_Error);
impl core::fmt::Debug for Error {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        unsafe { core::ffi::CStr::from_ptr(FT_Error_String(self.0)) }.fmt(f)
    }
}
