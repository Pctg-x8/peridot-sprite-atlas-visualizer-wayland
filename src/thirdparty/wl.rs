use core::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};
use std::{cell::UnsafeCell, os::fd::AsRawFd};

mod cursor_shape;
mod ffi;
mod fractional_scale;
mod xdg_foreign;
mod xdg_shell;
use ffi::wl_proxy_destroy;

pub use self::cursor_shape::*;
pub use self::ffi::Fixed;
pub use self::fractional_scale::*;
pub use self::xdg_foreign::*;
pub use self::xdg_shell::*;

const NEWID_ARG: ffi::Argument = ffi::Argument { n: 0 };

#[repr(transparent)]
pub struct OwnedProxy(NonNull<ffi::Proxy>);
impl Drop for OwnedProxy {
    fn drop(&mut self) {
        unsafe { ffi::wl_proxy_destroy(self.0.as_ptr()) }
    }
}
impl Deref for OwnedProxy {
    type Target = Proxy;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { Proxy::from_raw_ref(self.0.as_ref()) }
    }
}
impl DerefMut for OwnedProxy {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { Proxy::from_raw_ref_mut(self.0.as_mut()) }
    }
}

#[repr(transparent)]
pub struct Proxy(UnsafeCell<ffi::Proxy>);
impl Proxy {
    pub const unsafe fn from_raw_ptr_unchecked<'a>(ptr: *mut ffi::Proxy) -> &'a mut Self {
        unsafe { Self::from_raw_ref_mut(&mut *ptr) }
    }

    pub const unsafe fn from_raw_ref<'a>(r: &'a ffi::Proxy) -> &'a Self {
        unimplemented!()
        // unsafe { core::mem::transmute(r) }
    }

    pub const unsafe fn from_raw_ref_mut<'a>(r: &'a mut ffi::Proxy) -> &'a mut Self {
        unsafe { core::mem::transmute(UnsafeCell::from_mut(r)) }
    }

    #[inline(always)]
    pub fn version(&self) -> u32 {
        unsafe { ffi::wl_proxy_get_version(self as *const _ as _) }
    }

    #[inline(always)]
    pub unsafe fn add_listener(
        &mut self,
        function_table: *const core::ffi::c_void,
        user_data: *mut core::ffi::c_void,
    ) -> Result<(), ()> {
        let r =
            unsafe { ffi::wl_proxy_add_listener(self.0.get_mut() as _, function_table, user_data) };

        if r == 0 { Ok(()) } else { Err(()) }
    }

    #[inline]
    fn marshal_array_flags(
        &self,
        opcode: u32,
        interface: &ffi::Interface,
        version: u32,
        flags: u32,
        args: &mut [ffi::Argument],
    ) -> Result<NonNull<Proxy>, std::io::Error> {
        unsafe {
            NonNull::new(ffi::wl_proxy_marshal_array_flags(
                self.0.get(),
                opcode,
                interface as *const _,
                version,
                flags,
                args.as_mut_ptr(),
            ))
            .ok_or_else(|| std::io::Error::last_os_error())
            .map(NonNull::cast)
        }
    }

    #[inline]
    fn marshal_array_flags_void(
        &self,
        opcode: u32,
        flags: u32,
        args: &mut [ffi::Argument],
    ) -> Result<(), std::io::Error> {
        unsafe {
            ffi::wl_proxy_marshal_array_flags(
                self.0.get(),
                opcode,
                core::ptr::null(),
                self.version(),
                flags,
                if args.is_empty() {
                    core::ptr::null_mut()
                } else {
                    args.as_mut_ptr()
                },
            )
        };

        let e = unsafe { ffi::wl_display_get_error(ffi::wl_proxy_get_display(self.0.get())) };
        if e != 0 {
            Err(std::io::Error::from_raw_os_error(e))
        } else {
            Ok(())
        }
    }
}

