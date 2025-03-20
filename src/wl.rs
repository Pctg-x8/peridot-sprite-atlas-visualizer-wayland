use core::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

mod ffi;
mod xdg_shell;
use ffi::wl_proxy_destroy;

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
pub struct Proxy(ffi::Proxy);
impl Proxy {
    pub const unsafe fn from_raw_ptr_unchecked<'a>(ptr: *mut ffi::Proxy) -> &'a mut Self {
        unsafe { Self::from_raw_ref_mut(&mut *ptr) }
    }

    pub const unsafe fn from_raw_ref<'a>(r: &'a ffi::Proxy) -> &'a Self {
        unsafe { core::mem::transmute(r) }
    }

    pub const unsafe fn from_raw_ref_mut<'a>(r: &'a mut ffi::Proxy) -> &'a mut Self {
        unsafe { core::mem::transmute(r) }
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
            unsafe { ffi::wl_proxy_add_listener(self as *mut _ as _, function_table, user_data) };

        if r == 0 { Ok(()) } else { Err(()) }
    }

    #[inline]
    fn marshal_array_flags(
        &mut self,
        opcode: u32,
        interface: &ffi::Interface,
        version: u32,
        flags: u32,
        args: &mut [ffi::Argument],
    ) -> Result<NonNull<Proxy>, std::io::Error> {
        unsafe {
            NonNull::new(ffi::wl_proxy_marshal_array_flags(
                self as *mut _ as _,
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
        &mut self,
        opcode: u32,
        flags: u32,
        args: &mut [ffi::Argument],
    ) -> Result<(), std::io::Error> {
        unsafe {
            ffi::wl_proxy_marshal_array_flags(
                self as *mut _ as _,
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

        let e =
            unsafe { ffi::wl_display_get_error(ffi::wl_proxy_get_display(self as *mut _ as _)) };
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
}

pub struct Display {
    ffi: NonNull<ffi::Display>,
}
impl Drop for Display {
    fn drop(&mut self) {
        unsafe { ffi::wl_display_disconnect(self.ffi.as_ptr()) }
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
    pub fn get_registry(&mut self) -> Result<Owned<Registry>, std::io::Error> {
        let proxy_ptr = unsafe {
            Proxy::from_raw_ref_mut(core::mem::transmute(self.ffi.as_mut())).marshal_array_flags(
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
    pub fn roundtrip(&mut self) -> Result<u32, ()> {
        let r = unsafe { ffi::wl_display_roundtrip(self.ffi.as_ptr()) };

        if r < 0 { Err(()) } else { Ok(r as _) }
    }

    #[inline]
    pub fn error(&mut self) -> Option<core::ffi::c_int> {
        let r = unsafe { ffi::wl_display_get_error(self.ffi.as_ptr()) };

        if r == 0 { None } else { Some(r) }
    }

    #[inline]
    pub fn flush(&mut self) -> Result<core::ffi::c_int, std::io::Error> {
        let r = unsafe { ffi::wl_display_flush(self.ffi.as_ptr()) };

        if r == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(r)
        }
    }

    #[inline]
    pub fn dispatch(&mut self) -> Result<core::ffi::c_int, std::io::Error> {
        let r = unsafe { ffi::wl_display_dispatch(self.ffi.as_ptr()) };

        if r == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(r)
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

    pub fn bind<I: Interface>(
        &mut self,
        name: u32,
        version: u32,
    ) -> Result<Owned<I>, std::io::Error> {
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
    pub fn create_surface(&mut self) -> Result<Owned<Surface>, std::io::Error> {
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
    pub fn frame(&mut self) -> Result<Owned<Callback>, std::io::Error> {
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
    pub fn commit(&mut self) -> Result<(), std::io::Error> {
        self.0.marshal_array_flags_void(6, 0, &mut [])
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
