//! FindX2 托盘 GUI 设置（与 dotnet 版字段对齐）及拉起 findx2-service。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{Manager, Runtime};

fn default_true() -> bool {
    true
}

/// GUI 与 service 的协同方式。
/// - `Service`（默认）：GUI 普通用户运行；service 期望由 SCM（`findx2-service install` + `sc start`）拉起常驻；
///   缺服务时 GUI 仅做「以普通权限直拉同目录 service」兜底（USN 增量在该模式下会受限，但搜索 IPC 仍可工作）。
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

#[tauri::command]
pub fn load_findx_settings<R: Runtime>(app: tauri::AppHandle<R>) -> Result<FindxGuiSettings, String> {
    let path = settings_path(&app)?;
    if !path.exists() {
        return Ok(FindxGuiSettings::default());
    }
    let s = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&s).map_err(|e| e.to_string())
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
    let here = base.join("findx2-service.exe");
    if here.exists() {
        return Ok(here);
    }
    Err(
        "未找到 findx2-service.exe，请将可执行文件与 FindX2 同目录或于设置中指定存在的路径。"
            .into(),
    )
}

/// 与 `findx2-service` 同目录的 `findx2` / `fx` 命令行（建索引子进程）
pub fn resolve_cli_exe(base: &Path, settings: &FindxGuiSettings) -> Option<PathBuf> {
    for name in ["findx2.exe", "fx.exe"] {
        let p = base.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(svc) = resolve_service_exe(base, settings) {
        let parent = svc.parent()?;
        for name in ["findx2.exe", "fx.exe"] {
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
    use std::process::Command;

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

/// 直接拉起 findx2-service（要求 `index.bin` 已存在）。
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
        let _ = (exe, index, vol, pipe);
        Err("当前平台请自行启动 findx2-service（或后续接入 TCP）。".into())
    }
}

#[tauri::command]
pub fn stop_findx_service() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::process::Command;
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "findx2-service.exe"])
            .output();
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Err("当前平台未实现停止服务进程。".into())
    }
}
