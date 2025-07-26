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

pub type DBusObjectPathUnregisterFunction =
    extern "C" fn(connection: *mut DBusConnection, user_data: *mut core::ffi::c_void);
pub type DBusObjectPathMessageFunction = extern "C" fn(
    connection: *mut DBusConnection,
    message: *mut DBusMessage,
    user_data: *mut core::ffi::c_void,
) -> core::ffi::c_int;
type DBusObjectPathInternalFunction = extern "C" fn(*mut core::ffi::c_void);

#[repr(C)]
pub struct DBusObjectPathVTable {
    pub unregister_function: DBusObjectPathUnregisterFunction,
    pub message_function: DBusObjectPathMessageFunction,
    dbus_internal_pad1: DBusObjectPathInternalFunction,
    dbus_internal_pad2: DBusObjectPathInternalFunction,
    dbus_internal_pad3: DBusObjectPathInternalFunction,
    dbus_internal_pad4: DBusObjectPathInternalFunction,
}

FFIOpaqueStruct!(pub struct DBusWatch);

#[repr(C)]
pub enum DBusBusType {
    Session,
}

pub type dbus_bool_t = u32;

pub type DBusAddWatchFunction =
    Option<extern "C" fn(watch: *mut DBusWatch, data: *mut core::ffi::c_void) -> dbus_bool_t>;
pub type DBusRemoveWatchFunction =
    Option<extern "C" fn(watch: *mut DBusWatch, data: *mut core::ffi::c_void)>;
pub type DBusWatchToggledFunction =
    Option<extern "C" fn(watch: *mut DBusWatch, data: *mut core::ffi::c_void)>;
pub type DBusFreeFunction = Option<extern "C" fn(memory: *mut core::ffi::c_void)>;

pub const DBUS_MESSAGE_TYPE_INVALID: core::ffi::c_int = 0;
pub const DBUS_MESSAGE_TYPE_METHOD_CALL: core::ffi::c_int = 1;
pub const DBUS_MESSAGE_TYPE_METHOD_RETURN: core::ffi::c_int = 2;
pub const DBUS_MESSAGE_TYPE_ERROR: core::ffi::c_int = 3;
pub const DBUS_MESSAGE_TYPE_SIGNAL: core::ffi::c_int = 4;

pub const DBUS_TYPE_INVALID: core::ffi::c_int = 0;
pub const DBUS_TYPE_BOOLEAN: core::ffi::c_int = b'b' as _;
pub const DBUS_TYPE_STRING: core::ffi::c_int = b's' as _;
pub const DBUS_TYPE_OBJECT_PATH: core::ffi::c_int = b'o' as _;
pub const DBUS_TYPE_ARRAY: core::ffi::c_int = b'a' as _;
pub const DBUS_TYPE_VARIANT: core::ffi::c_int = b'v' as _;
pub const DBUS_TYPE_DICT_ENTRY: core::ffi::c_int = b'e' as _;
pub const DBUS_TYPE_STRUCT: core::ffi::c_int = b'r' as _;
pub const DBUS_TYPE_UINT: core::ffi::c_int = b'u' as _;

pub type DBusWatchFlags = core::ffi::c_uint;
pub const DBUS_WATCH_READABLE: DBusWatchFlags = 1 << 0;
pub const DBUS_WATCH_WRITABLE: DBusWatchFlags = 1 << 1;
pub const DBUS_WATCH_ERROR: DBusWatchFlags = 1 << 2;
pub const DBUS_WATCH_HANGUP: DBusWatchFlags = 1 << 3;

