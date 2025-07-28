use dbus::MessageIterAppendLike;

use crate::ObjectPath;

pub fn get_version(dbus: &dbus::Connection) -> u32 {
    dbus_proto::properties_get(
        dbus,
        Some(c"org.freedesktop.portal.Desktop"),
        c"/org/freedesktop/portal/desktop",
        c"org.freedesktop.portal.FileChooser",
        c"version",
    )
}

pub fn read_get_version_reply(msg: dbus::Message) -> Result<u32, dbus::Error> {
    if let Some(e) = msg.try_get_error() {
        return Err(e);
    }

    let mut reply_iter = msg.iter();
    Ok(reply_iter
        .try_begin_iter_variant_content()
        .expect("property must returns variant")
        .try_get_u32()
        .expect("unexpected version value"))
}

/// https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html#org-freedesktop-portal-filechooser-openfile
pub fn open_file(
    dbus: &dbus::Connection,
    parent_window: Option<&core::ffi::CStr>,
    title: &core::ffi::CStr,
    options_builder: impl FnOnce(&mut dbus::MessageIterAppendContainer<dbus::MessageIterAppend>),
) -> u32 {
    let mut msg = dbus::Message::new_method_call(
        Some(c"org.freedesktop.portal.Desktop"),
        c"/org/freedesktop/portal/desktop",
        Some(c"org.freedesktop.portal.FileChooser"),
        c"OpenFile",
    )
    .unwrap();
    let mut msg_args_appender = msg.iter_append();
    msg_args_appender.append_cstr(parent_window.unwrap_or(c""));
    msg_args_appender.append_cstr(title);
    let mut options_appender = msg_args_appender.open_array_container(c"{sv}").unwrap();
    options_builder(&mut options_appender);
    options_appender.close();

    dbus.send_with_serial(&mut msg).expect("no enough memory")
}

pub fn read_open_file_reply(msg: dbus::Message) -> Result<ObjectPath, dbus::Error> {
    if let Some(e) = msg.try_get_error() {
        return Err(e);
    }

    let msg_iter = msg.iter();
    let handle = ObjectPath(
        msg_iter
            .try_get_object_path()
            .expect("invalid response")
            .into(),
    );
    debug_assert!(!msg_iter.has_next(), "reply data left");

    Ok(handle)
}

/// https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html#org-freedesktop-portal-filechooser-savefile
pub fn save_file(
    dbus: &dbus::Connection,
    parent_window: Option<&core::ffi::CStr>,
    title: &core::ffi::CStr,
    options_builder: impl FnOnce(&mut dbus::MessageIterAppendContainer<dbus::MessageIterAppend>),
) -> u32 {
    let mut msg = dbus::Message::new_method_call(
        Some(c"org.freedesktop.portal.Desktop"),
        c"/org/freedesktop/portal/desktop",
        Some(c"org.freedesktop.portal.FileChooser"),
        c"SaveFile",
    )
    .unwrap();
    let mut msg_args_appender = msg.iter_append();
    msg_args_appender.append_cstr(parent_window.unwrap_or(c""));
    msg_args_appender.append_cstr(title);
    let mut options_appender = msg_args_appender.open_array_container(c"{sv}").unwrap();
    options_builder(&mut options_appender);
    options_appender.close();

    dbus.send_with_serial(&mut msg).expect("no enough memory")
}

pub fn read_save_file_reply(msg: dbus::Message) -> Result<ObjectPath, dbus::Error> {
    if let Some(e) = msg.try_get_error() {
        return Err(e);
    }

    let msg_iter = msg.iter();
    let handle = ObjectPath(
        msg_iter
            .try_get_object_path()
            .expect("invalid response")
            .into(),
    );
    debug_assert!(!msg_iter.has_next(), "reply data left");

    Ok(handle)
}
