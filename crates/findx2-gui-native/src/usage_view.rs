//! DiskTree-backed bounded scene. Actual upstream tree/treemap/classify and View algorithms; see disktree/README.md.
//! Filesystem access is deliberately absent; scene construction runs on a background worker.
use crate::disk_usage::Scan;
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Rect {
    pub fn inset(self, n: f32) -> Self {
        Self {
            x: self.x + n,
            y: self.y + n,
            w: (self.w - 2. * n).max(0.),
            h: (self.h - 2. * n).max(0.),
        }
    }
    pub fn contains(self, p: (f32, f32)) -> bool {
        p.0 >= self.x && p.1 >= self.y && p.0 <= self.x + self.w && p.1 <= self.y + self.h
    }
    pub fn lerp(self, b: Self, t: f32) -> Self {
        Self {
            x: self.x + (b.x - self.x) * t,
            y: self.y + (b.y - self.y) * t,
            w: self.w + (b.w - self.w) * t,
            h: self.h + (b.h - self.h) * t,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub scale: f32,
    pub x: f32,
    pub y: f32,
}
impl Default for Camera {
    fn default() -> Self {
        Self {
            scale: 1.,
            x: 0.,
            y: 0.,
        }
    }
}
impl Camera {
    pub fn reset(&mut self) {
        *self = Self {
            scale: 1.,
            x: 0.,
            y: 0.,
        };
    }
    pub fn screen(&self, p: (f32, f32)) -> (f32, f32) {
        ((p.0 - self.x) * self.scale, (p.1 - self.y) * self.scale)
    }
    pub fn base(&self, p: (f32, f32)) -> (f32, f32) {
        (self.x + p.0 / self.scale, self.y + p.1 / self.scale)
    }
    pub fn as_view(self) -> crate::disktree::viewport::View {
        crate::disktree::viewport::View {
            scale: self.scale,
            origin_x: self.x,
            origin_y: self.y,
        }
    }
    pub fn zoom(&mut self, factor: f32, p: (f32, f32), viewport: Rect) {
        let v = self
            .as_view()
            .zoomed_at(p.0, p.1, factor, crate::disktree::viewport::View::MAX_SCALE)
            .clamped(viewport.into());
        self.scale = v.scale;
        self.x = v.origin_x;
        self.y = v.origin_y;
    }
    pub fn pan(&mut self, dx: f32, dy: f32, viewport: Rect) {
        self.x -= dx / self.scale;
        self.y -= dy / self.scale;
        self.clamp(viewport);
    }
    fn clamp(&mut self, v: Rect) {
        self.x = self.x.clamp(0., (v.w - v.w / self.scale).max(0.));
        self.y = self.y.clamp(0., (v.h - v.h / self.scale).max(0.));
    }
    pub fn rect(&self, r: Rect) -> Rect {
        let (x, y) = self.screen((r.x, r.y));
        Rect {
            x,
            y,
            w: r.w * self.scale,
            h: r.h * self.scale,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TileKey {
    Node(usize),
    Other(usize),
}
#[derive(Clone, Debug)]
pub struct SceneTile {
    pub key: TileKey,
    pub rect: Rect,
    pub depth: usize,
    pub nested: bool,
    pub header: Option<Rect>,
    pub category: usize,
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneKey {
    pub scan: usize,
    pub root: usize,
    pub depth: usize,
    pub width: u32,
    pub height: u32,
    /// Screen-detail tier. Zoom exposes previously omitted children without a rescan.
    pub detail: u32,
}
#[derive(Clone, Debug)]
pub struct Scene {
    pub key: SceneKey,
    pub viewport: Rect,
    pub tiles: Vec<SceneTile>,
    pub layout_elapsed: std::time::Duration,
}
/// Greedily choose each short-side row by worst aspect ratio; zero weights never produce NaN.
#[cfg(test)]
pub fn squarify(weights: &[(usize, u64)], rect: Rect) -> Vec<(usize, Rect)> {
    let mut weights = weights
        .iter()
        .copied()
        .filter(|(_, w)| *w > 0)
        .collect::<Vec<_>>();
    weights.sort_by(|a, b| b.1.cmp(&a.1));
    let total = weights.iter().map(|(_, w)| *w as f64).sum::<f64>();
    if total <= 0. || rect.w <= 0. || rect.h <= 0. {
        return vec![];
    }
    let areas = weights
        .iter()
        .map(|(i, w)| (*i, *w as f64 / total * rect.w as f64 * rect.h as f64))
        .collect::<Vec<_>>();
    fn worst(row: &[(usize, f64)], side: f64) -> f64 {
        let sum = row.iter().map(|v| v.1).sum::<f64>();
        let min = row.iter().map(|v| v.1).fold(f64::INFINITY, f64::min);
        let max = row.iter().map(|v| v.1).fold(0., f64::max);
        if sum == 0. || side == 0. || min == 0. {
            return f64::INFINITY;
        }
        ((side * side * max) / (sum * sum)).max((sum * sum) / (side * side * min))
    }
    let mut remaining = rect;
    let mut at = 0;
    let mut out = Vec::with_capacity(areas.len());
    while at < areas.len() {
        let side = remaining.w.min(remaining.h) as f64;
        let mut end = at + 1;
        while end < areas.len() && worst(&areas[at..=end], side) <= worst(&areas[at..end], side) {
            end += 1;
        }
        let sum = areas[at..end].iter().map(|v| v.1).sum::<f64>() as f32;
        if remaining.w >= remaining.h {
            let width = (sum / remaining.h.max(f32::EPSILON)).min(remaining.w);
            let mut y = remaining.y;
            for &(index, area) in &areas[at..end] {
                let h = (area as f32 / width.max(f32::EPSILON))
                    .min((remaining.y + remaining.h - y).max(0.));
                out.push((
                    index,
                    Rect {
                        x: remaining.x,
                        y,
                        w: width,
                        h,
                    },
                ));
                y += h;
            }
            remaining.x += width;
            remaining.w = (remaining.w - width).max(0.);
        } else {
            let height = (sum / remaining.w.max(f32::EPSILON)).min(remaining.h);
            let mut x = remaining.x;
            for &(index, area) in &areas[at..end] {
                let w = (area as f32 / height.max(f32::EPSILON))
                    .min((remaining.x + remaining.w - x).max(0.));
                out.push((
                    index,
                    Rect {
                        x,
                        y: remaining.y,
                        w,
                        h: height,
                    },
                ));
                x += w;
            }
            remaining.y += height;
            remaining.h = (remaining.h - height).max(0.);
        }
        at = end;
    }
    out
}
impl From<Rect> for crate::disktree::treemap::Rect {
    fn from(r: Rect) -> Self {
        Self::new(r.x, r.y, r.w, r.h)
    }
}
impl From<crate::disktree::treemap::Rect> for Rect {
    fn from(r: crate::disktree::treemap::Rect) -> Self {
        Self {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }
    }
}
/// Build a bounded *adapter*, not a second filesystem tree. Values remain exact measured
/// allocation totals; omitted children become a real weighted tail. No file IO here.
pub fn build_scene(scan: &Scan, key: SceneKey) -> Scene {
    use crate::disktree::{
        classify,
        tree::{Metric, Node as TreeNode, NodeKind as Kind},
        treemap::{self, LayoutOptions, TileKind},
    };
    use std::collections::HashMap;
    let started = std::time::Instant::now();
    let viewport = Rect {
        x: 0.,
        y: 0.,
        w: key.width as f32,
        h: key.height as f32,
    };
    let mut address = HashMap::<Vec<usize>, (TileKey, usize)>::new();
    let mut budget = 16_384usize;
    fn adapt(
        scan: &Scan,
        id: usize,
        crumbs: &mut Vec<usize>,
        address: &mut HashMap<Vec<usize>, (TileKey, usize)>,
        budget: &mut usize,
        depth: usize,
    ) -> TreeNode {
        let n = &scan.nodes[id];
        *budget = budget.saturating_sub(1);
        let mut node = TreeNode::entry(
            n.path
                .file_name()
                .unwrap_or(n.path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            if n.directory {
                Kind::Directory
            } else {
                Kind::File
            },
            n.allocated,
        );
        node.files = n.files;
        node.dirs = n.directories.min(u32::MAX as u64) as u32;
        node.read_error = n.uncertain;
        address.insert(crumbs.clone(), (TileKey::Node(id), n.category));
        if n.directory && depth < 7 {
            let mut represented = 0u64;
            for &child in n.children.iter().take(192) {
                if *budget < 2 {
                    break;
                }
                crumbs.push(node.children.len());
                node.children
                    .push(adapt(scan, child, crumbs, address, budget, depth + 1));
                crumbs.pop();
                represented = represented.saturating_add(scan.nodes[child].allocated);
            }
            let tail = n.allocated.saturating_sub(represented);
            if tail > 0 && !node.children.is_empty() {
                crumbs.push(node.children.len());
                address.insert(crumbs.clone(), (TileKey::Other(id), n.category));
                crumbs.pop();
                node.children
                    .push(TreeNode::entry("其余已测占用", Kind::File, tail));
            }
        }
        node
    }
    if key.root >= scan.nodes.len() {
        return Scene {
            key,
            viewport,
            tiles: vec![],
            layout_elapsed: started.elapsed(),
        };
    }
    let mut tree = adapt(
        scan,
        key.root,
        &mut Vec::new(),
        &mut address,
        &mut budget,
        0,
    );
    classify::classify(&mut tree);
    let detail = key.detail.max(1) as f32;
    let options = LayoutOptions {
        max_depth: key.depth.clamp(1, 6) as u32,
        max_children: 96,
        ..Default::default()
    };
    let layouts = treemap::layout(
        &tree,
        &[],
        treemap::Rect::new(0., 0., viewport.w * detail, viewport.h * detail),
        Metric::Bytes,
        &options,
    );
    let mut tiles = Vec::with_capacity(layouts.len().min(1200) + 1);
    tiles.push(SceneTile {
        key: TileKey::Node(key.root),
        rect: viewport,
        depth: 0,
        nested: true,
        bytes: scan.nodes[key.root].allocated,
        header: None,
        category: scan.nodes[key.root].category,
    });
    for tile in layouts {
        let (tile_key, fallback) = match &tile.kind {
            TileKind::Node { crumbs } => address
                .get(crumbs)
                .copied()
                .unwrap_or((TileKey::Other(key.root), 5)),
            TileKind::Others { crumbs, .. } => {
                let parent = match address.get(crumbs) {
                    Some((TileKey::Node(i), _)) => *i,
                    _ => key.root,
                };
                (TileKey::Other(parent), scan.nodes[parent].category)
            }
        };
        let node = tree.resolve(tile.crumbs());
        let category = match node.map(|n| n.category) {
            Some(classify::Category::Documents) => 1,
            Some(classify::Category::Media) => 2,
            Some(classify::Category::Code | classify::Category::Git) => 3,
            Some(
                classify::Category::Toolchain
                | classify::Category::Cache
                | classify::Category::AgentScratch,
            ) => 5,
            _ => fallback,
        };
        let bytes = match &tile.kind {
            TileKind::Others { count, .. } => {
                let mut weights = node
                    .map(|n| {
                        n.children
                            .iter()
                            .filter(|c| c.bytes > 0)
                            .map(|c| c.bytes)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                weights.sort_unstable_by(|a, b| b.cmp(a));
                weights
                    .iter()
                    .skip(weights.len().saturating_sub(*count))
                    .copied()
                    .fold(0u64, u64::saturating_add)
            }
            _ => match tile_key {
                TileKey::Node(i) => scan.nodes[i].allocated,
                TileKey::Other(_) => node.map_or(0, |n| n.bytes),
            },
        };
        let tile_key = match tile_key {
            TileKey::Other(_) => TileKey::Other(tiles.len()),
            key => key,
        };
        let scale = |r: treemap::Rect| Rect::from(r.scaled(1. / detail));
        tiles.push(SceneTile {
            key: tile_key,
            rect: scale(tile.rect),
            header: tile.header.map(scale),
            depth: tile.depth as usize + 1,
            nested: tile.header.is_some(),
            bytes,
            category,
        });
    }
    // Preserve all top-level siblings before spending a finite draw budget on detail.
    tiles.sort_by_key(|t| t.depth);
    tiles.truncate(1200);
    Scene {
        key,
        viewport,
        tiles,
        layout_elapsed: started.elapsed(),
    }
}
pub fn ease_out(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(3)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn squarified_area_is_exact_and_bounded() {
        let r = Rect {
            x: 3.,
            y: 7.,
            w: 900.,
            h: 400.,
        };
        for (i, t) in squarify(&[(0, 10), (1, 30), (2, 60)], r) {
            assert!((t.w * t.h / (r.w * r.h) - [0.1, 0.3, 0.6][i]).abs() < 0.0001);
            assert!(
                t.x >= r.x
                    && t.y >= r.y
                    && t.x + t.w <= r.x + r.w + 0.01
                    && t.y + t.h <= r.y + r.h + 0.01
            );
        }
    }
    #[test]
    fn pointer_zoom_and_hit_transform_share_coordinates() {
        let mut c = Camera::default();
        c.reset();
        let v = Rect {
            w: 1000.,
            h: 600.,
            ..Default::default()
        };
        let p = (450., 230.);
        let before = c.base(p);
        c.zoom(1.15, p, v);
        let after = c.base(p);
        assert!((before.0 - after.0).abs() < 0.001 && (before.1 - after.1).abs() < 0.001);
        assert!(
            c.rect(Rect {
                x: 400.,
                y: 200.,
                w: 100.,
                h: 80.
            })
            .contains(p)
        );
    }
    #[test]
    fn pan_clamps_and_reset_is_exact() {
        let mut c = Camera::default();
        c.reset();
        let v = Rect {
            w: 1000.,
            h: 600.,
            ..Default::default()
        };
        c.zoom(4., (500., 300.), v);
        c.pan(-100000., -100000., v);
        assert_eq!(c.x, 750.);
        assert_eq!(c.y, 450.);
        c.reset();
        assert_eq!(c.base((75., 80.)), (75., 80.));
    }
    #[test]
    fn zero_and_extreme_weights_remain_finite() {
        assert!(
            squarify(
                &[(0, 0)],
                Rect {
                    w: 4.,
                    h: 4.,
                    ..Default::default()
                }
            )
            .is_empty()
        );
        for (_, r) in squarify(
            &[(0, u64::MAX), (1, 1)],
            Rect {
                w: 800.,
                h: 300.,
                ..Default::default()
            },
        ) {
            assert!(r.x.is_finite() && r.y.is_finite() && r.w.is_finite() && r.h.is_finite());
        }
    }
    #[test]
    fn large_scene_retains_all_root_siblings_with_hard_budget() {
        use crate::disk_usage::{Limits, Node, Scan};
        use std::{path::PathBuf, time::Duration};
        fn node(path: String, parent: Option<usize>, directory: bool, allocated: u64) -> Node {
            Node {
                path: PathBuf::from(path),
                parent,
                children: vec![],
                entries: 1,
                files: u64::from(!directory),
                directories: u64::from(directory),
                category: 0,
                own_allocated: 0,
                directory,
                logical: allocated,
                mtime: 0,
                allocated,
                hardlink_duplicate: false,
                uncertain: false,
            }
        }
        let mut nodes = vec![node("root".into(), None, true, 96 * 96)];
        let mut roots = vec![];
        for d in 0..96 {
            let parent = nodes.len();
            roots.push(parent);
            nodes[0].children.push(parent);
            nodes.push(node(format!("d{d}"), Some(0), true, 96));
            for f in 0..96 {
                let i = nodes.len();
                nodes[parent].children.push(i);
                nodes.push(node(format!("d{d}/f{f}"), Some(parent), false, 1));
            }
        }
        let scan = Scan {
            nodes,
            errors: 0,
            skipped_links: 0,
            hardlinks: 0,
            unknown: 0,
            partial: false,
            cancelled: false,
            elapsed: Duration::ZERO,
            entry_limit_reached: false,
            time_limit_reached: false,
            limits: Limits::default(),
            volume: None,
        };
        let key = SceneKey {
            scan: 1,
            root: 0,
            depth: 6,
            width: 1200,
            height: 800,
            detail: 1,
        };
        let started = std::time::Instant::now();
        let scene = build_scene(&scan, key);
        assert!(scene.tiles.len() <= 1200);
        for root in roots {
            assert!(scene.tiles.iter().any(|t| t.key == TileKey::Node(root)));
        }
        let area = scene
            .tiles
            .iter()
            .filter(|t| t.depth == 1)
            .map(|t| t.rect.w * t.rect.h)
            .sum::<f32>();
        assert!(area > 0. && area <= 1200. * 800.); // DiskTree intentionally reserves sibling gaps and directory headers.
        println!(
            "bounded scene: {} input nodes, {} tiles, {:?}",
            scan.nodes.len(),
            scene.tiles.len(),
            started.elapsed()
        );
    }
    #[test]
    fn transition_easing_is_monotonic_and_bounded() {
        let mut last = 0.;
        for i in 0..=100 {
            let v = ease_out(i as f32 / 100.);
            assert!(v >= last && v <= 1.);
            last = v;
        }
        assert_eq!(ease_out(1.), 1.);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Activation {
    Select,
    Drill,
    Preview,
}
pub(crate) fn activation(enter: bool, directory: bool) -> Activation {
    if !enter {
        Activation::Select
    } else if directory {
        Activation::Drill
    } else {
        Activation::Preview
    }
}
#[cfg(test)]
mod activation_tests {
    use super::*;
    #[test]
    fn click_only_selects_files_and_directories() {
        assert_eq!(activation(false, false), Activation::Select);
        assert_eq!(activation(false, true), Activation::Select);
    }
    #[test]
    fn enter_and_double_click_share_drill_or_preview_never_execute() {
        assert_eq!(activation(true, true), Activation::Drill);
        assert_eq!(activation(true, false), Activation::Preview);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticAction {
    Enter(usize),
    Up,
}
#[derive(Default)]
pub struct WheelGate {
    armed: Option<(SemanticAction, std::time::Instant, f32)>,
    last_switch: Option<std::time::Instant>,
}
impl WheelGate {
    /// A boundary needs a separate deliberate gesture; trackpad tails cannot drill
    /// through several levels. Horizontal/modifier events do not arm this gate.
    pub fn evaluate(
        &mut self,
        now: std::time::Instant,
        steps: f32,
        boundary: Option<SemanticAction>,
    ) -> Option<SemanticAction> {
        if self.last_switch.is_some_and(|t| {
            now.saturating_duration_since(t) < std::time::Duration::from_millis(450)
        }) {
            self.armed = None;
            return None;
        }
        let Some(action) = boundary.filter(|_| steps.abs() >= 0.08) else {
            self.armed = None;
            return None;
        };
        match self.armed {
            Some((previous, started, total))
                if previous == action
                    && now.saturating_duration_since(started)
                        < std::time::Duration::from_millis(1400) =>
            {
                let total = total + steps.abs();
                if now.saturating_duration_since(started) >= std::time::Duration::from_millis(170)
                    && total >= 1.3
                {
                    self.armed = None;
                    self.last_switch = Some(now);
                    Some(action)
                } else {
                    self.armed = Some((action, started, total));
                    None
                }
            }
            _ => {
                self.armed = Some((action, now, steps.abs()));
                None
            }
        }
    }
    pub fn reset(&mut self) {
        self.armed = None;
    }
}
/// GPUI Windows supplies wheel lines; 3 default lines == 90 logical pixels.
/// Keep both axes: horizontal-only gestures are pan, never semantic navigation.
pub fn normalized_wheel(dx: f32, dy: f32, lines: bool) -> (f32, f32) {
    let k = if lines { 30. } else { 1. };
    (dx * k, dy * k)
}
/// Renderer-only timings, no filenames. Max/mean are observations, not guarantees.
static PAINT_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PAINT_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static PAINT_MAX_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn record_paint(duration: std::time::Duration) {
    use std::sync::atomic::Ordering;
    let ns = duration.as_nanos().min(u64::MAX as u128) as u64;
    PAINT_NS.fetch_add(ns, Ordering::Relaxed);
    PAINT_COUNT.fetch_add(1, Ordering::Relaxed);
    PAINT_MAX_NS.fetch_max(ns, Ordering::Relaxed);
}
pub fn paint_metrics() -> (u64, u64, u64) {
    use std::sync::atomic::Ordering;
    (
        PAINT_COUNT.load(Ordering::Relaxed),
        PAINT_NS.load(Ordering::Relaxed),
        PAINT_MAX_NS.load(Ordering::Relaxed),
    )
}

pub fn hit_frame(frame: &[(TileKey, Rect, Option<Rect>)], p: (f32, f32)) -> Option<TileKey> {
    frame
        .iter()
        .rev()
        .find(|(_, r, _)| r.contains(p))
        .map(|(k, _, _)| *k)
}
#[cfg(test)]
mod viewport_integration_tests {
    use super::*;
    #[test]
    fn body_header_other_gap_share_the_painted_frame() {
        let dir = Rect {
            x: 4.,
            y: 4.,
            w: 100.,
            h: 100.,
        };
        let header = Rect { h: 20., ..dir };
        let file = Rect {
            x: 8.,
            y: 28.,
            w: 40.,
            h: 60.,
        };
        let other = Rect {
            x: 52.,
            y: 28.,
            w: 48.,
            h: 60.,
        };
        let frame = vec![
            (TileKey::Node(1), dir, Some(header)),
            (TileKey::Node(2), file, None),
            (TileKey::Other(3), other, None),
        ];
        assert_eq!(hit_frame(&frame, (20., 12.)), Some(TileKey::Node(1)));
        assert_eq!(hit_frame(&frame, (20., 40.)), Some(TileKey::Node(2)));
        assert_eq!(hit_frame(&frame, (70., 40.)), Some(TileKey::Other(3)));
        assert_eq!(hit_frame(&frame, (0., 0.)), None);
        assert_eq!(hit_frame(&frame, (50., 40.)), Some(TileKey::Node(1))); // parent's own interior gap
        let mut camera = Camera::default();
        camera.zoom(
            2.,
            (50., 50.),
            Rect {
                x: 0.,
                y: 0.,
                w: 300.,
                h: 300.,
            },
        );
        let projected = frame
            .iter()
            .map(|(k, r, h)| (*k, camera.rect(*r), h.map(|r| camera.rect(r))))
            .collect::<Vec<_>>();
        let screen = camera.screen((20., 40.));
        assert_eq!(hit_frame(&projected, screen), Some(TileKey::Node(2)));
    }
    #[test]
    fn wheel_lines_pixels_and_horizontal_axes_are_explicit() {
        assert_eq!(
            normalized_wheel(0., 3., true),
            normalized_wheel(0., 90., false)
        );
        assert_eq!(normalized_wheel(2., 0., true), (60., 0.));
    }
    #[test]
    fn semantic_edges_require_deliberate_second_gesture_and_cooldown() {
        let mut gate = WheelGate::default();
        let now = std::time::Instant::now();
        let enter = Some(SemanticAction::Enter(7));
        assert_eq!(gate.evaluate(now, 1., enter), None);
        assert_eq!(
            gate.evaluate(now + std::time::Duration::from_millis(80), 0.1, enter),
            None
        );
        assert_eq!(
            gate.evaluate(now + std::time::Duration::from_millis(200), 1., enter),
            enter
        );
        assert_eq!(
            gate.evaluate(now + std::time::Duration::from_millis(210), 1., enter),
            None
        );
        assert_eq!(
            gate.evaluate(
                now + std::time::Duration::from_millis(700),
                -1.,
                Some(SemanticAction::Up)
            ),
            None
        );
        assert_eq!(
            gate.evaluate(
                now + std::time::Duration::from_millis(900),
                -1.,
                Some(SemanticAction::Up)
            ),
            Some(SemanticAction::Up)
        );
        gate.reset();
        assert_eq!(
            gate.evaluate(now + std::time::Duration::from_secs(2), 0., None),
            None
        );
    }
    #[test]
    fn actual_disktree_transition_maps_the_focal_region_continuously() {
        use crate::disktree::{treemap::Rect as R, viewport::LayoutTransition};
        let src = R::new(100., 60., 200., 120.);
        let dst = R::new(0., 0., 1000., 600.);
        let t = LayoutTransition::new(src, dst);
        let initial = t.origin_of(dst);
        assert_eq!(initial, src);
        let (sample, running) = t.sample(dst);
        assert!(running && sample.w >= src.w && sample.w <= dst.w);
        let child = R::new(0., 0., 500., 300.);
        let origin = t.origin_of(child);
        assert!((origin.w - 100.).abs() < 0.001);
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WheelIntent {
    Ignore,
    Pan { dx: f32, dy: f32 },
    Zoom { steps: f32 },
}
pub fn wheel_intent(
    dx: f32,
    dy: f32,
    shift: bool,
    control: bool,
    alt: bool,
    platform: bool,
) -> WheelIntent {
    if !dx.is_finite() || !dy.is_finite() || alt || platform {
        return WheelIntent::Ignore;
    }
    if shift {
        WheelIntent::Pan {
            dx: if dx.abs() > 0.01 { dx } else { dy },
            dy: 0.,
        }
    } else if control || dy.abs() < 0.01 {
        WheelIntent::Pan { dx, dy }
    } else {
        WheelIntent::Zoom {
            steps: (dy / 90.).clamp(-2., 2.),
        }
    }
}
#[cfg(test)]
mod wheel_modifier_tests {
    use super::*;
    #[test]
    fn production_modifier_routing_never_arms_semantic_navigation_for_pan() {
        assert_eq!(
            wheel_intent(0., 90., false, false, false, false),
            WheelIntent::Zoom { steps: 1. }
        );
        assert_eq!(
            wheel_intent(0., 90., true, false, false, false),
            WheelIntent::Pan { dx: 90., dy: 0. }
        );
        assert_eq!(
            wheel_intent(60., 0., true, false, false, false),
            WheelIntent::Pan { dx: 60., dy: 0. }
        );
        assert_eq!(
            wheel_intent(60., 0., false, false, false, false),
            WheelIntent::Pan { dx: 60., dy: 0. }
        );
        assert_eq!(
            wheel_intent(30., 90., false, true, false, false),
            WheelIntent::Pan { dx: 30., dy: 90. }
        );
        assert_eq!(
            wheel_intent(0., 90., false, false, true, false),
            WheelIntent::Ignore
        );
        assert_eq!(
            wheel_intent(0., 90., false, false, false, true),
            WheelIntent::Ignore
        );
        assert_eq!(
            wheel_intent(f32::NAN, 90., false, false, false, false),
            WheelIntent::Ignore
        );
    }
}

#[cfg(test)]
mod resize_regression_tests {
    use super::*;
    #[test]
    fn resize_clamps_camera_without_resetting_zoom() {
        let mut camera = Camera::default();
        let old = Rect {
            x: 0.,
            y: 0.,
            w: 1000.,
            h: 600.,
        };
        camera.zoom(4., (500., 300.), old);
        camera.pan(-10000., -10000., old);
        camera.pan(
            0.,
            0.,
            Rect {
                x: 0.,
                y: 0.,
                w: 500.,
                h: 300.,
            },
        );
        assert_eq!((camera.scale, camera.x, camera.y), (4., 375., 225.));
    }
}
