//! findx2-content 构建脚本：拉取预编译 Pdfium 动态库（BSD-3，Chrome 同款 PDF 引擎）。
//!
//! - 只处理 Windows x86_64（当前 dev/发布目标）；其他平台跳过，运行时回退纯 Rust
//!   `pdf-extract`（慢但可用，毒文件由 `catch_unwind` 接住）。
//! - 下载源：bblanchon/pdfium-binaries 的版本化 tag（`chromium/8179`），URL 稳定；
//!   失败（离线/无 curl/tar）只警告不断构建——运行时自动降级。
//! - 产物 `pdfium.dll` 放到 profile 输出目录（`target/debug/` 等，exe 旁边），
//!   service/GUI/CLI 运行时就近加载；安装器后续负责随包带走。
//! - 已下载且版本标记一致则跳过（`pdfium.version` 标记文件）。

use std::path::PathBuf;
use std::process::Command;

const PDFIUM_TAG: &str = "chromium/8179";
const PDFIUM_URL: &str =
    "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F8179/pdfium-win-x64.tgz";

fn profile_dir() -> Option<PathBuf> {
    // OUT_DIR = <target>/<profile>/build/<crate>-<hash>/out → 上浮 3 级即 profile 目录。
    let out = PathBuf::from(std::env::var("OUT_DIR").ok()?);
    out.ancestors().nth(3).map(|p| p.to_path_buf())
}

fn run(cmd: &mut Command) -> bool {
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("x86_64")
    {
        return;
    }
    let Some(dir) = profile_dir() else {
        println!("cargo:warning=findx2-content: 无法定位输出目录，跳过 pdfium 下载");
        return;
    };
    let dll = dir.join("pdfium.dll");
    let marker = dir.join("pdfium.version");
    let want = format!("{PDFIUM_TAG}\n");
    if dll.is_file()
        && std::fs::read_to_string(&marker).ok().as_deref() == Some(want.as_str())
    {
        return;
    }
    let tmp = std::env::temp_dir().join("findx2-pdfium-win-x64.tgz");
    let tmp_s = tmp.to_string_lossy().into_owned();
    // curl Windows 自带（Win10+），Linux/macOS 常备；没有就降级。
    let dl_ok = run(Command::new("curl.exe").args([
        "-L",
        "--fail",
        "--connect-timeout",
        "20",
        "--max-time",
        "300",
        "-o",
        &tmp_s,
        PDFIUM_URL,
    ])) || run(Command::new("curl").args([
        "-L",
        "--fail",
        "--connect-timeout",
        "20",
        "--max-time",
        "300",
        "-o",
        &tmp_s,
        PDFIUM_URL,
    ]));
    if !dl_ok || !tmp.is_file() {
        println!("cargo:warning=findx2-content: pdfium 下载失败，PDF 回退 pdf-extract");
        return;
    }
    // tgz 里是 bin/pdfium.dll；tar（Win10+ 自带）解到临时目录再搬过去。
    let stage = std::env::temp_dir().join("findx2-pdfium-stage");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).ok();
    let stage_s = stage.to_string_lossy().into_owned();
    let un_ok = run(Command::new("tar.exe").args([
        "-xzf",
        &tmp_s,
        "-C",
        &stage_s,
    ])) || run(Command::new("tar").args(["-xzf", &tmp_s, "-C", &stage_s]));
    let staged = stage.join("bin").join("pdfium.dll");
    let staged = if staged.is_file() {
        staged
    } else {
        // 兜底：包内直放 pdfium.dll 的情况。
        stage.join("pdfium.dll")
    };
    if !un_ok || !staged.is_file() {
        println!("cargo:warning=findx2-content: pdfium 解包失败，PDF 回退 pdf-extract");
        return;
    }
    if std::fs::copy(&staged, &dll).is_err() {
        println!("cargo:warning=findx2-content: pdfium.dll 落盘失败，PDF 回退 pdf-extract");
        return;
    }
    let _ = std::fs::write(&marker, want);
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_dir_all(&stage);
    println!("cargo:warning=findx2-content: pdfium.dll 就绪 {}", dll.display());
}
