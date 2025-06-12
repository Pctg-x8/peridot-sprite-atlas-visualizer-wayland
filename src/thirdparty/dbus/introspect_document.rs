use std::borrow::Cow;

pub enum InterfaceElementContent<'a> {
    Method {
        name: Cow<'a, [u8]>,
        empty: bool,
    },
    Signal {
        name: Cow<'a, [u8]>,
        empty: bool,
    },
    Property {
        name: Cow<'a, [u8]>,
        r#type: Cow<'a, [u8]>,
        access: Cow<'a, [u8]>,
    },
}
pub enum MethodSignalElementContent<'a> {
    Arg {
        name: Cow<'a, [u8]>,
        r#type: Cow<'a, [u8]>,
        direction: Option<Cow<'a, [u8]>>,
    },
    Annotation {
        name: Cow<'a, [u8]>,
        value: Cow<'a, [u8]>,
    },
}

pub fn read_toplevel<'a>(
    reader: &mut quick_xml::Reader<&'a [u8]>,
    mut callback: impl for<'b> FnMut(
        &[Option<Result<String, quick_xml::Error>>],
        Cow<'b, [u8]>,
        &mut quick_xml::Reader<&'a [u8]>,
    ),
) {
    let mut node_paths = Vec::new();

    loop {
        match reader.read_event().unwrap() {
            quick_xml::events::Event::Start(x) if x.name().0 == b"node" => {
                let mut name = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.unescape_value().map(Into::into));
                    }
                }

                node_paths.push(name);
            }
            quick_xml::events::Event::Start(x) if x.name().0 == b"interface" => {
                let mut name = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.value);
                    }
                }

                callback(
                    &node_paths,
                    name.expect("no name attr in interface tag"),
                    reader,
                );
            }
            quick_xml::events::Event::End(x) if x.name().0 == b"node" => {
                if node_paths.pop().is_none() {
                    panic!("invalid pairing of </node>");
                }
            }
            quick_xml::events::Event::Text(x) if x.trim_ascii().is_empty() => (),
            quick_xml::events::Event::CData(x) => println!("? xml cdata: {x:?}"),
            quick_xml::events::Event::Comment(x) => println!("? xml comment: {x:?}"),
            quick_xml::events::Event::Decl(x) => println!("? xml decl: {x:?}"),
            quick_xml::events::Event::PI(x) => println!("? xml pi: {x:?}"),
            quick_xml::events::Event::DocType(x) => println!("? xml doctype: {x:?}"),
            quick_xml::events::Event::Eof => break,
            e => {
                unreachable!("unexpected toplevel element: {e:?}");
            }
        }
    }
}

pub fn read_interface_tag_content<'a>(
    reader: &mut quick_xml::Reader<&'a [u8]>,
    mut callback: impl for<'b> FnMut(InterfaceElementContent<'b>, &mut quick_xml::Reader<&'a [u8]>),
) {
    loop {
        match reader.read_event().unwrap() {
            quick_xml::events::Event::Start(x) if x.name().0 == b"method" => {
                let mut name = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.value);
                    }
                }

                callback(
                    InterfaceElementContent::Method {
                        name: name.expect("no name attr in method tag"),
                        empty: false,
                    },
                    reader,
                );
            }
            quick_xml::events::Event::Start(x) if x.name().0 == b"signal" => {
                let mut name = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.value);
                    }
                }

                callback(
                    InterfaceElementContent::Signal {
                        name: name.expect("no name attr in signal tag"),
                        empty: false,
                    },
                    reader,
                );
            }
            quick_xml::events::Event::Empty(x) if x.name().0 == b"method" => {
                let mut name = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.value);
                    }
                }

                callback(
                    InterfaceElementContent::Method {
                        name: name.expect("no name attr in method tag"),
                        empty: true,
                    },
                    reader,
                );
            }
            quick_xml::events::Event::Empty(x) if x.name().0 == b"signal" => {
                let mut name = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.value);
                    }
                }

                callback(
                    InterfaceElementContent::Signal {
                        name: name.expect("no name attr in signal tag"),
                        empty: true,
                    },
                    reader,
                );
            }
            quick_xml::events::Event::Empty(x) if x.name().0 == b"property" => {
                let mut name = None;
                let mut r#type = None;
                let mut access = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.value);
                    } else if a.key.0 == b"type" {
                        r#type = Some(a.value);
                    } else if a.key.0 == b"access" {
                        access = Some(a.value);
                    }
                }

                callback(
                    InterfaceElementContent::Property {
                        name: name.expect("no name attr in property tag"),
                        r#type: r#type.expect("no type attr in property tag"),
                        access: access.expect("no access attr in property tag"),
                    },
                    reader,
                );
            }
            quick_xml::events::Event::End(x) => {
                if x.name().0 == b"interface" {
                    break;
                }

                panic!("invalid closing pair: {:?}", x.name());
            }
            quick_xml::events::Event::Text(x) if x.trim_ascii().is_empty() => (),
            e => unreachable!("unexpected element in node: {e:?}"),
        }
    }
}

