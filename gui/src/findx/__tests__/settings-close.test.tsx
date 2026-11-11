import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { FindxGuiSettings } from "../findxGuiTypes";

const loaded: FindxGuiSettings = {
  indexPath: "index.bin",
  volume: "C:",
  pipeName: "findx2",
  pinyinDefault: true,
  serviceExePath: "",
  searchLimit: 5000,
  autoStartService: true,
  firstIndexFullMetadata: false,
  runMode: "service",
  drives: [],
  excludedDirs: [],
  enableMetadataBackfill: true,
  enableEverythingIpc: true,
  saveIntervalSecs: 30,
  autoStartApp: false,
  autoCheckUpdate: true,
};

const gate = vi.hoisted(() => ({
  hides: 0,
  saves: 0,
  saveError: false,
  loads: 0,
  closeHandlers: [] as Array<(event: { preventDefault: () => void }) => void>,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => {
    if (cmd === "load_findx_settings") {
      gate.loads += 1;
      return Promise.resolve(loaded);
    }
    if (cmd === "hide_settings_window") {
      gate.hides += 1;
      return Promise.resolve();
    }
    if (cmd === "save_findx_settings") {
      gate.saves += 1;
      return gate.saveError ? Promise.reject(new Error("disk")) : Promise.resolve();
    }
    if (cmd === "get_autostart_state") return Promise.resolve({ enabled: false, supported: false });
    if (cmd === "index_status") return Promise.resolve({ ready: false });
    return Promise.resolve(null);
  },
}));

vi.mock("@tauri-apps/api/event", () => ({
  emit: () => Promise.resolve(),
  listen: () => Promise.resolve(() => {}),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onCloseRequested: (fn: (event: { preventDefault: () => void }) => void) => {
      gate.closeHandlers.push(fn);
      return Promise.resolve(() => {
        const index = gate.closeHandlers.indexOf(fn);
        if (index >= 0) gate.closeHandlers.splice(index, 1);
      });
    },
    setTheme: () => Promise.resolve(),
    hide: () => Promise.resolve(),
    label: "settings",
  }),
}));

import SettingsWindow from "../SettingsWindow";

function setInput(el: HTMLInputElement, value: string) {
  const proto = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value");
  proto?.set?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

describe("设置窗口真正的关闭处理", () => {
  let root: Root | null = null;
  let host: HTMLDivElement | null = null;
  let confirms = 0;
  let confirmYes = false;

  beforeEach(() => {
    gate.hides = 0;
    gate.saves = 0;
    gate.saveError = false;
    gate.loads = 0;
    gate.closeHandlers = [];
    confirms = 0;
    confirmYes = false;
    window.matchMedia = () =>
      ({
        matches: false,
        addEventListener() {},
        removeEventListener() {},
      }) as unknown as MediaQueryList;
    window.confirm = () => {
      confirms += 1;
      return confirmYes;
    };
  });

  afterEach(() => {
    act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  async function flush() {
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
  }

  it("数字草稿会拦住系统关闭，取消保留，确认还原，保存改基线，失败不丢草稿", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    await act(async () => {
      root?.render(<SettingsWindow />);
    });
    await flush();
    expect(gate.loads).toBeGreaterThan(0);
    expect(gate.closeHandlers).toHaveLength(1);
    expect(host.querySelector(".fx-settings-dirty-indicator")).toBeNull();

    const searchTab = [...host.querySelectorAll('[role="tab"]')].find((el) => el.textContent === "搜索");
    await act(async () => {
      searchTab?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    const limit = host.querySelector("input.fx-input-narrow") as HTMLInputElement;
    expect(limit.value).toBe("5000");
    await act(async () => {
      setInput(limit, "5001");
    });
    expect(limit.value).toBe("5001");
    expect(host.querySelector(".fx-settings-dirty-indicator")).toBeTruthy();

    confirmYes = false;
    await act(async () => {
      gate.closeHandlers[0]?.({ preventDefault() {} });
    });
    expect(confirms).toBe(1);
    expect(gate.hides).toBe(0);
    expect(limit.value).toBe("5001");

    confirmYes = true;
    await act(async () => {
      gate.closeHandlers[0]?.({ preventDefault() {} });
    });
    expect(confirms).toBe(2);
    expect(gate.hides).toBe(1);
    expect(gate.closeHandlers).toHaveLength(1);
    expect((host.querySelector("input.fx-input-narrow") as HTMLInputElement).value).toBe("5000");
    expect(host.querySelector(".fx-settings-dirty-indicator")).toBeNull();

    const again = host.querySelector("input.fx-input-narrow") as HTMLInputElement;
    await act(async () => {
      setInput(again, "5001");
    });
    confirmYes = false;
    await act(async () => {
      gate.closeHandlers[0]?.({ preventDefault() {} });
    });
    expect(gate.hides).toBe(1);
    expect(again.value).toBe("5001");

    gate.saveError = true;
    const save = [...host.querySelectorAll("button")].find((el) => el.textContent === "保存");
    await act(async () => {
      save?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();
    expect(host.textContent).toContain("保存失败");
    expect(again.value).toBe("5001");
    expect(host.querySelector(".fx-settings-dirty-indicator")).toBeTruthy();

    gate.saveError = false;
    await act(async () => {
      save?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();
    expect(host.textContent).toContain("设置已保存");
    expect(host.querySelector(".fx-settings-dirty-indicator")).toBeNull();
    const confirmsBeforeCleanClose = confirms;
    await act(async () => {
      gate.closeHandlers[0]?.({ preventDefault() {} });
    });
    expect(confirms).toBe(confirmsBeforeCleanClose);
    expect(gate.hides).toBe(2);

    await act(async () => {
      setInput(again, "");
    });
    expect(host.querySelector(".fx-settings-dirty-indicator")).toBeTruthy();
    await act(async () => {
      save?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(again.value).toBe("");
    expect(host.textContent).toContain("整数");
  });
});
