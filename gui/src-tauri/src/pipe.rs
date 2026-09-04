//! 连接本地 findx2-service：Windows 命名管道，Unix 域套接字。同一套 JSON 行协议。

use findx2_ipc::{IpcRequest, IpcResponse};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;
#[cfg(unix)]
use tokio::net::UnixStream;

fn pipe_path() -> String {
    #[cfg(windows)]
    {
        std::env::var("FINDX2_PIPE").unwrap_or_else(|_| r"\\.\pipe\findx2".into())
    }
    #[cfg(unix)]
    {
        findx2_ipc::unix_socket_path("findx2")
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(not(any(windows, unix)))]
    {
        "findx2".into()
    }
}

fn normalize_pipe(pipe_name: &str) -> String {
    let p = pipe_name.trim();
    if p.is_empty() {
        return pipe_path();
    }
    #[cfg(windows)]
    {
        if p.starts_with(r"\\") {
            p.to_string()
        } else {
            format!(r"\\.\pipe\{p}")
        }
    }
    #[cfg(unix)]
    {
        findx2_ipc::unix_socket_path(p)
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(not(any(windows, unix)))]
    {
        p.to_string()
    }
}

fn map_pipe_open_err(e: std::io::Error) -> String {
    match e.raw_os_error() {
        Some(2) => {
            "无法连接 findx2-service：端点不存在（服务未在监听或仍在加载大索引）。请点「启动服务」。"
                .to_string()
        }
        Some(5) => {
            "无法连接 findx2-service：拒绝访问。".to_string()
        }
        _ => format!("无法连接 findx2-service: {e}"),
    }
}

async fn write_request<S: AsyncRead + AsyncWrite + Unpin>(
    client: S,
    req: IpcRequest,
) -> Result<IpcResponse, String> {
    let mut client = client;
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

#[cfg(windows)]
async fn connect_endpoint(endpoint: &str) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, String> {
    ClientOptions::new()
        .open(endpoint)
        .map_err(map_pipe_open_err)
}

#[cfg(unix)]
async fn connect_endpoint(endpoint: &str) -> Result<UnixStream, String> {
    UnixStream::connect(endpoint)
        .await
        .map_err(map_pipe_open_err)
}

pub async fn ipc_search_with_pipe_name(
    pipe_name: &str,
    query: String,
    pinyin: bool,
    limit: usize,
    offset: usize,
) -> Result<(Vec<findx2_ipc::SearchHitDto>, u32, u32), String> {
    ipc_search_on_pipe(normalize_pipe(pipe_name), query, pinyin, limit, offset).await
}

pub async fn ipc_search_on_pipe(
    pipe_endpoint: String,
    query: String,
    pinyin: bool,
    limit: usize,
    offset: usize,
) -> Result<(Vec<findx2_ipc::SearchHitDto>, u32, u32), String> {
    let client = connect_endpoint(&pipe_endpoint).await?;
    let req = IpcRequest::Search {
        query,
        pinyin,
        limit,
        offset,
    };
    match write_request(client, req).await? {
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
    let client = connect_endpoint(&pipe_endpoint).await?;
    write_request(client, IpcRequest::Status).await
}

pub async fn ipc_status_for_pipe_name(pipe_name: &str) -> Result<IpcResponse, String> {
    ipc_status_on_pipe(normalize_pipe(pipe_name)).await
}

pub fn probe_service_pipe_sync(pipe_name: &str) -> bool {
    let endpoint = normalize_pipe(pipe_name);
    let handle = std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
        else {
            return false;
        };
        rt.block_on(async {
            matches!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(200),
                    ipc_status_on_pipe(endpoint),
                )
                .await,
                Ok(Ok(_)),
            )
        })
    });
    handle.join().unwrap_or(false)
}
