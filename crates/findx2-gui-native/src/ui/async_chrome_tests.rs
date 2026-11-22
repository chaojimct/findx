//! Render-contract regression tests, not physical/continuous-frame acceptance.
#[test]
fn usage_chrome_never_inserts_async_controls_or_duplicates_cancel() {
    let src=include_str!("workspace_pages.rs");
    let toolbar=src.split("fn render_usage_toolbar").nth(1).unwrap().split("pub(super) fn render_workspace_usage").next().unwrap();
    assert!(!toolbar.contains(".when("));
    assert_eq!(toolbar.matches("Button::new(\"cancel-workspace-scan\")").count(),1);
    assert!(!toolbar.contains("usage-index-cancel"));
    for fixed in [".w(px(28.)).h(px(24.))", ".w(px(64.))", ".w(px(104.))", ".w(px(28.)).h(px(28.))"] { assert!(toolbar.contains(fixed), "{fixed}"); }
    assert!(toolbar.contains("this.cancel_engine_usage(cx)"));
    assert!(toolbar.contains("this.workspace.scan_cancel.store(true, Ordering::Release)"));
    assert!(toolbar.contains(".disabled(!(self.workspace.scanning || self.workspace.engine_busy"));
}
#[test]
fn shared_footer_async_polls_cannot_insert_activity_button() {
    let src=include_str!("main_window.rs");
    let footer=src.split("fn render_status_bar").nth(1).unwrap().split("impl Drop").next().unwrap();
    assert!(!footer.contains("ViewMode::Search && self.workspace.scanning"));
    assert!(footer.contains(".disabled(!self.workspace.scanning)"));
    assert!(footer.contains(".w(px(216.))"));
    assert!(footer.contains(".w(px(156.))"));
    // WAL 健康段自带定宽，出现/消失不得挤动左侧统计与右侧状态按钮。
    assert!(footer.contains(".w(px(148.))"));
    // The true engine warning is retained, not hidden by this layout correction.
    assert!(footer.contains("footer.warning()")); assert!(footer.contains("footer.tooltip()"));
}
#[test]
fn update_async_status_and_progress_have_permanent_reserved_rows() {
    let src=include_str!("update_card.rs");
    assert!(!src.contains(".when_some(model.status"));
    assert!(!src.contains(".when_some(model.progress_text"));
    assert!(src.contains("id(\"update-status\").h(px(20.))"));
    assert!(src.contains("id(\"update-progress\").h(px(20.))"));
    assert!(src.contains(".w(px(192.))"));
}
#[test]
fn settings_identification_keeps_permanent_rows_and_diagnostics_viewport() {
    let src=include_str!("settings_window.rs");
    let service=src.split("fn render_service_tab").nth(1).unwrap().split("fn render_service_confirm").next().unwrap();
    assert!(!service.contains(".when_some(self.service_snapshot"));
    assert!(!service.contains('·'));
    assert!(!service.contains("native-dev"));
    for id in ["service-status", "service-hint", "service-start-stop", "service-refresh", "service-boot", "service-login", "service-path", "service-options", "高级维护", "启动服务", "停止服务"] {
        assert!(service.contains(id), "missing {id}");
    }
    assert!(service.contains(".disabled(locked)") || service.contains(".disabled(self.service_busy"));
    let index=src.split("fn render_index_tab").nth(1).unwrap().split("fn render_search_tab").next().unwrap();
    assert!(!index.contains("native-dev"));
    assert!(!index.contains('·'));
    assert!(!src.contains('·'));
    assert!(!src.contains("render_advanced_tab"));
    assert!(src.contains("fn settings_page"));
    assert!(src.contains(".w(px(960.)).max_w_full()"));
    assert!(src.contains("theme-check-"));
    assert!(src.contains("id(\"settings-footer-hint\")"));
    assert!(src.contains(".w(px(96.))"));
    assert!(src.contains(".w(px(88.))"));
    assert!(src.contains("id(\"service-advanced-body\")"));
    assert!(src.contains("id(\"raw-status-viewport\").h(px(192.))"));
}

