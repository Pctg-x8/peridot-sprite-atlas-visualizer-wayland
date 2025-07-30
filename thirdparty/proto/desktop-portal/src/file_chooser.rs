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
    options_builder: impl FnOnce(OpenFileOptionsAppender),
) -> u32 {
    let mut msg = dbus::Message::new_method_call(
        Some(c"org.freedesktop.portal.Desktop"),
        c"/org/freedesktop/portal/desktop",
        Some(c"org.freedesktop.portal.FileChooser"),
        c"OpenFile",
    )
    .unwrap();
    let mut msg_args_appender = msg.iter_append();
    msg_args_appender
        .append_cstr(parent_window.unwrap_or(c""))
        .expect("no enough memory");
    msg_args_appender
        .append_cstr(title)
        .expect("no enough memory");
    let mut options_appender = msg_args_appender.open_array_container(c"{sv}").unwrap();
    options_builder(OpenFileOptionsAppender(&mut options_appender));
    options_appender.close().expect("no enough memory");

    dbus.send_with_serial(&mut msg).expect("no enough memory")
}

#[repr(transparent)]
pub struct OpenFileOptionsAppender<'a, 'm>(
    &'a mut dbus::MessageIterAppendContainer<'m, dbus::MessageIterAppend<'m>>,
);
impl OpenFileOptionsAppender<'_, '_> {
    pub fn append_handle_token(&mut self, value: &core::ffi::CStr) {
        let mut dict_appender = self.0.open_dict_entry_container().unwrap();
        dict_appender.append_cstr(c"handle_token").unwrap();
        dict_appender.append_variant_cstr(value).unwrap();
        dict_appender.close().unwrap();
    }

    pub fn append_multiple(&mut self, value: bool) {
        let mut dict_appender = self.0.open_dict_entry_container().unwrap();
        dict_appender.append_cstr(c"multiple").unwrap();
        dict_appender.append_variant_bool(value).unwrap();
        dict_appender.close().unwrap();
    }

    pub fn append_filters<'fname>(
        &mut self,
        filters: impl IntoIterator<Item = (&'fname core::ffi::CStr, impl IntoIterator<Item = Filter>)>,
    ) {
        let mut dict_appender = self.0.open_dict_entry_container().unwrap();
        dict_appender.append_cstr(c"filters").unwrap();
        let mut values_variant_appender =
            dict_appender.open_variant_container(c"a(sa(us))").unwrap();
        let mut values_appender = values_variant_appender
            .open_array_container(c"(sa(us))")
            .unwrap();
        for (name, filters) in filters {
            let mut pair_appender = values_appender.open_struct_container().unwrap();
            pair_appender.append_cstr(name).unwrap();
            let mut filters_appender = pair_appender.open_array_container(c"(us)").unwrap();
            for f in filters {
                let mut content_appender = filters_appender.open_struct_container().unwrap();
                match f {
                    Filter::Glob(x) => {
                        content_appender.append_u32(0).unwrap();
                        content_appender.append_cstr(&x).unwrap();
                    }
                    Filter::MIME(x) => {
                        content_appender.append_u32(1).unwrap();
                        content_appender.append_cstr(&x).unwrap();
                    }
                    Filter::Unknown(n, x) => {
                        content_appender.append_u32(n).unwrap();
                        content_appender.append_cstr(&x).unwrap();
                    }
                }
                content_appender.close().unwrap();
            }
            filters_appender.close().unwrap();
            pair_appender.close().unwrap();
        }
        values_appender.close().unwrap();
        values_variant_appender.close().unwrap();
        dict_appender.close().unwrap();
    }

