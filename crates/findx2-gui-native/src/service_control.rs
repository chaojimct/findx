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
            Self::Start => "启动服务",
            Self::Stop => "停止服务",
            Self::Auto => "服务开机自启",
            Self::Manual => "服务手动启动",
            Self::GuiAutoOn => "登录时自动启动 FindX",
            Self::GuiAutoOff => "关闭登录自启",
            Self::Compact => "备份后压缩索引",
            Self::Rebuild => "备份后重建索引",
            Self::ApplyPreferences => "应用已保存的服务选项",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Auto => "auto",
            Self::Manual => "manual",
            Self::GuiAutoOn => "gui-auto-on",
            Self::GuiAutoOff => "gui-auto-off",
            Self::Compact => "compact",
            Self::Rebuild => "rebuild",
            Self::ApplyPreferences => "apply-preferences",
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "start" => Self::Start,
            "stop" => Self::Stop,
            "auto" => Self::Auto,
            "manual" => Self::Manual,
            "gui-auto-on" => Self::GuiAutoOn,
            "gui-auto-off" => Self::GuiAutoOff,
            "compact" => Self::Compact,
            "rebuild" => Self::Rebuild,
            "apply-preferences" => Self::ApplyPreferences,
            _ => return None,
        })
    }

    /// SCM start/stop/config and index maintenance need an elevated token on Windows.
    /// Login autostart only writes the current user's HKCU Run key.
    pub fn needs_elevation(self) -> bool {
        !matches!(self, Self::GuiAutoOn | Self::GuiAutoOff)
    }

    /// Start, stop and the two switches are themselves the confirmation.
    /// Maintenance still asks, because it rewrites the index.
    pub fn needs_confirmation(self) -> bool {
        matches!(
            self,
            Self::Compact | Self::Rebuild | Self::ApplyPreferences
        )
    }
}

fn completed(action: Action) -> String {
    match action {
        Action::Start => "服务已启动".into(),
        Action::Stop => "服务已停止".into(),
        Action::Auto => "已设为开机自启".into(),
        Action::Manual => "已改为手动启动".into(),
        Action::GuiAutoOn => "登录时将自动启动 FindX".into(),
        Action::GuiAutoOff => "已关闭登录时自动启动".into(),
        Action::Compact | Action::Rebuild | Action::ApplyPreferences => {
            format!("{}完成", action.label())
        }
    }
}

/// UAC cancel is Win32 ERROR_CANCELLED (1223). Keep the mapping pure so tests
/// can lock the user-facing copy without a consent prompt.
pub fn elevation_failure(code: u32) -> String {
    let win32 = if code & 0xFFFF_0000 == 0x8007_0000 {
        code & 0xFFFF
    } else {
        code
    };
    if win32 == 1223 {
        "已取消管理员授权，服务未更改".into()
    } else {
        format!("无法请求管理员权限（{win32}）")
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
pub const LINUX_UNIT: &str = "findx2.service";
pub const LINUX_SERVICE_NAME: &str = "findx2";
pub const LINUX_SERVICE_EXE: &str = "/opt/FindX/findx2-service";
pub const MACOS_LABEL: &str = "com.findx.findx2";
pub const MACOS_SERVICE_EXE: &str = "/Applications/FindX.app/Contents/MacOS/findx2-service";

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
    #[cfg(target_os = "linux")]
    {
        linux_snapshot()
    }
    #[cfg(target_os = "macos")]
    {
        macos_snapshot()
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Err("当前平台暂不支持正式服务管理".into())
    }
}

pub fn parse_linux_systemctl_show(text: &str) -> Result<Snapshot, String> {
    let mut map = std::collections::BTreeMap::<String, String>::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let load = map.get("LoadState").map(|s| s.as_str()).unwrap_or("");
    if load == "not-found" {
        return Err("未找到正式 findx2 用户服务（/usr/lib/systemd/user/findx2.service）".into());
    }
    let id = map.get("Id").cloned().unwrap_or_default();
    if id != LINUX_UNIT && id != LINUX_SERVICE_NAME {
        if id.is_empty() && load.is_empty() && !map.contains_key("ExecStart") {
            return Err("systemctl 未返回正式 findx2 用户服务".into());
        }
        if !id.is_empty() && id != LINUX_UNIT {
            return Err(format!("拒绝操作非官方单元：{id}"));
        }
    }
    let active = map
        .get("ActiveState")
        .map(|s| s.as_str())
        .unwrap_or("unknown");
    let state = match active {
        "active" => "Running",
        "inactive" | "failed" => "Stopped",
        other => other,
    }
    .to_string();
    let start_mode = match map.get("UnitFileState").map(|s| s.as_str()).unwrap_or("") {
        "enabled" | "enabled-runtime" => "Auto",
        _ => "Manual",
    }
    .to_string();
    let pid = map.get("MainPID").and_then(|s| s.parse().ok()).unwrap_or(0);
    let path_name = map
        .get("ExecStart")
        .map(|s| extract_exec_argv(s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| LINUX_SERVICE_EXE.to_string());
    let s = Snapshot {
        name: LINUX_SERVICE_NAME.into(),
        state,
        start_mode,
        path_name,
        process_id: pid,
    };
    validate_linux(&s)?;
    Ok(s)
}

fn extract_exec_argv(exec_start: &str) -> String {
    if let Some(rest) = exec_start.split("argv[]=").nth(1) {
        let raw = rest.split(';').next().unwrap_or(rest).trim();
        return raw.to_string();
    }
    if let Some(rest) = exec_start.split("path=").nth(1) {
        return rest.split(';').next().unwrap_or(rest).trim().to_string();
    }
    exec_start.trim().to_string()
}

fn official_linux_command_line() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    format!("{LINUX_SERVICE_EXE} --index {home}/.local/share/FindX/index.bin --pipe findx2")
}

fn official_macos_command_line() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/var/root".into());
    format!(
        "{MACOS_SERVICE_EXE} --index {home}/Library/Application Support/FindX/index.bin --pipe findx2"
    )
}

