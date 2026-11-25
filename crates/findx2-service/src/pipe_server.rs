//! 命名管道 JSON 协议。
//!
//! 服务进程一启动就先把管道挂上去；`index.bin` 加载完成前，`Search` 返回 `Error("索引加载中…")`，
//! `Status` 返回 `loading=true`，让 GUI 立刻知道「服务在、索引还没就绪」而不是「管道超时」。

use findx2_ipc::{IpcRequest, IpcResponse};
use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, info};

use crate::ipc_dispatch::{process_request, req_kind_label, EngineSlot};

/// 命名管道的关键参数集中在这里，避免散在每次 create 处。
/// - `max_instances=254`：tokio `ServerOptions::max_instances` 接受 1..=254；
///   传 255（Win32 PIPE_UNLIMITED_INSTANCES 的值）会 panic
///   `cannot specify more than 254 instances`。254 实际等价于"无上限"，已经远大于
///   GUI + 测试客户端 + Everything IPC 的并发量；
/// - `in/out_buffer_size=64K`：search 单次响应最大 ~5K（500 条 hits × ~10B），64K 留余量；
///   太小会导致 IPC 写入分片，太大白白吃内核非分页池。
const PIPE_BUF_SIZE: u32 = 64 * 1024;
const PIPE_MAX_INSTANCES: u32 = 254;

fn configure_opts(opts: &mut tokio::net::windows::named_pipe::ServerOptions) {
    opts.reject_remote_clients(true).in_buffer_size(PIPE_BUF_SIZE)
        .out_buffer_size(PIPE_BUF_SIZE)
        .max_instances(PIPE_MAX_INSTANCES as usize);
}

/// 管理员 / SYSTEM 创建的命名管道默认 DACL 只有 Administrators。
/// GUI 以普通（过滤）令牌运行时，同一用户也会 `ERROR_ACCESS_DENIED (5)`。
/// 首实例上写入允许本机已登录用户读写的 DACL；后续实例继承。
///
/// `0x12019f` = FILE_GENERIC_READ | FILE_GENERIC_WRITE（管道客户端 CreateFile 所需）。
fn apply_local_client_acl(
    server: &tokio::net::windows::named_pipe::NamedPipeServer,
) -> anyhow::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{LocalFree, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        SetKernelObjectSecurity, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    const SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12019f;;;AU)";
    let wide: Vec<u16> = SDDL.encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )?;
        let handle = HANDLE(server.as_raw_handle());
        let applied = SetKernelObjectSecurity(handle, DACL_SECURITY_INFORMATION, sd);
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        applied?;
    }
    Ok(())
}

fn create_first_pipe_server(
    pipe_path: &str,
) -> anyhow::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    configure_opts(&mut opts);
    opts.first_pipe_instance(true);
    // 创建后要改 DACL，必须带 WRITE_DAC。
    opts.write_dac(true);
    let server=opts.create(pipe_path).map_err(|e|anyhow::anyhow!("exclusive first pipe ownership refused: {e}"))?;
    apply_local_client_acl(&server)?;
    Ok(server)
}

fn create_next_pipe_server(
    pipe_path: &str,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    configure_opts(&mut opts);
    opts.create(pipe_path)
}

pub async fn pipe_accept_loop(pipe_path: String, slot: EngineSlot) -> anyhow::Result<()> {
    info!("监听命名管道 {}", pipe_path);

    // 先挂好第一条监听（独占 + DACL），再起多个 accept worker，每条 worker
    // 独立持有 pending 中的监听实例。单 worker 时“接到连接→挂下一条”之间有
    // 调度窗口，GUI 每次击键 cancel+search 背靠背建连、负载稍高就撞窗
    // （客户端 ERROR_PIPE_BUSY 231 → 搜索失败）；多 worker 让窗口恒有后备。
    let first = create_first_pipe_server(&pipe_path)?;
    const ACCEPT_WORKERS: usize = 3;
    let mut set = tokio::task::JoinSet::new();
    set.spawn(accept_worker(pipe_path.clone(), slot.clone(), first));
    for _ in 1..ACCEPT_WORKERS {
        let next = create_next_pipe_server(&pipe_path)?;
        set.spawn(accept_worker(pipe_path.clone(), slot.clone(), next));
    }
    // 任一 worker 退出（无论 Ok 还是 Err）都视为致命：accept 能力缺损必须让
    // 外层 main 看到（沿用旧语义：listener 起不来不静默 hang，而是直接报错退出）。
    while let Some(r) = set.join_next().await {
        match r {
            Err(e) => return Err(anyhow::anyhow!("accept worker 崩溃: {e}")),
            Ok(Err(e)) => return Err(e),
            Ok(Ok(())) => return Err(anyhow::anyhow!("accept worker 意外退出")),
        }
    }
    Ok(())
}

