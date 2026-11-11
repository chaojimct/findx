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
    ApplyPreferences,
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
            Self::ApplyPreferences => "应用已保存的服务选项",
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
#[cfg(windows)]
pub fn split_command(s: &str) -> Vec<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::{
        Foundation::{HLOCAL, LocalFree},
        UI::Shell::CommandLineToArgvW,
    };
    let wide: Vec<u16> = std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut argc = 0;
    unsafe {
        let argv = CommandLineToArgvW(windows::core::PCWSTR(wide.as_ptr()), &mut argc);
        if argv.is_null() {
            return vec![];
        }
        let output = std::slice::from_raw_parts(argv, argc.max(0) as usize)
            .iter()
            .map(|s| s.to_string().unwrap_or_default())
            .collect();
        let _ = LocalFree(Some(HLOCAL(argv as *mut _)));
        output
    }
}
#[cfg(not(windows))]
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
pub fn execute(
    action: Action,
    expected: Snapshot,
    preferences: crate::settings_logic::FindxGuiSettings,
) -> Result<String, String> {
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
        Action::ApplyPreferences => return apply_preferences(&current, &preferences),
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
                let bytes = std::fs::read(&side).map_err(|e| e.to_string())?;
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|e| format!("正式排除规则损坏：{e}"))?;
                let dirs = value
                    .get("excluded_dirs")
                    .and_then(|v| v.as_array())
                    .ok_or("未知正式排除规则格式，拒绝重建")?;
                for dir in dirs {
                    let dir = dir.as_str().ok_or("正式排除规则包含非字符串")?;
                    if !std::path::Path::new(dir).is_absolute() {
                        return Err("正式排除目录不是完整路径，拒绝重建".into());
                    }
                    c.arg("--exclude-dir").arg(dir);
                }
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

/// Preserve every unrelated CLI flag and official JSON field. Only these three
/// runtime settings are owned by this operation; dev paths/UI prefs never migrate.
pub fn runtime_arguments(
    s: &Snapshot,
    p: &crate::settings_logic::FindxGuiSettings,
) -> Result<Vec<String>, String> {
    validate(s)?;
    let args = split_command(&s.path_name);
    let mut next = vec![];
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--save-interval-secs" => i += 2,
            "--no-backfill" | "--no-everything-ipc" => i += 1,
            _ => {
                next.push(args[i].clone());
                i += 1;
            }
        }
    }
    next.push("--save-interval-secs".into());
    next.push(p.save_interval_secs.max(1).to_string());
    if !p.enable_metadata_backfill {
        next.push("--no-backfill".into());
    }
    if !p.enable_everything_ipc {
        next.push("--no-everything-ipc".into());
    }
    Ok(next)
}

