use crate::isolation::{self, DEFAULT_ISOLATED_PIPE};

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub pipe_name: String,
    pub endpoint: String,
    pub mock: bool,
    pub diag: bool,
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<LaunchOptions, String> {
    let mut pipe_name = DEFAULT_ISOLATED_PIPE.to_string();
    let mut mock = false;
    let mut diag = false;
    let mut iter = args.into_iter().peekable();
    // skip argv0
    let _ = iter.next();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(help_text().into()),
            "--mock" => mock = true,
            "--diag" => diag = true,
            "--pipe" => {
                let Some(value) = iter.next() else {
                    return Err("--pipe 需要一个隔离管道名".into());
                };
                pipe_name = value;
            }
            other if other.starts_with("--pipe=") => {
                pipe_name = other.trim_start_matches("--pipe=").to_string();
            }
            other => return Err(format!("未知参数: {other}\n{}", help_text())),
        }
    }
    let endpoint = isolation::normalize_pipe(&pipe_name)?;
    Ok(LaunchOptions {
        pipe_name,
        endpoint,
        mock,
        diag,
    })
}

pub fn help_text() -> String {
    format!(
        "\
findx2-gui-native — FindX 原生客户端（隔离运行，不替代 WebView GUI）

用法:
  findx2-gui-native [--mock] [--pipe NAME] [--diag]

选项:
  --mock          在隔离管道上启动模拟后端（不接触生产 findx2-service）
  --pipe NAME     隔离管道名（默认 {DEFAULT_ISOLATED_PIPE}）。生产名 findx2 会被拒绝
  --diag          启动时打开诊断浮层
  -h, --help      显示帮助

示例:
  cargo run -p findx2-gui-native -- --mock
  cargo run -p findx2-gui-native -- --pipe findx2-native-dev
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(|x| x.to_string()).collect()
    }

    #[test]
    fn default_is_isolated() {
        let opt = parse_args(args("findx2-gui-native")).unwrap();
        assert_eq!(opt.pipe_name, DEFAULT_ISOLATED_PIPE);
        assert!(!opt.mock);
    }

    #[test]
    fn rejects_production_pipe_flag() {
        assert!(parse_args(args("findx2-gui-native --pipe findx2")).is_err());
    }

    #[test]
    fn accepts_mock_and_diag() {
        let opt = parse_args(args("findx2-gui-native --mock --diag")).unwrap();
        assert!(opt.mock && opt.diag);
    }
}
