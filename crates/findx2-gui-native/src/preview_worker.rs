//! Per-request document worker: a stuck decoder can be terminated without stalling GPUI.
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PdfOptions {
    pub page: u32,
    pub zoom: f32,
    pub fit_width: bool,
}
impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            page: 0,
            zoom: 1.,
            fit_width: true,
        }
    }
}
impl PdfOptions {
    pub fn navigate(&mut self, delta: i64, count: u32) {
        self.page = (self.page as i64 + delta).clamp(0, count.saturating_sub(1) as i64) as u32;
    }
    pub fn scale(&mut self, factor: f32) {
        self.fit_width = false;
        self.zoom = (self.zoom * factor).clamp(0.25, 3.);
    }
}
#[derive(Serialize, Deserialize)]
pub enum Request {
    Pdf { path: String, page: u32, edge: u32 },
    Text { path: String, ext: String },
}
#[derive(Serialize, Deserialize, Debug)]
pub enum Output {
    Pdf {
        png: Vec<u8>,
        count: u32,
        width: f32,
        height: f32,
    },
    Text(String),
}
pub type Cancel = Arc<AtomicBool>;
pub fn cancelled(c: &Cancel) -> bool {
    c.load(Ordering::Acquire)
}

/// Wire header: identical to [`Output`] except the PNG travels as raw bytes
/// after the JSON line instead of a serde_json integer array (a 2 MiB PNG
/// balloons to ~7 MiB of JSON and costs ~750 ms per round trip in debug).
#[derive(Serialize, Deserialize, Clone, Debug)]
enum OutputHead {
    Pdf {
        png_len: usize,
        count: u32,
        width: f32,
        height: f32,
    },
    Text(String),
}

/// Rendered-page cache:翻页/重选不再起新进程。Keyed by file identity +
/// page + render edge; stat-checked on lookup so edits invalidate.
struct PageCache {
    order: std::collections::VecDeque<(String, u64, u64, u32, u32)>,
    map: std::collections::HashMap<(String, u64, u64, u32, u32), (Vec<u8>, u32, f32, f32)>,
    bytes: usize,
}
impl PageCache {
    const MAX_BYTES: usize = 48 * 1024 * 1024;
    const MAX_PAGES: usize = 24;
    fn get(&mut self, key: &(String, u64, u64, u32, u32)) -> Option<(Vec<u8>, u32, f32, f32)> {
        self.map.get(key).cloned()
    }
    fn put(
        &mut self,
        key: (String, u64, u64, u32, u32),
        value: (Vec<u8>, u32, f32, f32),
    ) {
        if self.map.contains_key(&key) {
            return;
        }
        self.bytes += value.0.len();
        self.order.push_back(key.clone());
        self.map.insert(key, value);
        while self.bytes > Self::MAX_BYTES || self.map.len() > Self::MAX_PAGES {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some((png, _, _, _)) = self.map.remove(&old) {
                self.bytes = self.bytes.saturating_sub(png.len());
            }
        }
    }
}
static PDF_CACHE: std::sync::OnceLock<std::sync::Mutex<PageCache>> =
    std::sync::OnceLock::new();
fn pdf_cache() -> &'static std::sync::Mutex<PageCache> {
    PDF_CACHE.get_or_init(|| {
        std::sync::Mutex::new(PageCache {
            order: std::collections::VecDeque::new(),
            map: std::collections::HashMap::new(),
            bytes: 0,
        })
    })
}

/// Split a worker reply into its JSON header and raw PNG body.
/// Framing: one `\n`-terminated JSON `Result<OutputHead, String>`,
/// then exactly `png_len` raw bytes for `Pdf` (no JSON integer array).
fn decode_response(bytes: &[u8]) -> Result<Output, String> {
    let nl = bytes
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| "预览引擎返回无效结果: 缺少头部".to_string())?;
    let head: Result<OutputHead, String> = serde_json::from_slice(&bytes[..nl])
        .map_err(|e| format!("预览引擎返回无效结果: {e}"))?;
    match head {
        Err(e) => Err(e),
        Ok(OutputHead::Text(text)) => Ok(Output::Text(text)),
        Ok(OutputHead::Pdf {
            png_len,
            count,
            width,
            height,
        }) => {
            let body = &bytes[nl + 1..];
            if body.len() != png_len {
                return Err(format!(
                    "预览引擎返回无效结果: PNG 长度不符({} != {png_len})",
                    body.len()
                ));
            }
            Ok(Output::Pdf {
                png: body.to_vec(),
                count,
                width,
                height,
            })
        }
    }
}

