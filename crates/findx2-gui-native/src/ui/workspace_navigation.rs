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
        // The home label is virtual: confirming it (or an empty edit) refreshes
        // the overview instead of failing filesystem validation.
        if is_home_path(&current)
            && (draft.trim().is_empty() || draft.trim() == HOME_LABEL)
        {
            self.cancel_path_edit(cx);
            self.workspace.engine_cancel.store(true, Ordering::Release);
            self.workspace.engine_busy = false;
            self.load_home_overview(cx);
            cx.notify();
            return;
        }
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
        self.workspace.address_menu_header=None;
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
        parent_total: Option<u64>,
        current: &Path,
        owner: WeakEntity<Self>,
        mode: ViewMode,
        cx: &App,
    ) -> PopupMenu {
        let tokens = Self::tokens(cx);
        let max = parent_total.unwrap_or_else(|| items
            .entries
            .iter()
            .filter_map(|e| e.bytes)
            .max()
            .unwrap_or(1)).max(1);
        let empty=items.entries.is_empty();
        let mut menu = menu.scrollable(true).min_w(px(372.)).max_w(px(372.)).max_h(px(312.));
        if empty {menu=menu.item(PopupMenuItem::label("没有可显示的同级项目"));}
        for item in items.entries {
            let selected = key(&item.path) == key(current);
            let path = item.path;
            let directory = item.directory;
            let name = item.name;
            let bytes = item.bytes;
            let uncertain = item.uncertain;
            let ratio = (bytes.unwrap_or(0) as f32 / max as f32).clamp(0.,1.);
            let owner = owner.clone();
            menu = menu.item(
                PopupMenuItem::element(move |_, _| {
                    h_flex()
                        .id(SharedString::from(format!("usage-sibling-{name}")))
                        .w(px(340.))
                        .role(gpui::Role::MenuItem)
                        .aria_label(format!("{}{}",name,if selected {"，当前项目"} else {""}))
                        .child(div().w(px(14.)).flex_shrink_0().when(selected,|tick|tick.child(Icon::new(IconName::Check).size_3())))
                        .when(selected,|row|row.bg(tokens.row_selected).rounded_sm())
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
                                    .w(px(44.)).flex_shrink_0()
                                    .h(px(3.))
                                    .bg(tokens.border)
                                    .child(div().w(px(44. * ratio)).h_full().bg(tokens.accent)),
                            )
                            .child(
                                div()
                                    .w(px(98.)).min_w(px(98.)).flex_shrink_0().whitespace_nowrap()
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
        if mode == ViewMode::Usage && self.workspace.engine_usage {
            // Usage siblings belong to path.parent(), never to the selected child.
            // A re-rooted scene need not contain that parent. Read one indexed
            // directory, not the filesystem and not unbounded descendants.
            let header=format!("同级  {}",display_path(&parent));
            let menu=PopupMenu::build(window,cx,|menu,_,_|menu
                .item(PopupMenuItem::label("正在读取索引…"))
                .scrollable(true).min_w(px(372.)).max_w(px(372.)).max_h(px(312.)));
            self.workspace.address_menu_header=Some(header.clone());
            self.install_address_menu(menu.clone(),position,window,cx);
            let generation=self.workspace.address_menu_generation;
            self.workspace.address_menu_cancel=Arc::new(AtomicBool::new(false));
            let cancel=self.workspace.address_menu_cancel.clone();
            let expected=self.workspace.engine_generation;
            let cache=self.workspace.usage_page_cache.clone();
            let session=self.session.read(cx);let ipc=session.ipc.clone();let endpoint=session.endpoint.clone();
            cx.spawn(async move|this,cx|{
                let result=crate::indexed_siblings::load(parent.clone(),expected,cache,cancel.clone(),|after,stamp|{
                    let ipc=ipc.clone();let endpoint=endpoint.clone();let cancel=cancel.clone();let path=display_path(&parent);
                    async move{match ipc.request_cancellable(endpoint,findx2_ipc::IpcRequest::UsageChildren{path,expected:stamp,after,max_bytes:48*1024},cancel).await.map_err(|_|"读取已取消".to_string())??{
                        findx2_ipc::IpcResponse::UsageChildrenResult{page}=>Ok(page),
                        findx2_ipc::IpcResponse::Error{message}=>Err(message),
                        _=>Err("索引响应不兼容".into()),
                    }}
                }).await;
                this.update(cx,|this,cx|{
                    if generation!=this.workspace.address_menu_generation||this.workspace.address_menu.is_none()||cancel.load(Ordering::Acquire)||this.workspace.mode!=ViewMode::Usage||!this.workspace.engine_usage{return}
                    let result=result.and_then(|r|if this.workspace.engine_generation.is_some_and(|g|g.0!=r.stamp.0){Err("索引已重载，请重新打开".into())}else{Ok(r)});
                    let _=this.workspace.window.update(cx,|_,window,cx|{
                        menu.update(cx,|menu,cx|menu.rebuild(window,cx,|menu,_,cx|{
                            let menu=menu.scrollable(true).min_w(px(372.)).max_w(px(372.)).max_h(px(312.));
                            match result{
                                Ok(list)=>Self::sibling_menu_items(menu,list.items,Some(list.total),&current,owner,mode,cx),
                                Err(message)=>menu.item(PopupMenuItem::label(format!("读取失败：{message}"))),
                            }
                        }));
                    });
                }).ok();
            }).detach();
            return;
        }
        if mode == ViewMode::Usage {
            let entries = self.usage_render_scan()
                .and_then(|scan|scan.nodes.iter().position(|n|key(&n.path)==key(&parent)).map(|i|(i,scan)))
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
                                    bytes: Some(scan.weight(n)),
                                    uncertain: n.uncertain,
                                }
                            })
                            .collect(),
                    }
                });
            let menu = PopupMenu::build(window, cx, |menu, _, cx| {
                let menu = menu.item(PopupMenuItem::label(format!(
                    "同级  {}",
                    display_path(&parent)
                )));
                match entries {
                    Some(items) => Self::sibling_menu_items(menu, items, None, &current, owner, mode, cx),
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
                                Self::sibling_menu_items(menu, items, None, &current, owner, mode, cx)
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
    /// Drive switcher lives on the root breadcrumb chevron. The root has no
    /// parent, so its former "siblings" menu was permanently disabled and a
    /// multi-volume machine had no way to leave the current volume from the
    /// Usage view. This lists every available root and switches in place,
    /// preserving the current view mode.
    fn open_root_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_address_menu(cx);
        self.dismiss_file_menu(cx);
        let mut roots = self.session.read(cx).available_roots.clone();
        if roots.is_empty() {
            roots = crate::prefs::list_available_roots();
        }
        // At home no volume is current: never inject the virtual path as a drive.
        let current = if is_home_path(&self.workspace.location) {
            None
        } else {
            Some(
                self.workspace
                    .location
                    .ancestors()
                    .last()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.workspace.location.clone()),
            )
        };
        if let Some(current) = &current {
            if !roots
                .iter()
                .any(|p| key(Path::new(p)) == key(current))
            {
                roots.push(display_path(current));
            }
        }
        roots.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
        roots.dedup_by(|a, b| key(Path::new(a)) == key(Path::new(b)));
        let owner = cx.weak_entity();
        let mode = self.workspace.mode;
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            if roots.is_empty() {
                return menu.item(PopupMenuItem::label("未发现可用磁盘"));
            }
            let count = roots.len();
            for root in roots {
                let path = PathBuf::from(&root);
                let selected =
                    current.as_ref().is_some_and(|c| key(&path) == key(c));
                let owner = owner.clone();
                let target = path.clone();
                menu = menu.item(
                    PopupMenuItem::new(root)
                        .icon(Icon::default().path("icons/hard-drive.svg"))
                        .checked(selected)
                        .on_click(move |_, _, cx| {
                            owner
                                .update(cx, |this, cx| {
                                    this.dismiss_address_menu(cx);
                                    if key(&target) != key(&this.workspace.location) {
                                        this.workspace_navigate(
                                            target.clone(),
                                            mode,
                                            None,
                                            true,
                                            cx,
                                        );
                                    }
                                })
                                .ok();
                        }),
                );
            }
            // A handful of drives must never show a scrollbar; scrolling is
            // only enabled for abnormally long drive lists.
            if count > 10 {
                menu.scrollable(true).max_h(px(320.))
            } else {
                menu
            }
        });
        self.install_address_menu(menu, position, window, cx);
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
        // Home ("此电脑") has no filesystem ancestors: a single home crumb
        // plus the shared drive switcher.
        if self.workspace.mode == ViewMode::Usage && is_home_path(&self.workspace.location) {
            return frame
                .child(
                    Button::new("home-crumb")
                        .ghost()
                        .with_size(gpui_kit::component::Size::XSmall)
                        .h(px(24.))
                        .w(px(28.))
                        .px_0()
                        .rounded_sm()
                        .icon(Icon::default().path("icons/house.svg"))
                        .tooltip("全部磁盘（点击刷新）")
                        .accessibility_label("全部磁盘")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.workspace_navigate(
                                PathBuf::from(HOME_PATH),
                                ViewMode::Usage,
                                None,
                                true,
                                cx,
                            );
                        })),
                )
                .child(
                    Button::new("home-siblings")
                        .ghost()
                        .w(px(18.))
                        .h(px(24.))
                        .px_0()
                        .rounded_sm()
                        .icon(Icon::new(IconName::ChevronRight).size_3())
                        .accessibility_label("切换磁盘")
                        .tooltip("切换磁盘：C: / D: / E: …")
                        .on_click(cx.listener(
                            move |this, event: &ClickEvent, window, cx| {
                                this.open_root_menu(event.position(), window, cx)
                            },
                        )),
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
            .when(self.workspace.mode == ViewMode::Usage, |frame| {
                frame
                    .child(
                        Button::new("home-crumb-nav")
                            .ghost()
                            .with_size(gpui_kit::component::Size::XSmall)
                            .h(px(24.))
                            .w(px(28.))
                            .px_0()
                            .rounded_sm()
                            .icon(Icon::default().path("icons/house.svg"))
                            .tooltip("全部磁盘")
                            .accessibility_label("全部磁盘")
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
                        Button::new("home-root-menu")
                            .ghost()
                            .w(px(18.))
                            .h(px(24.))
                            .px_0()
                            .rounded_sm()
                            .icon(Icon::new(IconName::ChevronRight).size_3())
                            .accessibility_label("切换磁盘")
                            .tooltip("切换磁盘：C: / D: / E: …")
                            .on_click(cx.listener(
                                move |this, event: &ClickEvent, window, cx| {
                                    this.open_root_menu(event.position(), window, cx)
                                },
                            )),
                    )
            })
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
                            // In Usage mode the drive switcher lives on the home
                            // chevron; the volume root itself has no siblings.
                            .when(self.workspace.mode != ViewMode::Usage || index != 0, |row| {
                                row.child(
                                Button::new(SharedString::from(format!("path-siblings-{index}")))
                                    .ghost()
                                    .w(px(18.))
                                    .h(px(24.))
                                    .px_0()
                                    .rounded_sm()
                                    .icon(Icon::new(IconName::ChevronRight).size_3())
                                    .disabled(index != 0 && sibling_path.parent().is_none())
                                    .accessibility_label(if index == 0 {
                                        "切换磁盘"
                                    } else {
                                        "跳转到同级项目"
                                    })
                                    .tooltip(if index == 0 {
                                        "切换磁盘：C: / D: / E: …"
                                    } else {
                                        "跳转到同级项目"
                                    })
                                    .on_click(cx.listener(
                                        move |this, event: &ClickEvent, window, cx| {
                                            if index == 0 {
                                                this.open_root_menu(
                                                    event.position(),
                                                    window,
                                                    cx,
                                                );
                                            } else {
                                                this.open_sibling_menu(
                                                    sibling_path.clone(),
                                                    event.position(),
                                                    window,
                                                    cx,
                                                );
                                            }
                                        },
                                    )),
                                )
                            })
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

#[cfg(test)]mod sibling_stability_tests{#[test]fn heading_is_outside_scroll_and_selection_never_arms_delayed_auto_scroll(){let nav=include_str!("workspace_navigation.rs");let body=nav.split("fn sibling_menu_items(").nth(1).unwrap().split("fn install_address_menu(").next().unwrap();assert!(!body.contains(".checked(selected)"));assert!(!body.contains("items.partial"));assert!(!body.contains("完整记录请进入父目录"));let ui=include_str!("main_window.rs");assert!(ui.contains("address_menu_header.clone()"));assert!(ui.contains(".h(px(340.))"));assert!(nav.contains("address_menu_header=None"));}}
