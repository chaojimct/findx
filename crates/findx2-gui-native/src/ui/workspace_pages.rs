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

    fn tool_button(
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
                        .max_w(px(190.))
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
            .child(
                self.tool_button("search-actions", "⋯", 2, false, cx)
                    .accessibility_label("文件操作"),
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

    fn render_directory_tabs(&self, preview: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        h_flex()
            .w_full()
            .h(px(34.))
            .flex_shrink_0()
            .px_2()
            .gap_1()
            .bg(tokens.surface)
            .border_b_1()
            .border_color(tokens.border)
            .overflow_hidden()
            .children(self.workspace.tabs.iter().enumerate().map(|(index, tab)| {
                let path = if index == self.workspace.active_tab {
                    &self.workspace.location
                } else {
                    &tab.location
                };
                let title = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| display_path(path));
                let selected = index == self.workspace.active_tab;
                h_flex()
                    .max_w(px(170.))
                    .min_w(px(86.))
                    .h_full()
                    .border_b_2()
                    .border_color(if selected {
                        tokens.accent
                    } else {
                        tokens.surface
                    })
                    .bg(if selected { tokens.bg } else { tokens.surface })
                    .child(
                        Button::new(SharedString::from(format!("directory-tab-{index}")))
                            .ghost()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .rounded_none()
                            .icon(IconName::Folder)
                            .tooltip(display_path(path))
                            .label(title)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_directory_tab(index, cx)
                            })),
                    )
                    .when(self.workspace.tabs.len() > 1, |view| {
                        view.child(
                            Button::new(SharedString::from(format!("close-directory-tab-{index}")))
                                .ghost()
                                .w(px(22.))
                                .px_0()
                                .icon(IconName::Close)
                                .tooltip("关闭目录标签")
                                .accessibility_label("关闭目录标签")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.close_directory_tab(index, cx)
                                })),
                        )
                    })
            }))
            .child(
                Button::new("new-directory-tab")
                    .ghost()
                    .icon(IconName::Plus)
                    .tooltip("新建目录标签")
                    .accessibility_label("新建目录标签")
                    .w(px(30.))
                    .px_0()
                    .on_click(
                        cx.listener(|this, _, window, cx| this.new_directory_tab(window, cx)),
                    ),
            )
            .child(div().flex_1().min_w_0())
            .child(
                self.tool_button("directory-actions", "⋯", 2, false, cx)
                    .w(px(28.))
                    .px_0()
                    .accessibility_label("文件操作"),
            )
            .child(
                self.tool_button("directory-preview", "", 0, preview, cx)
                    .w(px(28.))
                    .px_0()
                    .tooltip("显示或隐藏预览")
                    .accessibility_label("预览"),
            )
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

    fn render_address(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Self::tokens(cx);
        if self.workspace.editing_path {
            return h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.workspace.path)),
                )
                .child(
                    Button::new("commit-directory-address")
                        .ghost()
                        .icon(IconName::ArrowRight)
                        .tooltip("转到目录")
                        .accessibility_label("转到目录")
                        .w(px(26.))
                        .h(px(30.))
                        .px_0()
                        .on_click(cx.listener(|this, _, _, cx| {
                            let path =
                                PathBuf::from(this.workspace.path.read(cx).value().to_string());
                            this.workspace_navigate(path, this.workspace.mode, None, true, cx);
                        })),
                )
                .into_any_element();
        }
        let mut crumbs = self
            .workspace
            .location
            .ancestors()
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        crumbs.reverse();
        let limit = if self.workspace.mode == ViewMode::Directory && self.last_content_width < 1200.
        {
            3
        } else {
            4
        };
        let collapsed = crumbs.len() > limit;
        if crumbs.len() > limit {
            let root = crumbs[0].clone();
            crumbs = std::iter::once(root)
                .chain(
                    crumbs
                        .into_iter()
                        .rev()
                        .take(limit - 1)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev(),
                )
                .collect();
        }
        let count = crumbs.len();
        h_flex()
            .flex_1()
            .min_w_0()
            .h(px(30.))
            .overflow_hidden()
            .rounded_md()
            .border_1()
            .border_color(tokens.border)
            .bg(tokens.surface_raised)
            .px_1()
            .children(crumbs.into_iter().enumerate().map(|(index, path)| {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| display_path(&path));
                h_flex()
                    .min_w_0()
                    .overflow_hidden()
                    .when(index > 0, |v| v.flex_1())
                    .when(index == 0, |v| v.flex_shrink_0())
                    .when(index > 0, |view| {
                        view.child(
                            div()
                                .px_1()
                                .text_xs()
                                .text_color(tokens.text_tertiary)
                                .child(if collapsed && index == 1 {
                                    "› … ›"
                                } else {
                                    "›"
                                }),
                        )
                    })
                    .child(
                        Button::new(SharedString::from(format!("path-crumb-{index}")))
                            .ghost()
                            .label(name)
                            .tooltip(display_path(&path))
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .h(px(26.))
                            .px_1()
                            .max_w(px(160.))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if index + 1 == count {
                                    this.workspace.editing_path = true;
                                    this.workspace.path.update(cx, |i, cx| i.focus(window, cx));
                                    cx.notify();
                                } else {
                                    this.workspace_navigate(
                                        path.clone(),
                                        this.workspace.mode,
                                        None,
                                        true,
                                        cx,
                                    );
                                }
                            })),
                    )
            }))
            .child(
                div()
                    .id("address-blank-edit")
                    .flex_1()
                    .h_full()
                    .cursor_text()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.workspace.editing_path = true;
                        this.workspace.path.update(cx, |i, cx| i.focus(window, cx));
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_directory_toolbar(&self, _preview: bool, cx: &mut Context<Self>) -> impl IntoElement {
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
                    } else if self.workspace.scan.is_some() {
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
        let mut map = div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .m_3()
            .overflow_hidden()
            .rounded_md()
            .bg(tokens.surface);
        if let (Some(scan), Some(root)) = (self.workspace.scan.as_ref(), root) {
            if scan.nodes[root].allocated > 0 {
                let mut budget = 1200;
                map =
                    map.child(self.render_nested_map(scan, root, selected, 0, 1., &mut budget, cx));
            } else {
                map = map.child(self.usage_empty(
                    "已测占用为 0；未知、空文件及硬链接别名仍可在右侧查看",
                    tokens,
                ));
            }
        } else {
            map = map.child(self.usage_empty(
                if self.workspace.scanning {
                    "正在后台扫描；总量未知，完成或取消后展示已测树图。"
                } else {
                    "选择目录并开始扫描\n面积表示实际占用，不是可删除空间。"
                },
                tokens,
            ));
        }
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
            .child(h_flex().h(px(32.)).px_3().gap_2().flex_shrink_0().text_xs()
                .child(Button::new("usage-depth-down").ghost().icon(IconName::Minus).w(px(24.)).h(px(24.)).px_0().accessibility_label("减少显示层数").disabled(self.workspace.usage_depth<=1).on_click(cx.listener(|this,_,_,cx|{this.workspace.usage_depth=this.workspace.usage_depth.saturating_sub(1).max(1);cx.notify();})))
                .child(format!("显示 {} 层",self.workspace.usage_depth))
                .child(Button::new("usage-depth-up").ghost().icon(IconName::Plus).w(px(24.)).h(px(24.)).px_0().accessibility_label("增加显示层数").disabled(self.workspace.usage_depth>=6).on_click(cx.listener(|this,_,_,cx|{this.workspace.usage_depth=(this.workspace.usage_depth+1).min(6);cx.notify();})))
                .child(div().text_color(tokens.text_tertiary).child("显示层数 ≠ 扫描深度；目录按主要类别着色"))
                .child(div().flex_1())
                .child(Button::new("usage-scan-budget").ghost().icon(IconName::Settings).h(px(26.)).label(format!("上限 {}",Limits::profile(self.workspace.usage_limit_profile).label())).tooltip("手动选择下一次扫描的资源上限；较高上限可能占用大量内存，不会自动开始扫描").disabled(self.workspace.scanning).on_click(cx.listener(|this,_,_,cx|{this.workspace.usage_limit_profile=(this.workspace.usage_limit_profile+1)%3;cx.notify();}))))
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

    fn usage_empty(&self, label: &str, tokens: FindxTokens) -> impl IntoElement {
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

    fn render_nested_map(
        &self,
        scan: &Scan,
        node: usize,
        selected: Option<usize>,
        depth: usize,
        area: f32,
        budget: &mut usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = Self::tokens(cx);
        let children = &scan.nodes[node].children;
        let limit = (if depth == 0 { 80usize } else { 24usize }).min(budget.saturating_sub(1));
        let mut weights = children
            .iter()
            .take(limit)
            .map(|&i| (i, scan.nodes[i].allocated))
            .collect::<Vec<_>>();
        let drawn = weights.iter().map(|(_, bytes)| *bytes).sum::<u64>();
        // Retain directory metadata and every aggregated child byte without walking all children per frame.
        let remainder = scan.nodes[node].allocated.saturating_sub(drawn);
        if remainder > 0 {
            weights.push((usize::MAX, remainder));
        }
        let mut view = div().size_full().relative().overflow_hidden();
        let tiles = disk_usage::tiles(&weights);
        *budget = budget.saturating_sub(tiles.len());
        for tile in tiles {
            let index = tile.index;
            if index == usize::MAX {
                let label = format!(
                    "目录自身 / 其余 {} 项 · {}",
                    children.len().saturating_sub(limit),
                    disk_usage::format_bytes(remainder)
                );
                view = view.child(
                    div()
                        .absolute()
                        .left(relative(tile.x))
                        .top(relative(tile.y))
                        .w(relative(tile.w))
                        .h(relative(tile.h))
                        .border_2()
                        .border_color(tokens.bg)
                        .p_2()
                        .bg(tokens.allocation_color(0))
                        .text_xs()
                        .overflow_hidden()
                        .child(label),
                );
                continue;
            }
            let n = &scan.nodes[index];
            let name = n
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| display_path(&n.path));
            let size = disk_usage::format_bytes(n.allocated);
            let nested = n.directory
                && !n.children.is_empty()
                && depth + 1 < self.workspace.usage_depth
                && tile.w * tile.h * area > 0.002
                && *budget > 1;
            let category = n.category;
            let on = selected == Some(index);
            let tile_view = v_flex()
                .id(SharedString::from(format!("usage-tile-{index}")))
                .role(gpui_kit::accesskit::Role::Button)
                .accessibility_id(SharedString::from(format!("usage-node-{index}")))
                .aria_label(format!("{name} · {size}"))
                .aria_description("单击选择；双击目录进入，不删除文件")
                .absolute()
                .left(relative(tile.x))
                .top(relative(tile.y))
                .w(relative(tile.w))
                .h(relative(tile.h))
                .overflow_hidden()
                .border_1()
                .border_color(if on { tokens.accent } else { tokens.bg })
                .bg(tokens.allocation_color(category))
                .text_color(tokens.text_primary)
                .cursor_pointer()
                .child(
                    div()
                        .flex_shrink_0()
                        .w_full()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(format!("{name}  {size}")),
                )
                .when(nested, |view| {
                    view.child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .mx_1()
                            .mb_1()
                            .overflow_hidden()
                            .child(self.render_nested_map(
                                scan,
                                index,
                                selected,
                                depth + 1,
                                area * tile.w * tile.h,
                                budget,
                                cx,
                            )),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_usage_node(index, false, cx);
                    cx.stop_propagation();
                }))
                .on_double_click(cx.listener(move |this, _, _, cx| {
                    this.select_usage_node(index, true, cx);
                    cx.stop_propagation();
                }));
            let owner = cx.weak_entity();
            view = view.child(tile_view.context_menu(move |menu, window, cx| {
                owner
                    .update(cx, |view, cx| {
                        view.select_usage_node(index, false, cx);
                        view.file_context_menu(menu, false, window, cx)
                    })
                    .unwrap()
            }));
        }
        view.into_any_element()
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
