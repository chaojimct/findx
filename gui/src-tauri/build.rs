//! 在编译 `findx2-gui` 之前按需构建前端（`npm run build`），使根目录 `cargo build --workspace`
//! 与 `cargo tauri build` 一样能得到 `gui/dist`，而不必单独记一步前端构建。
//!
//! 跳过：环境变量 `SKIP_FINDX_GUI_FRONTEND=1`（CI 已预构建 dist 等场景）。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

fn main() {
    maybe_build_gui_frontend();

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

fn gui_root() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("findx2-gui crate must live at gui/src-tauri")
        .to_path_buf()
}

fn maybe_build_gui_frontend() {
    let gui_root = gui_root();

    println!("cargo:rerun-if-changed={}", gui_root.join("package.json").display());
    println!("cargo:rerun-if-changed={}", gui_root.join("package-lock.json").display());
    println!("cargo:rerun-if-changed={}", gui_root.join("vite.config.ts").display());
    println!("cargo:rerun-if-changed={}", gui_root.join("tsconfig.json").display());
    println!("cargo:rerun-if-changed={}", gui_root.join("index.html").display());
    let src = gui_root.join("src");
    if src.is_dir() {
        println!("cargo:rerun-if-changed={}", src.display());
    }

    if env::var("SKIP_FINDX_GUI_FRONTEND").ok().as_deref() == Some("1") {
        if !gui_root.join("dist").join("index.html").exists() {
            println!(
                "cargo:warning=SKIP_FINDX_GUI_FRONTEND=1 但未找到 gui/dist/index.html，\
                 tauri-build 可能失败；请先执行 gui 目录下 npm run build，或去掉该环境变量。"
            );
        }
        return;
    }

    if !frontend_out_of_date(&gui_root) {
        return;
    }

    run_npm_build(&gui_root);
}

/// `dist/index.html` 不存在，或任意前端源/锁文件比它新 → 需要重新 `vite build`。
fn frontend_out_of_date(gui_root: &Path) -> bool {
    let index = gui_root.join("dist").join("index.html");
    if !index.exists() {
        return true;
    }
    let cutoff = match fs::metadata(&index).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return true,
    };

    for rel in [
        "package.json",
        "package-lock.json",
        "vite.config.ts",
        "tsconfig.json",
        "index.html",
    ] {
        let p = gui_root.join(rel);
        if p.is_file() {
            if let Ok(t) = fs::metadata(&p).and_then(|m| m.modified()) {
                if t > cutoff {
                    return true;
                }
            }
        }
    }

    let src = gui_root.join("src");
    dir_has_file_newer_than(&src, cutoff)
}

fn dir_has_file_newer_than(dir: &Path, cutoff: SystemTime) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let Ok(meta) = ent.metadata() else {
            continue;
        };
        if meta.is_dir() {
            if dir_has_file_newer_than(&path, cutoff) {
                return true;
            }
        } else if meta.is_file() {
            if let Ok(t) = meta.modified() {
                if t > cutoff {
                    return true;
                }
            }
        }
    }
    false
}

fn npm_cmd() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

/// `npm run build` 依赖 `node_modules/.bin/tsc`；未在 `gui` 下执行过 `npm install`/`npm ci` 时
/// Windows 会报「'tsc' 不是内部或外部命令」且 **stderr 常被 Cargo 折叠**，这里主动补装并失败时打出完整输出。
fn ensure_node_modules(gui_root: &Path) {
    let tsc_bin = gui_root
        .join("node_modules")
        .join(".bin")
        .join(if cfg!(windows) { "tsc.cmd" } else { "tsc" });
    if tsc_bin.is_file() {
        return;
    }
    eprintln!(
        "findx2-gui build.rs: 未找到 {}，正在于 {} 执行 npm ci …",
        tsc_bin.display(),
        gui_root.display()
    );
    let out = Command::new(npm_cmd())
        .args(["ci"])
        .current_dir(gui_root)
        .output()
        .unwrap_or_else(|e| panic!("无法执行 npm ci（{e}）。请安装 Node.js 并确保 npm 在 PATH 中。"));
    if !out.status.success() {
        eprintln!(
            "npm ci 失败 (code {:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        panic!(
            "npm ci 未成功，无法构建前端。请在 `gui` 目录手动执行 `npm ci` 或 `npm install` 后重试。"
        );
    }
}

fn run_npm_build(gui_root: &Path) {
    ensure_node_modules(gui_root);
    eprintln!(
        "findx2-gui build.rs: 正在构建前端 (npm run build)，目录 {}",
        gui_root.display()
    );
    let out = Command::new(npm_cmd())
        .args(["run", "build"])
        .current_dir(gui_root)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "无法执行 `npm run build`（{e}）。请安装 Node.js 并确保 npm 在 PATH 中，\
                 或设置 SKIP_FINDX_GUI_FRONTEND=1 跳过（需已存在 gui/dist）。"
            )
        });
    if !out.status.success() {
        eprintln!(
            "`npm run build` 失败 (code {:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        panic!(
            "`npm run build` 未成功。若缺少依赖请先 `cd gui && npm ci`；若为 TypeScript 错误请根据上方日志修复。"
        );
    }
}