/// must be transparent with ffi::Proxy(or Proxy wrapper newtype)
pub unsafe trait Interface {
    fn def() -> &'static ffi::Interface;

    unsafe fn destruct(&mut self) {
        unsafe {
            wl_proxy_destroy(self as *mut _ as _);
        }
    }
}

pub struct Owned<T: Interface>(NonNull<T>);
impl<T: Interface> Drop for Owned<T> {
    fn drop(&mut self) {
        tracing::trace!(type_name = core::any::type_name::<T>(), "drop wl owned");

        unsafe {
            self.0.as_mut().destruct();
        }
    }
}
impl<T: Interface> Deref for Owned<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}
impl<T: Interface> DerefMut for Owned<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}
impl<T: Interface> Owned<T> {
    pub const unsafe fn from_untyped_unchecked(untyped: NonNull<Proxy>) -> Self {
        Self(untyped.cast())
    }

    pub const fn leak(self) {
        core::mem::forget(self);
    }

    pub const fn unwrap(self) -> NonNull<T> {
        let ptr = unsafe { core::ptr::read(&self.0) };
        core::mem::forget(self);

        ptr
    }
}

pub struct Display {
    ffi: NonNull<ffi::Display>,
}
impl Drop for Display {
    fn drop(&mut self) {
        unsafe { ffi::wl_display_disconnect(self.ffi.as_ptr()) }
    }
}
impl AsRawFd for Display {
    #[inline(always)]
    fn as_raw_fd(&self) -> std::os::unix::prelude::RawFd {
        unsafe { ffi::wl_display_get_fd(self.ffi.as_ptr()) }
    }
}
impl Display {
    #[inline]
    pub fn connect() -> Option<Self> {
        let ffi = NonNull::new(unsafe { ffi::wl_display_connect(core::ptr::null()) })?;

        Some(Self { ffi })
    }

    pub const fn as_raw(&mut self) -> *mut ffi::Display {
        self.ffi.as_ptr()
    }

    #[inline]
    pub fn get_registry(&self) -> Result<Owned<Registry>, std::io::Error> {
        let proxy_ptr = unsafe {
            Proxy::from_raw_ptr_unchecked(self.ffi.as_ptr() as _).marshal_array_flags(
                1,
                Registry::def(),
                ffi::wl_proxy_get_version(self.ffi.as_ptr() as _),
                0,
                &mut [NEWID_ARG],
            )?
        };

        Ok(unsafe { Owned::from_untyped_unchecked(proxy_ptr) })
    }

    #[inline]
    pub fn roundtrip(&self) -> Result<u32, ()> {
        let r = unsafe { ffi::wl_display_roundtrip(self.ffi.as_ptr()) };

        if r < 0 { Err(()) } else { Ok(r as _) }
    }

    #[inline]
    pub fn error(&self) -> Option<core::ffi::c_int> {
        let r = unsafe { ffi::wl_display_get_error(self.ffi.as_ptr()) };

        if r == 0 { None } else { Some(r) }
    }

    #[inline]
    pub fn flush(&self) -> Result<core::ffi::c_int, std::io::Error> {
        let r = unsafe { ffi::wl_display_flush(self.ffi.as_ptr()) };

        if r == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(r)
        }
    }

    #[inline]
    pub fn dispatch_pending(&self) -> std::io::Result<core::ffi::c_int> {
        match unsafe { ffi::wl_display_dispatch_pending(self.ffi.as_ptr()) } {
            -1 => Err(std::io::Error::last_os_error()),
            r => Ok(r),
        }
    }

    #[inline]
    pub fn prepare_read(&self) -> std::io::Result<()> {
        match unsafe { ffi::wl_display_prepare_read(self.ffi.as_ptr()) } {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        }
    }

    #[inline]
    pub fn cancel_read(&self) {
        unsafe { ffi::wl_display_cancel_read(self.ffi.as_ptr()) }
    }

    #[inline]
    pub fn read_events(&self) -> std::io::Result<()> {
        match unsafe { ffi::wl_display_read_events(self.ffi.as_ptr()) } {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        }
    }
}

