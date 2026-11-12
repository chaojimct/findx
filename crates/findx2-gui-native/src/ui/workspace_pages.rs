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
                .min_w_0()
                .w_full()
                .overflow_hidden()
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
                Button::new("usage-index-source")
                    .ghost()
                    .h(px(24.))
                    .label(if self.workspace.engine_usage {
                        "索引逻辑"
                    } else if self.workspace.scanning {
                        "本地实测中"
                    } else {
                        "本地实测"
                    })
                    .tooltip("当前数据来源；点击选择索引逻辑。不会更新或重启引擎服务")
                    .selected(self.workspace.engine_usage)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select_engine_usage_source(cx);
                    })),
            )
            .when(
                self.workspace.engine_usage && self.workspace.engine_busy,
                |v| {
                    v.child(
                        Button::new("usage-index-cancel")
                            .ghost()
                            .label("取消")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.workspace.scan_generation += 1;
                                this.workspace.engine_busy = false;
                                this.workspace.engine_checked = false;
                                this.workspace.scan_status = "索引分页已取消；引擎继续监听".into();
                                cx.notify();
                            })),
                    )
                },
            )
            .when(
                self.workspace.engine_usage && self.workspace.engine_next.is_some(),
                |v| {
                    v.child(
                        Button::new("usage-index-next")
                            .ghost()
                            .label("下一页")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.load_engine_usage(true, cx)),
                            ),
                    )
                },
            )
            .child(
                div()
                    .mx_1()
                    .h(px(16.))
                    .border_r_1()
                    .border_color(tokens.border),
            )
            .child(
                Button::new("usage-depth-down")
                    .ghost()
                    .icon(IconName::Minus)
                    .w(px(22.))
                    .h(px(24.))
                    .px_0()
                    .accessibility_label("减少显示层数")
                    .disabled(self.workspace.usage_depth <= 1)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workspace.usage_depth =
                            this.workspace.usage_depth.saturating_sub(1).max(1);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .child(format!("层 {}", self.workspace.usage_depth)),
            )
            .child(
                Button::new("usage-depth-up")
                    .ghost()
                    .icon(IconName::Plus)
                    .w(px(22.))
                    .h(px(24.))
                    .px_0()
                    .accessibility_label("增加显示层数")
                    .disabled(self.workspace.usage_depth >= 6)
                    .tooltip("只改变图中显示层数，不限制扫描深度，不重新扫描")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workspace.usage_depth = (this.workspace.usage_depth + 1).min(6);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("usage-reset-compact")
                    .ghost()
                    .h(px(24.))
                    .label(format!("{:.0}%", self.workspace.usage_camera.scale * 100.))
                    .tooltip(
                        "滚轮以指针缩放；边界再滚进入/返回；Shift 横移，Ctrl 平移，0 复位，不重扫",
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workspace.usage_camera.reset();
                        this.workspace.usage_transition = None;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("usage-advanced-toggle")
                    .ghost()
                    .icon(IconName::Settings)
                    .w(px(24.))
                    .h(px(24.))
                    .px_0()
                    .selected(self.workspace.usage_advanced)
                    .tooltip("扫描预算与减少动态")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.workspace.usage_advanced = !this.workspace.usage_advanced;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(96.))
                    .min_w_0()
                    .overflow_hidden()
                    .child(self.render_address(cx)),
            )
            .child(
                Button::new("scan-current-directory")
                    .icon(Icon::default().path("icons/scan.svg"))
                    .primary()
                    .flex_shrink_0()
                    .h(px(28.))
                    .label(if self.workspace.scanning {
                        "扫描中…"
                    } else if self.scan_node().is_some() {
                        "本地实测"
                    } else {
                        "本地实测"
                    })
                    .disabled(self.workspace.scanning || self.workspace.op_busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.start_workspace_scan(this.scan_node().is_some(), cx)
                    })),
            )
            .when(self.workspace.scanning, |view| {
                view.child(
                    Button::new("cancel-workspace-scan")
                        .icon(IconName::Close)
                        .label("取消")
                        .h(px(28.))
                        .tooltip("停止后续枚举；已测部分会保留，不会当作完整结果")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.workspace.scan_cancel.store(true, Ordering::Release);
                            this.workspace.scan_status = "正在取消；已测部分将保留".into();
                            cx.notify();
                        })),
                )
            })
    }

    pub(super) fn render_workspace_usage(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let selected = self
            .workspace
            .usage_focus
            .as_ref()
            .and_then(|path| self.workspace.scan_lookup.get(&key(Path::new(path))))
            .copied()
            .or_else(|| self.scan_node());
        let root = self.scan_node();
        let map = self.render_cached_usage_map(cx);
        let limits = Limits::memory_profile(self.workspace.usage_memory_profile);
        v_flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .w_full()
            .overflow_hidden()
            .when(self.workspace.usage_advanced, |view| {
                view.child(
                    h_flex()
                        .h(px(28.))
                        .px_2()
                        .gap_2()
                        .flex_shrink_0()
                        .text_xs()
                        .border_b_1()
                        .border_color(tokens.border)
                        .child(
                            Button::new("usage-reduce-motion")
                                .ghost()
                                .h(px(24.))
                                .label("减少动态")
                                .selected(self.session.read(cx).prefs.usage_reduce_motion)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.workspace.usage_transition = None;
                                    let prefs = this.session.update(cx, |session, cx| {
                                        session.prefs.usage_reduce_motion = !session.prefs.usage_reduce_motion;
                                        cx.notify();
                                        session.prefs.clone()
                                    });
                                    crate::session::persist_prefs(prefs, cx);
                                    cx.notify();
                                })),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("usage-scan-budget")
                                .ghost()
                                .h(px(24.))
                                .label(format!("上限 {}", limits.label()))
                                .tooltip("默认遍历到结束或取消。高级选项只改变内存上限（默认 1GiB / 256MiB / 512MiB，含树、lookup 与构建暂存估计，不含 GPUI/GPU 基线），不是条数或分钟档位。超出预算或分配失败会停止并保留已测部分。旧截断快照需手动重扫，不会自动全盘扫描。")
                                .disabled(self.workspace.scanning)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.workspace.usage_memory_profile =
                                        (this.workspace.usage_memory_profile + 1) % 3;
                                    cx.notify();
                                })),
                        ),
                )
            })
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
                            .into_iter()
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
        const INSPECTOR: f32 = 280.;
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
        let mut head = v_flex()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_1()
            .child(div().text_xs().child(if self.workspace.engine_usage {
                "索引逻辑大小（非实际占用）"
            } else {
                "本地实测 · 实际占用"
            }));
        if self.workspace.scanning {
            let preview = self.workspace.usage_preview.borrow();
            head = head
                .child(
                    div()
                        .text_lg()
                        .child(format!("扫描预览 · {}", preview.scope_name)),
                )
                .child(div().text_xs().child("仅已测部分 · 总量未知"))
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
                    "{} 条路径 · {} 目录",
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
                        "已测 {} · 尚未完成",
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
        if let (Some(scan), Some(index)) = (self.workspace.scan.as_ref(), selected.or(root)) {
            let node = &scan.nodes[index];
            let name = node
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| display_path(&node.path));
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
                .child(
                    div()
                        .text_2xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(disk_usage::format_bytes(scan.weight(node))),
                )
                .child(div().w_full().h(px(3.)).rounded_sm().bg(tokens.accent))
                .child(div().text_xs().text_color(tokens.text_secondary).child(
                    if Some(index) == root {
                        format!(
                            "当前范围根 · 逻辑 {} · {} 秒",
                            disk_usage::format_bytes(node.logical),
                            scan.elapsed.as_secs()
                        )
                    } else {
                        format!("占当前范围 {ratio:.1}%")
                    },
                ))
                .child(
                    h_flex()
                        .w_full()
                        .mt_1()
                        .text_xs()
                        .text_color(tokens.text_secondary)
                        .child(div().flex_1().child(format!("文件 {}", node.files)))
                        .child(div().flex_1().child(format!("目录 {}", node.directories))),
                )
                .child(
                    h_flex()
                        .w_full()
                        .text_xs()
                        .text_color(tokens.text_secondary)
                        .child(
                            div()
                                .flex_1()
                                .child(format!("写入 {}", search_logic::format_time(node.mtime))),
                        )
                        .child(div().flex_1().child(if node.directory {
                            "目录"
                        } else {
                            "文件"
                        })),
                )
                .when(node.hardlink_duplicate, |view| {
                    view.child(
                        div()
                            .text_xs()
                            .text_color(tokens.warning)
                            .child("硬链接别名，空间已计入另一路径"),
                    )
                })
                .when(node.uncertain, |view| {
                    view.child(div().text_xs().text_color(tokens.warning).child(
                        if self.workspace.engine_usage {
                            "含未验证逻辑元数据；非实际占用"
                        } else {
                            "含未知占用；未知不是 0"
                        },
                    ))
                })
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
                                .w(px(28.))
                                .h(px(26.))
                                .px_0()
                                .tooltip("在文件中定位")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_usage_node(index, false, cx);
                                    this.workspace_reveal(cx);
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
                                        this.select_usage_node(index, true, cx)
                                    })),
                            )
                        }),
                );
            if let Some(scene) = &self.workspace.usage_scene {
                if !scene.worth_a_look.is_empty() {
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
            let children = &node.children;
            let count = children.len();
            if count > 0 {
                mid = mid.child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(tokens.text_secondary)
                        .child(format!(
                            "子项 · {count}（{}）",
                            if self.workspace.engine_usage {
                                "索引逻辑大小"
                            } else {
                                "按实际占用"
                            }
                        )),
                );
                mid = mid.child(
                    v_flex()
                        .id("usage-details-children")
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
                                    .child("仅显示前 80 项"),
                            )
                        }),
                );
            }
            if scan.partial
                || scan.errors > 0
                || scan.unknown > 0
                || scan.is_legacy_entry_truncation()
                || scan.memory_limit_reached
                || scan.alloc_failed
            {
                head = head.child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(tokens.warning)
                        .child(self.usage_warning_line(scan)),
                );
            }
            if let Some(v) = &scan.volume {
                let ratio = if v.total > 0 {
                    v.used() as f32 / v.total as f32
                } else {
                    0.
                };
                foot = foot
                    .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child("卷"))
                    .child(div().text_xs().text_ellipsis().text_color(tokens.text_secondary).child(display_path(&v.root)))
                    .child(
                        div()
                            .w_full()
                            .h(px(4.))
                            .rounded_sm()
                            .bg(tokens.border_soft)
                            .child(div().h_full().w(relative(ratio.clamp(0., 1.))).bg(tokens.accent)),
                    )
                    .child(div().text_xs().text_color(tokens.text_secondary).child(format!(
                        "已用 {} · 可用 {} · 共 {}",
                        disk_usage::format_bytes(v.used()),
                        disk_usage::format_bytes(v.available),
                        disk_usage::format_bytes(v.total)
                    )))
                    .child(div().text_xs().text_color(tokens.text_tertiary).child(
                        "卷已用含未扫描目录与系统保留；三种口径不相减推算可清理量。ADS 不在承诺范围。",
                    ));
            } else {
                foot = foot
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("卷"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .child("卷容量暂不可用；不代表 0 或整卷已扫描"),
                    );
            }
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

    fn usage_warning_line(&self, scan: &disk_usage::Scan) -> String {
        if let Some((epoch, generation)) = scan.logical_generation {
            return format!(
                "索引逻辑大小 · e{epoch}/g{generation} · {} 项元数据未验证 · 非实际占用{}",
                scan.unknown,
                if self.workspace.engine_next.is_some() {
                    " · 当前分页，尚有子项"
                } else {
                    ""
                }
            );
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
        parts.join(" · ")
    }
}