#[test] fn tables_retain_until_response_and_terminal_failure_ends_pending() {
 let src=include_str!("main_window.rs");
 let start=src.split("fn run_search").nth(1).unwrap().split("let old_generation").next().unwrap();
 let nonempty=start.split("self.page_loading = false").nth(1).unwrap();
 assert!(!nonempty.contains("self.hits.clear()"));
 assert!(nonempty.contains("self.freeze_preview()"));
 let files=include_str!("workspace.rs");
 let pending=files.split("self.workspace.listing = true;").nth(1).unwrap().split("let cancel =").next().unwrap();
 assert!(!pending.contains("rows.clear()")); assert!(pending.contains("freeze_preview"));
 assert!(files.contains("this.workspace.rows_epoch = None;"));
 for table in [include_str!("result_table.rs"),include_str!("browser_table.rs")] { assert!(table.contains("painted_event_current")); }
}
#[test] fn preview_publishes_payload_and_camera_together_not_loading_placeholder() {
 let src=include_str!("main_window.rs");
 let request=src.split("fn request_preview").nth(1).unwrap().split("fn move_sel").next().unwrap();
 assert!(!request.contains("PreviewState::loading("));
 assert!(request.contains("this.preview_options = options;"));
 assert!(request.contains("is_current_generation"));
 let render=src.split("fn render_pdf_controls").nth(1).unwrap().split("fn render_diag").next().unwrap();
 assert!(!render.contains("self.pdf_options"));
 assert!(src.contains("client.surface.set_interactive(self.preview_is_current())"));
}

#[test] fn usage_terminal_error_rolls_back_published_scope_without_hiding_error() {
 let src=include_str!("workspace.rs");
 let failure=src.split("// A terminal error ends pending navigation too.").nth(1).unwrap().split("let degraded =").next().unwrap();
 assert!(failure.contains("this.cancel_engine_usage(cx)"));
 assert!(failure.contains("format!(\"目录读取失败：{e}\")"));
 assert!(failure.contains("GENERATION_RESTART_MESSAGE"));
 assert!(failure.contains("this.workspace.engine_checked = false"));
}

#[test]
fn usage_activation_keeps_map_and_reserves_files_reveal_for_explicit_action() {
    let src=include_str!("workspace.rs");
    let handler=src.split("fn select_usage_node(").nth(1).unwrap().split("#[cfg(debug_assertions)]").next().unwrap();
    assert!(handler.contains("ViewMode::Usage"));
    assert!(handler.contains("Activation::Preview"));
    assert!(!handler.contains("self.workspace_reveal(cx)"));
    assert!(handler.contains("self.preview_active_selection(cx)"));
    let nav=include_str!("workspace_navigation.rs");
    let crumb=nav.split("path-crumb-{index}").nth(1).unwrap().split("path-siblings-{index}").next().unwrap();
    assert!(crumb.contains("this.workspace_navigate("));
    assert!(crumb.contains("this.workspace.mode"));
    assert!(!crumb.contains("file_scope_current"));
}
#[test]
fn usage_toolbar_width_accounts_for_label_padding_and_address_minimum() {
    let src=include_str!("workspace_pages.rs");
    let toolbar=src.split("fn render_usage_toolbar").nth(1).unwrap().split("pub(super) fn render_workspace_usage").next().unwrap();
    for id in ["usage-index-source","usage-index-next","usage-reset-compact"] {
        let b=toolbar.split(&format!("Button::new(\"{id}\")")).nth(1).unwrap().split(".on_click").next().unwrap();
        assert!(b.contains("Size::XSmall"));assert!(b.contains(if id=="usage-reset-compact" {".px_2()"} else {".px_0()"}));assert!(b.contains(".flex_shrink_0()"));
    }
    assert!(toolbar.contains(".min_w(px(120.))"));
}

