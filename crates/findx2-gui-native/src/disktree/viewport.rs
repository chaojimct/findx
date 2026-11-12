// Adapted from tobi/disktree 9b18b272ca3c5a145f6ff58eaf89dc4d311eeaea.
// Copyright (c) 2026 Tobi Lütke. MIT; see LICENSE.disktree and README.md.
use super::treemap::Rect;
use std::time::{Instant,Duration};
/// The treemap view transform: `screen = (base - origin) * scale`.
///
/// All viewport geometry stays in the base space of an unzoomed layout, so pan
/// and zoom never require a re-layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub scale: f32,
    pub origin_x: f32,
    pub origin_y: f32,
}

impl Default for View {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl View {
    pub const IDENTITY: Self = Self {
        scale: 1.0,
        origin_x: 0.0,
        origin_y: 0.0,
    };
    pub const MIN_SCALE: f32 = 1.0;
    /// Past this, zooming again descends into whatever is under the cursor
    /// instead of magnifying further: the continuous "keep zooming and you are
    /// inside" gesture, with a breadcrumb to walk back out.
    pub const MAX_SCALE: f32 = 5.0;

    pub fn project(&self, rect: Rect) -> Rect {
        Rect::new(
            (rect.x - self.origin_x) * self.scale,
            (rect.y - self.origin_y) * self.scale,
            rect.w * self.scale,
            rect.h * self.scale,
        )
    }

    /// Viewport point to base-space point.
    pub fn unproject(&self, x: f32, y: f32) -> (f32, f32) {
        (
            x / self.scale + self.origin_x,
            y / self.scale + self.origin_y,
        )
    }

    /// Zoom by `factor` toward `(x, y)` and no further than `ceiling`.
    ///
    /// The base point under `(x, y)` stays put, which is what makes a wheel
    /// zoom feel pointed at the tile rather than at the window.
    pub fn zoomed_at(&self, x: f32, y: f32, factor: f32, ceiling: f32) -> Self {
        let scale = (self.scale * factor).clamp(Self::MIN_SCALE, ceiling);
        let (base_x, base_y) = self.unproject(x, y);
        Self {
            scale,
            origin_x: base_x - x / scale,
            origin_y: base_y - y / scale,
        }
    }

    /// The scale at which `rect` exactly fits the viewport.
    pub fn fit_scale(rect: Rect, area: Rect) -> f32 {
        let width = area.w.max(1.0);
        let height = area.h.max(1.0);
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return Self::MIN_SCALE;
        }
        (width / rect.w).min(height / rect.h)
    }

    /// The region of the base layout the viewport currently shows.
    pub fn visible_base(&self, area: Rect) -> Rect {
        let width = area.w.max(1.0);
        let height = area.h.max(1.0);
        Rect::new(
            self.origin_x,
            self.origin_y,
            width / self.scale,
            height / self.scale,
        )
    }

    /// Keep the viewport inside the layout: no empty margins, ever.
    pub fn clamped(self, area: Rect) -> Self {
        let width = area.w;
        let height = area.h;
        let max_x = (width - width / self.scale).max(0.0);
        let max_y = (height - height / self.scale).max(0.0);
        Self {
            scale: self.scale,
            origin_x: self.origin_x.clamp(0.0, max_x),
            origin_y: self.origin_y.clamp(0.0, max_y),
        }
    }
}

/// A layout transition: the region the user is looking at, and where that same
/// region lands in the layout being switched to.
///
/// Every descending or ascending move is one region of the tree changing
/// address. Rather than moving a camera, each tile is drawn from where that
/// region *was* to where it *is*, so the tiles inside the directory that is
/// being entered grow into place and the ones being left slide out. That is
/// what makes "zoom in and descend" read as one motion: the newly visible
/// level is always larger than it was a moment ago, never smaller.
#[derive(Clone, Copy, Debug)]
pub struct LayoutTransition {
    /// The region in the old frame, in viewport coordinates.
    src: Rect,
    /// The same region in the new frame, in viewport coordinates.
    dst: Rect,
    started: Instant,
    duration: Duration,
}

impl LayoutTransition {
    pub fn new(src: Rect, dst: Rect) -> Self {
        // Longer pulls take a little longer, but never enough to feel slow.
        let ratio = if dst.w > 1.0 { src.w / dst.w } else { 1.0 };
        let magnitude = ratio.abs().max(1.0).log2().clamp(0.0, 4.0);
        Self {
            src,
            dst,
            started: Instant::now(),
            duration: Duration::from_millis(130 + (magnitude * 45.0) as u64),
        }
    }

    /// Where a rectangle in the new layout was, before the switch.
    pub fn origin_of(&self, rect: Rect) -> Rect {
        let scale = if self.dst.w > 0.0 {
            self.src.w / self.dst.w
        } else {
            1.0
        };
        Rect::new(
            (rect.x - self.dst.x).mul_add(scale, self.src.x),
            (rect.y - self.dst.y).mul_add(scale, self.src.y),
            rect.w * scale,
            rect.h * scale,
        )
    }

    /// The rectangle to draw at this instant, and whether the transition is
    /// still running.
    pub fn sample(&self, rect: Rect) -> (Rect, bool) {
        let elapsed =
            self.started.elapsed().as_secs_f32() / self.duration.as_secs_f32();
        if elapsed >= 1.0 {
            return (rect, false);
        }
        let eased = 1.0 - (1.0 - elapsed).powi(3);
        let from = self.origin_of(rect);
        let lerp = |a: f32, b: f32| (b - a).mul_add(eased, a);
        (
            Rect::new(
                lerp(from.x, rect.x),
                lerp(from.y, rect.y),
                lerp(from.w, rect.w),
                lerp(from.h, rect.h),
            ),
            true,
        )
    }
}