#[cfg(target_os = "linux")]
fn linux_snapshot() -> Result<Snapshot, String> {
    let mut c = Command::new("systemctl");
    c.args([
        "--user",
        "show",
        LINUX_UNIT,
        "--property=Id,LoadState,ActiveState,UnitFileState,ExecStart,MainPID",
        "--no-page",
    ]);
    let text = output(c).map_err(|e| format!("未找到正式 findx2 用户服务：{e}"))?;
    // systemctl's human ExecStart representation is not a lossless argv
    // format (notably HOME paths with spaces). Read the typed D-Bus array.
    let mut c = Command::new("busctl");
    c.args([
        "--user",
        "--json=short",
        "get-property",
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1/unit/findx2_2eservice",
        "org.freedesktop.systemd1.Service",
        "ExecStart",
    ]);
    let value: serde_json::Value = serde_json::from_str(&output(c)?).map_err(|e| e.to_string())?;
    let args = linux_dbus_arguments(&value)?;
    let command = join_unix_args(&args)?;
    let text = text
        .lines()
        .filter(|l| !l.starts_with("ExecStart="))
        .collect::<Vec<_>>()
        .join("\n");
    parse_linux_systemctl_show(&format!("{text}\nExecStart={command}\n"))
}

pub fn linux_dbus_arguments(value: &serde_json::Value) -> Result<Vec<String>, String> {
    fn collect(value: &serde_json::Value, found: &mut Vec<Vec<String>>) {
        if let Some(array) = value.as_array() {
            if array.first().and_then(|v| v.as_str()).is_some() {
                if let Some(args) = array.get(1).and_then(|v| v.as_array()) {
                    if args.iter().all(|v| v.is_string()) {
                        found.push(
                            args.iter()
                                .map(|v| v.as_str().unwrap().to_string())
                                .collect(),
                        );
                        return;
                    }
                }
            }
            for item in array {
                collect(item, found);
            }
        } else if let Some(object) = value.as_object() {
            for item in object.values() {
                collect(item, found);
            }
        }
    }
    let mut found = vec![];
    collect(value, &mut found);
    if found.len() != 1 {
        return Err("无法唯一解析正式 systemd ExecStart 参数数组".into());
    }
    Ok(found.remove(0))
}

pub fn parse_macos_launchctl_print(
    text: &str,
    path_name: &str,
    pid: u32,
) -> Result<Snapshot, String> {
    let state = if text.contains("state = running") || text.contains("\"running\"") {
        "Running"
    } else {
        "Stopped"
    }
    .to_string();
    let start_mode = if text.contains("run at load = true") || text.contains("KeepAlive") {
        "Auto"
    } else {
        "Manual"
    }
    .to_string();
    let s = Snapshot {
        name: MACOS_LABEL.into(),
        state,
        start_mode,
        path_name: path_name.to_string(),
        process_id: pid,
    };
    validate_macos(&s)?;
    Ok(s)
}

#[cfg(target_os = "macos")]
fn macos_snapshot() -> Result<Snapshot, String> {
    let uid = unsafe { libc::getuid() };
    let mut c = Command::new("launchctl");
    c.args(["print", &format!("gui/{uid}/{MACOS_LABEL}")]);
    let text = output(c).unwrap_or_default();
    let value = macos_agent_configuration()?;
    let args = value
        .get("ProgramArguments")
        .and_then(|v| v.as_array())
        .ok_or("LaunchAgent 缺少 ProgramArguments")?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("LaunchAgent 参数不是字符串")
        })
        .collect::<Result<Vec<_>, _>>()?;
    if value.get("Label").and_then(|v| v.as_str()) != Some(MACOS_LABEL) {
        return Err("LaunchAgent Label 不匹配".into());
    }
    let path_name = join_unix_args(&args)?;
    let pid = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("pid = ").and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    let mut snapshot = parse_macos_launchctl_print(&text, &path_name, pid)?;
    let mut disabled = Command::new("launchctl");
    disabled.args(["print-disabled", &format!("gui/{uid}")]);
    let disabled = output(disabled)?;
    let explicitly_disabled = disabled
        .lines()
        .any(|line| line.contains(&format!("\"{MACOS_LABEL}\"")) && line.contains("=> true"));
    snapshot.start_mode = if macos_plist_path()?.is_file()
        && !explicitly_disabled
        && (value.get("RunAtLoad").and_then(|v| v.as_bool()) == Some(true)
            || value.get("KeepAlive").and_then(|v| v.as_bool()) == Some(true))
    {
        "Auto"
    } else {
        "Manual"
    }
    .into();
    Ok(snapshot)
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
    #[cfg(windows)]
    {
        validate_windows(s)
    }
    #[cfg(target_os = "linux")]
    {
        validate_linux(s)
    }
    #[cfg(target_os = "macos")]
    {
        validate_macos(s)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = s;
        Err("当前平台暂不支持正式服务管理".into())
    }
}

