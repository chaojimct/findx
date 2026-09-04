//! FindX2 托盘 GUI 设置（与 dotnet 版字段对齐）及拉起 findx2-service。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{Manager, Runtime};

/// NSIS 安装程序在 `$INSTDIR` 下写入；存在时首启使用「ProgramData 索引 + 服务模式」。
#[cfg(windows)]
const FINDX_INSTALLED_MARKER: &str = "FindX.installed";

fn default_true() -> bool {
    true
}

/// GUI 与 service 的协同方式。
/// - `Service`（默认）：GUI 普通用户运行；只通过 SCM 拉起 `FindX2Search`（SYSTEM，才能读 USN）。
///   不在该模式下用当前用户权限直拉 `findx2-service`，否则必现「打开卷失败: 拒绝访问」。
/// - `Standalone`：GUI 与 service 跑在同一会话里；首次启动允许 ShellExecute runas 提权 spawn service。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    Service,
    Standalone,
}

impl Default for RunMode {
    fn default() -> Self {
        RunMode::Service
    }
}

fn default_save_interval() -> u64 {
    30
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindxGuiSettings {
    pub index_path: String,
    pub volume: String,
    pub pipe_name: String,
    pub pinyin_default: bool,
    pub service_exe_path: String,
    pub search_limit: u32,
    /// 启动 GUI 时若可能则自动建库/拉服务
    #[serde(default = "default_true")]
    pub auto_start_service: bool,
    /// 与 CLI `--full-stat` 一致；`false` 为快速首遍（默认）
    #[serde(default)]
    pub first_index_full_metadata: bool,
    /// 服务运行模式（默认 `Service`）；用户可在设置里切换为 `Standalone` 单体 UAC。
    #[serde(default)]
    pub run_mode: RunMode,
    /// 索引磁盘列表（如 `["C:", "D:"]`）；空 = 默认全盘（与 CLI 行为一致）。
    #[serde(default)]
    pub drives: Vec<String>,
    /// 排除目录（用户原样输入，service 启动 / CLI 建索引会做归一）；
    /// 写入 `<index>.exclude.json` 边车，service 加载时读回 `IndexStore.excluded_dirs`。
    #[serde(default)]
    pub excluded_dirs: Vec<String>,
    /// 是否启用「时间/大小」元数据后台回填线程；默认开。关闭后 fast 首遍未覆盖的 size/mtime 会一直为 0，
    /// 但 service 的 CPU/磁盘 IO 占用会显著降低。
    #[serde(default = "default_true")]
    pub enable_metadata_backfill: bool,
    /// 是否启用 Everything SDK v2 兼容窗口；默认开。关闭后 IbEverythingExt 等老客户端将无法接入。
    #[serde(default = "default_true")]
    pub enable_everything_ipc: bool,
    /// USN 落盘间隔（秒）。默认 30；调低增加写盘频率（更安全但更耗 IO）。
    #[serde(default = "default_save_interval")]
    pub save_interval_secs: u64,
}

impl Default for FindxGuiSettings {
    fn default() -> Self {
        Self {
            index_path: "index.bin".into(),
            volume: "C:".into(),
            pipe_name: "findx2".into(),
            pinyin_default: true,
            service_exe_path: String::new(),
            search_limit: 5000,
            auto_start_service: true,
            first_index_full_metadata: false,
            run_mode: RunMode::default(),
            drives: Vec::new(),
            excluded_dirs: Vec::new(),
            enable_metadata_backfill: true,
            enable_everything_ipc: true,
            save_interval_secs: default_save_interval(),
        }
    }
}

fn settings_path<R: Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?
        .join("findx2-gui-settings.json"))
}

/// 与 NSIS 安装器约定：已安装正式包时（同目录存在 `FindX.installed`），首启默认公共索引路径 + 服务模式。
#[cfg(windows)]
fn settings_for_nsis_installed_layout() -> Option<FindxGuiSettings> {
    let base = exe_resource_dir();
    if !base.join(FINDX_INSTALLED_MARKER).exists() {
        return None;
    }
    let pd = std::env::var_os("ProgramData")?;
    let index = Path::new(&pd).join("FindX").join("index.bin");
    let mut s = FindxGuiSettings::default();
    s.index_path = index.to_string_lossy().into_owned();
    s.run_mode = RunMode::Service;
    s.auto_start_service = true;
    Some(s)
}

