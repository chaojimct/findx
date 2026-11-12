use super::*;
impl MainWindow {
    pub(super) fn ensure_selection_identity(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(hit) = self.active_workspace_hit() else {
            self.identity_key = None;
            self.selected_identity = None;
            return false;
        };
        let key = workspace::preview_stamp(&hit.path, hit.size, hit.mtime);
        if self.identity_key.as_ref() == Some(&key) {
            return self.selected_identity.is_some()
                && !self.identity_pending
                && self.identity_error.is_none();
        }
        self.identity_key = Some(key.clone());
        self.identity_generation = self.identity_generation.wrapping_add(1);
        let generation = self.identity_generation;
        self.selected_identity = None;
        self.identity_pending = true;
        self.identity_error = None;
        self.pending_open = None;
        self.file_actions.pending_shell = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let stamp = crate::file_identity::capture(std::path::Path::new(&hit.path))
                        .map_err(|_| {
                            "位置不可用：文件可能已删除、权限不足或磁盘已断开。".to_string()
                        })?;
                    if !hit.is_directory
                        && ((hit.size != 0 && hit.size != stamp.size)
                            || (hit.mtime != 0
                                && search_logic::unix_secs_from_mtime(hit.mtime)
                                    != search_logic::unix_secs_from_mtime(stamp.modified)))
                    {
                        return Err("条目已变化或索引尚未更新；请刷新目录后重新选择。".into());
                    }
                    Ok::<_, String>(stamp)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.identity_generation != generation
                    || this.identity_key.as_ref() != Some(&key)
                {
                    return;
                }
                this.identity_pending = false;
                match result {
                    Ok(stamp) => {
                        this.selected_identity = Some(stamp);
                        this.preview_active_selection(cx);
                        if let Some(dialog) = this.file_actions.pending_shell.take() {
                            this.open_shell_dialog(dialog, cx);
                        }
                        if let Some(hit) = this.pending_open.take() {
                            this.activate_system_file(hit, cx);
                        }
                    }
                    Err(e) => {
                        this.identity_error = Some(e.clone());
                        this.file_actions.summary = e.clone();
                        this.workspace.status = e;
                        this.pause_preview();
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        false
    }
    pub(super) fn activate_system_file(&mut self, hit: SearchHitDto, cx: &mut Context<Self>) {
        if self.run_pending.is_some() {
            return;
        }
        if !self.ensure_selection_identity(cx) {
            if self.identity_pending {
                self.pending_open = Some(hit);
            }
            cx.notify();
            return;
        }
        let Some(stamp) = self.selected_identity else {
            return;
        };
        if crate::open_path::requires_run_confirmation(&hit.path) {
            self.pause_preview();
            self.run_pending = Some((hit, stamp));
            cx.notify();
        } else {
            self.perform_system_open(hit, stamp, cx);
        }
    }
    fn perform_system_open(
        &mut self,
        hit: SearchHitDto,
        stamp: crate::file_identity::FileStamp,
        cx: &mut Context<Self>,
    ) {
        self.pause_preview();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    crate::file_identity::revalidate(std::path::Path::new(&hit.path), stamp)?;
                    crate::open_path::open_path(&hit.path)
                })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.transient_file_notice("已交给系统打开".into(), cx),
                    Err(e) => {
                        this.workspace.status = e.clone();
                        this.file_actions.summary = e;
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    pub(super) fn render_run_confirmation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Self::tokens(cx);
        let name = self
            .run_pending
            .as_ref()
            .map(|(h, _)| h.name.clone())
            .unwrap_or_default();
        div().absolute().inset_0().occlude().bg(rgba(0x00000066)).flex().items_center().justify_center().child(v_flex().w(px(440.)).max_w_full().m_4().p_5().gap_4().rounded_lg().border_1().border_color(t.border).bg(t.surface_raised).child("确认交给系统运行").child(div().text_sm().text_ellipsis().child(name)).child(div().text_sm().text_color(t.text_secondary).child("此类型可能执行程序、脚本或快捷方式。仅在确认来源可信时继续；不会自动提升权限。"))
 .child(h_flex().justify_end().gap_2().child(Button::new("cancel-system-run").label("取消").on_click(cx.listener(|this,_,_,cx|{this.run_pending=None;cx.notify();}))).child(Button::new("confirm-system-run").primary().label("确认打开 / 运行").on_click(cx.listener(|this,_,_,cx|{if let Some((hit,stamp))=this.run_pending.take(){this.perform_system_open(hit,stamp,cx);}})))))
    }
}