pub fn validate_windows(s: &Snapshot) -> Result<(), String> {
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

fn validate_unix_storage(args: &[String]) -> Result<(), String> {
    if args.iter().filter(|a| a.as_str() == "--index").count() != 1
        || args.iter().filter(|a| a.as_str() == "--pipe").count() != 1
    {
        return Err("Unix 服务必须提供唯一明确的 --index / --pipe".into());
    }
    let index = args
        .windows(2)
        .find(|a| a[0] == "--index")
        .map(|a| a[1].as_str())
        .ok_or("服务索引未知")?;
    if !index.starts_with('/')
        || index
            .split('/')
            .any(|c| c == "FindX-native-dev" || c == "..")
    {
        return Err("正式 Unix 服务拒绝相对路径/native-dev/越界索引".into());
    }
    Ok(())
}

pub fn validate_linux(s: &Snapshot) -> Result<(), String> {
    let args = split_command(&s.path_name);
    validate_unix_storage(&args)?;
    let exe = args.first().map(|s| s.as_str()).unwrap_or("");
    let name_ok = s.name == LINUX_SERVICE_NAME || s.name == LINUX_UNIT;
    if !name_ok
        || exe != LINUX_SERVICE_EXE
        || !args
            .windows(2)
            .any(|a| a[0] == "--pipe" && a[1] == "findx2")
    {
        return Err("服务身份/安装路径/pipe 不匹配；拒绝操作任何其他服务".into());
    }
    Ok(())
}

pub fn validate_macos(s: &Snapshot) -> Result<(), String> {
    let args = split_command(&s.path_name);
    validate_unix_storage(&args)?;
    let exe = args.first().map(|s| s.as_str()).unwrap_or("");
    if s.name != MACOS_LABEL
        || exe != MACOS_SERVICE_EXE
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
fn same_snapshot(current: &Snapshot, expected: &Snapshot) -> bool {
    current.path_name == expected.path_name
        && current.process_id == expected.process_id
        && current.state == expected.state
}

/// Only invoke after the UI shows the verified target. Windows SCM changes from a
/// standard-user GUI are re-run through a hidden elevated copy of this executable.
pub fn execute(
    action: Action,
    expected: Snapshot,
    preferences: crate::settings_logic::FindxGuiSettings,
) -> Result<String, String> {
    let current = snapshot()?;
    if !same_snapshot(&current, &expected) {
        return Err("服务状态已变化，请刷新后再试".into());
    }
    dispatch(action, &current, &preferences)
}

/// Start the identity-checked official service from the search empty state.
/// Preferences are unused for start; the registered command line stays in place.
pub fn start_official() -> Result<String, String> {
    let current = snapshot()?;
    if current.state.eq_ignore_ascii_case("Running") {
        return Ok(completed(Action::Start));
    }
    let preferences =
        crate::settings_logic::FindxGuiSettings::isolated_default("findx2", String::new());
    execute(Action::Start, current, preferences)
}

fn dispatch(
    action: Action,
    current: &Snapshot,
    preferences: &crate::settings_logic::FindxGuiSettings,
) -> Result<String, String> {
    #[cfg(windows)]
    if action.needs_elevation() && !is_elevated() {
        return relaunch_elevated(action, current, preferences);
    }
    perform(action, current, preferences)
}

fn perform(
    action: Action,
    current: &Snapshot,
    preferences: &crate::settings_logic::FindxGuiSettings,
) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    {
        return execute_linux(action, current, preferences);
    }
    #[cfg(target_os = "macos")]
    {
        return execute_macos(action, current, preferences);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        perform_windows(action, current, preferences).map_err(friendly_service_error)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn perform_windows(
    action: Action,
    current: &Snapshot,
    preferences: &crate::settings_logic::FindxGuiSettings,
) -> Result<String, String> {
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
        Action::ApplyPreferences => return apply_preferences(current, preferences),
        Action::Compact | Action::Rebuild => return maintenance(action, current),
    }
    Ok(completed(action))
}

fn friendly_service_error(err: String) -> String {
    let lower = err.to_ascii_lowercase();
    if err.contains("拒绝访问")
        || lower.contains("access is denied")
        || err.contains("退出码 Some(5)")
        || err.contains("退出码 5")
    {
        return "没有权限更改索引服务。若取消了授权提示，服务不会变化。".into();
    }
    err
}

#[cfg(windows)]
fn is_elevated() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TokenElevation};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, TOKEN_QUERY};
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        let _ = CloseHandle(token);
        ok.is_ok() && elevation.TokenIsElevated != 0
    }
}

