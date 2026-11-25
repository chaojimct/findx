//! service 常驻内容索引：打开 + 增量跟随 + 在线构建 + 查询装配。
//!
//! - `init(index_bin, engine)`：`<index>.content/` 存在则 mmap 打开并起后台增量线程；
//!   不存在则保持 `Missing`（`content:` 查询自动回退逐文件慢路径，行为正确只是慢）。
//!   打开时校验指纹边车（schema/文件名索引规模与卷集合），对不上 → `Stale`。
//! - `notify_applied(volume, events)`：watch 热路径调。`Ready` 时只做事件→job 的轻量提取
//!   （不碰 store 读锁、不读文件），job 经有界 channel 扔给后台线程；
//!   `Building` 时暂存 stash（构建完应用，增量不丢）；其他状态直接丢弃。
//! - `start_build(rebuild)`：后台线程分片构建（`plan_range` 短持锁 + `build_from_plan`
//!   无锁执行），`tmp` 目录构建 → 原子 promote → 切增量跟随；可 `cancel_build()`。
//! - `content_opt()`：`search_ipc` / Everything `do_search` 共用，装入
//!   `SearchOptions.content`，使 `content:` 走 tantivy 快路径（仅 `Ready` 时挂载）。
//! - `status_snapshot()`：`Status` 轮询用（2s 一次，零新通道）。
//! - `snippets_for(...)`：搜索装配用，按页内 `entry_idx` 取前 N 个 snippet。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, OnceLock,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use findx2_content::SCHEMA_VERSION;
use findx2_core::{volume_key_for, ChangeEvent, SearchEngine};

/// 构建分片大小：每片持读锁只做纯内存规划（毫秒级），解锁后再抽取入库。
const PLAN_CHUNK: usize = 50_000;
/// 构建期间暂存增量上限，超了计数丢弃（构建完是全量快照，丢的只是重复 upsert）。
const STASH_CAP: usize = 100_000;
/// snippet 每页填充上限（tantivy 按 doc 生成，有界成本）。
pub(crate) const SNIPPET_PER_PAGE: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StateKind {
    Missing,
    Building,
    Ready,
    Stale,
    Failed,
    /// P3 预留：service 应用 GUI 的 follow 开关后启用（当前增量常开）。
    #[allow(dead_code)]
    Disabled,
}

impl StateKind {
    fn as_str(self) -> &'static str {
        use findx2_ipc::content_state as cs;
        match self {
            StateKind::Missing => cs::MISSING,
            StateKind::Building => cs::BUILDING,
            StateKind::Ready => cs::READY,
            StateKind::Stale => cs::STALE,
            StateKind::Failed => cs::FAILED,
            StateKind::Disabled => cs::DISABLED,
        }
    }
}

#[derive(Clone, Copy)]
enum JobKind {
    Upsert,
    Delete,
}

struct Job {
    volume: u8,
    frn: u64,
    kind: JobKind,
}

struct BuildProgress {
    cancel: Arc<AtomicBool>,
    entry_total: u64,
    scanned: u64,
    extracted: u64,
    skipped: u64,
    failed: u64,
    prev_state: StateKind,
}

struct Manager {
    index_bin: PathBuf,
    engine: Option<Arc<SearchEngine>>,
    index: Option<Arc<findx2_content::ContentIndex>>,
    /// 拼音辅库（首字母 / 全拼；缺失即 None，查询自动降级为主库）。
    aux_ini: Option<Arc<findx2_content::ContentIndex>>,
    aux_full: Option<Arc<findx2_content::ContentIndex>>,
    state: StateKind,
    /// Stale 原因 / Failed 错误 / Building 备注。
    detail: String,
    dropped_batches: u64,
    stash_dropped: u64,
    docs: u64,
    bytes: u64,
    entry_total: u64,
    build: Option<BuildProgress>,
    stash: Vec<Job>,
    tx: Option<std::sync::mpsc::SyncSender<Vec<Job>>>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// 拼音打标缓存：needle(+开关) → 辅库命中的 (volume_key, frn) 全集（带查询 limit）。
    /// MergedContentSearch 在合并查询时顺手填入（与打标同源），命中即省掉两次
    /// TopDocs(CAP) 重查；FIFO 上限防内存膨胀。
    aux_mark_cache: HashMap<(String, bool, bool), (usize, HashSet<(String, u64)>)>,
    aux_mark_order: std::collections::VecDeque<(String, bool, bool)>,
}

impl Manager {
    fn new() -> Self {
        Self {
            index_bin: PathBuf::new(),
            engine: None,
            index: None,
            aux_ini: None,
            aux_full: None,
            state: StateKind::Missing,
            detail: String::new(),
            dropped_batches: 0,
            stash_dropped: 0,
            docs: 0,
            bytes: 0,
            entry_total: 0,
            build: None,
            stash: Vec::new(),
            tx: None,
            worker: None,
            aux_mark_cache: HashMap::new(),
            aux_mark_order: std::collections::VecDeque::new(),
        }
    }
}

/// 指纹边车（`<index>.content/findx-content.json`，tantivy 的 meta.json 之外独立文件）。
#[derive(serde::Serialize, serde::Deserialize)]
struct Fingerprint {
    schema_version: u32,
    entry_count: usize,
    volumes: Vec<String>,
    built_unix: u64,
}

const SIDECAR: &str = "findx-content.json";

fn sidecar_path(cdir: &Path) -> PathBuf {
    cdir.join(SIDECAR)
}

fn fingerprint_of(engine: &Arc<SearchEngine>) -> Fingerprint {
    let g = engine.index_store();
    Fingerprint {
        schema_version: SCHEMA_VERSION,
        entry_count: g.entry_count(),
        volumes: g.volumes.iter().map(|v| v.volume_id.clone()).collect(),
        built_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    }
}

