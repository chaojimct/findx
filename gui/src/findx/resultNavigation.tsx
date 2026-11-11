import { useEffect, useRef, type RefObject } from "react";
import { describeTarget, recordKeyTrace, type KeyTrace } from "./devDiag";
import { moveResultSelection, type ResultNavKey } from "./resultSelection";

export type ResultKeyAction = { type: "move"; index: number } | { type: "open" };

type KeyLike = {
  key: string;
  code?: string;
  altKey?: boolean;
  ctrlKey?: boolean;
  metaKey?: boolean;
  isComposing?: boolean;
  target: EventTarget | null;
};

function isTextField(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || typeof el.tagName !== "string") return false;
  return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT" || el.isContentEditable === true;
}

/** ArrowDown 之外，部分环境会给出 Down / Unidentified + code。 */
export function normalizeNavKey(event: { key: string; code?: string }): ResultNavKey | null {
  const code = event.code ?? "";
  if (event.key === "ArrowDown" || event.key === "Down" || code === "ArrowDown") return "ArrowDown";
  if (event.key === "ArrowUp" || event.key === "Up" || code === "ArrowUp") return "ArrowUp";
  if (event.key === "Home" || code === "Home") return "Home";
  if (event.key === "End" || code === "End") return "End";
  return null;
}

/**
 * 结果列表的方向键。捕获阶段处理，避免停在搜索框或输入法里。
 * 侧栏里的输入框不抢。搜索框里的上下键移动选中行。
 * 输入法组合中的方向键先跳过，等 keyup 且不再组合时再移动一次。
 */
export function interpretResultKey(
  event: KeyLike,
  ctx: { count: number; selected: number | null; searchInput: HTMLElement | null },
): ResultKeyAction | null {
  if (event.isComposing) return null;
  if (event.altKey || event.ctrlKey || event.metaKey) return null;
  const nav = normalizeNavKey(event);
  const key = nav ?? (event.key === "Enter" ? "Enter" : null);
  if (!key) return null;
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
  const index = moveResultSelection(ctx.selected, key, ctx.count);
  if (index == null) return null;
  return { type: "move", index };
}

function traceOf(
  event: KeyboardEvent,
  ctx: { count: number; selected: number | null },
  decision: string,
  selectedAfter: number | null,
): KeyTrace {
  return {
    type: event.type,
    key: event.key,
    code: event.code,
    keyCode: event.keyCode,
    isComposing: event.isComposing,
    target: describeTarget(event.target),
    active: describeTarget(document.activeElement),
    count: ctx.count,
    selectedBefore: ctx.selected,
    selectedAfter,
    decision,
  };
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
    let pendingComposing: ResultNavKey | null = null;
    const onKey = (event: KeyboardEvent) => {
      const marked = event as KeyboardEvent & { __fxNav?: boolean };
      if (marked.__fxNav) return;
      marked.__fxNav = true;
      const now = latest.current;
      const ctx = { count: now.count, selected: now.selected, searchInput: now.searchInputRef.current };
      const nav = normalizeNavKey(event);
      if (event.type === "keyup") {
        if (!nav) return;
        if (pendingComposing === nav && !event.isComposing) {
          pendingComposing = null;
          const action = interpretResultKey(
            {
              key: nav,
              code: event.code,
              altKey: event.altKey,
              ctrlKey: event.ctrlKey,
              metaKey: event.metaKey,
              isComposing: false,
              target: event.target,
            },
            ctx,
          );
          if (action?.type === "move") {
            recordKeyTrace(traceOf(event, ctx, "move-from-keyup", action.index));
            event.preventDefault();
            now.onSelect(action.index);
            return;
          }
        }
        recordKeyTrace(traceOf(event, ctx, "keyup-noop", ctx.selected));
        return;
      }
      if (nav && event.isComposing) {
        pendingComposing = nav;
        recordKeyTrace(traceOf(event, ctx, "skip-composing", ctx.selected));
        return;
      }
      const action = interpretResultKey(event, ctx);
      if (!action) {
        recordKeyTrace(traceOf(event, ctx, nav ? "ignore" : "ignore-not-nav", ctx.selected));
        return;
      }
      pendingComposing = null;
      event.preventDefault();
      event.stopPropagation();
      if (action.type === "open") {
        recordKeyTrace(traceOf(event, ctx, "open", ctx.selected));
        now.onOpen();
        return;
      }
      recordKeyTrace(traceOf(event, ctx, "move", action.index));
      now.onSelect(action.index);
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKey, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onKey, true);
    };
  }, []);
  return null;
}
