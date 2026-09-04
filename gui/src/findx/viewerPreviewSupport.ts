/** preset-office 能覆盖的后缀。刻意不含二进制 .ppt/.pot，避免公开版水印。 */
export const VIEWER_EXTS = new Set([
  "pdf",
  "ofd",
  "doc",
  "docx",
  "docm",
  "dot",
  "dotx",
  "dotm",
  "rtf",
  "odt",
  "xls",
  "xlsx",
  "xlsm",
  "xlsb",
  "ods",
  "pptx",
  "pptm",
  "potx",
  "potm",
  "ppsx",
  "ppsm",
  "odp",
]);

export const VIEWER_MAX_BYTES = 80 * 1024 * 1024;

export function fileExt(path: string): string {
  const i = path.lastIndexOf(".");
  return i < 0 ? "" : path.slice(i + 1).toLowerCase();
}

export function viewerCanPreview(path: string): boolean {
  return VIEWER_EXTS.has(fileExt(path));
}

export function formatViewerSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(k)), sizes.length - 1);
  const n = bytes / k ** i;
  const digits = i === 0 ? 0 : n < 10 ? 2 : n < 100 ? 2 : 1;
  return `${n.toFixed(digits)} ${sizes[i]}`;
}

export function formatViewerError(err: unknown): string {
  if (err instanceof Error && err.message) return err.message;
  const text = String(err ?? "").replace(/^Error:\s*/i, "").trim();
  return text || "内置预览失败";
}