fn read_sidecar(cdir: &Path) -> Option<Fingerprint> {
    let bytes = std::fs::read(sidecar_path(cdir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_sidecar(cdir: &Path, fp: &Fingerprint) {
    let _ = std::fs::write(
        sidecar_path(cdir),
        serde_json::to_vec_pretty(fp).unwrap_or_default(),
    );
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(m) = ent.metadata() {
                total += m.len();
            }
        }
    }
    total
}

/// 指纹判定（纯函数，可单测）：无边车→旧版可用；版本/规模/卷集合任一对不上→Stale。
fn classify(fp: Option<&Fingerprint>, now: &Fingerprint) -> (StateKind, String) {
    match fp {
        None => (
            StateKind::Ready,
            "可用（旧版构建无指纹；范围变更后建议重建）".into(),
        ),
        Some(f) if f.schema_version != SCHEMA_VERSION => (
            StateKind::Stale,
            format!(
                "内容 schema v{}，当前 v{SCHEMA_VERSION}，需重建",
                f.schema_version
            ),
        ),
        Some(f) if f.entry_count != now.entry_count || f.volumes != now.volumes => (
            StateKind::Stale,
            "文件名索引已变化（重建/压缩/换库），建议重建内容索引".into(),
        ),
        Some(_) => (StateKind::Ready, String::new()),
    }
}

fn hub() -> &'static Mutex<Manager> {
    static H: OnceLock<Mutex<Manager>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(Manager::new()))
}

fn stop_incremental_locked(m: &mut Manager) {
    m.tx.take();
    if let Some(h) = m.worker.take() {
        let _ = h.join();
    }
}

fn start_incremental_locked(
    m: &mut Manager,
    engine: Arc<SearchEngine>,
    index: Arc<findx2_content::ContentIndex>,
    aux_ini: Option<Arc<findx2_content::ContentIndex>>,
    aux_full: Option<Arc<findx2_content::ContentIndex>>,
) {
    if m.tx.is_some() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<Job>>(256);
    m.tx = Some(tx);
    let cdir = findx2_content::content_dir_for(&m.index_bin);
    std::thread::Builder::new()
        .name("findx2-content-index".into())
        .spawn(move || {
            worker_loop(engine, index, aux_ini, aux_full, rx, move || {
                if let Ok(mut g) = hub().lock() {
                    g.bytes = dir_size(&cdir);
                }
            });
        })
        .ok();
}

/// service 启动后调（engine 已挂 EngineSlot 时机均可，早调早跟随）。
/// 内容索引不存在 → 返回 false（`Missing`），不报错；存在 → 挂载并返回 true。
pub(crate) fn init(index_bin: &std::path::Path, engine: Arc<SearchEngine>) -> bool {
    // 顺手清理历史残留的 promote 备份（best-effort，失败不管）。
    let cdir = findx2_content::content_dir_for(index_bin);
    if let Ok(rd) = std::fs::read_dir(&cdir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("content-backup-"))
            {
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }
    let mut m = hub().lock().expect("content hub 锁中毒");
    stop_incremental_locked(&mut m);
    m.index_bin = index_bin.to_path_buf();
    m.engine = Some(engine.clone());
    m.stash.clear();
    m.build = None;
    m.aux_ini = None;
    m.aux_full = None;
    let Some(cidx) = findx2_content::open_if_exists(index_bin) else {
        m.index = None;
        m.docs = 0;
        m.bytes = 0;
        if findx2_content::schema_mismatch(index_bin) {
            // 新旧二进制混跑：库是旧 schema，静默走慢路径会超时；明确提示重建。
            m.state = StateKind::Stale;
            m.detail = "内容索引版本过旧，需重建（全文页点“重建”）".into();
            tracing::warn!(
                "内容索引 schema 版本过旧（{}），`content:` 查询走慢路径，请重建",
                cdir.display()
            );
        } else {
            m.state = StateKind::Missing;
            m.detail = "内容索引未建（`findx2 content-index` 或 GUI 全文页构建后加速）".into();
            tracing::info!(
                "内容索引未建（{} 不存在）：`content:` 查询走慢路径",
                cdir.display()
            );
        }
        return false;
    };
    let cidx = Arc::new(cidx);
    let fp_now = fingerprint_of(&engine);
    let fp_old = read_sidecar(&cdir);
    let (state, detail) = classify(fp_old.as_ref(), &fp_now);
    m.state = state;
    m.detail = detail;
    if matches!(state, StateKind::Ready) && fp_old.is_none() {
        // 旧版 CLI 建的库：无指纹，补写当前指纹（下次即可比对）。
        write_sidecar(&cdir, &fp_now);
    }
    m.docs = cidx.num_docs();
    m.bytes = dir_size(&cdir);
    m.entry_total = fp_now.entry_count as u64;
    m.index = Some(cidx.clone());
    // 拼音辅库：版本一致才挂载，否则视为未建（查询降级，不报错）。
    let (aux_ini, aux_full) = findx2_content::open_aux(index_bin);
    m.aux_ini = aux_ini.map(Arc::new);
    m.aux_full = aux_full.map(Arc::new);
    let (ai, af) = (m.aux_ini.clone(), m.aux_full.clone());
    start_incremental_locked(&mut m, engine, cidx, ai, af);
    let docs = m.docs;
    let state = m.state;
    drop(m);
    tracing::info!(
        "内容索引已挂载：{}（{} docs，{:?}），增量跟随启动",
        cdir.display(),
        docs,
        state
    );
    true
}

/// 查询装配：search_ipc / do_search 共用。仅 `Ready` 挂载，其他状态回退慢路径。
pub(crate) fn content_opt() -> Option<Arc<dyn findx2_core::ContentSearch>> {
    content_opt_with(true, true)
}

/// 带拼音开关的装配（IPC `pinyin_initials` / `pinyin_full` 透传）。
/// 辅库缺失/开关关闭 → 自动降级为主库直查。
pub(crate) fn content_opt_with(
    use_ini: bool,
    use_full: bool,
) -> Option<Arc<dyn findx2_core::ContentSearch>> {
    let g = hub().lock().ok()?;
    if g.state != StateKind::Ready {
        return None;
    }
    let main = g.index.clone()?;
    let ini = if use_ini { g.aux_ini.clone() } else { None };
    let full = if use_full { g.aux_full.clone() } else { None };
    if ini.is_none() && full.is_none() {
        return Some(main as Arc<dyn findx2_core::ContentSearch>);
    }
    Some(Arc::new(MergedContentSearch {
        main,
        ini,
        full,
    }) as Arc<dyn findx2_core::ContentSearch>)
}

