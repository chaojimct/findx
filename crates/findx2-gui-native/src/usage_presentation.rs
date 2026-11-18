//! Bounded, geometry-independent Usage presentation policy. No file actions.
/// Fit complete Unicode scalar boundaries using the actual text-system measure.
/// Tiny tiles intentionally have no label, rather than a clipped glyph or dots.
pub fn fit_label(text:&str,width:f32,mut measure:impl FnMut(&str)->f32)->Option<String>{
 if width<20.||text.is_empty(){return None}if measure(text)<=width{return Some(text.into())}
 let offsets=text.char_indices().map(|(i,_)|i).chain(Some(text.len())).collect::<Vec<_>>();
 let mut lo=0;let mut hi=offsets.len()-1;
 while lo<hi{let mid=(lo+hi+1)/2;let s=format!("{}…",&text[..offsets[mid]]);if measure(&s)<=width{lo=mid}else{hi=mid-1}}
 if lo<2{return None}Some(format!("{}…",&text[..offsets[lo]]))
}
#[derive(Debug,Clone,Copy)]pub struct LabelPlan{pub name_width:f32,pub inline_size:bool,pub second_line:bool}
pub fn label_plan(width:f32,height:f32,header:bool,size_width:f32)->Option<LabelPlan>{
 // Nested DiskTree headers are15px, not the20px outer/leaf label box.
 // A valid directory title band must not be rejected by the leaf policy.
 if if header {width<44.||height<14.} else {width<48.||height<17.||width*height<900.} {return None}
 let inline=header&&width>=160.&&width-size_width-22.>=64.;
 Some(LabelPlan{name_width:width-12.-if inline{size_width+12.}else{0.},inline_size:inline,second_line:!header&&height>=43.&&width>=76.})
}
#[cfg(test)]mod tests{
 use super::*;
 #[test]fn names_never_overlap_header_size_or_fit_partial_unicode(){let p=label_plan(180.,20.,true,55.).unwrap();assert_eq!(p.name_width,101.);assert!(p.inline_size);let s=fit_label("中文目录项目",37.,|s|s.chars().map(|c|if c=='…'{7.}else{10.}).sum()).unwrap();assert_eq!(s,"中文目…");assert!(fit_label("中文目录",21.,|s|s.chars().count() as f32*10.).is_none());}
 #[test]fn tiny_and_narrow_tiles_do_not_show_garbage(){assert!(label_plan(20.,100.,false,20.).is_none());assert!(label_plan(90.,8.,true,20.).is_none());assert!(!label_plan(100.,20.,true,40.).unwrap().inline_size);assert!(label_plan(100.,48.,false,40.).unwrap().second_line);}
}

/// Watch diagnostics can describe another volume. Never infer this scope is a
/// snapshot or realtime merely from a global diagnostic or unknown counts.
pub fn source_label(indexed:bool,watch_error:Option<&str>)->&'static str{
 if !indexed{return "实际占用  本地测量"}
 match watch_error{Some(_)=>"逻辑大小  时效受限",None=>"逻辑大小  索引视图"}
}
#[cfg(test)]mod freshness_tests{
 #[test]fn global_watch_diagnostics_never_claim_current_scope_snapshot_or_realtime(){use super::source_label;assert_eq!(source_label(true,Some("F: isolated no-watch mode")),"逻辑大小  时效受限");assert_eq!(source_label(true,Some("cursor retained")),"逻辑大小  时效受限");assert_eq!(source_label(true,None),"逻辑大小  索引视图");assert_eq!(source_label(false,Some("no-watch")),"实际占用  本地测量");}
}

/// Scan quality is aggregated at the scan's data root, not its re-rooted scene.
pub struct InspectorFacts<'a>{pub selected:&'a crate::disk_usage::Node,pub statistics_root:&'a std::path::Path}
pub fn inspector_facts(scan:&crate::disk_usage::Scan,selected:Option<usize>)->Option<InspectorFacts<'_>>{
 Some(InspectorFacts{selected:scan.nodes.get(selected?)?,statistics_root:&scan.nodes.first()?.path})
}
#[cfg(test)]mod inspector_tests{
 #[test]fn selected_counts_and_aggregate_quality_remain_distinct_after_reroot(){
  use findx2_ipc::{UsageItemDto as I,UsagePageDto as P};
  let item=|id,name:&str,files,directories,unknown|I{id,name:name.into(),directory:true,logical:100,files,directories,unknown};
  let p=P{epoch:7,generation:11,root:item(1,"C",10,8,3900935),children:vec![item(2,"Users",3416011,444296,23)],next:None,resident_bytes:0,state:"fixture".into(),watch_error:None,metadata_error:None};
  let scan=crate::indexed_usage::scan_from_page(&p,"C".into());let f=super::inspector_facts(&scan,Some(1)).unwrap();
  assert_eq!(f.selected.files,3416011);assert_eq!(f.selected.directories,444296);assert_eq!(f.selected.path,std::path::Path::new("C").join("Users"));assert_eq!(f.statistics_root,std::path::Path::new("C"));assert_eq!(scan.unknown,3900935);assert_eq!(f.selected.mtime,0);
 }
}

#[cfg(test)] mod nested_header_regression {
 #[test] fn fifteen_pixel_directory_bands_keep_real_names_without_changing_geometry(){
  for width in [44.,60.,200.,600.] {let p=super::label_plan(width,15.,true,55.).expect("real inner15px directory header must carry text");assert!(p.name_width>=20.);assert!(!p.second_line);}
  assert!(super::label_plan(600.,13.,true,0.).is_none());assert!(super::label_plan(600.,15.,false,0.).is_none());
 }
}
