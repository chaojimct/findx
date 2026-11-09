/**
 * 深色是分层深灰。这些值和 design-tokens.css、index.html 首屏脚本保持同一色值。
 * 只用于 WebView 内容背景。Linux 标题栏按钮仍由系统绘制。
 */
export const THEME_DARK_BG = "#1b1b1b";
export const THEME_DARK_TITLE_BAR = "#242424";
export const THEME_DARK_TITLE_TEXT = "#f2f2f2";
export const THEME_LIGHT_BG = "#fafafa";
export const THEME_LIGHT_TITLE_BAR = "#ebebeb";
export const THEME_LIGHT_TITLE_TEXT = "#1f1f1f";

export function windowThemeColors(isDark: boolean): {
  backgroundColor: string;
  titleBarColor: string;
  titleBarTextColor: string;
} {
  if (isDark) {
    return {
      backgroundColor: THEME_DARK_BG,
      titleBarColor: THEME_DARK_TITLE_BAR,
      titleBarTextColor: THEME_DARK_TITLE_TEXT,
    };
  }
  return {
    backgroundColor: THEME_LIGHT_BG,
    titleBarColor: THEME_LIGHT_TITLE_BAR,
    titleBarTextColor: THEME_LIGHT_TITLE_TEXT,
  };
}
