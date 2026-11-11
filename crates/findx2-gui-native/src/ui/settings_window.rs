use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::prefs;
use crate::session::AppSession;
use crate::settings_logic::{
    CLOSE_DIRTY_MESSAGE, DraftCommit, FindxGuiSettings, UiThemePref, commit_save_interval_secs,
    commit_search_limit, form_is_dirty,
};
use crate::theme::FindxTokens;
use crate::ui::update_card::{UpdateCardState, render_update_card};
use crate::update_logic::UpdatePhase;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Index,
    Search,
    Service,
    Advanced,
    Diagnostics,
}

pub struct SettingsWindow {
    session: Entity<AppSession>,
    tab: SettingsTab,
    settings: FindxGuiSettings,
    original: FindxGuiSettings,
    theme: UiThemePref,
    original_theme: UiThemePref,
    search_limit: Entity<InputState>,
    save_interval: Entity<InputState>,
    index_path: Entity<InputState>,
    pipe_name: Entity<InputState>,
    service_exe: Entity<InputState>,
    excluded: Entity<TextareaState>,
    hint: SharedString,
    dirty_tick: u64,
    update: UpdateCardState,
    show_syntax: bool,
    close_guarded: bool,
    saving: bool,
    service_snapshot: Option<crate::service_control::Snapshot>,
    service_pending: Option<crate::service_control::Action>,
    service_busy: bool,
    service_hint: String,
    autostart_hint: String,
    _subscriptions: Vec<Subscription>,
}

