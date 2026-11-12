//! Background-only scene layout; rendering and hit testing use the same sampled screen rectangles.
use super::*;
use crate::usage_view::{self, Rect, SceneKey, TileKey};
use gpui_kit::component::menu::ContextMenuExt as _;
use std::time::Instant;
pub(super) struct Transition {
    from: HashMap<TileKey, Rect>,
    started: Instant,
    duration: Duration,
}
impl Transition {
    fn sample(&self, key: TileKey, to: Rect) -> Rect {
        let t = usage_view::ease_out(
            self.started.elapsed().as_secs_f32() / self.duration.as_secs_f32(),
        );
        self.from
            .get(&key)
            .copied()
            .unwrap_or(Rect {
                x: to.x + to.w / 2.,
                y: to.y + to.h / 2.,
                w: 0.,
                h: 0.,
            })
            .lerp(to, t)
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
        if let Some(transition) = &self.workspace.usage_transition {
            if transition.started.elapsed() < transition.duration {
                window.request_animation_frame();
            } else {
                self.workspace.usage_transition = None;
            }
        }
        let Some(scan) = self.workspace.scan.clone() else {
            return;
        };
        let Some(root) = self.scan_node() else {
            if self.workspace.usage_scene_key.is_some() || self.workspace.usage_scene.is_some() {
                self.workspace.usage_scene_generation =
                    self.workspace.usage_scene_generation.wrapping_add(1);
                self.workspace.usage_scene_key = None;
                self.workspace.usage_scene = None;
                self.workspace.usage_transition = None;
                self.workspace.usage_camera.reset();
            }
            return;
        };
        let bounds = self.workspace.usage_bounds.get();
        let width = f32::from(bounds.size.width).round() as u32;
        let height = f32::from(bounds.size.height).round() as u32;
        if width < 2 || height < 2 {
            return;
        }
        let key = SceneKey {
            scan: Arc::as_ptr(&scan) as usize,
            root,
            depth: self.workspace.usage_depth,
            width,
            height,
        };
        if self.workspace.usage_scene_key.as_ref() == Some(&key) {
            return;
        }
        self.workspace.usage_scene_key = Some(key.clone());
        self.workspace.usage_scene_generation =
            self.workspace.usage_scene_generation.wrapping_add(1);
        let generation = self.workspace.usage_scene_generation;
        cx.spawn(async move |this, cx| {
            let scene = cx
                .background_spawn(async move { usage_view::build_scene(&scan, key) })
                .await;
            this.update(cx, |this, cx| {
                if this.workspace.usage_scene_generation != generation {
                    return;
                }
                this.workspace.usage_transition = None;
                if let Some(previous) = &this.workspace.usage_scene {
                    if previous.key.root != scene.key.root
                        || previous.key.scan != scene.key.scan
                        || previous.key.width != scene.key.width
                        || previous.key.height != scene.key.height
                    {
                        if previous.key.scan == scene.key.scan
                            && previous.key.root != scene.key.root
                            && !this.session.read(cx).prefs.usage_reduce_motion
                        {
                            let camera = this.workspace.usage_camera;
                            let from = previous
                                .tiles
                                .iter()
                                .map(|tile| (tile.key, camera.rect(tile.rect)))
                                .collect();
                            this.workspace.usage_transition = Some(Transition {
                                from,
                                started: Instant::now(),
                                duration: Duration::from_millis(200),
                            });
                        }
                        this.workspace.usage_camera.reset();
                    }
                }
                this.workspace.usage_scene = Some(Arc::new(scene));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    pub(super) fn zoom_usage_center(&mut self, factor: f32, cx: &mut Context<Self>) {
        if let Some(scene) = &self.workspace.usage_scene {
            self.workspace.usage_transition = None;
            self.workspace.usage_camera.zoom(
                factor,
                (scene.viewport.w / 2., scene.viewport.h / 2.),
                scene.viewport,
            );
            cx.notify();
        }
    }
    pub(super) fn render_cached_usage_map(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Self::tokens(cx);
        let bounds = self.workspace.usage_bounds.clone();
        let owner = cx.weak_entity();
        let mut view = div()
            .id("usage-camera")
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .m_3()
            .overflow_hidden()
            .rounded_sm()
            .bg(tokens.surface)
            .track_focus(&self.workspace.usage_map_focus)
            .child(
                canvas(
                    move |b, _, cx| {
                        let old = bounds.replace(b);
                        if old.size != b.size {
                            let _ = owner.update(cx, |_, cx| cx.notify());
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                let Some(scene) = &this.workspace.usage_scene else {
                    return;
                };
                let viewport = scene.viewport;
                let delta = event.delta.pixel_delta(px(30.));
                let dx = f32::from(delta.x);
                let dy = f32::from(delta.y);
                this.workspace.usage_transition = None;
                if event.modifiers.shift {
                    this.workspace.usage_camera.pan(
                        if dx.abs() > 0.01 { dx } else { dy },
                        0.,
                        viewport,
                    );
                } else if event.modifiers.control {
                    this.workspace.usage_camera.pan(dx, dy, viewport);
                } else {
                    let b = this.workspace.usage_bounds.get();
                    let pointer = (
                        f32::from(event.position.x - b.origin.x),
                        f32::from(event.position.y - b.origin.y),
                    );
                    let factor = 1.15f32.powf((dy / 30.).clamp(-3., 3.));
                    this.workspace.usage_camera.zoom(factor, pointer, viewport);
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "+" | "=" => this.zoom_usage_center(1.15, cx),
                    "-" => this.zoom_usage_center(1. / 1.15, cx),
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
            }));
        if self.workspace.scan.is_some() && self.scan_node().is_none() {
            return view
                .child(self.usage_empty(
                    "当前目录不在已测范围内。点击开始扫描测量此目录；不会自动扫描。",
                    tokens,
                ))
                .into_any_element();
        }
        let Some(scan) = self.workspace.scan.as_ref() else {
            return view
                .child(self.usage_empty(
                    if self.workspace.scanning {
                        "扫描进行中；已有结果将在结束或取消后可用"
                    } else {
                        "选择目录并手动开始扫描；不会自动扫描整个卷"
                    },
                    tokens,
                ))
                .into_any_element();
        };
        let Some(scene) = self
            .workspace
            .usage_scene
            .as_ref()
            .filter(|s| s.key.scan == Arc::as_ptr(scan) as usize)
        else {
            return view
                .child(self.usage_empty("正在排布已测结果…", tokens))
                .into_any_element();
        };
        if scan
            .nodes
            .get(scene.key.root)
            .is_some_and(|n| n.allocated == 0)
        {
            return view
                .child(self.usage_empty(
                    "已测占用为 0；未知项、空文件和硬链接别名仍可在右侧查看",
                    tokens,
                ))
                .into_any_element();
        }
        let current = self.workspace.usage_scene_key.as_ref() == Some(&scene.key);
        let selected = self
            .workspace
            .usage_focus
            .as_ref()
            .and_then(|path| self.workspace.scan_lookup.get(&key(Path::new(path))))
            .copied();
        let camera = self.workspace.usage_camera;
        for tile in &scene.tiles {
            // The root is the map background, not an overlapping clickable sibling.
            if tile.depth == 0 {
                continue;
            }
            let base = self
                .workspace
                .usage_transition
                .as_ref()
                .map(|t| t.sample(tile.key, tile.rect))
                .unwrap_or(tile.rect);
            let rect = camera.rect(base);
            if rect.w < 1.
                || rect.h < 1.
                || rect.x > scene.viewport.w
                || rect.y > scene.viewport.h
                || rect.x + rect.w < 0.
                || rect.y + rect.h < 0.
            {
                continue;
            }
            let (index, label, category, tooltip) = match tile.key {
                TileKey::Node(index) => {
                    let n = &scan.nodes[index];
                    let name = n
                        .path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| display_path(&n.path));
                    (
                        Some(index),
                        format!("{}  {}", name, disk_usage::format_bytes(n.allocated)),
                        n.category,
                        display_path(&n.path),
                    )
                }
                TileKey::Other(parent) => (
                    None,
                    format!("目录自身 / 其余  {}", disk_usage::format_bytes(tile.bytes)),
                    0,
                    format!(
                        "{} · 保留已测面积的合并项；展开父目录查看明细",
                        display_path(&scan.nodes[parent].path)
                    ),
                ),
            };
            let mut color = tokens.allocation_color(category);
            color.l = (color.l
                + tile.depth.min(6) as f32 * if tokens.bg.l < 0.4 { -0.006 } else { 0.009 })
            .clamp(0., 1.);
            let mut element = div()
                .id(SharedString::from(format!("usage-tile-{:?}", tile.key)))
                .absolute()
                .left(px(rect.x))
                .top(px(rect.y))
                .w(px(rect.w))
                .h(px(rect.h))
                .overflow_hidden()
                .occlude()
                .border_1()
                .border_color(if index.is_some() && index == selected {
                    tokens.accent
                } else {
                    tokens.border_soft
                })
                .bg(color)
                .text_color(tokens.text_primary)
                .when(tile.depth > 0 && rect.w >= 72. && rect.h >= 20., |d| {
                    d.child(
                        div()
                            .px_1()
                            .h(px(20.))
                            .text_size(px(11.))
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .overflow_hidden()
                            .child(label),
                    )
                })
                .tooltip(move |w, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(w, cx)
                });
            if let Some(index) = index {
                let click_key = scene.key.clone();
                let double_key = scene.key.clone();
                let menu_key = scene.key.clone();
                element = element
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.workspace.mode != ViewMode::Usage
                            || this.workspace.usage_scene_key.as_ref() != Some(&click_key)
                            || this
                                .workspace
                                .scan
                                .as_ref()
                                .map(|s| Arc::as_ptr(s) as usize)
                                != Some(click_key.scan)
                        {
                            return;
                        }
                        this.workspace.usage_map_focus.focus(window, cx);
                        this.select_usage_node(index, false, cx);
                        cx.stop_propagation();
                    }))
                    .on_double_click(cx.listener(move |this, _, window, cx| {
                        if this.workspace.mode != ViewMode::Usage
                            || this.workspace.usage_scene_key.as_ref() != Some(&double_key)
                            || this
                                .workspace
                                .scan
                                .as_ref()
                                .map(|s| Arc::as_ptr(s) as usize)
                                != Some(double_key.scan)
                        {
                            return;
                        }
                        this.workspace.usage_map_focus.focus(window, cx);
                        this.select_usage_node(index, true, cx);
                        cx.stop_propagation();
                    }));
                let owner = cx.weak_entity();
                view = view.child(element.context_menu(move |menu, window, cx| {
                    owner
                        .update(cx, |this, cx| {
                            if this.workspace.usage_scene_key.as_ref() != Some(&menu_key)
                                || this
                                    .workspace
                                    .scan
                                    .as_ref()
                                    .map(|s| Arc::as_ptr(s) as usize)
                                    != Some(menu_key.scan)
                            {
                                return menu.item(gpui_kit::component::menu::PopupMenuItem::label(
                                    "视图已更新，请重新打开菜单",
                                ));
                            }
                            this.select_usage_node(index, false, cx);
                            this.file_context_menu(menu, false, window, cx)
                        })
                        .unwrap()
                }));
            } else {
                view = view.child(element);
            }
        }
        if !current {
            view = view.child(
                div()
                    .absolute()
                    .top_2()
                    .right_2()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(tokens.surface)
                    .text_xs()
                    .child("正在排布…"),
            );
        }
        view.into_any_element()
    }
}
