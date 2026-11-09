/** 与 design-tokens.css 的 `--fx-row-height` 保持同一数值。虚拟列表和单元格都用它。 */
export const RESULT_ROW_HEIGHT_PX = 32;

export type ColFrac = [number, number, number, number];

/** 名称、路径、大小、修改时间。名称的下限更高，窄表时先压缩其余列。 */
export const COLUMN_MIN_PX = [160, 72, 56, 88] as const;

function normalized(saved: ColFrac): ColFrac {
  const weights = saved.map((n) => (Number.isFinite(n) && n > 0 ? n : 0));
  const sum = weights.reduce((a, b) => a + b, 0) || 1;
  return weights.map((w) => w / sum) as ColFrac;
}

/**
 * 按当前表格像素宽度分配列。只改宽度比例，不改排序。
 * 名称列至少 min(160px, 55% 宽度)，差额从路径、修改时间、大小里扣。
 */
export function layoutColumnFractions(saved: ColFrac, tableWidth: number): ColFrac {
  const width = Math.max(1, tableWidth);
  const base = normalized(saved);
  const px = base.map((f) => f * width);
  const nameFloor = Math.min(COLUMN_MIN_PX[0], width * 0.55);
  let deficit = nameFloor - px[0];
  if (deficit > 0) {
    for (const idx of [1, 3, 2] as const) {
      if (deficit <= 0) break;
      const give = Math.min(deficit, Math.max(0, px[idx] - 24));
      px[idx] -= give;
      deficit -= give;
    }
    px[0] = nameFloor - deficit;
  }
  const total = px.reduce((a, b) => a + b, 0) || 1;
  return px.map((n) => n / total) as ColFrac;
}

/**
 * 拖动两列之间的分隔条。任一侧低于像素下限就停住，避免把名称列拖没。
 */
export function applyColumnDrag(
  base: ColFrac,
  leftIdx: 0 | 1 | 2,
  deltaPx: number,
  tableWidth: number,
): ColFrac {
  return applyColumnDragBetween(base, leftIdx, leftIdx + 1, deltaPx, tableWidth);
}

/**
 * 拖动两列之间的分隔条。任一侧低于像素下限就停住，避免把名称列拖没。
 * rightIdx 可以不是 leftIdx+1，这样中间有列被隐藏时仍能拖可见列。
 */
export function applyColumnDragBetween(
  base: ColFrac,
  leftIdx: number,
  rightIdx: number,
  deltaPx: number,
  tableWidth: number,
): ColFrac {
  const width = Math.max(1, tableWidth);
  const px = layoutColumnFractions(base, width).map((f) => f * width);
  const floors = COLUMN_MIN_PX.map((min, i) => Math.min(min, i === 0 ? width * 0.55 : width * 0.08));
  let left = px[leftIdx] + deltaPx;
  let right = px[rightIdx] - deltaPx;
  if (left < floors[leftIdx]) {
    right -= floors[leftIdx] - left;
    left = floors[leftIdx];
  }
  if (right < floors[rightIdx]) {
    left -= floors[rightIdx] - right;
    right = floors[rightIdx];
  }
  if (left < floors[leftIdx] || right < floors[rightIdx]) {
    return layoutColumnFractions(base, width);
  }
  const next = [...px];
  next[leftIdx] = left;
  next[rightIdx] = right;
  const total = next.reduce((a, b) => a + b, 0) || 1;
  return next.map((n) => n / total) as ColFrac;
}

export const COL_IDS = ["name", "path", "size", "mtime"] as const;
export type ColId = (typeof COL_IDS)[number];

export const COL_LABEL: Record<ColId, string> = {
  name: "名称",
  path: "路径",
  size: "大小",
  mtime: "修改时间",
};

export const COL_INDEX: Record<ColId, 0 | 1 | 2 | 3> = {
  name: 0,
  path: 1,
  size: 2,
  mtime: 3,
};

/**
 * 表头完整文字的最小宽度。放不下就隐藏低优先列，而不是把「大小」画成「大」。
 * 名称列最后保留。
 */
/** 名称列至少要能读出一段文件名，不能只剩两三个字。 */
export const NAME_COLUMN_MIN_PX = 160;

