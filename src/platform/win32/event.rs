//! Event Object Wrapper

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::SECURITY_ATTRIBUTES,
    System::Threading::{CreateEventW, ResetEvent, SetEvent},
};

#[repr(transparent)]
pub struct EventObject(HANDLE);
unsafe impl Sync for EventObject {}
unsafe impl Send for EventObject {}
impl Drop for EventObject {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0).unwrap();
        }
    }
}
impl EventObject {
    #[inline(always)]
    pub fn new(
        security_attributes: Option<*const SECURITY_ATTRIBUTES>,
        manual_reset: bool,
        initial_state: bool,
    ) -> windows::core::Result<Self> {
        unsafe {
            Ok(Self(CreateEventW(
                security_attributes,
                manual_reset,
                initial_state,
                None,
            )?))
        }
    }

    pub const fn handle(&self) -> HANDLE {
        self.0
    }

    #[inline(always)]
    pub fn set(&self) -> windows::core::Result<()> {
        unsafe { SetEvent(self.0) }
    }

    #[inline(always)]
    pub fn reset(&self) -> windows::core::Result<()> {
        unsafe { ResetEvent(self.0) }
    }
}
