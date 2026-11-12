fn main() {
    // GPUI's debug-mode fluent UI builders have large, finite frames. Windows' default
    // 1 MiB main stack is insufficient for the integrated settings/usage pages.
    // Reserve virtual address space only; committed pages still grow on demand.
    // Scope the supported MSVC linker option to this GUI binary, never the service.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg-bin=findx2-gui-native=/STACK:8388608");
    }
}