/// 正式安装后若沿用旧设置里的相对 `index.bin`，会落到 `{app}\index.bin` 旧库，
/// 并误用普通权限进程监听 USN。有 `FindX.installed` 时改走 ProgramData + 服务模式。
#[cfg(windows)]
fn migrate_installed_layout_settings<R: Runtime>(
    app: &tauri::AppHandle<R>,
    mut s: FindxGuiSettings,
) -> FindxGuiSettings {
    let Some(installed) = settings_for_nsis_installed_layout() else {
        return s;
    };
    let base = exe_resource_dir();
    let resolved = resolve_index_path(&base, &s);
    let trimmed = s.index_path.trim();
    let leftover = trimmed.is_empty()
        || !Path::new(trimmed).is_absolute()
        || resolved == base.join("index.bin");
    if !leftover {
        return s;
    }
    if resolved == PathBuf::from(&installed.index_path) && s.run_mode == RunMode::Service {
        return s;
    }
    s.index_path = installed.index_path;
    s.run_mode = RunMode::Service;
    s.auto_start_service = true;
    let _ = save_findx_settings(app.clone(), s.clone());
    s
}

#[tauri::command]
pub fn load_findx_settings<R: Runtime>(app: tauri::AppHandle<R>) -> Result<FindxGuiSettings, String> {
    let path = settings_path(&app)?;
    if !path.exists() {
        #[cfg(windows)]
        if let Some(s) = settings_for_nsis_installed_layout() {
            return Ok(s);
        }
        return Ok(FindxGuiSettings::default());
    }
    let s = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let parsed = serde_json::from_str(&s).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        return Ok(migrate_installed_layout_settings(&app, parsed));
    }
    #[cfg(not(windows))]
    {
        Ok(parsed)
    }
}

#[tauri::command]
pub fn save_findx_settings<R: Runtime>(
    app: tauri::AppHandle<R>,
    settings: FindxGuiSettings,
) -> Result<(), String> {
    let path = settings_path(&app)?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let s = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, s).map_err(|e| e.to_string())
}

/// 可执行同目录下 `index.bin` 或用户配置相对/绝对路径
pub fn resolve_index_path(base: &Path, settings: &FindxGuiSettings) -> PathBuf {
    let p = settings.index_path.trim();
    if Path::new(p).is_absolute() {
        return PathBuf::from(p);
    }
    base.join(p)
}

/// 解析 findx2-service.exe：若填写了自定义路径则必须存在（避免误以为在用 release 实际落在 debug）。
fn resolve_service_exe(base: &Path, settings: &FindxGuiSettings) -> Result<PathBuf, String> {
    let custom = settings.service_exe_path.trim();
    if !custom.is_empty() {
        let p = PathBuf::from(custom);
        if p.exists() {
            return Ok(p);
        }
        return Err(format!(
            "设置中的服务路径不存在: {}（请编译该配置、改为实际存在的路径，或清空此项以使用与 GUI 同目录下的 findx2-service.exe）",
            p.display()
        ));
    }
    for candidate in service_exe_search_paths(base).into_iter() {
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(
        "未找到 findx2-service.exe，请将可执行文件与 FindX2 同目录、resources\\bin 下，或于设置中指定存在的路径。"
            .into(),
    )
}

fn service_exe_search_paths(base: &Path) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        vec![
            base.join("findx2-service.exe"),
            base.join("resources").join("bin").join("findx2-service.exe"),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![
            base.join("findx2-service"),
            base.join("resources").join("bin").join("findx2-service"),
        ]
    }
}

fn cli_name_paths(base: &Path, name: &str) -> [PathBuf; 2] {
    [
        base.join(name),
        base.join("resources").join("bin").join(name),
    ]
}

