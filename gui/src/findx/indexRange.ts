/**
 * 索引范围只改展示。勾选值仍是 list_drives 返回的原始字符串。
 */

export type IndexRangeItem = {
  /** 写回 settings.drives 的原值 */
  value: string;
  title: string;
  detail: string;
  system: boolean;
};

const SYSTEM_ROOTS = ["/proc", "/sys", "/dev", "/run", "/tmp", "/snap", "/boot"];

export function isSystemMount(path: string): boolean {
  const norm = path.replace(/\\/g, "/").replace(/\/+$/, "") || path;
  if (norm === "/" || /^[A-Za-z]:$/.test(path.trim())) return false;
  if (/^[A-Za-z]:\\Windows(\\|$)/i.test(path)) return true;
  return SYSTEM_ROOTS.some((root) => norm === root || norm.startsWith(`${root}/`));
}

export function presentIndexRange(path: string): IndexRangeItem {
  const value = path;
  const trimmed = path.trim();
  const norm = trimmed.replace(/\\/g, "/").replace(/\/+$/, "") || trimmed;
  if (norm === "/" || trimmed === "/") {
    return { value, title: "根目录", detail: path, system: false };
  }
  const drive = trimmed.match(/^([A-Za-z]:)\\?$/);
  if (drive) {
    return { value, title: `磁盘 ${drive[1]}`, detail: path, system: false };
  }
  const parts = norm.split("/").filter(Boolean);
  const base = parts[parts.length - 1] || trimmed;
  let title = base;
  if (norm === "/home" || norm === "/Users") title = "家目录";
  return { value, title, detail: path, system: isSystemMount(norm) };
}

/** 分区只影响列表分组，不改原字符串，也不改传入数组。 */
export function partitionIndexRanges(paths: readonly string[]): {
  general: IndexRangeItem[];
  system: IndexRangeItem[];
} {
  const general: IndexRangeItem[] = [];
  const system: IndexRangeItem[] = [];
  for (const path of paths) {
    const item = presentIndexRange(path);
    if (item.system) system.push(item);
    else general.push(item);
  }
  return { general, system };
}

export function toggleRangeValue(selected: readonly string[], value: string, on: boolean): string[] {
  const next = selected.filter((item) => item !== value);
  if (on) next.push(value);
  return next;
}
