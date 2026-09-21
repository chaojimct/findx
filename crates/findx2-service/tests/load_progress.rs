//! 端到端验证：证明 `index.bin` 加载期间 `Status` IPC 会透出**真实推进的**加载阶段。
//!
//! 这是对 2.4.0「加载过程不可观测」修复的回归测试。要盖住的链路是：
//!
//! ```text
//! load_index_bin_with_progress(回调)
//!   → load_state::note_phase
//!   → ipc_dispatch 的 Status 分支读 load_state
//!   → JSON 序列化
//!   → 客户端反序列化出新字段
//! ```
//!
//! 只测 `load_state` 的内存态盖不住序列化这一段，所以这里**真起一个进程**跑
//! `findx2-service`，再用命名管道按 `Status` 轮询，读回 `loading_stage` 字段。
//!
//! 需要一个**真实存在且够大**的索引才会观察到阶段推进。用环境变量指定：
//!
//! ```text
//! FINDX_PROBE_INDEX=C:\ProgramData\FindX\index.bin cargo test -p findx2-service --test load_progress -- --nocapture
//! ```
//!
//! 未设置 `FINDX_PROBE_INDEX` 时测试直接跳过（CI 上没有生产索引，跳过是正确行为，
//! 而不是伪造一个小索引把断言糊过去）。

#![cfg(windows)]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// 连一次探针管道发 `Status`；管道未就绪返回 `Ok(None)`。
fn status_once(
    pipe: &str,
) -> std::io::Result<Option<(bool, Option<String>, Option<u64>, Option<u32>, Option<u32>)>> {
    let path = format!(r"\\.\pipe\{pipe}");
    let mut f = match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };
    writeln!(f, r#"{{"type":"status"}}"#)?;
    f.flush()?;
    let mut reader = BufReader::new(f);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let v: serde_json::Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => return Ok(None),
    };
    // 只处理 status_result，其余类型（pong 等）忽略。
    if obj.get("type").and_then(|t| t.as_str()) != Some("status_result") {
        return Ok(None);
    }
    Ok(Some((
        obj.get("loading").and_then(|x| x.as_bool()).unwrap_or(false),
        obj.get("loading_stage")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        obj.get("loading_elapsed_secs").and_then(|x| x.as_u64()),
        obj.get("loading_phase_done")
            .and_then(|x| x.as_u64())
            .map(|x| x as u32),
        obj.get("loading_phase_total")
            .and_then(|x| x.as_u64())
            .map(|x| x as u32),
    )))
}

/// 找到与测试二进制同目录的 `findx2-service.exe`。
fn service_exe() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?;
    let dir = me.parent()?;
    // 测试二进制在 target/<profile>/deps/，service 在 target/<profile>/。
    let candidates = [
        dir.join("findx2-service.exe"),
        dir.parent()?.join("findx2-service.exe"),
    ];
    candidates.into_iter().find(|p| p.exists())
}

