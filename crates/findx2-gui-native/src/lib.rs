//! FindX 原生 GUI 的可测试逻辑与 IPC 客户端。
//! 窗口代码在同一 crate 的 `main` 里组装。
pub mod cli;
pub mod ipc;
pub mod isolation;
pub mod mock_backend;
pub mod open_path;
pub mod prefs;
pub mod preview;
pub mod search_logic;
pub mod session;
pub mod settings_logic;
pub mod status_logic;
pub mod theme;
pub mod ui;
pub mod update_logic;

pub mod document_preview;

pub mod preview_worker;

#[cfg(windows)]
pub mod office_host;

pub mod autostart;
pub mod service_control;

pub mod browser;

pub mod disk_usage;

pub mod file_identity;

pub mod file_transfer;

pub mod navigation;

pub mod usage_view;

mod interaction_logic;
mod preview_visibility;

pub mod disktree;
pub mod usage_cache;

pub mod indexed_usage;

pub mod footer_status;

pub mod directory_filter;
