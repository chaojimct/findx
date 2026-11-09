import type { ReactNode } from "react";
import { useState } from "react";
import { createPortal } from "react-dom";
import { placeBoundedTip, type TipBox } from "./tipPlacement";

/**
 * 视口内换行的路径提示。不用原生 title，原生提示既不换行也会挡住结果。
 */
export function BoundedTip({
  text,
  children,
  className,
}: {
  text: string;
  children: ReactNode;
  className?: string;
}) {
  const [box, setBox] = useState<TipBox | null>(null);
  if (!text) {
    return <span className={className}>{children}</span>;
  }
  const open = (el: HTMLElement) => {
    const rect = el.getBoundingClientRect();
    setBox(
      placeBoundedTip(
        { top: rect.top, left: rect.left, bottom: rect.bottom },
        { width: window.innerWidth, height: window.innerHeight },
      ),
    );
  };
  return (
    <span
      className={className}
      style={{
        display: "block",
        minWidth: 0,
        maxWidth: "100%",
        overflow: "hidden",
        textOverflow: "ellipsis",
        whiteSpace: "nowrap",
      }}
      onMouseEnter={(e) => open(e.currentTarget)}
      onMouseLeave={() => setBox(null)}
      onFocus={(e) => open(e.currentTarget)}
      onBlur={() => setBox(null)}
    >
      {children}
      {box &&
        createPortal(
          <span
            className="fx-bounded-tip"
            role="tooltip"
            style={{
              top: box.top,
              left: box.left,
              maxWidth: box.maxWidth,
              maxHeight: box.maxHeight,
            }}
          >
            {text}
          </span>,
          document.body,
        )}
    </span>
  );
}
