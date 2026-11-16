use crate::theme::FindxTokens;
use crate::update_logic::{OfficialUpdateState, UpdateCardModel, update_card_model};
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
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
        .child(div().child(model.title))
        .child(div().text_sm().child(model.version_line))
        .child(div().text_sm().child(model.body))
        .when_some(model.status, |v, status| v.child(div().text_sm().child(status)))
        .when_some(model.progress_text, |v, text| v.child(div().text_sm().child(text)))
        .child(
            Button::new("update-action")
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
