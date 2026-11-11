use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{
    Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::prefs;
use crate::session::{AppSession, persist_settings};
use crate::settings_logic::{
    CLOSE_DIRTY_MESSAGE, CloseDecision, DraftCommit, FindxGuiSettings, RunMode, UiThemePref,
    close_decision, commit_save_interval_secs, commit_search_limit, form_is_dirty,
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
    excluded: Entity<InputState>,
    hint: SharedString,
    dirty_tick: u64,
    update: UpdateCardState,
    show_syntax: bool,
    close_guarded: bool,
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
        let excluded = cx
            .new(|cx| InputState::new(window, cx).default_value(settings.excluded_dirs.join("\n")));

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
            this.excluded.clone(),
        ] {
            this._subscriptions
                .push(cx.subscribe_in(&input, window, move |this, _, ev, _, cx| {
                    if let InputEvent::Change = ev {
                        mark_dirty(this, cx);
                    }
                }));
        }

        let entity = cx.entity();
        window.on_window_should_close(cx, move |window, cx| {
            entity.update(cx, |this, cx| this.confirm_close(window, cx))
        });

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

    fn confirm_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let dirty = self.is_dirty(cx);
        match close_decision(dirty, false) {
            CloseDecision::Hide => {
                if dirty {
                    // only when not dirty
                }
                true
            }
            CloseDecision::Stay => {
                if self.close_guarded {
                    return false;
                }
                self.close_guarded = true;
                let entity = cx.entity();
                window.open_alert_dialog(cx, move |alert, _, _| {
                    alert
                        .title("关闭设置？")
                        .description(CLOSE_DIRTY_MESSAGE)
                        .confirm()
                        .button_props(
                            DialogButtonProps::default()
                                .ok_text("丢弃并关闭")
                                .cancel_text("取消"),
                        )
                        .on_ok({
                            let entity = entity.clone();
                            move |_, window, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    this.revert_unsaved(cx);
                                    this.close_guarded = false;
                                    window.remove_window();
                                });
                                true
                            }
                        })
                        .on_cancel({
                            let entity = entity.clone();
                            move |_, _, cx| {
                                let _ = entity.update(cx, |this, cx| {
                                    this.close_guarded = false;
                                    cx.notify();
                                });
                                true
                            }
                        })
                });
                false
            }
        }
    }

    fn revert_unsaved(&mut self, cx: &mut Context<Self>) {
        self.settings = self.original.clone();
        self.theme = self.original_theme;
        self.session.update(cx, |session, cx| {
            session.set_theme_pref(this_theme(self.theme), cx);
        });
        self.hint = "已丢弃未保存的更改".into();
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.sync_text_fields(cx);
        let limit = commit_search_limit(&self.search_limit_text(cx));
        let interval = commit_save_interval_secs(&self.save_interval_text(cx));
        let DraftCommit::Ok(limit) = limit else {
            if let DraftCommit::Err(msg) = limit {
                self.hint = msg.into();
            }
            cx.notify();
            return;
        };
        let DraftCommit::Ok(interval) = interval else {
            if let DraftCommit::Err(msg) = interval {
                self.hint = msg.into();
            }
            cx.notify();
            return;
        };
        if crate::isolation::is_production_pipe(&self.settings.pipe_name) {
            self.hint = "拒绝把管道名保存为生产默认 findx2。请使用隔离管道名。".into();
            cx.notify();
            return;
        }
        self.settings.search_limit = limit as u32;
        self.settings.save_interval_secs = interval;
        let next = self.settings.clone();
        let theme = self.theme;
        self.original = next.clone();
        self.original_theme = theme;
        self.session.update(cx, |session, cx| {
            session.settings = next.clone();
            session.set_theme_pref(theme, cx);
        });
        persist_settings(next, cx);
        self.hint = "设置已保存（仅写入隔离目录，不影响正式 GUI）".into();
        cx.notify();
    }

    fn set_theme(&mut self, pref: UiThemePref, cx: &mut Context<Self>) {
        self.theme = pref;
        self.session.update(cx, |session, cx| {
            session.set_theme_pref(pref, cx);
        });
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

fn this_theme(pref: UiThemePref) -> UiThemePref {
    pref
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
                    }),
            )
            .child(self.render_footer(tokens, cx))
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
                        (SettingsTab::Advanced, "高级"),
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
            .child(Input::new(&self.excluded).h(px(96.)))
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

    fn render_service_tab(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .max_w(px(800.))
            .child(div().child("启动模式"))
            .child(
                v_flex()
                    .gap_1()
                    .child({
                        let entity = cx.entity();
                        radio_row(
                            "mode-service",
                            "服务模式（默认；不弹 UAC，开机自启）",
                            self.settings.run_mode == RunMode::Service,
                            move |checked, _, cx| {
                                if *checked {
                                    entity.update(cx, |this, cx| {
                                        this.settings.run_mode = RunMode::Service;
                                        cx.notify();
                                    });
                                }
                            },
                            tokens,
                        )
                    })
                    .child({
                        let entity = cx.entity();
                        radio_row(
                            "mode-standalone",
                            "单体 UAC 模式（每次启动会请求管理员授权）",
                            self.settings.run_mode == RunMode::Standalone,
                            move |checked, _, cx| {
                                if *checked {
                                    entity.update(cx, |this, cx| {
                                        this.settings.run_mode = RunMode::Standalone;
                                        cx.notify();
                                    });
                                }
                            },
                            tokens,
                        )
                    }),
            )
            .child(
                Checkbox::new("everything-ipc")
                    .checked(self.settings.enable_everything_ipc)
                    .label("开启 Everything SDK 兼容窗口（IbEverythingExt 等老客户端依赖）")
                    .on_change({
                    let entity = cx.entity();
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                        this.settings.enable_everything_ipc = *checked;
                        cx.notify();
                        });
                    }
                }),
            )
            .child(
                Checkbox::new("auto-start-svc")
                    .checked(self.settings.auto_start_service)
                    .label("GUI 启动时自动拉起索引服务")
                    .on_change({
                    let entity = cx.entity();
                    move |checked, _, cx| {
                        entity.update(cx, |this, cx| {
                        this.settings.auto_start_service = *checked;
                        cx.notify();
                        });
                    }
                }),
            )
            .child(
                Checkbox::new("auto-start-app")
                    .checked(self.settings.auto_start_app)
                    .disabled(true)
                    .label("随系统启动（登录时自动启动 FindX 托盘）"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(Button::new("start-svc").label("启动服务").disabled(true))
                    .child(Button::new("stop-svc").label("停止服务").disabled(true)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.warning)
                    .child("本客户端不会启动或停止生产 findx2-service，也不会改系统服务。上面的开关只写入隔离设置。"),
            )
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

    fn render_footer(&self, tokens: FindxTokens, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .h(px(56.))
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
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |this, cx| this.save(cx));
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

fn radio_row(
    id: &'static str,
    label: &'static str,
    selected: bool,
    on_change: impl Fn(&bool, &mut Window, &mut App) + 'static,
    tokens: FindxTokens,
) -> impl IntoElement {
    let _ = tokens;
    // Checkbox used as a simple exclusive row; the listener sets the enum.
    Checkbox::new(id)
        .checked(selected)
        .label(label)
        .on_change(on_change)
}
