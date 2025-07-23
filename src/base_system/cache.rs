use std::path::{Path, PathBuf};

/// Platform specific cache path generator
pub struct Cache {
    pub base_path: PathBuf,
}
impl Cache {
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        let base_path = {
            let mut p = PathBuf::from("/var/tmp");
            p = p.join("peridot/sprite-atlas-visualizer");
            if let Err(e) = std::fs::create_dir_all(&p) {
                tracing::warn!(reason = ?e, path = %p.display(), "creating cachedir failed");
            }

            p
        };
        #[cfg(target_os = "macos")]
        let base_path = {
            let fm = objc_rt::foundation::NSFileManager::default();
            let url = fm
                .url_for_directory(
                    objc_rt::foundation::NSSearchPathDirectory::CachesDirectory,
                    objc_rt::foundation::NSSearchPathDomainMask::LocalDomainMask,
                    None,
                    true,
                )
                .unwrap();

            PathBuf::from(url.file_system_representation().to_str().unwrap())
        };
        #[cfg(windows)]
        let base_path = {
            let base = PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("no %LOCALAPPDATA%"));
            let p = base.join("peridot/sprite-atlas-visualizer");
            if let Err(e) = std::fs::create_dir_all(&p) {
                tracing::warn!(reason = ?e, path = %p.display(), "creating cachedir failed");
            }

            p
        };

        tracing::info!(path = %base_path.display(), "cache initialized");
        Self { base_path }
    }

    #[inline]
    pub fn new_path(&self, file_name: impl AsRef<Path>) -> PathBuf {
        self.base_path.join(file_name)
    }
}