    pub fn append_current_filter(
        &mut self,
        filter_name: &core::ffi::CStr,
        filters: impl IntoIterator<Item = Filter>,
    ) {
        let mut dict_appender = self.0.open_dict_entry_container().unwrap();
        dict_appender.append_cstr(c"current_filter").unwrap();
        let mut value_variant_appender = dict_appender.open_variant_container(c"(sa(us))").unwrap();
        let mut value_pair_appender = value_variant_appender.open_struct_container().unwrap();
        value_pair_appender.append_cstr(filter_name).unwrap();
        let mut filters_appender = value_pair_appender.open_array_container(c"(us)").unwrap();
        for f in filters {
            let mut content_appender = filters_appender.open_struct_container().unwrap();
            match f {
                Filter::Glob(x) => {
                    content_appender.append_u32(0).unwrap();
                    content_appender.append_cstr(&x).unwrap();
                }
                Filter::MIME(x) => {
                    content_appender.append_u32(1).unwrap();
                    content_appender.append_cstr(&x).unwrap();
                }
                Filter::Unknown(n, x) => {
                    content_appender.append_u32(n).unwrap();
                    content_appender.append_cstr(&x).unwrap();
                }
            }
            content_appender.close().unwrap();
        }
        filters_appender.close().unwrap();
        value_pair_appender.close().unwrap();
        value_variant_appender.close().unwrap();
        dict_appender.close().unwrap();
    }
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
    options_builder: impl FnOnce(SaveFileOptionsAppender),
) -> u32 {
    let mut msg = dbus::Message::new_method_call(
        Some(c"org.freedesktop.portal.Desktop"),
        c"/org/freedesktop/portal/desktop",
        Some(c"org.freedesktop.portal.FileChooser"),
        c"SaveFile",
    )
    .unwrap();
    let mut msg_args_appender = msg.iter_append();
    msg_args_appender
        .append_cstr(parent_window.unwrap_or(c""))
        .expect("no enough memory");
    msg_args_appender
        .append_cstr(title)
        .expect("no enough memory");
    let mut options_appender = msg_args_appender.open_array_container(c"{sv}").unwrap();
    options_builder(SaveFileOptionsAppender(&mut options_appender));
    options_appender.close().expect("no enough memory");

    dbus.send_with_serial(&mut msg).expect("no enough memory")
}

#[repr(transparent)]
pub struct SaveFileOptionsAppender<'a, 'm>(
    &'a mut dbus::MessageIterAppendContainer<'m, dbus::MessageIterAppend<'m>>,
);
impl SaveFileOptionsAppender<'_, '_> {
    pub fn append_handle_token(&mut self, value: &core::ffi::CStr) {
        let mut dict_appender = self
            .0
            .open_dict_entry_container()
            .expect("no enough memory");
        dict_appender
            .append_cstr(c"handle_token")
            .expect("no enough memory");
        dict_appender
            .append_variant_cstr(value)
            .expect("no enough memory");
        dict_appender.close().expect("no enough memory");
    }

