//! Unix domain socket JSON 协议（与命名管道同一套 `IpcRequest`）。

use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{debug, error, info};

use crate::ipc_dispatch::{process_request, req_kind_label, EngineSlot};

pub async fn unix_accept_loop(socket_path: std::path::PathBuf, slot: EngineSlot) -> anyhow::Result<()> {
    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(&socket_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));
    }
    info!("监听 Unix socket {}", socket_path.display());

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                error!("Unix socket accept 失败: {e}");
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                continue;
            }
        };
        let slot = slot.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, slot).await {
                debug!("socket 会话结束: {e}");
            }
        });
    }
}

async fn handle_client(
    stream: tokio::net::UnixStream,
    slot: EngineSlot,
) -> anyhow::Result<()> {
    static SESSION_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sid = SESSION_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let (r, mut w) = split(stream);
    let mut reader = BufReader::new(r);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<findx2_ipc::IpcRequest>(trimmed) {
            Ok(req) => {
                let slot_cloned = slot.clone();
                let _kind = req_kind_label(&req);
                tokio::task::spawn_blocking(move || process_request(&slot_cloned, req))
                    .await
                    .unwrap_or_else(|e| findx2_ipc::IpcResponse::Error {
                        message: format!("处理请求 panic: {e}"),
                    })
            }
            Err(e) => findx2_ipc::IpcResponse::Error {
                message: format!("JSON 解析失败: {e}"),
            },
        };
        let mut body = serde_json::to_string(&resp)?;
        body.push('\n');
        w.write_all(body.as_bytes()).await?;
        w.flush().await?;
        let _ = sid;
    }
    Ok(())
}