impl SettingsWindow {
    pub fn new(session: Entity<AppSession>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (settings, theme) = session.read_with(cx, |s, _| (s.settings.clone(), s.prefs.theme));
        let search_limit = cx
            .new(|cx| InputState::new(window, cx).default_value(settings.search_limit.to_string()));
        let save_interval = cx.new(|cx| {
            InputState::new(window, cx).default_value(settings.save_interval_secs.to_string())
        });
        let index_path =
            cx.new(|cx| InputState::new(window, cx).default_value(settings.index_path.clone()));
        let pipe_name =
            cx.new(|cx| InputState::new(window, cx).default_value(settings.pipe_name.clone()));
        let service_exe = cx
            .new(|cx| InputState::new(window, cx).default_value(settings.service_exe_path.clone()));
        let excluded = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .default_value(settings.excluded_dirs.join("\n"))
        });

        let mut this = Self {
            session,
            tab: SettingsTab::Index,
            original: settings.clone(),
            settings,
            theme,
            original_theme: theme,
            search_limit: search_limit.clone(),
            save_interval: save_interval.clone(),
            index_path: index_path.clone(),
            pipe_name: pipe_name.clone(),
            service_exe: service_exe.clone(),
            excluded: excluded.clone(),
            hint: SharedString::from(""),
            dirty_tick: 0,
            update: UpdateCardState::new(theme_auto_check_placeholder()),
            show_syntax: false,
            close_guarded: false,
            saving: false,
            service_snapshot: None,
            service_pending: None,
            service_busy: false,
            service_hint: "正在识别正式服务…".into(),
            autostart_hint: "正在读取 HKCU\\Run\\FindX…".into(),
            _subscriptions: Vec::new(),
        };
        this.update.auto_check = this.settings.auto_check_update;

        let mark_dirty = |this: &mut Self, cx: &mut Context<Self>| {
            this.dirty_tick = this.dirty_tick.wrapping_add(1);
            this.sync_text_fields(cx);
            cx.notify();
        };
        for input in [
            this.search_limit.clone(),
            this.save_interval.clone(),
            this.index_path.clone(),
            this.pipe_name.clone(),
            this.service_exe.clone(),
        ] {
            this._subscriptions
                .push(cx.subscribe_in(&input, window, move |this, _, ev, _, cx| {
                    if let InputEvent::Change = ev {
                        mark_dirty(this, cx);
                    }
                }));
        }

        this._subscriptions
            .push(cx.observe(&this.session, |_, _, cx| cx.notify()));
        this._subscriptions.push(
            cx.subscribe_in(&this.excluded, window, |this, _, ev, _, cx| {
                if let InputEvent::Change = ev {
                    this.sync_text_fields(cx);
                    cx.notify();
                }
            }),
        );
        this._subscriptions.push(
            cx.subscribe_in(&this.excluded, window, |this, _, ev, _, cx| {
                if let InputEvent::Change = ev {
                    this.sync_text_fields(cx);
                    cx.notify();
                }
            }),
        );
        let entity = cx.entity();
        window.on_window_should_close(cx, move |window, cx| {
            entity.update(cx, |this, cx| this.confirm_close(window, cx))
        });

        this.refresh_service(cx);
        this
    }

    fn sync_text_fields(&mut self, cx: &App) {
        self.settings.index_path = self.index_path.read(cx).value().to_string();
        self.settings.pipe_name = self.pipe_name.read(cx).value().to_string();
        self.settings.service_exe_path = self.service_exe.read(cx).value().to_string();
        self.settings.excluded_dirs = self
            .excluded
            .read(cx)
            .value()
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    fn search_limit_text(&self, cx: &App) -> String {
        self.search_limit.read(cx).value().to_string()
    }

    fn save_interval_text(&self, cx: &App) -> String {
        self.save_interval.read(cx).value().to_string()
    }

    fn is_dirty(&self, cx: &App) -> bool {
        form_is_dirty(
            &self.settings,
            Some(&self.original),
            self.theme,
            self.original_theme,
            &self.search_limit_text(cx),
            &self.save_interval_text(cx),
        )
    }

    fn confirm_close(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.saving || self.service_busy {
            return false;
        }
        if !self.is_dirty(cx) {
            return true;
        }
        self.close_guarded = true;
        cx.notify();
        false
    }

    fn revert_unsaved(&mut self, cx: &mut Context<Self>) {
        self.settings = self.original.clone();
        self.theme = self.original_theme;
        crate::theme::apply_findx_theme(
            self.original_theme
                .resolve(self.session.read(cx).system_dark),
            cx,
        );
        cx.refresh_windows();
        self.hint = "已丢弃未保存的更改".into();
        cx.notify();
    }

    fn save(&mut self, close_after: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        self.sync_text_fields(cx);
        let (limit, interval) = match (
            commit_search_limit(&self.search_limit_text(cx)),
            commit_save_interval_secs(&self.save_interval_text(cx)),
        ) {
            (DraftCommit::Ok(l), DraftCommit::Ok(i)) => (l, i),
            (DraftCommit::Err(msg), _) | (_, DraftCommit::Err(msg)) => {
                self.hint = msg.into();
                self.close_guarded = false;
                cx.notify();
                return;
            }
        };
        if crate::isolation::is_production_pipe(&self.settings.pipe_name) {
            self.hint = "只允许保存隔离管道名；活动 live 连接不会被设置改变".into();
            self.close_guarded = false;
            cx.notify();
            return;
        }
        self.settings.search_limit = limit as u32;
        self.settings.save_interval_secs = interval;
        let next = self.settings.clone();
        let theme = self.theme;
        let mut prefs = self.session.read(cx).prefs.clone();
        prefs.theme = theme;
        let handle = window.window_handle();
        self.saving = true;
        self.hint = "正在保存并回读隔离设置…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = next.clone();
            let local_prefs = prefs.clone();
            let result = cx
                .background_spawn(async move {
                    crate::prefs::save_settings(&saved)?;
                    crate::prefs::save_prefs(&local_prefs)?;
                    let text = std::fs::read_to_string(crate::prefs::settings_path())
                        .map_err(|e| e.to_string())?;
                    let readback: FindxGuiSettings =
                        serde_json::from_str(&text).map_err(|e| e.to_string())?;
                    if readback != saved {
                        return Err("设置回读与保存内容不一致".into());
                    }
                    if crate::prefs::load_prefs().theme != local_prefs.theme {
                        return Err("主题回读失败".into());
                    }
                    Ok::<(), String>(())
                })
                .await;
            let ok = result.is_ok();
            this.update(cx, |this, cx| {
                this.saving = false;
                this.close_guarded = false;
                match result {
                    Ok(()) => {
                        this.original = next.clone();
                        this.settings = next.clone();
                        this.original_theme = theme;
                        this.session.update(cx, |s, cx| {
                            s.settings = next;
                            s.prefs.theme = theme;
                            s.apply_theme(cx);
                            cx.notify();
                        });
                        this.hint = "已保存并回读验证 · 仅 FindX-native-dev".into();
                    }
                    Err(e) => this.hint = format!("保存失败（草稿保留）：{e}").into(),
                }
                cx.notify();
            })
            .ok();
            if ok && close_after {
                let _ = handle.update(cx, |_, w, _| w.remove_window());
            }
        })
        .detach();
    }

    fn set_theme(&mut self, pref: UiThemePref, cx: &mut Context<Self>) {
        self.theme = pref;
        let dark = self.session.read(cx).system_dark;
        crate::theme::apply_findx_theme(pref.resolve(dark), cx);
        cx.refresh_windows();
        cx.notify();
    }

    fn tokens(cx: &App) -> FindxTokens {
        if cx.has_global::<FindxTokens>() {
            *cx.global::<FindxTokens>()
        } else {
            FindxTokens::for_theme(crate::settings_logic::EffectiveTheme::Light)
        }
    }

    fn close_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_close(window, cx) {
            window.remove_window();
        }
    }
}