    pub fn append_filters<'fname>(
        &mut self,
        filters: impl IntoIterator<Item = (&'fname core::ffi::CStr, impl IntoIterator<Item = Filter>)>,
    ) {
        let mut dict_appender = self.0.open_dict_entry_container().unwrap();
        dict_appender.append_cstr(c"filters").unwrap();
        let mut values_variant_appender =
            dict_appender.open_variant_container(c"a(sa(us))").unwrap();
        let mut values_appender = values_variant_appender
            .open_array_container(c"(sa(us))")
            .unwrap();
        for (name, filters) in filters {
            let mut pair_appender = values_appender.open_struct_container().unwrap();
            pair_appender.append_cstr(name).unwrap();
            let mut filters_appender = pair_appender.open_array_container(c"(us)").unwrap();
            for f in filters {
                let mut content_appender = filters_appender.open_struct_container().unwrap();
                match f {
                    Filter::Glob(x) => {
                        content_appender.append_u32(0).unwrap();
                        content_appender.append_cstr(&x).unwrap();
                    }
                    Filter::MIME(x) => {
                        content_appender.append_u32(1).unwrap();
                        content_appender.append_cstr(&x).unwrap();
                    }
                    Filter::Unknown(n, x) => {
                        content_appender.append_u32(n).unwrap();
                        content_appender.append_cstr(&x).unwrap();
                    }
                }
                content_appender.close().unwrap();
            }
            filters_appender.close().unwrap();
            pair_appender.close().unwrap();
        }
        values_appender.close().unwrap();
        values_variant_appender.close().unwrap();
        dict_appender.close().unwrap();
    }
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

#[derive(Debug, Clone)]
pub enum Filter {
    Glob(std::ffi::CString),
    MIME(std::ffi::CString),
    Unknown(u32, std::ffi::CString),
}
#[derive(Debug, Clone)]
pub struct CurrentFilterResponse {
    pub name: std::ffi::CString,
    pub filters: Vec<Filter>,
}
impl CurrentFilterResponse {
    pub fn read_all(value_content_iter: &mut dbus::MessageIter) -> Self {
        let mut struct_iter = value_content_iter
            .try_begin_iter_struct_content()
            .expect("invalid current_filter value content");
        let filter_name = struct_iter
            .try_get_cstr()
            .expect("unexpected filter name value")
            .to_owned();
        struct_iter.next();
        let mut array_iter = struct_iter
            .try_begin_iter_array_content()
            .expect("invalid current_filter value content");
        let mut filters = Vec::new();
        while array_iter.arg_type() != dbus::TYPE_INVALID {
            let mut struct_iter = array_iter
                .try_begin_iter_struct_content()
                .expect("invalid current_filter value content element");
            let v = struct_iter.try_get_u32().expect("unexpected type");
            struct_iter.next();
            let f = struct_iter.try_get_cstr().expect("unexpected filter value");
            filters.push(match v {
                0 => Filter::Glob(f.into()),
                1 => Filter::MIME(f.into()),
                x => Filter::Unknown(x, f.into()),
            });
            drop(struct_iter);

            array_iter.next();
        }

        Self {
            name: filter_name,
            filters,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResponseResults {
    pub uris: Vec<std::ffi::CString>,
    pub current_filter: Option<CurrentFilterResponse>,
}
impl ResponseResults {
    pub fn read_all(msg_iter: &mut dbus::MessageIter) -> Self {
        let mut results_iter = msg_iter
            .try_begin_iter_array_content()
            .expect("invalid response results");
        let mut data = ResponseResults {
            uris: Vec::new(),
            current_filter: None,
        };
        while results_iter.arg_type() != dbus::TYPE_INVALID {
            let mut kv_iter = results_iter
                .try_begin_iter_dict_entry_content()
                .expect("invalid results kv pair");

            match kv_iter.try_get_cstr().expect("unexpected key value") {
                x if x == c"uris" => {
                    kv_iter.next();

                    let mut value_iter = kv_iter
                        .try_begin_iter_variant_content()
                        .expect("invalid uris value");
                    let mut iter = value_iter
                        .try_begin_iter_array_content()
                        .expect("invalid uris value content");
                    while iter.arg_type() != dbus::TYPE_INVALID {
                        data.uris.push(std::ffi::CString::from(
                            iter.try_get_cstr().expect("unexpected uris value"),
                        ));
                        iter.next();
                    }
                }
                x if x == c"choices" => {
                    kv_iter.next();

                    let mut value_iter = kv_iter
                        .try_begin_iter_variant_content()
                        .expect("invalid choices value");
                    let mut iter = value_iter
                        .try_begin_iter_array_content()
                        .expect("invalid choices value content");
                    while iter.arg_type() != dbus::TYPE_INVALID {
                        let mut elements_iter = iter
                            .try_begin_iter_struct_content()
                            .expect("invalid choices value content element");
                        let key = elements_iter
                            .try_get_cstr()
                            .expect("unexpected key value")
                            .to_owned();
                        elements_iter.next();
                        let value = elements_iter
                            .try_get_cstr()
                            .expect("unexpected value")
                            .to_owned();
                        println!("choices {key:?} -> {value:?}");
                        drop(elements_iter);

                        iter.next();
                    }
                }
                x if x == c"current_filter" => {
                    if data.current_filter.is_some() {
                        panic!("current_filter received twice");
                    }

                    kv_iter.next();
                    let mut value_iter = kv_iter
                        .try_begin_iter_variant_content()
                        .expect("invalid content_filter value");
                    data.current_filter = Some(CurrentFilterResponse::read_all(&mut value_iter));
                }
                c => unreachable!("unexpected result entry: {c:?}"),
            }

            results_iter.next();
        }

        data
    }
}

#[inline(always)]
pub fn uri_path_part(uri: &str) -> &str {
    // desktop-portal file chooser returns uri strings that should start with "file://"(by protocol)
    debug_assert!(uri.starts_with("file://"));
    unsafe { uri.strip_prefix("file://").unwrap_unchecked() }
}
