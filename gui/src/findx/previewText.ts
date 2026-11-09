/** 普通文本换行；代码保留换行并可横向滚动。 */
const PROSE_EXTS = new Set(["txt", "log", "md", "markdown", "csv", "tsv"]);

export function previewWhitespace(ext: string): "prose" | "code" {
  return PROSE_EXTS.has(ext.toLowerCase()) ? "prose" : "code";
}
