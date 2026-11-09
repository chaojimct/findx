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
  const width = Math.max(1, tableWidth);
  const px = layoutColumnFractions(base, width).map((f) => f * width);
  const floors = COLUMN_MIN_PX.map((min, i) => Math.min(min, i === 0 ? width * 0.55 : width * 0.08));
  const rightIdx = (leftIdx + 1) as 1 | 2 | 3;
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
