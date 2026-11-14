fn main() {
    #[path = "../win_versioninfo.rs"]
    mod win_versioninfo;
    win_versioninfo::embed(&[
        ("findx2", "findx2.exe", "FindX command-line search"),
        ("fx", "fx.exe", "FindX command-line search"),
    ]);
}
