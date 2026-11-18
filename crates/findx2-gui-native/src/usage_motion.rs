//! Bounded, interruptible region mapping, following DiskTree's region-to-region
//! cubic ease-out. No IPC and no per-frame layout; paint and hit sample once.
use crate::usage_view::{Rect,TileKey};
use std::{collections::HashMap,time::{Duration,Instant}};
pub type Frame=(TileKey,Rect,Option<Rect>);
#[derive(Clone)]pub struct Motion{pub from:HashMap<TileKey,(Rect,Option<Rect>)>,pub src:Rect,pub dst:Rect,start:Instant,duration:Duration}
pub fn region(rect:Rect,src:Rect,dst:Rect)->Rect{let k=if src.w>0.01{dst.w/src.w}else{1.};Rect{x:(rect.x-src.x)*k+dst.x,y:(rect.y-src.y)*k+dst.y,w:rect.w*k,h:rect.h*k}}
impl Motion{
 pub fn new(frames:impl IntoIterator<Item=Frame>,src:Rect,dst:Rect,now:Instant)->Self{let ratio=(src.w/dst.w.max(1.)).abs().max((dst.w/src.w.max(1.)).abs()).max(1.);Self{from:frames.into_iter().map(|(k,r,h)|(k,(r,h))).collect(),src,dst,start:now,duration:Duration::from_millis(100+(ratio.log2().clamp(0.,4.)*20.)as u64)}}
 pub fn duration_ms(&self)->u64{self.duration.as_millis() as u64}
 pub fn progress(&self,now:Instant)->f32{let t=now.saturating_duration_since(self.start).as_secs_f32()/self.duration.as_secs_f32();let t=t.clamp(0.,1.);1.-(1.-t).powi(3)}
 pub fn running(&self,now:Instant)->bool{now.saturating_duration_since(self.start)<self.duration}
 pub fn sample(&self,key:TileKey,to:Rect,header:bool,now:Instant)->Rect{let from=self.from.get(&key).and_then(|(r,h)|if header{Some(h.unwrap_or(Rect{h:0.,..*r}))}else{Some(*r)}).unwrap_or_else(||region(to,self.dst,self.src));from.lerp(to,self.progress(now))}
 pub fn backdrop(&self,old:Rect,now:Instant)->Rect{old.lerp(region(old,self.src,self.dst),self.progress(now))}
}
#[cfg(test)]mod tests{
 use super::*;fn r(x:f32,w:f32)->Rect{Rect{x,y:0.,w,h:200.}}
 #[test]fn region_matches_upstream_width_mapping_and_children_keep_relative_positions(){let src=r(120.,200.);let dst=r(0.,800.);let child=Rect{x:130.,y:10.,w:40.,h:30.};let a=region(child,src,dst);assert_eq!((a.x,a.y,a.w,a.h),(40.,40.,160.,120.));}
 #[test]fn reverse_retargets_from_exact_current_sample_not_initial_or_previous_destination(){let n=Instant::now();let a=Motion::new([(TileKey::Node(1),r(20.,100.),None)],r(20.,100.),r(0.,800.),n);let now=n+Duration::from_millis(63);let current=a.sample(TileKey::Node(1),r(0.,800.),false,now);let reverse=Motion::new([(TileKey::Node(1),current,None)],current,r(20.,100.),now);assert_eq!(reverse.sample(TileKey::Node(1),r(20.,100.),false,now),current);assert_eq!(reverse.sample(TileKey::Node(1),r(20.,100.),false,now+Duration::from_secs(1)),r(20.,100.));}
 #[test]fn sampled_header_and_body_share_the_frame_and_coverage_is_not_replaced_by_empty_canvas(){let n=Instant::now();let full=Rect{x:0.,y:0.,w:800.,h:600.};let src=Rect{x:100.,y:50.,w:200.,h:150.};let m=Motion::new([],src,full,n);for ms in 0..400{let at=n+Duration::from_millis(ms);let b=m.backdrop(full,at);assert!(b.x<=0.&&b.y<=0.&&b.x+b.w>=800.&&b.y+b.h>=600.);let tile=m.sample(TileKey::Node(3),full,false,at);let header=m.sample(TileKey::Node(3),Rect{h:20.,..full},true,at);assert!((header.x-tile.x).abs()<0.001);assert!((header.y-tile.y).abs()<0.001);assert!((header.w-tile.w).abs()<0.001);}}
}

#[cfg(test)]mod header_tests{use super::*;#[test]fn newly_appearing_header_is_inside_matched_body_not_an_unrelated_fallback_region(){let now=Instant::now();let src=Rect{x:200.,y:100.,w:120.,h:80.};let dst=Rect{x:0.,y:0.,w:800.,h:600.};let m=Motion::new([(TileKey::Node(7),src,None)],src,dst,now);for ms in 0..500{let at=now+Duration::from_millis(ms);let body=m.sample(TileKey::Node(7),dst,false,at);let header=m.sample(TileKey::Node(7),Rect{h:20.,..dst},true,at);assert_eq!(body.x,header.x);assert_eq!(body.y,header.y);assert_eq!(body.w,header.w);assert!(header.h>=0.&&header.h<=body.h);}}}

