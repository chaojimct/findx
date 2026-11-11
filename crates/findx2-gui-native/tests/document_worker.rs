//! End-to-end tests run the actual GUI executable's isolated document-worker entry.
use findx2_gui_native::preview_worker::{Output, Request};
use std::{
    io::Write,
    process::{Command, Stdio},
};
fn fixture(name: &str) -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .to_string_lossy()
        .into_owned()
}
fn worker(req: Request) -> Result<Output, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_findx2-gui-native"))
        .arg("--document-preview-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut input = child.stdin.take().unwrap();
        input.write_all(&serde_json::to_vec(&req).unwrap()).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}
#[cfg(windows)]
#[test]
fn multi_page_pdf_has_distinct_pages_and_clamps_out_of_range() {
    let mut pages = Vec::new();
    for page in [0, 1, 2, 99] {
        let out = worker(Request::Pdf {
            path: fixture("findx-public-three-pages.pdf"),
            page,
            edge: 700,
        })
        .unwrap();
        match out {
            Output::Pdf {
                png,
                count,
                width,
                height,
            } => {
                assert_eq!(count, 3);
                assert_eq!(width, 560.);
                assert_eq!(height, 960.);
                assert!(png.starts_with(b"\x89PNG"));
                pages.push(png);
            }
            _ => panic!("not PDF"),
        }
    }
    assert_ne!(pages[0], pages[1]);
    assert_ne!(pages[1], pages[2]);
    assert_eq!(pages[2], pages[3]);
}
#[test]
fn public_ooxml_rtf_text_fallback_is_useful() {
    for ext in ["docx", "xlsx", "rtf"] {
        let Output::Text(text) = worker(Request::Text {
            path: fixture(&format!("findx-public-layout.{ext}")),
            ext: ext.into(),
        })
        .unwrap() else {
            panic!("not text")
        };
        assert!(text.len() > 20);
    }
}
#[test]
fn malformed_and_oversized_input_fail_calmly() {
    let dir = std::env::temp_dir().join(format!("findx-document-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("bad.docx");
    std::fs::write(&bad, b"not ZIP").unwrap();
    assert!(
        worker(Request::Text {
            path: bad.to_string_lossy().into_owned(),
            ext: "docx".into()
        })
        .is_err()
    );
    #[cfg(windows)]
    {
        let pdf = dir.join("large.pdf");
        let file = std::fs::File::create(&pdf).unwrap();
        file.set_len(33 * 1024 * 1024).unwrap();
        drop(file);
        let result = worker(Request::Pdf {
            path: pdf.to_string_lossy().into_owned(),
            page: 0,
            edge: 700,
        });
        assert!(matches!(result,Err(e) if e.contains("32 MiB")));
        std::fs::write(&pdf, b"%PDF-1.7\ncorrupt\n").unwrap();
        assert!(
            worker(Request::Pdf {
                path: pdf.to_string_lossy().into_owned(),
                page: 0,
                edge: 700
            })
            .is_err()
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
