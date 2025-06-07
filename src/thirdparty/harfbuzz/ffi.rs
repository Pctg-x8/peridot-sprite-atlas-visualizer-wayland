#![allow(non_camel_case_types)]

/// https://doc.rust-lang.org/nomicon/ffi.html#representing-opaque-structs
macro_rules! FFIOpaqueStruct {
    ($v: vis struct $t: ident) => {
        #[repr(C)]
        $v struct $t {
            _data: [u8; 0],
            _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
        }
    }
}

FFIOpaqueStruct!(pub struct hb_buffer_t);

pub type hb_direction_t = core::ffi::c_int;
pub const HB_DIRECTION_INVALID: hb_direction_t = 0;
pub const HB_DIRECTION_LTR: hb_direction_t = 4;
pub const HB_DIRECTION_RTL: hb_direction_t = 5;
pub const HB_DIRECTION_TIB: hb_direction_t = 6;
pub const HB_DIRECTION_BIT: hb_direction_t = 7;

pub type hb_script_t = core::ffi::c_uint;
pub const HB_SCRIPT_COMMON: hb_script_t = hb_tag(b"Zyyy");
pub const HB_SCRIPT_INHERITED: hb_script_t = hb_tag(b"Zinh");
pub const HB_SCRIPT_UNKNOWN: hb_script_t = hb_tag(b"Zzzz");
pub const HB_SCRIPT_LATIN: hb_script_t = hb_tag(b"Latn");

FFIOpaqueStruct!(pub struct hb_language_impl_t);
pub type hb_language_t = *const hb_language_impl_t;

pub type hb_tag_t = u32;
const fn hb_tag(tag: &[u8; 4]) -> hb_tag_t {
    (tag[0] as u32) << 24 | (tag[1] as u32) << 16 | (tag[2] as u32) << 8 | tag[3] as u32
}

#[repr(C)]
pub struct hb_feature_t {
    pub tag: hb_tag_t,
    pub value: u32,
    pub start: core::ffi::c_uint,
    pub end: core::ffi::c_uint,
}

FFIOpaqueStruct!(pub struct hb_font_t);

pub type hb_codepoint_t = u32;
pub type hb_mask_t = u32;

#[repr(C)]
pub union hb_var_int_t {
    r#u32: u32,
    r#i32: i32,
    r#u16: [u16; 2],
    r#i16: [i16; 2],
    r#u8: [u8; 4],
    r#i8: [i8; 4],
}

#[repr(C)]
pub struct hb_glyph_info_t {
    pub codepoint: hb_codepoint_t,
    mask: hb_mask_t,
    pub cluster: u32,
    var1: hb_var_int_t,
    var2: hb_var_int_t,
}

pub type hb_position_t = i32;

#[repr(C)]
pub struct hb_glyph_position_t {
    pub x_advance: hb_position_t,
    pub y_advance: hb_position_t,
    pub x_offset: hb_position_t,
    pub y_offset: hb_position_t,
    var: hb_var_int_t,
}

FFIOpaqueStruct!(pub struct hb_face_t);

#[link(name = "harfbuzz")]
unsafe extern "C" {
    pub unsafe fn hb_buffer_create() -> *mut hb_buffer_t;
    pub unsafe fn hb_buffer_reference(buffer: *mut hb_buffer_t) -> *mut hb_buffer_t;
    pub unsafe fn hb_buffer_destroy(buffer: *mut hb_buffer_t);
    pub unsafe fn hb_buffer_add_utf8(
        buffer: *mut hb_buffer_t,
        text: *const core::ffi::c_char,
        text_length: core::ffi::c_int,
        item_offset: core::ffi::c_uint,
        item_length: core::ffi::c_int,
    );
    pub unsafe fn hb_buffer_set_direction(buffer: *mut hb_buffer_t, direction: hb_direction_t);
    pub unsafe fn hb_buffer_set_script(buffer: *mut hb_buffer_t, script: hb_script_t);
    pub unsafe fn hb_buffer_set_language(buffer: *mut hb_buffer_t, language: hb_language_t);
    pub unsafe fn hb_buffer_guess_segment_properties(buffer: *mut hb_buffer_t);
    pub unsafe fn hb_buffer_get_glyph_infos(
        buffer: *mut hb_buffer_t,
        length: *mut core::ffi::c_uint,
    ) -> *mut hb_glyph_info_t;
    pub unsafe fn hb_buffer_get_glyph_positions(
        buffer: *mut hb_buffer_t,
        length: *mut core::ffi::c_uint,
    ) -> *mut hb_glyph_position_t;

    pub unsafe fn hb_shape(
        font: *mut hb_font_t,
        buffer: *mut hb_buffer_t,
        features: *const hb_feature_t,
        num_features: core::ffi::c_uint,
    );

    pub unsafe fn hb_language_from_string(
        str: *const core::ffi::c_char,
        len: core::ffi::c_int,
    ) -> hb_language_t;

    pub unsafe fn hb_font_destroy(font: *mut hb_font_t);
    pub unsafe fn hb_font_reference(font: *mut hb_font_t) -> *mut hb_font_t;

    pub unsafe fn hb_ft_face_create_referenced(ft_face: freetype2::FT_Face) -> *mut hb_face_t;
    pub unsafe fn hb_ft_font_create_referenced(ft_face: freetype2::FT_Face) -> *mut hb_font_t;
}
