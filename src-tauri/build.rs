fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        tauri_build::build();
        return;
    }

    // tauri-build embeds the Common Controls v6 manifest through a resource
    // file that only reaches the binary, so `cargo test` executables linked
    // without it: they bind System32's comctl32 5.82, which has no
    // `TaskDialogIndirect`, and die at load with 0xc0000139
    // (STATUS_ENTRYPOINT_NOT_FOUND) before any test runs. Hand the same
    // manifest to the linker instead, which applies it to every artifact this
    // package links — the app, the cdylib and the lib test binary alike.
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("windows-app-manifest.xml");
    println!("cargo:rerun-if-changed=windows-app-manifest.xml");
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());

    let windows = tauri_build::WindowsAttributes::new_without_app_manifest();
    let attributes = tauri_build::Attributes::new().windows_attributes(windows);
    if let Err(error) = tauri_build::try_build(attributes) {
        panic!("tauri-build failed: {error:#}");
    }
}
