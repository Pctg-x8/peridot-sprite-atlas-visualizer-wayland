//! Win32 Specific Helpers

pub mod event;

use windows::Win32::Foundation::{CloseHandle, HANDLE};

#[repr(transparent)]
pub struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if let Err(e) = unsafe { CloseHandle(self.0) } {
            tracing::warn!(reason = ?e, "closing handle failed");
        }
    }
}
impl OwnedHandle {
    pub const unsafe fn from_raw(h: HANDLE) -> Self {
        Self(h)
    }

    pub const fn handle(&self) -> &HANDLE {
        &self.0
    }
}
