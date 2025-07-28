#![allow(non_camel_case_types)]

use std::os::fd::AsRawFd;

pub type EPOLL_EVENTS = u32;
pub const EPOLLIN: EPOLL_EVENTS = 0x01;
pub const EPOLLPRI: EPOLL_EVENTS = 0x02;
pub const EPOLLOUT: EPOLL_EVENTS = 0x04;
pub const EPOLLRDNORM: EPOLL_EVENTS = 0x40;
pub const EPOLLRDBAND: EPOLL_EVENTS = 0x80;
pub const EPOLLWRNORM: EPOLL_EVENTS = 0x100;
pub const EPOLLWRBAND: EPOLL_EVENTS = 0x200;
pub const EPOLLMSG: EPOLL_EVENTS = 0x400;
pub const EPOLLERR: EPOLL_EVENTS = 0x800;
pub const EPOLLHUP: EPOLL_EVENTS = 0x10;
pub const EPOLLRDHUP: EPOLL_EVENTS = 0x2000;
pub const EPOLLEXCLUSIVE: EPOLL_EVENTS = 1 << 28;
pub const EPOLLWAKEUP: EPOLL_EVENTS = 1 << 29;
pub const EPOLLONESHOT: EPOLL_EVENTS = 1 << 30;
pub const EPOLLET: EPOLL_EVENTS = 1 << 31;

pub const EPOLL_CTL_ADD: core::ffi::c_int = 1;
pub const EPOLL_CTL_DEL: core::ffi::c_int = 2;
pub const EPOLL_CTL_MOD: core::ffi::c_int = 3;

#[repr(C)]
pub union epoll_data {
    pub ptr: *mut core::ffi::c_void,
    pub fd: core::ffi::c_int,
    pub r#u32: u32,
    pub r#u64: u64,
}

#[repr(C, packed)]
pub struct epoll_event {
    pub events: u32,
    pub data: epoll_data,
}

#[repr(C)]
pub struct epoll_params {
    pub busy_poll_usecs: u32,
    pub busy_poll_budget: u16,
    pub prefer_busy_poll: u8,
    __pad: u8,
}

pub const EPOLL_IOC_TYPE: core::ffi::c_uint = 0x8a;
pub const EPIOCSPARAMS: libc::Ioctl = libc::_IOW::<epoll_params>(EPOLL_IOC_TYPE, 0x01);
pub const EPIOCGPARAMS: libc::Ioctl = libc::_IOR::<epoll_params>(EPOLL_IOC_TYPE, 0x02);

#[link(name = "c")]
unsafe extern "C" {
    pub fn epoll_create1(flags: core::ffi::c_int) -> core::ffi::c_int;
    pub fn epoll_ctl(
        epfd: core::ffi::c_int,
        op: core::ffi::c_int,
        fd: core::ffi::c_int,
        event: *mut epoll_event,
    ) -> core::ffi::c_int;
    pub fn epoll_wait(
        epfd: core::ffi::c_int,
        events: *mut epoll_event,
        maxevents: core::ffi::c_int,
        timeout: core::ffi::c_int,
    ) -> core::ffi::c_int;

    fn close(fd: core::ffi::c_int) -> core::ffi::c_int;
}

#[derive(Clone, Copy)]
pub enum EpollData {
    Ptr(*mut core::ffi::c_void),
    Fd(core::ffi::c_int),
    U32(u32),
    U64(u64),
}
impl EpollData {
    const fn sys(self) -> epoll_data {
        match self {
            Self::Ptr(x) => epoll_data { ptr: x },
            Self::Fd(x) => epoll_data { fd: x },
            Self::U32(x) => epoll_data { r#u32: x },
            Self::U64(x) => epoll_data { r#u64: x },
        }
    }
}

#[repr(transparent)]
pub struct Epoll(core::ffi::c_int);
impl Drop for Epoll {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            close(self.0);
        }
    }
}
impl Epoll {
    #[inline]
    pub fn new(flags: core::ffi::c_int) -> std::io::Result<Self> {
        match unsafe { epoll_create1(flags) } {
            fd if fd < 0 => Err(std::io::Error::last_os_error()),
            fd => Ok(Self(fd)),
        }
    }

    #[inline]
    pub unsafe fn ctl(
        &self,
        op: core::ffi::c_int,
        fd: &(impl AsRawFd + ?Sized),
        event: *mut epoll_event,
    ) -> std::io::Result<()> {
        match unsafe { epoll_ctl(self.0, op, fd.as_raw_fd(), event) } {
            0 => Ok(()),
            _ => Err(std::io::Error::last_os_error()),
        }
    }

    #[inline(always)]
    pub fn add(
        &self,
        fd: &(impl AsRawFd + ?Sized),
        events: u32,
        data: EpollData,
    ) -> std::io::Result<()> {
        unsafe {
            self.ctl(
                EPOLL_CTL_ADD,
                fd,
                &mut epoll_event {
                    events,
                    data: data.sys(),
                },
            )
        }
    }

    #[inline(always)]
    pub fn del(&self, fd: &(impl AsRawFd + ?Sized)) -> std::io::Result<()> {
        unsafe { self.ctl(EPOLL_CTL_DEL, fd, core::ptr::null_mut()) }
    }

    #[inline(always)]
    pub fn r#mod(
        &self,
        fd: &(impl AsRawFd + ?Sized),
        events: u32,
        data: EpollData,
    ) -> std::io::Result<()> {
        unsafe {
            self.ctl(
                EPOLL_CTL_MOD,
                fd,
                &mut epoll_event {
                    events,
                    data: data.sys(),
                },
            )
        }
    }

    #[inline]
    pub fn wait(
        &self,
        events: &mut [core::mem::MaybeUninit<epoll_event>],
        timeout: Option<core::ffi::c_int>,
    ) -> std::io::Result<usize> {
        match unsafe {
            epoll_wait(
                self.0,
                events.as_mut_ptr() as _,
                events.len() as _,
                timeout.unwrap_or(-1),
            )
        } {
            r if r < 0 => Err(std::io::Error::last_os_error()),
            r => Ok(r as usize),
        }
    }
}