pub async fn run(request: Request, cancel: Cancel, timeout: Duration) -> Result<Output, String> {
    if cancelled(&cancel) {
        return Err("预览已取消".into());
    }
    // Fast path: identical page already rendered in this process.
    if let Request::Pdf { path, page, edge } = &request {
        if let Ok(meta) = std::fs::metadata(path) {
            let key = (
                path.clone(),
                meta.len(),
                meta.modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|t| t.as_secs())
                    .unwrap_or(0),
                *page,
                *edge,
            );
            if let Ok(mut cache) = pdf_cache().lock() {
                if let Some((png, count, width, height)) = cache.get(&key) {
                    return Ok(Output::Pdf {
                        png,
                        count,
                        width,
                        height,
                    });
                }
            }
            match run_uncached(request, cancel, timeout).await {
                Ok(Output::Pdf {
                    png,
                    count,
                    width,
                    height,
                }) => {
                    if let Ok(mut cache) = pdf_cache().lock() {
                        cache.put(key, (png.clone(), count, width, height));
                    }
                    return Ok(Output::Pdf {
                        png,
                        count,
                        width,
                        height,
                    });
                }
                other => return other,
            }
        }
    }
    run_uncached(request, cancel, timeout).await
}

async fn run_uncached(request: Request, cancel: Cancel, timeout: Duration) -> Result<Output, String> {
    if cancelled(&cancel) {
        return Err("预览已取消".into());
    }
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command.arg("--document-preview-worker");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    // Never park a shared executor thread on a multi-second worker: the
    // blocking wait lives on a throwaway OS thread, the async task only
    // awaits a oneshot (zero pool consumption while pending). Otherwise a
    // slow PDF plugs the pool and file switching stalls behind it.
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(supervised(command, bytes, cancel, timeout));
    });
    let result = rx
        .await
        .map_err(|_| "预览工作线程异常退出".to_string())??;
    decode_response(&result)
}
fn supervised(
    mut command: Command,
    input: Vec<u8>,
    cancel: Cancel,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    if cancelled(&cancel) {
        return Err("预览已取消".into());
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = (|| -> Result<Vec<u8>, String> {
            if let Some(mut stdin) = stdin {
                stdin.write_all(&input).map_err(|e| e.to_string())?;
            }
            let mut bytes = Vec::new();
            if let Some(stdout) = stdout {
                stdout
                    .take(32 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
            }
            if bytes.len() > 32 * 1024 * 1024 {
                return Err("预览输出超过安全上限".into());
            }
            Ok(bytes)
        })();
        let _ = tx.send(result);
    });
    let started = Instant::now();
    let result = loop {
        if cancelled(&cancel) {
            break Err("预览已取消".into());
        }
        if started.elapsed() >= timeout {
            break Err("预览超时，已终止独立解码进程；可重试或选择其他文件".into());
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(result) => break result,
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err("预览引擎已退出".into()),
            Err(_) => {}
        }
    };
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}
pub fn worker_main() {
    use std::io::Write as _;
    let run = || -> Result<(OutputHead, Option<Vec<u8>>), String> {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let request: Request = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        match request {
            Request::Text { path, ext } => {
                crate::document_preview::load_text(std::path::Path::new(&path), &ext)
                    .map(|text| (OutputHead::Text(text), None))
            }
            Request::Pdf { path, page, edge } => {
                #[cfg(windows)]
                {
                    crate::document_preview::pdf_page(std::path::Path::new(&path), page, edge).map(
                        |(png, count, width, height)| {
                            let head = OutputHead::Pdf {
                                png_len: png.len(),
                                count,
                                width,
                                height,
                            };
                            (head, Some(png))
                        },
                    )
                }
                #[cfg(not(windows))]
                {
                    let _ = (path, page, edge);
                    Err("PDF 预览需要 Windows PDF 引擎".into())
                }
            }
        }
    };
    // Framing must match run_uncached: one JSON line, then raw PNG bytes.
    let (head, body) = match run() {
        Ok((head, body)) => (Ok(head), body),
        Err(e) => (Err(e), None),
    };
    let mut stdout = std::io::stdout().lock();
    let _ = serde_json::to_writer(&mut stdout, &head);
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
    if let Some(png) = body {
        let _ = stdout.write_all(&png);
        let _ = stdout.flush();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn page_and_zoom_bounds() {
        let mut p = PdfOptions::default();
        p.navigate(-1, 3);
        assert_eq!(p.page, 0);
        p.navigate(99, 3);
        assert_eq!(p.page, 2);
        p.scale(100.);
        assert_eq!(p.zoom, 3.);
        p.scale(0.001);
        assert_eq!(p.zoom, 0.25);
    }
    fn framed(head: &OutputHead, body: &[u8]) -> Vec<u8> {
        let mut v = serde_json::to_vec(&Ok::<_, String>(head)).unwrap();
        v.push(b'\n');
        v.extend_from_slice(body);
        v
    }
    #[test]
    fn decode_response_round_trips_without_json_blob() {
        // Text head: header only.
        match decode_response(&framed(&OutputHead::Text("hello".into()), &[])).unwrap() {
            Output::Text(t) => assert_eq!(t, "hello"),
            _ => panic!("expected text"),
        }
        // Pdf head: header + raw bytes (not a JSON integer array: the old
        // 2 MiB PNG used to balloon to ~7 MiB of JSON).
        let png: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
        let raw = framed(
            &OutputHead::Pdf {
                png_len: png.len(),
                count: 7,
                width: 100.0,
                height: 200.0,
            },
            &png,
        );
        // Sanity: wire size stays close to payload size, not ~3.5x.
        assert!(raw.len() < png.len() + 256);
        match decode_response(&raw).unwrap() {
            Output::Pdf {
                png: back,
                count,
                width,
                height,
            } => {
                assert_eq!(back, png);
                assert_eq!(count, 7);
                assert_eq!((width, height), (100.0, 200.0));
            }
            _ => panic!("expected pdf"),
        }
        // Truncated body and missing header are errors, not panics.
        assert!(decode_response(b"no-newline").is_err());
        let short = framed(
            &OutputHead::Pdf {
                png_len: 16,
                count: 1,
                width: 1.0,
                height: 1.0,
            },
            &[0u8; 4],
        );
        assert!(decode_response(&short).is_err());
        // Engine error propagates.
        let mut err = serde_json::to_vec(&Err::<OutputHead, String>("boom".into())).unwrap();
        err.push(b'\n');
        assert_eq!(decode_response(&err).unwrap_err(), "boom");
    }
    #[test]
    fn page_cache_evicts_oldest_first_under_cap() {
        let mut cache = PageCache {
            order: std::collections::VecDeque::new(),
            map: std::collections::HashMap::new(),
            bytes: 0,
        };
        let key = |i: u64| ("p".to_string(), 1u64, 2u64, i as u32, 1600u32);
        for i in 0..(PageCache::MAX_PAGES as u64 + 4) {
            cache.put(key(i), (vec![i as u8; 64], 3, 10.0, 10.0));
        }
        assert!(cache.map.len() <= PageCache::MAX_PAGES);
        assert!(cache.get(&key(PageCache::MAX_PAGES as u64 + 3)).is_some());
        assert!(cache.get(&key(0)).is_none());
    }
    #[test]
    fn child_delay() {
        if crate::isolation::development_var_os("FINDX_PREVIEW_DELAY_TEST").is_some() {
            std::thread::sleep(Duration::from_secs(30));
        }
    }
    fn delay() -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "preview_worker::tests::child_delay"])
            .env("FINDX_PREVIEW_DELAY_TEST", "1");
        c
    }
    #[test]
    fn timeout_kills_only_owned_worker() {
        let start = Instant::now();
        let e = supervised(
            delay(),
            vec![],
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(e.contains("超时"));
        assert!(start.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn cancellation_kills_inflight_worker() {
        let c = Arc::new(AtomicBool::new(false));
        let c2 = c.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(80));
            c2.store(true, Ordering::Release);
        });
        let e = supervised(delay(), vec![], c, Duration::from_secs(10)).unwrap_err();
        assert!(e.contains("取消"));
    }
}