#[repr(transparent)]
pub struct Registry(Proxy);
unsafe impl Interface for Registry {
    fn def() -> &'static ffi::Interface {
        unsafe { &wl_registry_interface }
    }
}
impl Registry {
    pub fn add_listener<'l, L: RegistryListener + 'l>(
        &'l mut self,
        listener: &'l mut L,
    ) -> Result<(), ()> {
        extern "C" fn global_w<L: RegistryListener>(
            data: *mut core::ffi::c_void,
            registry: *mut ffi::Proxy,
            name: u32,
            interface: *const core::ffi::c_char,
            version: u32,
        ) {
            let listener_instance = unsafe { &mut *(data as *mut L) };

            listener_instance.global(
                unsafe { core::mem::transmute(Proxy::from_raw_ptr_unchecked(registry)) },
                name,
                unsafe { core::ffi::CStr::from_ptr(interface) },
                version,
            )
        }
        extern "C" fn global_remove_w<L: RegistryListener>(
            data: *mut core::ffi::c_void,
            registry: *mut ffi::Proxy,
            name: u32,
        ) {
            let listener_instance = unsafe { &mut *(data as *mut L) };

            listener_instance.global_remove(
                unsafe { core::mem::transmute(Proxy::from_raw_ptr_unchecked(registry)) },
                name,
            )
        }

        #[repr(C)]
        struct FunctionPointers {
            global: extern "C" fn(
                *mut core::ffi::c_void,
                *mut ffi::Proxy,
                u32,
                *const core::ffi::c_char,
                u32,
            ),
            global_remove: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32),
        }
        let fp: &'static FunctionPointers = &FunctionPointers {
            global: global_w::<L>,
            global_remove: global_remove_w::<L>,
        };

        unsafe {
            self.0
                .add_listener(fp as *const _ as _, listener as *mut _ as _)
        }
    }

    pub fn bind<I: Interface>(&self, name: u32, version: u32) -> Result<Owned<I>, std::io::Error> {
        let proxy_ptr = self.0.marshal_array_flags(
            0,
            I::def(),
            version,
            0,
            &mut [
                ffi::Argument { u: name },
                // dynamically-typed new id
                ffi::Argument { s: I::def().name },
                ffi::Argument { u: version },
                NEWID_ARG,
            ],
        )?;

        Ok(unsafe { Owned::from_untyped_unchecked(proxy_ptr) })
    }
}

pub trait RegistryListener {
    fn global(
        &mut self,
        registry: &mut Registry,
        name: u32,
        interface: &core::ffi::CStr,
        version: u32,
    );
    fn global_remove(&mut self, registry: &mut Registry, name: u32);
}

#[repr(transparent)]
pub struct Callback(Proxy);
unsafe impl Interface for Callback {
    fn def() -> &'static ffi::Interface {
        unsafe { &wl_callback_interface }
    }
}
impl Callback {
    pub fn add_listener<'l, L: CallbackEventListener + 'l>(
        &'l mut self,
        listener: &'l mut L,
    ) -> Result<(), ()> {
        extern "C" fn done<L: CallbackEventListener>(
            data: *mut core::ffi::c_void,
            callback: *mut ffi::Proxy,
            callback_data: u32,
        ) {
            let listener = unsafe { &mut *(data as *mut L) };

            listener.done(
                unsafe { core::mem::transmute(&mut *callback) },
                callback_data,
            )
        }
        #[repr(C)]
        struct FunctionPointer {
            done: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32),
        }
        let fp: &'static FunctionPointer = &FunctionPointer { done: done::<L> };

        unsafe {
            self.0
                .add_listener(fp as *const _ as _, listener as *mut _ as _)
        }
    }
}

pub trait CallbackEventListener {
    fn done(&mut self, callback: &mut Callback, data: u32);
}