#[link(name = "dbus-1")]
unsafe extern "C" {
    pub unsafe fn dbus_connection_ref(connection: *mut DBusConnection) -> *mut DBusConnection;
    pub unsafe fn dbus_connection_unref(connection: *mut DBusConnection);
    pub unsafe fn dbus_connection_send(
        connection: *mut DBusConnection,
        message: *mut DBusMessage,
        serial: *mut u32,
    ) -> dbus_bool_t;
    pub unsafe fn dbus_connection_send_with_reply(
        connection: *mut DBusConnection,
        message: *mut DBusMessage,
        pending_return: *mut *mut DBusPendingCall,
        timeout_milliseconds: core::ffi::c_int,
    ) -> u32;
    pub unsafe fn dbus_connection_get_dispatch_status(
        connection: *mut DBusConnection,
    ) -> core::ffi::c_int;
    pub unsafe fn dbus_connection_pop_message(connection: *mut DBusConnection) -> *mut DBusMessage;
    pub unsafe fn dbus_connection_dispatch(connection: *mut DBusConnection) -> core::ffi::c_int;
    pub unsafe fn dbus_connection_read_write(
        connection: *mut DBusConnection,
        timeout_seconds: core::ffi::c_int,
    ) -> dbus_bool_t;
    pub unsafe fn dbus_connection_set_watch_functions(
        connection: *mut DBusConnection,
        add_function: DBusAddWatchFunction,
        remove_function: DBusRemoveWatchFunction,
        toggled_function: DBusWatchToggledFunction,
        data: *mut core::ffi::c_void,
        free_data_function: DBusFreeFunction,
    ) -> dbus_bool_t;
    pub unsafe fn dbus_connection_try_register_object_path(
        connection: *mut DBusConnection,
        path: *const core::ffi::c_char,
        vtable: *const DBusObjectPathVTable,
        user_data: *mut core::ffi::c_void,
        error: *mut DBusError,
    ) -> dbus_bool_t;
    pub unsafe fn dbus_connection_unregister_object_path(
        connection: *mut DBusConnection,
        path: *const core::ffi::c_char,
    ) -> dbus_bool_t;

    pub unsafe fn dbus_bus_get(r#type: DBusBusType, error: *mut DBusError) -> *mut DBusConnection;
    pub unsafe fn dbus_bus_get_unique_name(
        connection: *mut DBusConnection,
    ) -> *const core::ffi::c_char;

    pub unsafe fn dbus_message_new_method_call(
        destination: *const core::ffi::c_char,
        path: *const core::ffi::c_char,
        iface: *const core::ffi::c_char,
        method: *const core::ffi::c_char,
    ) -> *mut DBusMessage;
    pub unsafe fn dbus_message_ref(message: *mut DBusMessage) -> *mut DBusMessage;
    pub unsafe fn dbus_message_unref(message: *mut DBusMessage);
    pub unsafe fn dbus_message_get_type(message: *mut DBusMessage) -> core::ffi::c_int;
    pub unsafe fn dbus_message_iter_init(
        message: *mut DBusMessage,
        iter: *mut DBusMessageIter,
    ) -> u32;
    pub unsafe fn dbus_message_iter_get_signature(
        iter: *mut DBusMessageIter,
    ) -> *mut core::ffi::c_char;
    pub unsafe fn dbus_message_iter_has_next(iter: *mut DBusMessageIter) -> dbus_bool_t;
    pub unsafe fn dbus_message_iter_next(iter: *mut DBusMessageIter) -> dbus_bool_t;
    pub unsafe fn dbus_message_iter_get_arg_type(iter: *mut DBusMessageIter) -> core::ffi::c_int;
    pub unsafe fn dbus_message_iter_get_basic(
        iter: *mut DBusMessageIter,
        value: *mut core::ffi::c_void,
    );
    pub unsafe fn dbus_message_iter_recurse(iter: *mut DBusMessageIter, sub: *mut DBusMessageIter);
    pub unsafe fn dbus_message_iter_init_append(
        message: *mut DBusMessage,
        iter: *mut DBusMessageIter,
    );
    pub unsafe fn dbus_message_iter_append_basic(
        iter: *mut DBusMessageIter,
        r#type: core::ffi::c_int,
        value: *const core::ffi::c_void,
    ) -> dbus_bool_t;
    pub unsafe fn dbus_message_iter_open_container(
        iter: *mut DBusMessageIter,
        r#type: core::ffi::c_int,
        contained_signature: *const core::ffi::c_char,
        sub: *mut DBusMessageIter,
    ) -> dbus_bool_t;
    pub unsafe fn dbus_message_iter_close_container(
        iter: *mut DBusMessageIter,
        sub: *mut DBusMessageIter,
    ) -> dbus_bool_t;
    pub unsafe fn dbus_message_iter_abandon_container(
        iter: *mut DBusMessageIter,
        sub: *mut DBusMessageIter,
    ) -> dbus_bool_t;
    pub unsafe fn dbus_set_error_from_message(
        error: *mut DBusError,
        message: *mut DBusMessage,
    ) -> dbus_bool_t;
    pub unsafe fn dbus_message_get_path(message: *mut DBusMessage) -> *const core::ffi::c_char;
    pub unsafe fn dbus_message_get_interface(message: *mut DBusMessage)
    -> *const core::ffi::c_char;
    pub unsafe fn dbus_message_get_member(message: *mut DBusMessage) -> *const core::ffi::c_char;
    pub unsafe fn dbus_message_get_signature(message: *mut DBusMessage)
    -> *const core::ffi::c_char;
    pub unsafe fn dbus_message_get_serial(message: *mut DBusMessage) -> u32;
    pub unsafe fn dbus_message_get_reply_serial(message: *mut DBusMessage) -> u32;

    pub unsafe fn dbus_pending_call_unref(pending: *mut DBusPendingCall);
    pub unsafe fn dbus_pending_call_block(pending: *mut DBusPendingCall);
    pub unsafe fn dbus_pending_call_steal_reply(pending: *mut DBusPendingCall) -> *mut DBusMessage;

    pub unsafe fn dbus_watch_get_unix_fd(watch: *const DBusWatch) -> core::ffi::c_int;
    pub unsafe fn dbus_watch_get_flags(watch: *const DBusWatch) -> DBusWatchFlags;
    pub unsafe fn dbus_watch_get_enabled(watch: *const DBusWatch) -> dbus_bool_t;
    pub unsafe fn dbus_watch_handle(watch: *mut DBusWatch, flags: DBusWatchFlags) -> dbus_bool_t;

    pub unsafe fn dbus_error_init(error: *mut DBusError);
    pub unsafe fn dbus_error_free(error: *mut DBusError);
    pub unsafe fn dbus_error_is_set(error: *const DBusError) -> dbus_bool_t;

    pub unsafe fn dbus_free(memory: *mut core::ffi::c_void);
}
