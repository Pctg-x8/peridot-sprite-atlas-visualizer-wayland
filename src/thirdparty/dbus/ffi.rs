//! libdbus-1 ffi
#![allow(non_camel_case_types, dead_code)]

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

FFIOpaqueStruct!(pub struct DBusConnection);
FFIOpaqueStruct!(pub struct DBusMessage);

#[repr(C)]
pub struct DBusMessageIter {
    // Note: ちゃんとサイズ確保してあげる必要がある(使い方参照)
    dummy1: *mut core::ffi::c_void,
    dummy2: *mut core::ffi::c_void,
    dummy3: u32,
    dummy4: core::ffi::c_int,
    dummy5: core::ffi::c_int,
    dummy6: core::ffi::c_int,
    dummy7: core::ffi::c_int,
    dummy8: core::ffi::c_int,
    dummy9: core::ffi::c_int,
    dummy10: core::ffi::c_int,
    dummy11: core::ffi::c_int,
    pad1: core::ffi::c_int,
    pad2: *mut core::ffi::c_void,
    pad3: *mut core::ffi::c_void,
    _marker: core::marker::PhantomData<(core::marker::PhantomPinned, *mut u8)>,
}

FFIOpaqueStruct!(pub struct DBusPendingCall);

#[repr(C)]
pub struct DBusError {
    pub name: *const core::ffi::c_char,
    pub message: *const core::ffi::c_char,
    dummy: core::ffi::c_uint,
    padding1: *mut core::ffi::c_void,
}

#[repr(C)]
pub enum DBusBusType {
    Session,
}

pub type dbus_bool_t = u32;

pub const DBUS_MESSAGE_TYPE_INVALID: core::ffi::c_int = 0;
pub const DBUS_MESSAGE_TYPE_METHOD_CALL: core::ffi::c_int = 1;
pub const DBUS_MESSAGE_TYPE_METHOD_RETURN: core::ffi::c_int = 2;
pub const DBUS_MESSAGE_TYPE_ERROR: core::ffi::c_int = 3;
pub const DBUS_MESSAGE_TYPE_SIGNAL: core::ffi::c_int = 4;

#[link(name = "dbus-1")]
unsafe extern "C" {
    pub unsafe fn dbus_connection_ref(connection: *mut DBusConnection) -> *mut DBusConnection;
    pub unsafe fn dbus_connection_unref(connection: *mut DBusConnection);
    pub unsafe fn dbus_connection_send_with_reply(
        connection: *mut DBusConnection,
        message: *mut DBusMessage,
        pending_return: *mut *mut DBusPendingCall,
        timeout_milliseconds: core::ffi::c_int,
    ) -> u32;

    pub unsafe fn dbus_bus_get(r#type: DBusBusType, error: *mut DBusError) -> *mut DBusConnection;

    pub unsafe fn dbus_message_new_method_call(
        destination: *const core::ffi::c_char,
        path: *const core::ffi::c_char,
        iface: *const core::ffi::c_char,
        method: *const core::ffi::c_char,
    ) -> *mut DBusMessage;
    pub unsafe fn dbus_message_unref(message: *mut DBusMessage);
    pub unsafe fn dbus_message_get_type(message: *mut DBusMessage) -> core::ffi::c_int;
    pub unsafe fn dbus_message_iter_init(
        message: *mut DBusMessage,
        iter: *mut DBusMessageIter,
    ) -> u32;
    pub unsafe fn dbus_message_iter_has_next(iter: *mut DBusMessageIter) -> u32;
    pub unsafe fn dbus_message_iter_get_arg_type(iter: *mut DBusMessageIter) -> core::ffi::c_int;
    pub unsafe fn dbus_message_iter_get_basic(
        iter: *mut DBusMessageIter,
        value: *mut core::ffi::c_void,
    );

    pub unsafe fn dbus_pending_call_unref(pending: *mut DBusPendingCall);
    pub unsafe fn dbus_pending_call_block(pending: *mut DBusPendingCall);
    pub unsafe fn dbus_pending_call_steal_reply(pending: *mut DBusPendingCall) -> *mut DBusMessage;

    pub unsafe fn dbus_error_init(error: *mut DBusError);
    pub unsafe fn dbus_error_free(error: *mut DBusError);
    pub unsafe fn dbus_error_is_set(error: *const DBusError) -> dbus_bool_t;
}
