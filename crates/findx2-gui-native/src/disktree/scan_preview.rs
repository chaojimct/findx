// Adapted from tobi/disktree 9b18b272ca3c5a145f6ff58eaf89dc4d311eeaea.
// Copyright (c) 2026 Tobi Lütke. MIT; see LICENSE.disktree and README.md.
//! Measured-only, byte-bounded hierarchical overview. Never a second filesystem index.
use super::classify::{Category, category_of_name};
use crate::usage_view::Rect;
use rustc_hash::FxHashMap;
use std::time::{Duration, Instant};
const BUDGET: usize = 256 * 1024;
pub const INTERVAL: Duration = Duration::from_millis(240);
const DURATION: Duration = Duration::from_millis(360);
#[derive(Clone, Debug)]
pub struct Entry {
    pub identity: usize,
    pub name: String,
    pub parent: Option<usize>,
    pub bytes: u64,
    pub own: u64,
    pub files: u64,
    pub directory: bool,
    pub class: Category,
}
#[derive(Clone, Debug, Default)]
pub struct Preview {
    pub entries: Vec<Entry>,
    pub revision: u64,
    pub coarsened: bool,
}
#[derive(Debug, Default)]
pub(crate) struct Builder {
    pub revision: u64,
    entries: Vec<Entry>,
    dirs: FxHashMap<usize, usize>,
    groups: FxHashMap<(usize, Category), usize>,
    charged: usize,
    coarsened: bool,
}
impl Builder {
    fn push(&mut self, mut entry: Entry) -> Option<usize> {
        let charge = 512 + entry.name.len() * 2;
        if self.charged + charge > BUDGET || self.entries.try_reserve(1).is_err() {
            self.coarsened = true;
            return None;
        }
        let i = self.entries.len();
        if !entry.directory {
            entry.identity = (1usize << 63) | i;
        }
        self.entries.push(entry);
        self.charged += charge;
        Some(i)
    }
    pub(crate) fn directory(&mut self, path: &std::path::Path) {
        if self.entries.is_empty() {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .chars()
                .take(192)
                .collect();
            self.push(Entry {
                identity: 0,
                name,
                parent: None,
                bytes: 0,
                own: 0,
                files: 0,
                directory: true,
                class: Category::Other,
            });
        }
    }
    pub(crate) fn add_path(
        &mut self,
        path: &[(usize, String)],
        class: Category,
        bytes: u64,
        files: u64,
    ) {
        if self.entries.is_empty() {
            self.directory(std::path::Path::new("扫描范围"));
        }
        let mut parent = 0;
        for (id, name) in path.iter().take(6) {
            if let Some(i) = self.dirs.get(id) {
                parent = *i;
                continue;
            }
            if self.dirs.try_reserve(1).is_err() {
                self.coarsened = true;
                break;
            }
            let inherited = category_of_name(name).unwrap_or(self.entries[parent].class);
            let Some(i) = self.push(Entry {
                identity: *id,
                name: name.chars().take(192).collect(),
                parent: Some(parent),
                bytes: 0,
                own: 0,
                files: 0,
                directory: true,
                class: inherited,
            }) else {
                break;
            };
            self.dirs.insert(*id, i);
            parent = i;
        }
        let class = if class == Category::Other {
            self.entries[parent].class
        } else {
            class
        };
        let key = (parent, class);
        let leaf = if let Some(i) = self.groups.get(&key) {
            Some(*i)
        } else if self.groups.try_reserve(1).is_ok() {
            let n = self.push(Entry {
                identity: 0,
                name: label(class).into(),
                parent: Some(parent),
                bytes: 0,
                own: 0,
                files: 0,
                directory: false,
                class,
            });
            if let Some(i) = n {
                self.groups.insert(key, i);
            }
            n
        } else {
            None
        };
        let mut at = leaf.unwrap_or(parent);
        self.entries[at].own = self.entries[at].own.saturating_add(bytes);
        loop {
            let n = &mut self.entries[at];
            n.bytes = n.bytes.saturating_add(bytes);
            n.files = n.files.saturating_add(files);
            if let Some(p) = n.parent {
                at = p
            } else {
                break;
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }
    pub(crate) fn snapshot(&self) -> Preview {
        Preview {
            entries: self.entries.clone(),
            revision: self.revision,
            coarsened: self.coarsened,
        }
    }
}
pub fn label(c: Category) -> &'static str {
    match c {
        Category::Code => "代码",
        Category::AgentScratch => "代理草稿",
        Category::Toolchain => "工具链",
        Category::Synced => "同步",
        Category::Git => "Git",
        Category::Media => "媒体",
        Category::Documents => "文档",
        Category::Cache => "缓存",
        Category::Other => "其他已测文件",
    }
}
pub fn file_class(path: &std::path::Path) -> Category {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" | "py" | "js" | "ts" | "tsx" | "cpp" | "h" | "go" | "json" => Category::Code,
        "jpg" | "jpeg" | "png" | "mp4" | "mp3" | "wav" | "mkv" | "webp" => Category::Media,
        "pdf" | "docx" | "xlsx" | "txt" | "md" | "pptx" => Category::Documents,
        "exe" | "dll" | "lib" => Category::Toolchain,
        _ => Category::Other,
    }
}
#[derive(Clone, Debug)]
pub struct Tile {
    pub id: usize,
    pub parent: usize,
    pub rect: Rect,
    pub depth: usize,
    pub header: bool,
    pub header_h: f32,
    pub class: Category,
    pub name: String,
    pub bytes: u64,
    pub opacity: f32,
}
/// Stable balanced partitions: never re-rank siblings by changing weights. Unlike
/// an all-height slice this subdivides in two dimensions, inside directory headers.
fn partition(items: &[(usize, u64)], r: Rect, out: &mut Vec<(usize, Rect)>, axis: bool, bit: u32) {
    if items.is_empty() {
        return;
    }
    if items.len() == 1 {
        out.push((items[0].0, r));
        return;
    }
    fn hash(id: usize) -> u64 {
        let mut x = id as u64;
        x = x.wrapping_add(0x9e3779b97f4a7c15);
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
        x ^ (x >> 31)
    }
    let (mut left, mut right) = (Vec::new(), Vec::new());
    for item in items {
        if hash(item.0) & (1u64 << bit) == 0 {
            left.push(*item)
        } else {
            right.push(*item)
        }
    }
    if left.is_empty() || right.is_empty() {
        if bit > 0 {
            return partition(items, r, out, axis, bit - 1);
        }
        left = items[..items.len() / 2].to_vec();
        right = items[items.len() / 2..].to_vec();
    }
    let sum = items.iter().map(|x| x.1 as f64).sum::<f64>();
    if sum <= 0. {
        return;
    }
    let f = (left.iter().map(|x| x.1 as f64).sum::<f64>() / sum) as f32;
    let (a, b) = if axis {
        (
            Rect { w: r.w * f, ..r },
            Rect {
                x: r.x + r.w * f,
                w: r.w * (1. - f),
                ..r
            },
        )
    } else {
        (
            Rect { h: r.h * f, ..r },
            Rect {
                y: r.y + r.h * f,
                h: r.h * (1. - f),
                ..r
            },
        )
    };
    partition(&left, a, out, !axis, bit.saturating_sub(1));
    partition(&right, b, out, !axis, bit.saturating_sub(1));
}
fn layout(
    p: &Preview,
    bounds: Rect,
    depth: usize,
    scope: usize,
    axes: &mut FxHashMap<usize, bool>,
) -> Vec<Tile> {
    let mut children = vec![vec![]; p.entries.len()];
    for (i, n) in p.entries.iter().enumerate() {
        if let Some(par) = n.parent {
            children[par].push(i)
        }
    }
    fn walk(
        id: usize,
        r: Rect,
        level: usize,
        p: &Preview,
        children: &[Vec<usize>],
        max: usize,
        out: &mut Vec<Tile>,
        axes: &mut FxHashMap<usize, bool>,
    ) {
        let n = &p.entries[id];
        let nested =
            n.directory && !children[id].is_empty() && level < max && r.w > 42. && r.h > 45.;
        if level > 0 {
            out.push(Tile {
                id: n.identity,
                parent: n.parent.map(|i| p.entries[i].identity).unwrap_or(0),
                rect: r,
                depth: level,
                header: nested,
                header_h: if nested { 20. } else { 0. },
                class: n.class,
                name: n.name.clone(),
                bytes: n.bytes,
                opacity: 1.,
            });
        }
        if level >= max {
            return;
        }
        let body = if level == 0 {
            r
        } else if nested {
            Rect {
                x: r.x + 2.,
                y: r.y + 20.,
                w: (r.w - 4.).max(0.),
                h: (r.h - 22.).max(0.),
            }
        } else {
            return;
        };
        let mut weights = children[id]
            .iter()
            .filter_map(|&i| {
                let b = p.entries[i].bytes;
                (b > 0).then_some((i, b))
            })
            .collect::<Vec<_>>();
        if n.directory && n.own > 0 {
            weights.push((usize::MAX - id, n.own));
        }
        let mut rects = vec![];
        let axis = *axes.entry(id).or_insert(body.w >= body.h);
        partition(&weights, body, &mut rects, axis, 63);
        for (i, rect) in rects {
            let rect = rect.inset(1.);
            if rect.w < 1. || rect.h < 1. {
                continue;
            }
            if i >= p.entries.len() {
                out.push(Tile {
                    id: (1usize << 62) | id,
                    parent: n.identity,
                    rect,
                    depth: level + 1,
                    header: false,
                    header_h: 0.,
                    class: n.class,
                    name: "其他已测项（预览合并）".into(),
                    bytes: n.own,
                    opacity: 1.,
                });
            } else {
                walk(i, rect, level + 1, p, children, max, out, axes)
            }
        }
    }
    let mut out = vec![];
    if !p.entries.is_empty() {
        walk(
            p.entries
                .iter()
                .position(|n| n.directory && n.identity == scope)
                .unwrap_or(0),
            bounds,
            0,
            p,
            &children,
            depth.max(1).min(7),
            &mut out,
            axes,
        )
    }
    out
}
#[derive(Clone)]
struct Moving {
    from: Tile,
    to: Tile,
}
#[derive(Default)]
pub struct Motion {
    moving: Vec<Moving>,
    pub revision: u64,
    pub builds: u64,
    pub layout_us: u128,
    pub coarsened: bool,
    last: Option<Instant>,
    start: Option<Instant>,
    size: (u32, u32, usize),
    source: usize,
    pub selected: Option<usize>,
    pub finalized: bool,
    pub scope: usize,
    pub scope_name: String,
    parents: FxHashMap<usize, Option<usize>>,
    scope_dirty: bool,
    axes: FxHashMap<usize, bool>,
}
impl Motion {
    pub fn due(&self, revision: u64, size: (u32, u32, usize), source: usize, now: Instant) -> bool {
        self.scope_dirty
            || self.source != source
            || self.size != size
            || self.last.is_none_or(|t| now.duration_since(t) >= INTERVAL)
                && self.revision != revision
    }
    pub fn running(&self, now: Instant) -> bool {
        self.start.is_some_and(|s| now.duration_since(s) < DURATION)
    }
    fn raw_sample(&self, now: Instant) -> Vec<Tile> {
        let t = self
            .start
            .map(|s| (now.duration_since(s).as_secs_f32() / DURATION.as_secs_f32()).clamp(0., 1.))
            .unwrap_or(1.);
        let t = t * t * (3. - 2. * t);
        self.moving
            .iter()
            .filter_map(|m| {
                let mut n = m.to.clone();
                n.rect = m.from.rect.lerp(m.to.rect, t);
                n.header_h = m.from.header_h + (m.to.header_h - m.from.header_h) * t;
                n.header = n.header_h > 0.5;
                n.opacity = m.from.opacity + (m.to.opacity - m.from.opacity) * t;
                (n.opacity > 0.002).then_some(n)
            })
            .collect::<Vec<_>>()
    }
    pub fn sample(&self, now: Instant) -> Vec<Tile> {
        let mut tiles = self.raw_sample(now);
        let parents = tiles
            .iter()
            .map(|n| (n.id, (n.parent, n.rect, n.header_h)))
            .collect::<FxHashMap<_, _>>();
        for n in &mut tiles {
            let mut p = n.parent;
            for _ in 0..8 {
                if p == self.scope {
                    break;
                }
                let Some((next, rect, header)) = parents.get(&p).copied() else {
                    break;
                };
                let top = rect.y + header;
                let x = n.rect.x.max(rect.x);
                let y = n.rect.y.max(top);
                n.rect = Rect {
                    x,
                    y,
                    w: (n.rect.x + n.rect.w).min(rect.x + rect.w).max(x) - x,
                    h: (n.rect.y + n.rect.h).min(rect.y + rect.h).max(y) - y,
                };
                p = next;
            }
        }
        tiles.retain(|n| n.rect.w > 0.1 && n.rect.h > 0.1);
        tiles
    }

