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
