use super::*;
impl MainWindow {
    pub(super) fn ensure_selection_identity(
        &mut self,
        hit: &SearchHitDto,
        cx: &mut Context<Self>,
    ) -> bool {
        let hit = hit.clone();
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
                        if let Some((dialog, hit)) = this.file_actions.pending_shell.take() {
                            this.open_shell_dialog(hit, dialog, cx);
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
        if !self.ensure_selection_identity(&hit, cx) {
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
    /// 在操作系统文件管理器中显示该项（Windows 资源管理器 / macOS Finder /
    /// Linux 文件管理器），而不是把应用内视图跳到父目录。Shell 调用必须在后台
    /// 任务里做，所以走 background_spawn，结果再回到主线程给出提示。
    pub(super) fn reveal_in_file_manager(&mut self, hit: &SearchHitDto, cx: &mut Context<Self>) {
        let path = hit.path.clone();
        self.pause_preview();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { crate::open_path::reveal_in_folder(&path) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.transient_file_notice("已在文件管理器中显示".into(), cx),
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

    /// Sidebar rows are plain paths (not search hits), so they get the same
    /// Explorer reveal through a dedicated entry point.
    pub(super) fn reveal_sidebar_path(&mut self, path: &str, cx: &mut Context<Self>) {
        let path = path.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { crate::open_path::reveal_in_folder(&path) })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.transient_file_notice("已在文件管理器中显示".into(), cx),
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
