use findx2_content::{aux_dir_for, open_or_create, AUX_FULL_SUFFIX, AUX_INITIALS_SUFFIX};

fn main() {
    let index_bin = std::path::Path::new("C:/Users/chaoj/AppData/Local/FindX-native-dev/index.bin");
    for (name, suffix) in [("initials", AUX_INITIALS_SUFFIX), ("full", AUX_FULL_SUFFIX)] {
        let dir = aux_dir_for(index_bin, suffix);
        let cidx = open_or_create(&dir).expect("open aux");
        println!("{name}: docs={}", cidx.num_docs());
    }
    let ini = open_or_create(&aux_dir_for(index_bin, AUX_INITIALS_SUFFIX)).unwrap();
    for q in ["qw", "qws", "zhong", "zhongwen"] {
        let hits = ini.search_pinyin(q, 5, false).unwrap_or_default();
        println!("q={q} hits={}", hits.len());
        for (v, f, s) in hits.iter().take(2) {
            println!("   score={s:.2} {v}:{f}");
        }
    }
    let full = open_or_create(&aux_dir_for(index_bin, AUX_FULL_SUFFIX)).unwrap();
    for q in ["quan wen", "zhong wen", "quanwen", "machuntian"] {
        let hits = full.search_pinyin(q, 5, true).unwrap_or_default();
        println!("full q={q} hits={}", hits.len());
    }
}