/// 探针等待上限（秒）。默认 180s，够覆盖本机 850 万条目的库（约 7s）；
/// 上亿条目的库会明显超出，此时设 `FINDX_PROBE_TIMEOUT_SECS` 拉长时间再跑，
/// 不要为了让断言通过而把上限硬编码成一个不真实的短值。
fn probe_timeout_secs() -> u64 {
    std::env::var("FINDX_PROBE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(180)
}

#[test]
fn status_ipc_reports_loading_stage_progress() {
    let Ok(index) = std::env::var("FINDX_PROBE_INDEX") else {
        eprintln!("跳过：未设置 FINDX_PROBE_INDEX（需要真实的大索引才能观察阶段推进）");
        return;
    };
    let index = PathBuf::from(index);
    if !index.exists() {
        eprintln!("跳过：{} 不存在", index.display());
        return;
    }
    let Some(exe) = service_exe() else {
        eprintln!("跳过：未找到 findx2-service.exe（先跑 cargo build）");
        return;
    };

    // 用独立 pipe 名，避免与正在运行的生产 service 抢同名管道。
    let pipe = format!("findx2-probe-{}", std::process::id());

    let mut child = std::process::Command::new(&exe)
        .arg("--index")
        .arg(&index)
        .arg("--pipe")
        .arg(&pipe)
        // 关掉 Everything 窗口与回填：探针只关心加载阶段，别的都是噪音。
        .arg("--no-everything-ipc")
        .arg("--no-backfill")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("拉起 findx2-service 失败");

    let deadline = Instant::now() + Duration::from_secs(probe_timeout_secs());
    let mut saw_loading = false;
    // 只记"阶段名"（去掉随秒数变化的耗时后缀），否则每秒都会被当成一个新阶段，
    // 上亿条目的索引会刷出上百条看起来像"阶段推进"的噪音。
    let mut stage_names: Vec<String> = Vec::new();
    let mut last_raw: Option<String> = None;
    let mut saw_loaded = false;
    // 管道"曾经连上过" = service 真的起来了。之后它若消失，说明进程退出了：
    // 要么加载完正常退出，要么崩了 —— 前者会先报 loading=false，后者不会。
    let mut pipe_seen = false;
    let mut misses_after_seen: u32 = 0;

    /// 从 `解析条目（3/8 阶段，已 42s，共 3.1 亿条）` 里剥出阶段名 `解析条目`。
    fn stage_name(raw: &str) -> &str {
        raw.split('（').next().unwrap_or(raw).trim()
    }

    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
        match status_once(&pipe) {
            Ok(Some((loading, stage, _elapsed, done, total))) => {
                pipe_seen = true;
                misses_after_seen = 0;
                if loading {
                    saw_loading = true;
                    if let Some(s) = stage {
                        // 阶段名变了才记一行，耗时变化不打日志（那是同一阶段在走）。
                        let name = stage_name(&s);
                        if stage_names.last().map(|n| n.as_str()) != Some(name) {
                            eprintln!("阶段推进: {name}（{done:?}/{total:?}）");
                            stage_names.push(name.to_string());
                        }
                        last_raw = Some(s);
                    }
                } else if saw_loading {
                    saw_loaded = true;
                    break;
                }
            }
            // 管道还没建好（服务仍在启动）。继续等。
            Ok(None) if !pipe_seen => continue,
            // 管道连不上了。若此前已经连上过，判定进程已退出，不再空转到 timeout。
            Ok(None) | Err(_) if pipe_seen => {
                misses_after_seen += 1;
                if misses_after_seen >= 3 {
                    eprintln!(
                        "service 进程在报告加载完成前退出（已观察到 {} 个阶段）",
                        stage_names.len()
                    );
                    break;
                }
                continue;
            }
            Ok(None) | Err(_) => continue,
        }
    }

    let _ = child.kill();
    let _ = child.wait();

    eprintln!("观测到 {} 个阶段：{:?}", stage_names.len(), stage_names);
    if let Some(raw) = &last_raw {
        eprintln!("最后一次上报：{raw}");
    }

    assert!(
        saw_loading,
        "从未观察到 loading=true —— service 要么没起来，要么加载快到来不及探到"
    );
    // 这一条才是本次修复的核心断言：加载期间必须能拿到非空的阶段描述。
    assert!(
        !stage_names.is_empty(),
        "loading=true 期间 loading_stage 一直为空：load_state 没透到 IPC（这正是本次要修的 bug）"
    );
    // 加载能否在时限内跑完，取决于索引大小与机器内存，不是这次修复能保证的；
    // 因此只警告、不判失败，避免把"索引太大"误报成"上报链坏了"。
    if saw_loaded {
        eprintln!("加载在时限内完成");
    } else {
        eprintln!(
            "提示：加载未在 {}s 时限内报完成（已推进到「{}」）。\
             大索引请加 FINDX_PROBE_TIMEOUT_SECS 重跑；这与阶段上报无关。",
            probe_timeout_secs(),
            stage_names.last().map(|s| s.as_str()).unwrap_or("?")
        );
    }
}
