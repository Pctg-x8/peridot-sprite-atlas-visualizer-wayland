use std::{ffi::OsString, path::PathBuf};

use win32_registry::{PredefinedKey, RegistryKey};
use windows::{Win32::System::Registry::KEY_READ, core::w};

pub struct Windows10SDK {
    installation_folder: PathBuf,
    product_version: OsString,
}
impl Windows10SDK {
    pub fn find() -> Self {
        // レジストリの中にあるらしい
        // https://stackoverflow.com/questions/35119223/how-to-programmatically-detect-and-locate-the-windows-10-sdk

        let key = PredefinedKey::LOCAL_MACHINE
            .open(
                Some(w!(
                    "SOFTWARE\\WOW6432Node\\Microsoft\\Microsoft SDKs\\Windows\\v10.0"
                )),
                0,
                KEY_READ,
            )
            .expect("Failed to open registry");

        let installation_folder = key
            .get_sz_value(None, Some(w!("InstallationFolder")))
            .expect("Failed to get InstallationFolder value");
        let mut product_version = key
            .get_sz_value(None, Some(w!("ProductVersion")))
            .expect("Failed to get ProductVersion value");
        product_version.push(".0");

        Self {
            installation_folder: PathBuf::from(installation_folder),
            product_version,
        }
    }

    pub fn include_folder(&self) -> PathBuf {
        self.installation_folder
            .join("Include")
            .join(&self.product_version)
    }

    pub fn bin_folder(&self) -> PathBuf {
        let bits_str = if cfg!(target_arch = "x86_64") {
            "x64"
        } else if cfg!(target_arch = "x86") {
            "x86"
        } else {
            unimplemented!();
        };

        self.installation_folder
            .join("bin")
            .join(&self.product_version)
            .join(bits_str)
    }
}