/// 与 `findx2-service` 同目录的 `findx2` / `fx` 命令行（建索引子进程）
pub fn resolve_cli_exe(base: &Path, settings: &FindxGuiSettings) -> Option<PathBuf> {
    let names = if cfg!(windows) {
        ["findx2.exe", "fx.exe"].as_slice()
    } else {
        ["findx2", "fx"].as_slice()
    };
    for name in names {
        for p in cli_name_paths(base, name).into_iter() {
            if p.exists() {
                return Some(p);
            }
        }
    }
    if let Ok(svc) = resolve_service_exe(base, settings) {
        let parent = svc.parent()?;
        for name in names {
            let p = parent.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// 资源目录：可执行所在目录（用于定位 index.bin / findx2.exe）
pub fn exe_resource_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(windows)]
fn path_contains_dir(path_value: &str, target_dir: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches(['\\', '/']).to_ascii_lowercase();
    let target = norm(target_dir);
    path_value
        .split(';')
        .map(norm)
        .filter(|s| !s.is_empty())
        .any(|s| s == target)
}

/// 将当前可执行目录加入「用户 PATH」：
/// - 安装后默认即可在新开的终端里直接调用 `findx2` / `fx`；
/// - 仅写入 HKCU（无需管理员），且具备幂等性（已存在则不重复追加）。
#[cfg(windows)]
pub fn ensure_cli_on_user_path() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let exe_dir = exe_resource_dir();
    if !exe_dir.exists() {
        return Ok(());
    }
    let target = exe_dir.to_string_lossy().to_string();

    // 先看当前进程 PATH（机器 + 用户合并视图），已存在则直接跳过。
    if let Ok(current_path) = std::env::var("PATH") {
        if path_contains_dir(&current_path, &target) {
            return Ok(());
        }
    }

    // 仅更新用户 PATH，避免触碰系统 PATH（不需要管理员权限）。
    let target_ps = target.replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='Stop';\
        $target='{target}';\
        $current=[Environment]::GetEnvironmentVariable('Path','User');\
        $parts=@();\
        if($current){{\
          $parts=$current -split ';' | ForEach-Object {{$_.Trim()}} | Where-Object {{$_ -ne ''}}\
        }};\
        $norm={{ param([string]$p) if(-not $p){{return ''}} return $p.Trim().TrimEnd('\\\\','/').ToLowerInvariant() }};\
        $exists=$false;\
        foreach($p in $parts){{\
          if((& $norm $p) -eq (& $norm $target)){{$exists=$true;break}}\
        }};\
        if(-not $exists){{\
          $next=if([string]::IsNullOrWhiteSpace($current)){{$target}}else{{$current.TrimEnd(';') + ';' + $target}};\
          [Environment]::SetEnvironmentVariable('Path',$next,'User')\
        }}",
        target = target_ps
    );

    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| format!("写入用户 PATH 失败：{e}"))?;

    if !status.success() {
        return Err(format!(
            "写入用户 PATH 失败（powershell 退出码 {:?}）",
            status.code()
        ));
    }

    // 当前会话立即生效（新终端将自动读取用户 PATH）。
    if let Ok(current_path) = std::env::var("PATH") {
        if !path_contains_dir(&current_path, &target) {
            std::env::set_var("PATH", format!("{current_path};{target}"));
        }
    }
    Ok(())
}

#[cfg(windows)]
const WINDOWS_SERVICE_NAME: &str = "FindX2Search";

#[cfg(windows)]
fn sc_run(args: &[&str]) -> Result<(i32, String), String> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = Command::new("sc.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("sc.exe: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok((out.status.code().unwrap_or(-1), text))
}

#[cfg(windows)]
fn windows_service_is_running() -> bool {
    matches!(sc_run(&["query", WINDOWS_SERVICE_NAME]), Ok((0, text)) if text.contains("RUNNING"))
}

/// `sc start`：0 已启动，1056 已在运行。普通用户对 SCM 可能没有权限。
#[cfg(windows)]
fn try_start_windows_service() -> Result<(), String> {
    if windows_service_is_running() {
        return Ok(());
    }
    let (code, text) = sc_run(&["start", WINDOWS_SERVICE_NAME])?;
    if code == 0 || code == 1056 || text.contains("1056") || windows_service_is_running() {
        return Ok(());
    }
    Err(format!(
        "sc start {WINDOWS_SERVICE_NAME} 失败 ({code}): {}",
        text.trim()
    ))
}

#[cfg(windows)]
fn install_args_for_settings(settings: &FindxGuiSettings, index: &Path, vol: &str, pipe: &str) -> String {
    use crate::elevate::quote_arg;
    let mut params = format!(
        "install --index {} --volume {} --pipe {} --save-interval-secs {}",
        quote_arg(&index.to_string_lossy()),
        quote_arg(vol),
        quote_arg(pipe),
        settings.save_interval_secs.max(1),
    );
    if !settings.enable_everything_ipc {
        params.push_str(" --no-everything-ipc");
    }
    if !settings.enable_metadata_backfill {
        params.push_str(" --no-backfill");
    }
    for dir in &settings.excluded_dirs {
        let d = dir.trim();
        if d.is_empty() {
            continue;
        }
        params.push_str(" --exclude-dir ");
        params.push_str(&quote_arg(d));
    }
    params
}

