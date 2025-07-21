use bitflags::bitflags;

use core::ffi::*;

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
