use findx2_core::{IndexBuilder,RawEntry};
fn raw(id:u64,parent:u64,name:String,dir:bool)->RawEntry{RawEntry{file_id:id,file_id_128:None,parent_id:parent,name,size:0,mtime:0,ctime:0,attrs:if dir{16}else{0},is_dir:dir}}
fn main(){
 let dirs=(1..=12).map(|i|raw(i,if i==1{0}else{i-1},format!("public-{i}"),true)).collect();
 let files=(0..50).map(|i|raw(1_000_000+i,12,format!("public-file-{i}.txt"),false)).collect();
 let mut g=IndexBuilder::new(b'C',1,7,0).build_from_raw(files,dirs,true).unwrap();
 let ids:Vec<_>=(0..50).map(|i|g.entry_on_volume(b'C',1_000_000+i).unwrap()).collect();
 for n in 0..50_000u64{g.frn_to_entry.insert_scoped(100+n,0,0);g.dir_index.insert_scoped(100+n,0,0);}
 for round in 0..3{let t=std::time::Instant::now();for &i in &ids{std::hint::black_box(g.entry_display_path(i as usize).unwrap());}println!("public_path_page round={} hits=50 depth=12 overlay=50000 us={}",round,t.elapsed().as_micros());}
}
