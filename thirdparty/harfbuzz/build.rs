fn main() {
    #[cfg(windows)]
    cfg_windows();
    #[cfg(target_os = "macos")]
    cfg_macos();
}

#[cfg(windows)]
fn cfg_windows() {
    println!(
        "cargo:rustc-link-search={}",
        std::env::current_dir()
            .unwrap()
            .join("../../vcpkg_installed/x64-windows-static-md/lib")
            .display()
    );
}

#[cfg(target_os = "macos")]
fn cfg_macos() {
    println!("cargo::rustc-link-search=/opt/homebrew/lib");
}