/// 主库 + 拼音辅库合并查询：主库精确命中在前，辅库拼音命中补后（按键去重，截断到 limit）。
struct MergedContentSearch {
    main: Arc<findx2_content::ContentIndex>,
    ini: Option<Arc<findx2_content::ContentIndex>>,
    full: Option<Arc<findx2_content::ContentIndex>>,
}

impl findx2_core::ContentSearch for MergedContentSearch {
    fn search_content(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, u64)>, findx2_core::Error> {
        use std::collections::HashSet;
        let map_err = |e: findx2_content::Error| {
            findx2_core::Error::Persist(format!("内容索引查询失败: {e}"))
        };
        let main_hits = self
            .main
            .search_ids(query, limit)
            .map_err(map_err)?;
        if !query.is_ascii() || limit == 0 {
            return Ok(main_hits
                .into_iter()
                .map(|(v, f, _)| (v, f))
                .collect());
        }
        let mut seen: HashSet<(String, u64)> = HashSet::with_capacity(limit * 2);
        let mut out: Vec<(String, u64)> = Vec::with_capacity(limit);
        for (v, f, _) in main_hits {
            if out.len() >= limit {
                break;
            }
            seen.insert((v.clone(), f));
            out.push((v, f));
        }
        if out.len() >= limit {
            return Ok(out);
        }
        // 辅库各取 limit 条（score 截断由主合并 cap 兜底），合并去重。
        let mut aux_all: Vec<(String, u64, f32)> = Vec::new();
        if let Some(ini) = self.ini.as_ref() {
            if let Ok(h) = ini.search_pinyin(query, limit) {
                aux_all.extend(h);
            }
        }
        if let Some(full) = self.full.as_ref() {
            if let Ok(h) = full.search_pinyin(query, limit) {
                aux_all.extend(h);
            }
        }
        // 顺手喂打标缓存：同 needle 的徽标判定免重查（与打标同源同 limit）。
        // main 填满提前返回的路径不写（辅库根本没查，写了会毒化缓存）。
        note_aux_marks(
            query,
            self.ini.is_some(),
            self.full.is_some(),
            limit,
            aux_all.iter().map(|(v, f, _)| (v.clone(), *f)),
        );
        // 按 score 降序，让更像的拼音命中靠前。
        aux_all.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        for (v, f, _) in aux_all {
            if out.len() >= limit {
                break;
            }
            if seen.insert((v.clone(), f)) {
                out.push((v, f));
            }
        }
        Ok(out)
    }
}

/// 拼音命中键集合（service 侧打 `pinyin_hit` 徽标用；与主库是否命中无关，只表示辅库可查到）。
/// 非 ASCII 查询直接返回空（走主库中文分词，无拼音概念）。
/// 合并查询已顺手填缓存（同 needle 同开关同 limit）：命中即免两次 TopDocs 重查。
pub(crate) fn aux_pinyin_keys(
    query: &str,
    use_ini: bool,
    use_full: bool,
    limit: usize,
) -> std::collections::HashSet<(String, u64)> {
    use std::collections::HashSet;
    let mut out = HashSet::new();
    if !query.is_ascii() || limit == 0 {
        return out;
    }
    if let Some(cached) = read_aux_marks(query, use_ini, use_full, limit) {
        return cached;
    }
    let g = match hub().lock() {
        Ok(g) => g,
        Err(_) => return out,
    };
    if g.state != StateKind::Ready {
        return out;
    }
    if use_ini {
        if let Some(ini) = g.aux_ini.as_ref() {
            if let Ok(h) = ini.search_pinyin(query, limit) {
                for (v, f, _) in h {
                    out.insert((v, f));
                }
            }
        }
    }
    if use_full {
        if let Some(full) = g.aux_full.as_ref() {
            if let Ok(h) = full.search_pinyin(query, limit) {
                for (v, f, _) in h {
                    out.insert((v, f));
                }
            }
        }
    }
    drop(g);
    // 回填缓存（合并查询没跑过此 needle 时走这里，后续翻页/重查命中）。
    note_aux_marks(query, use_ini, use_full, limit, out.iter().map(|(v, f)| (v.clone(), *f)));
    out
}

/// 打标缓存上限（needle 级，每项约 5 万 × (短串+u64) ≈ 2~3MB）。
const AUX_MARK_CACHE_CAP: usize = 4;

/// 合并查询顺手填入 / miss 时回填（FIFO 逐出最老）。
fn note_aux_marks(
    query: &str,
    use_ini: bool,
    use_full: bool,
    limit: usize,
    keys: impl Iterator<Item = (String, u64)>,
) {
    let Ok(mut g) = hub().lock() else {
        return;
    };
    let k = (query.to_string(), use_ini, use_full);
    if !g.aux_mark_cache.contains_key(&k) {
        g.aux_mark_order.push_back(k.clone());
        while g.aux_mark_order.len() > AUX_MARK_CACHE_CAP {
            if let Some(old) = g.aux_mark_order.pop_front() {
                g.aux_mark_cache.remove(&old);
            }
        }
    }
    g.aux_mark_cache.insert(k, (limit, keys.collect()));
}

/// 缓存命中要求同 needle 同开关且缓存 limit 覆盖本次请求（合并查询的 cap
/// 随候选集大小浮动；覆盖不足时回退重查，不毒化徽标）。
fn read_aux_marks(
    query: &str,
    use_ini: bool,
    use_full: bool,
    limit: usize,
) -> Option<std::collections::HashSet<(String, u64)>> {
    let g = hub().lock().ok()?;
    let (cached_limit, set) = g
        .aux_mark_cache
        .get(&(query.to_string(), use_ini, use_full))?;
    if *cached_limit >= limit {
        Some(set.clone())
    } else {
        None
    }
}

