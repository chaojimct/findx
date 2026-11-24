//! PDF worker 进程池：把 pdfium 隔离在独立单线程子进程里。
//!
//! 背景：pdfium 原生库在宿主多线程进程内并发使用会挂起整个进程
//!（串行 init 也救不回来）。AnyTXT 同构（ATDHelper* 独立进程）。
//! 池子里 N 个常驻 `pdf-worker`，单个文件崩/挂只 kill 重启该 worker，
//! 主构建最多丢一个文件（记失败），绝不冻屏。
//!
//! `PdfPool::global()` 进程级单例（构建/增量共用）；worker 缺失
//! （没编/没随包）则返回 None，调用方走纯 Rust 兜底。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use crate::{Error, Result};

/// worker 并发数：单文件 ~100ms 级，4 路 ≈ 40 篇/秒；再多则内存（每 worker
/// 常驻 pdfium）和收益不成正比，实测后再调。
const POOL_SIZE: usize = 4;
/// 单文件超时：扫描版/巨型 PDF 也应在 1 分钟内有结果；超时 kill + 重启 worker。
const JOB_TIMEOUT: Duration = Duration::from_secs(60);
/// worker 连续异常退出超过此数则认为环境不可用，池子整体降级。
const MAX_CONSECUTIVE_CRASHES: u32 = 3;

const FAIL_MARK: u64 = u64::MAX;

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

pub struct PdfPool {
    exe: PathBuf,
    inner: Mutex<PoolState>,
}

struct PoolState {
    workers: Vec<Worker>,
    spawned: usize,
    consecutive_crashes: u32,
    disabled: bool,
}

static GLOBAL: std::sync::OnceLock<Option<PdfPool>> = std::sync::OnceLock::new();

/// 进程级单例；worker exe 缺失则返回 None（调用方走纯 Rust 兜底）。
pub fn global() -> Option<&'static PdfPool> {
    GLOBAL.get_or_init(PdfPool::new).as_ref()
}

fn worker_exe() -> Option<PathBuf> {
    let mut p = std::env::current_exe().ok()?;
    p.pop();
    #[cfg(windows)]
    p.push("pdf-worker.exe");
    #[cfg(not(windows))]
    p.push("pdf-worker");
    p.is_file().then_some(p)
}

impl PdfPool {
    fn new() -> Option<Self> {
        let exe = worker_exe()?;
        Some(Self {
            exe,
            inner: Mutex::new(PoolState {
                workers: Vec::new(),
                spawned: 0,
                consecutive_crashes: 0,
                disabled: false,
            }),
        })
    }

    fn spawn_locked(&self, st: &mut PoolState) -> Result<()> {
        let mut child = Command::new(&self.exe)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Extract("pdf-worker".into(), format!("启动失败: {e}")))?;
        let stdin = child.stdin.take().ok_or_else(|| {
            Error::Extract("pdf-worker".into(), "stdin 接管失败".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            Error::Extract("pdf-worker".into(), "stdout 接管失败".into())
        })?;
        st.workers.push(Worker {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        });
        st.spawned += 1;
        Ok(())
    }

    /// 抽取单个 PDF 全文。worker 崩/超时 → kill + 补 worker，本文件记失败。
    pub fn extract(&self, path: &Path) -> Result<String> {
        let mut worker = self.checkout()?;
        let job = path.as_os_str().as_encoded_bytes().to_vec();
        let r = run_job(&mut worker, &job);
        self.checkin(worker, r.is_ok());
        r.and_then(|opt| {
            opt.ok_or_else(|| {
                Error::Extract(path.display().to_string(), "pdf 无文本层或解析失败".into())
            })
        })
    }

    fn checkout(&self) -> Result<Worker> {
        let mut st = self.inner.lock().expect("pdf pool 锁中毒");
        if st.disabled {
            return Err(Error::Extract("pdf-worker".into(), "池已降级".into()));
        }
        if let Some(w) = st.workers.pop() {
            return Ok(w);
        }
        if st.spawned < POOL_SIZE {
            self.spawn_locked(&mut st)?;
            return st
                .workers
                .pop()
                .ok_or_else(|| Error::Extract("pdf-worker".into(), "无可用 worker".into()));
        }
        // 池满：短暂自旋等待归还（rayon 线程会在此排队，正好做天然背压）。
        drop(st);
        let mut waited = 0u32;
        loop {
            std::thread::sleep(Duration::from_millis(5));
            waited += 1;
            let mut st = self.inner.lock().expect("pdf pool 锁中毒");
            if st.disabled {
                return Err(Error::Extract("pdf-worker".into(), "池已降级".into()));
            }
            if let Some(w) = st.workers.pop() {
                return Ok(w);
            }
            if waited > 12_000 {
                // 60s 都没 worker：极端饥饿，降级让调用方走兜底。
                return Err(Error::Extract("pdf-worker".into(), "等待 worker 超时".into()));
            }
        }
    }

    fn checkin(&self, worker: Worker, ok: bool) {
        let mut st = self.inner.lock().expect("pdf pool 锁中毒");
        if ok {
            st.consecutive_crashes = 0;
            st.workers.push(worker);
        } else {
            st.consecutive_crashes += 1;
            if st.consecutive_crashes >= MAX_CONSECUTIVE_CRASHES {
                st.disabled = true;
            }
            // 坏 worker 不归还（子进程已死或管道已断，由 Drop 回收）。
        }
    }
}

/// 在 scope 线程里做阻塞读写，主线程 `recv_timeout` 做超时；超时后杀进程。
/// scope 保证读写线程 join 后才返回，无游离线程、无 unsafe。
fn run_job(worker: &mut Worker, job: &[u8]) -> Result<Option<String>> {
    worker
        .stdin
        .write_all(job)
        .and_then(|_| worker.stdin.write_all(b"\n"))
        .and_then(|_| worker.stdin.flush())
        .map_err(|e| Error::Extract("pdf-worker".into(), format!("下发任务失败: {e}")))?;
    let (tx, rx) = mpsc::channel();
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
                    "pdf-worker".into(),
                    "单文件超时 60s，已重启 worker".into(),
                ))
            }
        }
    })
}

fn read_frame(out: &mut BufReader<ChildStdout>) -> Result<Option<String>> {
    let mut len_b = [0u8; 8];
    out.read_exact(&mut len_b)
        .map_err(|e| Error::Extract("pdf-worker".into(), format!("读回包失败: {e}")))?;
    let len = u64::from_le_bytes(len_b);
    if len == FAIL_MARK {
        return Ok(None);
    }
    if len > 256 * 1024 * 1024 {
        return Err(Error::Extract(
            "pdf-worker".into(),
            format!("回包超限 {len}B，疑似 worker 异常"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    out.read_exact(&mut buf)
        .map_err(|e| Error::Extract("pdf-worker".into(), format!("读正文失败: {e}")))?;
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| Error::Extract("pdf-worker".into(), format!("非 UTF-8 回包: {e}")))
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