fn theme_auto_check_placeholder() -> bool {
    false
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Self::tokens(cx);
        let dirty = self.is_dirty(cx);
        let tab = self.tab;
        let roots = self.session.read(cx).available_roots.clone();

        v_flex()
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(tokens.bg)
            .text_color(tokens.text_primary)
            .child(self.render_header(dirty, tokens, cx))
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .p_4()
                    .child(match tab {
                        SettingsTab::Index => {
                            self.render_index_tab(&roots, tokens, cx).into_any_element()
                        }
                        SettingsTab::Search => {
                            self.render_search_tab(tokens, cx).into_any_element()
                        }
                        SettingsTab::Service => {
                            self.render_service_tab(tokens, cx).into_any_element()
                        }
                        SettingsTab::Advanced => {
                            self.render_advanced_tab(tokens, cx).into_any_element()
                        }
                        SettingsTab::Diagnostics => {
                            self.render_diagnostics(tokens, cx).into_any_element()
                        }
                    }),
            )
            .child(self.render_footer(tokens, cx))
            .when(self.service_pending.is_some(), |view| {
                view.child(self.render_service_confirm(tokens, cx))
            })
            .when(self.close_guarded, |view| {
                view.child(self.render_close_confirm(tokens, cx))
            })
            .when(self.show_syntax, |this| {
                this.child(self.render_syntax_help(tokens, cx))
            })
    }
}

