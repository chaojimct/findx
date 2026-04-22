//! 命令行参数（前台进程与 SCM 拉起的服务进程共用）。

use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "findx2-service")]
pub struct Cli {
    /// Windows 服务模式（须由 SCM 启动；手动调试请删此参数用前台）
    #[arg(long)]
    pub service: bool,

    /// index.bin 路径
    #[arg(short, long, default_value = "index.bin")]
    pub index: PathBuf,

    /// USN 卷（须与建索引一致）
    #[arg(short, long, default_value = "C:")]
    pub volume: String,

    /// 命名管道名（实际为 \\\\.\\pipe\\<名称>）
    #[arg(short, long, default_value = "findx2")]
    pub pipe: String,

    #[arg(long, default_value_t = 30)]
    pub save_interval_secs: u64,

    /// 若 index.bin 不存在时自动全量建库：首遍读全量元数据（较慢；时间与大小一上来即准）
    #[arg(long, default_value_t = false)]
    pub full_stat: bool,

    /// 自动全量建库时并行扫描的最大卷线程数（仅多卷生效）
    #[arg(long, default_value_t = 4)]
    pub max_scan_threads: usize,

    /// 关闭 Everything SDK v2 兼容窗口（默认开启；老 Everything 客户端将无法连本服务）。
    #[arg(long, default_value_t = false)]
    pub no_everything_ipc: bool,

    /// 关闭后台元数据回填线程（默认开启；关闭后 fast 首遍未命中的 size/mtime 将一直为 0）。
    /// 适合超弱机：CPU/磁盘 IO 占用最低，但搜索时大小/时间筛选与排序不准。
    #[arg(long, default_value_t = false)]
    pub no_backfill: bool,

    /// 追加的排除目录（可多次传入）；优先级 = sidecar ∪ 命令行。
    /// 对历史已入库的命中条目，service 启动时会一次性打"已删除"墓碑。
    #[arg(long = "exclude-dir", value_name = "PATH")]
    pub exclude_dir: Vec<String>,

    #[command(subcommand)]
    pub cmd: Option<ServiceCmd>,
}

#[derive(Debug, Clone, clap::Subcommand)]
pub enum ServiceCmd {
    /// 注册 Windows 服务（需管理员）
    Install,
    /// 卸载 Windows 服务（需管理员）
    Uninstall,
}
