//! 隔离命名管道上的模拟后端。只用于原生 GUI 开发，不代替真实索引服务。

use findx2_ipc::{IpcRequest, IpcResponse, SearchHitDto};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

fn sample_hits() -> Vec<SearchHitDto> {
    vec![
        hit(
            "README.md",
            r"C:\FindX-example\project\README.md",
            4200,
            1_699_000_000,
            false,
            &[[0, 6]],
        ),
        hit(
            "测试文件.txt",
            r"C:\FindX-example\project\files_for_test\测试文件.txt",
            128,
            1_720_000_000,
            false,
            &[[0, 4]],
        ),
        hit(
            "FindXSearchApp.tsx",
            r"C:\FindX-example\project\gui\src\findx\FindXSearchApp.tsx",
            52_000,
            1_731_200_000,
            false,
            &[[0, 5]],
        ),
        hit(
            "docs",
            r"C:\FindX-example\project\docs",
            0,
            1_731_100_000,
            true,
            &[],
        ),
        hit(
            "hit_batch_01.png",
            r"C:\FindX-example\project\files_for_test\hit_batch_01.png",
            48_000,
            1_731_050_000,
            false,
            &[[0, 9]],
        ),
        hit(
            "北京旅游攻略.docx",
            r"D:\文档\北京旅游攻略.docx",
            1_200_000,
            1_700_000_000,
            false,
            &[[0, 2]],
        ),
        hit(
            "settingsDraft.ts",
            r"C:\FindX-example\project\gui\src\findx\settingsDraft.ts",
            3_300,
            1_731_300_000,
            false,
            &[[0, 8]],
        ),
        hit(
            "findx2-ipc",
            r"C:\FindX-example\project\crates\findx2-ipc",
            0,
            1_731_000_000,
            true,
            &[[0, 6]],
        ),
    ]
}

fn hit(
    name: &str,
    path: &str,
    size: u64,
    mtime: u64,
    is_directory: bool,
    highlight: &[[u32; 2]],
) -> SearchHitDto {
    SearchHitDto {
        entry_idx: 0,
        name: name.into(),
        path: path.into(),
        size,
        mtime,
        is_directory,
        name_highlight: highlight.to_vec(),
        snippet: String::new(),
    }
}

fn highlight_name(name: &str, query: &str) -> Vec<[u32; 2]> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let lower_name: String = name.chars().flat_map(|c| c.to_lowercase()).collect();
    let lower_q: String = q.chars().flat_map(|c| c.to_lowercase()).collect();
    let name_chars: Vec<char> = lower_name.chars().collect();
    let q_chars: Vec<char> = lower_q.chars().collect();
    if q_chars.is_empty() || q_chars.len() > name_chars.len() {
        return Vec::new();
    }
    for i in 0..=name_chars.len() - q_chars.len() {
        if name_chars[i..i + q_chars.len()] == q_chars[..] {
            return vec![[i as u32, (i + q_chars.len()) as u32]];
        }
    }
    Vec::new()
}

fn bare_terms(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|t| !t.contains(':') && !t.starts_with(';'))
        .collect::<Vec<_>>()
        .join(" ")
}