/// Parse the official SCM command: interval, metadata backfill enabled, Everything IPC enabled.
pub fn official_runtime_options(s: &Snapshot) -> Result<(u64, bool, bool), String> {
    validate(s)?;
    let args = split_command(&s.path_name);
    let mut interval = 30u64;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--save-interval-secs" {
            let raw = args
                .get(i + 1)
                .ok_or("正式命令缺少 --save-interval-secs 参数")?;
            interval = raw
                .parse::<u64>()
                .map_err(|_| format!("正式落盘间隔不是整数：{raw}"))?;
            if interval < 1 {
                return Err("正式落盘间隔小于 1 秒".into());
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    let backfill = !args.iter().any(|a| a == "--no-backfill");
    let everything = !args.iter().any(|a| a == "--no-everything-ipc");
    Ok((interval, backfill, everything))
}

fn same_command(a: &str, b: &str) -> bool {
    let left = split_command(a);
    let right = split_command(b);
    !left.is_empty() && left == right
}
fn quote_windows_arg(arg: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for ch in arg.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        let count = if ch == '"' { slashes * 2 + 1 } else { slashes };
        out.extend(std::iter::repeat_n('\\', count));
        out.push(ch);
        slashes = 0;
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}
fn apply_preferences(
    s: &Snapshot,
    p: &crate::settings_logic::FindxGuiSettings,
) -> Result<String, String> {
    let args = runtime_arguments(s, p)?;
    let command = args
        .iter()
        .map(|a| quote_windows_arg(a))
        .collect::<Vec<_>>()
        .join(" ");
    let official = PathBuf::from(std::env::var_os("APPDATA").ok_or("未找到 APPDATA")?)
        .join("tools.findx.gui/findx2-gui-settings.json");
    let old = std::fs::read(&official)
        .map_err(|e| format!("正式配置读取失败（不会创建覆盖未知配置）：{e}"))?;
    let mut value: serde_json::Value = serde_json::from_slice(&old).map_err(|e| e.to_string())?;
    let object = value.as_object_mut().ok_or("正式配置不是 JSON 对象")?;
    object.insert(
        "saveIntervalSecs".into(),
        p.save_interval_secs.max(1).into(),
    );
    object.insert(
        "enableMetadataBackfill".into(),
        p.enable_metadata_backfill.into(),
    );
    object.insert("enableEverythingIpc".into(), p.enable_everything_ipc.into());
    let backup = official.with_file_name(format!(
        "findx2-gui-settings.native-backup-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ));
    // Backup is created before changing either authoritative location; never overwrite an old backup.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup)
        .map_err(|e| e.to_string())?;
    f.write_all(&old).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);
    sc(&["config", "FindX2Search", "binPath=", &command])?;
    let result = (|| {
        let applied = snapshot()?;
        if !same_command(&applied.path_name, &command) {
            return Err(format!(
                "SCM 配置回读不一致：已写入 {command}，回读 {}",
                applied.path_name
            ));
        }
        crate::prefs::write_atomic(
            &official,
            &serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
        )?;
        let read: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&official).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if read != value {
            return Err("正式 JSON 回读不一致".into());
        }
        Ok::<(), String>(())
    })();
    if let Err(e) = result {
        let restored = sc(&["config", "FindX2Search", "binPath=", &s.path_name]);
        return Err(format!(
            "应用失败：{e}；SCM 回滚：{restored:?}；配置备份 {}",
            backup.display()
        ));
    }
    Ok(format!(
        "已更新正式 SCM 与正式 JSON 中的落盘间隔/回填/Everything，其他字段保留。当前进程不重启，下次启动生效。备份 {}",
        backup.display()
    ))
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
    #[test]
    fn runtime_settings_preserve_index_pipe_and_unknown_flags() {
        let exe = PathBuf::from(
            std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into()),
        )
        .join("FindX/findx2-service.exe");
        let s = Snapshot {
            name: "FindX2Search".into(),
            state: "Running".into(),
            start_mode: "Auto".into(),
            process_id: 1,
            path_name: format!(
                "\"{}\" --service --index C:\\ProgramData\\FindX\\index.bin --volume C: --pipe findx2 --save-interval-secs 30 --custom-flag",
                exe.display()
            ),
        };
        let mut p = crate::settings_logic::FindxGuiSettings::isolated_default(
            "findx2-native-dev",
            "NEVER-PRODUCTION".into(),
        );
        p.save_interval_secs = 45;
        let a = runtime_arguments(&s, &p).unwrap();
        assert!(a.contains(&"--custom-flag".into()));
        assert!(!a.contains(&"NEVER-PRODUCTION".into()));
        assert!(a.windows(2).any(|v| v == ["--pipe", "findx2"]));
        assert!(a.windows(2).any(|v| v == ["--save-interval-secs", "45"]));
    }
    #[test]
    fn differently_quoted_commands_are_equivalent() {
        let exe = r"C:\Program Files\FindX\findx2-service.exe";
        let compact = format!(r#""{exe}" --service --pipe findx2 --save-interval-secs 30"#);
        let quoted =
            format!(r#""{exe}" "--service" "--pipe" "findx2" "--save-interval-secs" "30""#);
        assert!(same_command(&compact, &quoted));
        assert!(!same_command(&compact, &format!("{compact} --custom-flag")));
    }
    #[cfg(windows)]
    #[test]
    fn official_identity_is_installed_findx2search() {
        let s = snapshot().expect("读取正式 FindX2Search 快照");
        validate(&s).expect("正式服务身份校验");
        assert_eq!(s.name, "FindX2Search");
        assert!(
            split_command(&s.path_name)
                .windows(2)
                .any(|a| a == ["--pipe", "findx2"])
        );
    }
    #[cfg(windows)]
    #[test]
    #[ignore = "no-op Start against the official service when it is already Running; never starts a stopped production service"]
    fn official_start_is_noop_when_already_running() {
        let s = snapshot().expect("读取正式 FindX2Search 快照");
        validate(&s).expect("正式服务身份校验");
        assert_eq!(s.state, "Running", "正式服务当前未运行；跳过以免误启动");
        let before = s.clone();
        let prefs = crate::settings_logic::FindxGuiSettings::isolated_default(
            "findx2-native-dev",
            "NEVER-PRODUCTION".into(),
        );
        execute(Action::Start, s, prefs).expect("已运行时启动应为无改动成功");
        let after = snapshot().expect("再次读取正式快照");
        assert_eq!(after.process_id, before.process_id);
        assert_eq!(after.path_name, before.path_name);
        assert_eq!(after.state, "Running");
        assert_eq!(after.start_mode, before.start_mode);
    }

    #[test]
    fn official_runtime_options_read_flags_and_interval() {
        let exe = PathBuf::from(
            std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into()),
        )
        .join("FindX/findx2-service.exe");
        let s = Snapshot {
            name: "FindX2Search".into(),
            state: "Running".into(),
            start_mode: "Auto".into(),
            process_id: 1,
            path_name: format!(
                r#""{}" --service --index C:\ProgramData\FindX\index.bin --pipe findx2 --save-interval-secs 15 --no-backfill --no-everything-ipc"#,
                exe.display()
            ),
        };
        assert_eq!(official_runtime_options(&s).unwrap(), (15, false, false));
        let s = Snapshot {
            name: "FindX2Search".into(),
            state: "Running".into(),
            start_mode: "Auto".into(),
            process_id: 1,
            path_name: format!(
                r#""{}" --service --index C:\ProgramData\FindX\index.bin --pipe findx2 --save-interval-secs 30"#,
                exe.display()
            ),
        };
        assert_eq!(official_runtime_options(&s).unwrap(), (30, true, true));
    }

    #[cfg(windows)]
    #[test]
    fn runtime_command_quotes_roundtrip() {
        let v = vec![
            r"C:\Program Files\FindX\service.exe",
            r"C:\",
            r"directory with spaces\",
            "quote\"inside",
        ];
        let command = v
            .iter()
            .map(|s| quote_windows_arg(s))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(split_command(&command), v);
    }
}
