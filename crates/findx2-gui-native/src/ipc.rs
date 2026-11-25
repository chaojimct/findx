//! 现有 JSON 行协议客户端。I/O 只跑在专用后台运行时，不进 UI 线程。
use findx2_ipc::{IpcRequest, IpcResponse, SearchHitDto};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

#[derive(Debug, Clone)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHitDto>,
    pub total: u32,
    pub elapsed_ms: u32,
    /// 发起 `Search` 时的 `request_id` 回显（P0-1；0 = 未跟踪的老调用）。
    /// GUI 凭此丢弃过期响应（防"结果闪回旧查询"）。
    pub request_id: u64,
}

/// service 回 `Cancelled` 时 GUI 侧的错误串前缀（P0-1 约定）：
/// `search_session` 收到此前缀应静默丢弃（同 ID 的新查询已在路上），不得弹错、不得清空旧结果。
pub const CANCELLED_PREFIX: &str = "__CANCELLED__:";

/// P0-1：`Err` 是否为"本次被替代"确认（调用方静默丢弃，不碰行/状态）。
pub fn is_cancelled_err(err: &str) -> bool {
    err.starts_with(CANCELLED_PREFIX)
}

/// P0-1：回包是否过期（`request_id` 与最新不一致；0 = 未跟踪的老调用/老服务，放行）。
pub fn is_stale_response(outcome_rid: u64, latest_rid: u64) -> bool {
    outcome_rid != 0 && outcome_rid != latest_rid
}

pub type IpcResult<T> = Result<T, String>;

enum Job {
    Request {
        endpoint: String,
        req: IpcRequest,
        cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        reply: oneshot::Sender<IpcResult<IpcResponse>>,
    },
    /// P0-1 取消（fire-and-forget；service 幂等回 Cancelled，本侧直接丢弃）。
    Cancel {
        endpoint: String,
        request_id: u64,
    },
    /// P1-D 上报打开（fire-and-forget；service 侧累计落盘，恒回 Pong）。
    RecordOpen {
        endpoint: String,
        path: String,
    },
    Search {
        endpoint: String,
        query: String,
        pinyin: bool,
        pinyin_initials: bool,
        pinyin_full: bool,
        limit: usize,
        offset: usize,
        request_id: u64,
        session_id: u64,
        reply: oneshot::Sender<IpcResult<SearchOutcome>>,
    },
    Status {
        endpoint: String,
        reply: oneshot::Sender<IpcResult<IpcResponse>>,
    },
    Ping {
        endpoint: String,
        reply: oneshot::Sender<IpcResult<()>>,
    },
}

#[derive(Clone)]
pub struct IpcClient {
    tx: mpsc::UnboundedSender<Job>,
}

impl IpcClient {
    pub fn request(
        &self,
        endpoint: String,
        req: IpcRequest,
    ) -> oneshot::Receiver<IpcResult<IpcResponse>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Job::Request {
            endpoint,
            req,
            cancel: None,
            reply,
        });
        rx
    }
    pub fn request_cancellable(&self, endpoint: String, req: IpcRequest, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> oneshot::Receiver<IpcResult<IpcResponse>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Job::Request { endpoint, req, cancel: Some(cancel), reply });
        rx
    }
    pub fn start() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new()
            .name("findx2-gui-native-ipc".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_io()
                    .enable_time()
                    .build()
                    .expect("ipc tokio runtime");
                rt.block_on(async move {
                    while let Some(job) = rx.recv().await {
                        tokio::spawn(dispatch(job));
                    }
                });
            })
            .expect("spawn ipc thread");
        Self { tx }
    }

    pub fn search(
        &self,
        endpoint: String,
        query: String,
        pinyin: bool,
        pinyin_initials: bool,
        pinyin_full: bool,
        limit: usize,
        offset: usize,
    ) -> oneshot::Receiver<IpcResult<SearchOutcome>> {
        self.search_tracked(endpoint, query, pinyin, pinyin_initials, pinyin_full, limit, offset, 0, 0)
    }

    /// 带跟踪的搜索（P0-1/P0-2）：`request_id` 自增（每次击键+1，翻页沿用），
    /// `session_id` 按输入框固定；击键前先对上一 `request_id` 发 `SearchCancel`。
    /// 收到 `Err("__CANCELLED__:id")` 即本次被替代，静默丢弃。详见 T122 文档。
    pub fn search_tracked(
        &self,
        endpoint: String,
        query: String,
        pinyin: bool,
        pinyin_initials: bool,
        pinyin_full: bool,
        limit: usize,
        offset: usize,
        request_id: u64,
        session_id: u64,
    ) -> oneshot::Receiver<IpcResult<SearchOutcome>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Job::Search {
            endpoint,
            query,
            pinyin,
            pinyin_initials,
            pinyin_full,
            limit,
            offset,
            request_id,
            session_id,
            reply,
        });
        rx
    }

    pub fn status(&self, endpoint: String) -> oneshot::Receiver<IpcResult<IpcResponse>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Job::Status { endpoint, reply });
        rx
    }

    pub fn ping(&self, endpoint: String) -> oneshot::Receiver<IpcResult<()>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Job::Ping { endpoint, reply });
        rx
    }

    /// P0-1：取消在飞的上一个查询（fire-and-forget；`request_id == 0` 无意义，直接返回）。
    pub fn cancel_search(&self, endpoint: String, request_id: u64) {
        if request_id == 0 {
            return;
        }
        let _ = self.tx.send(Job::Cancel {
            endpoint,
            request_id,
        });
    }

    /// P1-D：上报一次文件打开成功（fire-and-forget；service 侧累计落盘，恒回 Pong）。
    pub fn record_open(&self, endpoint: String, path: String) {
        if path.is_empty() {
            return;
        }
        let _ = self.tx.send(Job::RecordOpen { endpoint, path });
    }
}

