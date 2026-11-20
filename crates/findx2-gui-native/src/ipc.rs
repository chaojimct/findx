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
}

pub type IpcResult<T> = Result<T, String>;

enum Job {
    Request {
        endpoint: String,
        req: IpcRequest,
        cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        reply: oneshot::Sender<IpcResult<IpcResponse>>,
    },
    Search {
        endpoint: String,
        query: String,
        pinyin: bool,
        limit: usize,
        offset: usize,
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
        limit: usize,
        offset: usize,
    ) -> oneshot::Receiver<IpcResult<SearchOutcome>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.tx.send(Job::Search {
            endpoint,
            query,
            pinyin,
            limit,
            offset,
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
            limit,
            offset,
            reply,
        } => {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                ipc_search(endpoint, query, pinyin, limit, offset),
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
async fn connect_endpoint(
    endpoint: &str,
) -> IpcResult<tokio::net::windows::named_pipe::NamedPipeClient> {
    ClientOptions::new()
        .open(endpoint)
        .map_err(|e| map_pipe_open_err(endpoint, e))
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
    limit: usize,
    offset: usize,
) -> IpcResult<SearchOutcome> {
    let client = connect_endpoint(&endpoint).await?;
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
        } => Ok(SearchOutcome {
            hits,
            total,
            elapsed_ms,
        }),
        IpcResponse::Error { message } => Err(message),
        _ => Err("管道响应异常".into()),
    }
}

async fn ipc_status(endpoint: String) -> IpcResult<IpcResponse> {
    let client = connect_endpoint(&endpoint).await?;
    write_request(client, IpcRequest::Status).await
}

async fn ipc_ping(endpoint: String) -> IpcResult<()> {
    let client = connect_endpoint(&endpoint).await?;
    match write_request(client, IpcRequest::Ping).await? {
        IpcResponse::Pong => Ok(()),
        IpcResponse::Error { message } => Err(message),
        _ => Err("管道响应异常".into()),
    }
}
