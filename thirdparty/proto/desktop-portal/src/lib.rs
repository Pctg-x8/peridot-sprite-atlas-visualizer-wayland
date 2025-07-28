#[repr(transparent)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectPath(pub(crate) std::ffi::CString);
impl ObjectPath {
    #[inline(always)]
    pub fn as_c_str(&self) -> &core::ffi::CStr {
        self.0.as_c_str()
    }
}

pub mod file_chooser;
