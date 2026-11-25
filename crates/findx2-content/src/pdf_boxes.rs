//! PDF 命中框 worker：`pdf-worker --boxes` 的常驻单进程封装（预览高亮用）。
//!
//! 与抽取池（`pdf_pool`）分离的原因：boxes 是交互式逐页请求（翻页即查），
//! 需要 pdfium 常驻免去每次 ~200ms 的进程+库初始化；单 worker 串行即可
//! （同一时刻只预览一个文件的一页），崩/超时则杀掉，下次调用时懒重建。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use crate::{Error, Result};

/// 单 job 超时（交互路径；超时杀进程，下次懒重建）。
const JOB_TIMEOUT: Duration = Duration::from_secs(20);
const FAIL_MARK: u64 = u64::MAX;

/// 单页命中框（点坐标，左上原点）+ 页尺寸（点）。
pub struct PageBoxes {
    pub rects: Vec<[f32; 4]>,
    pub page_w: f32,
    pub page_h: f32,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static BOXES: std::sync::OnceLock<std::sync::Mutex<Option<Worker>>> =
    std::sync::OnceLock::new();

fn slot() -> &'static std::sync::Mutex<Option<Worker>> {
    BOXES.get_or_init(|| std::sync::Mutex::new(None))
}

fn spawn() -> Result<Worker> {
    let exe = crate::pdf_pool::worker_exe().ok_or_else(|| {
        Error::Extract("pdf-boxes".into(), "pdf-worker 缺失".into())
    })?;
    let mut child = Command::new(&exe)
        .arg("--boxes")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::Extract("pdf-boxes".into(), format!("启动失败: {e}")))?;
    let stdin = child.stdin.take().ok_or_else(|| {
        Error::Extract("pdf-boxes".into(), "stdin 接管失败".into())
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        Error::Extract("pdf-boxes".into(), "stdout 接管失败".into())
    })?;
    Ok(Worker {
        child,
        stdin,
        stdout: BufReader::new(stdout),
    })
}

/// 取单页命中框。worker 崩/超时返回 Err（调用方降级为无高亮，不影响预览本身）。
pub fn page_boxes(path: &Path, page: u32, needles: &[String]) -> Result<PageBoxes> {
    let job = serde_json::json!({
        "path": path.display().to_string(),
        "page": page,
        "needles": needles,
    });
    let mut line = serde_json::to_vec(&job)
        .map_err(|e| Error::Extract("pdf-boxes".into(), format!("job 序列化失败: {e}")))?;
    line.push(b'\n');
    let mut guard = slot().lock().expect("boxes worker 锁中毒");
    if guard.is_none() {
        *guard = Some(spawn()?);
    }
    let r = run_job(guard.as_mut().expect("boxes worker 已建"), &line);
    if r.is_err() {
        // 坏 worker 丢弃（Drop 杀进程），下次调用重建。
        *guard = None;
    }
    r
}

fn run_job(worker: &mut Worker, job: &[u8]) -> Result<PageBoxes> {
    worker
        .stdin
        .write_all(job)
        .and_then(|_| worker.stdin.flush())
        .map_err(|e| Error::Extract("pdf-boxes".into(), format!("下发任务失败: {e}")))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|s| {
        s.spawn(|| {
            let r = read_frame(&mut worker.stdout);
            let _ = tx.send(r);
        });
        match rx.recv_timeout(JOB_TIMEOUT) {
            Ok(r) => r,
            Err(_) => {
                let _ = worker.child.kill();
                Err(Error::Extract(
                    "pdf-boxes".into(),
                    "单页超时 20s，已重启 boxes worker".into(),
                ))
            }
        }
    })
}

fn read_frame(out: &mut BufReader<ChildStdout>) -> Result<PageBoxes> {
    let mut len_b = [0u8; 8];
    out.read_exact(&mut len_b)
        .map_err(|e| Error::Extract("pdf-boxes".into(), format!("读回包失败: {e}")))?;
    let len = u64::from_le_bytes(len_b);
    if len == FAIL_MARK {
        return Err(Error::Extract(
            "pdf-boxes".into(),
            "本页无框（越界/无文本层/解析失败）".into(),
        ));
    }
    if len > 8 * 1024 * 1024 {
        return Err(Error::Extract(
            "pdf-boxes".into(),
            format!("回包超限 {len}B，疑似 worker 异常"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    out.read_exact(&mut buf)
        .map_err(|e| Error::Extract("pdf-boxes".into(), format!("读正文失败: {e}")))?;
    let v: serde_json::Value = serde_json::from_slice(&buf)
        .map_err(|e| Error::Extract("pdf-boxes".into(), format!("回包非 JSON: {e}")))?;
    let rects: Vec<[f32; 4]> = v
        .get("rects")
        .and_then(|r| serde_json::from_value(r.clone()).ok())
        .unwrap_or_default();
    let page_w = v.get("page_w").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
    let page_h = v.get("page_h").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
    if !(page_w > 0.0 && page_h > 0.0) {
        return Err(Error::Extract("pdf-boxes".into(), "页尺寸非法".into()));
    }
    Ok(PageBoxes { rects, page_w, page_h })
}