#[test] fn actual_cancel_restores_published_scan_even_when_only_depth_data_arrived(){
 let s=include_str!("workspace.rs");let f=s.split("fn cancel_engine_usage(").nth(1).unwrap().split("fn select_engine_usage_source").next().unwrap();
 assert!(f.contains("usage_depth_pending || self.workspace.usage_pending_from.is_some()"));
 assert!(f.contains("cancel_pending_layout"));assert!(f.contains("self.workspace.scan=Some(scan.clone())"));
 assert!(f.contains("usage_scene_loaded_depth"));assert!(f.contains("usage_scene_blocked_paths.clone()"));
 let toolbar=include_str!("workspace_pages.rs");assert!(toolbar.contains("|| self.workspace.usage_depth_pending))"));
}

#[test]fn usage_quality_is_scope_level_without_repeated_unknown_dots_and_capacity_is_fixed(){
 let paint=include_str!("workspace_usage.rs");assert!(!paint.contains("if d.unknown {"));assert!(paint.contains("usage_presentation::fit_label"));assert!(paint.contains("usage_presentation::label_plan"));
 let page=include_str!("workspace_pages.rs");assert!(page.contains("usage-quality-summary"));assert!(page.contains(".h(px(126.)).min_h(px(126.)).max_h(px(126.))"));assert!(page.contains("usage_volume.get(volume_path)"));
 let w=include_str!("workspace.rs");assert!(w.contains("background_executor().spawn(async move{crate::usage_volume::query(&path)})"));
}

#[test]
fn index_tab_shows_live_indexed_locations_not_configured_drives() {
    let src = include_str!("settings_window.rs");
    let idx = src
        .split("fn render_index_tab")
        .nth(1)
        .unwrap()
        .split("fn render_service_tab")
        .next()
        .unwrap();
    assert!(idx.contains("indexed_locations_label"));
    assert!(idx.contains("id(\"indexed-locations-row\")"));
    assert!(idx.contains("暂无已索引位置"));
    // The live label itself parses indexed_drive_mask (with a legacy
    // volume_letter fallback) in the pure status_logic layer.
    let logic = include_str!("../status_logic.rs");
    assert!(logic.contains("indexed_drive_mask"));
    assert!(logic.contains("fn indexed_locations_label"));
}

#[test]
fn diagnostics_copy_redacts_before_it_reaches_the_clipboard() {
    let src = include_str!("settings_window.rs");
    let handler = src
        .split("fn copy_diagnostics")
        .nth(1)
        .unwrap()
        .split("fn render_footer")
        .next()
        .unwrap();
    let redacted = handler.find("diagnostics_bundle::redact").unwrap();
    let clipboard = handler.find("write_to_clipboard").unwrap();
    assert!(redacted < clipboard, "redaction must precede the clipboard write");
    assert!(handler.contains("diagnostics_bundle::home_dir"));
    assert!(handler.contains("已隐去用户名与邮箱"));
    // The button is a plain action: no async state, no extra rows.
    assert!(src.contains("Button::new(\"copy-diagnostics\")"));
    assert!(src.contains(".accessibility_label(\"复制已脱敏的诊断信息\")"));
}

#[test]
fn usage_copy_uses_grouping_without_changing_fixed_slots_or_raw_diagnostics() {
    let page=include_str!("workspace_pages.rs");
    let usage=&page[page.find("usage-detail-scroll").unwrap()..];
    assert!(!usage.contains('·'));
    for stable in ["usage-quality-summary", "usage-facts-popover", "usage-facts-scroll", "索引统计范围", "全局连接", ".h(px(126.)).min_h(px(126.)).max_h(px(126.))", "系统未返回容量；不是 0", "卷已用 = 总量 − 系统空闲", "索引时效见连接状态", "scan.unknown", "scan.partial", "scan.errors"] { assert!(page.contains(stable), "missing {stable}"); }
    assert!(!include_str!("../usage_presentation.rs").contains('·'));
    assert!(!include_str!("workspace_usage.rs").contains('·'));
    assert!(include_str!("../footer_status.rs").contains("detail.contains(\"索引逻辑 ·\")"));
}

