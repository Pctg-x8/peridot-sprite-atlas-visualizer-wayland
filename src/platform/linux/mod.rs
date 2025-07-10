mod epoll;
mod eventfd;
pub mod input_event_codes;

use core::ptr::NonNull;

use bitflags::bitflags;

pub use self::epoll::*;
pub use self::eventfd::*;

bitflags! {
    #[derive(Clone, Copy)]
    pub struct MemoryProtectionFlags : core::ffi::c_int {
        const READ = libc::PROT_READ;
        const WRITE = libc::PROT_WRITE;
    }
}
bitflags! {
    #[derive(Clone, Copy)]
    pub struct MemoryMapFlags : core::ffi::c_int {
        const SHARED = libc::MAP_SHARED;
    }
}

pub struct MappedMemoryBlock {
    head_ptr: NonNull<core::ffi::c_void>,
    length: usize,
}
impl Drop for MappedMemoryBlock {
    #[inline(always)]
    fn drop(&mut self) {
        if unsafe { munmap(self.head_ptr.as_ptr(), self.length) } < 0 {
            tracing::warn!(reason = ?std::io::Error::last_os_error(), "munmap failed");
            unreachable!();
        }
    }
}
impl MappedMemoryBlock {
    pub const fn unwrap(self) -> (NonNull<core::ffi::c_void>, usize) {
        let head_ptr = unsafe { core::ptr::read(&self.head_ptr) };
        let length = unsafe { core::ptr::read(&self.length) };
        core::mem::forget(self);

        (head_ptr, length)
    }

    pub const fn ptr(&self) -> NonNull<core::ffi::c_void> {
        self.head_ptr
    }

    pub const fn ptr_of<T>(&self) -> NonNull<T> {
        self.head_ptr.cast()
    }
}

#[inline]
pub fn mmap_random(
    fd: core::ffi::c_int,
    range: core::ops::Range<usize>,
    prot: MemoryProtectionFlags,
    flags: MemoryMapFlags,
) -> Result<MappedMemoryBlock, std::io::Error> {
    let r = unsafe {
        mmap(
            core::ptr::null_mut(),
            range.end - range.start,
            prot.bits(),
            flags.bits(),
            fd,
            range.start as _,
        )
    };
    if r == unsafe { core::mem::transmute::<isize, *mut core::ffi::c_void>(-1) } {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(MappedMemoryBlock {
            head_ptr: unsafe { NonNull::new_unchecked(r) },
            length: range.end - range.start,
        })
    }
}

bitflags! {
    #[derive(Clone, Copy)]
    pub struct OpenFlags : core::ffi::c_int {
        const READ_WRITE = libc::O_RDWR;
        const EXCLUSIVE = libc::O_EXCL;
        const CREATE = libc::O_CREAT;
    }
}

pub struct TemporalSharedMemory {
    name: std::ffi::CString,
    fd: core::ffi::c_int,
}
impl Drop for TemporalSharedMemory {
    #[inline]
    fn drop(&mut self) {
        if unsafe { close(self.fd) } < 0 {
            tracing::error!(reason = ?std::io::Error::last_os_error(), "implicit close() failed");
            unreachable!();
        }

        if unsafe { shm_unlink(self.name.as_ptr()) } < 0 {
            tracing::error!(reason = ?std::io::Error::last_os_error(), "implicit shm_unlink() failed");
            unreachable!();
        }
    }
}
impl std::os::fd::AsRawFd for TemporalSharedMemory {
    #[inline(always)]
    fn as_raw_fd(&self) -> std::os::unix::prelude::RawFd {
        self.fd
    }
}
impl TemporalSharedMemory {
    #[inline]
    pub fn create(
        name: std::ffi::CString,
        additional_flags: OpenFlags,
        mode: libc::mode_t,
    ) -> Result<Self, (std::io::Error, std::ffi::CString)> {
        let fd = unsafe {
            shm_open(
                name.as_ptr(),
                (additional_flags | OpenFlags::CREATE).bits(),
                mode,
            )
        };
        if fd < 0 {
            Err((std::io::Error::last_os_error(), name))
        } else {
            Ok(Self { name, fd })
        }
    }

    #[inline]
    pub fn truncate(&self, length: libc::off_t) -> Result<(), std::io::Error> {
        if unsafe { ftruncate(self.fd, length) } < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[inline]
    pub fn mmap_random(
        &self,
        range: core::ops::Range<usize>,
        prot: MemoryProtectionFlags,
        flags: MemoryMapFlags,
    ) -> Result<MappedMemoryBlock, std::io::Error> {
        mmap_random(self.fd, range, prot, flags)
    }
}

#[link(name = "c")]
unsafe extern "C" {
    fn close(fd: core::ffi::c_int) -> core::ffi::c_int;
    fn ftruncate(fd: core::ffi::c_int, length: libc::off_t) -> core::ffi::c_int;

    fn mmap(
        addr: *mut core::ffi::c_void,
        length: usize,
        prot: core::ffi::c_int,
        flags: core::ffi::c_int,
        fd: core::ffi::c_int,
        offs: libc::off_t,
    ) -> *mut core::ffi::c_void;
    fn munmap(addr: *mut core::ffi::c_void, length: usize) -> core::ffi::c_int;
}

#[link(name = "rt")]
unsafe extern "C" {
    fn shm_open(
        name: *const core::ffi::c_char,
        oflag: core::ffi::c_int,
        mode: libc::mode_t,
    ) -> core::ffi::c_int;
    fn shm_unlink(name: *const core::ffi::c_char) -> core::ffi::c_int;
}
