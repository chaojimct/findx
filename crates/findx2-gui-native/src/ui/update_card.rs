use crate::theme::FindxTokens;
use crate::update_logic::UpdatePhase;
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{Disableable as _, v_flex};
use gpui_kit::*;
use std::rc::Rc;
pub struct UpdateCardState {
    pub auto_check: bool,
}
impl UpdateCardState {
    pub fn new(auto_check: bool) -> Self {
        Self { auto_check }
    }
    pub fn set_demo(&mut self, _: UpdatePhase) {}
}
pub fn render_update_card(
    state: &UpdateCardState,
    tokens: FindxTokens,
    on_auto_check: Rc<dyn Fn(&mut Window, &mut App, bool)>,
    _on_demo: Rc<dyn Fn(&mut Window, &mut App, UpdatePhase)>,
) -> impl IntoElement {
    v_flex().w_full().gap_2().p_3().bg(tokens.surface).border_1().border_color(tokens.border).rounded_md()
    .child(div().child("应用更新 · 未连接安装协议"))
    .child(div().text_sm().child(format!("当前 native-dev 版本 {}",env!("CARGO_PKG_VERSION"))))
    .child(div().text_sm().child("此开发客户端不检查、下载或安装更新。更新请使用现有正式客户端；不展示虚构的新版本或下载进度。"))
    .child(Button::new("update-protected").label("检查 / 安装更新（不可操作）").disabled(true))
    .child(Checkbox::new("auto-check").checked(state.auto_check).label("自动检查偏好（只保存，本客户端不执行）").on_change(move|checked,w,cx|on_auto_check(w,cx,*checked)))
}
