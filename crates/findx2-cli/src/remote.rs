//! 通过命名管道连接 findx2-service。

use findx2_core::{Error, Result};
use findx2_ipc::{IpcRequest, IpcResponse};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;

/// CLI 端整体超时——回填高峰期 service 偶尔几秒不响应，但绝不应让 fx 死等。
/// 超时后用户 ctrl+c 也能立刻退出，且能看到明确的错误。
const REMOTE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn normalize_pipe_path(pipe: &str) -> String {
    if pipe.starts_with(r"\\") {
        pipe.to_string()
    } else {
        format!(r"\\.\pipe\{pipe}")
    }
}

pub(crate) fn remote_search_blocking(
    pipe: &str,
    query: &str,
    pinyin: bool,
    limit: usize,
) -> Result<Vec<findx2_ipc::SearchHitDto>> {
    let rt =
        tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| Error::Platform(e.to_string()))?;
    rt.block_on(async {
        tokio::time::timeout(REMOTE_TIMEOUT, remote_search(pipe, query, pinyin, limit))
            .await
            .map_err(|_| Error::Platform(format!("远程搜索超时（>{}s），service 可能正在合并回填或卡死", REMOTE_TIMEOUT.as_secs())))?
    })
}

async fn remote_search(
    pipe: &str,
    query: &str,
    pinyin: bool,
    limit: usize,
) -> Result<Vec<findx2_ipc::SearchHitDto>> {
    let path = normalize_pipe_path(pipe);
    let mut client = ClientOptions::new().open(path).map_err(|e| Error::Platform(format!("连接管道失败: {e}")))?;

    let req = IpcRequest::Search {
        query: query.to_string(),
        pinyin,
        limit,
    };
    let mut body = serde_json::to_string(&req).map_err(|e| Error::Json(e))?;
    body.push('\n');
    client
        .write_all(body.as_bytes())
        .await
        .map_err(|e| Error::Platform(e.to_string()))?;
    client
        .flush()
        .await
        .map_err(|e| Error::Platform(e.to_string()))?;

    let mut reader = BufReader::new(client);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|e| Error::Platform(e.to_string()))?;

    let resp: IpcResponse = serde_json::from_str(line.trim()).map_err(|e| Error::Json(e))?;
    match resp {
        IpcResponse::SearchResult { hits, .. } => Ok(hits),
        IpcResponse::Error { message } => Err(Error::Platform(message)),
        _ => Err(Error::Platform("管道响应类型异常".into())),
    }
}