/// watch 热路径调。`Ready/Stale/Failed` 翻译成 job（不阻塞，不碰 store）；
/// `Building` 暂存 stash（构建完应用）；其他状态丢弃。
pub(crate) fn notify_applied(volume: u8, events: &[ChangeEvent]) {
    let jobs: Vec<Job> = events
        .iter()
        .filter_map(|ev| match ev {
            ChangeEvent::Create { entry } if !entry.is_dir => Some(Job {
                volume,
                frn: entry.file_id,
                kind: JobKind::Upsert,
            }),
            ChangeEvent::CreatePending { file_id, is_dir, .. } if !is_dir => Some(Job {
                volume,
                frn: *file_id,
                kind: JobKind::Upsert,
            }),
            ChangeEvent::Delete { file_id } => Some(Job {
                volume,
                frn: *file_id,
                kind: JobKind::Delete,
            }),
            ChangeEvent::Rename { file_id, .. } => Some(Job {
                volume,
                frn: *file_id,
                kind: JobKind::Upsert,
            }),
            ChangeEvent::DataOrMeta { file_id, .. } => Some(Job {
                volume,
                frn: *file_id,
                kind: JobKind::Upsert,
            }),
            _ => None,
        })
        .collect();
    if jobs.is_empty() {
        return;
    }
    let mut g = match hub().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if g.state == StateKind::Building {
        if g.stash.len() + jobs.len() <= STASH_CAP {
            g.stash.extend(jobs);
        } else {
            g.stash_dropped += jobs.len() as u64;
            tracing::warn!(
                "内容构建中增量暂存溢出，丢弃 {}（构建完是全量快照，可自愈）",
                jobs.len()
            );
        }
        return;
    }
    let Some(tx) = g.tx.as_ref() else {
        return;
    };
    if tx.try_send(jobs).is_err() {
        g.dropped_batches += 1;
        tracing::warn!("内容索引 channel 满：丢弃一批增量（搜索优先；重建可自愈）");
    }
}

/// 开始后台构建。`rebuild=true` 删旧全量重建；否则复用追加（旧索引在构建期间照常服务）。
/// 已在构建 → Err。调用方（IPC）立即返回，进度走 `status_snapshot` 轮询。
pub(crate) fn start_build(rebuild: bool) -> Result<String, String> {
    let (engine, index_bin, prev, cancel) = {
        let mut g = hub().lock().map_err(|_| "内容管理器锁中毒".to_string())?;
        if g.state == StateKind::Building {
            return Err("内容索引正在构建中".into());
        }
        let engine = g.engine.clone().ok_or("文件名索引尚未加载")?;
        // 构建期间停增量 worker（同一目录不允许两个 writer），job 进 stash。
        stop_incremental_locked(&mut g);
        let prev = g.index.clone();
        let prev_state = g.state;
        let cancel = Arc::new(AtomicBool::new(false));
        g.state = StateKind::Building;
        g.detail = if rebuild { "全量重建中" } else { "增量复用构建中" }.into();
        g.build = Some(BuildProgress {
            cancel: cancel.clone(),
            entry_total: 0,
            scanned: 0,
            extracted: 0,
            skipped: 0,
            failed: 0,
            prev_state,
        });
        g.stash.clear();
        (engine, g.index_bin.clone(), prev, cancel)
    };
    std::thread::Builder::new()
        .name("findx2-content-build".into())
        .spawn(move || {
            // 构建线程 panic（毒文件/解析器 bug 漏网）不能无声冻屏：
            // 兜底落 FAILED 状态，GUI 看得到、下次可重建。
            if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_build(engine, index_bin, rebuild, prev, cancel)
            })) {
                let msg = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| {
                        payload
                            .downcast_ref::<&str>()
                            .map(|s| (*s).to_string())
                    })
                    .unwrap_or_else(|| "构建线程 panic".to_string());
                tracing::error!("内容索引构建线程 panic：{msg}");
                if let Ok(mut g) = hub().lock() {
                    g.build = None;
                    g.state = StateKind::Failed;
                    g.detail = format!("构建线程崩溃（{msg}），可重试");
                }
            }
        })
        .map_err(|e| format!("构建线程启动失败：{e}"))?;
    Ok(if rebuild { "已开始全量重建" } else { "已开始构建" }.into())
}

/// 取消正在进行的构建（幂等；无构建在跑也返回 true）。
pub(crate) fn cancel_build() -> bool {
    let g = hub().lock().ok();
    match g.as_ref().and_then(|g| g.build.as_ref()) {
        Some(b) => {
            b.cancel.store(true, Ordering::Release);
            true
        }
        None => true,
    }
}

pub(crate) struct ContentStatusSnapshot {
    pub state: &'static str,
    pub detail: String,
    pub docs: u64,
    pub bytes: u64,
    pub entry_total: u64,
    pub scanned: u64,
    pub extracted: u64,
    pub skipped: u64,
    pub failed: u64,
    pub error: Option<String>,
    /// service 实际挂载的内容目录（GUI 设置页展示用；不从 GUI 的 indexPath 推导，
    /// dev 隔离 service 的 index 与 GUI 设置里的正式库路径不同）。
    pub content_dir: String,
    /// 拼音辅库挂载情况（缺库/开关关闭即 0，查询自动降级）。
    pub aux_ini_docs: u64,
    pub aux_ini_bytes: u64,
    pub aux_full_docs: u64,
    pub aux_full_bytes: u64,
}

