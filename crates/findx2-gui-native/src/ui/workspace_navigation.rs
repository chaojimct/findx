//! Cohesive tabs, compact content-width breadcrumbs and transactional compound editor.
use super::*;
use crate::navigation::{self, Sibling, Siblings};
use gpui_kit::component::{
    ActiveTheme as _,
    menu::{PopupMenu, PopupMenuItem},
    tooltip::Tooltip,
};

impl MainWindow {
    pub(in super::super) fn begin_path_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_address_menu(cx);
        self.dismiss_file_menu(cx);
        if !self.workspace.editing_path {
            self.workspace.path_status_before_edit = Some(self.workspace.status.clone());
        }
        self.workspace.path_edit.begin();
        self.workspace.path_error = None;
        self.workspace.editing_path = true;
        let value = display_path(&self.workspace.location);
        self.workspace.path.update(cx, |input, cx| {
            input.set_value(value, window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }
    pub(in super::super) fn cancel_path_edit(&mut self, cx: &mut Context<Self>) {
        self.workspace.path_edit.invalidate();
        self.workspace.path_error = None;
        if std::mem::replace(&mut self.workspace.editing_path, false) {
            if let Some(status) = self.workspace.path_status_before_edit.take() {
                self.workspace.status = status;
            }
            self.update_location_input(cx);
            cx.notify();
        }
    }
    fn address_is_composing(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.workspace.path.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        })
    }
    pub(super) fn submit_path_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.address_is_composing(window, cx) {
            return;
        }
        let Some(ticket) = self.workspace.path_edit.submit() else {
            return;
        };
        let draft = self.workspace.path.read(cx).value().to_string();
        let current = self.workspace.location.clone();
        let mode = self.workspace.mode;
        self.workspace.path_error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { navigation::validate_directory(&draft, &current) })
                .await;
            this.update(cx, |this, cx| {
                if !this.workspace.path_edit.finish(ticket) {
                    return;
                }
                match result {
                    Ok(path) => {
                        this.workspace_navigate(path, mode, None, true, cx);
                        let input = this.workspace.path.clone();
                        let focus = this.focus.clone();
                        let compound = this.workspace.address_focus.clone();
                        let _ = this.workspace.window.update(cx, move |_, window, cx| {
                            if window.is_window_active()
                                && (input.read(cx).focus_handle(cx).contains_focused(window, cx)
                                    || compound.contains_focused(window, cx))
                            {
                                focus.focus(window, cx);
                            }
                        });
                    }
                    Err(error) => {
                        let message = match error.kind() {
                            std::io::ErrorKind::NotFound => "位置不存在，请检查路径或驱动器连接。",
                            std::io::ErrorKind::PermissionDenied => "没有读取此目录的权限。",
                            std::io::ErrorKind::NotADirectory => "此路径不是目录。",
                            std::io::ErrorKind::InvalidInput => "请输入有效的目录路径。",
                            _ => "无法读取此位置；请检查路径、权限或驱动器连接。",
                        };
                        this.workspace.path_error = Some(message.into());
                        this.workspace.status = message.into();
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
    pub(in super::super) fn dismiss_address_menu(&mut self, cx: &mut Context<Self>) {
        self.workspace.address_menu_generation =
            self.workspace.address_menu_generation.wrapping_add(1);
        self.workspace
            .address_menu_cancel
            .store(true, Ordering::Release);
        if self.workspace.address_menu.take().is_some() {
            self.workspace.address_menu_subscription = None;
            if let Some(token) = self.workspace.address_menu_token.take() {
                self.finish_native_menu_focus(token, cx);
            }
            cx.notify();
        }
    }
    fn sibling_menu_items(
        menu: PopupMenu,
        items: Siblings,
        current: &Path,
        owner: WeakEntity<Self>,
        mode: ViewMode,
        cx: &App,
    ) -> PopupMenu {
        let tokens = Self::tokens(cx);
        let max = items
            .entries
            .iter()
            .filter_map(|e| e.bytes)
            .max()
            .unwrap_or(1)
            .max(1);
        let mut menu = menu.scrollable(true).max_h(px(340.));
        for item in items.entries {
            let selected = key(&item.path) == key(current);
            let path = item.path;
            let directory = item.directory;
            let name = item.name;
            let bytes = item.bytes;
            let uncertain = item.uncertain;
            let ratio = bytes.unwrap_or(0) as f32 / max as f32;
            let owner = owner.clone();
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    h_flex()
                        .w(px(340.))
                        .gap_2()
                        .text_size(px(12.))
                        .child(
                            Icon::new(if directory {
                                IconName::Folder
                            } else {
                                IconName::File
                            })
                            .size_3p5()
                            .text_color(tokens.text_secondary),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                                .child(name.clone()),
                        )
                        .when_some(bytes, |row, bytes| {
                            row.child(
                                div()
                                    .w(px(44.))
                                    .h(px(3.))
                                    .bg(tokens.border)
                                    .child(div().w(px(44. * ratio)).h_full().bg(tokens.accent)),
                            )
                            .child(
                                div()
                                    .w(px(82.))
                                    .text_right()
                                    .text_color(tokens.text_secondary)
                                    .child(format!(
                                        "{}{}",
                                        disk_usage::format_bytes(bytes),
                                        if uncertain { " +?" } else { "" }
                                    )),
                            )
                        })
                })
                .checked(selected)
                .on_click(move |_, _, cx| {
                    owner
                        .update(cx, |this, cx| {
                            this.dismiss_address_menu(cx);
                            if directory {
                                this.workspace_navigate(path.clone(), mode, None, true, cx);
                            } else if let Some(parent) = path.parent() {
                                this.workspace_navigate(
                                    parent.to_path_buf(),
                                    ViewMode::Directory,
                                    Some(display_path(&path)),
                                    true,
                                    cx,
                                );
                            }
                        })
                        .ok();
                }),
            );
        }
        if items.partial {
            menu = menu.item(PopupMenuItem::label(
                "仅显示部分同级项目；未知大小不补零，其他项目可进入父目录查看",
            ));
        }
        menu
    }
    fn install_address_menu(
        &mut self,
        menu: Entity<PopupMenu>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let token = self.track_native_menu_focus(menu.read(cx).focus_handle(cx), window, cx);
        self.workspace.address_menu_token = Some(token);
        self.workspace.address_menu_subscription = Some(cx.subscribe_in(
            &menu,
            window,
            move |this, _, _: &DismissEvent, window, cx| {
                if this.workspace.address_menu_token != Some(token) {
                    return;
                }
                this.dismiss_address_menu(cx);
                // Only reclaim focus while this popup was the focus owner, never after a user's outside click.
                if window.focused(cx).is_none() {
                    this.focus.focus(window, cx);
                }
            },
        ));
        self.workspace.address_menu_position = position;
        menu.read(cx).focus_handle(cx).focus(window, cx);
        self.workspace.address_menu = Some(menu);
        cx.notify();
    }
    fn open_sibling_menu(
        &mut self,
        path: PathBuf,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_address_menu(cx);
        self.dismiss_file_menu(cx);
        let Some(parent) = path.parent().map(Path::to_path_buf) else {
            return;
        };
        let owner = cx.weak_entity();
        let mode = self.workspace.mode;
        let current = path.clone();
        if mode == ViewMode::Usage {
            let entries = self
                .workspace
                .scan_lookup
                .get(&key(&parent))
                .and_then(|&i| self.workspace.scan.as_ref().map(|s| (i, s)))
                .map(|(i, scan)| {
                    let p = &scan.nodes[i];
                    Siblings {
                        partial: scan.partial || p.uncertain || p.children.len() > 256,
                        entries: p
                            .children
                            .iter()
                            .take(256)
                            .map(|&i| {
                                let n = &scan.nodes[i];
                                Sibling {
                                    path: n.path.clone(),
                                    name: n
                                        .path
                                        .file_name()
                                        .map(|s| s.to_string_lossy().into_owned())
                                        .unwrap_or_else(|| display_path(&n.path)),
                                    directory: n.directory,
                                    bytes: Some(n.allocated),
                                    uncertain: n.uncertain,
                                }
                            })
                            .collect(),
                    }
                });
            let menu = PopupMenu::build(window, cx, |menu, _, cx| {
                let menu = menu.item(PopupMenuItem::label(format!(
                    "同级 · {}",
                    display_path(&parent)
                )));
                match entries {
                    Some(items) => Self::sibling_menu_items(menu, items, &current, owner, mode, cx),
                    None => menu.item(PopupMenuItem::label("尚未扫描此层；打开菜单不会启动扫描")),
                }
            });
            self.install_address_menu(menu, position, window, cx);
            return;
        }
        let menu = PopupMenu::build(window, cx, |menu, _, _| {
            menu.item(PopupMenuItem::label("正在读取同级项目…"))
        });
        self.install_address_menu(menu.clone(), position, window, cx);
        let generation = self.workspace.address_menu_generation;
        self.workspace.address_menu_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.workspace.address_menu_cancel.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { navigation::list_siblings(&parent, &cancel) })
                .await;
            this.update(cx, |this, cx| {
                if generation != this.workspace.address_menu_generation
                    || this.workspace.address_menu.is_none()
                {
                    return;
                }
                let _ = this.workspace.window.update(cx, |_, window, cx| {
                    menu.update(cx, |menu, cx| {
                        menu.rebuild(window, cx, |menu, _, cx| match result {
                            Ok(items) => {
                                Self::sibling_menu_items(menu, items, &current, owner, mode, cx)
                            }
                            Err(_) => menu.item(PopupMenuItem::label(
                                "无法读取同级项目；请检查权限或磁盘连接",
                            )),
                        });
                    })
                });
            })
            .ok();
        })
        .detach();
    }
    fn open_ancestor_menu(
        &mut self,
        paths: Vec<PathBuf>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_address_menu(cx);
        self.dismiss_file_menu(cx);
        let owner = cx.weak_entity();
        let mode = self.workspace.mode;
        let menu = PopupMenu::build(window, cx, |mut menu, _, _| {
            for path in paths {
                let owner = owner.clone();
                menu = menu.item(
                    PopupMenuItem::new(display_path(&path))
                        .icon(IconName::Folder)
                        .on_click(move |_, _, cx| {
                            owner
                                .update(cx, |this, cx| {
                                    this.dismiss_address_menu(cx);
                                    this.workspace_navigate(path.clone(), mode, None, true, cx);
                                })
                                .ok();
                        }),
                );
            }
            menu.scrollable(true).max_h(px(320.))
        });
        self.install_address_menu(menu, position, window, cx);
    }
    /// Entire tab is one surface; the close button is a small, geometry-stable child hot zone.
    pub(super) fn render_directory_tabs(
        &self,
        preview: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
            .child(
                h_flex()
                    .id("directory-tab-scroll")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_x_scroll()
                    .track_scroll(&self.workspace.tab_scroll)
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
                        let tooltip = display_path(path);
                        let selected = index == self.workspace.active_tab;
                        h_flex()
                            .id(SharedString::from(format!("directory-tab-{index}")))
                            .h_full()
                            .w(px(
                                (title.chars().count() as f32 * 7. + 62.).clamp(100., 194.)
                            ))
                            .flex_shrink_0()
                            .px_2()
                            .gap_1()
                            .border_b_2()
                            .border_color(if selected {
                                tokens.accent
                            } else {
                                tokens.surface
                            })
                            .bg(if selected { tokens.bg } else { tokens.surface })
                            .text_color(if selected {
                                tokens.text_primary
                            } else {
                                tokens.text_secondary
                            })
                            .cursor_pointer()
                            .hover(|d| d.bg(tokens.surface_raised))
                            .aria_label(format!("目录标签：{tooltip}"))
                            .aria_selected(selected)
                            .tooltip(move |window, cx| {
                                Tooltip::new(tooltip.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.select_directory_tab(index, cx)
                            }))
                            .child(Icon::new(IconName::Folder).size_3p5().flex_shrink_0())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(12.))
                                    .text_ellipsis()
                                    .overflow_hidden()
                                    .child(title),
                            )
                            .child(div().w(px(22.)).flex_shrink_0().when(
                                self.workspace.tabs.len() > 1,
                                |d| {
                                    d.child(
                                        Button::new(SharedString::from(format!(
                                            "close-directory-tab-{index}"
                                        )))
                                        .ghost()
                                        .rounded_sm()
                                        .w(px(20.))
                                        .h(px(20.))
                                        .px_0()
                                        .icon(Icon::new(IconName::Close).size_3())
                                        .accessibility_label("关闭目录标签")
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.close_directory_tab(index, cx);
                                            }),
                                        ),
                                    )
                                },
                            ))
                    }))
                    .child(
                        Button::new("new-directory-tab")
                            .ghost()
                            .w(px(28.))
                            .h(px(28.))
                            .px_0()
                            .icon(Icon::new(IconName::Plus).size_3p5())
                            .accessibility_label("新建目录标签")
                            .on_click(cx.listener(|this, _, w, cx| this.new_directory_tab(w, cx))),
                    ),
            )
            .child(
                self.tool_button("directory-preview", "", 0, preview, cx)
                    .w(px(28.))
                    .px_0()
                    .accessibility_label("预览"),
            )
            .into_any_element()
    }
    pub(super) fn render_address(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = Self::tokens(cx);
        let measured_width = self.workspace.address_width.clone();
        let owner = cx.weak_entity();
        let frame = h_flex()
            .id("directory-address-compound")
            .relative()
            .flex_1()
            .min_w_0()
            .h(px(30.))
            .overflow_hidden()
            .rounded_sm()
            .border_1()
            .border_color(if self.workspace.path_error.is_some() {
                tokens.error
            } else {
                tokens.border
            })
            .bg(tokens.surface_raised)
            .px_1()
            .track_focus(&self.workspace.address_focus)
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let width = f32::from(bounds.size.width);
                        if (measured_width.get() - width).abs() > 1. {
                            measured_width.set(width);
                            let _ = owner.update(cx, |_, cx| cx.notify());
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                if this.workspace.editing_path {
                    let input = this.workspace.path.read(cx);
                    let associated_popup = input.has_selection_focus(window, cx)
                        && !input.focus_handle(cx).contains_focused(window, cx);
                    if !associated_popup {
                        this.cancel_path_edit(cx);
                    }
                }
            }))
            .capture_action(
                cx.listener(|this, _: &gpui_kit::base::input::Escape, window, cx| {
                    let input = this.workspace.path.read(cx);
                    let internal_popup = input.has_selection_focus(window, cx)
                        && !input.focus_handle(cx).contains_focused(window, cx);
                    if !this.workspace.editing_path
                        || internal_popup
                        || this.address_is_composing(window, cx)
                    {
                        cx.propagate();
                        return;
                    }
                    this.cancel_path_edit(cx);
                    this.focus.focus(window, cx);
                    cx.stop_propagation();
                }),
            )
            .capture_action(
                cx.listener(|this, _: &gpui_kit::base::input::Enter, window, cx| {
                    this.workspace.path_ime_enter =
                        this.workspace.editing_path && this.address_is_composing(window, cx);
                    cx.propagate();
                }),
            )
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let composing =
                    this.workspace.editing_path && this.address_is_composing(window, cx);
                this.workspace.path_ime_enter = composing && event.keystroke.key == "enter";
                this.workspace.path_ime_escape = composing && event.keystroke.key == "escape";
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape"
                    && std::mem::take(&mut this.workspace.path_ime_escape)
                {
                    return;
                }
                if this.workspace.editing_path
                    && event.keystroke.key == "escape"
                    && !this.address_is_composing(window, cx)
                {
                    this.cancel_path_edit(cx);
                    this.focus.focus(window, cx);
                    cx.stop_propagation();
                }
            }));
        if self.workspace.editing_path {
            return frame
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.workspace.path).appearance(false)),
                )
                .child(
                    Button::new("commit-directory-address")
                        .ghost()
                        .w(px(24.))
                        .h(px(24.))
                        .px_0()
                        .icon(Icon::new(IconName::ArrowRight).size_3p5())
                        .accessibility_label("确认并转到目录")
                        .disabled(self.workspace.path_edit.pending())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.submit_path_edit(window, cx)),
                        ),
                )
                .into_any_element();
        }
        let mut paths = self
            .workspace
            .location
            .ancestors()
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        paths.reverse();
        let names = paths
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| display_path(p))
            })
            .collect::<Vec<_>>();
        let widths = names
            .iter()
            .map(|name| {
                let text_system = cx.text_system();
                let font = text_system.resolve_font(&Font {
                    family: cx.theme().font_family.clone(),
                    ..Default::default()
                });
                name.chars()
                    .map(|ch| f32::from(text_system.layout_width(font, px(12.), ch)))
                    .sum::<f32>()
                    + 30.
            })
            .collect::<Vec<_>>();
        let available = (self.last_content_width
            - if self.workspace.mode == ViewMode::Directory {
                DIRECTORY_SIDEBAR_WIDTH
                    + if self.last_content_width < 900. {
                        142.
                    } else {
                        196.
                    }
                    + 136.
            } else {
                150.
            })
        .max(100.);
        let available = if self.workspace.address_width.get() > 0. {
            (self.workspace.address_width.get() - 20.).max(80.)
        } else {
            available
        };
        let plan = navigation::plan_crumbs(&widths, available);
        let last = paths.len().saturating_sub(1);
        let hidden = plan
            .hidden
            .iter()
            .map(|&i| paths[i].clone())
            .collect::<Vec<_>>();
        frame
            .children(
                plan.visible
                    .iter()
                    .enumerate()
                    .map(|(visible_index, &index)| {
                        let path = paths[index].clone();
                        let sibling_path = path.clone();
                        let tooltip = display_path(&path);
                        let width = if index == last {
                            plan.terminal_width
                        } else {
                            widths[index]
                        };
                        h_flex()
                            .flex_shrink_0()
                            .min_w_0()
                            .h_full()
                            .when(visible_index == 1 && !hidden.is_empty(), |row| {
                                let hidden = hidden.clone();
                                row.child(
                                    Button::new("collapsed-ancestors")
                                        .ghost()
                                        .w(px(28.))
                                        .h(px(24.))
                                        .px_0()
                                        .icon(IconName::Ellipsis)
                                        .accessibility_label("展开折叠的上级路径")
                                        .on_click(cx.listener(
                                            move |this, event: &ClickEvent, window, cx| {
                                                this.open_ancestor_menu(
                                                    hidden.clone(),
                                                    event.position(),
                                                    window,
                                                    cx,
                                                )
                                            },
                                        )),
                                )
                            })
                            .child(
                                Button::new(SharedString::from(format!("path-crumb-{index}")))
                                    .ghost()
                                    .with_size(gpui_kit::component::Size::XSmall)
                                    .h(px(24.))
                                    .w(px((width - 18.).max(18.)))
                                    .min_w_0()
                                    .px_1()
                                    .rounded_sm()
                                    .text_size(px(12.))
                                    .overflow_hidden()
                                    .label(names[index].clone())
                                    .when(index == last, |button| {
                                        button.font_weight(FontWeight::SEMIBOLD)
                                    })
                                    .tooltip(tooltip)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if key(&path) != key(&this.workspace.location) {
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
                            .child(
                                Button::new(SharedString::from(format!("path-siblings-{index}")))
                                    .ghost()
                                    .w(px(18.))
                                    .h(px(24.))
                                    .px_0()
                                    .rounded_sm()
                                    .icon(Icon::new(IconName::ChevronRight).size_3())
                                    .disabled(sibling_path.parent().is_none())
                                    .accessibility_label("跳转到同级项目")
                                    .on_click(cx.listener(
                                        move |this, event: &ClickEvent, window, cx| {
                                            this.open_sibling_menu(
                                                sibling_path.clone(),
                                                event.position(),
                                                window,
                                                cx,
                                            )
                                        },
                                    )),
                            )
                    }),
            )
            .child(
                div()
                    .id("address-blank-edit")
                    .flex_1()
                    .min_w(px(12.))
                    .h_full()
                    .cursor_text()
                    .on_click(cx.listener(|this, _, window, cx| this.begin_path_edit(window, cx))),
            )
            .into_any_element()
    }
}