#[repr(transparent)]
pub struct Compositor(Proxy);
unsafe impl Interface for Compositor {
    fn def() -> &'static ffi::Interface {
        unsafe { &wl_compositor_interface }
    }
}
impl Compositor {
    #[inline]
    pub fn create_surface(&self) -> Result<Owned<Surface>, std::io::Error> {
        let proxy_ptr =
            self.0
                .marshal_array_flags(0, Surface::def(), self.0.version(), 0, &mut [NEWID_ARG])?;

        Ok(unsafe { Owned::from_untyped_unchecked(proxy_ptr) })
    }
}

#[repr(transparent)]
pub struct Surface(Proxy);
unsafe impl Interface for Surface {
    fn def() -> &'static ffi::Interface {
        unsafe { &wl_surface_interface }
    }
}
impl Surface {
    pub const fn as_raw(&mut self) -> *mut ffi::Proxy {
        &mut self.0 as *mut _ as _
    }

    #[inline]
    pub fn frame(&self) -> Result<Owned<Callback>, std::io::Error> {
        let proxy_ptr = self.0.marshal_array_flags(
            3,
            Callback::def(),
            self.0.version(),
            0,
            &mut [NEWID_ARG],
        )?;

        Ok(unsafe { Owned::from_untyped_unchecked(proxy_ptr) })
    }

    #[inline]
    pub fn commit(&self) -> Result<(), std::io::Error> {
        self.0.marshal_array_flags_void(6, 0, &mut [])
    }

    #[inline]
    pub fn set_buffer_scale(&self, scale: i32) -> Result<(), std::io::Error> {
        self.0
            .marshal_array_flags_void(8, 0, &mut [ffi::Argument { i: scale }])
    }

    pub fn add_listener<'l, L: SurfaceEventListener + 'l>(
        &'l mut self,
        listener: &'l mut L,
    ) -> Result<(), ()> {
        extern "C" fn enter<L: SurfaceEventListener>(
            data: *mut core::ffi::c_void,
            surface: *mut ffi::Proxy,
            output: *mut ffi::Proxy,
        ) {
            let listener = unsafe { &mut *(data as *mut L) };

            listener.enter(unsafe { core::mem::transmute(&mut *surface) }, unsafe {
                core::mem::transmute(&mut *output)
            })
        }
        extern "C" fn leave<L: SurfaceEventListener>(
            data: *mut core::ffi::c_void,
            surface: *mut ffi::Proxy,
            output: *mut ffi::Proxy,
        ) {
            let listener = unsafe { &mut *(data as *mut L) };

            listener.leave(unsafe { core::mem::transmute(&mut *surface) }, unsafe {
                core::mem::transmute(&mut *output)
            })
        }
        extern "C" fn preferred_buffer_scale<L: SurfaceEventListener>(
            data: *mut core::ffi::c_void,
            surface: *mut ffi::Proxy,
            factor: i32,
        ) {
            let listener = unsafe { &mut *(data as *mut L) };

            listener.preferred_buffer_scale(unsafe { core::mem::transmute(&mut *surface) }, factor)
        }
        extern "C" fn preferred_buffer_transform<L: SurfaceEventListener>(
            data: *mut core::ffi::c_void,
            surface: *mut ffi::Proxy,
            transform: u32,
        ) {
            let listener = unsafe { &mut *(data as *mut L) };

            listener.preferred_buffer_transform(
                unsafe { core::mem::transmute(&mut *surface) },
                transform,
            )
        }
        #[repr(C)]
        struct FunctionPointer {
            enter: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, *mut ffi::Proxy),
            leave: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, *mut ffi::Proxy),
            preferred_buffer_scale: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, i32),
            preferred_buffer_transform: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32),
        }
        let fp: &'static FunctionPointer = &FunctionPointer {
            enter: enter::<L>,
            leave: leave::<L>,
            preferred_buffer_scale: preferred_buffer_scale::<L>,
            preferred_buffer_transform: preferred_buffer_transform::<L>,
        };

        unsafe {
            self.0
                .add_listener(fp as *const _ as _, listener as *mut _ as _)
        }
    }
}