/// Status 轮询用快照（2s 一次；只读计数器，不碰 tantivy）。
pub(crate) fn status_snapshot() -> ContentStatusSnapshot {
    let g = hub().lock().expect("content hub 锁中毒");
    let (scanned, extracted, skipped, failed, entry_total) = match &g.build {
        Some(b) => (b.scanned, b.extracted, b.skipped, b.failed, b.entry_total),
        None => (0, 0, 0, 0, g.entry_total),
    };
    let mut error = None;
    if g.state == StateKind::Failed {
        error = Some(g.detail.clone());
    } else if g.dropped_batches > 0 {
        error = Some(format!(
            "增量丢批 {} 次（搜索优先；重建可自愈）",
            g.dropped_batches
        ));
    }
    ContentStatusSnapshot {
        state: g.state.as_str(),
        detail: g.detail.clone(),
        docs: g.docs,
        bytes: g.bytes,
        entry_total,
        scanned,
        extracted,
        skipped,
        failed,
        error,
        content_dir: findx2_content::content_dir_for(&g.index_bin)
            .display()
            .to_string(),
        aux_ini_docs: g.aux_ini.as_ref().map(|a| a.num_docs()).unwrap_or(0),
        aux_ini_bytes: g
            .aux_ini
            .as_ref()
            .map(|_| dir_size(&findx2_content::aux_dir_for(&g.index_bin, findx2_content::AUX_INITIALS_SUFFIX)))
            .unwrap_or(0),
        aux_full_docs: g.aux_full.as_ref().map(|a| a.num_docs()).unwrap_or(0),
        aux_full_bytes: g
            .aux_full
            .as_ref()
            .map(|_| dir_size(&findx2_content::aux_dir_for(&g.index_bin, findx2_content::AUX_FULL_SUFFIX)))
            .unwrap_or(0),
    }
}

/// 搜索装配用：按页内 `entry_idx` 取前 `cap` 个 snippet（key→html，无命中缺席）。
/// 搜索装配用：按页内 `entry_idx` 取第 `window_idx` 个匹配窗口的 snippet。
/// 返回 `entry_idx → (html, 全文匹配点总数, 实际窗口号)`；找不到/回读失败的 key 缺席。
pub(crate) fn snippets_for(
    engine: &SearchEngine,
    query: &str,
    entry_indices: &[u32],
    cap: usize,
    window_idx: usize,
) -> HashMap<u32, (String, usize, usize)> {
    let cidx = match hub().lock().ok().and_then(|g| g.index.clone()) {
        Some(c) => c,
        None => return HashMap::new(),
    };
    // entry_idx → (volume_key, frn)：FRN 只在卷内唯一，逐卷核对 slot。
    let (keys, mut rev): (Vec<u64>, HashMap<u64, u32>) = {
        let g = engine.index_store();
        let mut keys = Vec::with_capacity(entry_indices.len().min(cap.max(1)));
        let mut rev = HashMap::new();
        for &idx in entry_indices {
            let frn = match g.frns.get(idx as usize).copied() {
                Some(f) => f,
                None => continue,
            };
            for (slot, vol) in g.volumes.iter().enumerate() {
                if g.frn_to_entry.get_scoped(frn, slot as u32) == Some(idx) {
                    let key = findx2_content::doc_key(&volume_key_for(&vol.volume_id, slot), frn);
                    keys.push(key);
                    rev.insert(key, idx);
                    break;
                }
            }
            if keys.len() >= cap.max(1) {
                break;
            }
        }
        (keys, rev)
    };
    cidx.snippets_for_keys_windowed(query, &keys, cap, window_idx)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, (html, total, window))| rev.remove(&k).map(|idx| (idx, (html, total, window))))
        .collect()
}

fn tmp_build_dir(final_cdir: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut s = final_cdir.as_os_str().to_owned();
    s.push(format!(".build-{nanos}"));
    PathBuf::from(s)
}

/// 原子 promote：`final → backup` → `tmp → final` → 删 backup。
/// Windows 下旧目录可能被正在进行的搜索 mmap 占用：rename 失败则整体放弃，
/// 旧索引继续服务，调用方（GUI）提示重试；tmp 保留供下次复用/清理。
fn promote_dir(tmp: &Path, fin: &Path) -> Result<(), String> {
    if !fin.exists() {
        std::fs::rename(tmp, fin).map_err(|e| format!("发布内容索引失败：{e}"))?;
        return Ok(());
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut s = fin.as_os_str().to_owned();
    s.push(format!(".backup-{nanos}"));
    let bak = PathBuf::from(s);
    std::fs::rename(fin, &bak).map_err(|e| format!("旧索引移出失败（可能有搜索正在进行）：{e}"))?;
    match std::fs::rename(tmp, fin) {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&bak);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::rename(&bak, fin);
            Err(format!("新索引就位失败，已恢复旧索引：{e}"))
        }
    }
}

