//! Three deliberately different information architectures, inside one custom window shell.
use super::*;
use gpui_kit::component::menu::ContextMenuExt as _;

impl MainWindow {
    pub(in super::super) fn render_workspace_page(
        &self,
        sidebar: bool,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.workspace.mode {
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
                                .child(self.render_workspace_center(cx)),
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
                                .child(self.render_workspace_center(cx)),
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
                .w_full()
                .child(self.render_usage_toolbar(cx))
                .child(self.render_workspace_usage(cx))
                .into_any_element(),
        }
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

    pub(super) fn render_directory_table(&self, cx: &mut Context<Self>) -> AnyElement {
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
            .child(
                DataTable::new(&self.workspace.table)
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
            )
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
        h_flex().gap_1().children(
            [
                ("directory-back", "后退 · Alt+Left", 0),
                ("directory-forward", "前进 · Alt+Right", 1),
                ("directory-up", "上一级 · Alt+Up", 2),
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
                        _ => self.workspace.location.parent().is_none(),
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
        if self
            .workspace
            .locations
            .iter()
            .any(|(_, _, p)| key(p) == current)
        {
            return Some("locations");
        }
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
        if home.as_ref().is_some_and(|p| key(p) == current)
            || self
                .session
                .read(cx)
                .available_roots
                .iter()
                .any(|p| key(Path::new(p)) == current)
        {
            return Some("user");
        }
        let prefs = &self.session.read(cx).prefs;
        if prefs
            .browser_favorites
            .iter()
            .any(|p| key(Path::new(p)) == current)
        {
            Some("favorites")
        } else if prefs
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
    fn sidebar_item(
        &self,
        group: &'static str,
        index: usize,
        label: String,
        icon: String,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on = self.sidebar_active_group(cx) == Some(group)
            && key(&path) == key(&self.workspace.location);
        let full = display_path(&path);
        // The row background is inset 12px on BOTH edges. Inside: 16px indent,
        // 20px icon slot, a flexible ellipsized text slot, and 16px tail slot.
        let row_width = DIRECTORY_SIDEBAR_WIDTH - 24.;
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
            .on_click(cx.listener(move |this, _, _, cx| {
                this.workspace.sidebar_source = Some((group, key(&path)));
                this.workspace_navigate(path.clone(), ViewMode::Directory, None, true, cx);
            }))
            .into_any_element()
    }
    fn render_location_group(
        &self,
        group: &'static str,
        title: &'static str,
        items: Vec<(String, String, PathBuf)>,
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
                                .map(|(i, (label, icon, path))| {
                                    self.sidebar_item(group, i, label, icon, path, cx)
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
    fn render_locations(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Self::tokens(cx);
        let prefs = &self.session.read(cx).prefs;
        let locations = self
            .workspace
            .locations
            .iter()
            .map(|(name, icon, path)| (name.to_string(), icon.to_string(), path.clone()))
            .collect();
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\"));
        let mut user = vec![("主目录".into(), "house".into(), home)];
        user.extend(
            self.session
                .read(cx)
                .available_roots
                .iter()
                .map(|p| (p.clone(), "hard-drive".into(), PathBuf::from(p))),
        );
        let path_items = |paths: &Vec<String>, icon: &str| {
            paths
                .iter()
                .take(8)
                .map(|p| {
                    let path = PathBuf::from(p);
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| display_path(&path));
                    (name, icon.to_string(), path)
                })
                .collect()
        };
        let favorites = path_items(&prefs.browser_favorites, "folder");
        let recent = path_items(&prefs.browser_recent, "folder-clock");
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
                    .child(self.render_location_group("locations", "位置", locations, cx))
                    .child(self.render_location_group("user", "用户目录", user, cx))
                    .child(self.render_location_group("favorites", "收藏夹", favorites, cx))
                    .child(self.render_location_group("recent", "最近使用", recent, cx)),
            )
    }

    fn render_usage_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        h_flex()
            .w_full()
            .h(px(46.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(tokens.border)
            .bg(tokens.bg)
            .child(self.navigation_buttons(cx))
            .child(self.render_address(cx))
            .child(
                Button::new("scan-current-directory")
                    .icon(Icon::default().path("icons/scan.svg"))
                    .primary()
                    .h(px(30.))
                    .label(if self.workspace.scanning {
                        "扫描中…"
                    } else if self.scan_node().is_some() {
                        "重新扫描"
                    } else {
                        "开始扫描"
                    })
                    .disabled(self.workspace.scanning || self.workspace.op_busy)
                    .on_click(cx.listener(|this, _, _, cx| this.start_workspace_scan(cx))),
            )
            .when(self.workspace.scanning, |view| {
                view.child(
                    Button::new("cancel-workspace-scan")
                        .icon(IconName::Close)
                        .label("取消")
                        .h(px(30.))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.workspace.scan_cancel.store(true, Ordering::Release);
                            this.workspace.scan_status =
                                "正在取消；已测结果将保留为部分扫描".into();
                            cx.notify();
                        })),
                )
            })
    }

    pub(super) fn render_workspace_usage(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let selected = self.active_workspace_hit().and_then(|hit| {
            self.workspace
                .scan_lookup
                .get(&key(Path::new(&hit.path)))
                .copied()
        });
        let root = self.scan_node();
        let map = self.render_cached_usage_map(cx);
        let summary = if let (Some(scan), Some(root)) = (self.workspace.scan.as_ref(), root) {
            let node = &scan.nodes[root];
            format!(
                "{}已测 {} · 逻辑 {} · {} 文件 / {} 目录 · {} 秒{}",
                if self.workspace.scanning {
                    "上次缓存 · "
                } else if self.workspace.scan_stale {
                    "缓存可能过期 · "
                } else {
                    ""
                },
                disk_usage::format_bytes(node.allocated),
                disk_usage::format_bytes(node.logical),
                node.files,
                node.directories,
                scan.elapsed.as_secs(),
                if scan.partial {
                    "   ·   部分结果"
                } else {
                    ""
                }
            )
        } else {
            "只读目录测量 · 不读取另一套全盘索引".into()
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(
                h_flex()
                    .h(px(36.))
                    .px_3()
                    .gap_3()
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .child(div().flex_1().min_w_0().text_ellipsis().child(summary))
                    .child(div().text_xs().child("单击选择 · 双击目录钻取")),
            )
            .child(
                h_flex().h(px(32.)).px_3().gap_1().flex_shrink_0().text_xs()
                    .child(Button::new("usage-depth-down").ghost().icon(IconName::Minus).w(px(24.)).h(px(24.)).px_0()
                        .accessibility_label("减少显示层数").disabled(self.workspace.usage_depth <= 1)
                        .on_click(cx.listener(|this, _, _, cx| {this.workspace.usage_depth = this.workspace.usage_depth.saturating_sub(1).max(1);cx.notify();})))
                    .child(div().px_1().child(format!("显示 {} 层",self.workspace.usage_depth)))
                    .child(Button::new("usage-depth-up").ghost().icon(IconName::Plus).w(px(24.)).h(px(24.)).px_0()
                        .accessibility_label("增加显示层数").disabled(self.workspace.usage_depth >= 6)
                        .tooltip("只改变图中显示层数，不限制扫描深度，不重新扫描")
                        .on_click(cx.listener(|this, _, _, cx| {this.workspace.usage_depth = (this.workspace.usage_depth+1).min(6);cx.notify();})))
                    .child(div().mx_2().h(px(16.)).border_r_1().border_color(tokens.border))
                    .child(Button::new("usage-zoom-out").ghost().icon(IconName::Minus).w(px(24.)).h(px(24.)).px_0()
                        .accessibility_label("缩小占用图").on_click(cx.listener(|this,_,_,cx|this.zoom_usage_center(1./1.15,cx))))
                    .child(Button::new("usage-reset-camera").ghost().icon(IconName::Maximize).h(px(24.)).label(format!("{:.0}%",self.workspace.usage_camera.scale*100.))
                        .accessibility_label("复位占用图缩放").tooltip("滚轮围绕指针缩放；Shift+滚轮平移；+ / - 缩放，0 复位。不重新扫描")
                        .on_click(cx.listener(|this,_,_,cx|{this.workspace.usage_camera.reset();this.workspace.usage_transition=None;cx.notify();})))
                    .child(Button::new("usage-zoom-in").ghost().icon(IconName::Plus).w(px(24.)).h(px(24.)).px_0()
                        .accessibility_label("放大占用图").on_click(cx.listener(|this,_,_,cx|this.zoom_usage_center(1.15,cx))))
                    .child(Button::new("usage-reduce-motion").ghost().icon(IconName::Minus).h(px(24.)).label("减少动态")
                        .selected(self.session.read(cx).prefs.usage_reduce_motion)
                        .on_click(cx.listener(|this,_,_,cx| {
                            this.workspace.usage_transition=None;
                            let prefs=this.session.update(cx,|session,cx| {session.prefs.usage_reduce_motion=!session.prefs.usage_reduce_motion;cx.notify();session.prefs.clone()});
                            crate::session::persist_prefs(prefs,cx);cx.notify();
                        })))
                    .child(div().flex_1().min_w_0())
                    .child(Button::new("usage-scan-budget").ghost().icon(IconName::Settings).h(px(26.)).label(format!("上限 {}",Limits::profile(self.workspace.usage_limit_profile).label()))
                        .tooltip("手动选择下一次扫描的资源上限；较高上限可能占用大量内存，不会自动开始扫描")
                        .disabled(self.workspace.scanning)
                        .on_click(cx.listener(|this,_,_,cx|{this.workspace.usage_limit_profile=(this.workspace.usage_limit_profile+1)%3;cx.notify();}))),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .items_stretch()
                    .child(map)
                    .child(self.render_usage_details(selected, root, cx)),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(tokens.border)
                    .text_xs()
                    .text_color(tokens.text_secondary)
                    .children(
                        [
                            "目录 / 其他",
                            "文档",
                            "媒体",
                            "代码",
                            "压缩包",
                            "应用数据",
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(i, label)| {
                            h_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .w(px(9.))
                                        .h(px(9.))
                                        .rounded_sm()
                                        .bg(tokens.allocation_color(i)),
                                )
                                .child(label)
                        }),
                    )
                    .child(div().flex_1())
                    .child("实际占用 ≠ 可回收量"),
            )
    }

    pub(super) fn usage_empty(&self, label: &str, tokens: FindxTokens) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_6()
            .text_sm()
            .text_color(tokens.text_secondary)
            .child(label.to_string())
    }

    fn render_usage_details(
        &self,
        selected: Option<usize>,
        root: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Self::tokens(cx);
        let mut panel = v_flex()
            .id("usage-detail-panel")
            .overflow_y_scrollbar()
            .w(px(if self.last_content_width < 1200. {
                240.
            } else {
                260.
            }))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .p_3()
            .gap_2()
            .border_l_1()
            .border_color(tokens.border)
            .bg(tokens.surface);
        if let (Some(scan), Some(index)) = (self.workspace.scan.as_ref(), selected.or(root)) {
            let node = &scan.nodes[index];
            let name = node
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| display_path(&node.path));
            let total = root.map(|i| scan.nodes[i].allocated).unwrap_or(0);
            let ratio = if total > 0 {
                node.allocated as f64 / total as f64 * 100.
            } else {
                0.
            };
            if let Some(v) = &scan.volume {
                panel=panel.child(div().text_xs().text_color(tokens.text_secondary).child(format!("系统卷 {}（扫描开始时）\n已用 {} · 可用 {}\n容量 {}",display_path(&v.root),disk_usage::format_bytes(v.used()),disk_usage::format_bytes(v.free),disk_usage::format_bytes(v.total))))
                    .child(div().text_xs().text_color(tokens.text_tertiary).child("卷已用包含未扫描目录、系统保留/元数据；受权限、链接边界及扫描时点影响，不应与目录合计直接相减当作可清理量。"));
            }
            if scan.partial {
                panel = panel.child(
                    div()
                        .text_xs()
                        .text_color(tokens.warning)
                        .child(format!("部分结果：{}", scan.partial_reasons())),
                );
            }
            panel = panel
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(tokens.text_secondary)
                        .child("选中项 · 已测实际占用"),
                )
                .child(
                    div()
                        .text_3xl()
                        .child(disk_usage::format_bytes(node.allocated)),
                )
                .child(div().text_xs().text_color(tokens.text_secondary).child(
                    if Some(index) == root {
                        "当前已测范围根；不是卷已用空间".to_string()
                    } else {
                        format!("占当前已测范围 {ratio:.1}%（非扫描覆盖率）")
                    },
                ))
                .child(
                    div()
                        .w_full()
                        .h(px(5.))
                        .rounded_sm()
                        .bg(tokens.border_soft)
                        .child(
                            div()
                                .h_full()
                                .w(relative((ratio / 100.).clamp(0., 1.) as f32))
                                .bg(tokens.accent),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(tokens.text_secondary)
                        .child(format!(
                            "逻辑 {} · {}",
                            disk_usage::format_bytes(node.logical),
                            search_logic::format_time(node.mtime)
                        )),
                )
                .when(node.hardlink_duplicate, |view| {
                    view.child(div().text_xs().text_color(tokens.warning).child(
                        "硬链接别名：实际空间已计入同一物理文件的另一条路径，本项不重复计入。",
                    ))
                })
                .when(node.uncertain, |view| {
                    view.child(div().text_xs().text_color(tokens.warning).child(
                        "本项含未知占用；已测值并不完整，未知不等于 0，不能据此判断可删除空间。",
                    ))
                })
                .child(
                    h_flex()
                        .w_full()
                        .gap_1()
                        .child(
                            Button::new("usage-scope-search")
                                .ghost()
                                .icon(IconName::Search)
                                .w(px(30.))
                                .h(px(28.))
                                .px_0()
                                .tooltip("在此范围搜索")
                                .accessibility_label("在此范围搜索")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let path = this
                                        .active_workspace_hit()
                                        .map(|h| {
                                            if h.is_directory {
                                                PathBuf::from(h.path)
                                            } else {
                                                PathBuf::from(h.path)
                                                    .parent()
                                                    .unwrap_or(Path::new("C:\\"))
                                                    .to_path_buf()
                                            }
                                        })
                                        .unwrap_or_else(|| this.workspace.location.clone());
                                    this.begin_scoped_search(path, window, cx);
                                })),
                        )
                        .child(
                            Button::new("usage-locate")
                                .ghost()
                                .icon(IconName::FolderOpen)
                                .w(px(30.))
                                .h(px(28.))
                                .px_0()
                                .tooltip("在文件中定位")
                                .accessibility_label("在文件中定位")
                                .on_click(cx.listener(|this, _, _, cx| this.workspace_reveal(cx))),
                        )
                        .when(node.directory, |v| {
                            v.child(
                                Button::new("usage-enter")
                                    .ghost()
                                    .icon(IconName::ArrowRight)
                                    .w(px(30.))
                                    .h(px(28.))
                                    .px_0()
                                    .tooltip("进入此目录")
                                    .accessibility_label("进入此目录")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_usage_node(index, true, cx)
                                    })),
                            )
                        })
                        .child(
                            Button::new("usage-file-actions")
                                .ghost()
                                .icon(IconName::Ellipsis)
                                .w(px(30.))
                                .h(px(28.))
                                .px_0()
                                .tooltip("文件操作")
                                .accessibility_label("文件操作")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.show_file_menu(window, cx)
                                })),
                        ),
                )
                .child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(tokens.text_secondary)
                        .child(format!("子项 · {} 项（按实际占用）", node.children.len())),
                );
            let children = &node.children;
            let count = children.len();
            panel = panel.child(
                v_flex()
                    .id("usage-details-children")
                    .max_h(px(220.))
                    .flex_shrink_0()
                    .gap_1()
                    .overflow_y_scrollbar()
                    .children(children.iter().copied().take(80).map(|child| {
                        let n = &scan.nodes[child];
                        let label = n
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        h_flex()
                            .id(SharedString::from(format!("usage-detail-{child}")))
                            .role(gpui_kit::accesskit::Role::Button)
                            .aria_label(format!(
                                "{} · {}",
                                label,
                                disk_usage::format_bytes(n.allocated)
                            ))
                            .aria_description("单击选择；双击目录钻取")
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
                                    .child(disk_usage::format_bytes(n.allocated)),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_usage_node(child, false, cx)
                            }))
                            .on_double_click(cx.listener(move |this, _, _, cx| {
                                this.select_usage_node(child, true, cx)
                            }))
                    }))
                    .when(count > 80, |view| {
                        view.child(
                            div()
                                .text_xs()
                                .text_color(tokens.text_tertiary)
                                .child("仅显示前 80 项；在文件中定位以查看全部"),
                        )
                    }),
            );
            panel = panel.child(
                div()
                    .text_xs()
                    .text_color(tokens.text_tertiary)
                    .child(format!(
                        "硬链接去重 {} · 跳过边界 {}\n读取错误 {} · 未知占用 {}",
                        scan.hardlinks, scan.skipped_links, scan.errors, scan.unknown
                    )),
            );
        } else {
            panel = panel.child(div().text_sm().child("选中项详情")).child(
                div().text_xs().text_color(tokens.text_secondary).child(
                    "扫描后选择图块或目录项。\n不会将未知占用当成 0，也不提供清理或删除判断。",
                ),
            );
        }
        panel.into_any_element()
    }
}
