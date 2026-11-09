export type TipAnchor = {
  top: number;
  left: number;
  bottom: number;
};

export type TipBox = {
  top: number;
  left: number;
  maxWidth: number;
  maxHeight: number;
};

/**
 * 长路径提示限制在视口内，并限制高度，避免盖住整张结果表。
 * 文本用换行而不是把提示拉出屏幕。
 */
export function placeBoundedTip(
  anchor: TipAnchor,
  viewport: { width: number; height: number },
): TipBox {
  const margin = 8;
  const vw = Math.max(1, viewport.width);
  const vh = Math.max(1, viewport.height);
  const maxWidth = Math.max(120, Math.min(420, vw - margin * 2));
  const maxHeight = Math.max(48, Math.min(160, Math.floor(vh * 0.34)));
  let left = anchor.left;
  if (left + maxWidth > vw - margin) left = vw - margin - maxWidth;
  if (left < margin) left = margin;
  const below = anchor.bottom + 6;
  const above = anchor.top - 6 - maxHeight;
  let top = below;
  if (below + maxHeight > vh - margin && above >= margin) top = above;
  if (top < margin) top = margin;
  if (top + maxHeight > vh - margin) top = Math.max(margin, vh - margin - maxHeight);
  return { top, left, maxWidth, maxHeight };
}
