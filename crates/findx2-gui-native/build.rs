use std::{env, path::PathBuf, process::Command};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rustc-link-arg-bin=findx2-gui-native=/STACK:8388608");
    println!("cargo:rerun-if-changed=../../gui/src-tauri/icons/icon.ico");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numbers: Vec<_> = version
        .split('.')
        .take(3)
        .map(|s| s.parse::<u16>().expect("numeric product version"))
        .collect();
    assert_eq!(numbers.len(), 3);
    let numeric = format!("{},{},{},0", numbers[0], numbers[1], numbers[2]);
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let icon = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../gui/src-tauri/icons/icon.ico")
        .canonicalize()
        .unwrap();
    let icon = icon
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");
    let resource = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
 BLOCK "StringFileInfo"
 BEGIN
  BLOCK "040904B0"
  BEGIN
   VALUE "CompanyName", "FindX\0"
   VALUE "FileDescription", "FindX Native\0"
   VALUE "FileVersion", "{version}\0"
   VALUE "ProductName", "FindX\0"
   VALUE "ProductVersion", "{version}\0"
   VALUE "OriginalFilename", "FindX.exe\0"
  END
 END
 BLOCK "VarFileInfo"
 BEGIN
  VALUE "Translation", 0x409, 1200
 END
END
"#
    );
    let rc_file = out.join("findx-native.rc");
    let res = out.join("findx-native.res");
    std::fs::write(&rc_file, resource).unwrap();
    let rc = env::var_os("RC").map(PathBuf::from).unwrap_or_else(|| {
        let base = PathBuf::from(
            env::var_os("ProgramFiles(x86)").unwrap_or_else(|| r"C:\Program Files (x86)".into()),
        )
        .join("Windows Kits/10/bin");
        let mut versions: Vec<_> = std::fs::read_dir(&base)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("10."))
            })
            .collect();
        versions.sort();
        versions.reverse();
        versions
            .into_iter()
            .map(|p| p.join("x64/rc.exe"))
            .find(|p| p.is_file())
            .unwrap_or_else(|| "rc.exe".into())
    });
    let output = Command::new(rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(&rc_file)
        .output()
        .expect("Windows SDK rc.exe is required for product version resources");
    assert!(
        output.status.success(),
        "rc.exe failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!(
        "cargo:rustc-link-arg-bin=findx2-gui-native={}",
        res.display()
    );
}
