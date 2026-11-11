/** 主窗口诊断。默认关。Ctrl+Shift+D，或 localStorage findx_dev_diag=1，或 ?fxDiag=1。 */

export const DIAG_STORAGE_KEY = "findx_dev_diag";
export const DIAG_LAST_KEY = "findx_dev_diag_last";

export type KeyTrace = {
  type: string;
  key: string;
  code: string;
  keyCode: number;
  isComposing: boolean;
  target: string;
  active: string;
  count: number;
  selectedBefore: number | null;
  selectedAfter: number | null;
  decision: string;
};

export type SelectionReset = {
  reason: string;
  selected: number | null;
};

export type LayoutSample = {
  at: number;
  innerWidth: number;
  innerHeight: number;
  docClientWidth: number;
  docClientHeight: number;
  rootClientWidth: number;
  rootClientHeight: number;
  observerWidth: number;
  devicePixelRatio: number;
  screenWidth: number;
  screenHeight: number;
  screenAvailWidth: number;
  screenAvailHeight: number;
  visualViewportWidth: number | null;
  visualViewportHeight: number | null;
  visualViewportScale: number | null;
};

const traces: KeyTrace[] = [];
const resets: SelectionReset[] = [];
let sample: LayoutSample = {
  at: 0,
  innerWidth: 0,
  innerHeight: 0,
  docClientWidth: 0,
  docClientHeight: 0,
  rootClientWidth: 0,
  rootClientHeight: 0,
  observerWidth: 0,
  devicePixelRatio: 1,
  screenWidth: 0,
  screenHeight: 0,
  screenAvailWidth: 0,
  screenAvailHeight: 0,
  visualViewportWidth: null,
  visualViewportHeight: null,
  visualViewportScale: null,
};

export function diagEnabled(): boolean {
  try {
    if (localStorage.getItem(DIAG_STORAGE_KEY) === "1") return true;
  } catch {
    /* ignore */
  }
  try {
    return new URLSearchParams(window.location.search).get("fxDiag") === "1";
  } catch {
    return false;
  }
}

export function setDiagEnabled(on: boolean) {
  try {
    localStorage.setItem(DIAG_STORAGE_KEY, on ? "1" : "0");
  } catch {
    /* ignore */
  }
}

export function noteLayoutSample(partial: Partial<LayoutSample>) {
  sample = { ...sample, ...partial, at: Date.now() };
}

export function readLayoutSample(): LayoutSample {
  return sample;
}

export function recordKeyTrace(trace: KeyTrace) {
  traces.push(trace);
  if (traces.length > 20) traces.splice(0, traces.length - 20);
}

export function recentKeyTraces(): KeyTrace[] {
  return traces.slice();
}

export function recordSelectionReset(reason: string, selected: number | null) {
  resets.push({ reason, selected });
  if (resets.length > 8) resets.splice(0, resets.length - 8);
}

export function recentSelectionResets(): SelectionReset[] {
  return resets.slice();
}

export function resetDiagForTests() {
  traces.splice(0, traces.length);
  resets.splice(0, resets.length);
}

export function describeTarget(target: EventTarget | null): string {
  const el = target as HTMLElement | null;
  if (!el || typeof el.tagName !== "string") return String(target);
  const cls = typeof el.className === "string" && el.className ? `.${el.className.trim().split(/\s+/).join(".")}` : "";
  return `${el.tagName}${cls}`;
}

export function formatDiagText(input: {
  decisionWidth: number;
  sidebarReason: string;
  previewReason: string;
  sidebarForce: string;
  previewForce: string;
  savedSidebar: boolean;
  savedPreview: boolean;
  selected: number | null;
  selectedRows: number;
  resultsCount: number;
  sample: LayoutSample;
  traces: KeyTrace[];
  resets: SelectionReset[];
}): string {
  const s = input.sample;
  const lines = [
    "这段文字只报告测量，不改变折叠。截图像素不是 innerWidth。",
    `决策宽度 contentWidth=${input.decisionWidth}`,
    `inner ${s.innerWidth}x${s.innerHeight}  documentElement.client ${s.docClientWidth}x${s.docClientHeight}`,
    `root.client ${s.rootClientWidth}x${s.rootClientHeight}  ResizeObserver ${s.observerWidth}`,
    `devicePixelRatio ${s.devicePixelRatio}  screen ${s.screenWidth}x${s.screenHeight}  avail ${s.screenAvailWidth}x${s.screenAvailHeight}`,
    `visualViewport ${s.visualViewportWidth ?? "-"}x${s.visualViewportHeight ?? "-"} scale ${s.visualViewportScale ?? "-"}`,
    input.previewReason,
    input.sidebarReason,
    `手动 force preview=${input.previewForce} sidebar=${input.sidebarForce}  保存 preview=${input.savedPreview ? "开" : "关"} sidebar=${input.savedSidebar ? "开" : "关"}`,
    `selectedIndex=${input.selected === null ? "null" : String(input.selected)}  tr.selected=${input.selectedRows}  results=${input.resultsCount}`,
    input.resets.length === 0
      ? "选中未被查询清掉"
      : `选中被清掉: ${input.resets.map((item) => `${item.reason}@${item.selected === null ? "null" : item.selected}`).join(" | ")}`,
    "最近按键（空则说明事件没进页面）:",
    ...input.traces.map(
      (item) =>
        `${item.type} key=${item.key} code=${item.code} keyCode=${item.keyCode} composing=${item.isComposing} target=${item.target} active=${item.active} ${item.decision} before=${item.selectedBefore === null ? "null" : item.selectedBefore} after=${item.selectedAfter === null ? "null" : item.selectedAfter} n=${item.count}`,
    ),
  ];
  return lines.join("\n");
}