fn run_build(
    engine: Arc<SearchEngine>,
    index_bin: PathBuf,
    rebuild: bool,
    prev: Option<Arc<findx2_content::ContentIndex>>,
    cancel_flag: Arc<AtomicBool>,
) {
    let final_cdir = findx2_content::content_dir_for(&index_bin);
    // 复用追加可直接写 final（增量 worker 已停，无 writer 冲突）；
    // 重建/新建走 tmp + promote，旧索引构建期间照常服务。
    let use_tmp = rebuild || !final_cdir.join("meta.json").is_file();
    let cdir = if use_tmp {
        tmp_build_dir(&final_cdir)
    } else {
        final_cdir.clone()
    };
    if use_tmp {
        if cdir.exists() {
            let _ = std::fs::remove_dir_all(&cdir);
        }
    }
    let entry_len = engine.index_store().entries.len();
    {
        let mut g = match hub().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(b) = g.build.as_mut() {
            b.entry_total = entry_len as u64;
        }
    }
    let vols = {
        let g = engine.index_store();
        findx2_content::build_vols(&g)
    };
    let mut st = findx2_content::BuildStats::default();
    let mut first = true;
    let mut result: Result<u64, findx2_content::Error> = Ok(0);
    let mut start = 0usize;
    while start < entry_len {
        if cancel_flag.load(Ordering::Acquire) {
            result = Err(findx2_content::Error::Cancelled);
            break;
        }
        let end = (start + PLAN_CHUNK).min(entry_len);
        // 短持锁：纯内存规划，毫秒级。
        let items = {
            let g = engine.index_store();
            let mut tmp_st = findx2_content::BuildStats::default();
            let items = findx2_content::plan_range_from_store(&g, &vols, start, end, &mut tmp_st);
            st.scanned += tmp_st.scanned;
            st.failed += tmp_st.failed;
            items
        };
        let rebuild_flag = first && (rebuild || use_tmp);
        first = false;
        let flag = cancel_flag.clone();
        match findx2_content::build_from_plan(&items, &cdir, rebuild_flag, &mut st, |s| {
            if let Ok(mut g) = hub().lock() {
                if let Some(b) = g.build.as_mut() {
                    b.scanned = s.scanned;
                    b.extracted = s.extracted;
                    b.skipped = s.skipped;
                    b.failed = s.failed;
                }
            }
            !flag.load(Ordering::Acquire)
        }) {
            Ok(_) => {}
            Err(e) => {
                result = Err(e);
                break;
            }
        }
        start = end;
    }
    let docs = match result {
        Err(findx2_content::Error::Cancelled) => {
            if use_tmp {
                let _ = std::fs::remove_dir_all(&cdir);
            }
            let mut g = match hub().lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let prev_state = g.build.as_ref().map(|b| b.prev_state).unwrap_or(StateKind::Missing);
            // 复用追加被取消：final 里可能有未 commit 的隐形段（下次打开不可见，无害）。
            g.build = None;
            if prev.is_some() {
                g.state = StateKind::Ready;
                g.detail = "构建已取消，仍用旧索引".into();
                let (eng, idx, ai, af) = (
                    g.engine.clone(),
                    g.index.clone(),
                    g.aux_ini.clone(),
                    g.aux_full.clone(),
                );
                drop(g);
                if let (Some(eng), Some(idx)) = (eng, idx) {
                    let mut m = hub().lock().expect("content hub 锁中毒");
                    start_incremental_locked(&mut m, eng, idx, ai, af);
                }
            } else {
                g.state = prev_state;
                g.detail = "构建已取消".into();
            }
            tracing::info!("内容索引构建已取消");
            return;
        }
        Err(e) => {
            if use_tmp {
                let _ = std::fs::remove_dir_all(&cdir);
            }
            let mut g = match hub().lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            g.build = None;
            if prev.is_some() {
                // 重建失败：旧索引继续服务 + 恢复增量。
                g.state = StateKind::Ready;
                g.detail = format!("重建失败，仍用旧索引：{e}");
                let (eng, idx, ai, af) = (
                    g.engine.clone(),
                    g.index.clone(),
                    g.aux_ini.clone(),
                    g.aux_full.clone(),
                );
                drop(g);
                if let (Some(eng), Some(idx)) = (eng, idx) {
                    let mut m = hub().lock().expect("content hub 锁中毒");
                    start_incremental_locked(&mut m, eng, idx, ai, af);
                }
            } else {
                g.state = StateKind::Failed;
                g.detail = format!("构建失败：{e}");
            }
            tracing::warn!("内容索引构建失败：{e}");
            return;
        }
        Ok(d) => d,
    };
    // 成功：promote（tmp 路径）→ 重开 → 指纹 → stash 应用 → 恢复增量。
    if use_tmp {
        if let Err(e) = promote_dir(&cdir, &final_cdir) {
            let mut g = match hub().lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            g.build = None;
            if prev.is_some() {
                g.state = StateKind::Ready;
                g.detail = format!("发布失败，仍用旧索引：{e}");
                let (eng, idx, ai, af) = (
                    g.engine.clone(),
                    g.index.clone(),
                    g.aux_ini.clone(),
                    g.aux_full.clone(),
                );
                drop(g);
                if let (Some(eng), Some(idx)) = (eng, idx) {
                    let mut m = hub().lock().expect("content hub 锁中毒");
                    start_incremental_locked(&mut m, eng, idx, ai, af);
                }
            } else {
                g.state = StateKind::Failed;
                g.detail = e;
            }
            tracing::warn!("内容索引发布失败");
            return;
        }
    }
    let cidx = match findx2_content::open_if_exists(&index_bin) {
        Some(c) => Arc::new(c),
        None => {
            let mut g = match hub().lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            g.build = None;
            g.state = StateKind::Failed;
            g.detail = "构建完成但索引打不开（磁盘异常？）".into();
            return;
        }
    };
    // 主库建成后重建拼音辅库（幂等 upsert；全量 tmp 发布后 rebuild=true 清旧）。
    // 辅库失败不影响主库可用：记 detail 提示，查询自动降级。
    let aux_note = match findx2_content::build_aux_from_index(&index_bin, use_tmp, |_, _| {
        !cancel_flag.load(std::sync::atomic::Ordering::Acquire)
    }) {
        Ok(st) => {
            tracing::info!(
                "拼音辅库重建完成：{} docs（ini {}/full {}）",
                st.docs,
                st.docs_ini,
                st.docs_full
            );
            String::new()
        }
        Err(e) => {
            tracing::warn!("拼音辅库重建失败（主库可用，拼音降级）：{e}");
            format!("（拼音辅库失败：{e}）")
        }
    };
    let (aux_ini_new, aux_full_new) = findx2_content::open_aux(&index_bin);
    write_sidecar(&final_cdir, &fingerprint_of(&engine));
    let bytes = dir_size(&final_cdir);
    let stash: Vec<Job> = {
        let mut g = match hub().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        g.build = None;
        g.index = Some(cidx.clone());
        g.aux_ini = aux_ini_new.map(Arc::new);
        g.aux_full = aux_full_new.map(Arc::new);
        g.docs = docs;
        g.bytes = bytes;
        g.entry_total = entry_len as u64;
        g.state = StateKind::Ready;
        g.detail.clear();
        if !aux_note.is_empty() {
            g.detail = aux_note;
        }
        let (ai, af) = (g.aux_ini.clone(), g.aux_full.clone());
        start_incremental_locked(&mut g, engine.clone(), cidx.clone(), ai, af);
        std::mem::take(&mut g.stash)
    };
    // 构建窗口期的增量：按 FRN 重新 resolve 后应用（改名/删除天然收敛，主+辅同写）。
    if !stash.is_empty() {
        let n = stash.len();
        let (ai, af) = hub()
            .lock()
            .ok()
            .map(|g| (g.aux_ini.clone(), g.aux_full.clone()))
            .unwrap_or((None, None));
        for job in &stash {
            let _ = apply_job(&engine, &cidx, ai.as_ref(), af.as_ref(), job);
        }
        let _ = cidx.commit();
        if let Some(a) = ai.as_ref() {
            let _ = a.commit();
        }
        if let Some(a) = af.as_ref() {
            let _ = a.commit();
        }
        tracing::info!("内容索引构建完成，应用构建窗口增量 {n} 条");
    }
    tracing::info!(
        "内容索引构建完成：{} docs，{} → 增量跟随已恢复",
        docs,
        final_cdir.display()
    );
}

