use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme::FindxTokens;
use crate::update_logic::{
    UpdateInfo, UpdatePhase, UpdateProgress, derive_update_phase, format_asset_size,
    placeholder_available, placeholder_current, placeholder_error, placeholder_progress,
};

pub struct UpdateCardState {
    pub busy: bool,
    pub info: Option<UpdateInfo>,
    pub progress: Option<UpdateProgress>,
    pub downloaded_path: Option<String>,
    pub auto_check: bool,
}

impl UpdateCardState {
    pub fn new(auto_check: bool) -> Self {
        Self {
            busy: false,
            info: None,
            progress: None,
            downloaded_path: None,
            auto_check,
        }
    }

    pub fn phase(&self) -> UpdatePhase {
        derive_update_phase(
            self.busy,
            self.progress.as_ref(),
            self.info.as_ref(),
            self.downloaded_path.as_deref(),
        )
    }

    pub fn set_demo(&mut self, phase: UpdatePhase) {
        self.busy = false;
        self.info = None;
        self.progress = None;
        self.downloaded_path = None;
        match phase {
            UpdatePhase::Idle => {}
            UpdatePhase::Checking => self.busy = true,
            UpdatePhase::Available => self.info = Some(placeholder_available()),
            UpdatePhase::Downloading => {
                self.busy = true;
                self.progress = Some(placeholder_progress(41.7));
            }
            UpdatePhase::Downloaded => {
                self.info = Some(placeholder_available());
                self.downloaded_path = Some("(placeholder) FindX-setup.exe".into());
            }
            UpdatePhase::Current => self.info = Some(placeholder_current()),
            UpdatePhase::Error => self.info = Some(placeholder_error()),
        }
    }
}

pub fn render_update_card(
    state: &UpdateCardState,
    tokens: FindxTokens,
    on_auto_check: Rc<dyn Fn(&mut Window, &mut App, bool)>,
    on_demo: Rc<dyn Fn(&mut Window, &mut App, UpdatePhase)>,
) -> impl IntoElement {
    let phase = state.phase();
    let label = phase.label();
    let notes = state
        .info
        .as_ref()
        .filter(|i| i.has_update)
        .and_then(|i| i.release_notes.as_deref())
        .unwrap_or("")
        .to_string();

    v_flex()
        .id("fx-update-card")
        .w_full()
        .gap_2()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(tokens.border)
        .bg(tokens.surface)
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .child(div().text_color(tokens.text_primary).child("应用更新"))
                .when_some(label, |this, text| {
                    this.child(
                        div()
                            .text_color(tokens.text_secondary)
                            .text_sm()
                            .child(text),
                    )
                }),
        )
        .child(
            h_flex()
                .gap_2()
                .flex_wrap()
                .child(
                    Button::new("upd-check")
                        .small()
                        .label(if phase == UpdatePhase::Checking {
                            "检查中..."
                        } else {
                            "检查更新"
                        })
                        .disabled(state.busy && phase != UpdatePhase::Downloading)
                        .on_click({
                            let on_demo = on_demo.clone();
                            move |_, window, cx| on_demo(window, cx, UpdatePhase::Available)
                        }),
                )
                .when(phase == UpdatePhase::Available, |this| {
                    this.child(
                        Button::new("upd-download")
                            .small()
                            .primary()
                            .label("下载 2.4.11 (40.0 MB)")
                            .on_click({
                                let on_demo = on_demo.clone();
                                move |_, window, cx| on_demo(window, cx, UpdatePhase::Downloading)
                            }),
                    )
                })
                .when(phase == UpdatePhase::Downloading, |this| {
                    this.child(
                        Button::new("upd-cancel")
                            .small()
                            .label("取消下载")
                            .on_click({
                                let on_demo = on_demo.clone();
                                move |_, window, cx| on_demo(window, cx, UpdatePhase::Idle)
                            }),
                    )
                })
                .when(phase == UpdatePhase::Downloaded, |this| {
                    this.child(
                        Button::new("upd-install")
                            .small()
                            .primary()
                            .label("安装更新并重启")
                            .on_click({
                                let on_demo = on_demo.clone();
                                move |_, window, cx| on_demo(window, cx, UpdatePhase::Current)
                            }),
                    )
                }),
        )
        .when(phase == UpdatePhase::Downloading, |this| {
            let pct = state.progress.as_ref().map(|p| p.percent).unwrap_or(0.0);
            let text = state
                .progress
                .as_ref()
                .map(|p| {
                    if p.total > 0 {
                        format!(
                            "{:.1}% ({} / {})",
                            p.percent,
                            format_asset_size(p.downloaded),
                            format_asset_size(p.total)
                        )
                    } else {
                        format!("已下载 {}", format_asset_size(p.downloaded))
                    }
                })
                .unwrap_or_default();
            this.child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .w_full()
                            .h(px(6.))
                            .rounded_sm()
                            .bg(tokens.border_soft)
                            .child(
                                div()
                                    .h_full()
                                    .w(relative((pct / 100.0).clamp(0.0, 1.0)))
                                    .bg(tokens.accent)
                                    .rounded_sm(),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(tokens.text_secondary)
                            .child(text),
                    ),
            )
        })
        .when(phase == UpdatePhase::Error, |this| {
            let err = state
                .info
                .as_ref()
                .and_then(|i| i.error.clone())
                .unwrap_or_default();
            this.child(div().text_color(tokens.error).text_sm().child(err))
        })
        .when(!notes.is_empty(), |this| {
            let latest = state
                .info
                .as_ref()
                .and_then(|i| i.latest_version.clone())
                .unwrap_or_default();
            let current = state
                .info
                .as_ref()
                .map(|i| i.current_version.clone())
                .unwrap_or_default();
            this.child(
                v_flex()
                    .gap_1()
                    .p_2()
                    .rounded_sm()
                    .bg(tokens.hover)
                    .child(
                        div()
                            .text_color(tokens.text_primary)
                            .child(format!("新版本 {latest}（当前 {current}）")),
                    )
                    .child(
                        div()
                            .text_color(tokens.text_secondary)
                            .text_sm()
                            .child(notes),
                    ),
            )
        })
        .child(
            Checkbox::new("auto-check")
                .checked(state.auto_check)
                .label("启动时自动检查更新（仅提示，不自动下载）")
                .on_change({
                    let on_auto_check = on_auto_check.clone();
                    move |checked, window, cx| on_auto_check(window, cx, *checked)
                }),
        )
        .child(
            div()
                .text_sm()
                .text_color(tokens.text_tertiary)
                .child("本批不连接 GitHub，按钮只切换占位状态。真实检查、下载、安装是后续阶段。"),
        )
        .child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .child(
                    div()
                        .text_sm()
                        .text_color(tokens.text_tertiary)
                        .child("演示状态："),
                )
                .children(
                    [
                        (UpdatePhase::Idle, "空闲"),
                        (UpdatePhase::Checking, "检查中"),
                        (UpdatePhase::Available, "有新版本"),
                        (UpdatePhase::Downloading, "下载中"),
                        (UpdatePhase::Downloaded, "已下载"),
                        (UpdatePhase::Current, "已是最新"),
                        (UpdatePhase::Error, "失败"),
                    ]
                    .into_iter()
                    .map(move |(phase, label)| {
                        Button::new(SharedString::from(format!("demo-{label}")))
                            .xsmall()
                            .ghost()
                            .label(label)
                            .on_click({
                                let on_demo = on_demo.clone();
                                move |_, window, cx| on_demo(window, cx, phase)
                            })
                    }),
                ),
        )
}
