fn main() {
    // 与 tauri-build 内建 tauri_winres 使用同一条资源链设置 RT_MANIFEST，避免与 /MANIFEST:NO、
    // 第二份 .rc 再嵌 manifest 时发生 CVT1100 重复。
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=windows-app-manifest.xml");
        let windows = tauri_build::WindowsAttributes::new()
            .app_manifest(include_str!("windows-app-manifest.xml"));
        let attrs = tauri_build::Attributes::new().windows_attributes(windows);
        tauri_build::try_build(attrs).expect("tauri-build");
    }
    #[cfg(not(windows))]
    tauri_build::build();
}