export const COLUMN_HEADER_MIN_PX: Record<ColId, number> = {
  name: NAME_COLUMN_MIN_PX,
  path: 72,
  size: 72,
  mtime: 96,
};

const DROP_FIRST: ColId[] = ["size", "mtime", "path"];

export function visibleColumnIds(tableWidth: number): ColId[] {
  const width = Math.max(0, tableWidth);
  let ids: ColId[] = ["name", "path", "size", "mtime"];
  const need = (cols: ColId[]) => cols.reduce((sum, id) => sum + COLUMN_HEADER_MIN_PX[id], 0);
  for (const drop of DROP_FIRST) {
    if (need(ids) <= width) break;
    ids = ids.filter((id) => id !== drop);
  }
  return ids;
}

/** 可见列至少有表头最小宽度。多余空间按已保存比例分，不改排序。 */
export function columnPixelWidths(
  saved: ColFrac,
  tableWidth: number,
  visible: readonly ColId[] = visibleColumnIds(tableWidth),
): Record<ColId, number> {
  const width = Math.max(1, tableWidth);
  const px: Record<ColId, number> = { name: 0, path: 0, size: 0, mtime: 0 };
  if (visible.length === 0) return px;
  const weights = visible.map((id) => {
    const w = saved[COL_INDEX[id]];
    return Number.isFinite(w) && w > 0 ? w : 0.0001;
  });
  const weightSum = weights.reduce((a, b) => a + b, 0) || 1;
  const minSum = visible.reduce((sum, id) => sum + COLUMN_HEADER_MIN_PX[id], 0);
  const extra = Math.max(0, width - minSum);
  visible.forEach((id, i) => {
    px[id] = COLUMN_HEADER_MIN_PX[id] + extra * (weights[i] / weightSum);
  });
  if (minSum > width) {
    visible.forEach((id) => {
      px[id] = COLUMN_HEADER_MIN_PX[id];
    });
  }
  return px;
}

/** 窄窗口自动收起面板，给结果表留宽。用户当次手动打开或关闭优先于自动策略。 */
export const PREVIEW_AUTO_HIDE_BELOW = 900;
export const SIDEBAR_AUTO_HIDE_BELOW = 760;

export function resolvePanelOpen(args: {
  windowWidth: number;
  savedOpen: boolean;
  forced: "show" | "hide" | null;
  kind: "sidebar" | "preview";
}): boolean {
  if (args.forced === "show") return true;
  if (args.forced === "hide") return false;
  const limit = args.kind === "preview" ? PREVIEW_AUTO_HIDE_BELOW : SIDEBAR_AUTO_HIDE_BELOW;
  if (args.windowWidth < limit) return false;
  return args.savedOpen;
}

export const SIDEBAR_WIDTH_PX = 210;
export const NAME_READABLE_PX = 180;

/**
 * 用内容区实际宽度，不用 window.innerWidth。
 * 没在本次手动打开时，小窗先收起面板，保证名称区域够读。
 * forced 只来自本次点击，不把上次保存的「展开」当成手动覆盖。
 */
export function resolvePanels(args: {
  contentWidth: number;
  previewWidth: number;
  savedSidebar: boolean;
  savedPreview: boolean;
  sidebarForce: "show" | "hide" | null;
  previewForce: "show" | "hide" | null;
}): { sidebar: boolean; preview: boolean } {
  let sidebar = resolvePanelOpen({
    windowWidth: args.contentWidth,
    savedOpen: args.savedSidebar,
    forced: args.sidebarForce,
    kind: "sidebar",
  });
  let preview = resolvePanelOpen({
    windowWidth: args.contentWidth,
    savedOpen: args.savedPreview,
    forced: args.previewForce,
    kind: "preview",
  });
  const results = (side: boolean, prev: boolean) =>
    args.contentWidth - (side ? SIDEBAR_WIDTH_PX : 0) - (prev ? args.previewWidth + 4 : 0);
  if (results(sidebar, preview) < NAME_READABLE_PX && preview && args.previewForce !== "show") {
    preview = false;
  }
  if (results(sidebar, preview) < NAME_READABLE_PX && sidebar && args.sidebarForce !== "show") {
    sidebar = false;
  }
  return { sidebar, preview };
}