pub trait SurfaceEventListener {
    fn enter(&mut self, surface: &mut Surface, output: &mut Output);
    fn leave(&mut self, surface: &mut Surface, output: &mut Output);
    // --- version 6 additions ---
    fn preferred_buffer_scale(&mut self, surface: &mut Surface, factor: i32);
    fn preferred_buffer_transform(&mut self, surface: &mut Surface, transform: u32);
}

#[repr(transparent)]
pub struct Seat(Proxy);
unsafe impl Interface for Seat {
    fn def() -> &'static ffi::Interface {
        unsafe { &wl_seat_interface }
    }

    unsafe fn destruct(&mut self) {
        if self.0.version() < 5 {
            // no destruction method implemented
            return;
        }

        if let Err(e) = unsafe { self.destroy() } {
            let de = unsafe {
                ffi::wl_display_get_error(ffi::wl_proxy_get_display(&mut self.0 as *mut _ as _))
            };

            panic!("Failed to call destroy: {de} {e:?}");
        }
    }
}
impl Seat {
    #[inline]
    pub fn get_pointer(&self) -> Result<Owned<Pointer>, std::io::Error> {
        let proxy_ptr =
            self.0
                .marshal_array_flags(0, Pointer::def(), self.0.version(), 0, &mut [NEWID_ARG])?;

        Ok(unsafe { Owned::from_untyped_unchecked(proxy_ptr) })
    }

    // v5
    #[inline]
    pub unsafe fn destroy(&self) -> Result<(), std::io::Error> {
        self.0.marshal_array_flags_void(3, 0, &mut [])
    }

    pub fn add_listener<'l, L: SeatEventListener + 'l>(
        &'l mut self,
        listener: &'l mut L,
    ) -> Result<(), ()> {
        extern "C" fn capabilities<L: SeatEventListener>(
            data: *mut core::ffi::c_void,
            seat: *mut ffi::Proxy,
            capabilities: u32,
        ) {
            let listener = unsafe { &mut *(data as *mut L) };

            listener.capabilities(unsafe { core::mem::transmute(&mut *seat) }, capabilities)
        }
        extern "C" fn name<L: SeatEventListener>(
            data: *mut core::ffi::c_void,
            seat: *mut ffi::Proxy,
            name: *const core::ffi::c_char,
        ) {
            let listener = unsafe { &mut *(data as *mut L) };

            listener.name(unsafe { core::mem::transmute(&mut *seat) }, unsafe {
                core::ffi::CStr::from_ptr(name)
            })
        }
        #[repr(C)]
        struct FunctionPointer {
            capabilities: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32),
            name: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, *const core::ffi::c_char),
        }
        let fp: &'static FunctionPointer = &FunctionPointer {
            capabilities: capabilities::<L>,
            name: name::<L>,
        };

        unsafe {
            self.0
                .add_listener(fp as *const _ as _, listener as *mut _ as _)
        }
    }
}

pub trait SeatEventListener {
    fn capabilities(&mut self, seat: &mut Seat, capabilities: u32);
    // v2
    fn name(&mut self, seat: &mut Seat, name: &core::ffi::CStr);
}

