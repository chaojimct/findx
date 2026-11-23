use crate::theme::FindxTokens;
use crate::update_logic::{OfficialUpdateState, UpdateCardModel, update_card_model};
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Disableable as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

pub struct UpdateCardState {
    pub auto_check: bool,
}

impl UpdateCardState {
    pub fn new(auto_check: bool) -> Self {
        Self { auto_check }
    }
}

pub fn render_update_card(
    official_product: bool,
    current_version: &str,
    runtime: OfficialUpdateState,
    auto_check: bool,
    tokens: FindxTokens,
    on_auto_check: Rc<dyn Fn(&mut Window, &mut App, bool)>,
    on_action: Rc<dyn Fn(&mut Window, &mut App)>,
) -> impl IntoElement {
    let model = update_card_model(official_product, current_version, &runtime, auto_check);
    render_update_card_model(model, auto_check, tokens, on_auto_check, on_action)
}

fn render_update_card_model(
    model: UpdateCardModel,
    auto_check: bool,
    tokens: FindxTokens,
    on_auto_check: Rc<dyn Fn(&mut Window, &mut App, bool)>,
    on_action: Rc<dyn Fn(&mut Window, &mut App)>,
) -> impl IntoElement {
    let button_enabled = model.button_enabled;
    v_flex()
        .w_full()
        .gap_2()
        .p_3()
        .bg(tokens.surface)
        .border_1()
        .border_color(tokens.border)
        .rounded_md()
        .child(div().h(px(20.)).flex_shrink_0().text_ellipsis().child(model.title))
        .child(div().h(px(20.)).flex_shrink_0().text_sm().text_ellipsis().child(model.version_line))
        .child(div().id("update-body").h(px(64.)).flex_shrink_0().overflow_y_scrollbar().text_sm().child(model.body))
        .child(div().id("update-status").h(px(20.)).flex_shrink_0().text_sm().text_ellipsis()
            .child(model.status.unwrap_or_default()))
        .child(div().id("update-progress").h(px(20.)).flex_shrink_0().text_sm().text_ellipsis()
            .child(model.progress_text.unwrap_or_default()))
        // Permanent row: the bar appears for download/install, the row itself
        // never does, so the button below never shifts (see the reserved-rows
        // regression test).
        .child(
            div()
                .id("update-progress-bar")
                .h(px(12.))
                .flex_shrink_0()
                .w_full()
                .items_center()
                .when_some(model.progress_bar, |this, bar| {
                    this.child(
                        Progress::new("update-progress-fill")
                            .loading(bar.percent.is_none())
                            .value(bar.percent.unwrap_or(0.0))
                            .accessibility_label("更新进度"),
                    )
                }),
        )
        .child(
            Button::new("update-action")
                .w(px(192.)).flex_shrink_0()
                .label(model.button_label)
                .disabled(!button_enabled)
                .on_click(move |_, window, cx| {
                    if button_enabled {
                        on_action(window, cx);
                    }
                }),
        )
        .child(
            Checkbox::new("auto-check")
                .checked(auto_check)
                .label(model.auto_check_label)
                .on_change(move |checked, w, cx| on_auto_check(w, cx, *checked)),
        )
}
