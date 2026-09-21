//! 「随系统启动」开关（登录时自动拉起 GUI 托盘）。
//!
//! 设计取舍：
//! - **不引入 `tauri-plugin-autostart` / `winreg`**。GUI 已依赖 `windows` crate 并启用了
//!   `Win32_System_Registry`（安装器写 PATH 也用同一批 API），复用它能少一个传递依赖，
//!   也让「开 = 一次 `RegSetValueEx`，关 = 一次 `RegDeleteValue`」这件事完全显式。
//! - **只写 `HKCU`**，不碰 `HKLM`：无需管理员、不弹 UAC，与安装器往 `HKLM` 写 PATH 的
//!   那条路径解耦。用户级自启也符合「这是个人搜索工具」的定位。
//! - **幂等**：重复开启即覆盖同名值；关闭时只在值存在时才删（不存在 = 已经是关的）。
//!
//! Unix 侧暂不实现（macOS 的 `LaunchAgent` / Linux 的 `~/.config/autostart` 需要在
//! `Info.plist` 与 desktop entry 层面配合，属于 M6 macOS 迭代范围）。Unix 上这两个命令
//! 显式返回「暂不支持」，而不是静默假装成功。

use serde::Serialize;

#[cfg(windows)]
const RUN_VALUE_NAME: &str = "FindX";

/// 自启动项当前状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartState {
    /// 当前是否已启用（且指向当前 exe）
    pub enabled: bool,
    /// 注册表/启动项里记录的完整命令行（仅 Windows 有值）
    pub command: Option<String>,
    /// 平台是否支持由本程序管理自启
    pub supported: bool,
    /// 不支持时的原因，直接展示给用户
    pub unsupported_reason: Option<String>,
}

#[cfg(windows)]
mod imp {
    use super::{AutostartState, RUN_VALUE_NAME};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE,
        REG_SZ,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 逐字节转 UTF-16 以保留 REG_SZ 的精确长度（去掉结尾 NUL）。
    fn from_wide_bytes(bytes: &[u8]) -> String {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let trimmed = units
            .iter()
            .position(|&c| c == 0)
            .map(|i| &units[..i])
            .unwrap_or(&units[..]);
        String::from_utf16_lossy(trimmed)
    }

