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
#[derive(Serialize, Deserialize)]
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

pub fn run(request: Request, cancel: Cancel, timeout: Duration) -> Result<Output, String> {
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
    let result = supervised(command, bytes, cancel, timeout)?;
    serde_json::from_slice::<Result<Output, String>>(&result)
        .map_err(|e| format!("预览引擎返回无效结果: {e}"))?
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
    let run = || -> Result<Output, String> {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let request: Request = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        match request {
            Request::Text { path, ext } => {
                crate::document_preview::load_text(std::path::Path::new(&path), &ext)
                    .map(Output::Text)
            }
            Request::Pdf { path, page, edge } => {
                #[cfg(windows)]
                {
                    crate::document_preview::pdf_page(std::path::Path::new(&path), page, edge).map(
                        |(png, count, width, height)| Output::Pdf {
                            png,
                            count,
                            width,
                            height,
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
    let _ = serde_json::to_writer(std::io::stdout(), &run());
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
    #[test]
    fn child_delay() {
        if std::env::var_os("FINDX_PREVIEW_DELAY_TEST").is_some() {
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