#[repr(transparent)]
pub struct Pointer(Proxy);
unsafe impl Interface for Pointer {
    fn def() -> &'static ffi::Interface {
        unsafe { &wl_pointer_interface }
    }
}
impl Pointer {
    pub fn add_listener<'l, L: PointerEventListener + 'l>(
        &'l mut self,
        listener: &'l mut L,
    ) -> Result<(), ()> {
        extern "C" fn enter<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
            serial: u32,
            surface: *mut ffi::Proxy,
            surface_x: Fixed,
            surface_y: Fixed,
        ) {
            let listener = unsafe { &mut *(data as *mut L) };

            listener.enter(
                unsafe { core::mem::transmute(&mut *pointer) },
                serial,
                unsafe { core::mem::transmute(&mut *surface) },
                surface_x,
                surface_y,
            )
        }
        extern "C" fn leave<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
            serial: u32,
            surface: *mut ffi::Proxy,
        ) {
            unsafe { &mut *(data as *mut L) }.leave(
                unsafe { core::mem::transmute(&mut *pointer) },
                serial,
                unsafe { core::mem::transmute(&mut *surface) },
            )
        }
        extern "C" fn motion<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
            time: u32,
            surface_x: Fixed,
            surface_y: Fixed,
        ) {
            unsafe { &mut *(data as *mut L) }.motion(
                unsafe { core::mem::transmute(&mut *pointer) },
                time,
                surface_x,
                surface_y,
            )
        }
        extern "C" fn button<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
            serial: u32,
            time: u32,
            button: u32,
            state: PointerButtonState,
        ) {
            unsafe { &mut *(data as *mut L) }.button(
                unsafe { core::mem::transmute(&mut *pointer) },
                serial,
                time,
                button,
                state,
            )
        }
        extern "C" fn axis<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
            time: u32,
            axis: u32,
            value: Fixed,
        ) {
            unsafe { &mut *(data as *mut L) }.axis(
                unsafe { core::mem::transmute(&mut *pointer) },
                time,
                axis,
                value,
            )
        }
        extern "C" fn frame<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
        ) {
            unsafe { &mut *(data as *mut L) }.frame(unsafe { core::mem::transmute(&mut *pointer) })
        }
        extern "C" fn axis_source<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
            axis_source: u32,
        ) {
            L::axis_source(
                unsafe { core::mem::transmute(&mut *data) },
                unsafe { core::mem::transmute(&mut *pointer) },
                axis_source,
            )
        }
        extern "C" fn axis_stop<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
            time: u32,
            axis: u32,
        ) {
            L::axis_stop(
                unsafe { core::mem::transmute(&mut *data) },
                unsafe { core::mem::transmute(&mut *pointer) },
                time,
                axis,
            )
        }
        extern "C" fn axis_discrete<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
            axis: u32,
            discrete: i32,
        ) {
            L::axis_discrete(
                unsafe { core::mem::transmute(&mut *data) },
                unsafe { core::mem::transmute(&mut *pointer) },
                axis,
                discrete,
            )
        }
        extern "C" fn axis_value120<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
            axis: u32,
            value120: i32,
        ) {
            L::axis_value120(
                unsafe { core::mem::transmute(&mut *data) },
                unsafe { core::mem::transmute(&mut *pointer) },
                axis,
                value120,
            )
        }
        extern "C" fn axis_relative_direction<L: PointerEventListener>(
            data: *mut core::ffi::c_void,
            pointer: *mut ffi::Proxy,
            axis: u32,
            direction: u32,
        ) {
            L::axis_relative_direction(
                unsafe { core::mem::transmute(&mut *data) },
                unsafe { core::mem::transmute(&mut *pointer) },
                axis,
                direction,
            )
        }

        #[repr(C)]
        struct FunctionPointers {
            enter: extern "C" fn(
                *mut core::ffi::c_void,
                *mut ffi::Proxy,
                u32,
                *mut ffi::Proxy,
                Fixed,
                Fixed,
            ),
            leave: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32, *mut ffi::Proxy),
            motion: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32, Fixed, Fixed),
            button: extern "C" fn(
                *mut core::ffi::c_void,
                *mut ffi::Proxy,
                u32,
                u32,
                u32,
                PointerButtonState,
            ),
            axis: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32, u32, Fixed),
            frame: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy),
            axis_source: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32),
            axis_stop: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32, u32),
            axis_discrete: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32, i32),
            axis_value120: extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32, i32),
            axis_relative_direction:
                extern "C" fn(*mut core::ffi::c_void, *mut ffi::Proxy, u32, u32),
        }
        let fp: &'static FunctionPointers = &FunctionPointers {
            enter: enter::<L>,
            leave: leave::<L>,
            motion: motion::<L>,
            button: button::<L>,
            axis: axis::<L>,
            frame: frame::<L>,
            axis_source: axis_source::<L>,
            axis_stop: axis_stop::<L>,
            axis_discrete: axis_discrete::<L>,
            axis_value120: axis_value120::<L>,
            axis_relative_direction: axis_relative_direction::<L>,
        };
        unsafe {
            self.0
                .add_listener(fp as *const _ as _, listener as *mut _ as _)
        }
    }
}

