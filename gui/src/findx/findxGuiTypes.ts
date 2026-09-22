/** 与 Rust findx_settings::FindxGuiSettings 对应；主窗口与设置窗口共用。 */

export type RunMode = "service" | "standalone";

export type FindxGuiSettings = {
  indexPath: string;
  volume: string;
  pipeName: string;
  pinyinDefault: boolean;
  serviceExePath: string;
  searchLimit: number;
  autoStartService?: boolean;
  firstIndexFullMetadata?: boolean;
  runMode?: RunMode;
  drives?: string[];
  excludedDirs?: string[];
  enableMetadataBackfill?: boolean;
  enableEverythingIpc?: boolean;
  saveIntervalSecs?: number;
  /**
   * 随系统启动 GUI（登录时自动拉起托盘）。
   *
   * Windows 写 `HKCU\...\CurrentVersion\Run`（无需管理员）；
   * 由 Rust 侧 `autostart_*` 命令读写，不依赖第三方插件。
   *
   * 注意与 `autoStartService` 区分：后者是「GUI 起来后自动拉索引服务」，
   * 本项是「开机让 GUI 自己起来」。服务本身在服务模式下由 SCM 开机自启，与本项无关。
   */
  autoStartApp?: boolean;
  /** 启动 GUI 后自动检查 GitHub 新版本（仅提示，不自动下载安装）。 */
  autoCheckUpdate?: boolean;
};

export type UiThemePref = "light" | "dark" | "system";

export const UI_THEME_KEY = "findx2_ui_theme";

export function loadUiThemePref(): UiThemePref {
  try {
    const v = localStorage.getItem(UI_THEME_KEY);
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {
    /* ignore */
  }
  return "light";
}

/** 与 Rust `autostart::AutostartState` 对应（「随系统启动」开关的真实状态）。 */
export type AutostartState = {
  /** 是否已启用且指向当前 exe */
  enabled: boolean;
  /** 注册表里记录的命令行（仅 Windows） */
  command?: string;
  /** 平台是否支持由 FindX 管理自启 */
  supported: boolean;
  /** 不支持的原因，直接展示 */
  unsupportedReason?: string;
};

/** 与 Rust `app_update::AppUpdateInfo` 对应（GitHub Releases 检测） */
export type AppUpdateInfo = {
  ok: boolean;
  error?: string;
  currentVersion: string;
  latestVersion?: string;
  hasUpdate: boolean;
  releasePageUrl?: string;
  publishedAt?: string;
  /** Windows 安装器（FindX-*-setup.exe）直链；非 Windows 或资产缺失时缺省 */
  downloadUrl?: string;
  /** 安装包字节数 */
  assetSize?: number;
  /** 发行说明（截断到 600 字符） */
  releaseNotes?: string;
};

/** 后端 `findx2-update-progress` 事件载荷 */
export type UpdateProgressEvent = {
  downloaded: number;
  total: number;
  percent: number;
};

/** 后端 `findx2-update-finished` 事件载荷 */
export type UpdateFinishedEvent = {
  ok: boolean;
  path?: string;
  error?: string;
};
