use dbus::MessageIterAppendLike;

pub const fn cstr2str<'s>(x: &'s core::ffi::CStr) -> &'s str {
    // d-bus strings can be always assumed as valid UTF-8 sequence
    unsafe { str::from_utf8_unchecked(x.to_bytes()) }
}

pub fn introspect(
    dbus: &dbus::Connection,
    dest: Option<&core::ffi::CStr>,
    path: &core::ffi::CStr,
) -> u32 {
    dbus.send_with_serial(
        &mut dbus::Message::new_method_call(
            dest,
            path,
            Some(c"org.freedesktop.DBus.Introspectable"),
            c"Introspect",
        )
        .expect("no enough memory"),
    )
    .expect("no enough memory")
}

pub fn properties_get(
    dbus: &dbus::Connection,
    dest: Option<&core::ffi::CStr>,
    path: &core::ffi::CStr,
    interface: &core::ffi::CStr,
    name: &core::ffi::CStr,
) -> u32 {
    let mut msg = dbus::Message::new_method_call(
        dest,
        path,
        Some(c"org.freedesktop.DBus.Properties"),
        c"Get",
    )
    .expect("no enough memory");
    let mut msg_args_appender = msg.iter_append();
    msg_args_appender
        .append_cstr(interface)
        .expect("no enough memory");
    msg_args_appender
        .append_cstr(name)
        .expect("no enough memory");
    drop(msg_args_appender);

    dbus.send_with_serial(&mut msg).expect("no enough memory")
}
