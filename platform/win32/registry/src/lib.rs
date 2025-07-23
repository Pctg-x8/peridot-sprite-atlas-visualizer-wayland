use std::os::windows::ffi::OsStringExt;

use windows::{
    Win32::{
        Foundation::ERROR_MORE_DATA,
        System::Registry::{
            HKEY, HKEY_LOCAL_MACHINE, REG_ROUTINE_FLAGS, REG_SAM_FLAGS, REG_VALUE_TYPE,
            RRF_RT_REG_SZ, RegCloseKey, RegGetValueW, RegOpenKeyExW,
        },
    },
    core::PCWSTR,
};

pub struct PredefinedKey(HKEY);
impl PredefinedKey {
    pub const LOCAL_MACHINE: Self = Self(HKEY_LOCAL_MACHINE);
}
impl RegistryKey for PredefinedKey {
    #[inline(always)]
    fn as_hkey(&self) -> HKEY {
        self.0
    }
}

pub struct OwnedKey(HKEY);
impl Drop for OwnedKey {
    #[inline]
    fn drop(&mut self) {
        if let Err(e) = unsafe { RegCloseKey(self.0).ok() } {
            eprintln!("warn: RegCloseKey error: {e:?}");
        }
    }
}
impl RegistryKey for OwnedKey {
    #[inline(always)]
    fn as_hkey(&self) -> HKEY {
        self.0
    }
}

pub trait RegistryKey {
    fn as_hkey(&self) -> HKEY;

    #[inline]
    fn open(
        &self,
        subkey: Option<PCWSTR>,
        options: u32,
        sam_desired: REG_SAM_FLAGS,
    ) -> windows::core::Result<OwnedKey> {
        let mut h = core::mem::MaybeUninit::uninit();
        unsafe {
            RegOpenKeyExW(
                self.as_hkey(),
                subkey.unwrap_or(const { PCWSTR::null() }),
                Some(options),
                sam_desired,
                h.as_mut_ptr(),
            )
            .ok()?;
        }

        Ok(OwnedKey(unsafe { h.assume_init() }))
    }

    #[inline]
    unsafe fn get_value<T>(
        &self,
        subkey: Option<PCWSTR>,
        value: Option<PCWSTR>,
        flags: REG_ROUTINE_FLAGS,
        type_out: *mut REG_VALUE_TYPE,
        data_out: *mut T,
        data_length_inout: *mut u32,
    ) -> windows::core::Result<()> {
        unsafe {
            RegGetValueW(
                self.as_hkey(),
                subkey.unwrap_or(const { PCWSTR::null() }),
                value.unwrap_or(const { PCWSTR::null() }),
                flags,
                Some(type_out),
                Some(data_out as _),
                Some(data_length_inout),
            )
            .ok()
        }
    }

    fn get_sz_value(
        &self,
        subkey: Option<PCWSTR>,
        value: Option<PCWSTR>,
    ) -> windows::core::Result<std::ffi::OsString> {
        let mut shortbuf = [0u16; 256];
        let mut buflen_bytes = 256 << 1;
        match unsafe {
            self.get_value(
                subkey,
                value,
                RRF_RT_REG_SZ,
                core::ptr::null_mut(),
                shortbuf.as_mut_ptr(),
                &mut buflen_bytes,
            )
        } {
            Ok(()) => {
                assert_eq!(buflen_bytes & 0x01, 0);
                // Note: RustのOsStringはnul終端なくていい
                Ok(std::ffi::OsString::from_wide(
                    &shortbuf[..((buflen_bytes >> 1) - 1) as usize],
                ))
            }
            Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => {
                // retry with correct size buffer
                assert_eq!(buflen_bytes & 0x01, 0);
                let mut buf = Vec::with_capacity((buflen_bytes >> 1) as _);
                unsafe {
                    buf.set_len(buf.capacity());
                    self.get_value(
                        subkey,
                        value,
                        RRF_RT_REG_SZ,
                        core::ptr::null_mut(),
                        buf.as_mut_ptr(),
                        &mut buflen_bytes,
                    )?;
                }
                assert_eq!(buflen_bytes & 0x01, 0);
                // Note: RustのOsStringはnul終端なくていい
                Ok(std::ffi::OsString::from_wide(
                    &buf[..((buflen_bytes >> 1) - 1) as usize],
                ))
            }
            Err(e) => Err(e),
        }
    }
}
