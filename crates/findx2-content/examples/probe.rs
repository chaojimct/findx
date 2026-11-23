//! 临时探针：真实 fixture 抽取验证（保留，日后可转为回归测试）。
fn main() {
    let dir = r"C:\Users\chaoj\dev\tools\findx-content\crates\findx2-gui-native\tests\fixtures";
    for f in [
        "findx-public-layout.docx",
        "findx-public-layout.xlsx",
        "findx-public-three-pages.pdf",
    ] {
        let p = std::path::Path::new(dir).join(f);
        match findx2_content::extract_text(&p) {
            Ok(t) => println!(
                "{f}: OK {} chars head={:?}",
                t.text.len(),
                t.text.chars().take(80).collect::<String>()
            ),
            Err(e) => println!("{f}: FAIL {e}"),
        }
    }
    // 真实世界 PDF（lopdf 自带资产）：区分“fixture 手写 PDF 太糙”还是“抽取器不行”。
    let real =
        r"C:\Users\chaoj\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\lopdf-0.34.0\assets\example.pdf";
    match findx2_content::extract_text(std::path::Path::new(real)) {
        Ok(t) => println!(
            "example.pdf: OK {} chars head={:?}",
            t.text.len(),
            t.text.chars().take(80).collect::<String>()
        ),
        Err(e) => println!("example.pdf: FAIL {e}"),
    }
}
