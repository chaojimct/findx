//! Control the already registered FindX2Search service; never kill by process name.
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Snapshot {
    pub name: String,
    pub state: String,
    pub start_mode: String,
    pub path_name: String,
    pub process_id: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Auto,
    Manual,
    GuiAutoOn,
    GuiAutoOff,
    Compact,
    Rebuild,
}
impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "启动正式服务",
            Self::Stop => "停止正式服务",
            Self::Auto => "服务开机自动启动",
            Self::Manual => "服务改为手动启动",
            Self::GuiAutoOn => "启用正式 GUI 登录启动",
            Self::GuiAutoOff => "关闭正式 GUI 登录启动",
            Self::Compact => "备份后压缩正式索引",
            Self::Rebuild => "备份后重建正式索引",
        }
    }
}
fn hidden(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x08000000);
    }
}
fn output(mut c: Command) -> Result<String, String> {
    hidden(&mut c);
    let out = c.output().map_err(|e| e.to_string())?;
    let bytes = [out.stdout, out.stderr].concat();
    let text = String::from_utf8(bytes.clone())
        .unwrap_or_else(|_| encoding_rs::GBK.decode(&bytes).0.into_owned());
    if !out.status.success() {
        return Err(format!("退出码 {:?}: {text}", out.status.code()));
    }
    Ok(text)
}
pub fn snapshot() -> Result<Snapshot, String> {
    #[cfg(windows)]
    {
        let mut c = Command::new("powershell.exe");
        c.args(["-NoProfile","-NonInteractive","-Command","[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new(); $ErrorActionPreference='Stop'; Get-CimInstance Win32_Service -Filter \"Name='FindX2Search'\" | Select-Object Name,State,StartMode,PathName,ProcessId | ConvertTo-Json -Compress"]);
        let s: Snapshot = serde_json::from_str(output(c)?.trim().trim_start_matches('\u{feff}'))
            .map_err(|e| format!("未找到正式 FindX2Search 服务：{e}"))?;
        validate(&s)?;
        Ok(s)
    }
    #[cfg(not(windows))]
    {
        Err("仅 Windows 支持 SCM 服务管理".into())
    }
}
pub fn split_command(s: &str) -> Vec<String> {
    let mut q = false;
    let mut item = String::new();
    let mut out = vec![];
    for ch in s.chars() {
        match ch {
            '"' => q = !q,
            c if c.is_whitespace() && !q => {
                if !item.is_empty() {
                    out.push(std::mem::take(&mut item));
                }
            }
            c => item.push(c),
        }
    }
    if !item.is_empty() {
        out.push(item)
    }
    out
}
pub fn validate(s: &Snapshot) -> Result<(), String> {
    let args = split_command(&s.path_name);
    let expected =
        PathBuf::from(std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into()))
            .join("FindX/findx2-service.exe");
    let norm = |s: &str| s.replace('/', "\\").to_lowercase();
    if s.name != "FindX2Search"
        || args
            .first()
            .is_none_or(|a| norm(a) != norm(&expected.to_string_lossy()))
        || !args
            .windows(2)
            .any(|a| a[0] == "--pipe" && a[1] == "findx2")
    {
        return Err("服务身份/安装路径/pipe 不匹配；拒绝操作任何其他服务".into());
    }
    Ok(())
}
fn arg(s: &Snapshot, key: &str) -> Result<String, String> {
    split_command(&s.path_name)
        .windows(2)
        .find(|a| a[0] == key)
        .map(|a| a[1].clone())
        .ok_or_else(|| format!("服务未提供 {key}，拒绝猜测"))
}
fn sc(args: &[&str]) -> Result<String, String> {
    let mut c = Command::new("sc.exe");
    c.args(args);
    output(c)
}
fn wait_state(state: &str) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(45) {
        let s = snapshot()?;
        if s.state.eq_ignore_ascii_case(state) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(format!("等待 SCM {state} 超时；请查看诊断，不代表操作成功"))
}
/// Only invoke after the UI displays the exact target and obtains explicit confirmation.
pub fn execute(action: Action, expected: Snapshot) -> Result<String, String> {
    let current = snapshot()?;
    if current.path_name != expected.path_name
        || current.process_id != expected.process_id
        || current.state != expected.state
    {
        return Err("确认后服务状态已改变；请刷新重新确认".into());
    }
    match action {
        Action::Start => {
            if current.state != "Running" {
                sc(&["start", "FindX2Search"])?;
                wait_state("Running")?;
            }
        }
        Action::Stop => {
            if current.state != "Stopped" {
                sc(&["stop", "FindX2Search"])?;
                wait_state("Stopped")?;
            }
        }
        Action::Auto => {
            sc(&["config", "FindX2Search", "start=", "auto"])?;
            if snapshot()?.start_mode != "Auto" {
                return Err("自启动回读未生效".into());
            }
        }
        Action::Manual => {
            sc(&["config", "FindX2Search", "start=", "demand"])?;
            if snapshot()?.start_mode != "Manual" {
                return Err("手动启动回读未生效".into());
            }
        }
        Action::GuiAutoOn | Action::GuiAutoOff => {
            let enabled = action == Action::GuiAutoOn;
            let state = crate::autostart::apply(enabled)?;
            if state.enabled != enabled {
                return Err("正式 GUI 自启动回读不一致".into());
            }
        }
        Action::Compact | Action::Rebuild => return maintenance(action, &current),
    }
    Ok(format!("{}完成；SCM/注册表已回读验证", action.label()))
}
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    let m = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return Err("备份拒绝重解析点/挂载路径".into());
        }
    }
    if m.is_symlink() {
        return Err("备份拒绝符号链接".into());
    }
    if m.is_dir() {
        std::fs::create_dir(dst).map_err(|e| e.to_string())?;
        for e in std::fs::read_dir(src).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            copy_tree(&e.path(), &dst.join(e.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst).map_err(|e| e.to_string())?;
    }
    Ok(())
}
const SUFFIXES: [&str; 3] = ["", ".v6", ".exclude.json"];
fn part(index: &Path, suffix: &str) -> PathBuf {
    let mut p = index.as_os_str().to_owned();
    p.push(suffix);
    p.into()
}
fn copy_bundle(src: &Path, dst: &Path) -> Result<(), String> {
    for s in SUFFIXES {
        let p = part(src, s);
        if p.exists() {
            copy_tree(&p, &part(dst, s))?;
        }
    }
    Ok(())
}
/// Rename a complete staged bundle only after successful CLI validation. Never delete originals.
fn promote(index: &Path, staged: &Path, rollback: &Path) -> Result<(), String> {
    let mut moved = vec![];
    let mut installed = vec![];
    let result = (|| {
        for s in SUFFIXES {
            let p = part(index, s);
            if p.exists() {
                std::fs::rename(&p, part(rollback, s)).map_err(|e| e.to_string())?;
                moved.push(s);
            }
        }
        for s in SUFFIXES {
            let p = part(staged, s);
            if p.exists() {
                std::fs::rename(&p, part(index, s)).map_err(|e| e.to_string())?;
                installed.push(s);
            }
        }
        Ok::<(), String>(())
    })();
    if let Err(e) = result {
        for s in installed.iter().rev() {
            let _ = std::fs::rename(part(index, s), part(staged, s));
        }
        for s in moved.iter().rev() {
            std::fs::rename(part(rollback, s), part(index, s)).map_err(|restore| {
                format!(
                    "替换失败 {e}；自动恢复失败 {restore}；旧库在 {}",
                    rollback.display()
                )
            })?;
        }
        return Err(format!("替换失败，已恢复原库：{e}"));
    }
    Ok(())
}
fn maintenance(action: Action, s: &Snapshot) -> Result<String, String> {
    let index = PathBuf::from(arg(s, "--index")?);
    if !index.is_absolute() {
        return Err("服务索引路径非绝对路径，拒绝维护".into());
    }
    let exe = PathBuf::from(split_command(&s.path_name)[0].clone());
    let cli = exe.parent().ok_or("服务路径无父目录")?.join("findx2.exe");
    if !cli.is_file() {
        return Err("未找到安装版 findx2.exe，拒绝混用开发 CLI".into());
    }
    // Require administrative privileges BEFORE stopping an active service; no silent UAC or installation.
    #[cfg(windows)]
    {
        let mut c = Command::new("powershell.exe");
        c.args(["-NoProfile","-NonInteractive","-Command","if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw '请以管理员身份运行 native 客户端后重试；未停止服务' }"]);
        output(c)?;
    }
    let running = s.state == "Running";
    if running {
        sc(&["stop", "FindX2Search"])?;
        wait_state("Stopped")?;
    }
    let result = (|| {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis();
        let work = index
            .parent()
            .ok_or("索引无父目录")?
            .join(format!("native-maintenance-{id}"));
        std::fs::create_dir(&work).map_err(|e| e.to_string())?;
        let backup = work.join("backup");
        let staging = work.join("staging");
        let rollback = work.join("rollback");
        for dir in [&backup, &staging, &rollback] {
            std::fs::create_dir(dir).map_err(|e| e.to_string())?;
        }
        let backup_index = backup.join("index.bin");
        copy_bundle(&index, &backup_index)?;
        let staged = staging.join("index.bin");
        let mut c = Command::new(&cli);
        if action == Action::Compact {
            copy_bundle(&backup_index, &staged)?;
            c.arg("compact").arg("--index").arg(&staged);
        } else {
            c.arg("index")
                .arg("--output")
                .arg(&staged)
                .arg("--volume")
                .arg(arg(s, "--volume")?);
            // Exclusion sidecar is preserved as authoritative; unknown layouts stop rather than rebuild the wrong scope.
            let side = part(&index, ".exclude.json");
            if side.exists() {
                return Err(format!(
                    "发现排除规则 {}；为避免错误扫描范围，此版本拒绝自动重建。旧库未改，服务将恢复。",
                    side.display()
                ));
            }
        }
        // Maintenance stdout/stderr are bounded to a log file, never pipe-buffer deadlock the CLI.
        let log = std::fs::File::create(work.join("maintenance.log")).map_err(|e| e.to_string())?;
        c.stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log);
        hidden(&mut c);
        let status = c.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!(
                "安装版 CLI 失败；原库未改。日志 {}",
                work.join("maintenance.log").display()
            ));
        }
        if !staged.is_file() {
            return Err("CLI 未生成 index.bin；原库未改".into());
        }
        let mut check = Command::new(&cli);
        check.arg("status").arg("--index").arg(&staged);
        output(check)?;
        promote(&index, &staged, &rollback.join("index.bin"))?;
        Ok(format!(
            "{}成功；保留完整备份与旧库：{}",
            action.label(),
            work.display()
        ))
    })();
    let restart = if running {
        sc(&["start", "FindX2Search"]).and_then(|_| wait_state("Running"))
    } else {
        Ok(())
    };
    match (result, restart) {
        (Ok(m), Ok(())) => Ok(format!("{m}；服务状态已恢复")),
        (Err(e), Ok(())) => Err(format!("{e}；服务状态已恢复")),
        (r, Err(e)) => Err(format!("维护结果 {:?}；服务恢复失败：{e}", r)),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_service_command_splits() {
        let s = r#""C:\Program Files\FindX\findx2-service.exe" --service --index "C:\ProgramData\FindX\index.bin" --pipe findx2"#;
        let a = split_command(s);
        assert_eq!(a.len(), 6);
        assert_eq!(a[0], r"C:\Program Files\FindX\findx2-service.exe");
    }
    #[test]
    fn unknown_service_is_rejected() {
        let s = Snapshot {
            name: "Other".into(),
            state: "Running".into(),
            start_mode: "Auto".into(),
            path_name: "other.exe --pipe findx2".into(),
            process_id: 1,
        };
        assert!(validate(&s).is_err());
    }
    #[test]
    fn bundle_promotes_without_deleting_originals() {
        let base = std::env::temp_dir().join(format!("findx-native-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir(&base).unwrap();
        let old = base.join("old");
        let staged = base.join("stage");
        let rollback = base.join("backup");
        std::fs::write(&old, b"old").unwrap();
        std::fs::write(&staged, b"new").unwrap();
        promote(&old, &staged, &rollback).unwrap();
        assert_eq!(std::fs::read(&old).unwrap(), b"new");
        assert_eq!(std::fs::read(&rollback).unwrap(), b"old");
        std::fs::remove_dir_all(base).unwrap();
    }
}