#[cfg(windows)]
fn start_service_mode_process(
    exe: &Path,
    index: &Path,
    vol: &str,
    pipe: &str,
    settings: &FindxGuiSettings,
) -> Result<(), String> {
    use crate::elevate::{process_is_elevated, shell_execute_runas};
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    if try_start_windows_service().is_ok() {
        return Ok(());
    }

    let work = exe.parent().unwrap_or_else(|| Path::new("."));
    let params = install_args_for_settings(settings, index, vol, pipe);

    if process_is_elevated() {
        let mut cmd = Command::new(exe);
        cmd.current_dir(work)
            .arg("install")
            .arg("--index")
            .arg(index)
            .arg("--volume")
            .arg(vol)
            .arg("--pipe")
            .arg(pipe)
            .arg("--save-interval-secs")
            .arg(format!("{}", settings.save_interval_secs.max(1)))
            .creation_flags(CREATE_NO_WINDOW);
        if !settings.enable_everything_ipc {
            cmd.arg("--no-everything-ipc");
        }
        if !settings.enable_metadata_backfill {
            cmd.arg("--no-backfill");
        }
        for dir in &settings.excluded_dirs {
            let d = dir.trim();
            if d.is_empty() {
                continue;
            }
            cmd.arg("--exclude-dir").arg(d);
        }
        let status = cmd
            .status()
            .map_err(|e| format!("注册系统服务失败: {e}"))?;
        if !status.success() {
            return Err(format!(
                "注册系统服务失败（退出码 {}）。",
                status.code().unwrap_or(-1)
            ));
        }
        return try_start_windows_service();
    }

    let code = shell_execute_runas(exe, Some(&params), work, true)
        .map_err(|e| format!("提权注册系统服务失败: {e}"))?;
    if let Some(c) = code {
        if c != 0 {
            return Err(format!("提权注册系统服务失败（退出码 {c}）。"));
        }
    }
    if try_start_windows_service().is_ok() {
        return Ok(());
    }
    let sc = Path::new(r"C:\Windows\System32\sc.exe");
    shell_execute_runas(sc, Some(&format!("start {WINDOWS_SERVICE_NAME}")), work, true)
        .map_err(|e| format!("提权启动系统服务失败: {e}"))?;
    Ok(())
}