async fn accept_worker(
    pipe_path: String,
    slot: EngineSlot,
    mut server: tokio::net::windows::named_pipe::NamedPipeServer,
) -> anyhow::Result<()> {
    loop {
        if let Err(e) = server.connect().await {
            error!("命名管道 connect 失败: {e}");
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            continue;
        }

        let connected = server;
        // 立刻挂下一条 listener（本 worker 在 create 完成前无监听，但其余
        // worker 的 pending accept 仍然在线，客户端 connect 不会撞 231）。
        // 这里 unwrap_or_else：万一 create 失败也别让 accept loop 死掉，sleep 后重试。
        server = match create_next_pipe_server(&pipe_path) {
            Ok(s) => s,
            Err(e) => {
                error!("创建后续命名管道实例失败: {e}，500ms 后重试");
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                match create_next_pipe_server(&pipe_path) {
                    Ok(s) => s,
                    Err(e2) => {
                        // 实在起不来就退出 accept loop，让外层 main 看到错误（而不是静默 hang）。
                        return Err(anyhow::anyhow!("创建后续管道实例两次均失败: {e}; {e2}"));
                    }
                }
            }
        };

        let slot = slot.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(connected, slot).await {
                debug!("管道会话结束: {e}");
            }
        });
    }
}

async fn handle_client(
    server: tokio::net::windows::named_pipe::NamedPipeServer,
    slot: EngineSlot,
) -> anyhow::Result<()> {
    // 每个会话分配一个递增 id，方便日志里把多并发请求对应起来。
    static SESSION_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sid = SESSION_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    debug!(sid, "pipe 会话开始");

    use std::os::windows::io::AsRawHandle;
    let pipe_handle=server.as_raw_handle() as usize;
    let (r, mut w) = split(server);
    let mut reader = BufReader::new(r);
    loop {
        let Some(line) = read_bounded_frame(&mut reader).await? else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req_started = std::time::Instant::now();
        let req_summary = if trimmed.len() <= 120 {
            trimmed.to_string()
        } else {
            format!(
                "{}…(共 {} 字节)",
                trimmed.chars().take(120).collect::<String>(),
                trimmed.len()
            )
        };

        let resp = match serde_json::from_str::<IpcRequest>(trimmed) {
            Ok(req) => {
                // process_request 里 engine.search() 是 CPU-bound 重活（最长可达 100ms+），
                // 直接在 tokio worker 上跑会把 worker 池打满，新管道连接 connect 失败 →
                // 用户看到 ERROR_PIPE_BUSY (231)。挪到 blocking pool 隔离。
                let management=requires_management_authorization(&req);
                let authorized=if management {management_authorized(pipe_handle)}else{false};
                let slot_cloned = slot.clone();
                let req_kind = req_kind_label(&req);
                debug!(sid, kind = req_kind, "spawn_blocking 派发");
                let blocking_started = std::time::Instant::now();
                let result =
                    tokio::task::spawn_blocking(move || {
                        if management&&authorized {
                            match slot_cloned.read().ok().and_then(|s|s.clone()){
                                Some(e)=>crate::runtime_lifecycle::management(&e,req),
                                None=>IpcResponse::Error{message:"engine not loaded".into()}
                            }
                        }else{process_request(&slot_cloned, req)}
                    }).await;
                let blocking_ms = blocking_started.elapsed().as_millis();
                if blocking_ms > 200 {
                    info!(sid, kind = req_kind, blocking_ms, "请求处理偏慢");
                }
                result.unwrap_or_else(|e| {
                    error!(sid, "spawn_blocking panic: {e}");
                    IpcResponse::Error {
                        message: format!("处理请求 panic: {e}"),
                    }
                })
            }
            Err(e) => IpcResponse::Error {
                message: format!("JSON 解析失败: {e}"),
            },
        };

        let mut body = serde_json::to_string(&resp)?;
        body.push('\n');
        w.write_all(body.as_bytes()).await?;
        w.flush().await?;
        let total_ms = req_started.elapsed().as_millis();
        if total_ms > 500 {
            // 任何超过 500ms 的请求都是疑似阻塞——记录 req 摘要方便复盘
            info!(sid, total_ms, req = %req_summary, "请求总耗时偏长");
        }
    }

    Ok(())
}

