#[cfg(unix)]
pub mod wayland;
#[cfg(windows)]
pub mod win32;

#[cfg(unix)]
pub use self::wayland::AppShell;

#[cfg(windows)]
pub use self::win32::AppShell;