fn worker_loop(
    engine: Arc<SearchEngine>,
    index: Arc<findx2_content::ContentIndex>,
    aux_ini: Option<Arc<findx2_content::ContentIndex>>,
    aux_full: Option<Arc<findx2_content::ContentIndex>>,
    rx: std::sync::mpsc::Receiver<Vec<Job>>,
    on_commit: impl Fn() + Send + 'static,
) {
    let mut since_commit = 0u64;
    let mut last_commit = Instant::now();
    let mut upserts = 0u64;
    let mut deletes = 0u64;
    let mut failed = 0u64;
    loop {
        // 5 秒一批：忙时吞吐，闲时及时可见。
        let mut batch: Vec<Job> = match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(j) => j,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Vec::new(),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        while let Ok(mut more) = rx.try_recv() {
            batch.append(&mut more);
            if batch.len() >= 5000 {
                break;
            }
        }
        for job in &batch {
            match apply_job(&engine, &index, aux_ini.as_ref(), aux_full.as_ref(), job) {
                Ok(true) => {
                    since_commit += 1;
                    match job.kind {
                        JobKind::Upsert => upserts += 1,
                        JobKind::Delete => deletes += 1,
                    }
                }
                Ok(false) => {}
                Err(_) => failed += 1,
            }
        }
        if since_commit >= 500 || (since_commit > 0 && last_commit.elapsed() >= Duration::from_secs(30)) {
            if index.commit().is_ok() {
                if let Some(a) = aux_ini.as_ref() {
                    let _ = a.commit();
                }
                if let Some(a) = aux_full.as_ref() {
                    let _ = a.commit();
                }
                since_commit = 0;
                last_commit = Instant::now();
                on_commit();
            }
        }
    }
    let _ = index.commit();
    if let Some(a) = aux_ini.as_ref() {
        let _ = a.commit();
    }
    if let Some(a) = aux_full.as_ref() {
        let _ = a.commit();
    }
    on_commit();
    tracing::info!(
        "内容索引线程退出：upsert {upserts} / delete {deletes} / 失败 {failed}"
    );
}