    fn current_exe_command() -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|e| format!("取当前可执行路径失败: {e}"))?;
        // Run 项的值是命令行：路径含空格必须加引号，否则 Windows 只取到第一个空格前。
        Ok(format!("\"{}\"", exe.display()))
    }

    /// 打开 Run 键；`create` 为真时不存在则创建。
    fn open_run_key(create: bool, write: bool) -> Result<HKEY, String> {
        let sub = wide(RUN_KEY);
        let mut hkey = HKEY::default();
        let access = if write {
            KEY_READ | KEY_SET_VALUE
        } else {
            KEY_READ
        };
        let rc = unsafe {
            if create {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(sub.as_ptr()),
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    access,
                    None,
                    &mut hkey,
                    None,
                )
            } else {
                RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(sub.as_ptr()),
                    None,
                    access,
                    &mut hkey,
                )
            }
        };
        // 读路径下「键不存在」= 从未启用过，交给调用方按"未启用"处理，不当错误。
        if rc == ERROR_FILE_NOT_FOUND {
            return Err("__NOT_FOUND__".into());
        }
        if rc != ERROR_SUCCESS {
            return Err(format!("打开 HKCU\\{RUN_KEY} 失败: Win32 错误 {}", rc.0));
        }
        Ok(hkey)
    }

    struct KeyGuard(HKEY);
    impl Drop for KeyGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                unsafe {
                    let _ = RegCloseKey(self.0);
                }
            }
        }
    }

    fn read_command() -> Result<Option<String>, String> {
        let hkey = match open_run_key(false, false) {
            Ok(k) => k,
            Err(e) if e == "__NOT_FOUND__" => return Ok(None),
            Err(e) => return Err(e),
        };
        let _guard = KeyGuard(hkey);
        let name = wide(RUN_VALUE_NAME);
        let mut ty = REG_SZ;
        let mut size: u32 = 0;
        // 先探长度（lpData 传 None / lpcbData 传 &mut size）。
        let rc = unsafe {
            RegQueryValueExW(
                hkey,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut ty),
                None,
                Some(&mut size),
            )
        };
        if rc == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if rc != ERROR_SUCCESS {
            return Err(format!("读取自启动项失败: Win32 错误 {}", rc.0));
        }
        let mut buf = vec![0u8; size as usize];
        let rc = unsafe {
            RegQueryValueExW(
                hkey,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut ty),
                Some(buf.as_mut_ptr()),
                Some(&mut size),
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(format!("读取自启动项失败: Win32 错误 {}", rc.0));
        }
        buf.truncate(size as usize);
        Ok(Some(from_wide_bytes(&buf)))
    }

    /// 注册表里的命令是否指向当前这台机器上的这个 exe。
    ///
    /// 只比较规范化后的路径部分（去掉引号、大小写无关），不比较参数——
    /// 换过安装目录/大小写后仍能被认出来，避免"其实已经开着但显示关闭"。
    fn command_matches_current(stored: &str) -> bool {
        let Ok(current) = current_exe_command() else {
            return false;
        };
        let norm = |s: &str| {
            s.trim()
                .trim_matches('"')
                .replace('/', "\\")
                .to_ascii_lowercase()
        };
        norm(stored) == norm(&current)
    }

    pub fn read() -> AutostartState {
        match read_command() {
            Ok(Some(cmd)) => AutostartState {
                enabled: command_matches_current(&cmd),
                command: Some(cmd),
                supported: true,
                unsupported_reason: None,
            },
            Ok(None) => AutostartState {
                enabled: false,
                command: None,
                supported: true,
                unsupported_reason: None,
            },
            Err(e) => AutostartState {
                enabled: false,
                command: None,
                supported: true,
                unsupported_reason: Some(e),
            },
        }
    }

    pub fn apply(enable: bool) -> Result<AutostartState, String> {
        let hkey = open_run_key(true, true)?;
        let _guard = KeyGuard(hkey);
        let name = wide(RUN_VALUE_NAME);

        if enable {
            let cmd = current_exe_command()?;
            let data = wide(&cmd);
            let bytes = unsafe {
                std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2)
            };
            let rc = unsafe {
                RegSetValueExW(hkey, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes))
            };
            if rc != ERROR_SUCCESS {
                return Err(format!("写入自启动项失败: Win32 错误 {}", rc.0));
            }
        } else {
            let rc = unsafe { RegDeleteValueW(hkey, PCWSTR(name.as_ptr())) };
            // 值本来就不存在 = 已经是关的，不算错。
            if rc != ERROR_SUCCESS && rc != ERROR_FILE_NOT_FOUND {
                return Err(format!("删除自启动项失败: Win32 错误 {}", rc.0));
            }
        }
        // 落盘后立刻回读，让 UI 拿到的是注册表真值而不是"我以为写成功了"。
        Ok(read())
    }
}

#[cfg(windows)]
pub use imp::{apply, read};

#[cfg(not(windows))]
pub fn read() -> AutostartState {
    AutostartState {
        enabled: false,
        command: None,
        supported: false,
        unsupported_reason: Some(
            "当前平台暂不支持由 FindX 管理开机启动（macOS 的 LaunchAgent / Linux 的 autostart 将在后续版本接入）"
                .into(),
        ),
    }
}

#[cfg(not(windows))]
pub fn apply(_enable: bool) -> Result<AutostartState, String> {
    Err(
        "当前平台暂不支持由 FindX 管理开机启动（macOS 的 LaunchAgent / Linux 的 autostart 将在后续版本接入）"
            .into(),
    )
}
