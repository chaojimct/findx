//! Three deliberately different information architectures, inside one custom window shell.
use super::*;
use gpui_kit::component::FocusableExt as _;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_kit::AnimationExt as _;

impl MainWindow {
    pub(in super::super) fn render_workspace_page(
        &self,
        sidebar: bool,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body: AnyElement = match self.workspace.mode {
            ViewMode::Search => v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(self.render_search_toolbar(preview, cx))
                .child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .items_stretch()
                        .overflow_hidden()
                        .when(sidebar, |view| view.child(self.render_sidebar(cx)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .min_h_0()
                                .child(self.render_workspace_center(window, cx)),
                        )
                        .when(preview, |view| {
                            view.child(self.render_preview_separator(window, cx))
                                .child(self.render_preview(cx))
                        }),
                )
                .into_any_element(),
            ViewMode::Directory => h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_stretch()
                .overflow_hidden()
                .when(sidebar, |view| view.child(self.render_locations(cx)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .child(self.render_directory_tabs(preview, cx))
                        .child(self.render_directory_toolbar(preview, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_h_0()
                                .child(self.render_workspace_center(window, cx)),
                        ),
                )
                .when(preview, |view| {
                    view.child(self.render_preview_separator(window, cx))
                        .child(self.render_preview(cx))
                })
                .into_any_element(),
            ViewMode::Usage => v_flex()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .w_full()
                .overflow_hidden()
                .child(self.render_usage_toolbar(cx))
                .child(self.render_workspace_usage(cx))
                .into_any_element(),
        };
        // Page-enter slide (Argus parity, stock GPUI): new mode glides in
        // +32px -> 0 over 250ms ease-out with full opacity throughout.
        // A fresh animation id per page_epoch restarts the slide on every
        // mode switch; same-mode navigation reuses the id (no replay).
        // `relative` keeps layout stable: only paint offset changes per frame.
        // Honour the global motion kill-switch (settings UI binding follows).
        if self.session.read(cx).prefs.reduce_motion {
            return body;
        }
        let epoch = self.workspace.page_epoch;
        // This wrapper MUST be a flex column: `body` carries `flex_1()` and only
        // expands to fill when its parent is a flex container. A plain non-flex
        // container here collapses `body` to content height and leaves a blank
        // strip between the page and the status bar.
        v_flex()
            .relative()
            .size_full()
            .child(body)
            .with_animation(
                gpui_kit::ElementId::named_usize("page-enter", epoch as usize),
                gpui_kit::Animation::new(std::time::Duration::from_millis(
                    crate::ui::motion::PAGE_ENTER_MS,
                ))
                .with_easing(crate::ui::motion::ease_out_cubic),
                move |el, delta| {
                    el.left(px(crate::ui::motion::PAGE_ENTER_PX * (1.0 - delta)))
                        .opacity(delta)
                },
            )
            .into_any_element()
    }

    pub(super) fn tool_button(
        &self,
        id: &'static str,
        label: &'static str,
        action: u8,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .ghost()
            .label(if label == "⋯" { "" } else { label })
            .icon(match action {
                0 => IconName::PanelRight,
                1 => IconName::Settings,
                2 => IconName::Ellipsis,
                _ => IconName::FolderOpen,
            })
            .tooltip(if label == "⋯" {
                "文件操作"
            } else {
                label
            })
            .selected(selected)
            .h(px(30.))
            .px_2()
            .on_click(cx.listener(move |this, _, window, cx| match action {
                0 => this.toggle_panel(search_logic::PanelKind::Preview, selected, cx),
                1 => this.open_settings_page(window, cx),
                2 => {
                    this.show_file_menu(window, cx);
                }
                3 => this.workspace_reveal(cx),
                _ => {}
            }))
    }

    fn render_search_toolbar(&self, preview: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        h_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .flex_shrink_0()
            .bg(tokens.bg)
            .border_b_1()
            .border_color(tokens.border)
            .when_some(self.search_scope.as_ref(), |view, path| {
                view.child(
                    div()
                        .id("search-scope-summary")
                        .min_w_0()
                        .max_w(px(190.))
                        .tooltip({
                            let full = display_path(path);
                            move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(full.clone())
                                    .build(window, cx)
                            }
                        })
                        .text_xs()
                        .text_color(tokens.text_secondary)
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(format!("范围：{}（含子目录）", display_path(path))),
                )
                .child(
                    Button::new("return-global-search")
                        .icon(Icon::default().path("icons/arrow-left.svg"))
                        .ghost()
                        .label("返回全局")
                        .on_click(cx.listener(|this, _, _, cx| this.return_global_search(cx))),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.search).cleanable(true)),
            )
            .child(
                self.tool_button("search-reveal", "定位", 3, false, cx)
                    .disabled(self.active_workspace_hit().is_none()),
            )
            .child(self.tool_button("search-preview", "预览", 0, preview, cx))
    }

    pub(super) fn render_directory_table(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Self::tokens(cx);
        let empty = self.workspace.rows.is_empty();
        let message = if self.workspace.listing {
            "正在读取目录…"
        } else if self.workspace.status.starts_with("目录读取失败") {
            "无法读取此目录；请检查位置、权限或磁盘连接。诊断见状态栏。"
        } else if !self.workspace.filter_text.is_empty() {
            "当前目录没有匹配项；本层过滤不包含子目录。"
        } else {
            "此目录为空"
        };
        let owner = cx.weak_entity();
        div()
            .id("directory-table-context")
            .relative()
            .size_full()
            .child(crate::ui::native_focus::table_frame(
                DataTable::new(&self.workspace.table)
                    .focus_ring(false)
                    .scrollbar_visible(
                        true,
                        self.workspace
                            .table
                            .read(cx)
                            .delegate()
                            .widths
                            .iter()
                            .sum::<f32>()
                            > self.table_viewport_width + 0.5,
                    )
                    .bordered(false),
                self.table_navigation_focus.visible(
                    self.workspace
                        .table
                        .read(cx)
                        .focus_handle(cx)
                        .is_focused(window),
                    window.is_window_active(),
                ),
                tokens.text_tertiary,
            ))
            .when(empty, |view| {
                view.child(
                    div()
                        .absolute()
                        .top(px(32.))
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .bg(tokens.bg)
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_4()
                        .text_sm()
                        .text_color(tokens.text_secondary)
                        .child(message),
                )
            })
            .context_menu(move |menu, window, cx| {
                owner
                    .update(cx, |view, cx| {
                        view.file_context_menu(menu, true, window, cx)
                    })
                    .unwrap()
            })
            .into_any_element()
    }

    fn navigation_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().flex_shrink_0().gap_1().children(
            [
                ("directory-back", "后退（Alt+Left）", 0),
                ("directory-forward", "前进（Alt+Right）", 1),
                ("directory-up", "上一级（Alt+Up）", 2),
            ]
            .into_iter()
            .map(|(id, label, action)| {
                Button::new(id)
                    .ghost()
                    .icon(match action {
                        0 => IconName::ArrowLeft,
                        1 => IconName::ArrowRight,
                        _ => IconName::ArrowUp,
                    })
                    .tooltip(label)
                    .accessibility_label(label)
                    .w(px(28.))
                    .h(px(30.))
                    .px_0()
                    .disabled(match action {
                        0 => self.workspace.history.back.is_empty(),
                        1 => self.workspace.history.forward.is_empty(),
                        // Volume roots in indexed usage drill up to home.
                        _ => {
                            if is_home_path(&self.workspace.location) {
                                true
                            } else if self
                                .workspace
                                .location
                                .parent()
                                .filter(|p| !p.as_os_str().is_empty())
                                .is_some()
                            {
                                false
                            } else {
                                !(self.workspace.mode == ViewMode::Usage
                                    && self.workspace.engine_usage)
                            }
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| match action {
                        0 => this.workspace_history(false, cx),
                        1 => this.workspace_history(true, cx),
                        _ => this.workspace_up(cx),
                    }))
            }),
        )
    }

    fn render_directory_toolbar(&self, _preview: bool, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Self::tokens(cx);
        h_flex()
            .w_full()
            .h(px(44.))
            .px_2()
            .gap_2()
            .flex_shrink_0()
            .bg(tokens.bg)
            .border_b_1()
            .border_color(tokens.border)
            .child(self.navigation_buttons(cx))
            .child(self.render_address(cx))
            .child(
                div()
                    .w(px(if self.last_content_width < 900. {
                        142.
                    } else {
                        196.
                    }))
                    .child(Input::new(&self.workspace.filter_input).cleanable(true)),
            )
            .into_any_element()
    }

    // Sidebar anatomy is fixed for every group and item. No label-sized centring.
    fn sidebar_active_group(&self, cx: &App) -> Option<&'static str> {
        let current = key(&self.workspace.location);
        if let Some((group, path)) = &self.workspace.sidebar_source {
            if path == &current {
                return Some(*group);
            }
        }
        let session = self.session.read(cx);
        if self
            .workspace
            .locations
            .iter()
            .any(|(_, _, p)| key(p) == current)
            || session
                .prefs
                .browser_favorites
                .iter()
                .any(|p| key(Path::new(p)) == current)
        {
            return Some("favorites");
        }
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
        if home.as_ref().is_some_and(|p| key(p) == current)
            || session
                .available_roots
                .iter()
                .any(|p| key(Path::new(p)) == current)
            || self.workspace.tree_children.values().any(|children| {
                children
                    .iter()
                    .any(|(_, path)| key(path) == current)
            })
        {
            return Some("tree");
        }
        if session
            .prefs
            .browser_recent
            .iter()
            .any(|p| key(Path::new(p)) == current)
        {
            Some("recent")
        } else {
            None
        }
    }
    fn sidebar_header(
        &self,
        group: &'static str,
        title: &'static str,
        empty: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = Self::tokens(cx);
        let collapsed = self.workspace.collapsed_groups.contains(group);
        let favorite = self
            .session
            .read(cx)
            .prefs
            .browser_favorites
            .iter()
            .any(|p| key(Path::new(p)) == key(&self.workspace.location));
        // 16px outer padding + 8px header inset + a permanent 24px action slot.
        let header_content = DIRECTORY_SIDEBAR_WIDTH - 16. - 8. - 24.;
        h_flex()
            .w_full()
            .h(px(26.))
            .px_1()
            .child(
                Button::new(SharedString::from(format!("sidebar-group-{group}")))
                    .ghost()
                    .rounded_sm()
                    .h_full()
                    .px_0()
                    .flex_1()
                    .min_w_0()
                    .accessibility_label(format!(
                        "{}{}分组",
                        if collapsed { "展开" } else { "折叠" },
                        title
                    ))
                    .child(
                        h_flex()
                            .w(px(header_content))
                            .items_center()
                            .child(
                                h_flex().w(px(16.)).flex_shrink_0().justify_center().child(
                                    Icon::new(if collapsed {
                                        IconName::ChevronRight
                                    } else {
                                        IconName::ChevronDown
                                    })
                                    .size_3(),
                                ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .pl_1()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_xs()
                                    .text_color(t.text_primary)
                                    .text_ellipsis()
                                    .child(title),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.workspace.collapsed_groups.remove(group) {
                            this.workspace.collapsed_groups.insert(group);
                        }
                        cx.notify();
                    })),
            )
            .child(
                h_flex()
                    .w(px(24.))
                    .h_full()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .when(group == "favorites", |v| {
                        v.child(
                            Button::new("favorite-current-directory")
                                .ghost()
                                .w(px(24.))
                                .h(px(24.))
                                .px_0()
                                .icon(Icon::default().path(if favorite {
                                    "icons/bookmark-minus.svg"
                                } else {
                                    "icons/plus.svg"
                                }))
                                .tooltip(if favorite {
                                    "从收藏夹移除此目录"
                                } else {
                                    "收藏当前目录"
                                })
                                .accessibility_label(if favorite {
                                    "从收藏夹移除此目录"
                                } else {
                                    "收藏当前目录"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_favorite(this.workspace.location.clone(), cx)
                                })),
                        )
                    })
                    .when(group == "tree", |v| {
                        v.child(
                            Button::new("refresh-directory-tree")
                                .ghost()
                                .w(px(24.))
                                .h(px(24.))
                                .px_0()
                                .icon(IconName::RefreshCw)
                                .tooltip("刷新文件夹树")
                                .accessibility_label("刷新文件夹树")
                                .on_click(cx.listener(|this, _, _, cx| this.refresh_tree(cx))),
                        )
                    })
                    .when(group == "recent" && !empty, |v| {
                        v.child(
                            Button::new("clear-recent-directories")
                                .ghost()
                                .w(px(24.))
                                .h(px(24.))
                                .px_0()
                                .icon(IconName::Close)
                                .tooltip("清空最近目录记录")
                                .accessibility_label("清空最近目录记录")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.session.update(cx, |s, cx| {
                                        s.prefs.browser_recent.clear();
                                        crate::session::persist_prefs(s.prefs.clone(), cx);
                                    });
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .into_any_element()
    }
    /// One shared context menu for every sidebar row. Right-click records the
    /// target path first; the actions then consume it through the session focus.
    fn sidebar_context_menu(
        &self,
        favorite_label: Option<&'static str>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let focus = self.focus.clone();
        move |menu, _, _| {
            let mut menu = menu.menu("在新标签页中打开", Box::new(SidebarTreeOpenNewTab));
            if let Some(label) = favorite_label {
                menu = menu.menu(label, Box::new(SidebarTreeToggleFavorite));
            }
            menu.action_context(focus.clone())
        }
    }

    fn sidebar_item(
        &self,
        group: &'static str,
        index: usize,
        label: String,
        icon: String,
        path: PathBuf,
        favorite_label: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on = self.sidebar_active_group(cx) == Some(group)
            && key(&path) == key(&self.workspace.location);
        let full = display_path(&path);
        // The row background is inset 12px on BOTH edges. Inside: 16px indent,
        // 20px icon slot, a flexible ellipsized text slot, and 16px tail slot.
        let row_width = DIRECTORY_SIDEBAR_WIDTH - 24.;
        let right_path = path.clone();
        Button::new(SharedString::from(format!("sidebar-{group}-{index}")))
            .ghost()
            .w_full()
            .h(px(28.))
            .px_0()
            .rounded_sm()
            .selected(on)
            .accessibility_label(label.clone())
            .tooltip(full)
            .child(
                h_flex()
                    .w(px(row_width))
                    .items_center()
                    .child(div().w(px(16.)).flex_shrink_0())
                    .child(
                        h_flex()
                            .w(px(20.))
                            .flex_shrink_0()
                            .justify_center()
                            .child(Icon::default().path(format!("icons/{icon}.svg")).size_4()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .pl_1()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(label),
                    )
                    .child(div().w(px(16.)).flex_shrink_0()),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                this.workspace.sidebar_tree_target = Some(right_path.clone());
                cx.stop_propagation();
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.workspace.sidebar_source = Some((group, key(&path)));
                this.workspace_navigate(path.clone(), ViewMode::Directory, None, true, cx);
            }))
            .context_menu(self.sidebar_context_menu(favorite_label))
            .into_any_element()
    }
    fn render_location_group(
        &self,
        group: &'static str,
        title: &'static str,
        items: Vec<(String, String, PathBuf, Option<&'static str>)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = Self::tokens(cx);
        let collapsed = self.workspace.collapsed_groups.contains(group);
        let empty = items.is_empty();
        v_flex()
            .w_full()
            .py_1()
            .border_b_1()
            .border_color(t.border_soft)
            .child(self.sidebar_header(group, title, empty, cx))
            .when(!collapsed, |v| {
                v.child(
                    v_flex()
                        .w_full()
                        .px_1()
                        .children(
                            items
                                .into_iter()
                                .enumerate()
                                .map(|(i, (label, icon, path, favorite_label))| {
                                    self.sidebar_item(
                                        group,
                                        i,
                                        label,
                                        icon,
                                        path,
                                        favorite_label,
                                        cx,
                                    )
                                }),
                        ),
                )
            })
            .when(!collapsed && empty, |v| {
                v.child(
                    div()
                        .pl(px(32.))
                        .pr_3()
                        .py_2()
                        .text_xs()
                        .text_color(t.text_tertiary)
                        .child(if group == "favorites" {
                            "点 + 收藏当前目录"
                        } else if group == "recent" {
                            "访问目录后在此显示"
                        } else {
                            "暂无可用位置"
                        }),
                )
            })
            .into_any_element()
    }
    /// One row of the Explorer-style folder tree. The chevron loads children
    /// lazily; the row itself navigates. Right-click uses the shared menu.
    fn sidebar_tree_item(
        &self,
        depth: usize,
        label: String,
        icon: String,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = Self::tokens(cx);
        let k = key(&path);
        let expanded = self.workspace.tree_expanded.contains_key(&k);
        let on = self.sidebar_active_group(cx) == Some("tree")
            && key(&path) == key(&self.workspace.location);
        let full = display_path(&path);
        let right_path = path.clone();
        let chevron_path = path.clone();
        let favorite_label = if self
            .session
            .read(cx)
            .prefs
            .browser_favorites
            .iter()
            .any(|p| key(Path::new(p)) == k)
        {
            Some("从收藏夹移除")
        } else {
            Some("加入收藏夹")
        };
        let indent = 4. + depth.min(6) as f32 * 12.;
        let row_width = DIRECTORY_SIDEBAR_WIDTH - 24.;
        div()
            .id(SharedString::from(format!("sidebar-tree-{k}")))
            .w(px(row_width))
            .h(px(28.))
            .flex_shrink_0()
            .rounded_sm()
            .when(on, |v| v.bg(t.row_selected))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
            })
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, _, cx| {
                this.workspace.sidebar_tree_target = Some(right_path.clone());
                cx.stop_propagation();
            }))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.workspace.sidebar_source = Some(("tree", key(&path)));
                this.workspace_navigate(path.clone(), ViewMode::Directory, None, true, cx);
            }))
            .context_menu(self.sidebar_context_menu(favorite_label))
            .child(
                h_flex()
                    .w_full()
                    .h_full()
                    .items_center()
                    .child(div().w(px(indent)).flex_shrink_0())
                    .child(
                        Button::new(SharedString::from(format!("sidebar-tree-chevron-{k}")))
                            .ghost()
                            .w(px(16.))
                            .h(px(24.))
                            .px_0()
                            .flex_shrink_0()
                            .icon(Icon::new(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            }).size_3())
                            .accessibility_label(if expanded { "折叠文件夹" } else { "展开文件夹" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_tree_node(chevron_path.clone(), cx);
                            })),
                    )
                    .child(
                        h_flex()
                            .w(px(18.))
                            .flex_shrink_0()
                            .justify_center()
                            .child(Icon::default().path(format!("icons/{icon}.svg")).size_4()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .pl_1()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .child(label),
                    )
                    .child(div().w(px(8.)).flex_shrink_0()),
            )
            .into_any_element()
    }

    fn sidebar_tree_children(
        &self,
        parent: &Path,
        depth: usize,
        rows: &mut Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) {
        if depth >= 8 || !self.workspace.tree_expanded.contains_key(&key(parent)) {
            return;
        }
        let Some(children) = self.workspace.tree_children.get(&key(parent)).cloned() else {
            return;
        };
        if children.is_empty() {
            rows.push(
                div()
                    .pl(px(20. + depth.min(6) as f32 * 12.))
                    .py_0p5()
                    .text_xs()
                    .text_color(Self::tokens(cx).text_tertiary)
                    .child("（空文件夹）")
                    .into_any_element(),
            );
            return;
        }
        for (label, path) in children {
            rows.push(self.sidebar_tree_item(
                depth + 1,
                label,
                "folder".into(),
                path.clone(),
                cx,
            ));
            self.sidebar_tree_children(&path, depth + 1, rows, cx);
        }
    }

    fn render_tree_group(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = Self::tokens(cx);
        let collapsed = self.workspace.collapsed_groups.contains("tree");
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\"));
        let roots = { self.session.read(cx).available_roots.clone() };
        let mut items = vec![("主目录".to_string(), "house".to_string(), home)];
        for drive in roots {
            items.push((drive.clone(), "hard-drive".to_string(), PathBuf::from(drive)));
        }
        let mut rows: Vec<AnyElement> = Vec::new();
        for (label, icon, path) in items {
            rows.push(self.sidebar_tree_item(0, label, icon, path.clone(), cx));
            self.sidebar_tree_children(&path, 0, &mut rows, cx);
        }
        v_flex()
            .w_full()
            .py_1()
            .border_b_1()
            .border_color(t.border_soft)
            .child(self.sidebar_header("tree", "此电脑", false, cx))
            .when(!collapsed, |v| v.child(v_flex().w_full().px_1().children(rows)))
            .into_any_element()
    }

    fn render_locations(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Self::tokens(cx);
        let (fav_paths, recent_paths) = {
            let prefs = &self.session.read(cx).prefs;
            (prefs.browser_favorites.clone(), prefs.browser_recent.clone())
        };
        let path_items = |paths: &Vec<String>, icon: &str, label: Option<&'static str>| {
            paths
                .iter()
                .take(8)
                .map(|p| {
                    let path = PathBuf::from(p);
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| display_path(&path));
                    (name, icon.to_string(), path, label)
                })
                .collect::<Vec<_>>()
        };
        // Windows known folders are fixed favorites, not a separate "位置" group.
        let mut favorites: Vec<(String, String, PathBuf, Option<&'static str>)> = self
            .workspace
            .locations
            .iter()
            .map(|(name, icon, path)| {
                ((*name).to_string(), (*icon).to_string(), path.clone(), None)
            })
            .collect();
        for item in path_items(&fav_paths, "folder", Some("从收藏夹移除")) {
            if favorites.iter().any(|(_, _, p, _)| key(p) == key(&item.2)) {
                continue;
            }
            favorites.push(item);
        }
        let recent = path_items(&recent_paths, "folder-clock", None);
        v_flex()
            .w(px(DIRECTORY_SIDEBAR_WIDTH))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .bg(t.surface)
            .border_r_1()
            .border_color(t.border)
            .child(
                v_flex()
                    .id("browser-location-groups")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .px_2()
                    .child(self.render_location_group("favorites", "收藏夹", favorites, cx))
                    .child(self.render_tree_group(cx))
                    .child(self.render_location_group("recent", "最近使用", recent, cx)),
            )
    }

    fn render_usage_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        h_flex()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .h(px(40.))
            .flex_shrink_0()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(tokens.border)
            .bg(tokens.bg)
            .child(self.navigation_buttons(cx))
            .child(
                Button::new("usage-home")
                    .with_size(gpui_kit::component::Size::XSmall)
                    .ghost()
                    .icon(Icon::default().path("icons/house.svg"))
                    .w(px(28.)).h(px(24.)).px_0().flex_shrink_0()
                    .selected(is_home_path(&self.workspace.location))
                    .accessibility_label("全部磁盘")
                    .tooltip("全部磁盘全览：各盘索引逻辑合计，点击磁盘进入")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !is_home_path(&this.workspace.location) {
                            this.workspace_navigate(
                                PathBuf::from(HOME_PATH),
                                ViewMode::Usage,
                                None,
                                true,
                                cx,
                            );
                        }
                    })),
            )
            .child(
                Button::new("usage-index-source")
                    .with_size(gpui_kit::component::Size::XSmall)
                    .ghost()
                    .icon(Icon::default().path("icons/database.svg"))
                    .w(px(28.)).h(px(24.)).px_0().flex_shrink_0()
                    .accessibility_label("索引逻辑")
                    .tooltip(if self.workspace.engine_usage {
                        "索引逻辑大小，非实际占用；不会更新或重启引擎"
                    } else {
                        "当前为本地实测；选择索引逻辑，不更新或重启引擎"
                    })
                    .selected(self.workspace.engine_usage)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_engine_usage_source(cx);
                    })),
            )
            // Async work never inserts/replaces a navigation slot. Paging is
            // permanently reserved; the single cancel control lives on the right.
            .child(
                Button::new("usage-index-next")
                    .with_size(gpui_kit::component::Size::XSmall)
                    .ghost()
                    .icon(Icon::default().path("icons/list-plus.svg"))
                    .w(px(28.)).h(px(24.)).px_0().flex_shrink_0()
                    .accessibility_label("下一页")
                    .tooltip("读取下一页索引子项；无后续页或读取中时不可用")
                    .disabled(!self.workspace.engine_usage || self.workspace.engine_busy
                        || self.workspace.engine_next.is_none())
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.workspace.engine_usage && !this.workspace.engine_busy
                            && this.workspace.engine_next.is_some() {
                            this.load_engine_usage(true, cx);
                        }
                    })),
            )
            .child(
                div()
                    .mx_1()
                    .h(px(16.))
                    .border_r_1()
                    .border_color(tokens.border),
            )
            .child(
                h_flex().id("usage-depth-controls").flex_shrink_0().gap(px(2.))
                .child(
                    Button::new("usage-depth-down")
                        .flex_shrink_0()
                        .ghost()
                        .icon(IconName::Minus)
                        .w(px(22.))
                        .h(px(24.))
                        .px_0()
                        .accessibility_label("减少显示层数")
                        .tooltip("减少显示层数；最少 1 层")
                        .disabled(self.workspace.usage_depth <= 1)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.workspace.scanning && !this.usage_scene_interactive(){return;}
                            this.adjust_usage_depth(-1,cx);
                        })),
                )
                .child(
                    div()
                        .id("usage-depth-value")
                        .text_xs()
                        .w(px(16.)).flex_shrink_0().text_center().whitespace_nowrap()
                        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("显示层数（1–6）").build(window, cx))
                        .child(self.workspace.usage_depth.to_string()),
                )
                .child(
                    Button::new("usage-depth-up")
                        .flex_shrink_0()
                        .ghost()
                        .icon(IconName::Plus)
                        .w(px(22.))
                        .h(px(24.))
                        .px_0()
                        .accessibility_label("增加显示层数")
                        .disabled(self.workspace.usage_depth >= 6)
                        .tooltip("显示更深一级；按需读取索引元数据，不重新扫描")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.workspace.scanning && !this.usage_scene_interactive(){return;}
                            this.adjust_usage_depth(1,cx);
                        })),
                )
            )
            .child(
                Button::new("usage-reset-compact")
                    .with_size(gpui_kit::component::Size::XSmall)
                    .px_2()
                    .text_size(px(12.))
                    .ghost()
                    .h(px(24.))
                    .w(px(64.)).flex_shrink_0()
                    .label(format!("{:.0}%", self.workspace.usage_camera.scale * 100.))
                    .tooltip(
                        "滚轮以指针缩放；边界再滚进入/返回；Shift 横移，Ctrl 平移，0 复位，不重扫",
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.workspace.scanning && !this.usage_scene_interactive(){return;}
                        this.workspace.usage_camera.reset();
                        this.workspace.usage_transition = None;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(120.))
                    .overflow_hidden()
                    .child(self.render_address(cx)),
            )
            .child(
                Button::new("scan-current-directory")
                    .icon(Icon::default().path("icons/scan.svg"))
                    .primary()
                    .flex_shrink_0()
                    .h(px(28.))
                    .w(px(104.))
                    .label("本地实测")
                    .disabled(self.workspace.scanning || self.workspace.op_busy
                        || is_home_path(&self.workspace.location))
                    .tooltip(if is_home_path(&self.workspace.location) {
                        "全览为虚拟范围，不可实测；双击进入磁盘后再测"
                    } else {
                        "本地实测当前目录"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.start_workspace_scan(this.scan_node().is_some(), cx)
                    })),
            )
            .child(
                Button::new("cancel-workspace-scan")
                    .ghost()
                    .icon(IconName::Close)
                    .w(px(28.)).h(px(28.)).px_0().flex_shrink_0()
                    .accessibility_label("取消当前占用任务")
                    .disabled(!(self.workspace.scanning || self.workspace.engine_busy
                        || self.workspace.usage_pending_from.is_some() || self.workspace.usage_depth_pending))
                    .tooltip("取消当前目录读取或实测；保留已发布画面，不影响引擎监听")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.workspace.engine_usage {
                            this.cancel_engine_usage(cx);
                        } else if this.workspace.scanning {
                            this.workspace.scan_cancel.store(true, Ordering::Release);
                            this.workspace.scan_status = "正在取消；已测部分将保留".into();
                            cx.notify();
                        }
                    })),
            )
    }

    pub(super) fn render_workspace_usage(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let (root,selected)=crate::usage_view::presented_scope(
            self.workspace.usage_scene.as_ref().map(|scene|(scene.key.root,self.workspace.usage_scene_selected)),
            self.scan_node(),
            self.workspace.usage_focus.as_ref().and_then(|p|self.workspace.scan_lookup.get(&key(Path::new(p)))).copied(),
        );
        let map = self.render_cached_usage_map(cx);
        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(22.))
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .px_2()
                    .gap_2()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(tokens.text_secondary)
                    .children(
                        crate::disktree::classify::Category::LEGEND
                            .into_iter().chain(std::iter::once(crate::disktree::classify::Category::Other))
                            .map(|class| {
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .w(px(8.))
                                            .h(px(8.))
                                            .rounded_sm()
                                            .bg(tokens.usage_accent(class)),
                                    )
                                    .child(FindxTokens::usage_class_label(class))
                            }),
                    )
                    .child(div().flex_1())
                    .child("滚轮缩放 / 层级"),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .w_full()
                    .overflow_hidden()
                    .items_stretch()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .child(map),
                    )
                    .child(self.render_usage_details(selected, root, cx)),
            )
    }

    pub(super) fn usage_empty(
        &self,
        label: &str,
        tokens: FindxTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let retry = label == crate::indexed_usage::GENERATION_RESTART_MESSAGE;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(
                div()
                    .h(px(20.))
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .child(label.to_string()),
            )
            .child(
                div().h(px(32.)).flex_shrink_0().when(retry, |slot| {
                    slot.child(
                        Button::new("usage-generation-retry")
                            .w(px(72.))
                            .h(px(32.))
                            .flex_shrink_0()
                            .label("重试")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load_engine_usage(false, cx);
                            })),
                    )
                }),
            )
    }

    fn render_usage_details(
        &self,
        selected: Option<usize>,
        root: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Self::tokens(cx);
        const INSPECTOR: f32 = 280.;
        let inspector_scope=self.usage_inspector_key();
        let mut panel = v_flex()
            .id("usage-detail-panel")
            .overflow_hidden()
            .w(px(INSPECTOR))
            .min_w(px(INSPECTOR))
            .max_w(px(INSPECTOR))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .border_l_1()
            .border_color(tokens.border)
            .bg(tokens.surface);
        let watcher_detail=self.session.read(cx).engine_footer.last.as_ref().and_then(|s|s.watch_error.clone());
        let source_label=if self.workspace.inspector.pending.is_some(){"读取详情…"}else{crate::usage_presentation::source_label(self.workspace.engine_usage,watcher_detail.as_deref())};
        let mut head = v_flex()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_1()
            .child(h_flex().w_full().h(px(26.)).min_h(px(26.)).justify_between().gap_1()
                .child(div().flex_1().min_w_0().text_xs().text_ellipsis().whitespace_nowrap().text_color(tokens.text_secondary).child(source_label))
                .child(Popover::new("usage-facts-popover").anchor(Anchor::RightCenter).w(px(348.))
                    .open(self.workspace.usage_facts_open)
                    .on_open_change({let owner=cx.weak_entity();move|open,_,cx|{let _=owner.update(cx,|this,cx|{this.workspace.usage_facts_open=*open;cx.notify();});}})
                    .trigger(Button::new("usage-quality-summary").ghost().w(px(100.)).h(px(24.)).px_1()
                        .label(if self.usage_render_scan().is_some_and(|s|s.unknown>0||s.partial||s.errors>0)||watcher_detail.is_some(){"详情  待核对"}else{"详细信息"})
                        .accessibility_label("详细信息")
                        // The framework's mouse-down wrapper alone exposes no UIA
                        // Invoke. Keep one normal button activation path for pointer,
                        // keyboard and assistive technology; suppress double toggle.
                        .on_mouse_down(MouseButton::Left,|_,_,cx|cx.stop_propagation())
                        .on_click(cx.listener(|this,_,_,cx|{this.workspace.usage_facts_open=!this.workspace.usage_facts_open;cx.notify();}))
                        .text_color(if self.usage_render_scan().is_some_and(|s|s.unknown>0||s.partial||s.errors>0)||watcher_detail.is_some(){tokens.warning}else{tokens.text_secondary})
                        .tooltip("记录数、写入时间与范围质量；按 Esc 关闭"))
                    .content({let owner=cx.weak_entity();move|_,_,cx|{
                        let close_owner=owner.clone();
                        div().id("usage-facts-body").child(h_flex().w_full().justify_between().px_3().pt_2().child(div().text_xs().child("详细信息")).child(Button::new("usage-facts-close").ghost().icon(Icon::default().path("icons/x.svg")).accessibility_label("关闭详细信息").tooltip("关闭详细信息（Esc）").on_click(move|_,_,cx|{let _=close_owner.update(cx,|this,cx|{this.workspace.usage_facts_open=false;cx.notify();});}))).child(owner.upgrade().map(|owner|owner.read(cx).render_usage_facts(cx)).unwrap_or_else(||div().child("详情暂不可用").into_any_element())).into_any_element()
                    }})));
        if self.workspace.scanning {
            let preview = self.workspace.usage_preview.borrow();
            head = head
                .child(
                    div()
                        .text_lg()
                        .child(format!("扫描预览  {}", preview.scope_name)),
                )
                .child(div().text_xs().child("仅已测部分，总量未知"))
                .child(
                    div()
                        .text_xs()
                        .child("双击目录深入 / 向上返回；文件操作待扫描结束"),
                );
        }
        let mut mid = v_flex()
            .id("usage-detail-scroll")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .px_3()
            .gap_1()
            .overflow_y_scrollbar();
        let mut foot = v_flex()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_1()
            .h(px(126.)).min_h(px(126.)).max_h(px(126.))
            .border_t_1()
            .border_color(tokens.border);
        if self.workspace.scanning {
            use std::sync::atomic::Ordering;
            let progress = &self.workspace.progress;
            let preview = self.workspace.usage_preview.borrow();
            mid = mid
                .child(div().text_2xl().child(disk_usage::format_bytes(
                    progress.allocated.load(Ordering::Relaxed),
                )))
                .child(div().text_xs().child(format!(
                    "{} 条路径  {} 目录",
                    progress.entries.load(Ordering::Relaxed),
                    progress.directories.load(Ordering::Relaxed)
                )))
                .child(div().text_xs().child(if preview.coarsened {
                    "预览按资源预算合并；不影响实际扫描"
                } else {
                    "层级与面积仅表示目前已测部分"
                }));
            if let Some((name, bytes)) = preview.selection_detail() {
                mid = mid
                    .child(div().mt_3().text_sm().child(name))
                    .child(div().text_xs().child(format!(
                        "已测 {}，未完成",
                        disk_usage::format_bytes(bytes)
                    )));
            }
            foot = foot.child(
                div()
                    .text_xs()
                    .child("扫描中不提供清理操作；未知占用不是 0"),
            );
            return panel.child(head).child(mid).child(foot).into_any_element();
        }
        if let Some((scan,index,visible_root)) = self.inspector_data(selected,root) {
            let root=Some(visible_root);
            let node = &scan.nodes[index];
            let name = home_display_name(&node.path);
            let absolute_path=if is_home_path(&node.path) {
                HOME_LABEL.into()
            } else {
                display_path(&node.path)
            };
            let total = root.map(|i| scan.weight(&scan.nodes[i])).unwrap_or(0);
            let ratio = if total > 0 {
                scan.weight(node) as f64 / total as f64 * 100.
            } else {
                0.
            };
            head = head
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(name),
                )
                .child(div().id("usage-object-path").h(px(17.)).min_h(px(17.)).text_xs().text_color(tokens.text_secondary).text_ellipsis().whitespace_nowrap().overflow_hidden().child(absolute_path.clone()).tooltip(move|window,cx|gpui_kit::component::tooltip::Tooltip::new(absolute_path.clone()).build(window,cx)))
                .child(
                    div()
                        .text_2xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(disk_usage::format_bytes(scan.weight(node))),
                )
                .child(div().w_full().h(px(4.)).rounded_sm().bg(tokens.border_soft).child(div().h_full().w(relative((ratio/100.).clamp(0.,1.) as f32)).bg(tokens.accent)))
                .child(div().text_xs().text_color(tokens.text_secondary).child(
                    if Some(index)==root {"当前范围 100%".into()}
                    else if ratio<=100. {format!("占当前范围 {ratio:.1}%")}
                    else {"索引汇总暂不能核对比例".into()},
                ))
                .child(
                    h_flex()
                        .w_full()
                        .gap_1()
                        .mt_1()
                        .child(
                            Button::new("usage-scope-search")
                                .ghost()
                                .icon(IconName::Search)
                                .w(px(28.))
                                .h(px(26.))
                                .px_0()
                                .tooltip("在此范围搜索")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.inspector_action(inspector_scope,0,Some(window),cx);
                                })),
                        )
                        .child(
                            Button::new("usage-locate")
                                .ghost()
                                .icon(IconName::FolderOpen)
                                .w(px(28.))
                                .h(px(26.))
                                .px_0()
                                .tooltip("在文件中定位")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.inspector_action(inspector_scope,1,None,cx);
                                })),
                        )
                        .when(node.directory, |v| {
                            v.child(
                                Button::new("usage-enter")
                                    .ghost()
                                    .icon(IconName::ArrowRight)
                                    .w(px(28.))
                                    .h(px(26.))
                                    .px_0()
                                    .tooltip("进入此目录")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.inspector_action(inspector_scope,2,None,cx);
                                    })),
                            )
                        }),
                );
            if let Some(scene) = &self.workspace.usage_scene {
                if Some(index)==root && !scene.worth_a_look.is_empty() {
                    mid = mid
                        .child(
                            div()
                                .mt_2()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("值得一看"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(tokens.text_tertiary)
                                .child("规则候选，不是可释放量或删除建议"),
                        );
                    for (name, reason, bytes) in scene.worth_a_look.iter().take(5) {
                        mid = mid.child(
                            h_flex()
                                .w_full()
                                .text_xs()
                                .gap_1()
                                .child(div().flex_1().min_w_0().text_ellipsis().child(name.clone()))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_color(tokens.text_secondary)
                                        .child(reason.clone()),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .child(disk_usage::format_bytes(*bytes)),
                                ),
                        );
                    }
                }
            }
            let mut children=node.children.clone();
            children.sort_by(|a,b|scan.weight(&scan.nodes[*b]).cmp(&scan.weight(&scan.nodes[*a])).then_with(||scan.nodes[*a].path.cmp(&scan.nodes[*b].path)));
            let count = children.len();
            let inspector_issue=self.workspace.inspector.displayed.as_ref().and_then(|s|s.issue.clone());
            let inspector_partial=self.workspace.inspector.displayed.as_ref().is_some_and(|s|s.partial);
            if node.directory && count==0 {
                let status=inspector_issue.clone().unwrap_or_else(||if inspector_partial{"子项列表受限".into()}else if self.workspace.engine_usage{"索引未列出子项".into()}else{"没有已测子项".into()});
                mid=mid.child(div().id("usage-children-status").mt_2().h(px(20.)).min_h(px(20.)).text_xs().text_color(tokens.text_secondary).text_ellipsis().whitespace_nowrap().child(status.clone()).tooltip(move|window,cx|gpui_kit::component::tooltip::Tooltip::new(status.clone()).build(window,cx)));
            }
            if count > 0 {
                mid = mid.child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(tokens.text_secondary)
                        .child(format!(
                            "较大子项 {count} 项（{}）",
                            if inspector_partial {"已载入列表"}else if self.workspace.engine_usage {"逻辑大小"}else{"实际占用"}
                        )),
                );
                mid = mid.child(
                    v_flex()
                        .id("usage-details-children")
                        .gap_1()
                        .overflow_y_scrollbar()
                        .children(children.iter().copied().take(12).map(|child| {
                            let n = &scan.nodes[child];
                            let label = n
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned();
                            h_flex()
                                .id(SharedString::from(format!("usage-detail-{child}")))
                                .w_full()
                                .px_1()
                                .py_1()
                                .gap_2()
                                .cursor_pointer()
                                .rounded_sm()
                                .hover(|s| s.bg(tokens.hover))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(tokens.text_secondary)
                                        .child(disk_usage::format_bytes(scan.weight(n))),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.inspector_pick(inspector_scope,child,false,cx);
                                }))
                                .on_double_click(cx.listener(move |this, _, _, cx| {
                                    this.inspector_pick(inspector_scope,child,true,cx);
                                }))
                        }))
                        .when(count > 12, |view| {
                            view.child(
                                div()
                                    .text_xs()
                                    .text_color(tokens.text_tertiary)
                                    .child(if inspector_partial{"列已载入项中最大 12 项；双击目录深入"}else{"仅列最大 12 项；双击目录继续查看"}),
                            )
                        }),
                );
            }
            // At home the foot follows the selected volume tile; the virtual
            // root itself has no OS capacity.
            let volume_path= {
                let root_path=&scan.nodes[root.unwrap_or(0)].path;
                if is_home_path(root_path) {
                    selected.and_then(|i|scan.nodes.get(i)).filter(|n|n.parent==Some(root.unwrap_or(0))).map(|n|&n.path).unwrap_or(root_path)
                } else { root_path }
            };
            let cached=self.workspace.usage_volume.get(volume_path);
            let value=match cached{Some(Ok(v))=>Some(v),Some(Err(_))=>None,None=>scan.volume.as_ref()};
            foot=foot.child(div().h(px(18.)).min_h(px(18.)).text_xs().font_weight(FontWeight::SEMIBOLD).text_ellipsis().whitespace_nowrap().overflow_hidden().child(value.map(|v|format!("系统卷容量  {}",display_path(&v.root))).unwrap_or_else(||"系统卷容量".into())));
            let volume_ratio=value.map_or(0.,|v|v.used() as f64/v.total.max(1) as f64) as f32;
            foot=foot.child(div().w_full().h(px(5.)).rounded_sm().bg(tokens.border_soft).child(div().h_full().w(relative(volume_ratio.clamp(0.,1.))).bg(tokens.accent)));
            let mut values=h_flex().w_full().gap_2();
            for (label,number) in [("总量",value.map(|v|v.total)),("已用",value.map(|v|v.used())),("剩余",value.map(|v|v.free))] {
                values=values.child(v_flex().flex_1().min_w_0().child(div().text_xs().text_color(tokens.text_secondary).child(label)).child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(number.map(disk_usage::format_bytes).unwrap_or_else(||"—".into()))));
            }
            foot=foot.child(values);
            let explanation=if let Some(Err(_))=cached{"系统未返回容量；不是 0，也不由目录合计推算".to_string()}
            else if let Some(v)=value{if let Some(age)=self.workspace.usage_volume.age(volume_path){format!("{} 秒前采样  账户可用 {}",age.as_secs(),disk_usage::format_bytes(v.available))}else{"扫描时容量，独立刷新中".into()}}
            else if self.workspace.usage_volume.slow(volume_path){"系统响应较慢；保持界面可用".into()}
            else {"正在读取系统容量；与目录大小口径不同".into()};
            let explanation_detail=match cached{Some(Err(e))=>e.clone(),_=>"卷已用 = 总量 − 系统空闲；不等于目录逻辑合计，不能推算可清理量。索引时效见连接状态。".into()};
            foot=foot.child(div().id("usage-volume-explanation").h(px(30.)).overflow_hidden().text_xs().text_color(tokens.text_tertiary).child(explanation).tooltip(move|window,cx|gpui_kit::component::tooltip::Tooltip::new(explanation_detail.clone()).build(window,cx)));
            panel = panel.child(head).child(mid).child(foot);
        } else {
            panel = panel
                .child(div().px_3().py_2().text_sm().child("选中项详情"))
                .child(
                    div()
                        .text_xs()
                        .text_color(tokens.text_secondary)
                        .child("扫描后选择图块。未知占用不是 0，也不提供清理判断。"),
                );
        }
        panel.into_any_element()
    }

    fn render_usage_facts(&self,cx:&App)->AnyElement {
        let tokens=Self::tokens(cx);
        let mut body=v_flex().id("usage-facts-scroll").w_full().max_h(px(300.)).overflow_y_scrollbar().p_3().gap_2().text_xs();
        let (root,selected)=crate::usage_view::presented_scope(self.workspace.usage_scene.as_ref().map(|s|(s.key.root,self.workspace.usage_scene_selected)),self.scan_node(),self.workspace.usage_focus.as_ref().and_then(|p|self.workspace.scan_lookup.get(&key(Path::new(p)))).copied());
        let Some((scan,selected,_root))=self.inspector_data(selected,root) else{return body.child("尚无已发布的目录数据").into_any_element()};
        let Some(facts)=crate::usage_presentation::inspector_facts(&scan,Some(selected)) else{return body.child("请选择目录或文件").into_any_element()};
        let node=facts.selected;
        let section=|title:&str|div().font_weight(FontWeight::SEMIBOLD).text_color(tokens.text_primary).child(title.to_owned());
        let row=|label:&str,value:String|h_flex().w_full().gap_2().items_start()
            .child(div().w(px(76.)).flex_shrink_0().text_color(tokens.text_secondary).child(label.to_owned()))
            .child(div().flex_1().min_w_0().child(value));
        let list_status=self.workspace.inspector.displayed.as_ref().map(|s|s.issue.clone().unwrap_or_else(||if s.partial{"当前有界列表不完整".into()}else if node.directory{format!("{} 个直接子项",node.children.len())}else{"文件不含子项".into()})).unwrap_or_else(||"尚未读取".into());
        body=body.child(section("当前对象"))
            .child(row("路径",display_path(&node.path)))
            .child(row("文件记录",node.files.to_string()))
            .child(row("目录记录",node.directories.to_string()))
            .child(row("写入时间",if node.mtime>0{search_logic::format_time(node.mtime)}else{"未记录".into()}))
            .child(row("子项",list_status))
            .when(node.hardlink_duplicate,|b|b.child(div().text_color(tokens.warning).child("硬链接别名；空间计入另一路径")))
            .child(div().h(px(1.)).bg(tokens.border))
            .child(section(if scan.logical_generation.is_some(){"索引统计范围"}else{"扫描统计范围"}))
            .child(row("范围",display_path(facts.statistics_root)))
            .child(div().text_color(if scan.partial||scan.unknown>0||scan.errors>0{tokens.warning}else{tokens.text_secondary}).child(self.usage_warning_line(&scan)))
            .child(div().text_color(tokens.text_tertiary).child("待验证数属于上述统计范围，不是选中项计数"))
            .child(div().h(px(1.)).bg(tokens.border))
            .child(section("口径与连接"))
            .child(div().text_color(tokens.text_secondary).child(if self.workspace.engine_usage{"目录显示索引逻辑大小，不等于磁盘实占；系统卷容量独立采样"}else{"目录显示本地已测占用；系统卷容量独立采样"}));
        if let Some(error)=self.session.read(cx).engine_footer.last.as_ref().and_then(|s|s.watch_error.as_ref()){
            body=body.child(row("全局连接",error.clone())).child(div().text_color(tokens.text_tertiary).child("全局诊断可能涉及其他卷，不代表选中对象的独立时效"));
        }
        body.child(row("索引标识",format!("{:?}",scan.logical_generation)))
            .when(!self.workspace.scan_status.is_empty(),|b|b.child(row("读取状态",self.workspace.scan_status.clone())))
            .into_any_element()
    }

    fn usage_warning_line(&self, scan: &disk_usage::Scan) -> String {
        if scan.logical_generation.is_some() {
            let mut text=if scan.unknown>0{format!("{} 项元数据待验证",scan.unknown)}else{"索引数据已载入".into()};
            if scan.partial{text.push_str("；部分未确认");}
            return text;
        }
        let mut parts = Vec::new();
        if scan.is_legacy_entry_truncation() {
            parts.push("旧的条目或时间上限截断结果，请手动重新扫描".into());
        }
        if scan.memory_limit_reached {
            parts.push(format!(
                "达到内存预算 {}",
                disk_usage::format_bytes(scan.limits.memory_bytes as u64)
            ));
        }
        if scan.alloc_failed {
            parts.push("系统内存不足".into());
        }
        if scan.cancelled {
            parts.push("已取消".into());
        }
        if scan.errors > 0 {
            parts.push(format!("{} 无法读取", scan.errors));
        }
        if scan.skipped_links > 0 {
            parts.push(format!("{} 个边界未跟随", scan.skipped_links));
        }
        if scan.unknown > 0 {
            parts.push(format!("{} 项占用未知", scan.unknown));
        }
        if parts.is_empty() && scan.partial {
            parts.push("部分结果".into());
        }
        parts.join("；")
    }
}
