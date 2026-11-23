use super::browser_table::{BrowserTable, DEFAULT_WIDTHS};
use crate::{
    browser::{self, FileAction},
    preview::{self, PreviewKind, PreviewState},
    preview_worker::{Cancel, PdfOptions},
    theme::FindxTokens,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::component::{Disableable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
pub struct BrowserWindow {
    path: Entity<InputState>,
    target: Entity<InputState>,
    table: Entity<TableState<BrowserTable>>,
    current: PathBuf,
    history: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    selected: Option<usize>,
    generation: u64,
    cancel: Cancel,
    preview_cancel: Cancel,
    preview_gen: u64,
    preview: PreviewState,
    pdf: PdfOptions,
    status: String,
    busy: bool,
    operation: Option<FileAction>,
    confirmed: bool,
    subscriptions: Vec<Subscription>,
}
impl BrowserWindow {
    pub fn new(initial: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(initial.to_string_lossy().into_owned())
                .placeholder("目录完整路径")
        });
        let target = cx
            .new(|cx| InputState::new(window, cx).placeholder("新名称 / 目标完整路径（含文件名）"));
        let table = cx.new(|cx| {
            TableState::new(
                BrowserTable {
                    hits: vec![],
                    widths: DEFAULT_WIDTHS,
                    more: false,
                },
                window,
                cx,
            )
            .col_resizable(true)
            .row_selectable(true)
            .col_movable(false)
        });
        let mut s = Self {
            path,
            target,
            table: table.clone(),
            current: initial.clone(),
            history: vec![],
            forward: vec![],
            selected: None,
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            preview_cancel: Arc::new(AtomicBool::new(false)),
            preview_gen: 0,
            preview: PreviewState::empty(),
            pdf: Default::default(),
            status: "正在读取目录…".into(),
            busy: false,
            operation: None,
            confirmed: false,
            subscriptions: vec![],
        };
        s.subscriptions.push(cx.subscribe_in(
            &table,
            window,
            |this, _, ev: &TableEvent, window, cx| match ev {
                TableEvent::SelectRow(ix) => this.select(*ix, cx),
                TableEvent::DoubleClickedRow(ix) => {
                    this.select(*ix, cx);
                    this.enter(window, cx);
                }
                TableEvent::ColumnWidthsChanged(w) if w.len() == 4 => {
                    let widths = std::array::from_fn(|i| w[i].as_f32());
                    this.table
                        .update(cx, |t, _| t.delegate_mut().widths = widths);
                }
                _ => {}
            },
        ));
        let target = s.target.clone();
        s.subscriptions.push(cx.subscribe_in(
            &target,
            window,
            |this, _, ev: &gpui_kit::component::input::InputEvent, _, cx| {
                if let gpui_kit::component::input::InputEvent::Change = ev {
                    this.confirmed = false;
                    cx.notify();
                }
            },
        ));
        s.navigate(initial, false, window, cx);
        s
    }
    fn navigate(
        &mut self,
        path: PathBuf,
        remember: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.cancel.store(true, Ordering::Release);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.generation = self.generation.wrapping_add(1);
        let gen_id = self.generation;
        let old = self.current.clone();
        self.preview_cancel.store(true, Ordering::Release);
        self.preview = PreviewState::empty();
        self.selected = None;
        self.status = "正在读取目录（不使用搜索索引）…".into();
        self.operation = None;
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_spawn(async move { browser::list(&path, &cancel) })
                .await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.generation != gen_id {
                        return;
                    }
                    match out {
                        Ok((path, rows, cap)) => {
                            if remember && path != old {
                                this.history.push(old);
                                this.forward.clear();
                            }
                            this.current = path.clone();
                            this.path.update(cx, |i, cx| {
                                i.set_value(path.to_string_lossy().into_owned(), window, cx)
                            });
                            this.status = format!(
                                "{} 项{}，双击目录进入；选中文件可预览",
                                rows.len(),
                                if cap {
                                    "（达到 100000 项安全上限）"
                                } else {
                                    ""
                                }
                            );
                            this.table.update(cx, |t, cx| {
                                t.delegate_mut().hits = rows;
                                t.refresh(cx);
                            });
                        }
                        Err(e) => this.status = format!("读取失败：{e}"),
                    };
                    cx.notify();
                });
            });
        })
        .detach();
        cx.notify();
    }
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        self.pdf = Default::default();
        self.operation = None;
        self.request_preview(cx);
    }
    fn request_preview(&mut self, cx: &mut Context<Self>) {
        let Some(hit) = self
            .selected
            .and_then(|i| self.table.read(cx).delegate().hits.get(i).cloned())
        else {
            return;
        };
        self.preview_cancel.store(true, Ordering::Release);
        self.preview_cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.preview_cancel.clone();
        self.preview_gen = self.preview_gen.wrapping_add(1);
        let gen_id = self.preview_gen;
        let pdf = self.pdf;
        self.preview = PreviewState::loading(
            gen_id,
            preview::meta_from_hit(&hit.name, &hit.path, hit.size, hit.mtime, hit.is_directory),
        );
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    preview::load_preview_with_options(
                        gen_id,
                        &hit.name,
                        &hit.path,
                        hit.size,
                        hit.mtime,
                        hit.is_directory,
                        pdf,
                        false,
                        cancel,
                    )
                    .await
                })
                .await;
            this.update(cx, |this, cx| {
                if this.preview_gen == gen_id {
                    this.preview = loaded;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
    fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(hit) = self
            .selected
            .and_then(|i| self.table.read(cx).delegate().hits.get(i).cloned())
        {
            if hit.is_directory {
                self.navigate(hit.path.into(), true, window, cx);
            }
        }
    }
    fn start_operation(&mut self, action: FileAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self
            .selected
            .and_then(|i| self.table.read(cx).delegate().hits.get(i))
        else {
            return;
        };
        let text = if action == FileAction::Rename {
            hit.name.clone()
        } else {
            self.current
                .join(format!("副本-{}", hit.name))
                .to_string_lossy()
                .into_owned()
        };
        self.target
            .update(cx, |i, cx| i.set_value(text, window, cx));
        self.operation = Some(action);
        self.confirmed = false;
        cx.notify();
    }
    fn execute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.operation else {
            return;
        };
        let Some(hit) = self
            .selected
            .and_then(|i| self.table.read(cx).delegate().hits.get(i).cloned())
        else {
            return;
        };
        let src = PathBuf::from(hit.path);
        let dst = match browser::destination(action, &src, &self.target.read(cx).value()) {
            Ok(d) => d,
            Err(e) => {
                self.status = e;
                cx.notify();
                return;
            }
        };
        if !self.confirmed {
            self.confirmed = true;
            self.status = format!(
                "确认{}：{} → {}；不覆盖已有项。再次点击确认执行。",
                action.label(),
                src.display(),
                dst.display()
            );
            cx.notify();
            return;
        }
        self.busy = true;
        self.cancel.store(true, Ordering::Release);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { browser::operate(action, &src, &dst, &cancel) })
                .await;
            let _ = handle.update(cx, |_, w, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    this.operation = None;
                    let path = this.current.clone();
                    this.navigate(path, false, w, cx);
                    this.status = match result {
                        Ok(()) => format!("{}完成；已刷新目录", action.label()),
                        Err(e) => format!("{}失败：{e}", action.label()),
                    };
                    cx.notify();
                });
            });
        })
        .detach();
        cx.notify();
    }
}
impl Drop for BrowserWindow {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.preview_cancel.store(true, Ordering::Release);
    }
}
impl Render for BrowserWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.global::<FindxTokens>();
        let entity = cx.entity();
        let buttons = [
            ("back", "后退"),
            ("forward", "前进"),
            ("up", "上级"),
            ("go", "转到"),
            ("refresh", "刷新"),
            ("enter", "进入目录"),
        ];
        let toolbar = h_flex()
            .w_full()
            .flex_shrink_0()
            .p_2()
            .gap_1()
            .flex_wrap()
            .children(buttons.into_iter().map(|(id, label)| {
                let e = entity.clone();
                Button::new(id)
                    .label(label)
                    .disabled(self.busy)
                    .on_click(move |_, w, cx| {
                        e.update(cx, |this, cx| match id {
                            "back" => {
                                if let Some(p) = this.history.pop() {
                                    this.forward.push(this.current.clone());
                                    this.navigate(p, false, w, cx);
                                }
                            }
                            "forward" => {
                                if let Some(p) = this.forward.pop() {
                                    this.history.push(this.current.clone());
                                    this.navigate(p, false, w, cx);
                                }
                            }
                            "up" => {
                                if let Some(p) = this.current.parent() {
                                    this.navigate(p.to_path_buf(), true, w, cx);
                                }
                            }
                            "go" => this.navigate(
                                PathBuf::from(this.path.read(cx).value().to_string()),
                                true,
                                w,
                                cx,
                            ),
                            "enter" => this.enter(w, cx),
                            _ => this.navigate(this.current.clone(), false, w, cx),
                        })
                    })
            }));
        let mut preview_body = v_flex().w_full().gap_2().p_3();
        if let Some(meta) = &self.preview.meta {
            preview_body = preview_body
                .child(div().text_sm().child(meta.name.clone()))
                .child(div().text_xs().child(preview::format_meta_line(meta)))
                .child(div().text_xs().child(meta.path.clone()));
        }
        if let Some(pdf) = self.preview.pdf {
            preview_body = preview_body.child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(div().child(format!("{} / {}", self.pdf.page + 1, pdf.count)))
                    .children(
                        [
                            ("prev", "上一页"),
                            ("next", "下一页"),
                            ("zoom", "放大"),
                            ("fit", "适宽"),
                        ]
                        .into_iter()
                        .map(|(id, label)| {
                            let e = entity.clone();
                            Button::new(id)
                                .ghost()
                                .label(label)
                                .on_click(move |_, _, cx| {
                                    e.update(cx, |this, cx| {
                                        match id {
                                            "prev" => this.pdf.navigate(-1, pdf.count),
                                            "next" => this.pdf.navigate(1, pdf.count),
                                            "zoom" => this.pdf.scale(1.25),
                                            _ => this.pdf.fit_width = true,
                                        }
                                        this.request_preview(cx);
                                    })
                                })
                        }),
                    ),
            );
        }
        if let Some(image) = self.preview.image.clone() {
            let width = if self.preview.pdf.is_some() && !self.pdf.fit_width {
                self.preview.pdf.unwrap().width * self.pdf.zoom
            } else {
                280.
            };
            preview_body =
                preview_body.child(img(image).w(px(width)).object_fit(ObjectFit::Contain));
        }
        if let Some(note) = &self.preview.text_encoding {
            preview_body = preview_body.child(div().text_xs().child(note.clone()));
        }
        if let Some(text) = &self.preview.text {
            preview_body = preview_body.child(div().text_sm().child(text.clone()));
        }
        if let Some(note) = &self.preview.message {
            preview_body = preview_body.child(div().text_sm().child(note.clone()));
        }
        if self.preview.kind == PreviewKind::Directory {
            for e in &self.preview.dir_entries {
                preview_body = preview_body.child(div().text_sm().child(format!(
                    "{} {}",
                    if e.is_dir { "目录" } else { "文件" },
                    e.name
                )));
            }
        }
        v_flex()
            .size_full()
            .overflow_hidden()
            .bg(tokens.bg)
            .text_color(tokens.text_primary)
            .child(toolbar)
            .child(
                div()
                    .w_full()
                    .px_2()
                    .pb_2()
                    .flex_shrink_0()
                    .child(Input::new(&self.path)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .p_2()
                    .flex_shrink_0()
                    .flex_wrap()
                    .children(
                        [FileAction::Copy, FileAction::Move, FileAction::Rename]
                            .into_iter()
                            .map(|action| {
                                let e = entity.clone();
                                Button::new(SharedString::from(action.label()))
                                    .label(action.label())
                                    .disabled(self.busy || self.selected.is_none())
                                    .on_click(move |_, w, cx| {
                                        e.update(cx, |this, cx| this.start_operation(action, w, cx))
                                    })
                            }),
                    )
                    .child({
                        let e = entity.clone();
                        Button::new("cancel-work")
                            .label("取消当前读取/复制")
                            .on_click(move |_, _, cx| {
                                e.update(cx, |this, cx| {
                                    this.cancel.store(true, Ordering::Release);
                                    this.status = "已请求取消；已提交的移动/重命名不能撤销".into();
                                    cx.notify();
                                })
                            })
                    }),
            )
            .when(self.operation.is_some(), |v| {
                v.child(
                    h_flex()
                        .gap_2()
                        .p_2()
                        .flex_shrink_0()
                        .child(div().flex_1().min_w_0().child(Input::new(&self.target)))
                        .child({
                            let e = entity.clone();
                            Button::new("execute-fileop")
                                .label(if self.confirmed {
                                    "确认执行"
                                } else {
                                    "检查并确认目标"
                                })
                                .disabled(self.busy)
                                .on_click(move |_, w, cx| {
                                    e.update(cx, |this, cx| this.execute(w, cx))
                                })
                        }),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .w_full()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(DataTable::new(&self.table).scrollbar_visible(true, true)),
                    )
                    .child(
                        div()
                            .id("browse-preview")
                            .w(px(320.))
                            .h_full()
                            .flex_shrink_0()
                            .overflow_scrollbar()
                            .border_l_1()
                            .border_color(tokens.border)
                            .child(preview_body),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .p_2()
                    .text_xs()
                    .child(self.status.clone()),
            )
    }
}
pub fn open(initial: PathBuf, cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1080.), px(700.)),
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(480.))),
        titlebar: Some(TitlebarOptions {
            title: Some("FindX 目录浏览".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let _ = gpui_kit::open_window(options, cx, move |w, cx| {
        w.set_window_title("FindX 目录浏览");
        cx.new(|cx| BrowserWindow::new(initial, w, cx))
    });
}
