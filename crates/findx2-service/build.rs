fn main() {
    #[path = "../win_versioninfo.rs"]
    mod win_versioninfo;
    win_versioninfo::embed(&[("findx2-service", "findx2-service.exe", "FindX search service")]);
}
