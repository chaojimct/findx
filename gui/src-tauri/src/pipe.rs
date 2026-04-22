#![cfg(target_os = "windows")]
//! 连接本地 findx2-service 命名管道。

use findx2_ipc::{IpcRequest, IpcResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;

fn pipe_path() -> String {
    std::env::var("FINDX2_PIPE").unwrap_or_else(|_| r"\\.\pipe\findx2".into())
}

fn normalize_pipe(pipe_name: &str) -> String {
    let p = pipe_name.trim();
    if p.is_empty() {
        return pipe_path();
    }
    if p.starts_with(r"\\") {
        p.to_string()
    } else {
        format!(r"\\.\pipe\{p}")
    }
}

/// 客户端连命名管道失败时，Windows 常为 `ERROR_FILE_NOT_FOUND`（2）：表示尚无服务端在监听该管道。
fn map_pipe_open_err(e: std::io::Error) -> String {
    if e.raw_os_error() == Some(2) {
        "无法连接 findx2-service：命名管道不存在（服务未在监听或仍在加载大索引）。请点「启动服务」或手动运行同目录 findx2-service.exe；若仍失败请打开 %TEMP%\\findx2-service-last-error.txt 查看原因，并确认设置里 index.bin 路径与 exe 目录一致。"
            .to_string()
    } else {
        format!("无法连接 findx2-service: {e}")
    }
}

/// 使用与 `index_status` / 设置中 `pipe_name` 一致的管道端点。
///
/// 返回 `(hits, total, elapsed_ms)`：
/// - `hits` 已被 `limit` 截断；
/// - `total` 是 service 端「截断与排序前」真实匹配数（与 Everything 左下角语义一致）；
/// - `elapsed_ms` 是 service 端 search 调用纯耗时（不含 IPC 往返）。
pub async fn ipc_search_with_pipe_name(
    pipe_name: &str,
    query: String,
    pinyin: bool,
    limit: usize,
) -> Result<(Vec<findx2_ipc::SearchHitDto>, u32, u32), String> {
    ipc_search_on_pipe(normalize_pipe(pipe_name), query, pinyin, limit).await
}

pub async fn ipc_search_on_pipe(
    pipe_endpoint: String,
    query: String,
    pinyin: bool,
    limit: usize,
) -> Result<(Vec<findx2_ipc::SearchHitDto>, u32, u32), String> {
    let mut client = ClientOptions::new()
        .open(pipe_endpoint)
        .map_err(map_pipe_open_err)?;

    let req = IpcRequest::Search {
        query,
        pinyin,
        limit,
    };
    let mut body = serde_json::to_string(&req).map_err(|e| e.to_string())?;
    body.push('\n');
    client
        .write_all(body.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    client.flush().await.map_err(|e| e.to_string())?;

    let mut reader = BufReader::new(client);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?;

    match serde_json::from_str::<IpcResponse>(line.trim()).map_err(|e| e.to_string())? {
        IpcResponse::SearchResult {
            hits,
            total,
            elapsed_ms,
        } => Ok((hits, total, elapsed_ms)),
        IpcResponse::Error { message } => Err(message),
        _ => Err("管道响应异常".into()),
    }
}

pub async fn ipc_status_on_pipe(pipe_endpoint: String) -> Result<IpcResponse, String> {
    let mut client = ClientOptions::new()
        .open(pipe_endpoint)
        .map_err(map_pipe_open_err)?;

    let req = IpcRequest::Status;
    let mut body = serde_json::to_string(&req).map_err(|e| e.to_string())?;
    body.push('\n');
    client
        .write_all(body.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    client.flush().await.map_err(|e| e.to_string())?;

    let mut reader = BufReader::new(client);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|e| e.to_string())?;

    serde_json::from_str(line.trim()).map_err(|e| e.to_string())
}

/// 使用设置中的管道名（不含 `\\.\pipe\` 前缀亦可）
pub async fn ipc_status_for_pipe_name(pipe_name: &str) -> Result<IpcResponse, String> {
    ipc_status_on_pipe(normalize_pipe(pipe_name)).await
}
