use crate::isolation::{self, DEFAULT_ISOLATED_PIPE, PRODUCTION_PIPE};

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub pipe_name: String,
    pub endpoint: String,
    pub mock: bool,
    pub live: bool,
    pub diag: bool,
    pub preview_file: Option<String>,
    /// Official installed product (see isolation::detect_official_entry), not a fixture.
    pub official_product: bool,
}

impl LaunchOptions {
    pub fn window_title(&self) -> &'static str {
        isolation::window_title(self.official_product, self.live)
    }
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<LaunchOptions, String> {
    let installed = std::env::current_exe()
        .ok()
        .is_some_and(|exe| isolation::detect_official_entry(&exe));
    parse_args_for_install(args, installed)
}

fn parse_args_for_install(
    args: impl IntoIterator<Item = String>,
    installed: bool,
) -> Result<LaunchOptions, String> {
    let mut pipe_name = DEFAULT_ISOLATED_PIPE.to_string();
    let mut pipe_set = false;
    let mut mock = false;
    let mut live = false;
    let mut diag = false;
    let mut preview_file = None;
    let mut iter = args.into_iter().peekable();
    // skip argv0
    let _ = iter.next();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(help_text().into()),
            "--mock" => mock = true,
            "--live" => live = true,
            "--diag" => diag = true,
            "--preview-file" => {
                preview_file = Some(iter.next().ok_or("--preview-file 需要本地文件路径")?);
            }
            "--pipe" => {
                let Some(value) = iter.next() else {
                    return Err("--pipe 需要一个管道名".into());
                };
                pipe_name = value;
                pipe_set = true;
            }
            other if other.starts_with("--pipe=") => {
                pipe_name = other.trim_start_matches("--pipe=").to_string();
                pipe_set = true;
            }
            other => return Err(format!("未知参数: {other}\n{}", help_text())),
        }
    }
    if installed && !mock && !pipe_set {
        live = true;
    }
    if live && mock {
        return Err(
            "--live 与 --mock 不能同时使用。--live 连接现有生产服务；--mock 使用隔离模拟数据。"
                .into(),
        );
    }
    if live && !pipe_set {
        pipe_name = PRODUCTION_PIPE.to_string();
    }
    let official_product =
        isolation::is_official_product_entry(installed, mock, live, pipe_set, &pipe_name);
    let endpoint = isolation::normalize_pipe_ex(&pipe_name, live)?;
    Ok(LaunchOptions {
        pipe_name,
        endpoint,
        mock,
        live,
        diag,
        preview_file,
        official_product,
    })
}

pub fn help_text() -> String {
    format!(
        "\
FindX — 正式原生客户端（未安装/显式隔离入口使用独立开发配置）
用法:
  FindX [--mock | --live] [--pipe NAME] [--diag]

安装版无参数启动连接当前用户的正式服务；不会自动启动引擎或扫描全盘。
服务维护/改参数必须在设置页单独确认。未安装入口默认隔离。

选项:
  --mock          在隔离管道上启动模拟后端（不接触生产 findx2-service）
  --live          连接现有生产管道 findx2；连接本身不启动/重启服务、不写索引
  --pipe NAME     管道名。默认隔离名是 {DEFAULT_ISOLATED_PIPE}；加 --live 时默认是 {PRODUCTION_PIPE}。
                  生产名 findx2 仅在 --live 时允许，单独 --pipe findx2 会被拒绝
  --preview-file PATH  直接只读预览本地文件（不插入搜索结果、不要求索引包含该文件）
  --installer-health DIR  独立临时配置启动、绘制并正常退出，仅供安装器健康检查
  --diag          启动时打开诊断浮层
  -h, --help      显示帮助

示例:
  cargo run -p findx2-gui-native -- --mock
  cargo run -p findx2-gui-native -- --live
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
        assert!(!opt.live);
        assert_eq!(opt.window_title(), "FindX Native (isolated)");
    }

    #[test]
    fn rejects_production_pipe_flag() {
        assert!(parse_args(args("findx2-gui-native --pipe findx2")).is_err());
        assert!(parse_args(args(r"findx2-gui-native --pipe \\.\pipe\findx2")).is_err());
    }

    #[test]
    fn accepts_mock_and_diag() {
        let opt = parse_args(args("findx2-gui-native --mock --diag")).unwrap();
        assert!(opt.mock && opt.diag);
        assert!(!opt.live);
    }

    #[test]
    fn live_allows_production_pipe() {
        let opt = parse_args(args("findx2-gui-native --live")).unwrap();
        assert!(opt.live);
        assert!(!opt.mock);
        assert_eq!(opt.pipe_name, PRODUCTION_PIPE);
        assert!(!opt.official_product);
        assert_eq!(opt.window_title(), isolation::LIVE_DEV_WINDOW_TITLE);
        #[cfg(windows)]
        assert_eq!(opt.endpoint, r"\\.\pipe\findx2");

        let opt = parse_args(args("findx2-gui-native --live --pipe findx2")).unwrap();
        assert!(opt.live);
        assert_eq!(opt.pipe_name, "findx2");
    }

    #[test]
    fn live_and_mock_conflict() {
        assert!(parse_args(args("findx2-gui-native --live --mock")).is_err());
    }
    #[test]
    fn installed_alias_defaults_to_existing_service_without_starting_it() {
        let opt = parse_args_for_install(args("FindX.exe"), true).unwrap();
        assert!(opt.live);
        assert_eq!(opt.pipe_name, PRODUCTION_PIPE);
        assert!(opt.official_product);
        assert_eq!(opt.window_title(), isolation::OFFICIAL_WINDOW_TITLE);
        let opt = parse_args_for_install(args("FindX.exe --live"), true).unwrap();
        assert!(opt.official_product);
        assert_eq!(opt.window_title(), isolation::OFFICIAL_WINDOW_TITLE);
        let opt = parse_args_for_install(args("FindX.exe --mock"), true).unwrap();
        assert!(!opt.live);
        assert!(opt.mock);
        assert_eq!(opt.pipe_name, DEFAULT_ISOLATED_PIPE);
        assert!(!opt.official_product);
        assert_eq!(opt.window_title(), isolation::ISOLATED_WINDOW_TITLE);
        let opt = parse_args_for_install(args("FindX.exe --pipe isolated-fixture"), true).unwrap();
        assert!(!opt.live);
        assert_eq!(opt.pipe_name, "isolated-fixture");
        assert!(!opt.official_product);
        assert_eq!(opt.window_title(), isolation::ISOLATED_WINDOW_TITLE);
    }
}
