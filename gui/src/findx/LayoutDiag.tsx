import { useEffect, useState } from "react";
import {
  DIAG_LAST_KEY,
  diagEnabled,
  formatDiagText,
  readLayoutSample,
  recentKeyTraces,
  recentSelectionResets,
  setDiagEnabled,
} from "./devDiag";
import { explainPanels } from "./resultTableLayout";

/** 固定浮层。关掉时不占布局。Ctrl+Shift+D 开关，并写入 localStorage。 */
export function LayoutDiag({
  decisionWidth,
  previewWidth,
  savedSidebar,
  savedPreview,
  sidebarForce,
  previewForce,
  selected,
  resultsCount,
}: {
  decisionWidth: number;
  previewWidth: number;
  savedSidebar: boolean;
  savedPreview: boolean;
  sidebarForce: "show" | "hide" | null;
  previewForce: "show" | "hide" | null;
  selected: number | null;
  resultsCount: number;
}) {
  const [open, setOpen] = useState(() => diagEnabled());
  const [text, setText] = useState("");

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!event.ctrlKey || !event.shiftKey || event.altKey || event.metaKey) return;
      if (event.key !== "D" && event.key !== "d") return;
      event.preventDefault();
      setOpen((cur) => {
        const next = !cur;
        setDiagEnabled(next);
        return next;
      });
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  useEffect(() => {
    if (!open) return;
    const write = () => {
      const explained = explainPanels({
        contentWidth: decisionWidth,
        previewWidth,
        savedSidebar,
        savedPreview,
        sidebarForce,
        previewForce,
      });
      const body = formatDiagText({
        decisionWidth,
        sidebarReason: explained.sidebarReason,
        previewReason: explained.previewReason,
        sidebarForce: sidebarForce ?? "null",
        previewForce: previewForce ?? "null",
        savedSidebar,
        savedPreview,
        selected,
        selectedRows: document.querySelectorAll("tr.selected").length,
        resultsCount,
        sample: readLayoutSample(),
        traces: recentKeyTraces(),
        resets: recentSelectionResets(),
      });
      setText(body);
      try {
        localStorage.setItem(DIAG_LAST_KEY, body);
      } catch {
        /* ignore */
      }
    };
    write();
    const timer = window.setInterval(write, 300);
    return () => window.clearInterval(timer);
  }, [open, decisionWidth, previewWidth, savedSidebar, savedPreview, sidebarForce, previewForce, selected, resultsCount]);

  if (!open) return null;
  return (
    <aside className="fx-layout-diag" data-fx-diag="1">
      <div className="fx-layout-diag-bar">
        <strong>诊断</strong>
        <span>Ctrl+Shift+D 关闭</span>
        <button type="button" onClick={() => void navigator.clipboard?.writeText(text).catch(() => {})}>
          复制
        </button>
      </div>
      <pre>{text}</pre>
    </aside>
  );
}
