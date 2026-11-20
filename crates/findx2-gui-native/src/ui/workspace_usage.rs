//! One Canvas/viewport, actual DiskTree layout/View/LayoutTransition source port.
//! Paint and every input use the exact same sampled frame rectangles. No tile div
//! or descendant occluding hitbox can swallow parent wheel events.
//! Paint strategy adapted from tobi/disktree@9b18b27 app/treemap_view.rs;
//! Copyright (c) 2026 Tobi Lütke. MIT: ../disktree/LICENSE.disktree.
use super::*;
use crate::disktree::viewport::View;
use crate::usage_view::{self, Rect, SceneKey, SemanticAction, TileKey};
use gpui_kit::component::menu::ContextMenuExt as _;
use std::time::Instant;
pub(super) struct Transition {motion:crate::usage_motion::Motion,backdrop:Vec<PaintTile>}
impl Transition{
 fn running(&self)->bool{self.motion.running(Instant::now())}
 fn sample(&self,k:TileKey,r:Rect,header:bool,now:Instant)->Rect{self.motion.sample(k,r,header,now)}
 fn old_frame(&self,now:Instant)->Vec<PaintTile>{self.backdrop.iter().cloned().map(|mut t|{t.rect=self.motion.backdrop(t.rect,now);t.header=t.header.map(|h|self.motion.backdrop(h,now));t}).collect()}
}
#[derive(Clone)]
struct PaintTile {
    key: TileKey,
    interactive: bool,
    rect: Rect,
    header: Option<Rect>,
    depth: usize,
    color: Hsla,
    opacity: f32,
    name: String,
    size: String,
    selected: bool,
    hovered: bool,
    unknown: bool,
    class: crate::disktree::classify::Category,
}
// Build a fresh frame from a reflowed immutable published scene. This is used
// only when the canvas reports a different ACTUAL viewport during prepaint;
// interpolating old coordinates (or scaling font/tiles) would produce blanks
// and a different target under the pointer. No IPC, classification or scan.
fn resized_scene_draws(
    scan: &Scan,
    scene: &usage_view::Scene,
    mut camera: usage_view::Camera,
    depth: usize,
    selected: Option<usize>,
    hover: Option<TileKey>,
    tokens: crate::theme::FindxTokens,
) -> (Vec<PaintTile>, usage_view::Camera) {
    camera.pan(0., 0., scene.viewport);
    let mut draws = Vec::with_capacity(scene.tiles.len());
    for tile in scene
        .tiles
        .iter()
        .filter(|t| usage_view::visible_at_depth(t.depth, depth))
    {
        let r = camera.rect(tile.rect);
        if r.w < 1.
            || r.h < 1.
            || r.x >= scene.viewport.w
            || r.y >= scene.viewport.h
            || r.x + r.w <= 0.
            || r.y + r.h <= 0.
        {
            continue;
        }
        let (name, unknown) = match tile.key {
            TileKey::Node(i) => {
                let n = &scan.nodes[i];
                (
                    n.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| display_path(&n.path)),
                    n.uncertain,
                )
            }
            TileKey::Other(_) => ("其他小项".into(), false),
        };
        draws.push(PaintTile {
            key: tile.key,
            interactive: true,
            rect: r,
            header: if tile.depth >= depth {
                None
            } else {
                tile.header.map(|h| camera.rect(h))
            },
            depth: tile.depth,
            color: tokens.usage_fill(tile.class, tile.depth),
            opacity: 1.,
            name,
            size: disk_usage::format_bytes(tile.bytes),
            unknown,
            class: tile.class,
            selected: matches!(tile.key,TileKey::Node(i) if Some(i)==selected),
            hovered: hover == Some(tile.key),
        });
    }
    (draws, camera)
}
fn tile_body(rect: Rect, header: Option<Rect>) -> Rect {
    if let Some(h) = header {
        let top = (h.y + h.h).max(rect.y);
        Rect {
            y: top,
            h: (rect.y + rect.h - top).max(0.),
            ..rect
        }
    } else {
        rect
    }
}
impl MainWindow {
    /// Retarget from the actual last painted rectangles. Old coverage is retained
    /// only as paint: its source node indices can never enter the new hit frame.
    fn usage_presented_camera(&self,scene:&usage_view::Scene)->usage_view::Camera{
        if self.workspace.usage_transition.is_some(){if let Some((_,r,_))=self.workspace.usage_frame.borrow().iter().find(|(k,_,_)|*k==TileKey::Node(scene.key.root)){let scale=(r.w/scene.viewport.w.max(1.)).max(0.001);return usage_view::Camera{scale,x:scene.viewport.x-r.x/scale,y:scene.viewport.y-r.y/scale}}}
        self.workspace.usage_camera
    }
    fn retarget_usage_camera(&mut self,target:usage_view::Camera,cx:&mut Context<Self>){
        let Some(scene)=self.workspace.usage_scene.as_ref()else{return};
        let input_started=Instant::now();
        let frames=self.workspace.usage_frame.borrow().clone();
        let src=frames.iter().find(|(k,_,_)|*k==TileKey::Node(scene.key.root)).map(|(_,r,_)|*r).unwrap_or(scene.viewport);
        let dst=target.rect(scene.viewport);let now=Instant::now();
        let backdrop=self.workspace.usage_transition.as_ref().map(|t|t.old_frame(now)).unwrap_or_default();
        self.workspace.usage_camera=target;
        self.workspace.usage_transition=if self.session.read(cx).prefs.usage_reduce_motion||frames.is_empty(){None}else{Some(Transition{motion:crate::usage_motion::Motion::new(frames,src,dst,now),backdrop})};
        if crate::isolation::development_var_os("FINDX_NATIVE_USAGE_TRACE").is_some(){eprintln!("usage_timing input prep_us={} duration_ms={} target_scale={}",input_started.elapsed().as_micros(),self.workspace.usage_transition.as_ref().map(|t|t.motion.duration_ms()).unwrap_or(0),target.scale);}
        cx.notify();
    }
    fn begin_usage_scene_motion(&mut self,new:&usage_view::Scene,new_scan:&Scan,cx:&Context<Self>){
        let handoff_started=Instant::now();
        let (Some(old),Some(scan))=(self.workspace.usage_scene.as_ref(),self.workspace.usage_scene_scan.as_ref())else{return};
        if old.viewport!=new.viewport{self.workspace.usage_transition=None;return} // resize genuinely reflows, never stretches old geometry
        let old_path=&scan.nodes[old.key.root].path;let new_path=&new_scan.nodes[new.key.root].path;
        let changed=super::key(old_path)!=super::key(new_path);let now=Instant::now();
        if !changed&&self.workspace.usage_transition.is_none(){return}
        let frame=self.workspace.usage_frame.borrow().clone();
        let mut by_path=std::collections::HashMap::new();
        for (key,r,h) in &frame{if let TileKey::Node(i)=key{if let Some(n)=scan.nodes.get(*i){by_path.insert(super::key(&n.path),(*r,*h,*i));}}}
        let old_key=super::key(old_path);let new_key=super::key(new_path);
        let descend=changed&&new_key.starts_with(&format!("{}\\",old_key.trim_end_matches('\\')));
        let ascend=changed&&old_key.starts_with(&format!("{}\\",new_key.trim_end_matches('\\')));
        let region=if descend{by_path.get(&new_key).map(|(r,h,_)|(tile_body(*r,*h),new.viewport))}
        else if ascend{new.tiles.iter().find(|t|matches!(t.key,TileKey::Node(i) if super::key(&new_scan.nodes[i].path)==old_key)).map(|t|(old.viewport,tile_body(t.rect,t.header)))}
        else if !changed{Some((by_path.get(&old_key).map(|(r,_,_)|*r).unwrap_or(old.viewport),self.workspace.usage_camera.rect(new.viewport)))}else{None};
        let Some((src,dst))=region else{self.workspace.usage_transition=None;self.workspace.usage_camera.reset();return};
        if self.session.read(cx).prefs.usage_reduce_motion{self.workspace.usage_transition=None;if changed{self.workspace.usage_camera.reset()}return}
        // Scene swaps use ONE DiskTree region map, not per-tile old/new
        // layout interpolation which can make siblings cross mid-transition.
        let from=Vec::<crate::usage_motion::Frame>::new();
        let mut backdrop=self.workspace.usage_transition.as_ref().map(|t|t.old_frame(now)).unwrap_or_default();
        let old_tiles:std::collections::HashMap<_,_>=old.tiles.iter().map(|t|(t.key,t)).collect();
        if changed{let tokens=Self::tokens(cx);for (k,r,h) in frame{let TileKey::Node(i)=k else{continue};let Some(n)=scan.nodes.get(i)else{continue};let Some(tile)=old_tiles.get(&k)else{continue};backdrop.push(PaintTile{key:TileKey::Other(usize::MAX),interactive:false,rect:r,header:h,depth:tile.depth,color:tokens.usage_fill(tile.class,tile.depth),opacity:1.,name:n.path.file_name().map(|s|s.to_string_lossy().into_owned()).unwrap_or_else(||display_path(&n.path)),size:disk_usage::format_bytes(scan.weight(n)),selected:false,hovered:false,unknown:n.uncertain,class:tile.class});}}
        // The coverage list is bounded by one old frame. Never accumulate a chain
        // of obsolete roots under rapid navigation.
        if backdrop.len()>8192{backdrop.truncate(8192);}
        if crate::isolation::development_var_os("FINDX_NATIVE_USAGE_TRACE").is_some(){eprintln!("usage_motion begin old={:?} new={:?} matched={} backdrop={} src={:?} dst={:?}",old_path,new_path,from.len(),backdrop.len(),src,dst);}
        self.workspace.usage_transition=Some(Transition{motion:crate::usage_motion::Motion::new(from,src,dst,now),backdrop});
        if changed{self.workspace.usage_camera.reset();self.workspace.usage_hover=None;}
        if crate::isolation::development_var_os("FINDX_NATIVE_USAGE_TRACE").is_some(){eprintln!("usage_timing handoff prep_us={} tiles={} duration_ms={}",handoff_started.elapsed().as_micros(),old.tiles.len(),self.workspace.usage_transition.as_ref().map(|t|t.motion.duration_ms()).unwrap_or(0));}
    }