/// Pipe buffers are not frame limits. Refuse oversized input before growing it.
async fn read_bounded_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> anyhow::Result<Option<String>> {
    const MAX_FRAME: usize = 256 * 1024;
    let mut bytes = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let take = chunk
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(chunk.len());
        if bytes.len().saturating_add(take) > MAX_FRAME {
            anyhow::bail!("IPC request exceeds 256KiB frame budget")
        }
        bytes
            .try_reserve(take)
            .map_err(|_| anyhow::anyhow!("IPC frame allocation failed"))?;
        let complete = chunk[take - 1] == b'\n';
        bytes.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if complete {
            break;
        }
    }
    Ok(Some(String::from_utf8(bytes)?))
}
#[cfg(test)]
mod frame_tests {
    use super::*;
    #[tokio::test]
    async fn bounded_memory_frame_handles_utf8_and_rejects_oversize() {
        let data = "{\"type\":\"ping\",\"name\":\"目录\"}\n";
        let mut r = BufReader::new(data.as_bytes());
        assert_eq!(read_bounded_frame(&mut r).await.unwrap().unwrap(), data);
        let bytes = vec![b'a'; 256 * 1024 + 1];
        let mut r = BufReader::new(bytes.as_slice());
        assert!(read_bounded_frame(&mut r).await.is_err());
    }
}

// Impersonate only after this exact frame was read, never trust a caller PID/string.
// A dedicated OS thread cannot leak an impersonation token into Tokio's pool even
// if RevertToSelf fails: thread termination discards its thread-owned token.
fn management_authorized(handle:usize)->bool{
    std::thread::spawn(move||unsafe{
        #[link(name="advapi32")]
        unsafe extern "system" {
            fn ImpersonateNamedPipeClient(pipe:*mut std::ffi::c_void)->i32;
            fn RevertToSelf()->i32;
            fn CreateWellKnownSid(kind:i32,domain:*const std::ffi::c_void,sid:*mut std::ffi::c_void,size:*mut u32)->i32;
            fn CheckTokenMembership(token:*mut std::ffi::c_void,sid:*const std::ffi::c_void,member:*mut i32)->i32;
        }
        if ImpersonateNamedPipeClient(handle as *mut _) ==0{return false;}
        let mut sid=[0u64;9];let mut size=72u32;let mut member=0;
        let allowed=CreateWellKnownSid(26,std::ptr::null(),sid.as_mut_ptr().cast(),&mut size)!=0&&CheckTokenMembership(std::ptr::null_mut(),sid.as_ptr().cast(),&mut member)!=0&&member!=0;
        let reverted=RevertToSelf()!=0;allowed&&reverted
    }).join().unwrap_or(false)
}

// V2 negotiation is available only through the same elevated transport as prepare.
fn requires_management_authorization(req:&IpcRequest)->bool {
    matches!(req,IpcRequest::UpgradeCapabilities{protocol_version:2}|IpcRequest::PrepareStop{..}|IpcRequest::CommitStop{..}|IpcRequest::CancelStop{..}|IpcRequest::CheckpointIdentity{..})
}
#[cfg(test)] mod protocol2_capability_transport_tests {
    use super::*;
    #[test] fn protocol2_capability_requires_os_authentication_not_public_dispatch(){
        assert!(requires_management_authorization(&IpcRequest::UpgradeCapabilities{protocol_version:2}));
        for protocol_version in [1,0,252,u32::MAX] {
            assert!(!requires_management_authorization(&IpcRequest::UpgradeCapabilities{protocol_version}));
        }
    }
}