fn handle_request(req: IpcRequest) -> IpcResponse {
    match req {
        // A rendering fixture is never a durable engine, regardless of Status.
        IpcRequest::UpgradeCapabilities { .. } | IpcRequest::PrepareStop { .. } | IpcRequest::CommitStop { .. } | IpcRequest::CancelStop { .. } | IpcRequest::CheckpointIdentity { .. } => IpcResponse::Error {
            message: "mock backend cannot prepare an upgrade or issue a durable-save receipt".into(),
        },
        IpcRequest::UsageCapabilities | IpcRequest::UsageChildren { .. } => IpcResponse::Error {
            message: "mock backend does not support indexed usage".into(),
        },
        IpcRequest::Ping => IpcResponse::Pong,
        IpcRequest::ContentBuild { .. } | IpcRequest::ContentCancel => IpcResponse::Error {
            message: "mock backend does not support content indexing".into(),
        },
        IpcRequest::Status => IpcResponse::StatusResult {
            entry_count: 8,
            dir_count: 2,
            last_usn: 0,
            journal_id: 0,
            volume_letter: Some('C'),
            healthy: true,
            metadata_ready: true,
            backfill_done: 0,
            backfill_total: 0,
            loading: false,
            loading_stage: None,
            loading_elapsed_secs: None,
            loading_phase_done: None,
            loading_phase_total: None,
            watch_error: None,
            backfill_error: None,
            tombstone_count: Some(0),
            indexed_drive_mask: 0,
            wal_bytes: 0,
            last_fold_secs_ago: None,
        content_state: String::new(),
        content_docs: 0,
        content_bytes: 0,
        content_scanned: 0,
        content_extracted: 0,
        content_skipped: 0,
        content_failed: 0,
        content_error: None,
        content_detail: String::new(),
        content_entry_total: 0,
        },
        IpcRequest::Search {
            query,
            pinyin: _,
            limit,
            offset,
        } => {
            let started = Instant::now();
            let needle = bare_terms(&query);
            let mut hits = sample_hits();
            if !needle.is_empty() {
                hits.retain(|h| {
                    h.name.to_lowercase().contains(&needle.to_lowercase())
                        || h.path.to_lowercase().contains(&needle.to_lowercase())
                });
                for h in &mut hits {
                    h.name_highlight = highlight_name(&h.name, &needle);
                }
            }
            let total = hits.len() as u32;
            let end = (offset + limit).min(hits.len());
            let start = offset.min(hits.len());
            let hits = hits[start..end].to_vec();
            IpcResponse::SearchResult {
                hits,
                total,
                elapsed_ms: started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            }
        }
    }
}

async fn serve_lines<S>(stream: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut stream = BufReader::new(stream);
    loop {
        let mut line = String::new();
        match stream.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let req: IpcRequest = match serde_json::from_str(line.trim()) {
                    Ok(r) => r,
                    Err(e) => {
                        let mut body = serde_json::to_string(&IpcResponse::Error {
                            message: format!("坏请求: {e}"),
                        })
                        .unwrap_or_else(|_| r#"{"type":"error","message":"bad request"}"#.into());
                        body.push('\n');
                        let _ = stream.get_mut().write_all(body.as_bytes()).await;
                        break;
                    }
                };
                let mut body = serde_json::to_string(&handle_request(req)).unwrap();
                body.push('\n');
                if stream.get_mut().write_all(body.as_bytes()).await.is_err() {
                    break;
                }
                let _ = stream.get_mut().flush().await;
                break;
            }
            Err(_) => break,
        }
    }
}

/// 在当前线程的 tokio 运行时上循环接受连接。成功监听后返回。
pub async fn run_mock_server(endpoint: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        run_windows_pipe(endpoint).await
    }
    #[cfg(unix)]
    {
        run_unix(endpoint).await
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = endpoint;
        Err("当前平台没有实现模拟后端传输".into())
    }
}

/// 后台线程启动模拟服务，返回 JoinHandle。
pub fn spawn_mock_server(endpoint: String) -> std::thread::JoinHandle<Result<(), String>> {
    std::thread::Builder::new()
        .name("findx2-gui-native-mock".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .map_err(|e| e.to_string())?;
            rt.block_on(run_mock_server(endpoint))
        })
        .expect("spawn mock backend")
}

#[cfg(windows)]
async fn run_windows_pipe(endpoint: String) -> Result<(), String> {
    eprintln!("findx2-gui-native: mock backend listening on {endpoint}");
    let mut server = create_server(&endpoint, true)?;
    loop {
        server.connect().await.map_err(|e| e.to_string())?;
        let connected = server;
        server = create_server(&endpoint, false)?;
        tokio::spawn(async move {
            serve_lines(connected).await;
        });
    }
}

#[cfg(windows)]
fn create_server(endpoint: &str, first: bool) -> Result<NamedPipeServer, String> {
    ServerOptions::new()
        .first_pipe_instance(first)
        .create(endpoint)
        .map_err(|e| format!("无法创建隔离管道 {endpoint}: {e}"))
}

#[cfg(unix)]
async fn run_unix(endpoint: String) -> Result<(), String> {
    let path = std::path::PathBuf::from(&endpoint);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = UnixListener::bind(&path).map_err(|e| format!("无法绑定 {endpoint}: {e}"))?;
    eprintln!("findx2-gui-native: mock backend listening on {endpoint}");
    loop {
        let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
        tokio::spawn(async move {
            serve_lines(stream).await;
        });
    }
}