    pub fn selection_detail(&self) -> Option<(String, u64)> {
        self.selected.and_then(|id| {
            self.moving
                .iter()
                .find(|m| m.to.id == id)
                .map(|m| (m.to.name.clone(), m.to.bytes))
        })
    }
    pub fn enter(&mut self, id: usize) -> bool {
        if id != self.scope && self.parents.contains_key(&id) {
            self.scope = id;
            self.scope_dirty = true;
            true
        } else {
            false
        }
    }
    pub fn up(&mut self) -> bool {
        if let Some(Some(id)) = self.parents.get(&self.scope).copied() {
            self.scope = id;
            self.scope_dirty = true;
            true
        } else {
            false
        }
    }
    pub fn first_child(&self) -> Option<usize> {
        self.parents
            .iter()
            .filter_map(|(id, p)| (*p == Some(self.scope)).then_some(*id))
            .min()
    }
    pub fn can_handoff(&self, source: usize) -> bool {
        self.source == source && !self.finalized && !self.moving.is_empty()
    }
    pub fn handoff(&mut self, target: Vec<Tile>, now: Instant, reduce: bool) {
        self.retarget(target, now, reduce);
        self.finalized = true;
    }
    fn retarget(&mut self, target: Vec<Tile>, now: Instant, reduce: bool) {
        let old = self
            .raw_sample(now)
            .into_iter()
            .map(|t| (t.id, t))
            .collect::<FxHashMap<_, _>>();
        let targets = target
            .iter()
            .map(|t| (t.id, t.rect))
            .collect::<FxHashMap<_, _>>();
        let mut moving = Vec::with_capacity(target.len() + old.len());
        for to in target {
            let from = old.get(&to.id).cloned().unwrap_or_else(|| {
                let mut n = to.clone();
                let parent = old
                    .get(&to.parent)
                    .map(|t| t.rect)
                    .or_else(|| targets.get(&to.parent).copied())
                    .unwrap_or(to.rect);
                n.rect = Rect {
                    x: parent.x + parent.w * 0.5,
                    y: parent.y + parent.h * 0.5,
                    w: 0.,
                    h: 0.,
                };
                n.opacity = 0.;
                n
            });
            moving.push(Moving {
                from: if reduce { to.clone() } else { from },
                to,
            });
        }
        for (id, from) in old {
            if !targets.contains_key(&id) {
                let mut to = from.clone();
                to.opacity = 0.;
                to.rect = Rect {
                    x: from.rect.x + from.rect.w * 0.5,
                    y: from.rect.y + from.rect.h * 0.5,
                    w: 0.,
                    h: 0.,
                };
                if !reduce {
                    moving.push(Moving { from, to });
                }
            }
        }
        self.moving = moving;
        self.start = (!reduce).then_some(now);
    }
    pub fn update(
        &mut self,
        p: Preview,
        size: (u32, u32, usize),
        source: usize,
        now: Instant,
        reduce: bool,
    ) {
        let before = Instant::now();
        if source != self.source {
            self.selected = None;
            self.scope = 0;
            self.moving.clear();
            self.start = None;
        }
        self.parents = p
            .entries
            .iter()
            .filter(|n| n.directory)
            .map(|n| (n.identity, n.parent.map(|i| p.entries[i].identity)))
            .collect();
        self.scope_name = p
            .entries
            .iter()
            .find(|n| n.identity == self.scope && n.directory)
            .map(|n| n.name.clone())
            .unwrap_or_default();
        if self.scope_dirty
            || self.source != source
            || self.size.0 != size.0
            || self.size.1 != size.1
        {
            self.axes.clear();
        }
        let target = layout(
            &p,
            Rect {
                x: 0.,
                y: 0.,
                w: size.0 as f32,
                h: size.1 as f32,
            },
            size.2,
            self.scope,
            &mut self.axes,
        );
        self.scope_dirty = false;
        self.retarget(target, now, reduce);
        self.finalized = false;
        self.revision = p.revision;
        self.source = source;
        self.size = size;
        self.coarsened = p.coarsened;
        self.last = Some(now);
        self.builds += 1;
        self.layout_us = before.elapsed().as_micros();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Builder {
        let mut b = Builder::default();
        b.directory(std::path::Path::new("fixture"));
        for i in 1..13 {
            b.add_path(
                &[(i, format!("dir-{i}")), (100 + i, "src".into())],
                Category::Code,
                100,
                1,
            );
        }
        b
    }
    #[test]
    fn hierarchy_and_measured_weight_survive_byte_budget() {
        let mut b = fixture();
        for i in 1000..12000 {
            b.add_path(&[(i, format!("directory-{i}"))], Category::Media, 13, 1);
        }
        let p = b.snapshot();
        assert!(b.charged <= BUDGET);
        assert!(p.coarsened);
        assert_eq!(p.entries[0].bytes, 1200 + 11000 * 13);
        assert!(p.entries.iter().any(|n| n.parent.is_some_and(|i| i > 0)));
    }
    #[test]
    fn continuous_retarget_throttle_reduce_and_two_dimensional_layout() {
        let mut b = fixture();
        let now = Instant::now();
        let mut m = Motion::default();
        m.update(b.snapshot(), (700, 500, 4), 1, now, false);
        let a = m.sample(now + Duration::from_millis(120));
        b.add_path(&[(1, "dir-1".into())], Category::Media, 9999, 1);
        assert!(!m.due(
            b.revision,
            (700, 500, 4),
            1,
            now + Duration::from_millis(100)
        ));
        let at = now + Duration::from_millis(150);
        let before = m.sample(at);
        m.update(b.snapshot(), (700, 500, 4), 1, at, false);
        let after = m.sample(at);
        for t in before {
            assert_eq!(t.rect, after.iter().find(|x| x.id == t.id).unwrap().rect);
        }
        assert!(a.iter().any(|x| x.depth > 1));
        m.update(b.snapshot(), (430, 370, 4), 1, at, true);
        assert!(!m.running(at));
        let tiles = m.sample(at);
        assert!(tiles.iter().all(|t| t.rect.w.is_finite() && t.rect.h >= 0.));
        assert!(tiles.iter().any(|t| t.rect.y > 50.));
        assert!(tiles.iter().any(|t| t.rect.x > 100.));
    }
    #[test]
    fn progress_layout_is_coalesced_and_scope_hit_handoff_are_stable() {
        let mut b = fixture();
        let now = Instant::now();
        let mut m = Motion::default();
        m.update(b.snapshot(), (640, 440, 4), 9, now, true);
        for step in 1..80 {
            b.add_path(&[(1, "dir-1".into())], Category::Media, step, 1);
            let at = now + Duration::from_millis(step * 10);
            if m.due(b.revision, (640, 440, 4), 9, at) {
                m.update(b.snapshot(), (640, 440, 4), 9, at, false);
            }
        }
        assert!(m.builds <= 5);
        assert!(m.enter(1));
        assert!(m.due(b.revision, (640, 440, 4), 9, now + Duration::from_secs(1)));
        m.update(
            b.snapshot(),
            (640, 440, 4),
            9,
            now + Duration::from_secs(1),
            true,
        );
        assert_eq!(m.scope, 1);
        assert!(m.up());
        m.update(
            b.snapshot(),
            (640, 440, 4),
            9,
            now + Duration::from_secs(2),
            true,
        );
        let tiles = m.sample(now + Duration::from_secs(2));
        let frames = tiles
            .iter()
            .map(|t| {
                (
                    crate::usage_view::TileKey::Other(t.id),
                    t.rect,
                    t.header.then_some(Rect { h: 20., ..t.rect }),
                )
            })
            .collect::<Vec<_>>();
        let leaf = tiles
            .iter()
            .filter(|t| !t.header)
            .max_by_key(|t| t.depth)
            .unwrap();
        let point = (
            leaf.rect.x + leaf.rect.w / 2.,
            leaf.rect.y + leaf.rect.h / 2.,
        );
        assert_eq!(
            crate::usage_view::hit_frame(&frames, point),
            Some(crate::usage_view::TileKey::Other(leaf.id))
        );
        m.selected = Some(1);
        let at = now + Duration::from_secs(3);
        let before = m.sample(at);
        m.handoff(tiles, at, false);
        let after = m.sample(at);
        for t in before {
            assert_eq!(t.rect, after.iter().find(|n| n.id == t.id).unwrap().rect);
        }
        assert_eq!(m.selected, Some(1));
        assert!(m.finalized);
        assert!(!m.can_handoff(9));
    }
}
