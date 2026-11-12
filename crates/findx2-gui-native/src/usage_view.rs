//! Independently implemented bounded squarified scene, invertible camera and transition math.
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
    pub fn zoom(&mut self, factor: f32, p: (f32, f32), viewport: Rect) {
        let base = self.base(p);
        self.scale = (self.scale * factor).clamp(1., 24.);
        self.x = base.0 - p.0 / self.scale;
        self.y = base.1 - p.1 / self.scale;
        self.clamp(viewport);
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
    pub bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SceneKey {
    pub scan: usize,
    pub root: usize,
    pub depth: usize,
    pub width: u32,
    pub height: u32,
}
#[derive(Clone, Debug)]
pub struct Scene {
    pub key: SceneKey,
    pub viewport: Rect,
    pub tiles: Vec<SceneTile>,
}
/// Greedily choose each short-side row by worst aspect ratio; zero weights never produce NaN.
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
pub fn build_scene(scan: &Scan, key: SceneKey) -> Scene {
    let viewport = Rect {
        x: 0.,
        y: 0.,
        w: key.width as f32,
        h: key.height as f32,
    };
    let mut tiles = Vec::with_capacity(1200);
    fn fill(
        scan: &Scan,
        node: usize,
        rect: Rect,
        depth: usize,
        max_depth: usize,
        budget: usize,
        out: &mut Vec<SceneTile>,
    ) {
        if budget == 0 || out.len() >= 1200 || rect.w < 1. || rect.h < 1. {
            return;
        }
        let n = &scan.nodes[node];
        let nested = n.directory
            && !n.children.is_empty()
            && depth < max_depth
            && rect.w >= 36.
            && rect.h >= 38.
            && budget >= 3;
        out.push(SceneTile {
            key: TileKey::Node(node),
            rect,
            depth,
            nested,
            bytes: n.allocated,
        });
        if !nested {
            return;
        }
        let mut content = rect.inset(if depth == 0 { 3. } else { 1. });
        if depth > 0 {
            content.y += 20.;
            content.h = (content.h - 20.).max(0.);
        }
        let limit = 96.min(budget.saturating_sub(2));
        let total = n.allocated.max(1) as f64;
        let weights = n
            .children
            .iter()
            .take(limit)
            .filter(|&&i| {
                scan.nodes[i].allocated as f64 / total * content.w as f64 * content.h as f64 >= 25.
            })
            .map(|&i| (i, scan.nodes[i].allocated))
            .collect::<Vec<_>>();
        let drawn = weights.iter().map(|v| v.1).sum::<u64>();
        let others = n.allocated.saturating_sub(drawn);
        let mut weights = weights;
        if others > 0 {
            weights.push((usize::MAX, others));
        }
        let rectangles = squarify(&weights, content);
        let mut remaining_budget = budget - 1;
        let count = rectangles.len();
        for (position, (index, rect)) in rectangles.into_iter().enumerate() {
            let child_budget = (remaining_budget / (count - position).max(1)).max(1);
            let before = out.len();
            if index == usize::MAX {
                out.push(SceneTile {
                    key: TileKey::Other(node),
                    rect,
                    depth: depth + 1,
                    nested: false,
                    bytes: others,
                });
            } else {
                fill(scan, index, rect, depth + 1, max_depth, child_budget, out);
            }
            remaining_budget = remaining_budget.saturating_sub(out.len() - before);
        }
    }
    if key.root < scan.nodes.len() {
        fill(scan, key.root, viewport, 0, key.depth, 1200, &mut tiles);
    }
    Scene {
        key,
        viewport,
        tiles,
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
        assert!((area - 1194. * 794.).abs() < 2.);
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
