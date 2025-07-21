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
            .join("vcpkg_installed/x64-windows-static-md/lib")
            .display()
    );
    println!("cargo:rustc-link-search=static={}/Lib", env!("VK_SDK_PATH"));
}

#[cfg(target_os = "macos")]
fn cfg_macos() {
    println!("cargo::rustc-link-search=/opt/homebrew/lib");

    // Note: ビルド前に bass source (Vulkan SDK Path)/setup-env.sh を実行する
    let vk_sdk_base =
        std::path::PathBuf::from(option_env!("VULKAN_SDK").expect("VULKAN_SDK required"));
    let framework_path = vk_sdk_base.join("Frameworks");
    let lib_path = vk_sdk_base.join("lib");

    println!(
        "cargo:rustc-link-search=framework={}",
        framework_path.display()
    );
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_path.display());
}
