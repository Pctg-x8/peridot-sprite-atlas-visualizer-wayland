use std::{
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

mod ffi;

#[repr(transparent)]
pub struct BufferOwned(core::ptr::NonNull<Buffer>);
impl Drop for BufferOwned {
    fn drop(&mut self) {
        unsafe {
            ffi::hb_buffer_destroy(self.0.as_ptr() as _);
        }
    }
}
impl Clone for BufferOwned {
    fn clone(&self) -> Self {
        let ptr = unsafe { ffi::hb_buffer_reference(self.0.as_ptr() as _) };

        Self(unsafe { core::ptr::NonNull::new_unchecked(ptr as _) })
    }
}
impl Deref for BufferOwned {
    type Target = Buffer;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}
impl DerefMut for BufferOwned {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

#[repr(transparent)]
pub struct Buffer(ffi::hb_buffer_t);
impl Buffer {
    pub fn new() -> BufferOwned {
        let ptr = unsafe { ffi::hb_buffer_create() };

        BufferOwned(unsafe { core::ptr::NonNull::new_unchecked(ptr as _) })
    }

    pub fn add(&mut self, content: &str) {
        unsafe {
            ffi::hb_buffer_add_utf8(
                self as *mut _ as _,
                content.as_ptr() as _,
                content.len() as _,
                0,
                -1,
            )
        }
    }

    pub fn guess_segment_properties(&mut self) {
        unsafe { ffi::hb_buffer_guess_segment_properties(self as *mut _ as _) }
    }

    pub fn get_shape_results(&mut self) -> (&mut [GlyphInfo], &mut [GlyphPosition]) {
        let mut count = MaybeUninit::uninit();
        let ptr =
            unsafe { ffi::hb_buffer_get_glyph_infos(self as *mut _ as _, count.as_mut_ptr()) };

        let info = unsafe { core::slice::from_raw_parts_mut(ptr, count.assume_init() as _) };

        let ptr =
            unsafe { ffi::hb_buffer_get_glyph_positions(self as *mut _ as _, count.as_mut_ptr()) };

        let pos = unsafe { core::slice::from_raw_parts_mut(ptr, count.assume_init() as _) };

        (info, pos)
    }
}

pub type GlyphInfo = ffi::hb_glyph_info_t;
pub type GlyphPosition = ffi::hb_glyph_position_t;

#[repr(transparent)]
pub struct FontOwned(core::ptr::NonNull<Font>);
impl Drop for FontOwned {
    fn drop(&mut self) {
        unsafe { ffi::hb_font_destroy(self.0.as_ptr() as _) }
    }
}
impl Clone for FontOwned {
    fn clone(&self) -> Self {
        let ptr = unsafe { ffi::hb_font_reference(self.0.as_ptr() as _) };

        Self(unsafe { core::ptr::NonNull::new_unchecked(ptr as _) })
    }
}
impl Deref for FontOwned {
    type Target = Font;

    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}
impl DerefMut for FontOwned {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

#[repr(transparent)]
pub struct Font(ffi::hb_font_t);
impl Font {
    pub fn from_ft_face_referenced(face: &mut freetype::Face) -> FontOwned {
        let ptr = unsafe { ffi::hb_ft_font_create_referenced(face.as_raw()) };

        FontOwned(unsafe { core::ptr::NonNull::new_unchecked(ptr as _) })
    }
}

pub type Feature = ffi::hb_feature_t;

#[inline(always)]
pub fn shape(font: &mut Font, buffer: &mut Buffer, features: &[Feature]) {
    unsafe {
        ffi::hb_shape(
            font as *mut _ as _,
            buffer as *mut _ as _,
            features.as_ptr(),
            features.len() as _,
        )
    }
}