#[test]
fn usage_toolbar_icon_slots_and_depth_value_are_compact_and_stable() {
    let page=include_str!("workspace_pages.rs");
    let bar=&page[page.find("fn render_usage_toolbar").unwrap()..page.find("pub(super) fn render_workspace_usage").unwrap()];
    for label in ["索引逻辑", "下一页", "减少显示层数", "增加显示层数"] { assert!(bar.contains(&format!(".accessibility_label(\"{label}\")"))); }
    assert!(bar.contains("icons/database.svg"));assert!(bar.contains("icons/list-plus.svg"));
    assert!(!bar.contains(".label(\"下一页\")"));assert!(!bar.contains(".w(px(76.))"));assert!(!bar.contains(".w(px(72.))"));assert!(!bar.contains(".w(px(30.))"));
    assert!(bar.contains(".w(px(16.)).flex_shrink_0().text_center().whitespace_nowrap()"));
    assert!(bar.contains("h_flex().id(\"usage-depth-controls\").flex_shrink_0().gap(px(2.))"));
    assert!(bar.contains("self.workspace.usage_depth <= 1"));assert!(bar.contains("self.workspace.usage_depth >= 6"));
    assert!(bar.contains("self.workspace.engine_next.is_none()"));
    assert!(bar.contains("Button::new(\"cancel-workspace-scan\")"));
    assert!(!bar.contains(".when(")); // Busy, cancel and paging never remove/insert slots.
}

#[test]fn usage_facts_trigger_has_one_accessible_activation_path(){
 let page=include_str!("workspace_pages.rs");let trigger=&page[page.find(r#"Popover::new("usage-facts-popover")"#).unwrap()..];let trigger=&trigger[..trigger.find(".content(").unwrap()];
 assert!(trigger.contains(".open(self.workspace.usage_facts_open)"));assert!(trigger.contains(".on_open_change("));assert!(trigger.contains(".on_mouse_down(MouseButton::Left"));assert!(trigger.contains(".on_click(cx.listener("));assert!(trigger.contains(r#".accessibility_label("详细信息")"#));
}

#[test] fn every_palette_command_has_an_execution_branch() {
    // Logic closure: every command offered in the palette must be handled in
    // run_command_palette_selected, otherwise it silently does nothing.
    let src = include_str!("main_window.rs");
    let list = src.split("fn command_palette_commands").nth(1).unwrap()
        .split("fn run_command_palette_selected").next().unwrap();
    let run = src.split("fn run_command_palette_selected").nth(1).unwrap()
        .split("fn toggle_panel").next().unwrap();
    let ids: Vec<&str> = list.split("id: \"").skip(1)
        .map(|s| s.split('"').next().unwrap()).collect();
    assert!(!ids.is_empty(), "palette must offer commands");
    for id in ids {
        assert!(run.contains(&format!("\"{id}\" =>")),
            "palette command '{id}' has no execution branch (dead command)");
    }
    // CSV export was explicitly rejected; it must not reappear in the palette.
    assert!(!list.contains("export_results"), "CSV export command must stay removed");
}

#[test] fn settings_page_enter_slide_matches_workspace_and_honours_reduce_motion() {
    let src = include_str!("settings_window.rs");
    assert!(src.contains("with_animation("), "settings page must animate on enter");
    assert!(src.contains("\"settings-enter\""), "animation id must be settings-specific");
    assert!(src.contains("self.enter_epoch"), "animation id must include per-instance epoch");
    assert!(src.contains("motion::PAGE_ENTER_MS"), "must reuse the shared 250ms duration");
    assert!(src.contains("motion::ease_out_cubic"), "must reuse the shared easing");
    assert!(src.contains("motion::PAGE_ENTER_PX"), "must reuse the shared 32px travel");
    assert!(src.contains("prefs.reduce_motion"), "must honour the global motion kill-switch");
    // Enter epoch must be assigned once per instance, not derived from the tab.
    assert!(src.contains("SETTINGS_ENTER_EPOCH.fetch_add(1"), "epoch must be fresh per page");
}

#[test] fn settings_nav_search_has_gray_placeholder() {
    let src = include_str!("settings_window.rs");
    assert!(src.contains("InputState::new(window, cx).placeholder(\"搜索设置项…\")"),
        "nav search must show a short gray placeholder hint that fits the rail");
}

