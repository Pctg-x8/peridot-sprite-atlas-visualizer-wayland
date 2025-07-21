#[cfg(all(unix, not(target_os = "macos")))]
pub mod wayland;
#[cfg(all(unix, not(target_os = "macos")))]
pub use self::wayland::AppShell;

#[cfg(windows)]
pub mod win32;
#[cfg(windows)]
pub use self::win32::AppShell;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use self::macos::AppShell;
