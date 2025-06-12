pub mod ffi;
pub mod introspect_document;

use core::{
    ffi::CStr,
    fmt::Debug,
    marker::PhantomData,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

pub use self::ffi::DBUS_MESSAGE_TYPE_ERROR as MESSAGE_TYPE_ERROR;
pub use self::ffi::DBUS_MESSAGE_TYPE_INVALID as MESSAGE_TYPE_INVALID;
pub use self::ffi::DBUS_MESSAGE_TYPE_METHOD_CALL as MESSAGE_TYPE_METHOD_CALL;
pub use self::ffi::DBUS_MESSAGE_TYPE_METHOD_RETURN as MESSAGE_TYPE_METHOD_RETURN;
pub use self::ffi::DBUS_MESSAGE_TYPE_SIGNAL as MESSAGE_TYPE_SIGNAL;
pub use self::ffi::DBusBusType as BusType;

#[repr(transparent)]
pub struct Error(ffi::DBusError);
impl Drop for Error {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            ffi::dbus_error_free(&mut self.0);
        }
    }
}
impl Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "DBusError({:?}: {:?})", self.name(), self.message())
    }
}
impl Error {
    pub fn new() -> Self {
        let mut ptr = MaybeUninit::uninit();
        unsafe {
            ffi::dbus_error_init(ptr.as_mut_ptr());
        }

        Self(unsafe { ptr.assume_init() })
    }

    #[inline]
    pub fn reset(&mut self) {
        unsafe {
            ffi::dbus_error_free(&mut self.0);
        }
    }

    #[inline]
    pub fn is_set(&self) -> bool {
        unsafe { ffi::dbus_error_is_set(&self.0) == 1 }
    }

    pub const fn name(&self) -> &CStr {
        unsafe { CStr::from_ptr(self.0.name) }
    }

    pub const fn message(&self) -> &CStr {
        unsafe { CStr::from_ptr(self.0.message) }
    }
}
impl AsRef<ffi::DBusError> for Error {
    #[inline(always)]
    fn as_ref(&self) -> &ffi::DBusError {
        &self.0
    }
}
impl AsMut<ffi::DBusError> for Error {
    #[inline(always)]
    fn as_mut(&mut self) -> &mut ffi::DBusError {
        &mut self.0
    }
}
impl Deref for Error {
    type Target = ffi::DBusError;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for Error {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[repr(transparent)]
pub struct Connection(NonNull<ffi::DBusConnection>);
impl Drop for Connection {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            ffi::dbus_connection_unref(self.0.as_ptr());
        }
    }
}
impl Clone for Connection {
    #[inline]
    fn clone(&self) -> Self {
        Self(
            NonNull::new(unsafe { ffi::dbus_connection_ref(self.0.as_ptr()) })
                .expect("dbus_connection_ref failed"),
        )
    }
}
impl Connection {
    pub const unsafe fn from_ptr(p: NonNull<ffi::DBusConnection>) -> Self {
        Self(p)
    }

    pub const fn as_ptr(&self) -> *mut ffi::DBusConnection {
        self.0.as_ptr()
    }

    pub unsafe fn clone_unchecked(&self) -> Self {
        Self(unsafe { NonNull::new_unchecked(ffi::dbus_connection_ref(self.0.as_ptr())) })
    }

    pub fn connect_bus(ty: BusType) -> Result<Self, Error> {
        let mut e = Error::new();
        let r = unsafe { ffi::dbus_bus_get(ty, e.as_mut()) };

        match NonNull::new(r) {
            Some(r) => Ok(unsafe { Self::from_ptr(r) }),
            None => Err(e),
        }
    }

    pub fn send_with_reply(
        &mut self,
        message: &mut Message,
        timeout_milliseconds: Option<core::ffi::c_int>,
    ) -> Option<PendingCall> {
        let mut pc = core::mem::MaybeUninit::uninit();
        let r = unsafe {
            ffi::dbus_connection_send_with_reply(
                self.0.as_ptr(),
                message.0.as_ptr(),
                pc.as_mut_ptr(),
                timeout_milliseconds.unwrap_or(-1),
            )
        };

        if r == 0 {
            None
        } else {
            Some(PendingCall(NonNull::new(unsafe { pc.assume_init() })?))
        }
    }
}

const fn opt_cstr_ptr(a: Option<&CStr>) -> *const core::ffi::c_char {
    match a {
        Some(x) => x.as_ptr(),
        None => core::ptr::null(),
    }
}

#[repr(transparent)]
pub struct Message<'a>(NonNull<ffi::DBusMessage>, PhantomData<&'a CStr>);
impl Drop for Message<'_> {
    fn drop(&mut self) {
        unsafe {
            ffi::dbus_message_unref(self.0.as_ptr());
        }
    }
}
impl<'a> Message<'a> {
    pub fn new_method_call(
        destination: Option<&'a CStr>,
        path: &'a CStr,
        iface: Option<&'a CStr>,
        method: &'a CStr,
    ) -> Option<Self> {
        Some(Self(
            NonNull::new(unsafe {
                ffi::dbus_message_new_method_call(
                    opt_cstr_ptr(destination),
                    path.as_ptr(),
                    opt_cstr_ptr(iface),
                    method.as_ptr(),
                )
            })?,
            PhantomData,
        ))
    }

    pub fn r#type(&self) -> core::ffi::c_int {
        unsafe { ffi::dbus_message_get_type(self.0.as_ptr()) }
    }

    pub fn iter<'m>(&'m self) -> MessageIter<'m, 'a> {
        let mut iter = MaybeUninit::uninit();
        unsafe {
            ffi::dbus_message_iter_init(self.0.as_ptr(), iter.as_mut_ptr());
        }

        MessageIter(unsafe { iter.assume_init() }, PhantomData)
    }
}

#[repr(transparent)]
pub struct MessageIter<'m, 'a>(ffi::DBusMessageIter, PhantomData<&'m Message<'a>>);
impl MessageIter<'_, '_> {
    pub fn has_next(&mut self) -> bool {
        unsafe { ffi::dbus_message_iter_has_next(&mut self.0) != 0 }
    }

    pub fn arg_type(&mut self) -> core::ffi::c_int {
        unsafe { ffi::dbus_message_iter_get_arg_type(&mut self.0) }
    }

    pub unsafe fn get_value_basic(&mut self, sink: *mut core::ffi::c_void) {
        unsafe { ffi::dbus_message_iter_get_basic(&mut self.0, sink) }
    }
}

#[repr(transparent)]
pub struct PendingCall(NonNull<ffi::DBusPendingCall>);
impl Drop for PendingCall {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            ffi::dbus_pending_call_unref(self.0.as_ptr());
        }
    }
}
impl PendingCall {
    #[inline]
    pub fn block(&mut self) {
        unsafe {
            ffi::dbus_pending_call_block(self.0.as_ptr());
        }
    }

    #[inline]
    pub fn steal_reply(&mut self) -> Option<Message> {
        Some(Message(
            NonNull::new(unsafe { ffi::dbus_pending_call_steal_reply(self.0.as_ptr()) })?,
            core::marker::PhantomData,
        ))
    }
}