/// 单 job：resolve → 抽取 → 入库（主库 + 拼音辅库同写）。返回 Ok(true)=产生变更。
fn apply_job(
    engine: &Arc<SearchEngine>,
    index: &Arc<findx2_content::ContentIndex>,
    aux_ini: Option<&Arc<findx2_content::ContentIndex>>,
    aux_full: Option<&Arc<findx2_content::ContentIndex>>,
    job: &Job,
) -> Result<bool, ()> {
    // 路径解析：FRN 在多卷下可能碰撞，按 volume_letter 命中卷，兜底扫全卷。
    let (slot, vol_key, path, mtime, size) = {
        let g = engine.index_store();
        let mut hit: Option<(u32, String, String, u64, u64)> = None;
        for (i, v) in g.volumes.iter().enumerate() {
            if v.volume_letter != job.volume && job.volume != 0 {
                continue;
            }
            let idx = match g.frn_to_entry.get_scoped(job.frn, i as u32) {
                Some(idx) => idx as usize,
                None => continue,
            };
            let e = match g.entries.get(idx) {
                Some(e) => e,
                None => continue,
            };
            if e.is_dir_entry() || e.is_deleted() {
                // 目录/墓碑：Delete 事件走删除；Upsert 遇到说明条目已变，直接跳过。
                if matches!(job.kind, JobKind::Delete) {
                    hit = Some((
                        i as u32,
                        volume_key_for(&v.volume_id, i),
                        String::new(),
                        0,
                        0,
                    ));
                    break;
                }
                continue;
            }
            let p = g.entry_display_path(idx).map_err(|_| ())?;
            hit = Some((
                i as u32,
                volume_key_for(&v.volume_id, i),
                p,
                e.mtime as u64,
                e.size,
            ));
            break;
        }
        // volume_letter==0（unix 多卷）或字母对不上：全卷扫 FRN。
        if hit.is_none() {
            for (i, v) in g.volumes.iter().enumerate() {
                let idx = match g.frn_to_entry.get_scoped(job.frn, i as u32) {
                    Some(idx) => idx as usize,
                    None => continue,
                };
                let e = match g.entries.get(idx) {
                    Some(e) => e,
                    None => continue,
                };
                if e.is_dir_entry() || e.is_deleted() {
                    continue;
                }
                let p = g.entry_display_path(idx).map_err(|_| ())?;
                hit = Some((
                    i as u32,
                    volume_key_for(&v.volume_id, i),
                    p,
                    e.mtime as u64,
                    e.size,
                ));
                break;
            }
        }
        hit.ok_or(())?
    };
    let _ = slot;
    match job.kind {
        JobKind::Delete => {
            index.delete_by_frn(&vol_key, job.frn).map_err(|_| ())?;
            if let Some(a) = aux_ini {
                let _ = a.delete_by_frn(&vol_key, job.frn);
            }
            if let Some(a) = aux_full {
                let _ = a.delete_by_frn(&vol_key, job.frn);
            }
            Ok(true)
        }
        JobKind::Upsert => {
            let p = std::path::Path::new(&path);
            if findx2_content::should_extract(p).is_none() {
                return Ok(false);
            }
            let t = findx2_content::extract_text(p).map_err(|_| ())?;
            let meta = findx2_content::DocMeta {
                frn: job.frn,
                volume_id: vol_key,
                path,
                mtime,
                size,
            };
            index.index_doc(&meta, &t.text).map_err(|_| ())?;
            // 辅库同写：拼音文本由原文派生（与回填同一算法），失败不影响主库。
            if aux_ini.is_some() || aux_full.is_some() {
                let (ini_text, full_text) = findx2_content::pinyin_texts(&t.text);
                if let Some(a) = aux_ini {
                    if !ini_text.is_empty() {
                        let _ = a.index_doc(&meta, &ini_text);
                    } else {
                        let _ = a.delete_by_frn(&meta.volume_id, meta.frn);
                    }
                }
                if let Some(a) = aux_full {
                    if !full_text.is_empty() {
                        let _ = a.index_doc(&meta, &full_text);
                    } else {
                        let _ = a.delete_by_frn(&meta.volume_id, meta.frn);
                    }
                }
            }
            Ok(true)
        }
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;

    fn fp(schema: u32, entries: usize, vols: &[&str]) -> Fingerprint {
        Fingerprint {
            schema_version: schema,
            entry_count: entries,
            volumes: vols.iter().map(|s| s.to_string()).collect(),
            built_unix: 0,
        }
    }

    #[test]
    fn missing_sidecar_is_usable_legacy() {
        let now = fp(SCHEMA_VERSION, 100, &["v1"]);
        let (st, detail) = classify(None, &now);
        assert_eq!(st, StateKind::Ready);
        assert!(detail.contains("旧版"), "{detail}");
    }

    #[test]
    fn schema_bump_needs_rebuild() {
        let now = fp(SCHEMA_VERSION, 100, &["v1"]);
        let other = fp(SCHEMA_VERSION + 1, 100, &["v1"]);
        let (st, detail) = classify(Some(&other), &now);
        assert_eq!(st, StateKind::Stale);
        assert!(detail.contains("需重建"), "{detail}");
    }

    #[test]
    fn index_rescale_or_revolume_is_stale() {
        let now = fp(SCHEMA_VERSION, 100, &["v1"]);
        assert_eq!(classify(Some(&fp(SCHEMA_VERSION, 90, &["v1"])), &now).0, StateKind::Stale);
        assert_eq!(
            classify(Some(&fp(SCHEMA_VERSION, 100, &["v2"])), &now).0,
            StateKind::Stale
        );
        assert_eq!(
            classify(Some(&fp(SCHEMA_VERSION, 100, &["v1"])), &now).0,
            StateKind::Ready
        );
    }

    #[test]
    fn state_strings_match_ipc_contract() {
        use findx2_ipc::content_state as cs;
        assert_eq!(StateKind::Missing.as_str(), cs::MISSING);
        assert_eq!(StateKind::Building.as_str(), cs::BUILDING);
        assert_eq!(StateKind::Ready.as_str(), cs::READY);
        assert_eq!(StateKind::Stale.as_str(), cs::STALE);
        assert_eq!(StateKind::Failed.as_str(), cs::FAILED);
        assert_eq!(StateKind::Disabled.as_str(), cs::DISABLED);
    }
}

#[cfg(test)]
mod merged_search_tests {
    use super::*;
    use findx2_core::ContentSearch;

    fn tiny_index(
        dir: &std::path::Path,
        docs: &[(&str, u64, &str)],
    ) -> Arc<findx2_content::ContentIndex> {
        let idx = Arc::new(findx2_content::open_or_create(dir).unwrap());
        for (vol, frn, body) in docs {
            idx.index_doc(
                &findx2_content::DocMeta {
                    frn: *frn,
                    volume_id: (*vol).to_string(),
                    path: format!("C:\\t\\{frn}.txt"),
                    mtime: 0,
                    size: body.len() as u64,
                },
                body,
            )
            .unwrap();
        }
        idx.commit().unwrap();
        idx
    }

    #[test]
    fn merged_search_finds_cjk_via_pinyin_and_dedups() {
        let case = tempfile::tempdir().unwrap();
        // 主库含 CJK + 一段 ascii（ascii 部分让主/辅同时命中，验证去重）。
        let main = tiny_index(
            &case.path().join("main"),
            &[("v", 7, "全文搜索 qw 备注")],
        );
        let (ini_text, full_text) = findx2_content::pinyin_texts("全文搜索 qw 备注");
        assert!(!ini_text.is_empty() && !full_text.is_empty());
        let ini = tiny_index(&case.path().join("ini"), &[("v", 7, &ini_text)]);
        let full = tiny_index(&case.path().join("full"), &[("v", 7, &full_text)]);
        let merged = MergedContentSearch {
            main,
            ini: Some(ini),
            full: Some(full),
        };
        // 中文直查命中主库。
        let zh = merged.search_content("全文", 50).unwrap();
        assert!(zh.iter().any(|(_, f)| *f == 7), "{zh:?}");
        // 首字母命中（主库亦有 ascii qw，去重后只出现一次）。
        let py = merged.search_content("qw", 50).unwrap();
        assert!(py.iter().any(|(_, f)| *f == 7), "{py:?}");
        assert_eq!(py.iter().filter(|(_, f)| *f == 7).count(), 1, "{py:?}");
        // 全拼命中。
        let qp = merged.search_content("quan wen", 50).unwrap();
        assert!(qp.iter().any(|(_, f)| *f == 7), "{qp:?}");
        // 非 ASCII 查询不走辅库、不 panic。
        let _ = merged.search_content("全文搜索", 50).unwrap();
    }

    #[test]
    fn merged_search_without_aux_behaves_like_main() {
        let case = tempfile::tempdir().unwrap();
        let main = tiny_index(&case.path().join("m"), &[("v", 9, "hello world")]);
        let merged = MergedContentSearch {
            main,
            ini: None,
            full: None,
        };
        let hits = merged.search_content("hello", 50).unwrap();
        assert!(hits.iter().any(|(_, f)| *f == 9), "{hits:?}");
        // 无辅库时拼音查中文为空（不回退慢路径、不 panic）。
        let main2 = tiny_index(&case.path().join("m2"), &[("v", 10, "全文")]);
        let merged2 = MergedContentSearch {
            main: main2,
            ini: None,
            full: None,
        };
        assert!(merged2.search_content("qw", 50).unwrap().is_empty());
    }
}
