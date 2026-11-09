import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { recentKeyTraces, resetDiagForTests } from "../devDiag";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => {
    if (cmd === "load_findx_settings") {
      return Promise.resolve({
        indexPath: "index.bin",
        volume: "C:",
        pipeName: "findx2",
        pinyinDefault: true,
        serviceExePath: "",
        searchLimit: 5000,
        autoStartService: true,
        runMode: "service",
        saveIntervalSecs: 30,
        autoCheckUpdate: false,
      });
    }
    if (cmd === "search_files") {
      return Promise.resolve({
        hits: [
          { name: "hit_batch_001.txt", path: "/workspace/a.txt", extension: "txt", size: 16, modifiedUnix: 1, isDirectory: false },
          { name: "hit_batch_002.txt", path: "/workspace/b.txt", extension: "txt", size: 17, modifiedUnix: 1, isDirectory: false },
          { name: "hit_batch_003.txt", path: "/workspace/c.txt", extension: "txt", size: 18, modifiedUnix: 1, isDirectory: false },
        ],
        total: 3,
        elapsedMs: 1,
      });
    }
    if (cmd === "index_status") return Promise.resolve({ ready: true, indexedCount: 98, metadataReady: true });
    if (cmd === "detect_legacy_v1_installation") return Promise.resolve(false);
    return Promise.resolve(null);
  },
}));

vi.mock("@tauri-apps/api/event", () => ({
  emit: () => Promise.resolve(),
  listen: () => Promise.resolve(() => {}),
}));

import FindXSearchApp from "../FindXSearchApp";

function setInput(el: HTMLInputElement, value: string) {
  const proto = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
  proto?.set?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

function key(target: HTMLElement, type: string, keyName: string, extra: KeyboardEventInit = {}) {
  const event = new KeyboardEvent(type, { key: keyName, bubbles: true, cancelable: true, ...extra });
  target.dispatchEvent(event);
  return event;
}

describe("主窗口搜索框方向键", () => {
  let root: Root | null = null;
  let host: HTMLDivElement | null = null;

  beforeEach(() => {
    resetDiagForTests();
    localStorage.clear();
    window.matchMedia = () =>
      ({
        matches: false,
        addEventListener() {},
        removeEventListener() {},
      }) as unknown as MediaQueryList;
    if (!("ResizeObserver" in window)) {
      class Observer {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
      (window as unknown as { ResizeObserver: typeof Observer }).ResizeObserver = Observer;
    }
  });

  afterEach(() => {
    act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  it("真实搜索框连按两次向下，选中行从第一条到第二条", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    await act(async () => {
      root?.render(<FindXSearchApp />);
    });
    const search = host.querySelector(".fx-search-input") as HTMLInputElement;
    await act(async () => {
      setInput(search, "hit_batch");
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 150));
    });
    expect(host.textContent).toContain("hit_batch_001.txt");
    expect(host.querySelector("tr.selected")).toBeNull();

    search.focus();
    await act(async () => {
      key(search, "keydown", "ArrowDown");
    });
    expect(host.querySelector("tr.selected")?.textContent).toContain("hit_batch_001.txt");
    await act(async () => {
      key(search, "keydown", "ArrowDown");
    });
    expect(host.querySelector("tr.selected")?.textContent).toContain("hit_batch_002.txt");
    const moves = recentKeyTraces().filter((item) => item.decision === "move");
    expect(moves.map((item) => item.selectedAfter)).toEqual([0, 1]);
    expect(moves[1]?.target).toContain("INPUT");
    expect(moves[1]?.isComposing).toBe(false);

    const sidebar = document.createElement("input");
    sidebar.className = "fx-filter-input";
    host.appendChild(sidebar);
    sidebar.focus();
    await act(async () => {
      key(sidebar, "keydown", "ArrowDown");
    });
    expect(host.querySelector("tr.selected")?.textContent).toContain("hit_batch_002.txt");

    search.focus();
    const composing = new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true });
    Object.defineProperty(composing, "isComposing", { get: () => true });
    await act(async () => {
      search.dispatchEvent(composing);
    });
    expect(host.querySelector("tr.selected")?.textContent).toContain("hit_batch_002.txt");
    expect(recentKeyTraces().some((item) => item.decision === "skip-composing")).toBe(true);

    await act(async () => {
      key(search, "keyup", "ArrowDown");
    });
    expect(host.querySelector("tr.selected")?.textContent).toContain("hit_batch_003.txt");
    expect(recentKeyTraces().some((item) => item.decision === "move-from-keyup")).toBe(true);
  });
});