    pub(in super::super) fn ensure_usage_scene(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.mode != ViewMode::Usage {
            return;
        }
        if let Some(t) = &self.workspace.usage_transition {
            if t.running() {
                window.request_animation_frame();
            } else {
                if crate::isolation::development_var_os("FINDX_NATIVE_USAGE_TRACE").is_some(){crate::usage_motion::trace_settled();}
                self.workspace.usage_transition = None;
            }
        }
        if self.workspace.scanning {
            let b = self.workspace.usage_bounds.get();
            let size = (
                f32::from(b.size.width).max(0.) as u32,
                f32::from(b.size.height).max(0.) as u32,
                self.workspace.usage_depth,
            );
            let now = Instant::now();
            let source = Arc::as_ptr(&self.workspace.progress) as usize;
            let mut motion = self.workspace.usage_preview.borrow_mut();
            let snapshot = {
                let builder = crate::disktree::lock(&self.workspace.progress.preview);
                (size.0 > 1 && size.1 > 1 && motion.due(builder.revision, size, source, now))
                    .then(|| builder.snapshot())
            };
            if let Some(snapshot) = snapshot {
                motion.update(
                    snapshot,
                    size,
                    source,
                    now,
                    self.session.read(cx).prefs.usage_reduce_motion,
                );
                if crate::isolation::development_var_os("FINDX_NATIVE_USAGE_TRACE").is_some() {
                    let (paints, ns, max) = usage_view::paint_metrics();
                    eprintln!(
                        "usage_preview revision={} builds={} layout_us={} paint_count={} paint_mean_us={} paint_max_us={}",
                        motion.revision,
                        motion.builds,
                        motion.layout_us,
                        paints,
                        ns / paints.max(1) / 1000,
                        max / 1000
                    );
                }
            }
            if motion.running(now) {
                window.request_animation_frame();
            }
            return;
        }
        if self
            .workspace
            .usage_preview
            .borrow()
            .running(Instant::now())
        {
            window.request_animation_frame();
        }
        let Some(scan) = self.workspace.scan.clone() else {
            return;
        };
        let Some(root) = self.scan_node() else {
            if self.workspace.usage_pending_from.is_some() {
                return;
            }
            self.workspace.usage_scene_generation =
                self.workspace.usage_scene_generation.wrapping_add(1);
            self.workspace.usage_scene_key = None;
            self.workspace.usage_scene = None;
            self.workspace.usage_frame.borrow_mut().clear();
            self.workspace.usage_hover = None;
            return;
        };
        let b = self.workspace.usage_bounds.get();
        let width = f32::from(b.size.width).round() as u32;
        let height = f32::from(b.size.height).round() as u32;
        if width < 2 || height < 2 {
            return;
        }
        let root_change = self.workspace.usage_scene.as_ref().is_some_and(|s| {
            s.key.root != root
                || self
                    .workspace
                    .usage_scene_scan
                    .as_ref()
                    .and_then(|old| old.nodes.get(s.key.root))
                    .map(|n| &n.path)
                    != scan.nodes.get(root).map(|n| &n.path)
                || (!self.workspace.engine_usage && s.key.scan != self.usage_scan_identity(&scan))
        });
        let detail = if root_change {
            1
        } else {
            self.workspace.usage_camera.scale.floor().clamp(1., 5.) as u32
        };
        let key = SceneKey {
            scan: self.usage_scan_identity(&scan),
            root,
            depth: usage_view::PRESENTATION_DEPTH,
            width,
            height,
            detail,
        };
        if self.workspace.usage_scene_key.as_ref() == Some(&key) {
            return;
        }
        if !usage_view::claim_layout(&mut self.workspace.usage_layout_inflight) {
            return;
        }
        self.workspace.usage_scene_key = Some(key.clone());
        self.workspace.usage_scene_generation =
            self.workspace.usage_scene_generation.wrapping_add(1);
        let generation = self.workspace.usage_scene_generation;
        let layout_started=Instant::now();
        let handoff_scan = scan.clone();
        let reusable_scene = self.workspace.usage_scene.clone();
        cx.spawn(async move |this, cx| {
            let (scene,worker_wait_us,worker_cpu_us) = cx
                .background_spawn(async move {let wait=layout_started.elapsed().as_micros();let cpu=Instant::now();let s=reusable_scene.as_ref().and_then(|s|s.relayout(key.clone())).unwrap_or_else(||usage_view::build_scene(&scan,key));(s,wait,cpu.elapsed().as_micros())})
                .await;
            if crate::isolation::development_var_os("FINDX_NATIVE_USAGE_TRACE").is_some(){eprintln!("usage_timing layout elapsed_us={} tiles={} worker_wait_us={} worker_cpu_us={} treemap_us={}",layout_started.elapsed().as_micros(),scene.tiles.len(),worker_wait_us,worker_cpu_us,scene.layout_elapsed.as_micros());}
            this.update(cx, |this, cx| {
                if !usage_view::finish_layout(
                    &mut this.workspace.usage_layout_inflight,
                    generation,
                    this.workspace.usage_scene_generation,
                ) {
                    cx.notify();
                    return;
                }
                let source = Arc::as_ptr(&this.workspace.progress) as usize;
                let handing = !this.workspace.engine_usage
                    && handoff_scan.logical_generation.is_none()
                    && this.workspace.usage_preview.borrow().can_handoff(source);
                if !handing {this.begin_usage_scene_motion(&scene,&handoff_scan,cx);}
                eprintln!(
                    "usage_layout tiles={} elapsed_us={} detail={}",
                    scene.tiles.len(),
                    scene.layout_elapsed.as_micros(),
                    scene.key.detail
                );
                if handing {
                    let target = scene
                        .tiles
                        .iter()
                        .filter(|t| t.depth > 0)
                        .map(|t| {
                            let (id, parent, name) = match t.key {
                                TileKey::Node(i) => (
                                    i,
                                    handoff_scan.nodes[i].parent.unwrap_or(0),
                                    handoff_scan.nodes[i]
                                        .path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned(),
                                ),
                                TileKey::Other(i) => (
                                    (1usize << 63) | (1usize << 62) | i,
                                    scene.key.root,
                                    "其他小项".into(),
                                ),
                            };
                            crate::disktree::scan_preview::Tile {
                                id,
                                parent,
                                rect: t.rect,
                                depth: t.depth,
                                header: t.header.is_some(),
                                header_h: t.header.map_or(0., |h| h.h),
                                class: t.class,
                                name,
                                bytes: t.bytes,
                                opacity: 1.,
                            }
                        })
                        .collect();
                    this.workspace.usage_preview.borrow_mut().handoff(
                        target,
                        Instant::now(),
                        this.session.read(cx).prefs.usage_reduce_motion,
                    );
                }
                // Preserve zoom but clamp its origin after resize/detail relayout.
                this.workspace.usage_camera.pan(0., 0., scene.viewport);
                // Publish immutable geometry and its matching node snapshot together.
                // Until the new scene has actually painted, old sampled hitboxes
                // must not be interpreted against newly published node indices.
                this.workspace.usage_frame.borrow_mut().clear();
                this.workspace.usage_hover = None; // old node indices/tooltip must not cross a same-root scene handoff
                this.workspace.usage_scene_selected = this
                    .workspace
                    .usage_focus
                    .as_ref()
                    .and_then(|p| this.workspace.scan_lookup.get(&super::key(Path::new(p))))
                    .copied();
                this.workspace.usage_scene_next = this.workspace.engine_next;
                this.workspace.usage_scene_loaded_depth = this.workspace.usage_loaded_depth;
                this.workspace.usage_scene_more_depth = this.workspace.usage_more_depth;
                this.workspace.usage_scene_blocked_paths =
                    this.workspace.usage_blocked_paths.clone();
                this.workspace.usage_scene_scan = Some(handoff_scan);
                this.workspace.usage_scene = Some(Arc::new(scene));
                this.inspector_default(cx);
                this.workspace.usage_pending_from = None;
                this.workspace.usage_pending_direction=0;
                this.workspace.usage_pending_history=None;
                if this.workspace.engine_usage {
                    this.workspace.usage_depth_pending = false;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
    pub(super) fn zoom_usage_center(&mut self, factor: f32, cx: &mut Context<Self>) {
        if !self.workspace.scanning && !self.usage_scene_interactive() {
            return;
        }
        if self.workspace.usage_pending_from.is_some() {
            return;
        }
        if self.workspace.scanning {
            let b = self.workspace.usage_bounds.get();
            let v = Rect {
                x: 0.,
                y: 0.,
                w: f32::from(b.size.width),
                h: f32::from(b.size.height),
            };
            self.workspace
                .usage_camera
                .zoom(factor, (v.w / 2., v.h / 2.), v);
            cx.notify();
            return;
        }
        if let Some(scene) = &self.workspace.usage_scene {
            let mut target=self.usage_presented_camera(scene);
            target.zoom(factor,(scene.viewport.w/2.,scene.viewport.h/2.),scene.viewport);
            self.workspace.usage_wheel_gate.reset();self.retarget_usage_camera(target,cx);
            cx.notify();
        }
    }
    fn usage_point(&self, p: Point<Pixels>) -> (f32, f32) {
        let b = self.workspace.usage_bounds.get();
        (f32::from(p.x - b.origin.x), f32::from(p.y - b.origin.y))
    }
    fn usage_hit(&self, p: (f32, f32)) -> Option<TileKey> {
        if self.workspace.usage_pending_from.is_some() {
            return None;
        }
        if self.workspace.scanning {
            return usage_view::hit_frame(&self.workspace.usage_frame.borrow(), p);
        }
        if self.workspace.usage_scene.as_ref().is_none_or(|s| {
            Some(s.key.root) != self.scan_node()
                || self
                    .workspace
                    .scan
                    .as_ref()
                    .is_none_or(|scan| s.key.scan != self.usage_scan_identity(scan))
        }) {
            return None;
        }
        usage_view::hit_frame(&self.workspace.usage_frame.borrow(), p)
    }
    fn usage_scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        if self.workspace.usage_pending_from.is_some(){
            let (_,dy)=match event.delta{ScrollDelta::Lines(p)=>usage_view::normalized_wheel(p.x,p.y,true),ScrollDelta::Pixels(p)=>usage_view::normalized_wheel(f32::from(p.x),f32::from(p.y),false)};
            if let usage_view::WheelIntent::Zoom{steps}=usage_view::wheel_intent(0.,dy,event.modifiers.shift,event.modifiers.control,event.modifiers.alt,event.modifiers.platform){
                if steps.signum() as i8 == -self.workspace.usage_pending_direction && self.workspace.usage_pending_direction!=0{
                    self.cancel_engine_usage(cx);self.workspace.usage_pending_direction=0;
                    if let Some(scene)=&self.workspace.usage_scene{let mut target=self.workspace.usage_camera;target.zoom(1.30f32.powf(steps),self.usage_point(event.position),scene.viewport);self.retarget_usage_camera(target,cx);}
                }
            }
            cx.stop_propagation();return;
        }
        if !self.workspace.scanning && !self.usage_scene_interactive() {
            cx.stop_propagation();
            return;
        }
        if self.workspace.scanning {
            let b = self.workspace.usage_bounds.get();
            let viewport = Rect {
                x: 0.,
                y: 0.,
                w: f32::from(b.size.width),
                h: f32::from(b.size.height),
            };
            let (dx, dy) = match event.delta {
                ScrollDelta::Lines(p) => usage_view::normalized_wheel(p.x, p.y, true),
                ScrollDelta::Pixels(p) => {
                    usage_view::normalized_wheel(f32::from(p.x), f32::from(p.y), false)
                }
            };
            match usage_view::wheel_intent(
                dx,
                dy,
                event.modifiers.shift,
                event.modifiers.control,
                event.modifiers.alt,
                event.modifiers.platform,
            ) {
                usage_view::WheelIntent::Zoom { steps } => {
                    let p = self.usage_point(event.position);
                    self.workspace
                        .usage_camera
                        .zoom(1.30f32.powf(steps), p, viewport);
                }
                usage_view::WheelIntent::Pan { dx, dy } => {
                    self.workspace.usage_camera.pan(dx, dy, viewport)
                }
                _ => {}
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }

        let Some(scene) = self.workspace.usage_scene.clone().filter(|s| {
            Some(s.key.root) == self.scan_node()
                && self
                    .workspace
                    .scan
                    .as_ref()
                    .is_some_and(|scan| s.key.scan == self.usage_scan_identity(scan))
        }) else {
            return;
        };
        let (dx, dy) = match event.delta {
            ScrollDelta::Lines(p) => usage_view::normalized_wheel(p.x, p.y, true),
            ScrollDelta::Pixels(p) => {
                usage_view::normalized_wheel(f32::from(p.x), f32::from(p.y), false)
            }
        };
        if self.file_actions.native_menu.is_open() {
            cx.stop_propagation();
            return;
        }
        let intent = usage_view::wheel_intent(
            dx,
            dy,
            event.modifiers.shift,
            event.modifiers.control,
            event.modifiers.alt,
            event.modifiers.platform,
        );
        match intent {
            usage_view::WheelIntent::Ignore => {
                self.workspace.usage_wheel_gate.reset();
                return;
            }
            usage_view::WheelIntent::Pan { dx, dy } => {
                self.workspace.usage_wheel_gate.reset();
                let mut target=self.usage_presented_camera(&scene);target.pan(dx,dy,scene.viewport);self.retarget_usage_camera(target,cx);
            }
            usage_view::WheelIntent::Zoom { steps } => {
                let pointer = self.usage_point(event.position);
                let camera = self.usage_presented_camera(&scene);
                let candidate = {
                    let frames = self.workspace.usage_frame.borrow();
                    frames
                        .iter()
                        .rev()
                        .filter_map(|(k, r, h)| {
                            let TileKey::Node(i) = *k else {
                                return None;
                            };
                            let scan = self.workspace.scan.as_ref()?;
                            if i != scene.key.root
                                && scan.nodes.get(i).is_some_and(|n| n.directory)
                                && tile_body(*r, *h).contains(pointer)
                            {
                                Some(i)
                            } else {
                                None
                            }
                        })
                        .next()
                };
                let ceiling = candidate
                    .and_then(|i| scene.tiles.iter().find(|t| t.key == TileKey::Node(i)))
                    .map(|t| {
                        View::fit_scale(tile_body(t.rect, t.header).into(), scene.viewport.into())
                            .clamp(1., View::MAX_SCALE)
                    })
                    .unwrap_or(View::MAX_SCALE);
                // Indexed usage pages root a fresh subtree at each requested path.
                // Its root.parent is None even when the LOCATION has an ancestor.
                let boundary = usage_view::wheel_boundary(
                    steps,
                    camera.scale,
                    ceiling,
                    candidate,
                    self.workspace.location.parent().is_some(),
                );
                if let Some(action) =
                    self.workspace
                        .usage_wheel_gate
                        .evaluate_input(Instant::now(), steps, boundary, matches!(event.delta,ScrollDelta::Lines(_)))
                {
                    let history=self.workspace.history.clone();
                    let direction=match action {
                        SemanticAction::Enter(i) => {self.select_usage_node(i,true,cx);1},
                        SemanticAction::Up => {self.workspace_up(cx);-1},
                    };
                    if self.workspace.usage_pending_from.is_some(){self.workspace.usage_pending_direction=direction;self.workspace.usage_pending_history=Some(history);}
                } else {
                    let base=crate::usage_motion::wheel_base(camera,self.workspace.usage_camera,steps);
                    let v = base
                        .as_view()
                        .zoomed_at(
                            pointer.0,
                            pointer.1,
                            1.30f32.powf(steps),
                            ceiling.max(base.scale),
                        )
                        .clamped(scene.viewport.into());
                    self.retarget_usage_camera(usage_view::Camera{scale:v.scale,x:v.origin_x,y:v.origin_y},cx);
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
    pub(super) fn render_cached_usage_map(&self, cx: &mut Context<Self>) -> AnyElement {
        let render_started=Instant::now();
        let tokens = Self::tokens(cx);
        let bounds = self.workspace.usage_bounds.clone();
        let frame = self.workspace.usage_frame.clone();
        let owner = cx.weak_entity();
        let selected = self.workspace.usage_scene_selected;
        let now=Instant::now();
        let trace_moving=crate::isolation::development_var_os("FINDX_NATIVE_USAGE_TRACE").is_some()&&self.workspace.usage_transition.is_some();
        let mut draws = self.workspace.usage_transition.as_ref().map(|t|t.old_frame(now)).unwrap_or_default();
        let mut empty = None;
        if self.workspace.scanning {
            let now = Instant::now();
            let motion = self.workspace.usage_preview.borrow();
            for t in motion.sample(now) {
                let rect = self.workspace.usage_camera.rect(t.rect);
                let mut color = tokens.usage_fill(t.class, t.depth);
                color.a *= t.opacity;
                draws.push(PaintTile {
                    key: TileKey::Other(t.id),
                    interactive: true,
                    rect,
                    header: (t.header_h > 0.5).then_some(Rect {
                        h: t.header_h,
                        ..rect
                    }),
                    depth: t.depth,
                    color,
                    opacity: t.opacity,
                    name: t.name,
                    size: disk_usage::format_bytes(t.bytes),
                    selected: motion.selected == Some(t.id),
                    hovered: self.workspace.usage_hover == Some(TileKey::Other(t.id)),
                    unknown: false,
                    class: t.class,
                });
            }
            if draws.is_empty() {
                empty = Some("本地实测中，等待首批数据（总量未知）");
            }
        } else if self.workspace.engine_usage
            && self.scan_node().is_none()
            && self.workspace.usage_pending_from.is_none()
        {
            empty = Some(if self.workspace.engine_busy {
                "正在读取索引目录…（最长 45 秒，可取消）"
            } else {
                self.workspace.scan_status.as_str()
            });
        } else if let (Some(scan), Some(scene)) =
            (self.usage_render_scan(), &self.workspace.usage_scene)
        {
            if self.workspace.usage_pending_from.is_some()
                || ((if self.workspace.engine_usage {
                    crate::indexed_usage::scene_can_paint(
                        scan,
                        scene.key.root,
                        &self.workspace.location,
                    )
                } else {
                    scene.key.scan == self.usage_scan_identity(scan)
                }) && Some(scene.key.root) == self.scan_node())
            {
                let preview = self.workspace.usage_preview.borrow();
                let blended = if !self.workspace.engine_usage
                    && preview.scope == scene.key.root
                    && preview.finalized
                    && preview.running(Instant::now())
                {
                    preview
                        .sample(Instant::now())
                        .into_iter()
                        .map(|t| (t.id, t))
                        .collect::<std::collections::HashMap<_, _>>()
                } else {
                    Default::default()
                };
                let mut remaining = blended.clone();
                for tile in scene
                    .tiles
                    .iter()
                    .filter(|t| usage_view::visible_at_depth(t.depth, self.workspace.usage_depth))
                {
                    let sample=|r,header|{let target=self.workspace.usage_camera.rect(r);self.workspace.usage_transition.as_ref().map(|t|t.sample(tile.key,target,header,now)).unwrap_or(target)};
                    let id = match tile.key {
                        TileKey::Node(i) => i,
                        TileKey::Other(i) => (1usize << 63) | (1usize << 62) | i,
                    };
                    let blend = blended.get(&id);
                    remaining.remove(&id);
                    let r = blend
                        .map(|t| self.workspace.usage_camera.rect(t.rect))
                        .unwrap_or_else(|| sample(tile.rect,false));
                    let opacity = blend.map_or(1., |t| t.opacity);
                    if r.w < 1.
                        || r.h < 1.
                        || r.x >= scene.viewport.w
                        || r.y >= scene.viewport.h
                        || r.x + r.w <= 0.
                        || r.y + r.h <= 0.
                    {
                        continue;
                    }
                    let (name, size, unknown) = match tile.key {
                        TileKey::Node(i) => {
                            let n = &scan.nodes[i];
                            (
                                n.path
                                    .file_name()
                                    .map(|x| x.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| display_path(&n.path)),
                                disk_usage::format_bytes(scan.weight(n)),
                                n.uncertain,
                            )
                        }
                        TileKey::Other(_) => (
                            "其他小项".into(),
                            disk_usage::format_bytes(tile.bytes),
                            false,
                        ),
                    };
                    let mut color = tokens.usage_fill(tile.class, tile.depth);
                    color.a *= opacity;
                    draws.push(PaintTile {
                        key: tile.key,
                        interactive: true,
                        rect: r,
                        header: if tile.depth >= self.workspace.usage_depth {
                            None
                        } else if let Some(t) = blend {
                            (t.header_h > 0.5).then_some(Rect { h: t.header_h, ..r })
                        } else {
                            tile.header.map(|h|sample(h,true))
                        },
                        depth: tile.depth,
                        color,
                        opacity,
                        name,
                        size,
                        unknown,
                        class: tile.class,
                        selected: matches!(tile.key,TileKey::Node(i) if Some(i)==selected),
                        hovered: self.workspace.usage_hover == Some(tile.key),
                    });
                }
                for (_, t) in remaining {
                    let mut color = tokens.usage_fill(t.class, t.depth);
                    color.a *= t.opacity;
                    let rect = self.workspace.usage_camera.rect(t.rect);
                    draws.push(PaintTile {
                        key: TileKey::Other(t.id),
                        interactive: true,
                        rect,
                        header: (t.header_h > 0.5).then_some(Rect {
                            h: t.header_h,
                            ..rect
                        }),
                        depth: t.depth,
                        color,
                        opacity: t.opacity,
                        name: t.name,
                        size: disk_usage::format_bytes(t.bytes),
                        selected: false,
                        hovered: false,
                        unknown: false,
                        class: t.class,
                    });
                }
                if scan.weight(&scan.nodes[scene.key.root]) == 0 {
                    empty = Some(if scan.logical_generation.is_some() {
                        "索引逻辑大小为 0；未验证元数据不是已确认空文件"
                    } else {
                        "已测占用为 0；未知项、空文件和硬链接别名仍可在右侧查看"
                    });
                }
            } else {
                empty = Some("正在排布已测结果…");
            }
        } else {
            empty = Some(if self.workspace.engine_usage {
                if self.workspace.engine_busy {
                    "正在读取引擎索引逻辑目录树…"
                } else {
                    if self.workspace.scan_status.contains("不支持") {
                        self.workspace.scan_status.as_str()
                    } else {
                        "当前服务尚未提供索引逻辑目录树；状态见下方，可明确选择本地实测"
                    }
                }
            } else {
                "选择目录开始测量；已有快照会直接读取，不会自动全盘扫描"
            });
        }
        if trace_moving{crate::usage_motion::trace_render(render_started.elapsed());}
        let resize_source = if !self.workspace.scanning && empty.is_none() {
            self.usage_render_scan()
                .cloned()
                .zip(self.workspace.usage_scene.clone())
        } else {
            None
        };
        let resize_camera = self.workspace.usage_camera;
        let resize_depth = self.workspace.usage_depth;
        let resize_hover = self.workspace.usage_hover;
        let menu_owner = cx.weak_entity();
        let hover_detail = self.workspace.usage_hover.and_then(|hit| match hit {
            TileKey::Node(i) => self.usage_render_scan().and_then(|scan| {
                scan.nodes.get(i).map(|n| {
                    format!(
                        "{}\n{}  {}{}",
                        display_path(&n.path),
                        disk_usage::format_bytes(scan.weight(n)),
                        if n.directory { "目录" } else { "文件" },
                        if n.uncertain {
                            "；元数据待验证"
                        } else {
                            ""
                        }
                    )
                })
            }),
            _ => None,
        });
        let mut view = div()
            .id("usage-camera")
            .flex()
            .flex_col()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .m_2()
            .overflow_hidden()
            .rounded_sm()
            .bg(tokens.surface)
            .when(
                matches!(self.workspace.usage_hover, Some(TileKey::Node(_))),
                |view| view.cursor_pointer(),
            )
            .track_focus(&self.workspace.usage_map_focus)
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| this.usage_scroll(event, cx)))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let hit = this.usage_hit(this.usage_point(event.position));
                if hit != this.workspace.usage_hover {
                    this.workspace.usage_hover = hit;
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.workspace.usage_hover = None;
                    this.workspace.usage_wheel_gate.reset();
                    cx.notify();
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                let p = this.usage_point(window.mouse_position());
                this.workspace.usage_map_focus.focus(window, cx);
                if this.workspace.scanning {
                    if let Some(TileKey::Other(id)) = this.usage_hit(p) {
                        this.workspace.usage_preview.borrow_mut().selected = Some(id);
                    }
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                match this.usage_hit(p) {
                    Some(TileKey::Node(i)) => this.select_usage_node(i, false, cx),
                    _ => {
                        if let Some(root) = this.scan_node() {
                            this.select_usage_node(root, false, cx);
                        }
                    }
                }
                cx.stop_propagation();
            }))
            .on_double_click(cx.listener(|this, _, window, cx| {
                if this.workspace.scanning {
                    if let Some(TileKey::Other(id)) =
                        this.usage_hit(this.usage_point(window.mouse_position()))
                    {
                        let entered = this.workspace.usage_preview.borrow_mut().enter(id);
                        if entered {
                            this.workspace.usage_camera.reset();
                            cx.notify();
                        }
                    }
                    cx.stop_propagation();
                    return;
                }
                if let Some(TileKey::Node(i)) =
                    this.usage_hit(this.usage_point(window.mouse_position()))
                {
                    this.select_usage_node(i, true, cx);
                }
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let m = event.keystroke.modifiers;
                if this.text_edit_active(window, cx)
                    || m.control
                    || m.alt
                    || m.platform
                    || this.file_menu_is_open()
                {
                    return;
                }
                match event.keystroke.key.as_str() {
                    "+" | "=" => this.zoom_usage_center(1.3, cx),
                    "-" => this.zoom_usage_center(1. / 1.3, cx),
                    "0" => {
                        this.workspace.usage_camera.reset();
                        this.workspace.usage_transition = None;
                        cx.notify();
                    }
                    "enter" => this.workspace_enter(cx),
                    "backspace" => this.workspace_up(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .when_some(hover_detail, |view, detail| {
                view.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(detail.clone()).build(window, cx)
                })
            })
            .context_menu(move |menu, window, cx| {
                menu_owner
                    .update(cx, |this, cx| {
                        this.workspace.usage_hover = None;
                        this.workspace.usage_wheel_gate.reset();
                        if this.workspace.scanning {
                            return menu.item(gpui_kit::component::menu::PopupMenuItem::label(
                                "扫描正在更新；完成后再打开条目操作",
                            ));
                        }
                        if let Some(TileKey::Node(i)) =
                            this.usage_hit(this.usage_point(window.mouse_position()))
                        {
                            this.select_usage_node(i, false, cx);
                            this.file_context_menu(menu, false, window, cx)
                        } else {
                            menu.item(gpui_kit::component::menu::PopupMenuItem::label(
                                "空隙 / 合并项无文件操作；右键实际条目",
                            ))
                        }
                    })
                    .unwrap()
            })
            .child(
                canvas(
                    move |b, _, cx| {
                        #[cfg(debug_assertions)]
                        fixture_geometry("layout", b, draws.len());
                        let old = bounds.replace(b);
                        let mut draws = draws;
                        if let Some((scan, source)) = &resize_source {
                            let width = b.size.width.as_f32().round().max(0.) as u32;
                            let height = b.size.height.as_f32().round().max(0.) as u32;
                            if source.key.width != width || source.key.height != height {
                                let scene = Arc::new(source.reflow(width, height));
                                let (reflowed, camera) = resized_scene_draws(
                                    scan,
                                    &scene,
                                    resize_camera,
                                    resize_depth,
                                    selected,
                                    resize_hover,
                                    tokens,
                                );
                                draws = reflowed;
                                let _ = owner.update(cx, |this, cx| {
                                    if this
                                        .workspace
                                        .usage_scene
                                        .as_ref()
                                        .is_some_and(|current| Arc::ptr_eq(current, source))
                                    {
                                        // Publish only the viewport of this SAME immutable
                                        // scene. Never promote pending data/identities here.
                                        if let Some(key) = &mut this.workspace.usage_scene_key {
                                            if key.scan == source.key.scan
                                                && key.root == source.key.root
                                                && key.detail == source.key.detail
                                                && key.depth == source.key.depth
                                            {
                                                key.width = width;
                                                key.height = height;
                                            }
                                        }
                                        this.workspace.usage_scene = Some(scene);
                                        this.workspace.usage_camera = camera;
                                        this.workspace.usage_transition = None;
                                        this.workspace.usage_hover = None;
                                        cx.notify();
                                    }
                                });
                            }
                        }
                        *frame.borrow_mut() = draws
                            .iter()
                            .filter(|d| d.interactive && d.key != TileKey::Other(usize::MAX))
                            .map(|d| (d.key, d.rect, d.header))
                            .collect();
                        if old.size != b.size {
                            let _ = owner.update(cx, |_, cx| cx.notify());
                        }
                        draws // EXACT prepaint geometry supplies both hitboxes and paint.
                    },
                    move |b, paint: Vec<PaintTile>, window, cx| {
                        #[cfg(debug_assertions)]
                        fixture_geometry("paint", b, paint.len());
                        let started = Instant::now();
                        let mut quads_time=std::time::Duration::ZERO;let mut labels_time=std::time::Duration::ZERO;
                        let screen = |r: Rect| Bounds {
                            origin: point(
                                b.origin.x + px(r.x.round()),
                                b.origin.y + px(r.y.round()),
                            ),
                            size: size(px(r.w.round().max(0.)), px(r.h.round().max(0.))),
                        };
                        window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
                            // Fill first, then labels, finally outlines (selected stays visible).
                            for d in &paint {
                                // An opaque root/backdrop keeps every sampled region covered.
                                window.paint_quad(fill(screen(d.rect), d.color));
                                if d.depth == 0 {continue;}
                                if let Some(header) = d.header {
                                    let mut shade = d.color;
                                    shade.l = (shade.l - 0.035).clamp(0., 1.);
                                    window.paint_quad(fill(screen(header), shade));
                                }
                                if d.rect.w >= 8. && d.rect.h >= 8. {
                                    let mut edge = tokens.border;
                                    edge.a = if d.depth == 1 { 0.6 } else { 0.35 };
                                    let mut clear = tokens.surface;
                                    clear.a = 0.;
                                    window.paint_quad(quad(
                                        screen(d.rect),
                                        0.,
                                        clear,
                                        0.75,
                                        edge,
                                        BorderStyle::Solid,
                                    ));
                                }
                            }
                            if trace_moving{quads_time=started.elapsed();}
                            let labels_started=trace_moving.then(Instant::now);
                            // No repeated uncertainty dots. Scope quality remains in the
                            // inspector/status and the exact hovered item tooltip.
                            for d in &paint {
                                let label = d.header.unwrap_or(d.rect);
                                // Coverage belongs to the old node namespace: never
                                // paint its labels over newly published directory text.
                                if d.depth==0 || !d.interactive || d.opacity < 0.65 {
                                    continue;
                                }
                                // The existing label policy rejects these rectangles
                                // regardless of size-text width. Do not shape invisible
                                // size strings for hundreds of tiny tiles every frame.
                                if crate::usage_presentation::label_plan(label.w,label.h,d.header.is_some(),0.).is_none(){continue;}
                                let make_run = |text: &str, size_color: Hsla| TextRun {
                                    len: text.len(),
                                    font: Font {
                                        weight: if d.header.is_some() {
                                            FontWeight::SEMIBOLD
                                        } else {
                                            FontWeight::NORMAL
                                        },
                                        ..font("Segoe UI")
                                    },
                                    color: size_color,
                                    ..Default::default()
                                };
                                let mut primary = tokens.text_primary;
                                primary.a *= d.opacity;
                                let mut secondary = tokens.text_secondary;
                                secondary.a *= d.opacity;
                                let size_line = window.text_system().shape_line(
                                    SharedString::from(d.size.clone()),
                                    px(10.),
                                    &[TextRun {
                                        len: d.size.len(),
                                        font: font("Segoe UI"),
                                        color: secondary,
                                        ..Default::default()
                                    }],
                                    None,
                                );
                                let size_width = f32::from(size_line.width());
                                let Some(plan) = crate::usage_presentation::label_plan(
                                    label.w,
                                    label.h,
                                    d.header.is_some(),
                                    size_width,
                                ) else {
                                    continue;
                                };
                                let name = crate::usage_presentation::fit_label(
                                    &d.name,
                                    plan.name_width,
                                    |text| {
                                        let line = window.text_system().shape_line(
                                            SharedString::from(text.to_string()),
                                            px(12.),
                                            &[make_run(text, primary)],
                                            None,
                                        );
                                        f32::from(line.width())
                                    },
                                );
                                let Some(name) = name else { continue };
                                let line = window.text_system().shape_line(
                                    SharedString::from(name.clone()),
                                    px(12.),
                                    &[make_run(&name, primary)],
                                    None,
                                );
                                let clip = screen(label);
                                window.with_content_mask(
                                    Some(ContentMask { bounds: clip }),
                                    |window| {
                                        let _ = line.paint(
                                            point(clip.origin.x + px(5.), clip.origin.y + px(if d.header.is_some(){(label.h-label.h.min(17.))*0.5}else{2.})),
                                            px(if d.header.is_some(){label.h.min(17.)}else{17.}),
                                            TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                        if plan.inline_size || plan.second_line {
                                            let x = if plan.inline_size {
                                                label.w - size_width - 6.
                                            } else {
                                                5.
                                            };
                                            let y = if plan.inline_size { (label.h-label.h.min(15.))*0.5 } else { 21. };
                                            let _ = size_line.paint(
                                                point(clip.origin.x + px(x), clip.origin.y + px(y)),
                                                px(if plan.inline_size{label.h.min(15.)}else{15.}),
                                                TextAlign::Left,
                                                None,
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                );
                            }
                            if let Some(t)=labels_started{labels_time=t.elapsed();}
                            for d in paint
                                .iter()
                                .filter(|d| d.depth > 0 && (d.selected || d.hovered))
                            {
                                let mut clear = tokens.surface;
                                clear.a = 0.;
                                window.paint_quad(quad(
                                    screen(d.rect),
                                    0.,
                                    clear,
                                    if d.selected { 2. } else { 1. },
                                    if d.selected {
                                        tokens.accent
                                    } else {
                                        tokens.text_secondary
                                    },
                                    BorderStyle::Solid,
                                ));
                            }
                        });
                        usage_view::record_paint(started.elapsed());
                        if trace_moving{crate::usage_motion::trace_frame(started,started.elapsed(),quads_time,labels_time);}
                    },
                )
                .absolute()
                .size_full(),
            );
        if let Some(message) = empty {
            view = view.child(self.usage_empty(message, tokens, cx));
        }
        view.into_any_element()
    }
}

#[cfg(debug_assertions)]
fn fixture_geometry(event: &str, b: Bounds<Pixels>, tiles: usize) {
    use std::io::Write;
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    if crate::isolation::development_var_os("FINDX_NATIVE_USAGE_OPEN").is_none()
        || std::env::args().any(|a| a == "--live")
        || N.fetch_add(1, Ordering::Relaxed) >= 64
    {
        return;
    }
    let path =
        std::env::temp_dir().join(format!("findx-fixture-geometry-{}.log", std::process::id()));
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            f,
            "{} x={} y={} width={} height={} tiles={}",
            event,
            f32::from(b.origin.x),
            f32::from(b.origin.y),
            f32::from(b.size.width),
            f32::from(b.size.height),
            tiles
        );
    }
}

#[cfg(test)]
mod resize_frame_tests {
    use super::resized_scene_draws;
    use crate::usage_view::{self, SceneKey, TileKey};
    #[test]
    fn resized_paint_and_click_frame_share_identity_labels_and_bounds() {
        let scan = usage_view::stability_fixture();
        let scene = usage_view::build_scene(
            &scan,
            SceneKey {
                scan: 1,
                root: 1,
                depth: 6,
                width: 700,
                height: 500,
                detail: 1,
            },
        );
        let resized = scene.reflow(1200, 800);
        let tokens =
            crate::theme::FindxTokens::for_theme(crate::settings_logic::EffectiveTheme::Light);
        let (draws, _) = resized_scene_draws(
            &scan,
            &resized,
            usage_view::Camera::default(),
            3,
            Some(2),
            None,
            tokens,
        );
        let frame = draws
            .iter()
            .map(|d| (d.key, d.rect, d.header))
            .collect::<Vec<_>>();
        assert_eq!(usage_view::hit_frame(&frame, (1200.1, 400.)), None);
        for d in draws.iter().filter(|d| d.depth == 1) {
            if let Some(h) = d.header {
                assert_eq!(
                    usage_view::hit_frame(&frame, (h.x + h.w / 2., h.y + h.h / 2.)),
                    Some(d.key)
                );
            }
            if d.key == TileKey::Node(2) {
                assert!(d.selected);
                assert_eq!(
                    d.class,
                    usage_view::category_of_path(&scan.nodes[2].path, true)
                );
            }
        }
        let source = include_str!("workspace_usage.rs");
        assert!(source.contains("move |b, paint: Vec<PaintTile>, window, cx|"));
        assert!(
            source.contains("draws // EXACT prepaint geometry supplies both hitboxes and paint.")
        );
    }
}
