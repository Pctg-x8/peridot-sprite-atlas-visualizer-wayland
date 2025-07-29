#[repr(transparent)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectPath(pub(crate) std::ffi::CString);
impl ObjectPath {
    #[inline(always)]
    pub fn as_c_str(&self) -> &core::ffi::CStr {
        self.0.as_c_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestResponseCode {
    Success,
    Cancelled,
    InteractionEnded,
    Unknown(u32),
}
impl RequestResponseCode {
    pub fn read(msg_iter: &dbus::MessageIter) -> Self {
        match msg_iter.try_get_u32().expect("invalid response code") {
            0 => Self::Success,
            1 => Self::Cancelled,
            2 => Self::InteractionEnded,
            code => Self::Unknown(code),
        }
    }
}

pub mod file_chooser;
