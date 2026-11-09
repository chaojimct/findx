import { useEffect, useRef, type RefObject } from "react";
import { moveResultSelection, type ResultNavKey } from "./resultSelection";

export type ResultKeyAction = { type: "move"; index: number } | { type: "open" };

function isTextField(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || typeof el.tagName !== "string") return false;
  return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable === true;
}

/**
 * 结果列表的方向键。捕获阶段处理，避免停在搜索框或输入法里。
 * 侧栏里的输入框不抢。搜索框里的上下键移动选中行。
 */
export function interpretResultKey(
  event: {
    key: string;
    altKey?: boolean;
    ctrlKey?: boolean;
    metaKey?: boolean;
    isComposing?: boolean;
    target: EventTarget | null;
  },
  ctx: { count: number; selected: number | null; searchInput: HTMLElement | null },
): ResultKeyAction | null {
  if (event.isComposing) return null;
  if (event.altKey || event.ctrlKey || event.metaKey) return null;
  const key = event.key;
  const nav = key === "ArrowDown" || key === "ArrowUp" || key === "Home" || key === "End";
  if (!nav && key !== "Enter") return null;
  const el = event.target as Node | null;
  const inSearch = !!ctx.searchInput && !!el && (el === ctx.searchInput || ctx.searchInput.contains(el));
  if (isTextField(event.target) && !inSearch) return null;
  if (inSearch && (key === "Home" || key === "End" || key === "Enter")) return null;
  if (key === "Enter") {
    const tag = (event.target as HTMLElement | null)?.tagName;
    if (tag === "BUTTON" || tag === "INPUT" || tag === "TEXTAREA" || ctx.selected == null) return null;
    return { type: "open" };
  }
  if (ctx.count <= 0) return null;
  const index = moveResultSelection(ctx.selected, key as ResultNavKey, ctx.count);
  if (index == null) return null;
  return { type: "move", index };
}

/** FindXSearchApp 使用的监听。测试渲染这个组件并派发真实 keydown。 */
export function ResultNavigation({
  count,
  selected,
  onSelect,
  onOpen,
  searchInputRef,
}: {
  count: number;
  selected: number | null;
  onSelect: (index: number) => void;
  onOpen: () => void;
  searchInputRef: RefObject<HTMLInputElement | null>;
}) {
  const latest = useRef({ count, selected, onSelect, onOpen, searchInputRef });
  latest.current = { count, selected, onSelect, onOpen, searchInputRef };
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      const now = latest.current;
      const action = interpretResultKey(event, {
        count: now.count,
        selected: now.selected,
        searchInput: now.searchInputRef.current,
      });
      if (!action) return;
      event.preventDefault();
      event.stopPropagation();
      if (action.type === "open") now.onOpen();
      else now.onSelect(action.index);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);
  return null;
}