async fn dispatch(job: Job) {
    match job {
        Job::Request {
            endpoint,
            req,
            cancel,
            mut reply,
        } => {
            let r = tokio::select! {
                result = tokio::time::timeout(std::time::Duration::from_secs(45), async {
                let c = connect_endpoint(&endpoint).await?;
                write_request(c, req).await
            }) => result.unwrap_or_else(|_| Err("索引目录请求超时（45 秒）；刷新可重试".into())),
                _ = async {
                    loop {
                        if cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire)) { break; }
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                } => Err("索引目录请求已取消".into()),
                _ = reply.closed() => return,
            };
            let _ = reply.send(r);
        }
        Job::Search {
            endpoint,
            query,
            pinyin,
            pinyin_initials,
            pinyin_full,
            limit,
            offset,
            request_id,
            session_id,
            reply,
        } => {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                ipc_search(
                    endpoint,
                    query,
                    pinyin,
                    pinyin_initials,
                    pinyin_full,
                    limit,
                    offset,
                    request_id,
                    session_id,
                ),
            )
            .await
            .unwrap_or_else(|_| Err("搜索响应超时（15 秒）".into()));
            let _ = reply.send(result);
        }
        Job::Status { endpoint, reply } => {
            let result =
                tokio::time::timeout(std::time::Duration::from_secs(5), ipc_status(endpoint))
                    .await
                    .unwrap_or_else(|_| Err("Status 响应超时（5 秒）".into()));
            let _ = reply.send(result);
        }
        Job::Cancel {
            endpoint,
            request_id,
        } => {
            tokio::spawn(cancel_search(endpoint, request_id));
        }
        Job::RecordOpen { endpoint, path } => {
            tokio::spawn(record_open(endpoint, path));
        }
        Job::Ping { endpoint, reply } => {
            let _ = reply.send(ipc_ping(endpoint).await);
        }
    }
}

fn map_pipe_open_err(endpoint: &str, e: std::io::Error) -> String {
    let _ = endpoint;
    // ENOENT / ERROR_FILE_NOT_FOUND, and connection refused on Unix sockets.
    let not_listening = matches!(e.raw_os_error(), Some(2) | Some(61) | Some(111))
        || e.kind() == std::io::ErrorKind::NotFound
        || e.kind() == std::io::ErrorKind::ConnectionRefused;
    if not_listening {
        return "无法连接索引服务：服务未运行。".into();
    }
    if e.raw_os_error() == Some(5) {
        return "无法连接索引服务：拒绝访问。".into();
    }
    "无法连接索引服务。".into()
}

