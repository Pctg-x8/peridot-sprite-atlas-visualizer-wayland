#![cfg(target_os = "linux")]

use std::os::fd::AsRawFd;

use bitflags::bitflags;

bitflags! {
    #[derive(Clone, Copy)]
    pub struct EventFDOptions : core::ffi::c_int {
        const CLOEXEC = libc::EFD_CLOEXEC;
        const NONBLOCK = libc::EFD_NONBLOCK;
    }
}

#[repr(transparent)]
pub struct EventFD(core::ffi::c_int);
impl Drop for EventFD {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}
impl AsRawFd for EventFD {
    #[inline(always)]
    fn as_raw_fd(&self) -> std::os::unix::prelude::RawFd {
        self.0
    }
}
impl EventFD {
    #[inline]
    pub fn new(init: core::ffi::c_uint, options: EventFDOptions) -> std::io::Result<Self> {
        match unsafe { libc::eventfd(init, options.bits()) } {
            -1 => Err(std::io::Error::last_os_error()),
            fd => Ok(Self(fd)),
        }
    }

    #[inline]
    pub fn take(&self) -> std::io::Result<u64> {
        let mut sink = core::mem::MaybeUninit::<u64>::uninit();
        match unsafe { libc::read(self.0, sink.as_mut_ptr() as _, core::mem::size_of::<u64>()) } {
            -1 => Err(std::io::Error::last_os_error()),
            8 => Ok(unsafe { sink.assume_init() }),
            r => unreachable!("eventfd read returns unexpected value: {r}"),
        }
    }

    #[inline]
    pub fn add(&self, val: u64) -> std::io::Result<()> {
        match unsafe { libc::write(self.0, &val as *const _ as _, core::mem::size_of::<u64>()) } {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        }
    }
}
