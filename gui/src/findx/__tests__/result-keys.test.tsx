import { act, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";
import { ResultNavigation } from "../resultNavigation";

function Harness() {
  const searchRef = useRef<HTMLInputElement>(null);
  const [selected, setSelected] = useState<number | null>(null);
  return (
    <div>
      <input ref={searchRef} className="search" />
      <input className="sidebar" />
      <div className="sel">{selected == null ? "none" : String(selected)}</div>
      <ResultNavigation
        count={3}
        selected={selected}
        onSelect={setSelected}
        onOpen={() => {}}
        searchInputRef={searchRef}
      />
    </div>
  );
}

describe("结果列表方向键走生产监听", () => {
  let root: Root | null = null;
  let host: HTMLDivElement | null = null;

  afterEach(() => {
    act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  it("搜索框里连按两次向下，选中从第一条到第二条", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    await act(async () => {
      root?.render(<Harness />);
    });
    const search = host.querySelector(".search") as HTMLInputElement;
    const sidebar = host.querySelector(".sidebar") as HTMLInputElement;
    search.focus();
    const down = () =>
      search.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }));
    await act(async () => {
      down();
    });
    expect(host.querySelector(".sel")?.textContent).toBe("0");
    await act(async () => {
      down();
    });
    expect(host.querySelector(".sel")?.textContent).toBe("1");

    sidebar.focus();
    await act(async () => {
      sidebar.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }));
    });
    expect(host.querySelector(".sel")?.textContent).toBe("1");

    const composing = new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true });
    Object.defineProperty(composing, "isComposing", { get: () => true });
    search.focus();
    await act(async () => {
      search.dispatchEvent(composing);
    });
    expect(host.querySelector(".sel")?.textContent).toBe("1");
  });
});
