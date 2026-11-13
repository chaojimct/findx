use findx2_core::{IndexBuilder,RawEntry};
fn main(){
 let root=RawEntry{file_id:1,file_id_128:None,parent_id:0,name:"public-root".into(),size:0,mtime:0,ctime:0,attrs:16,is_dir:true};
 let mut g=IndexBuilder::new(b'C',1,7,0).build_from_raw(vec![],vec![root],true).unwrap();
 for n in 0..50_000u64 {g.dir_index.insert_scoped(100+n,0,0);g.frn_to_entry.insert_scoped(100+n,0,0);}
 let t=std::time::Instant::now();for n in 0..200u64{std::hint::black_box(g.dir_index.get(&(1_000_000+n)));}let old=t.elapsed();
 let t=std::time::Instant::now();for n in 0..200u64{assert_eq!(g.dir_on_volume(b'C',1_000_000+n),None);assert_eq!(g.entry_on_volume(b'C',1_000_000+n),None);}let new=t.elapsed();
 println!("public_overlay=50000 misses=200 legacy_directory_scan_us={} keyed_entry_plus_directory_us={}",old.as_micros(),new.as_micros());
}