pub trait PointerEventListener {
    fn enter(
        &mut self,
        pointer: &mut Pointer,
        serial: u32,
        surface: &mut Surface,
        surface_x: Fixed,
        surface_y: Fixed,
    );
    fn leave(&mut self, pointer: &mut Pointer, serial: u32, surface: &mut Surface);
    fn motion(&mut self, pointer: &mut Pointer, time: u32, surface_x: Fixed, surface_y: Fixed);
    fn button(
        &mut self,
        pointer: &mut Pointer,
        serial: u32,
        time: u32,
        button: u32,
        state: PointerButtonState,
    );
    fn axis(&mut self, pointer: &mut Pointer, time: u32, axis: u32, value: Fixed);
    // v5
    fn frame(&mut self, pointer: &mut Pointer);
    fn axis_source(&mut self, pointer: &mut Pointer, axis_source: u32);
    fn axis_stop(&mut self, pointer: &mut Pointer, time: u32, axis: u32);
    fn axis_discrete(&mut self, pointer: &mut Pointer, axis: u32, discrete: i32);
    // v8
    fn axis_value120(&mut self, pointer: &mut Pointer, axis: u32, value120: i32);
    // v9
    fn axis_relative_direction(&mut self, pointer: &mut Pointer, axis: u32, direction: u32);
}

#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PointerButtonState {
    Released = 0,
    Pressed = 1,
}

#[repr(transparent)]
pub struct Output(Proxy);
unsafe impl Interface for Output {
    fn def() -> &'static ffi::Interface {
        unsafe { &wl_callback_interface }
    }
}

// pub trait OutputEventListener {
//     fn geometry(&mut self, output: &mut Output, x: i32, y: i32, physical_width: i32, physical_height: i32, subpixel: i32, make: &core::ffi::CStr, model: &core::ffi::CStr, transform: i32);
//     fn mode(&mut self, output: &mut Output, flags: u32, width: i32, height: i32, refresh: i32);
//     // -- version 2 additions ---
//     fn done(&mut self, output: &mut Output);
//     fn scale(&mut self, output: &mut Output, factor: i32);

// }

#[link(name = "wayland-client")]
unsafe extern "C" {
    static wl_registry_interface: ffi::Interface;
    static wl_compositor_interface: ffi::Interface;
    static wl_surface_interface: ffi::Interface;
    static wl_seat_interface: ffi::Interface;
    static wl_output_interface: ffi::Interface;
    static wl_callback_interface: ffi::Interface;
    static wl_pointer_interface: ffi::Interface;
}

const fn message(
    name: &'static core::ffi::CStr,
    signature: &'static core::ffi::CStr,
    types: &'static [*const ffi::Interface],
) -> ffi::Message {
    ffi::Message {
        name: name.as_ptr(),
        signature: signature.as_ptr(),
        types: types.as_ptr(),
    }
}

const fn interface(
    name: &'static core::ffi::CStr,
    version: core::ffi::c_int,
    methods: &'static [ffi::Message],
    events: &'static [ffi::Message],
) -> ffi::Interface {
    ffi::Interface {
        name: name.as_ptr(),
        version,
        method_count: methods.len() as _,
        methods: methods.as_ptr(),
        event_count: events.len() as _,
        events: events.as_ptr(),
    }
}
