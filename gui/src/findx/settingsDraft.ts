import type { FindxGuiSettings, UiThemePref } from "./findxGuiTypes";

/** 与 `FindxGuiSettings.search_limit: u32` 相同的上界，不是额外的性能限制。 */
export const SEARCH_LIMIT_U32_MAX = 4_294_967_295;

export type DraftCommit = { ok: true; value: number } | { ok: false; message: string };

/**
 * 编辑过程允许空串和未完成的数字。保存时才提交。
 * 接受 1..=u32::MAX。0 不接受：搜索循环把 0 当成「无上限」，旧界面 `parseInt(...) || 500` 也存不住 0。
 */
export function commitSearchLimit(text: string): DraftCommit {
  const trimmed = text.trim();
  if (trimmed === "") {
    return { ok: false, message: "结果条数上限还是空的。编辑时可以先清空，保存前需要填一个整数。" };
  }
  if (!/^\d+$/.test(trimmed)) {
    return { ok: false, message: "结果条数上限必须是十进制整数。" };
  }
  const value = Number(trimmed);
  if (!Number.isSafeInteger(value) || value < 1 || value > SEARCH_LIMIT_U32_MAX) {
    return {
      ok: false,
      message: `结果条数上限须在 1 到 ${SEARCH_LIMIT_U32_MAX} 之间，与设置字段 u32 一致。`,
    };
  }
  return { ok: true, value };
}

/** `save_interval_secs` 在后端是 u64，界面只拒绝空值、非整数和小于 1。 */
export function commitSaveIntervalSecs(text: string): DraftCommit {
  const trimmed = text.trim();
  if (trimmed === "") {
    return { ok: false, message: "增量落盘间隔还是空的。保存前需要填一个不小于 1 的整数秒。" };
  }
  if (!/^\d+$/.test(trimmed)) {
    return { ok: false, message: "增量落盘间隔必须是十进制整数秒。" };
  }
  const value = Number(trimmed);
  if (!Number.isSafeInteger(value) || value < 1) {
    return { ok: false, message: "增量落盘间隔必须是不小于 1 的整数秒。" };
  }
  return { ok: true, value };
}

export function closeDecision(isDirty: boolean, confirmed: boolean): "hide" | "stay" {
  if (!isDirty) return "hide";
  return confirmed ? "hide" : "stay";
}

export const CLOSE_DIRTY_MESSAGE = "设置已修改但未保存,确定要关闭吗？未保存的更改将丢失。";

export function discardUnsaved(input: {
  settings: FindxGuiSettings;
  original: FindxGuiSettings | null;
  theme: UiThemePref;
  originalTheme: UiThemePref;
}): { settings: FindxGuiSettings; theme: UiThemePref } {
  return {
    settings: input.original ?? input.settings,
    theme: input.originalTheme,
  };
}

/** 启动命令的返回值只说明命令已发出，不说明索引已可用。 */
export function serviceStartHint(kind: "sent" | "already" | "failed", detail?: string): string {
  if (kind === "sent") {
    return "已发送启动命令。命令返回不代表索引可用，请看主窗口状态栏。";
  }
  if (kind === "already") return "服务已在运行中。";
  return `启动失败：${detail ?? "未知错误"}`;
}