#[test] fn file_context_menu_is_mode_aware_and_can_open_new_tab() {
    let menu = include_str!("file_actions.rs");
    // 搜索页保留「定位所在目录」；文件页用原生资源管理器显示。
    assert!(menu.contains("\"定位所在目录\""),
        "search context menu must keep the in-app parent locate");
    assert!(menu.contains("\"在文件资源管理器中显示\""),
        "directory context menu must use the native-manager label");
    assert!(menu.contains("self.workspace.mode == ViewMode::Search"),
        "locate label must be selected by page mode");
    assert!(menu.contains("2 if single && self.workspace.mode == ViewMode::Search =>"),
        "search locate branch must be mode-gated");
    assert!(menu.contains("self.reveal_in_directory(&hits[0], cx)"),
        "search locate must navigate in-app to the parent directory");
    assert!(menu.contains("2 if single => self.reveal_in_file_manager(&hits[0], cx)"),
        "directory locate must dispatch to native file-manager reveal");
    // 文件页新增「在新标签页中打开」。
    assert!(menu.contains("\"在新标签页中打开\""), "directory menu must offer open-in-new-tab");
    assert!(menu.contains("\"在新标签页中打开所在目录\""), "file menu must offer open-parent-in-new-tab");
    assert!(menu.contains("Box::new(ContextOpenInNewTab)"), "new-tab item must have an action");
    assert!(menu.contains("11 if single => {"), "new-tab action must have an execution branch");

    let ctx = include_str!("file_context.rs");
    assert!(ctx.contains("pub(super) fn reveal_in_file_manager"),
        "native reveal handler must exist");
    assert!(ctx.contains("crate::open_path::reveal_in_folder"),
        "must call the existing native reveal implementation");
    assert!(ctx.contains("background_spawn"), "shell call must run off the render thread");

    let ws = include_str!("workspace.rs");
    assert!(ws.contains("pub(super) fn open_directory_in_new_tab"),
        "new-tab handler must exist");
    let main = include_str!("main_window.rs");
    assert!(main.contains("ContextOpenInNewTab"), "new-tab action must be declared and routed");
}

#[test] fn directory_tabs_support_mouse_close_and_strip_double_click() {
    let src = include_str!("workspace_navigation.rs");
    let tabs = src
        .split("pub(super) fn render_directory_tabs")
        .nth(1)
        .unwrap()
        .split("pub(super) fn render_address")
        .next()
        .unwrap();
    assert!(tabs.contains("MouseButton::Middle"), "middle-click must be wired on tabs");
    assert!(tabs.contains("this.close_directory_tab(index, cx)"),
        "middle/double click must close that exact tab");
    // One close-button click plus the two new mouse gestures.
    assert_eq!(tabs.matches("this.close_directory_tab(index, cx)").count(), 3,
        "both middle-click and double-click must close, alongside the X button");
    assert!(tabs.contains(".on_double_click("), "double-click handler must exist");
    assert!(tabs.contains("this.new_directory_tab(window, cx)"),
        "double-click on the empty strip must open a new tab");
    assert!(tabs.contains("cx.stop_propagation()"),
        "tab handlers must stop the strip-level double-click");
}

