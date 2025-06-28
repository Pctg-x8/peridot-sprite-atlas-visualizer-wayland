fn main() {
    #[cfg(windows)]
    println!(
        "cargo:rustc-link-search={}",
        std::env::current_dir()
            .unwrap()
            .join("vcpkg_installed/x64-windows-static-md/lib")
            .display()
    );
    #[cfg(windows)]
    println!("cargo:rustc-link-search=static={}/Lib", env!("VK_SDK_PATH"));
}