async fn write_request<S: AsyncRead + AsyncWrite + Unpin>(
    client: S,
    req: IpcRequest,
) -> IpcResult<IpcResponse> {
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
    if line.trim().is_empty() {
        return Err("管道响应为空".into());
    }
    serde_json::from_str(line.trim()).map_err(|e| e.to_string())
}

#[cfg(windows)]
async fn connect_endpoint(endpoint: &str) -> IpcResult<tokio::net::windows::named_pipe::NamedPipeClient> {
    // ERROR_PIPE_BUSY(231)：服务端监听实例正在换挂（单监听的调度窗口；旧 service
    // 常驻单 accept loop 时高频撞窗）。瞬态，短暂退避重试几次；其他错误直接返回。
    // 重试上限 400ms，远小于上层 15s/45s 超时，不会拖慢真实故障的报错。
    const ERROR_PIPE_BUSY: i32 = 231;
    let mut attempts = 0u32;
    loop {
        match ClientOptions::new().open(endpoint) {
            Ok(c) => return Ok(c),
            Err(e) => {
                attempts += 1;
                if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 5 {
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    continue;
                }
                return Err(map_pipe_open_err(endpoint, e));
            }
        }
    }
}

#[cfg(unix)]
async fn connect_endpoint(endpoint: &str) -> IpcResult<UnixStream> {
    UnixStream::connect(endpoint)
        .await
        .map_err(|e| map_pipe_open_err(endpoint, e))
}

#[cfg(not(any(windows, unix)))]
async fn connect_endpoint(endpoint: &str) -> IpcResult<()> {
    let _ = endpoint;
    Err("当前平台没有实现 IPC 传输".into())
}

async fn ipc_search(
    endpoint: String,
    query: String,
    pinyin: bool,
    pinyin_initials: bool,
    pinyin_full: bool,
    limit: usize,
    offset: usize,
    request_id: u64,
    session_id: u64,
) -> IpcResult<SearchOutcome> {
    let client = connect_endpoint(&endpoint).await?;
    let req = IpcRequest::Search {
        query,
        pinyin,
        limit,
        offset,
        pinyin_initials,
        pinyin_full,
        request_id,
        session_id,
    };
    match write_request(client, req).await? {
        IpcResponse::SearchResult {
            hits,
            total,
            elapsed_ms,
            request_id,
        } => Ok(SearchOutcome {
            hits,
            total,
            elapsed_ms,
            request_id,
        }),
        IpcResponse::Cancelled { request_id } => Err(format!("{CANCELLED_PREFIX}{request_id}")),
        IpcResponse::Error { message } => Err(message),
        _ => Err("管道响应异常".into()),
    }
}

async fn ipc_status(endpoint: String) -> IpcResult<IpcResponse> {
    let client = connect_endpoint(&endpoint).await?;
    write_request(client, IpcRequest::Status).await
}

/// P0-1 取消发送（后台 detached；响应直接丢弃，service 侧幂等）。
async fn cancel_search(endpoint: String, request_id: u64) {
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            let client = connect_endpoint(&endpoint).await?;
            let req = IpcRequest::SearchCancel { request_id };
            let _ = write_request(client, req).await;
            Ok::<_, String>(())
        },
    )
    .await;
}

/// P1-D 上报打开（后台 detached；恒回 Pong，失败静默）。
async fn record_open(endpoint: String, path: String) {
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        async {
            let client = connect_endpoint(&endpoint).await?;
            let req = IpcRequest::RecordOpen { path };
            let _ = write_request(client, req).await;
            Ok::<_, String>(())
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_err_and_stale_response_predicates() {
        // 取消确认静默丢弃。
        assert!(is_cancelled_err("__CANCELLED__:7"));
        assert!(!is_cancelled_err("管道响应超时（15 秒）"));
        assert!(!is_cancelled_err(""));
        // request_id 对不上即过期；0 放行（老调用/老服务）。
        assert!(is_stale_response(7, 9));
        assert!(!is_stale_response(9, 9));
        assert!(!is_stale_response(0, 9));
        assert!(!is_stale_response(0, 0));
    }
}

async fn ipc_ping(endpoint: String) -> IpcResult<()> {
    let client = connect_endpoint(&endpoint).await?;
    match write_request(client, IpcRequest::Ping).await? {
        IpcResponse::Pong => Ok(()),
        IpcResponse::Error { message } => Err(message),
        _ => Err("管道响应异常".into()),
    }
}
