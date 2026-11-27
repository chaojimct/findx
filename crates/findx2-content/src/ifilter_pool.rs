//! IFilter worker 进程池：老格式（.doc/.ppt/.msg/.rtf）文本抽取隔离在子进程。
//!
//! 为什么独立进程：系统 IFilter 是第三方 COM 组件（Office/WPS/Adobe），
//! 病态文件可能让它挂死或崩溃。AnyTXT 用 `ATFHelper.exe` 做同一件事。
//! 池内 N 个常驻 `pdf-worker --ifilter`，单文件超时直接 kill 重启，主构建
//! 最多丢一个文件。
//!
//! worker 缺失（未随包/未编译）时降级为进程内 IFilter（见 `ifilter`）。

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use crate::{Error, Result};

const POOL_SIZE: usize = 2;
/// 单文件超时：IFilter 正常毫秒级，30 秒不返回按挂死处理。
const JOB_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONSECUTIVE_CRASHES: u32 = 3;
const FAIL_MARK: u64 = u64::MAX;

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

pub struct IfilterPool {
    exe: PathBuf,
    inner: Mutex<PoolState>,
}

struct PoolState {
    workers: Vec<Worker>,
    spawned: usize,
    consecutive_crashes: u32,
    disabled: bool,
}

static GLOBAL: std::sync::OnceLock<Option<IfilterPool>> = std::sync::OnceLock::new();

pub fn global() -> Option<&'static IfilterPool> {
    GLOBAL.get_or_init(IfilterPool::new).as_ref()
}

/// 生产路径：优先进程池，worker 缺失才退回进程内。
pub fn extract(path: &Path) -> Result<String> {
    match global() {
        Some(pool) => pool.extract(path),
        None => crate::ifilter::extract(path),
    }
}

impl IfilterPool {
    fn new() -> Option<Self> {
        let exe = crate::pdf_pool::worker_exe()?;
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
            .arg("--ifilter")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Extract("ifilter-worker".into(), format!("启动失败: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Extract("ifilter-worker".into(), "stdin 接管失败".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Extract("ifilter-worker".into(), "stdout 接管失败".into()))?;
        st.workers.push(Worker {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        });
        st.spawned += 1;
        Ok(())
    }

    pub fn extract(&self, path: &Path) -> Result<String> {
        let mut worker = self.checkout()?;
        let job = path.as_os_str().as_encoded_bytes().to_vec();
        let r = run_job(&mut worker, &job);
        self.checkin(worker, r.is_ok());
        r.and_then(|opt| {
            opt.ok_or_else(|| {
                Error::Extract(path.display().to_string(), "IFilter 无文本或解析失败".into())
            })
        })
    }

    fn checkout(&self) -> Result<Worker> {
        let mut st = self.inner.lock().expect("ifilter pool 锁中毒");
        if st.disabled {
            return Err(Error::Extract("ifilter-worker".into(), "池已降级".into()));
        }
        if let Some(w) = st.workers.pop() {
            return Ok(w);
        }
        if st.spawned < POOL_SIZE {
            self.spawn_locked(&mut st)?;
            return st
                .workers
                .pop()
                .ok_or_else(|| Error::Extract("ifilter-worker".into(), "无可用 worker".into()));
        }
        drop(st);
        let mut waited = 0u32;
        loop {
            std::thread::sleep(Duration::from_millis(5));
            waited += 1;
            let mut st = self.inner.lock().expect("ifilter pool 锁中毒");
            if st.disabled {
                return Err(Error::Extract("ifilter-worker".into(), "池已降级".into()));
            }
            if let Some(w) = st.workers.pop() {
                return Ok(w);
            }
            if waited > 6_000 {
                return Err(Error::Extract("ifilter-worker".into(), "等待 worker 超时".into()));
            }
        }
    }

    fn checkin(&self, worker: Worker, ok: bool) {
        let mut st = self.inner.lock().expect("ifilter pool 锁中毒");
        if ok {
            st.consecutive_crashes = 0;
            st.workers.push(worker);
        } else {
            st.consecutive_crashes += 1;
            if st.consecutive_crashes >= MAX_CONSECUTIVE_CRASHES {
                st.disabled = true;
            }
        }
    }
}

fn run_job(worker: &mut Worker, job: &[u8]) -> Result<Option<String>> {
    worker
        .stdin
        .write_all(job)
        .and_then(|_| worker.stdin.write_all(b"\n"))
        .and_then(|_| worker.stdin.flush())
        .map_err(|e| Error::Extract("ifilter-worker".into(), format!("下发任务失败: {e}")))?;
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
                    "ifilter-worker".into(),
                    "单文件超时 30s，已重启 worker".into(),
                ))
            }
        }
    })
}

fn read_frame(out: &mut BufReader<ChildStdout>) -> Result<Option<String>> {
    let mut len_b = [0u8; 8];
    out.read_exact(&mut len_b)
        .map_err(|e| Error::Extract("ifilter-worker".into(), format!("读回包失败: {e}")))?;
    let len = u64::from_le_bytes(len_b);
    if len == FAIL_MARK {
        return Ok(None);
    }
    if len > 256 * 1024 * 1024 {
        return Err(Error::Extract(
            "ifilter-worker".into(),
            format!("回包超限 {len}B，疑似 worker 异常"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    out.read_exact(&mut buf)
        .map_err(|e| Error::Extract("ifilter-worker".into(), format!("读正文失败: {e}")))?;
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| Error::Extract("ifilter-worker".into(), format!("非 UTF-8 回包: {e}")))
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
