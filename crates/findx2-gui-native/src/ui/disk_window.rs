use crate::{
    disk_usage::{self, Limits, Progress, Scan},
    search_logic::format_size,
    theme::FindxTokens,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{Disableable as _, h_flex, v_flex};
use gpui_kit::*;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub struct DiskWindow {
    path: Entity<InputState>,
    scan: Option<Scan>,
    current: usize,
    detail: Option<usize>,
    cancel: Arc<AtomicBool>,
    progress: Arc<Progress>,
    generation: u64,
    busy: bool,
    status: String,
    _tick: Task<()>,
}
impl DiskWindow {
    pub fn new(w: &mut Window, cx: &mut Context<Self>) -> Self {
        let path =
            cx.new(|cx| InputState::new(w, cx).placeholder("明确选择扫描目录，不自动扫描整个磁盘"));
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if this.busy {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            path,
            scan: None,
            current: 0,
            detail: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Progress::default()),
            generation: 0,
            busy: false,
            status: "只读分析：请选择一个目录再扫描；没有删除操作，也不读取搜索分页作为统计。"
                .into(),
            _tick: tick,
        }
    }
    fn start(&mut self, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Release);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.progress = Arc::new(Progress::default());
        self.generation = self.generation.wrapping_add(1);
        let gen_id = self.generation;
        let path = PathBuf::from(self.path.read(cx).value().to_string());
        let cancel = self.cancel.clone();
        let p = self.progress.clone();
        self.busy = true;
        self.scan = None;
        self.detail = None;
        self.status = "扫描在后台进行，搜索窗口保持独立可用…".into();
        cx.spawn(async move|this,cx|{let result=cx.background_spawn(async move{disk_usage::scan(&path,cancel,p,Limits::default())}).await;this.update(cx,|this,cx|{if this.generation!=gen_id{return;}this.busy=false;match result{Ok(s)=>{this.status=format!("{}，{} 个路径，{} 秒，硬链接去重 {}，跳过链接/挂载/离线 {}，错误 {}，未知占用 {}",if s.cancelled{"已取消，部分结果"}else if s.partial{"部分扫描（不是全盘结论）"}else{"目录扫描完成"},s.nodes.len(),s.elapsed.as_secs(),s.hardlinks,s.skipped_links,s.errors,s.unknown);this.current=0;this.scan=Some(s);},Err(e)=>this.status=format!("扫描失败：{e}")};cx.notify();}).ok();}).detach();
        cx.notify();
    }
}
impl Drop for DiskWindow {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Render for DiskWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.global::<FindxTokens>();
        let entity = cx.entity();
        let mut tree = div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(tokens.surface);
        let mut list = v_flex().gap_1().p_2();
        let mut title = "没有扫描结果".to_string();
        let mut detail="逻辑大小按路径统计；实际占用按卷+FileId 去重。树图只表示已读取的目录，不代表删除可释放量。Windows 文件系统元数据/快照/权限不可读项不在此统计中。".to_string();
        if let Some(s) = &self.scan {
            let node = &s.nodes[self.current];
            title = format!(
                "{}，逻辑 {}，实际占用 {}",
                node.path.display(),
                format_size(node.logical, false),
                format_size(node.allocated, false)
            );
            let mut children = node.children.clone();
            children.sort_by_key(|&i| std::cmp::Reverse(s.nodes[i].allocated));
            let weights = children
                .iter()
                .take(127)
                .map(|&i| (i, s.nodes[i].allocated))
                .chain(std::iter::once((
                    usize::MAX,
                    children
                        .iter()
                        .skip(127)
                        .map(|&i| s.nodes[i].allocated)
                        .sum(),
                )))
                .collect::<Vec<_>>();
            for tile in disk_usage::tiles(&weights) {
                let i = tile.index;
                let n = if i == usize::MAX {
                    None
                } else {
                    Some(&s.nodes[i])
                };
                let label = n
                    .map(|n| {
                        format!(
                            "{}\n{}",
                            n.path.file_name().unwrap_or_default().to_string_lossy(),
                            format_size(n.allocated, false)
                        )
                    })
                    .unwrap_or_else(|| "其他项目（列表可查看）".into());
                let color = [0x245b78, 0x406c42, 0x6d4c7d, 0x8a632e, 0x3d6673, 0x664949]
                    [i.wrapping_add(1) % 6];
                let e = entity.clone();
                tree = tree.child(
                    div()
                        .id(ElementId::Name(format!("tile-{i}").into()))
                        .absolute()
                        .left(relative(tile.x))
                        .top(relative(tile.y))
                        .w(relative(tile.w))
                        .h(relative(tile.h))
                        .p_1()
                        .border_1()
                        .border_color(tokens.border)
                        .bg(rgb(color))
                        .text_color(rgb(0xffffff))
                        .overflow_hidden()
                        .text_sm()
                        .child(
                            div()
                                .w_full()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .child(label.replace('\n', "，")),
                        )
                        .on_click(move |_, _, cx| {
                            e.update(cx, |this, cx| {
                                if i == usize::MAX {
                                    return;
                                }
                                this.detail = Some(i);
                                if this.scan.as_ref().is_some_and(|s| s.nodes[i].directory) {
                                    this.current = i;
                                }
                                cx.notify();
                            })
                        }),
                );
            }
            // Bounded visible listing is explicit; tree retains up to 500k nodes and supports drilling.
            for &i in children.iter().take(500) {
                let n = &s.nodes[i];
                let e = entity.clone();
                list = list.child(
                    Button::new(SharedString::from(format!("disk-node-{i}")))
                        .ghost()
                        .label(format!(
                            "{}，{}{}{}",
                            n.path.file_name().unwrap_or_default().to_string_lossy(),
                            format_size(n.allocated, false),
                            if n.hardlink_duplicate {
                                "，硬链接重复计 0"
                            } else {
                                ""
                            },
                            if n.uncertain { "，占用未知" } else { "" }
                        ))
                        .on_click(move |_, _, cx| {
                            e.update(cx, |this, cx| {
                                this.detail = Some(i);
                                if this.scan.as_ref().is_some_and(|s| s.nodes[i].directory) {
                                    this.current = i;
                                }
                                cx.notify();
                            })
                        }),
                );
            }
            if children.len() > 500 {
                list = list.child(div().text_xs().child(format!(
                    "按实际占用显示前 500 / {} 个直接子项；不是完整项目列表",
                    children.len()
                )));
            }
            if let Some(i) = self.detail {
                let n = &s.nodes[i];
                detail = format!(
                    "{}\n逻辑 {}，实际占用 {}{}{}",
                    n.path.display(),
                    format_size(n.logical, false),
                    format_size(n.allocated, false),
                    if n.hardlink_duplicate {
                        "，硬链接已在其他路径计入"
                    } else {
                        ""
                    },
                    if n.uncertain {
                        "，实际占用未知，非零占用保证"
                    } else {
                        ""
                    }
                );
            }
        }
        v_flex().size_full().overflow_hidden().bg(tokens.bg).text_color(tokens.text_primary)
 .child(h_flex().p_2().gap_2().w_full().flex_shrink_0().child(div().flex_1().min_w_0().child(Input::new(&self.path)))
 .child({let e=entity.clone();Button::new("scan-space").label("扫描目录").on_click(move|_,_,cx|e.update(cx,|this,cx|this.start(cx)))})
 .child({let e=entity.clone();Button::new("cancel-space").label("取消扫描").disabled(!self.busy).on_click(move|_,_,cx|e.update(cx,|this,cx|{this.cancel.store(true,Ordering::Release);cx.notify();}))})
 .child({let e=entity.clone();Button::new("space-up").label("上级树图").on_click(move|_,_,cx|e.update(cx,|this,cx|{if let Some(parent)=this.scan.as_ref().and_then(|s|s.nodes[this.current].parent){this.current=parent;cx.notify();}}))}))
 .child(div().flex_shrink_0().p_2().text_sm().child(title))
 .child(h_flex().flex_1().min_h_0().items_stretch().w_full().child(tree).child(div().id("disk-child-list").w(px(340.)).h_full().flex_shrink_0().overflow_y_scrollbar().child(list)))
 .child(div().flex_shrink_0().p_2().text_xs().child(detail))
 .child(div().flex_shrink_0().p_2().text_xs().child(if self.busy{format!("扫描中，已读 {} 项 / {} 目录，实际占用累计 {}，同卷、不跟随链接；最多 500000 项 / 300 秒",self.progress.entries.load(Ordering::Relaxed),self.progress.directories.load(Ordering::Relaxed),format_size(self.progress.allocated.load(Ordering::Relaxed),false))}else{self.status.clone()}))
    }
}
pub fn open(cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1080.), px(700.)),
            cx,
        ))),
        window_min_size: Some(size(px(720.), px(480.))),
        titlebar: Some(TitlebarOptions {
            title: Some("FindX 磁盘占用（只读）".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let _ = gpui_kit::open_window(options, cx, |w, cx| {
        w.set_window_title("FindX 磁盘占用（只读）");
        cx.new(|cx| DiskWindow::new(w, cx))
    });
}