#[test] fn shortcuts_page_is_editable_and_persists_overrides() {
    let src = include_str!("settings_window.rs");
    assert!(src.contains("fn render_shortcuts("), "shortcuts tab must be a real editor");
    assert!(src.contains("crate::keybindings::defs()"),
        "every rebindable action must be listed");
    assert!(src.contains("fn begin_capture(") && src.contains("fn save_shortcut("),
        "capture -> save flow must exist");
    assert!(src.contains("fn reset_shortcut(") && src.contains("fn reset_all_shortcuts("),
        "per-item and global reset must exist");
    assert!(src.contains("crate::keybindings::from_keystroke(&event.keystroke)"),
        "raw key events must be recorded as canonical bindings");
    assert!(src.contains("session.set_key_bindings(overrides, cx)"),
        "edits must persist through the session");
    assert!(src.contains(".track_focus(&self.capture_focus)"),
        "capture focus must be tracked so raw key events reach the view");

    let registry = include_str!("../keybindings.rs");
    assert!(registry.contains("pub fn apply("), "keymap must be reconciled at runtime");
    assert!(registry.contains("cx.clear_key_bindings()") == false,
        "clearing the whole keymap would destroy lazily registered component bindings");
    assert!(registry.contains("Unbind("), "old bindings must be explicitly unbound");

    let main = include_str!("main_window.rs");
    assert!(main.contains("crate::keybindings::apply(&saved_bindings, cx)"),
        "saved shortcuts must be applied at startup");
}

#[test] fn settings_search_lists_matched_items_and_jumps_to_their_tab() {
    // Logic closure: the nav search must search setting *content* via the
    // shared catalog/pinyin filter, list match buttons, and jump to their tab.
    let src = include_str!("settings_window.rs");
    assert!(src.contains("crate::settings_search::filter("),
        "nav search must use the content filter, not just tab-title matching");
    assert!(src.contains("crate::settings_search::all_settings_items()"),
        "nav search must use the full settings catalog");
    assert!(src.contains("settings-match-"),
        "matched settings must be rendered as clickable rail items");
    assert!(src.contains("this.tab = SettingsTab::from_canonical(tab_id)"),
        "clicking a match must switch to that setting's tab");
    assert!(src.contains("this.located_label = Some(label)"),
        "clicking a match must give visible location feedback");
    // The old title-only filter must be gone.
    assert!(!src.contains("label.to_lowercase().contains(&q)"),
        "title-only filtering must not remain");
}

#[test] fn page_enter_wrapper_is_flex_column_so_content_fills_height() {
    // Regression: the page-enter slide must not wrap `body` in a plain `div()`.
    // `body` carries `flex_1()`; only a flex-column parent lets it expand to the
    // full available height. A non-flex wrapper collapses it to content height
    // and leaves a blank strip between the page and the status bar.
    let src = include_str!("workspace_pages.rs");
    let start = src.find("let epoch = self.workspace.page_epoch;").unwrap();
    let wrap = &src[start..src[start..].find(".with_animation(").unwrap() + start];
    assert!(wrap.contains("v_flex()"), "page-enter wrapper must be v_flex(), got: {wrap}");
    assert!(!wrap.contains("div()"), "page-enter wrapper must not be a plain div(): {wrap}");
    assert!(wrap.contains(".size_full()"));
    assert!(wrap.contains(".child(body)"));
}

#[test]fn home_completion_keeps_published_scene_scan_until_scene_handoff(){
 // A scope change publishes (scene, scene_scan) atomically at handoff. If a
 // completion overwrote usage_scene_scan early, the stale scene's tile indices
 // would be interpreted against the new graph: harmless for 8k-node volumes,
 // an out-of-bounds panic for the 4-node home scene (observed index 5).
 let src=include_str!("workspace.rs");
 let home=src.split("fn load_home_overview").nth(1).unwrap().split("fn load_engine_usage").next().unwrap();
 assert!(!home.contains("usage_scene_scan=")&&!home.contains("usage_scene_scan ="),"home completion must not touch the published scene scan");
 assert!(home.contains("usage_scene_key = None"),"home completion must still trigger a scene rebuild");
}
