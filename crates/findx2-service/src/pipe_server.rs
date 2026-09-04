//! 命名管道 JSON 协议。
//!
//! 服务进程一启动就先把管道挂上去；`index.bin` 加载完成前，`Search` 返回 `Error("索引加载中…")`，
//! `Status` 返回 `loading=true`，让 GUI 立刻知道「服务在、索引还没就绪」而不是「管道超时」。

use findx2_ipc::{IpcRequest, IpcResponse};
use tokio::io::{split, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, info, warn};

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
    opts.in_buffer_size(PIPE_BUF_SIZE)
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
        DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetKernelObjectSecurity,
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
        let _ = LocalFree(HLOCAL(sd.0));
        applied?;
    }
    Ok(())
}

fn create_first_pipe_server(pipe_path: &str) -> anyhow::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    configure_opts(&mut opts);
    opts.first_pipe_instance(true);
    // 创建后要改 DACL，必须带 WRITE_DAC。
    opts.write_dac(true);
    match opts.create(pipe_path) {
        Ok(s) => {
            apply_local_client_acl(&s)?;
            info!("命名管道 ACL 已放宽：本机已登录用户可连接（管理员服务 + 普通 GUI）");
            Ok(s)
        }
        Err(e) => {
            // 残留实例或异常状态下一度会失败；再试非首实例，避免服务直接起不来、GUI 永远连不上管道。
            warn!("命名管道 first_pipe_instance(true) 失败: {e}，尝试非首实例…");
            let mut opts2 = tokio::net::windows::named_pipe::ServerOptions::new();
            configure_opts(&mut opts2);
            opts2.first_pipe_instance(false);
            opts2.write_dac(true);
            let s = opts2
                .create(pipe_path)
                .map_err(|e2| anyhow::anyhow!("创建命名管道失败: {e}；重试: {e2}"))?;
            if let Err(acl_e) = apply_local_client_acl(&s) {
                warn!("非首实例写入管道 ACL 失败: {acl_e}");
            }
            Ok(s)
        }
    }
}

fn create_next_pipe_server(pipe_path: &str) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut opts = tokio::net::windows::named_pipe::ServerOptions::new();
    configure_opts(&mut opts);
    opts.create(pipe_path)
}

pub async fn pipe_accept_loop(pipe_path: String, slot: EngineSlot) -> anyhow::Result<()> {
    info!("监听命名管道 {}", pipe_path);

    // 先挂好第一条监听，再在接到连接后立即 create 下一条，避免出现「无监听实例」窗口导致 ERROR_PIPE_BUSY(231)。
    let mut server = create_first_pipe_server(&pipe_path)?;

    loop {
        if let Err(e) = server.connect().await {
            error!("命名管道 connect 失败: {e}");
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            continue;
        }

        let connected = server;
        // 立刻挂下一条 listener，避免出现"无监听实例"窗口（客户端 connect 必 ERROR_PIPE_BUSY 231）。
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
                        return Err(anyhow::anyhow!(
                            "创建后续管道实例两次均失败: {e}; {e2}"
                        ));
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

    let (r, mut w) = split(server);
    let mut reader = BufReader::new(r);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            debug!(sid, "pipe 会话结束（客户端关闭）");
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let req_started = std::time::Instant::now();
        let req_summary = if trimmed.len() <= 120 {
            trimmed.to_string()
        } else {
            format!("{}…(共 {} 字节)", &trimmed[..120], trimmed.len())
        };

        let resp = match serde_json::from_str::<IpcRequest>(trimmed) {
            Ok(req) => {
                // process_request 里 engine.search() 是 CPU-bound 重活（最长可达 100ms+），
                // 直接在 tokio worker 上跑会把 worker 池打满，新管道连接 connect 失败 →
                // 用户看到 ERROR_PIPE_BUSY (231)。挪到 blocking pool 隔离。
                let slot_cloned = slot.clone();
                let req_kind = req_kind_label(&req);
                debug!(sid, kind = req_kind, "spawn_blocking 派发");
                let blocking_started = std::time::Instant::now();
                let result = tokio::task::spawn_blocking(move || process_request(&slot_cloned, req))
                    .await;
                let blocking_ms = blocking_started.elapsed().as_millis();
                if blocking_ms > 200 {
                    info!(sid, kind = req_kind, blocking_ms, "请求处理偏慢");
                }
                result.unwrap_or_else(|e| {
                    error!(sid, "spawn_blocking panic: {e}");
                    IpcResponse::Error { message: format!("处理请求 panic: {e}") }
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