#[cfg(windows)]
fn relaunch_elevated(
    action: Action,
    expected: &Snapshot,
    preferences: &crate::settings_logic::FindxGuiSettings,
) -> Result<String, String> {
    let dir = std::env::temp_dir().join(format!(
        "findx-service-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let request = dir.join("request.json");
    let payload = serde_json::json!({
        "action": action.key(),
        "expected": expected,
        "preferences": preferences,
    });
    std::fs::write(&request, serde_json::to_vec(&payload).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let params = format!(
        "--elevated-service {}",
        quote_windows_arg(&request.display().to_string())
    );
    let launched = shell_runas(&exe, &params);
    let result_path = request.with_extension("result.json");
    let message = match launched {
        Err(code) => Err(elevation_failure(code)),
        Ok(exit) => read_elevated_result(&result_path, exit),
    };
    let _ = std::fs::remove_dir_all(&dir);
    message
}

#[cfg(windows)]
fn read_elevated_result(path: &Path, exit: u32) -> Result<String, String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let message = value
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or(if exit == 0 {
            "操作完成"
        } else {
            "服务操作失败"
        })
        .to_string();
    if exit == 0 && value.get("ok").and_then(|v| v.as_bool()) != Some(false) {
        Ok(message)
    } else {
        Err(message)
    }
}

#[cfg(windows)]
fn shell_runas(exe: &Path, params: &str) -> Result<u32, u32> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, WAIT_OBJECT_0,
    };
    use windows::Win32::UI::Shell::{
        SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
        ShellExecuteExW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::PCWSTR;
    let file = exe.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    let verb: Vec<u16> = "runas".encode_utf16().chain(Some(0)).collect();
    let parameters: Vec<u16> = params.encode_utf16().chain(Some(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) } {
        return Err(error.code().0 as u32);
    }
    let handle = info.hProcess;
    if handle.is_invalid() {
        return Err(0);
    }
    let waited = unsafe { WaitForSingleObject(handle, 0xFFFF_FFFF) };
    let mut code = 1u32;
    if waited == WAIT_OBJECT_0 {
        let _ = unsafe { GetExitCodeProcess(handle, &mut code) };
    } else {
        code = 1;
    }
    let _ = unsafe { CloseHandle(handle) };
    Ok(code)
}

#[derive(Deserialize)]
struct ElevatedRequest {
    action: String,
    expected: Snapshot,
    preferences: crate::settings_logic::FindxGuiSettings,
}

/// Hidden entry used by the elevated copy. It never opens a window or a console.
pub fn run_elevated_request(path: &Path) -> i32 {
    let outcome = elevated_request_inner(path);
    let body = match &outcome {
        Ok(message) => serde_json::json!({"ok": true, "message": message}),
        Err(message) => serde_json::json!({"ok": false, "message": message}),
    };
    let _ = std::fs::write(
        path.with_extension("result.json"),
        serde_json::to_vec(&body).unwrap_or_default(),
    );
    if outcome.is_ok() { 0 } else { 1 }
}

fn elevated_request_inner(path: &Path) -> Result<String, String> {
    #[cfg(not(windows))]
    {
        let _ = path;
        return Err("当前平台不使用 UAC 提权".into());
    }
    #[cfg(windows)]
    {
        if !is_elevated() {
            return Err("管理员权限未生效，服务未更改".into());
        }
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let request: ElevatedRequest =
            serde_json::from_slice(&bytes).map_err(|e| format!("无法读取提权请求：{e}"))?;
        let action = Action::parse(&request.action).ok_or("拒绝未知的服务操作")?;
        if !action.needs_elevation() {
            return Err("该操作不需要提权".into());
        }
        // Re-validate the live official service inside the elevated process.
        // A swapped request still cannot name any service other than FindX2Search.
        execute(action, request.expected, request.preferences)
    }
}

#[cfg(target_os = "linux")]
fn execute_linux(
    action: Action,
    current: &Snapshot,
    preferences: &crate::settings_logic::FindxGuiSettings,
) -> Result<String, String> {
    validate_linux(current)?;
    match action {
        Action::Start => {
            if current.state != "Running" {
                set_running(true)?;
            }
        }
        Action::Stop => {
            if current.state != "Stopped" {
                set_running(false)?;
            }
        }
        Action::Auto => {
            linux_systemctl(&["enable", LINUX_UNIT])?;
        }
        Action::Manual => {
            linux_systemctl(&["disable", LINUX_UNIT])?;
        }
        Action::GuiAutoOn | Action::GuiAutoOff => {
            let enabled = action == Action::GuiAutoOn;
            let state = crate::autostart::apply(enabled)?;
            if state.enabled != enabled {
                return Err("正式 GUI 自启动回读不一致".into());
            }
        }
        Action::ApplyPreferences => return unix_apply_preferences(current, preferences),
        Action::Compact | Action::Rebuild => return maintenance(action, current),
    }
    Ok(completed(action))
}

fn linux_systemctl(args: &[&str]) -> Result<String, String> {
    let mut c = Command::new("systemctl");
    let mut all = vec!["--user"];
    all.extend_from_slice(args);
    c.args(&all);
    output(c)
}

#[cfg(target_os = "macos")]
fn execute_macos(
    action: Action,
    current: &Snapshot,
    preferences: &crate::settings_logic::FindxGuiSettings,
) -> Result<String, String> {
    validate_macos(current)?;
    match action {
        Action::Start => {
            macos_launchctl_kickstart()?;
        }
        Action::Stop => {
            macos_launchctl_bootout()?;
        }
        Action::Auto | Action::Manual => {
            return macos_set_start_mode(current, action == Action::Auto);
        }
        Action::GuiAutoOn | Action::GuiAutoOff => {
            let enabled = action == Action::GuiAutoOn;
            let state = crate::autostart::apply(enabled)?;
            if state.enabled != enabled {
                return Err("正式 GUI 自启动回读不一致".into());
            }
        }
        Action::ApplyPreferences => return unix_apply_preferences(current, preferences),
        Action::Compact | Action::Rebuild => return maintenance(action, current),
    }
    Ok(completed(action))
}

#[cfg(target_os = "macos")]
fn macos_set_start_mode(current: &Snapshot, auto: bool) -> Result<String, String> {
    ensure_macos_agent_file()?;
    let path = macos_plist_path()?;
    let old = std::fs::read(&path).map_err(|e| e.to_string())?;
    let mut value = macos_agent_configuration()?;
    value["RunAtLoad"] = serde_json::json!(auto);
    value["KeepAlive"] = serde_json::json!(auto);
    let backup = path.with_extension(format!(
        "backup-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(backup)
        .and_then(|mut f| f.write_all(&old))
        .map_err(|e| e.to_string())?;
    let running = current.state == "Running";
    if running {
        set_running(false)?;
    }
    let result = (|| {
        crate::prefs::write_atomic(
            &path,
            &serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
        )?;
        let mut c = Command::new("plutil");
        c.args(["-convert", "xml1"]).arg(&path);
        output(c)?;
        // Manual mode is expressed in the plist, not a disabled label: a disabled
        // job cannot be started on demand. Keep the label usable in both modes.
        let mut c = Command::new("launchctl");
        c.args(["enable", &format!("gui/{}/{MACOS_LABEL}", unix_uid()?)]);
        output(c)?;
        if running {
            set_running(true)?;
        }
        let actual = snapshot()?;
        if actual.start_mode != if auto { "Auto" } else { "Manual" } {
            return Err("LaunchAgent 启动模式回读不一致".into());
        }
        Ok("LaunchAgent 启动模式已保存；原运行状态已恢复".into())
    })();
    if let Err(e) = result {
        if running {
            let _ = set_running(false);
        }
        crate::prefs::write_atomic(&path, &old)?;
        if running {
            set_running(true)?;
        }
        return Err(format!("启动模式应用失败，已恢复 plist: {e}"));
    }
    result
}

fn macos_launchctl_kickstart() -> Result<(), String> {
    let uid = unix_uid()?;
    let mut enable = Command::new("launchctl");
    enable.args(["enable", &format!("gui/{uid}/{MACOS_LABEL}")]);
    output(enable)?;
    let domain = format!("gui/{uid}");
    let plist = macos_plist_path()?;
    ensure_macos_agent_file()?;
    let mut c = Command::new("launchctl");
    c.args(["print", &format!("{domain}/{MACOS_LABEL}")]);
    if output(c).is_err() {
        let mut c = Command::new("launchctl");
        c.arg("bootstrap").arg(&domain).arg(plist);
        output(c)?;
    }
    let mut c = Command::new("launchctl");
    c.args(["kickstart", &format!("{domain}/{MACOS_LABEL}")]);
    output(c).map(|_| ())
}

fn macos_launchctl_bootout() -> Result<(), String> {
    let uid = unix_uid()?;
    let domain = format!("gui/{uid}/{MACOS_LABEL}");
    let mut c = Command::new("launchctl");
    c.args(["bootout", &domain]);
    match output(c) {
        Ok(_) => Ok(()),
        Err(e) if e.contains("No such process") || e.contains("Could not find service") => Ok(()),
        Err(e) => Err(e),
    }
}

// User-service lifecycle operations only. Never kill processes by name or touch SCM.
fn unix_uid() -> Result<u32, String> {
    let mut c = Command::new("id");
    c.arg("-u");
    output(c)?
        .trim()
        .parse()
        .map_err(|_| "无法读取当前用户 UID".into())
}
fn macos_plist_path() -> Result<PathBuf, String> {
    Ok(
        PathBuf::from(std::env::var_os("HOME").ok_or("HOME 未设置")?)
            .join("Library/LaunchAgents/com.findx.findx2.plist"),
    )
}
fn macos_plist_json(path: &Path) -> Result<serde_json::Value, String> {
    let mut c = Command::new("plutil");
    c.args(["-convert", "json", "-o", "-"]).arg(path);
    serde_json::from_str(&output(c)?).map_err(|e| e.to_string())
}
fn ensure_macos_agent_file() -> Result<(), String> {
    let plist = macos_plist_path()?;
    if !plist.exists() {
        // A standard drag-install has no per-user agent yet. Create it only
        // after the user explicitly confirms Start, never at GUI startup.
        let value = macos_agent_configuration()?;
        std::fs::create_dir_all(plist.parent().ok_or("LaunchAgent 无父目录")?)
            .map_err(|e| e.to_string())?;
        crate::prefs::write_atomic(
            &plist,
            &serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
        )?;
        let mut convert = Command::new("plutil");
        convert.args(["-convert", "xml1"]).arg(&plist);
        output(convert)?;
    }
    Ok(())
}

/// Load the user's existing agent verbatim, or prepare the shipped template
/// for a first drag-install. Reading a snapshot must not create/start an agent.
fn macos_agent_configuration() -> Result<serde_json::Value, String> {
    let path = macos_plist_path()?;
    let exists = path.exists();
    let value = macos_plist_json(if exists {
        &path
    } else {
        Path::new("/Applications/FindX.app/Contents/Resources/com.findx.findx2.plist")
    })?;
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    prepare_macos_agent(value, &home, !exists)
}
pub fn prepare_macos_agent(
    mut value: serde_json::Value,
    home: &str,
    from_template: bool,
) -> Result<serde_json::Value, String> {
    if value.get("Label").and_then(|v| v.as_str()) != Some(MACOS_LABEL) {
        return Err("LaunchAgent Label 不匹配".into());
    }
    let args = value
        .get_mut("ProgramArguments")
        .and_then(|v| v.as_array_mut())
        .ok_or("LaunchAgent 缺少 ProgramArguments")?;
    if from_template {
        for arg in args.iter_mut() {
            if let Some(raw) = arg.as_str().and_then(|s| s.strip_prefix("__HOME__/")) {
                *arg = serde_json::json!(format!("{home}/{raw}"));
            }
        }
    }
    let args = args
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("LaunchAgent 参数不是字符串")
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_macos(&Snapshot {
        name: MACOS_LABEL.into(),
        state: "Stopped".into(),
        start_mode: "Manual".into(),
        process_id: 0,
        path_name: join_unix_args(&args)?,
    })?;
    Ok(value)
}

pub fn join_unix_args(args: &[String]) -> Result<String, String> {
    args.iter()
        .map(|s| {
            if s.chars()
                .any(|c| matches!(c, '"' | '\\' | '\0' | '\n' | '\r'))
            {
                return Err("服务参数含有不支持的转义字符；拒绝改写".into());
            }
            Ok(format!("\"{s}\""))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|v| v.join(" "))
}
pub fn systemd_override(args: &[String]) -> Result<String, String> {
    // systemd has specifier and dollar expansion in addition to argument quoting.
    let command = join_unix_args(args)?.replace('%', "%%").replace('$', "$$");
    Ok(format!("[Service]\nExecStart=\nExecStart={command}\n"))
}
pub(crate) fn set_running_for_update(running: bool) -> Result<(), String> {
    set_running(running)
}
fn set_running(running: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        sc(&[if running { "start" } else { "stop" }, "FindX2Search"])?;
    }
    #[cfg(target_os = "linux")]
    {
        linux_systemctl(&[if running { "start" } else { "stop" }, LINUX_UNIT])?;
    }
    #[cfg(target_os = "macos")]
    {
        if running {
            macos_launchctl_kickstart()?
        } else {
            macos_launchctl_bootout()?
        }
    }
    wait_state(if running { "Running" } else { "Stopped" })
}
#[cfg(unix)]
fn unix_apply_preferences(
    s: &Snapshot,
    p: &crate::settings_logic::FindxGuiSettings,
) -> Result<String, String> {
    let args = runtime_arguments(s, p)?;
    // Runtime options may not redirect the authoritative index, pipe or scan scope.
    let args_text = join_unix_args(&args)?;
    if arg(s, "--index")?
        != arg(
            &Snapshot {
                path_name: args_text,
                ..s.clone()
            },
            "--index",
        )?
    {
        return Err("服务索引路径改变，拒绝应用".into());
    }
    #[cfg(target_os = "linux")]
    let (path, bytes) = {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or(
                PathBuf::from(std::env::var_os("HOME").ok_or("HOME 未设置")?).join(".config"),
            );
        (
            root.join("systemd/user/findx2.service.d/90-findx-settings.conf"),
            systemd_override(&args)?.into_bytes(),
        )
    };
    #[cfg(target_os = "macos")]
    let (path, bytes) = {
        let path = macos_plist_path()?;
        let mut value = macos_plist_json(&path)?;
        value["ProgramArguments"] = serde_json::json!(args);
        (
            path,
            serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
        )
    };
    let old = match std::fs::read(&path) {
        Ok(v) => Some(v),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // A unique sibling backup survives success and failure. Never overwrite an older backup.
    if let Some(ref bytes) = old {
        use std::io::Write;
        let backup = path.with_extension(format!(
            "backup-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
            .and_then(|mut f| f.write_all(bytes))
            .map_err(|e| e.to_string())?;
    }
    let running = s.state == "Running";
    if running {
        set_running(false)?;
    }
    let reload = || -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            linux_systemctl(&["daemon-reload"])?;
        }
        Ok(())
    };
    let result = (|| {
        crate::prefs::write_atomic(&path, &bytes)?;
        #[cfg(target_os = "macos")]
        {
            let mut c = Command::new("plutil");
            c.args(["-convert", "xml1"]).arg(&path);
            output(c)?;
        }
        reload()?;
        if running {
            set_running(true)?;
        }
        let actual = snapshot()?;
        if official_runtime_options(&actual)?
            != (
                p.save_interval_secs.max(5),
                p.enable_metadata_backfill,
                p.enable_everything_ipc,
            )
        {
            return Err("服务参数回读与请求不一致".into());
        }
        Ok("服务选项已应用并回读；原索引/扫描范围不变，原启动状态已恢复".into())
    })();
    if let Err(error) = result {
        // Stop only our exact registered service before restoring its config.
        if running {
            let _ = set_running(false);
        }
        let restore = (|| {
            match old {
                Some(ref bytes) => crate::prefs::write_atomic(&path, bytes)?,
                None => std::fs::remove_file(&path).map_err(|e| e.to_string())?,
            }
            reload()?;
            if running {
                set_running(true)?;
            }
            Ok::<_, String>(())
        })();
        return Err(format!("应用失败: {error}；配置/服务恢复结果: {restore:?}"));
    }
    result
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
const SUFFIXES: [&str; 5] = ["", ".v6", ".exclude.json", ".tri", ".tri.pending"];
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
    validate_snapshot_identity(s)?;
    let index = PathBuf::from(arg(s, "--index")?);
    // Validate rebuild scope before stopping anything. Never guess a full-disk root.
    if action == Action::Rebuild {
        let volume = arg(s, "--volume")?;
        if cfg!(unix) && volume.split(',').any(|v| !v.starts_with('/')) {
            return Err("Unix 重建范围必须为明确绝对根路径".into());
        }
    }
    if cfg!(unix)
        && index
            .components()
            .any(|c| c.as_os_str() == "FindX-native-dev")
    {
        return Err("正式服务不得指向 native-dev 索引".into());
    }
    if !index.is_absolute() {
        return Err("服务索引路径非绝对路径，拒绝维护".into());
    }
    let exe = PathBuf::from(split_command(&s.path_name)[0].clone());
    let cli = exe
        .parent()
        .ok_or("服务路径无父目录")?
        .join(if cfg!(windows) {
            "findx2.exe"
        } else {
            "findx2"
        });
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
        set_running(false)?;
    }
    let result = maintenance_offline(action, s, &index, &cli);
    let restart = if running { set_running(true) } else { Ok(()) };
    match (result, restart) {
        (Ok(m), Ok(())) => Ok(format!("{m}；服务状态已恢复")),
        (Err(e), Ok(())) => Err(format!("{e}；服务状态已恢复")),
        (r, Err(e)) => Err(format!("维护结果 {:?}；服务恢复失败：{e}", r)),
    }
}

fn maintenance_offline(
    action: Action,
    s: &Snapshot,
    index: &Path,
    cli: &Path,
) -> Result<String, String> {
    (|| {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
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
            let source_args = split_command(&s.path_name);
            if source_args.iter().any(|a| a == "--full-stat") {
                c.arg("index").arg("--full-stat");
            } else {
                c.arg("index");
            }
            for pair in source_args.windows(2) {
                if pair[0] == "--exclude-dir" || pair[0] == "--max-scan-threads" {
                    c.arg(&pair[0]).arg(&pair[1]);
                }
            }
            c.arg("--output")
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
    })()
}

/// Preserve every unrelated CLI flag and official JSON field. Only these three
/// runtime settings are owned by this operation; dev paths/UI prefs never migrate.
pub fn validate_snapshot_identity(s: &Snapshot) -> Result<(), String> {
    if s.name == "FindX2Search" {
        validate_windows(s)
    } else if s.name == LINUX_SERVICE_NAME || s.name == LINUX_UNIT {
        validate_linux(s)
    } else if s.name == MACOS_LABEL {
        validate_macos(s)
    } else {
        Err("服务身份/安装路径/pipe 不匹配；拒绝操作任何其他服务".into())
    }
}

pub fn runtime_arguments(
    s: &Snapshot,
    p: &crate::settings_logic::FindxGuiSettings,
) -> Result<Vec<String>, String> {
    validate_snapshot_identity(s)?;
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
    next.push(
        p.save_interval_secs
            .max(if s.name == "FindX2Search" { 1 } else { 5 })
            .to_string(),
    );
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
    validate_snapshot_identity(s)?;
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
    fn privileged_actions_stay_explicit_and_cancel_is_not_a_failure_dump() {
        assert!(Action::Start.needs_elevation());
        assert!(Action::Stop.needs_elevation());
        assert!(Action::Auto.needs_elevation());
        assert!(Action::Manual.needs_elevation());
        assert!(Action::Compact.needs_elevation());
        assert!(Action::Rebuild.needs_elevation());
        assert!(Action::ApplyPreferences.needs_elevation());
        assert!(!Action::GuiAutoOn.needs_elevation());
        assert!(!Action::GuiAutoOff.needs_elevation());
        assert!(!Action::Start.needs_confirmation());
        assert!(Action::Rebuild.needs_confirmation());
        assert_eq!(Action::parse("start"), Some(Action::Start));
        assert!(Action::parse("kill-all").is_none());
        assert!(elevation_failure(1223).contains("已取消"));
        assert!(elevation_failure(0x8007_04C7).contains("已取消"));
        assert!(!elevation_failure(1223).contains("·"));
        let denied = friendly_service_error("退出码 Some(5): Access is denied.".into());
        assert!(denied.contains("没有权限"));
        assert!(!denied.contains("Access is denied"));
    }
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
    #[ignore = "opt-in read-only installed-service identity probe; normal unit/CI tests must not require a production FindX2Search service"]
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

    #[test]
    fn linux_and_macos_official_service_identity() {
        let linux = Snapshot {
            name: LINUX_SERVICE_NAME.into(),
            state: "Running".into(),
            start_mode: "Auto".into(),
            process_id: 9,
            path_name: format!("{LINUX_SERVICE_EXE} --index /tmp/index.bin --pipe findx2"),
        };
        assert!(validate_linux(&linux).is_ok());
        let evil = Snapshot {
            name: LINUX_SERVICE_NAME.into(),
            state: "Running".into(),
            start_mode: "Auto".into(),
            process_id: 9,
            path_name: "/tmp/evil --pipe findx2".into(),
        };
        assert!(validate_linux(&evil).is_err());
        let mac = Snapshot {
            name: MACOS_LABEL.into(),
            state: "Stopped".into(),
            start_mode: "Auto".into(),
            process_id: 0,
            path_name: format!("{MACOS_SERVICE_EXE} --index /tmp/index.bin --pipe findx2"),
        };
        assert!(validate_macos(&mac).is_ok());
        let show = concat!(
            "Id=findx2.service
",
            "LoadState=loaded
",
            "ActiveState=active
",
            "UnitFileState=enabled
",
            "MainPID=4242
",
            "ExecStart={ path=/opt/FindX/findx2-service ; argv[]=/opt/FindX/findx2-service --index /tmp/index.bin --pipe findx2 ; ignore_errors=no }
",
        );
        let parsed = parse_linux_systemctl_show(show).unwrap();
        assert_eq!(parsed.name, LINUX_SERVICE_NAME);
        assert_eq!(parsed.state, "Running");
        assert_eq!(parsed.start_mode, "Auto");
        assert_eq!(parsed.process_id, 4242);
        assert!(parsed.path_name.contains(LINUX_SERVICE_EXE));
        assert!(parsed.path_name.contains("--pipe"));
        let missing = parse_linux_systemctl_show(
            "LoadState=not-found
Id=findx2.service
",
        );
        assert!(missing.is_err());
    }
}

#[cfg(test)]
mod unix_completion_tests {
    use super::*;
    #[test]
    fn systemd_options_quote_and_preserve_scope() {
        let s = Snapshot { name: "findx2".into(), state: "Stopped".into(), start_mode: "Manual".into(), process_id: 0,
            path_name: "/opt/FindX/findx2-service --pipe findx2 --index \"/home/test user/.local/share/FindX/index.bin\" --volume /fixture --exclude-dir /fixture/skip".into() };
        let prefs =
            crate::settings_logic::FindxGuiSettings::isolated_default("unused", "unused".into());
        let args = runtime_arguments(&s, &prefs).unwrap();
        assert_eq!(split_command(&join_unix_args(&args).unwrap()), args);
        assert!(args.windows(2).any(|a| a == ["--volume", "/fixture"]));
        assert!(
            args.windows(2)
                .any(|a| a == ["--exclude-dir", "/fixture/skip"])
        );
        let text =
            systemd_override(&["/opt/FindX/findx2-service".into(), "$HOME/%n".into()]).unwrap();
        assert!(text.contains("ExecStart=\nExecStart="));
        assert!(text.contains("$$HOME/%%n"));
        assert!(join_unix_args(&["bad\nargument".into()]).is_err());
        assert!(join_unix_args(&["bad\"argument".into()]).is_err());
    }
    #[test]
    #[cfg(unix)]
    fn real_cli_maintenance_tiny_fixture() {
        let Ok(cli) = std::env::var("FINDX_TEST_CLI") else {
            return;
        };
        let dir =
            std::env::temp_dir().join(format!("findx-maintenance-test-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let volume = dir.join("volume");
        std::fs::create_dir(&volume).unwrap();
        std::fs::write(volume.join("alpha-fixture.txt"), "one").unwrap();
        let index = dir.join("index.bin");
        let mut c = Command::new(&cli);
        c.arg("index")
            .arg("--output")
            .arg(&index)
            .arg("--volume")
            .arg(&volume);
        output(c).unwrap();
        let original = std::fs::read(&index).unwrap();
        let s = Snapshot {
            name: "findx2".into(),
            state: "Stopped".into(),
            start_mode: "Manual".into(),
            process_id: 0,
            path_name: format!(
                "/opt/FindX/findx2-service --pipe findx2 --index {} --volume {}",
                index.display(),
                volume.display()
            ),
        };
        maintenance_offline(Action::Compact, &s, &index, Path::new(&cli)).unwrap();
        // Failure cannot promote a broken staged index or touch the current one.
        let compacted = std::fs::read(&index).unwrap();
        assert!(
            maintenance_offline(Action::Rebuild, &s, &index, Path::new("/nonexistent-cli"))
                .is_err()
        );
        assert_eq!(std::fs::read(&index).unwrap(), compacted);
        std::fs::write(volume.join("beta-fixture.txt"), "two").unwrap();
        maintenance_offline(Action::Rebuild, &s, &index, Path::new(&cli)).unwrap();
        let mut c = Command::new(&cli);
        c.arg("search")
            .arg("beta-fixture")
            .arg("--index")
            .arg(&index);
        assert!(output(c).unwrap().contains("beta-fixture"));
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e
                    .file_name()
                    .to_string_lossy()
                    .starts_with("native-maintenance-"))
                .any(|e| std::fs::read(e.path().join("backup/index.bin")).ok()
                    == Some(original.clone()))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod unix_preferences_fixture {
    use super::*;
    #[test]
    fn simulated_user_manager_transaction_roundtrip() {
        let Ok(root) = std::env::var("FINDX_TEST_USER_MANAGER") else {
            return;
        };
        let root = PathBuf::from(root);
        let before_index = std::fs::read(root.join("index.bin")).unwrap();
        let original = snapshot().unwrap();
        assert_eq!(original.state, "Running");
        let mut prefs =
            crate::settings_logic::FindxGuiSettings::isolated_default("unused", "unused".into());
        prefs.save_interval_secs = 77;
        prefs.enable_metadata_backfill = false;
        prefs.enable_everything_ipc = false;
        execute(Action::ApplyPreferences, original, prefs.clone()).unwrap();
        let updated = snapshot().unwrap();
        assert_eq!(updated.state, "Running");
        assert_eq!(
            official_runtime_options(&updated).unwrap(),
            (77, false, false)
        );
        let config = root.join("config/systemd/user/findx2.service.d/90-findx-settings.conf");
        let saved = std::fs::read(&config).unwrap();
        std::fs::write(root.join("fail-next-start"), "").unwrap();
        prefs.save_interval_secs = 88;
        assert!(execute(Action::ApplyPreferences, updated, prefs.clone()).is_err());
        assert_eq!(std::fs::read(&config).unwrap(), saved);
        assert_eq!(snapshot().unwrap().state, "Running");
        assert_eq!(std::fs::read(root.join("index.bin")).unwrap(), before_index);
        set_running(false).unwrap();
        let stopped = snapshot().unwrap();
        execute(Action::ApplyPreferences, stopped, prefs).unwrap();
        assert_eq!(snapshot().unwrap().state, "Stopped");
    }
}

#[cfg(test)]
mod macos_first_install_tests {
    use super::*;
    #[test]
    fn template_agent_preserves_home_spaces_and_existing_options() {
        let template = serde_json::json!({"Label":MACOS_LABEL,"ProgramArguments":[MACOS_SERVICE_EXE,"--index","__HOME__/Library/Application Support/FindX/index.bin","--pipe","findx2","--volume","/"],"KeepAlive":true});
        let ready = prepare_macos_agent(template, "/Users/fixture user", true).unwrap();
        assert_eq!(
            ready["ProgramArguments"][2],
            "/Users/fixture user/Library/Application Support/FindX/index.bin"
        );
        assert_eq!(
            prepare_macos_agent(ready.clone(), "/Users/another", false).unwrap(),
            ready
        );
        let mut wrong = ready;
        wrong["ProgramArguments"][2] =
            serde_json::json!("/Users/fixture/FindX-native-dev/index.bin");
        assert!(prepare_macos_agent(wrong, "/Users/fixture", false).is_err());
    }
}
