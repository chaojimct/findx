export type ResultNavKey = "ArrowDown" | "ArrowUp" | "Home" | "End";

/** 方向键移动当前行。不改排序，只改选中下标。 */
export function moveResultSelection(
  current: number | null,
  key: ResultNavKey,
  count: number,
): number | null {
  if (count <= 0) return null;
  if (key === "Home") return 0;
  if (key === "End") return count - 1;
  if (current == null) return key === "ArrowUp" ? count - 1 : 0;
  if (key === "ArrowDown") return Math.min(count - 1, current + 1);
  return Math.max(0, current - 1);
}

export function scrollTopForRow(
  row: number,
  rowHeight: number,
  scrollTop: number,
  viewport: number,
): number {
  const top = Math.max(0, row) * rowHeight;
  const bottom = top + rowHeight;
  if (top < scrollTop) return top;
  if (bottom > scrollTop + viewport) return Math.max(0, bottom - viewport);
  return scrollTop;
}