pub fn read_method_tag_content<'a>(
    reader: &mut quick_xml::Reader<&'a [u8]>,
    mut callback: impl for<'b> FnMut(MethodSignalElementContent<'b>, &mut quick_xml::Reader<&'a [u8]>),
) {
    loop {
        match reader.read_event().unwrap() {
            quick_xml::events::Event::Empty(x) if x.name().0 == b"arg" => {
                let mut name = None;
                let mut r#type = None;
                let mut direction = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.value);
                    } else if a.key.0 == b"type" {
                        r#type = Some(a.value);
                    } else if a.key.0 == b"direction" {
                        direction = Some(a.value);
                    }
                }

                callback(
                    MethodSignalElementContent::Arg {
                        name: name.expect("no name attr in arg tag"),
                        r#type: r#type.expect("no type attr in arg tag"),
                        direction,
                    },
                    reader,
                );
            }
            quick_xml::events::Event::Empty(x) if x.name().0 == b"annotation" => {
                let mut name = None;
                let mut r#value = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.value);
                    } else if a.key.0 == b"value" {
                        value = Some(a.value);
                    }
                }

                callback(
                    MethodSignalElementContent::Annotation {
                        name: name.expect("no name attr in annotation tag"),
                        value: value.expect("no value attr in annotation tag"),
                    },
                    reader,
                );
            }
            quick_xml::events::Event::End(x) => {
                if x.name().0 == b"method" {
                    break;
                }

                panic!("invalid closing pair: {:?}", x.name());
            }
            quick_xml::events::Event::Text(x) if x.trim_ascii().is_empty() => (),
            e => unreachable!("unexpected element in node: {e:?}"),
        }
    }
}

pub fn read_signal_tag_content<'a>(
    reader: &mut quick_xml::Reader<&'a [u8]>,
    mut callback: impl for<'b> FnMut(MethodSignalElementContent<'b>, &mut quick_xml::Reader<&'a [u8]>),
) {
    loop {
        match reader.read_event().unwrap() {
            quick_xml::events::Event::Empty(x) if x.name().0 == b"arg" => {
                let mut name = None;
                let mut r#type = None;
                let mut direction = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.value);
                    } else if a.key.0 == b"type" {
                        r#type = Some(a.value);
                    } else if a.key.0 == b"direction" {
                        direction = Some(a.value);
                    }
                }

                callback(
                    MethodSignalElementContent::Arg {
                        name: name.expect("no name attr in arg tag"),
                        r#type: r#type.expect("no type attr in arg tag"),
                        direction,
                    },
                    reader,
                );
            }
            quick_xml::events::Event::Empty(x) if x.name().0 == b"annotation" => {
                let mut name = None;
                let mut r#value = None;
                for a in x.attributes() {
                    let a = a.unwrap();

                    if a.key.0 == b"name" {
                        name = Some(a.value);
                    } else if a.key.0 == b"value" {
                        value = Some(a.value);
                    }
                }

                callback(
                    MethodSignalElementContent::Annotation {
                        name: name.expect("no name attr in annotation tag"),
                        value: value.expect("no value attr in annotation tag"),
                    },
                    reader,
                );
            }
            quick_xml::events::Event::End(x) => {
                if x.name().0 == b"signal" {
                    break;
                }

                panic!("invalid closing pair: {:?}", x.name());
            }
            quick_xml::events::Event::Text(x) if x.trim_ascii().is_empty() => (),
            e => unreachable!("unexpected element in node: {e:?}"),
        }
    }
}
