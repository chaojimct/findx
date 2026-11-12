//! One Canvas/viewport, actual DiskTree layout/View/LayoutTransition source port.
//! Paint and every input use the exact same sampled frame rectangles. No tile div
//! or descendant occluding hitbox can swallow parent wheel events.
//! Paint strategy adapted from tobi/disktree@9b18b27 app/treemap_view.rs;
//! Copyright (c) 2026 Tobi Lütke. MIT: ../disktree/LICENSE.disktree.
use super::*;
use crate::disktree::viewport::{LayoutTransition, View};
use crate::usage_view::{self, Rect, SceneKey, SemanticAction, TileKey};
use gpui_kit::component::menu::ContextMenuExt as _;
use std::time::Instant;
pub(super) struct Transition {
    layout: LayoutTransition,
}
impl Transition {
    fn sample(&self, to: Rect) -> Rect {
        self.layout.sample(to.into()).0.into()
    }
    fn running(&self) -> bool {
        self.layout
            .sample(crate::disktree::treemap::Rect::new(0., 0., 1., 1.))
            .1
    }
}
#[derive(Clone)]
struct PaintTile {
    key: TileKey,
    rect: Rect,
    header: Option<Rect>,
    depth: usize,
    color: Hsla,
    opacity: f32,
    name: String,
    size: String,
    selected: bool,
    hovered: bool,
    unknown: bool,
    class: crate::disktree::classify::Category,
}
fn tile_body(rect: Rect, header: Option<Rect>) -> Rect {
    if let Some(h) = header {
        let top = (h.y + h.h).max(rect.y);
        Rect {
            y: top,
            h: (rect.y + rect.h - top).max(0.),
            ..rect
        }
    } else {
        rect
    }
}
impl MainWindow {
    pub(in super::super) fn ensure_usage_scene(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.mode != ViewMode::Usage {
            return;
        }
        if let Some(t) = &self.workspace.usage_transition {
            if t.running() {
                window.request_animation_frame();
            } else {
                self.workspace.usage_transition = None;
            }
        }
        if self.workspace.scanning {
            let b = self.workspace.usage_bounds.get();
            let size = (
                f32::from(b.size.width).max(0.) as u32,
                f32::from(b.size.height).max(0.) as u32,
                self.workspace.usage_depth,
            );
            let now = Instant::now();
            let source = Arc::as_ptr(&self.workspace.progress) as usize;
            let mut motion = self.workspace.usage_preview.borrow_mut();
            let snapshot = {
                let builder = crate::disktree::lock(&self.workspace.progress.preview);
                (size.0 > 1 && size.1 > 1 && motion.due(builder.revision, size, source, now))
                    .then(|| builder.snapshot())
            };
            if let Some(snapshot) = snapshot {
                motion.update(
                    snapshot,
                    size,
                    source,
                    now,
                    self.session.read(cx).prefs.usage_reduce_motion,
                );
                if std::env::var_os("FINDX_NATIVE_USAGE_TRACE").is_some() {
                    let (paints, ns, max) = usage_view::paint_metrics();
                    eprintln!(
                        "usage_preview revision={} builds={} layout_us={} paint_count={} paint_mean_us={} paint_max_us={}",
                        motion.revision,
                        motion.builds,
                        motion.layout_us,
                        paints,
                        ns / paints.max(1) / 1000,
                        max / 1000
                    );
                }
            }
            if motion.running(now) {
                window.request_animation_frame();
            }
            return;
        }
        if self
            .workspace
            .usage_preview
            .borrow()
            .running(Instant::now())
        {
            window.request_animation_frame();
        }
        let Some(scan) = self.workspace.scan.clone() else {
            return;
        };
        let Some(root) = self.scan_node() else {
            self.workspace.usage_scene_generation =
                self.workspace.usage_scene_generation.wrapping_add(1);
            self.workspace.usage_scene_key = None;
            self.workspace.usage_scene = None;
            self.workspace.usage_frame.borrow_mut().clear();
            self.workspace.usage_hover = None;
            return;
        };
        let b = self.workspace.usage_bounds.get();
        let width = f32::from(b.size.width).round() as u32;
        let height = f32::from(b.size.height).round() as u32;
        if width < 2 || height < 2 {
            return;
        }
        let root_change = self.workspace.usage_scene.as_ref().is_some_and(|s| {
            s.key.root != root
                || (!self.workspace.engine_usage && s.key.scan != Arc::as_ptr(&scan) as usize)
        });
        let detail = if root_change {
            1
        } else {
            self.workspace.usage_camera.scale.floor().clamp(1., 5.) as u32
        };
        let key = SceneKey {
            scan: Arc::as_ptr(&scan) as usize,
            root,
            depth: self.workspace.usage_depth,
            width,
            height,
            detail,
        };
        if self.workspace.usage_scene_key.as_ref() == Some(&key) {
            return;
        }
        self.workspace.usage_scene_key = Some(key.clone());
        self.workspace.usage_scene_generation =
            self.workspace.usage_scene_generation.wrapping_add(1);
        let generation = self.workspace.usage_scene_generation;
        let handoff_scan = scan.clone();
        cx.spawn(async move |this, cx| {
            let scene = cx
                .background_spawn(async move { usage_view::build_scene(&scan, key) })
                .await;
            this.update(cx, |this, cx| {
                if this.workspace.usage_scene_generation != generation {
                    return;
                }
                let source = Arc::as_ptr(&this.workspace.progress) as usize;
                let handing = !this.workspace.engine_usage
                    && handoff_scan.logical_generation.is_none()
                    && this.workspace.usage_preview.borrow().can_handoff(source);
                if let Some(previous) = this.workspace.usage_scene.as_ref().filter(|_| !handing) {
                    if previous.key.root != scene.key.root
                        || (!this.workspace.engine_usage && previous.key.scan != scene.key.scan)
                    {
                        this.workspace.usage_transition = None;
                        if previous.key.scan == scene.key.scan
                            && !this.session.read(cx).prefs.usage_reduce_motion
                        {
                            let camera = this.workspace.usage_camera;
                            let regions = if let Some(target) = previous
                                .tiles
                                .iter()
                                .find(|t| t.key == TileKey::Node(scene.key.root))
                            {
                                Some((
                                    camera.rect(tile_body(target.rect, target.header)),
                                    scene.viewport,
                                ))
                            } else if let Some(target) = scene
                                .tiles
                                .iter()
                                .find(|t| t.key == TileKey::Node(previous.key.root))
                            {
                                Some((previous.viewport, tile_body(target.rect, target.header)))
                            } else {
                                None
                            };
                            if let Some((src, dst)) = regions {
                                if src.w > 0. && dst.w > 0. {
                                    this.workspace.usage_transition = Some(Transition {
                                        layout: LayoutTransition::new(src.into(), dst.into()),
                                    });
                                }
                            }
                        }
                        this.workspace.usage_camera.reset();
                        this.workspace.usage_hover = None;
                    }
                }
                eprintln!(
                    "usage_layout tiles={} elapsed_us={} detail={}",
                    scene.tiles.len(),
                    scene.layout_elapsed.as_micros(),
                    scene.key.detail
                );
                if handing {
                    let target = scene
                        .tiles
                        .iter()
                        .filter(|t| t.depth > 0)
                        .map(|t| {
                            let (id, parent, name) = match t.key {
                                TileKey::Node(i) => (
                                    i,
                                    handoff_scan.nodes[i].parent.unwrap_or(0),
                                    handoff_scan.nodes[i]
                                        .path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned(),
                                ),
                                TileKey::Other(i) => (
                                    (1usize << 63) | (1usize << 62) | i,
                                    scene.key.root,
                                    "其他小项".into(),
                                ),
                            };
                            crate::disktree::scan_preview::Tile {
                                id,
                                parent,
                                rect: t.rect,
                                depth: t.depth,
                                header: t.header.is_some(),
                                header_h: t.header.map_or(0., |h| h.h),
                                class: t.class,
                                name,
                                bytes: t.bytes,
                                opacity: 1.,
                            }
                        })
                        .collect();
                    this.workspace.usage_preview.borrow_mut().handoff(
                        target,
                        Instant::now(),
                        this.session.read(cx).prefs.usage_reduce_motion,
                    );
                }
                // Preserve zoom but clamp its origin after resize/detail relayout.
                this.workspace.usage_camera.pan(0., 0., scene.viewport);
                this.workspace.usage_scene = Some(Arc::new(scene));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    pub(super) fn zoom_usage_center(&mut self, factor: f32, cx: &mut Context<Self>) {
        if self.workspace.scanning {
            let b = self.workspace.usage_bounds.get();
            let v = Rect {
                x: 0.,
                y: 0.,
                w: f32::from(b.size.width),
                h: f32::from(b.size.height),
            };
            self.workspace
                .usage_camera
                .zoom(factor, (v.w / 2., v.h / 2.), v);
            cx.notify();
            return;
        }
        if let Some(scene) = &self.workspace.usage_scene {
            self.workspace.usage_transition = None;
            self.workspace.usage_wheel_gate.reset();
            self.workspace.usage_camera.zoom(
                factor,
                (scene.viewport.w / 2., scene.viewport.h / 2.),
                scene.viewport,
            );
            cx.notify();
        }
    }
    fn usage_point(&self, p: Point<Pixels>) -> (f32, f32) {
        let b = self.workspace.usage_bounds.get();
        (f32::from(p.x - b.origin.x), f32::from(p.y - b.origin.y))
    }
    fn usage_hit(&self, p: (f32, f32)) -> Option<TileKey> {
        if self.workspace.scanning {
            return usage_view::hit_frame(&self.workspace.usage_frame.borrow(), p);
        }
        if self.workspace.usage_scene.as_ref().is_none_or(|s| {
            Some(s.key.root) != self.scan_node()
                || self
                    .workspace
                    .scan
                    .as_ref()
                    .is_none_or(|scan| s.key.scan != Arc::as_ptr(scan) as usize)
        }) {
            return None;
        }
        usage_view::hit_frame(&self.workspace.usage_frame.borrow(), p)
    }
    fn usage_scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        if self.workspace.scanning {
            let b = self.workspace.usage_bounds.get();
            let viewport = Rect {
                x: 0.,
                y: 0.,
                w: f32::from(b.size.width),
                h: f32::from(b.size.height),
            };
            let (dx, dy) = match event.delta {
                ScrollDelta::Lines(p) => usage_view::normalized_wheel(p.x, p.y, true),
                ScrollDelta::Pixels(p) => {
                    usage_view::normalized_wheel(f32::from(p.x), f32::from(p.y), false)
                }
            };
            match usage_view::wheel_intent(
                dx,
                dy,
                event.modifiers.shift,
                event.modifiers.control,
                event.modifiers.alt,
                event.modifiers.platform,
            ) {
                usage_view::WheelIntent::Zoom { steps } => {
                    let p = self.usage_point(event.position);
                    self.workspace
                        .usage_camera
                        .zoom(1.30f32.powf(steps), p, viewport);
                }
                usage_view::WheelIntent::Pan { dx, dy } => {
                    self.workspace.usage_camera.pan(dx, dy, viewport)
                }
                _ => {}
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }

        let Some(scene) = self.workspace.usage_scene.clone().filter(|s| {
            Some(s.key.root) == self.scan_node()
                && self
                    .workspace
                    .scan
                    .as_ref()
                    .is_some_and(|scan| s.key.scan == Arc::as_ptr(scan) as usize)
        }) else {
            return;
        };
        let (dx, dy) = match event.delta {
            ScrollDelta::Lines(p) => usage_view::normalized_wheel(p.x, p.y, true),
            ScrollDelta::Pixels(p) => {
                usage_view::normalized_wheel(f32::from(p.x), f32::from(p.y), false)
            }
        };
        if self.file_actions.native_menu.is_open() {
            cx.stop_propagation();
            return;
        }
        let intent = usage_view::wheel_intent(
            dx,
            dy,
            event.modifiers.shift,
            event.modifiers.control,
            event.modifiers.alt,
            event.modifiers.platform,
        );
        self.workspace.usage_transition = None;
        match intent {
            usage_view::WheelIntent::Ignore => {
                self.workspace.usage_wheel_gate.reset();
                return;
            }
            usage_view::WheelIntent::Pan { dx, dy } => {
                self.workspace.usage_wheel_gate.reset();
                self.workspace.usage_camera.pan(dx, dy, scene.viewport);
            }
            usage_view::WheelIntent::Zoom { steps } => {
                let pointer = self.usage_point(event.position);
                let camera = self.workspace.usage_camera;
                let candidate = {
                    let frames = self.workspace.usage_frame.borrow();
                    frames
                        .iter()
                        .rev()
                        .filter_map(|(k, r, h)| {
                            let TileKey::Node(i) = *k else {
                                return None;
                            };
                            let scan = self.workspace.scan.as_ref()?;
                            if scan.nodes.get(i).is_some_and(|n| n.directory)
                                && tile_body(*r, *h).contains(pointer)
                            {
                                Some(i)
                            } else {
                                None
                            }
                        })
                        .next()
                };
                let ceiling = candidate
                    .and_then(|i| scene.tiles.iter().find(|t| t.key == TileKey::Node(i)))
                    .map(|t| {
                        View::fit_scale(tile_body(t.rect, t.header).into(), scene.viewport.into())
                            .clamp(1., View::MAX_SCALE)
                    })
                    .unwrap_or(View::MAX_SCALE);
                let parent = self
                    .workspace
                    .scan
                    .as_ref()
                    .and_then(|s| s.nodes.get(scene.key.root))
                    .and_then(|n| n.parent);
                let boundary = if steps > 0. && camera.scale >= ceiling - 0.01 {
                    candidate.map(SemanticAction::Enter)
                } else if steps < 0. && camera.scale <= 1.001 && parent.is_some() {
                    Some(SemanticAction::Up)
                } else {
                    None
                };
                if let Some(action) =
                    self.workspace
                        .usage_wheel_gate
                        .evaluate(Instant::now(), steps, boundary)
                {
                    match action {
                        SemanticAction::Enter(i) => self.select_usage_node(i, true, cx),
                        SemanticAction::Up => self.workspace_up(cx),
                    }
                } else {
                    let v = camera
                        .as_view()
                        .zoomed_at(
                            pointer.0,
                            pointer.1,
                            1.30f32.powf(steps),
                            ceiling.max(camera.scale),
                        )
                        .clamped(scene.viewport.into());
                    self.workspace.usage_camera = usage_view::Camera {
                        scale: v.scale,
                        x: v.origin_x,
                        y: v.origin_y,
                    };
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn render_cached_usage_map(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Self::tokens(cx);
        let bounds = self.workspace.usage_bounds.clone();
        let frame = self.workspace.usage_frame.clone();
        let owner = cx.weak_entity();
        let selected = self
            .workspace
            .usage_focus
            .as_ref()
            .and_then(|p| self.workspace.scan_lookup.get(&key(Path::new(p))))
            .copied();
        let mut draws = Vec::<PaintTile>::new();
        let mut empty = None;
        if self.workspace.scanning {
            let now = Instant::now();
            let motion = self.workspace.usage_preview.borrow();
            for t in motion.sample(now) {
                let rect = self.workspace.usage_camera.rect(t.rect);
                let mut color = tokens.usage_fill(t.class, t.depth);
                color.a *= t.opacity;
                draws.push(PaintTile {
                    key: TileKey::Other(t.id),
                    rect,
                    header: (t.header_h > 0.5).then_some(Rect {
                        h: t.header_h,
                        ..rect
                    }),
                    depth: t.depth,
                    color,
                    opacity: t.opacity,
                    name: t.name,
                    size: disk_usage::format_bytes(t.bytes),
                    selected: motion.selected == Some(t.id),
                    hovered: self.workspace.usage_hover == Some(TileKey::Other(t.id)),
                    unknown: false,
                    class: t.class,
                });
            }
            if draws.is_empty() {
                empty = Some("本地实测中 · 等待首批已测数据（总量未知）");
            }
        } else if let (Some(scan), Some(scene)) =
            (&self.workspace.scan, &self.workspace.usage_scene)
        {
            if scene.key.scan == Arc::as_ptr(scan) as usize
                && Some(scene.key.root) == self.scan_node()
            {
                let preview = self.workspace.usage_preview.borrow();
                let blended = if !self.workspace.engine_usage
                    && preview.scope == scene.key.root
                    && preview.finalized
                    && preview.running(Instant::now())
                {
                    preview
                        .sample(Instant::now())
                        .into_iter()
                        .map(|t| (t.id, t))
                        .collect::<std::collections::HashMap<_, _>>()
                } else {
                    Default::default()
                };
                let mut remaining = blended.clone();
                for tile in scene.tiles.iter().filter(|t| t.depth > 0) {
                    let sample = |r| {
                        self.workspace.usage_camera.rect(
                            self.workspace
                                .usage_transition
                                .as_ref()
                                .map(|t| t.sample(r))
                                .unwrap_or(r),
                        )
                    };
                    let id = match tile.key {
                        TileKey::Node(i) => i,
                        TileKey::Other(i) => (1usize << 63) | (1usize << 62) | i,
                    };
                    let blend = blended.get(&id);
                    remaining.remove(&id);
                    let r = blend
                        .map(|t| self.workspace.usage_camera.rect(t.rect))
                        .unwrap_or_else(|| sample(tile.rect));
                    let opacity = blend.map_or(1., |t| t.opacity);
                    if r.w < 1.
                        || r.h < 1.
                        || r.x >= scene.viewport.w
                        || r.y >= scene.viewport.h
                        || r.x + r.w <= 0.
                        || r.y + r.h <= 0.
                    {
                        continue;
                    }
                    let (name, size, unknown) = match tile.key {
                        TileKey::Node(i) => {
                            let n = &scan.nodes[i];
                            (
                                n.path
                                    .file_name()
                                    .map(|x| x.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| display_path(&n.path)),
                                disk_usage::format_bytes(scan.weight(n)),
                                n.uncertain,
                            )
                        }
                        TileKey::Other(_) => (
                            "其他小项".into(),
                            disk_usage::format_bytes(tile.bytes),
                            false,
                        ),
                    };
                    let mut color = tokens.usage_fill(tile.class, tile.depth);
                    color.a *= opacity;
                    draws.push(PaintTile {
                        key: tile.key,
                        rect: r,
                        header: if let Some(t) = blend {
                            (t.header_h > 0.5).then_some(Rect { h: t.header_h, ..r })
                        } else {
                            tile.header.map(sample)
                        },
                        depth: tile.depth,
                        color,
                        opacity,
                        name,
                        size,
                        unknown,
                        class: tile.class,
                        selected: matches!(tile.key,TileKey::Node(i) if Some(i)==selected),
                        hovered: self.workspace.usage_hover == Some(tile.key),
                    });
                }
                for (_, t) in remaining {
                    let mut color = tokens.usage_fill(t.class, t.depth);
                    color.a *= t.opacity;
                    let rect = self.workspace.usage_camera.rect(t.rect);
                    draws.push(PaintTile {
                        key: TileKey::Other(t.id),
                        rect,
                        header: (t.header_h > 0.5).then_some(Rect {
                            h: t.header_h,
                            ..rect
                        }),
                        depth: t.depth,
                        color,
                        opacity: t.opacity,
                        name: t.name,
                        size: disk_usage::format_bytes(t.bytes),
                        selected: false,
                        hovered: false,
                        unknown: false,
                        class: t.class,
                    });
                }
                if scan.weight(&scan.nodes[scene.key.root]) == 0 {
                    empty = Some(if scan.logical_generation.is_some() {
                        "索引逻辑大小为 0；未验证元数据不是已确认空文件"
                    } else {
                        "已测占用为 0；未知项、空文件和硬链接别名仍可在右侧查看"
                    });
                }
            } else {
                empty = Some("正在排布已测结果…");
            }
        } else {
            empty = Some(if self.workspace.engine_usage {
                if self.workspace.engine_busy {
                    "正在读取引擎索引逻辑目录树…"
                } else {
                    if self.workspace.scan_status.contains("不支持") {
                        self.workspace.scan_status.as_str()
                    } else {
                        "当前服务尚未提供索引逻辑目录树；状态见下方，可明确选择本地实测"
                    }
                }
            } else {
                "选择目录开始测量；已有快照会直接读取，不会自动全盘扫描"
            });
        }
        let paint = draws.clone();
        let menu_owner = cx.weak_entity();
        let mut view = div()
            .id("usage-camera")
            .flex()
            .flex_col()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .m_2()
            .overflow_hidden()
            .rounded_sm()
            .bg(tokens.surface)
            .when(
                matches!(self.workspace.usage_hover, Some(TileKey::Node(_))),
                |view| view.cursor_pointer(),
            )
            .track_focus(&self.workspace.usage_map_focus)
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| this.usage_scroll(event, cx)))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let hit = this.usage_hit(this.usage_point(event.position));
                if hit != this.workspace.usage_hover {
                    this.workspace.usage_hover = hit;
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.workspace.usage_hover = None;
                    this.workspace.usage_wheel_gate.reset();
                    cx.notify();
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                let p = this.usage_point(window.mouse_position());
                this.workspace.usage_map_focus.focus(window, cx);
                if this.workspace.scanning {
                    if let Some(TileKey::Other(id)) = this.usage_hit(p) {
                        this.workspace.usage_preview.borrow_mut().selected = Some(id);
                    }
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                match this.usage_hit(p) {
                    Some(TileKey::Node(i)) => this.select_usage_node(i, false, cx),
                    _ => {
                        if let Some(root) = this.scan_node() {
                            this.select_usage_node(root, false, cx);
                        }
                    }
                }
                cx.stop_propagation();
            }))
            .on_double_click(cx.listener(|this, _, window, cx| {
                if this.workspace.scanning {
                    if let Some(TileKey::Other(id)) =
                        this.usage_hit(this.usage_point(window.mouse_position()))
                    {
                        let entered = this.workspace.usage_preview.borrow_mut().enter(id);
                        if entered {
                            this.workspace.usage_camera.reset();
                            cx.notify();
                        }
                    }
                    cx.stop_propagation();
                    return;
                }
                if let Some(TileKey::Node(i)) =
                    this.usage_hit(this.usage_point(window.mouse_position()))
                {
                    this.select_usage_node(i, true, cx);
                }
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "+" | "=" => this.zoom_usage_center(1.3, cx),
                    "-" => this.zoom_usage_center(1. / 1.3, cx),
                    "0" => {
                        this.workspace.usage_camera.reset();
                        this.workspace.usage_transition = None;
                        cx.notify();
                    }
                    "enter" => this.workspace_enter(cx),
                    "backspace" => this.workspace_up(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .context_menu(move |menu, window, cx| {
                menu_owner
                    .update(cx, |this, cx| {
                        this.workspace.usage_hover = None;
                        this.workspace.usage_wheel_gate.reset();
                        if this.workspace.scanning {
                            return menu.item(gpui_kit::component::menu::PopupMenuItem::label(
                                "扫描正在更新；完成后再打开条目操作",
                            ));
                        }
                        if let Some(TileKey::Node(i)) =
                            this.usage_hit(this.usage_point(window.mouse_position()))
                        {
                            this.select_usage_node(i, false, cx);
                            this.file_context_menu(menu, false, window, cx)
                        } else {
                            menu.item(gpui_kit::component::menu::PopupMenuItem::label(
                                "空隙 / 合并项无文件操作；右键实际条目",
                            ))
                        }
                    })
                    .unwrap()
            })
            .child(
                canvas(
                    move |b, _, cx| {
                        #[cfg(debug_assertions)]
                        fixture_geometry("layout", b, draws.len());
                        let old = bounds.replace(b);
                        *frame.borrow_mut() = draws
                            .iter()
                            .filter(|d| d.key != TileKey::Other(usize::MAX))
                            .map(|d| (d.key, d.rect, d.header))
                            .collect();
                        if old.size != b.size {
                            let _ = owner.update(cx, |_, cx| cx.notify());
                        }
                    },
                    move |b, _, window, cx| {
                        #[cfg(debug_assertions)]
                        fixture_geometry("paint", b, paint.len());
                        let started = Instant::now();
                        let screen = |r: Rect| Bounds {
                            origin: point(
                                b.origin.x + px(r.x.round()),
                                b.origin.y + px(r.y.round()),
                            ),
                            size: size(px(r.w.round().max(0.)), px(r.h.round().max(0.))),
                        };
                        window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
                            // Fill first, then labels, finally outlines (selected stays visible).
                            for d in &paint {
                                if d.depth == 0 {
                                    continue;
                                }
                                window.paint_quad(fill(screen(d.rect), d.color));
                                if d.depth == 1 {
                                    window.paint_quad(fill(
                                        screen(Rect { h: 2., ..d.rect }),
                                        tokens.usage_accent(d.class),
                                    ));
                                }
                                if d.unknown {
                                    window.paint_quad(fill(
                                        screen(Rect {
                                            x: d.rect.x + d.rect.w - 5.,
                                            y: d.rect.y,
                                            w: 5.,
                                            h: 5.,
                                        }),
                                        tokens.warning,
                                    ));
                                }
                            }
                            for d in &paint {
                                let label = d.header.unwrap_or(d.rect);
                                if label.w < 48. || label.h < 16. || d.opacity < 0.65 {
                                    continue;
                                }
                                let clip = screen(label);
                                let text = d.name.clone();
                                let run = TextRun {
                                    len: text.len(),
                                    font: Font {
                                        weight: if d.header.is_some() {
                                            FontWeight::SEMIBOLD
                                        } else {
                                            FontWeight::NORMAL
                                        },
                                        ..font("Segoe UI")
                                    },
                                    color: {
                                        let mut c = tokens.text_primary;
                                        c.a *= d.opacity;
                                        c
                                    },
                                    ..Default::default()
                                };
                                let line = window.text_system().shape_line(
                                    SharedString::from(text),
                                    px(11.),
                                    &[run],
                                    None,
                                );
                                window.with_content_mask(
                                    Some(ContentMask { bounds: clip }),
                                    |window| {
                                        let _ = line.paint(
                                            point(clip.origin.x + px(5.), clip.origin.y + px(2.)),
                                            px(16.),
                                            TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                    },
                                );
                                if d.header.is_some() && label.w >= 118. {
                                    let run = TextRun {
                                        len: d.size.len(),
                                        font: font("Segoe UI"),
                                        color: {
                                            let mut c = tokens.text_secondary;
                                            c.a *= d.opacity;
                                            c
                                        },
                                        ..Default::default()
                                    };
                                    let line = window.text_system().shape_line(
                                        SharedString::from(d.size.clone()),
                                        px(10.),
                                        &[run],
                                        None,
                                    );
                                    let width = f32::from(line.width());
                                    window.with_content_mask(
                                        Some(ContentMask { bounds: clip }),
                                        |window| {
                                            let origin = point(
                                                clip.origin.x + px((label.w - width - 6.).max(48.)),
                                                clip.origin.y + px(2.),
                                            );
                                            let _ = line.paint(
                                                origin,
                                                px(16.),
                                                TextAlign::Left,
                                                None,
                                                window,
                                                cx,
                                            );
                                        },
                                    );
                                } else if d.header.is_none() && label.h >= 44. && label.w >= 70. {
                                    let run = TextRun {
                                        len: d.size.len(),
                                        font: font("Segoe UI"),
                                        color: {
                                            let mut c = tokens.text_secondary;
                                            c.a *= d.opacity;
                                            c
                                        },
                                        ..Default::default()
                                    };
                                    let line = window.text_system().shape_line(
                                        SharedString::from(d.size.clone()),
                                        px(10.),
                                        &[run],
                                        None,
                                    );
                                    window.with_content_mask(
                                        Some(ContentMask { bounds: clip }),
                                        |window| {
                                            let _ = line.paint(
                                                point(
                                                    clip.origin.x + px(5.),
                                                    clip.origin.y + px(20.),
                                                ),
                                                px(14.),
                                                TextAlign::Left,
                                                None,
                                                window,
                                                cx,
                                            );
                                        },
                                    );
                                }
                            }
                            for d in paint
                                .iter()
                                .filter(|d| d.depth > 0 && (d.selected || d.hovered))
                            {
                                let mut clear = tokens.surface;
                                clear.a = 0.;
                                window.paint_quad(quad(
                                    screen(d.rect),
                                    0.,
                                    clear,
                                    if d.selected { 2. } else { 1. },
                                    if d.selected {
                                        tokens.accent
                                    } else {
                                        tokens.text_secondary
                                    },
                                    BorderStyle::Solid,
                                ));
                            }
                        });
                        usage_view::record_paint(started.elapsed());
                    },
                )
                .absolute()
                .size_full(),
            );
        if let Some(message) = empty {
            view = view.child(self.usage_empty(message, tokens));
        }
        view.into_any_element()
    }
}

#[cfg(debug_assertions)]
fn fixture_geometry(event: &str, b: Bounds<Pixels>, tiles: usize) {
    use std::io::Write;
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if std::env::var_os("FINDX_NATIVE_USAGE_OPEN").is_none()
        || std::env::args().any(|a| a == "--live")
        || N.fetch_add(1, Ordering::Relaxed) >= 64
    {
        return;
    }
    let path =
        std::env::temp_dir().join(format!("findx-fixture-geometry-{}.log", std::process::id()));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            f,
            "{} x={} y={} width={} height={} tiles={}",
            event,
            f32::from(b.origin.x),
            f32::from(b.origin.y),
            f32::from(b.size.width),
            f32::from(b.size.height),
            tiles
        );
    }
}
