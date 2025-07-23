fn main() {
    #[cfg(windows)]
    cfg_windows();
    #[cfg(target_os = "macos")]
    cfg_macos();
}

#[cfg(windows)]
fn cfg_windows() {
    let project_root = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("no CARGO_MANIFEST_DIR"),
    );
    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("no OUT_DIR"));

    println!(
        "cargo:rustc-link-search={}",
        std::env::current_dir()
            .unwrap()
            .join("vcpkg_installed/x64-windows-static-md/lib")
            .display()
    );
    println!("cargo:rustc-link-search=static={}/Lib", env!("VK_SDK_PATH"));

    // process win32 exe resources
    let win10_sdk = microsoft_sdk_locator::Windows10SDK::find();
    let win10_sdk_include_dir = win10_sdk.include_folder();
    std::process::Command::new(win10_sdk.bin_folder().join("rc.exe"))
        .arg("/I")
        .arg(win10_sdk_include_dir.join("um"))
        .arg("/I")
        .arg(win10_sdk_include_dir.join("shared"))
        .args(["/r", "/fo"])
        .arg(out_dir.join("exe.res"))
        .arg(project_root.join("win32_exe.rc"))
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap()
        .wait()
        .unwrap();
    // +verbatimで拡張子そのままにLinkerに渡せるらしい
    // https://github.com/rust-lang/rust/issues/81488
    println!("cargo:rustc-link-lib=dylib:+verbatim=exe.res");
    println!("cargo:rustc-link-search={}", out_dir.display());
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
