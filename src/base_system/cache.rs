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

        Self { base_path }
    }

    #[inline]
    pub fn new_path(&self, file_name: impl AsRef<Path>) -> PathBuf {
        self.base_path.join(file_name)
    }
}