/// Accumulate discrete deltas without discarding the unfinished part of each
/// preceding notch. Reversal abandons queued work at the presented frame.
pub fn wheel_base(presented:crate::usage_view::Camera,queued:crate::usage_view::Camera,steps:f32)->crate::usage_view::Camera{if (queued.scale-presented.scale)*steps < -0.0001{presented}else{queued}}
#[cfg(test)]mod wheel_retarget_regression{
 use super::*;use crate::usage_view::Camera;
 #[test]fn rapid_same_direction_notches_keep_all_requested_scale_and_reverse_is_immediate(){let p=Camera{scale:1.2,x:0.,y:0.};let q=Camera{scale:2.2,x:10.,y:20.};assert_eq!(wheel_base(p,q,1.).scale,2.2);assert_eq!(wheel_base(p,q,-1.).scale,1.2);let p=Camera::default();let mut q=p;for _ in 0..6{q.scale=wheel_base(p,q,1.).scale*1.3;}assert!((q.scale-1.3f32.powi(6)).abs()<0.0001);}
 #[test]fn motion_has_bounded_settle_but_never_jumps_first_frame(){let n=Instant::now();let src=Rect{x:0.,y:0.,w:800.,h:600.};let dst=Rect{w:1040.,h:780.,..src};let m=Motion::new([],src,dst,n);assert_eq!(m.progress(n),0.);assert!(!m.running(n+Duration::from_millis(180)));}
}

// Opt-in existing candidate trace only: canvas callback cadence is NOT compositor FPS.
#[derive(Default)]struct TraceFrames{last:Option<Instant>,gaps:Vec<u64>,paint_us:Vec<u64>,render_max:u64,quads_max:u64,labels_max:u64}
static TRACE_FRAMES:std::sync::OnceLock<std::sync::Mutex<TraceFrames>>=std::sync::OnceLock::new();
pub fn trace_render(prep:Duration){let mut s=TRACE_FRAMES.get_or_init(Default::default).lock().unwrap();s.render_max=s.render_max.max(prep.as_micros() as u64);}
pub fn trace_frame(at:Instant,paint:Duration,quads:Duration,labels:Duration){let mut s=TRACE_FRAMES.get_or_init(Default::default).lock().unwrap();if let Some(last)=s.last{if s.gaps.len()<512{s.gaps.push(at.saturating_duration_since(last).as_micros() as u64);}}s.last=Some(at);s.quads_max=s.quads_max.max(quads.as_micros()as u64);s.labels_max=s.labels_max.max(labels.as_micros()as u64);if s.paint_us.len()<512{s.paint_us.push(paint.as_micros()as u64);}}
pub fn trace_settled(){let mut s=TRACE_FRAMES.get_or_init(Default::default).lock().unwrap();let mut s=std::mem::take(&mut *s);if s.paint_us.is_empty(){return}s.gaps.sort_unstable();s.paint_us.sort_unstable();let p=|v:&Vec<u64>,n:usize|v.get(v.len().saturating_sub(1)*n/100).copied().unwrap_or(0);eprintln!("usage_timing settled canvas_frames={} gap_p50_us={} gap_p95_us={} gap_max_us={} gaps_over_33ms={} paint_p95_us={} paint_max_us={} render_prep_max_us={} quads_max_us={} labels_max_us={} compositor_fps=not_measured",s.paint_us.len(),p(&s.gaps,50),p(&s.gaps,95),p(&s.gaps,100),s.gaps.iter().filter(|&&v|v>33333).count(),p(&s.paint_us,95),p(&s.paint_us,100),s.render_max,s.quads_max,s.labels_max);}
#[cfg(test)]mod handoff_cost_probe{
 #[test]#[ignore="bounded synthetic CPU probe, run explicitly; not a UI fps benchmark"]fn linear_lookup_vs_previous_quadratic_handoff(){use std::{collections::HashMap,time::Instant,hint::black_box};let tiles=(0usize..6722).map(|i|(i,i%6)).collect::<Vec<_>>();let n=Instant::now();let mut a=0;for &(k,_) in &tiles{a+=tiles.iter().find(|t|t.0==black_box(k)).unwrap().1;}let old=n.elapsed();let n=Instant::now();let map=tiles.iter().map(|t|(t.0,t.1)).collect::<HashMap<_,_>>();let mut b=0;for &(k,_) in &tiles{b+=map[&black_box(k)];}let new=n.elapsed();assert_eq!(a,b);eprintln!("handoff_cpu_probe nodes=6722 previous_us={} indexed_us={} equivalent_checksum={} not_actual_frame_timing=true",old.as_micros(),new.as_micros(),a);}
}

#[cfg(test)]mod invisible_label_work_tests{#[test]fn early_geometry_gate_is_exactly_equivalent_for_every_size_measurement(){for w in [0.,20.,47.,48.,60.,90.,160.,300.]{for h in [0.,8.,16.,17.,20.,43.,100.]{for header in [false,true]{for text_width in [0.,20.,80.,180.]{assert_eq!(crate::usage_presentation::label_plan(w,h,header,0.).is_none(),crate::usage_presentation::label_plan(w,h,header,text_width).is_none());}}}}}}