/// 拉起索引服务（要求 `index.bin` 已存在）。
/// 服务模式只走 SCM；单体模式才在当前会话 spawn。
pub fn spawn_findx_service_process<R: Runtime>(app: tauri::AppHandle<R>) -> Result<(), String> {
    let settings = load_findx_settings(app.clone())?;
    let base = exe_resource_dir();
    let exe = resolve_service_exe(&base, &settings)?;
    let index = resolve_index_path(&base, &settings);
    if !index.exists() {
        return Err(format!("索引文件不存在: {}", index.display()));
    }
    let vol = settings.volume.trim();
    let vol = if vol.is_empty() { "C:" } else { vol };
    let pipe = settings.pipe_name.trim();
    let pipe = if pipe.is_empty() { "findx2" } else { pipe };

    #[cfg(windows)]
    {
        use crate::elevate::{quote_arg, shell_execute_runas};
        use std::os::windows::process::CommandExt;
        use std::process::Command;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        if settings.run_mode == RunMode::Service {
            return start_service_mode_process(&exe, &index, vol, pipe, &settings);
        }

        let work = exe.parent().unwrap_or_else(|| Path::new("."));
        let index_str = index.to_string_lossy().to_string();
        let vol_s = vol.to_string();
        let pipe_s = pipe.to_string();

        // 把 service 的 stdout/stderr 转发到 work 目录下的 findx2-service.log，
        // 否则 CREATE_NO_WINDOW 会把所有 progress! / 探针日志吞掉，调优时无法定位。
        // 用 append；启动时单独写一行分隔头，便于多次重启时区分会话。
        let log_path = work.join("findx2-service.log");
        let log_for_stdout = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok();
        let log_for_stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok();
        if let Some(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .ok()
        {
            use std::io::Write;
            // 用 SystemTime 而非 chrono：GUI 不依赖 chrono；service 自己的 progress! 行已带 HH:MM:SS.mmm。
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(
                f,
                "\n=== findx2-service spawn (epoch={}, exe={}) ===",
                secs,
                exe.display()
            );
        }

        let mut cmd = Command::new(&exe);
        cmd.current_dir(work)
            .args([
                "--index",
                index_str.as_str(),
                "--volume",
                vol_s.as_str(),
                "--pipe",
                pipe_s.as_str(),
            ])
            .args([
                "--save-interval-secs",
                &format!("{}", settings.save_interval_secs.max(1)),
            ])
            .creation_flags(CREATE_NO_WINDOW);
        if !settings.enable_everything_ipc {
            cmd.arg("--no-everything-ipc");
        }
        if !settings.enable_metadata_backfill {
            cmd.arg("--no-backfill");
        }
        for dir in &settings.excluded_dirs {
            let d = dir.trim();
            if d.is_empty() {
                continue;
            }
            cmd.arg("--exclude-dir");
            cmd.arg(d);
        }
        if let Some(out) = log_for_stdout {
            cmd.stdout(std::process::Stdio::from(out));
        }
        if let Some(err) = log_for_stderr {
            cmd.stderr(std::process::Stdio::from(err));
        }
        match cmd.spawn() {
            Ok(_) => return Ok(()),
            Err(e) => {
                // Service 模式：不弹 UAC（应由 SCM 拉起）；只汇报失败原因，让 GUI 走「请以 sc start findx2-service」分支。
                if settings.run_mode == RunMode::Service {
                    return Err(format!(
                        "直接启动 findx2-service 失败: {e}。当前为「服务模式」，请用 `findx2-service install` 注册为系统服务后由 SCM 启动；或在设置里切换为「单体 UAC 模式」。"
                    ));
                }
                if crate::elevate::process_is_elevated() {
                    return Err(format!(
                        "直接启动 findx2-service 失败: {e}。当前 FindX2 已以管理员运行，子进程应能继承权限，请检查 exe 是否被拦截、路径与 index.bin 是否正确。"
                    ));
                }
                let mut params = format!(
                    "--index {} --volume {} --pipe {} --save-interval-secs {}",
                    quote_arg(&index.to_string_lossy()),
                    quote_arg(vol),
                    quote_arg(pipe.trim()),
                    settings.save_interval_secs.max(1),
                );
                if !settings.enable_everything_ipc {
                    params.push_str(" --no-everything-ipc");
                }
                if !settings.enable_metadata_backfill {
                    params.push_str(" --no-backfill");
                }
                for dir in &settings.excluded_dirs {
                    let d = dir.trim();
                    if d.is_empty() {
                        continue;
                    }
                    params.push_str(" --exclude-dir ");
                    params.push_str(&quote_arg(d));
                }
                shell_execute_runas(&exe, Some(&params), work, false)
                    .map_err(|u| format!("直接启动失败: {e}；提权启动失败: {u}"))?;
                Ok(())
            }
        }
    }
    #[cfg(not(windows))]
    {
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("--index")
            .arg(index.as_os_str())
            .arg("--volume")
            .arg(vol)
            .arg("--pipe")
            .arg(pipe);
        if !settings.enable_metadata_backfill {
            cmd.arg("--no-backfill");
        }
        for d in &settings.excluded_dirs {
            if !d.trim().is_empty() {
                cmd.arg("--exclude-dir").arg(d);
            }
        }
        cmd.spawn()
            .map_err(|e| format!("启动 findx2-service 失败: {e}"))?;
        Ok(())
    }
}

/// 发起 `taskkill` 后不等待其结束。Windows 上丢弃 `Child` 只会关闭句柄，**不会**终止已启动的 taskkill，
/// 用于退出 GUI 时避免在 UI 线程上阻塞。
#[cfg(windows)]
pub(crate) fn stop_findx_service_detached() {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", "findx2-service.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(not(windows))]
pub(crate) fn stop_findx_service_detached() {
    let _ = std::process::Command::new("pkill")
        .args(["-f", "findx2-service"])
        .spawn();
}

#[tauri::command]
pub fn stop_findx_service() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::Command;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "findx2-service.exe"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("pkill")
            .args(["-x", "findx2-service"])
            .status();
        Ok(())
    }
}