impl SettingsWindow {
    fn render_header(
        &self,
        dirty: bool,
        tokens: FindxTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .bg(tokens.surface)
            .border_b_1()
            .border_color(tokens.border)
            .flex_shrink_0()
            .px_4()
            .pt_3()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().child("FindX2 设置"))
                    .when(dirty, |this| {
                        this.child(div().text_color(tokens.accent).child("● 未保存"))
                    }),
            )
            .child(
                h_flex().gap_1().children(
                    [
                        (SettingsTab::Index, "索引"),
                        (SettingsTab::Search, "搜索"),
                        (SettingsTab::Service, "服务模式"),
                        (SettingsTab::Advanced, "外观与高级"),
                        (SettingsTab::Diagnostics, "诊断"),
                    ]
                    .into_iter()
                    .map(|(tab, label)| {
                        let selected = self.tab == tab;
                        {
                            let entity = cx.entity();
                            Button::new(SharedString::from(label))
                                .ghost()
                                .label(label)
                                .selected(selected)
                                .on_click(move |_, _, cx| {
                                    entity.update(cx, |this, cx| {
                                        this.tab = tab;
                                        cx.notify();
                                    });
                                })
                        }
                    }),
                ),
            )
    }

    fn render_index_tab(
        &self,
        roots: &[String],
        tokens: FindxTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.settings.drives.clone();
        v_flex()
            .gap_4()
            .max_w(px(800.))
            .child(section_title("索引范围", tokens))
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .child("勾选要扫描的盘。一个都不勾 = 全盘，与命令行默认一致。本批只编辑隔离设置，不会重建生产索引。"),
            )
            .when(roots.is_empty(), |this| {
                this.child(div().text_sm().text_color(tokens.text_tertiary).child("（加载磁盘列表中…）"))
            })
            .children(roots.iter().cloned().map(|root| {
                let checked = selected.is_empty() || selected.iter().any(|d| d == &root);
                let root_key = root.clone();
                Checkbox::new(SharedString::from(format!("drive-{root}")))
                    .label(root.clone())
                    .checked(checked && !selected.is_empty() || selected.iter().any(|d| d == &root))
                    .on_change({
                    let entity = cx.entity();
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                        let mut drives = this.settings.drives.clone();
                        if *checked {
                            if !drives.iter().any(|d| d == &root_key) {
                                drives.push(root_key.clone());
                            }
                        } else {
                            drives.retain(|d| d != &root_key);
                        }
                        this.settings.drives = drives;
                        cx.notify();
                        });
                    }
                })
            }))
            .child(section_title("排除目录", tokens))
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .child("每行一个完整路径。规则只写入隔离设置；本批不会对生产 index 执行重建。"),
            )
            .child(Textarea::new(&self.excluded).h(px(96.)))
            .child(section_title("元数据", tokens))
            .child(
                Checkbox::new("backfill")
                    .checked(self.settings.enable_metadata_backfill)
                    .label("开启时间/大小元数据回填（默认开启）")
                    .on_change({
                    let entity = cx.entity();
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                        this.settings.enable_metadata_backfill = *checked;
                        cx.notify();
                        });
                    }
                }),
            )
            .child(
                Checkbox::new("full-stat")
                    .checked(self.settings.first_index_full_metadata)
                    .label("首次建库直接读全量元数据（更慢，但即时可用）")
                    .on_change({
                    let entity = cx.entity();
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                        this.settings.first_index_full_metadata = *checked;
                        cx.notify();
                        });
                    }
                }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("rebuild").primary().label("重建索引").disabled(true))
                    .child(Button::new("compact").label("压缩索引").disabled(true)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.warning)
                    .child("重建和压缩会动真实索引与服务，不属于本批。请继续用现有 WebView GUI 操作生产库。"),
            )
    }

    fn render_search_tab(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .max_w(px(800.))
            .child(
                Checkbox::new("pinyin")
                    .checked(self.settings.pinyin_default)
                    .label("默认拼音匹配")
                    .on_change({
                        let entity = cx.entity();
                        move |checked, _, cx| {
                            entity.update(cx, |this, cx| {
                                this.settings.pinyin_default = *checked;
                                cx.notify();
                            });
                        }
                    }),
            )
            .child(
                div().text_sm().text_color(tokens.text_secondary).child(
                    "与现 GUI 相同：全拼 beijing、简拼 bj；;py 强制拼音，;en / ;np 关拼音。",
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(120.)).child("结果条数上限"))
                    .child(div().w(px(140.)).child(Input::new(&self.search_limit)))
                    .child({
                        let entity = cx.entity();
                        Button::new("syntax")
                            .small()
                            .label("语法帮助")
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| {
                                    this.show_syntax = true;
                                    cx.notify();
                                });
                            })
                    }),
            )
            .child(div().text_sm().text_color(tokens.text_tertiary).child(
                "编辑时可以暂时留空。点保存时检查：须为 1 到 4294967295 的整数。默认 5000。",
            ))
    }

    fn refresh_service(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (snapshot, auto) = cx
                .background_spawn(async {
                    let s = crate::service_control::snapshot();
                    let a = crate::autostart::read();
                    (s, a)
                })
                .await;
            this.update(cx, |this, cx| {
                this.autostart_hint = format!(
                    "正式 GUI 登录自启动：{} · {}",
                    if auto.enabled {
                        "已启用"
                    } else {
                        "未启用或指向其他程序"
                    },
                    auto.command.as_deref().unwrap_or("没有 Run 项")
                );
                match snapshot {
                    Ok(s) => {
                        this.service_hint = format!(
                            "{} · {} · {} · PID {}",
                            s.name, s.state, s.start_mode, s.process_id
                        );
                        this.service_snapshot = Some(s);
                    }
                    Err(e) => {
                        this.service_snapshot = None;
                        this.service_hint = e;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    fn render_service_tab(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::service_control::Action;
        v_flex().gap_3().w_full()
            .child(section_title("管理已安装的正式 FindX 服务",tokens))
            .child(div().text_sm().child("只管理身份核对通过的 FindX2Search；不会按进程名称批量终止，也不会注册另一套开发服务。操作前确认，执行后回读。"))
            .child(div().text_sm().child(self.service_hint.clone()))
            .when_some(self.service_snapshot.as_ref(),|v,s|v.child(div().text_sm().child(s.path_name.clone())))
            .child(div().text_sm().child(self.autostart_hint.clone()))
            .child({let entity=cx.entity();Button::new("refresh-service").label("刷新服务状态").disabled(self.service_busy).on_click(move|_,_,cx|entity.update(cx,|this,cx|this.refresh_service(cx)))})
            .child(h_flex().gap_2().flex_wrap().children([Action::Start,Action::Stop,Action::Auto,Action::Manual,Action::GuiAutoOn,Action::GuiAutoOff,Action::Compact,Action::Rebuild].into_iter().map(|action|{let entity=cx.entity();Button::new(SharedString::from(action.label())).label(action.label()).disabled(self.service_busy||self.service_snapshot.is_none()).on_click(move|_,_,cx|entity.update(cx,|this,cx|{this.service_pending=Some(action);cx.notify();}))})))
            .child(div().text_sm().text_color(tokens.warning).child("服务控制需要 SCM 权限；权限不足会报告真实错误，不静默假装成功。维护需要管理员；先停服务并完整备份，在副本上运行安装版 CLI，成功校验后替换；保留旧库并恢复原运行状态。排除规则无法确定时拒绝重建。"))
            .child(div().text_sm().child("此页修改 SCM / HKCU 正式自启动；其他标签的范围/路径草稿只保存 native-dev，不覆盖正式服务配置。活动服务的命令行才是当前启动参数真源。"))
    }
    fn render_service_confirm(
        &self,
        tokens: FindxTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let action = self.service_pending.unwrap();
        let target = self
            .service_snapshot
            .as_ref()
            .map(|s| {
                format!(
                    "目标：{} · PID {} · {}\n{}",
                    s.name, s.process_id, s.state, s.path_name
                )
            })
            .unwrap_or_default();
        div().absolute().inset_0().bg(rgba(0x00000088)).flex().items_center().justify_center().child(v_flex().w(px(520.)).max_w_full().m_4().p_4().gap_3().rounded_lg().bg(tokens.surface_raised)
            .child(div().child(format!("确认{}？",action.label())))
            .child(div().text_sm().child(target))
            .child(div().text_sm().child("此操作真正影响正式 FindX。停止/维护期间搜索将不可用。备份可能占用大量磁盘空间；过程失败会明确报告。没有自动删除备份。"))
            .child(h_flex().gap_2().child({let e=cx.entity();Button::new("confirm-service").label("确认执行").on_click(move|_,_,cx|e.update(cx,|this,cx|{let Some(expected)=this.service_snapshot.clone()else{return;};this.service_pending=None;this.service_busy=true;this.service_hint="正在执行正式服务操作，请勿关闭窗口…".into();cx.notify();cx.spawn(async move|this,cx|{let result=cx.background_spawn(async move{crate::service_control::execute(action,expected)}).await;this.update(cx,|this,cx|{this.service_busy=false;this.hint=match result{Ok(m)=>m.into(),Err(e)=>format!("操作失败：{e}").into()};this.refresh_service(cx);cx.notify();}).ok();}).detach();}))})
            .child({let e=cx.entity();Button::new("cancel-service").label("取消操作").on_click(move|_,_,cx|e.update(cx,|this,cx|{this.service_pending=None;cx.notify();}))})))
    }

    fn render_advanced_tab(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let auto = self.update.auto_check;
        v_flex()
            .gap_4()
            .max_w(px(800.))
            .child(render_update_card(
                &self.update,
                tokens,
                {
                    let entity = entity.clone();
                    Rc::new(move |_, cx: &mut App, checked| {
                        entity.update(cx, |this, cx| {
                            this.update.auto_check = checked;
                            this.settings.auto_check_update = checked;
                            cx.notify();
                        });
                    })
                },
                {
                    let entity = entity.clone();
                    Rc::new(move |_, cx: &mut App, phase: UpdatePhase| {
                        entity.update(cx, |this, cx| {
                            this.update.set_demo(phase);
                            cx.notify();
                        });
                    })
                },
            ))
            .child(section_title("界面主题", tokens))
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .child("改主题会立刻预览。未保存就关闭时，会回到打开设置页时的主题。"),
            )
            .child(
                h_flex().gap_2().children(
                    [
                        (UiThemePref::Light, "浅色"),
                        (UiThemePref::Dark, "深色"),
                        (UiThemePref::System, "跟随系统"),
                    ]
                    .into_iter()
                    .map(|(pref, label)| {
                        let entity = cx.entity();
                        Button::new(SharedString::from(label))
                            .label(label)
                            .selected(self.theme == pref)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |this, cx| this.set_theme(pref, cx));
                            })
                    }),
                ),
            )
            .child(section_title("路径", tokens))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .child("索引文件（隔离目录，不是正式安装的 ProgramData）"),
                    )
                    .child(Input::new(&self.index_path)),
            )
            .child(section_title("专家选项", tokens))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .child("命名管道名（禁止使用生产默认 findx2）"),
                    )
                    .child(Input::new(&self.pipe_name)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .child("findx2-service 路径（可空；本批不会拉起）"),
                    )
                    .child(Input::new(&self.service_exe)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(160.)).child("增量落盘间隔（秒）"))
                    .child(div().w(px(140.)).child(Input::new(&self.save_interval))),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.text_tertiary)
                    .child(format!(
                        "隔离设置文件：{}",
                        prefs::settings_path().display()
                    )),
            )
            .when(auto, |this| this)
    }

    fn render_close_confirm(
        &self,
        tokens: FindxTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .absolute()
            .inset_0()
            .bg(rgba(0x00000066))
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .w(px(420.))
                    .max_w_full()
                    .m_4()
                    .p_4()
                    .gap_3()
                    .rounded_lg()
                    .bg(tokens.surface_raised)
                    .border_1()
                    .border_color(tokens.border)
                    .child(div().child("设置尚未保存"))
                    .child(div().text_sm().child(CLOSE_DIRTY_MESSAGE))
                    .child(
                        h_flex().gap_2().flex_wrap().children(
                            [
                                ("save-close", "保存并关闭"),
                                ("discard-close", "丢弃并关闭"),
                                ("cancel-close", "取消"),
                            ]
                            .into_iter()
                            .map(|(id, label)| {
                                let entity = cx.entity();
                                Button::new(id).label(label).disabled(self.saving).on_click(
                                    move |_, window, cx| {
                                        entity.update(cx, |this, cx| match id {
                                            "save-close" => this.save(true, window, cx),
                                            "discard-close" => {
                                                this.revert_unsaved(cx);
                                                window.remove_window();
                                            }
                                            _ => {
                                                this.close_guarded = false;
                                                cx.notify();
                                            }
                                        })
                                    },
                                )
                            }),
                        ),
                    ),
            )
    }

    fn render_diagnostics(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.session.read(cx);
        let rows = vec![
            format!(
                "Native 客户端版本：{} ({})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::ARCH
            ),
            format!(
                "连接：{}",
                if s.connected {
                    "Status 响应正常"
                } else {
                    "未连接 / 查询失败"
                }
            ),
            format!("活动端点：{}", s.endpoint),
            format!(
                "模式：{}",
                if s.live {
                    "live 只读生产管道"
                } else if s.mock {
                    "模拟后端"
                } else {
                    "隔离后端"
                }
            ),
            format!("最近成功 Status：{}（本地时间）", s.status_checked),
            format!("服务版本：当前 Status 协议未提供版本，不推断"),
            format!("索引：{}", s.index_status.summary),
            format!("错误：{}", s.last_error.as_deref().unwrap_or("无")),
            format!("设置文件：{}", prefs::settings_path().display()),
            format!(
                "隔离设置管道：{}（不改变当前活动连接）",
                self.settings.pipe_name
            ),
            "自动折叠：预览 <900 / 侧栏 <760 逻辑 px；手动按钮可覆盖。不是物理屏幕像素。".into(),
            "只读范围：Search / Status / Ping；未读取或打开生产索引文件。".into(),
        ];
        v_flex()
            .w_full()
            .gap_3()
            .child(section_title("只读运行诊断", tokens))
            .children(rows.into_iter().map(|r| div().text_sm().child(r)))
            .child(section_title("原始 Status 响应（每 2 秒更新）", tokens))
            .child(
                div()
                    .text_xs()
                    .p_3()
                    .bg(tokens.surface)
                    .child(s.status_raw.clone()),
            )
    }

    fn render_footer(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .h(px(56.))
            .flex_shrink_0()
            .px_4()
            .gap_2()
            .items_center()
            .bg(tokens.surface)
            .border_t_1()
            .border_color(tokens.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(tokens.text_secondary)
                    .text_ellipsis()
                    .child(self.hint.clone()),
            )
            .child({
                let entity = cx.entity();
                Button::new("save")
                    .primary()
                    .label("保存")
                    .disabled(self.saving)
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| this.save(false, window, cx));
                    })
            })
            .child({
                let entity = cx.entity();
                Button::new("close")
                    .label("关闭")
                    .on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| this.close_clicked(window, cx));
                    })
            })
    }

    fn render_syntax_help(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        {
            let entity = cx.entity();
            let entity2 = entity.clone();
            div()
                .id("syntax-overlay")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x00000066))
                .on_click(move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.show_syntax = false;
                        cx.notify();
                    });
                })
                .child(
                    v_flex()
                        .id("syntax-dialog")
                        .w(px(520.))
                        .max_h(px(560.))
                        .p_4()
                        .gap_2()
                        .rounded_lg()
                        .bg(tokens.surface_raised)
                        .border_1()
                        .border_color(tokens.border)
                        .on_click(|_, _, _| {})
                        .child(
                            h_flex()
                                .justify_between()
                                .child(div().child("搜索语法帮助"))
                                .child(Button::new("close-syntax").ghost().label("关闭").on_click(
                                    move |_, _, cx| {
                                        entity2.update(cx, |this, cx| {
                                            this.show_syntax = false;
                                            cx.notify();
                                        });
                                    },
                                )),
                        )
                        .child(
                            div()
                                .id("syntax-body")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scrollbar()
                                .text_sm()
                                .text_color(tokens.text_secondary)
                                .child(
                                    "基础：beijing / bj 拼音，直接英文名。\n\
修饰：;py 强制拼音，;en 或 ;np 关拼音。\n\
类型：ext:pdf、file:report、folder:docs、startwith:2024、endwith:backup、path:、parent:。\n\
组合：report | document 为或，!temp 排除。\n\
路径：Windows 用 C:\\Users\\...，Unix 用 /home/user 或 ~/Documents，不要写盘符。",
                                ),
                        ),
                )
        }
    }
}

fn section_title(text: &str, tokens: FindxTokens) -> impl IntoElement {
    div()
        .text_color(tokens.text_primary)
        .child(text.to_string())
}
