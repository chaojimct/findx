import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
// @ts-expect-error node builtin is available to vitest, not to the webview tsconfig
import { readFileSync } from "node:fs";
// @ts-expect-error node builtin is available to vitest, not to the webview tsconfig
import { dirname, resolve } from "node:path";
// @ts-expect-error node builtin is available to vitest, not to the webview tsconfig
import { fileURLToPath } from "node:url";
import { UpdateLayoutFixture } from "../dev/UpdateLayoutFixture";
import { IndexRangeList } from "../IndexRangeList";
import { indexOpFeedback } from "../indexOps";
import { partitionIndexRanges } from "../indexRange";
import { isWatchDegraded, summarizeIndexStatus } from "../indexStatus";
import { previewWhitespace } from "../previewText";
import {
  COL_LABEL,
  COLUMN_HEADER_MIN_PX,
  columnPixelWidths,
  NAME_COLUMN_MIN_PX,
  resolvePanelOpen,
  explainPanels,
  resolvePanels,
  visibleColumnIds,
} from "../resultTableLayout";
import { moveResultSelection, scrollTopForRow } from "../resultSelection";
import { placeBoundedTip } from "../tipPlacement";
import { deriveUpdatePhase } from "../UpdateCard";

const here = dirname(fileURLToPath(import.meta.url));

function readSrc(rel: string): string {
  return readFileSync(resolve(here, rel), "utf8");
}

describe("截图里的控件和主题", () => {
  it("间隔框在列布局里仍是短的单行", () => {
    const style = document.createElement("style");
    style.textContent = [
      readSrc("../design-tokens.css"),
      readSrc("../components/ui-components.css"),
      readSrc("../settings-window.css"),
    ].join("\n");
    document.head.appendChild(style);
    const host = document.createElement("div");
    host.className = "fx-settings-row-container";
    host.style.width = "640px";
    const input = document.createElement("input");
    input.className = "fx-input fx-input-narrow";
    input.value = "30";
    host.appendChild(input);
    document.body.appendChild(host);
    const cs = getComputedStyle(input);
    expect(parseFloat(cs.height)).toBeLessThanOrEqual(32);
    expect(parseFloat(cs.maxHeight)).toBeLessThanOrEqual(32);
    expect(parseFloat(cs.width)).toBeGreaterThan(40);
    expect(parseFloat(cs.width)).toBeLessThan(200);
    host.remove();
    style.remove();
  });

  it("窄输入不再用 flex-basis 充当高度", () => {
    const ui = readSrc("../components/ui-components.css");
    const settings = readSrc("../settings-window.css");
    const narrow = settings.match(/\.fx-input-narrow\s*\{[^}]*\}/g) ?? [];
    expect(narrow.length).toBeGreaterThan(0);
    for (const block of narrow) expect(block).not.toMatch(/flex\s*:/);
    expect(ui).toMatch(/\.fx-input\s*\{[^}]*flex:\s*0\s+0\s+auto/s);
    expect(ui).toMatch(/max-height:\s*var\(--fx-control-md\)/);
    expect(ui).toMatch(/\.fx-input\.fx-input-narrow\s*\{[^}]*width:\s*8\.5rem/s);
  });

  it("深色令牌、首屏背景和主题同步都是分层深灰", () => {
    const tokens = readSrc("../design-tokens.css");
    const dark = tokens.split('[data-fx-theme="dark"]')[1] ?? "";
    expect(dark).toContain("--fx-bg: #1b1b1b");
    expect(dark).toContain("--fx-surface: #242424");
    expect(dark).toContain("--fx-surface-raised: #2c2c2c");
    expect(dark).not.toMatch(/--fx-(bg|surface):\s*#000000/);
    const html = readSrc("../../../index.html");
    expect(html).toContain("#1b1b1b");
    expect(html).not.toContain("#000000");
    expect(readSrc("../themeColors.ts")).toContain('"#1b1b1b"');
    expect(readSrc("../SettingsWindow.tsx")).not.toContain("全黑");
    expect(readSrc("../FindXSearchApp.tsx")).not.toContain("#000000");
    const findx = readSrc("../findx.css");
    expect(findx).not.toContain("#0d0d0d");
    const header = findx.match(/\.fx-th-btn span \{[^}]*\}/)?.[0] ?? "";
    expect(header).not.toContain("ellipsis");
  });
});

describe("索引范围仍使用原始路径", () => {
  const paths = ["/workspace/data", "/sys/kernel/debug", "/", "/dev/mqueue", "C:"];

  it("系统挂载折叠分组，勾选值不改写", () => {
    const { general, system } = partitionIndexRanges(paths);
    expect(general.map((item) => item.value)).toEqual(["/workspace/data", "/", "C:"]);
    expect(system.map((item) => item.value)).toEqual(["/sys/kernel/debug", "/dev/mqueue"]);
    expect(general.find((item) => item.value === "/")?.title).toBe("根目录");
    expect(general.find((item) => item.value === "C:")?.title).toBe("磁盘 C:");
    expect(system[0]?.detail).toBe("/sys/kernel/debug");
    expect(paths).toEqual(["/workspace/data", "/sys/kernel/debug", "/", "/dev/mqueue", "C:"]);
  });
});

describe("底栏短摘要", () => {
  const degraded = "无 CAP_SYS_ADMIN，增量已降级为 inotify（非整盘实时；重启可能漏事件）";

  it("降级监视只在详情里保留技术句", () => {
    expect(isWatchDegraded(degraded)).toBe(true);
    const view = summarizeIndexStatus({ indexedCount: 98, lastError: degraded });
    expect(view.summary).toBe("索引 98 项 · 实时监控已降级");
    expect(view.summary).not.toContain("CAP_SYS_ADMIN");
    expect(view.summary).not.toContain("inotify");
    expect(view.detail).toBe(degraded);
  });

  it("其它错误用完整短句，不把原文截进底栏", () => {
    const view = summarizeIndexStatus({
      indexedCount: 3,
      lastError: "inotify 队列溢出，已对监听根做追赶扫描（仍非 fanotify 整盘实时）",
    });
    expect(view.summary).toBe("索引 3 项 · 有警告");
    expect(view.detail).toContain("队列溢出");
  });

  it("建库时长说明进详情", () => {
    const view = summarizeIndexStatus({
      indexing: true,
      indexingPhase: "scanning",
      indexingMessage: "正在读取一个很长的卷路径 /workspace/findx-gui-remodel-verify/runtime/data",
      indexedCount: 10,
    });
    expect(view.summary).toBe("索引 · 建库中 · 扫描");
    expect(view.detail).toContain("很长的卷路径");
    expect(view.summary).not.toContain("很长的卷路径");
  });
});

describe("窄表列和面板", () => {
  const saved = [0.28, 0.38, 0.1, 0.24] as [number, number, number, number];

  it("放不下表头就隐藏低优先列，可见列不窄于表头", () => {
    expect(visibleColumnIds(180)).toEqual(["name"]);
    expect(visibleColumnIds(120)).toEqual(["name"]);
    expect(NAME_COLUMN_MIN_PX).toBeGreaterThanOrEqual(160);
    expect(visibleColumnIds(646)).toContain("size");
    const shown = visibleColumnIds(220);
    const px = columnPixelWidths(saved, 220, shown);
    let sum = 0;
    for (const id of shown) {
      expect(px[id]).toBeGreaterThanOrEqual(COLUMN_HEADER_MIN_PX[id]);
      expect(COL_LABEL[id].length).toBeGreaterThan(1);
      sum += px[id];
    }
    expect(sum).toBeLessThanOrEqual(220 + 0.01);
    expect(COL_LABEL.size).toBe("大小");
  });

  it("647 宽且没有本次手动打开时，两块面板都收起", () => {
    expect(
      resolvePanels({
        contentWidth: 647,
        previewWidth: 360,
        savedSidebar: true,
        savedPreview: true,
        sidebarForce: null,
        previewForce: null,
      }),
    ).toEqual({ sidebar: false, preview: false });
    expect(
      resolvePanels({
        contentWidth: 647,
        previewWidth: 360,
        savedSidebar: true,
        savedPreview: true,
        sidebarForce: "show",
        previewForce: "show",
      }),
    ).toEqual({ sidebar: true, preview: true });
  });

  it("诊断理由和折叠结果一致，阈值没有被放大", () => {
    const widths = [437, 647, 800, 959, 1200];
    for (const contentWidth of widths) {
      const args = {
        contentWidth,
        previewWidth: 360,
        savedSidebar: true,
        savedPreview: true,
        sidebarForce: null,
        previewForce: null,
      } as const;
      const explained = explainPanels(args);
      expect({ sidebar: explained.sidebar, preview: explained.preview }).toEqual(resolvePanels(args));
    }
    const narrow = explainPanels({
      contentWidth: 647,
      previewWidth: 360,
      savedSidebar: true,
      savedPreview: true,
      sidebarForce: null,
      previewForce: null,
    });
    expect(narrow.previewReason).toContain("647px < 900px");
    expect(narrow.sidebarReason).toContain("647px < 760px");
    const wide = explainPanels({
      contentWidth: 1200,
      previewWidth: 360,
      savedSidebar: true,
      savedPreview: true,
      sidebarForce: null,
      previewForce: null,
    });
    expect(wide.preview).toBe(true);
    expect(wide.sidebar).toBe(true);
    expect(wide.previewReason).toContain("1200px >= 900px");
  });

  it("窄窗口自动收起面板，手动打开可以盖过自动策略", () => {
    expect(
      resolvePanelOpen({ windowWidth: 646, savedOpen: true, forced: null, kind: "preview" }),
    ).toBe(false);
    expect(
      resolvePanelOpen({ windowWidth: 646, savedOpen: true, forced: null, kind: "sidebar" }),
    ).toBe(false);
    expect(
      resolvePanelOpen({ windowWidth: 646, savedOpen: true, forced: "show", kind: "preview" }),
    ).toBe(true);
    expect(
      resolvePanelOpen({ windowWidth: 1200, savedOpen: false, forced: null, kind: "sidebar" }),
    ).toBe(false);
  });
});

describe("预览、提示和结果方向键", () => {
  it("普通文本换行，代码保留横滚", () => {
    expect(previewWhitespace("txt")).toBe("prose");
    expect(previewWhitespace("md")).toBe("prose");
    expect(previewWhitespace("TSX")).toBe("code");
    expect(previewWhitespace("rs")).toBe("code");
  });

  it("长路径提示留在视口内且高度有限", () => {
    const box = placeBoundedTip({ top: 400, left: 500, bottom: 420 }, { width: 640, height: 436 });
    expect(box.maxWidth).toBeLessThanOrEqual(420);
    expect(box.maxHeight).toBeLessThanOrEqual(Math.floor(436 * 0.34));
    expect(box.left).toBeGreaterThanOrEqual(8);
    expect(box.left + box.maxWidth).toBeLessThanOrEqual(640);
    expect(box.top).toBeGreaterThanOrEqual(8);
    expect(box.top + box.maxHeight).toBeLessThanOrEqual(436);
  });

  it("方向键只移动选中行", () => {
    expect(moveResultSelection(null, "ArrowDown", 3)).toBe(0);
    expect(moveResultSelection(0, "ArrowDown", 3)).toBe(1);
    expect(moveResultSelection(2, "ArrowDown", 3)).toBe(2);
    expect(moveResultSelection(null, "ArrowUp", 4)).toBe(3);
    expect(scrollTopForRow(10, 32, 0, 100)).toBe(10 * 32 + 32 - 100);
  });
});

describe("重建和压缩反馈", () => {
  it("不编造百分比，也不把命令返回说成索引已完成", () => {
    expect(indexOpFeedback("rebuild", "submitted")).toContain("不代表索引已经重建完成");
    expect(indexOpFeedback("rebuild", "submitted")).not.toMatch(/%/);
    expect(indexOpFeedback("compact", "working")).not.toMatch(/%/);
    expect(indexOpFeedback("compact", "failed", "denied")).toContain("denied");
  });
});

describe("更新阶段", () => {
  it("由真实字段区分检查、下载、失败和已是最新", () => {
    expect(deriveUpdatePhase({ busy: true, progress: null, info: null, downloadedPath: null })).toBe(
      "checking",
    );
    expect(
      deriveUpdatePhase({
        busy: true,
        progress: { downloaded: 1, total: 4, percent: 25 },
        info: null,
        downloadedPath: null,
      }),
    ).toBe("downloading");
    expect(
      deriveUpdatePhase({
        busy: false,
        progress: null,
        info: { ok: false, hasUpdate: false, currentVersion: "2.4.10", error: "nope" },
        downloadedPath: null,
      }),
    ).toBe("error");
    expect(
      deriveUpdatePhase({
        busy: false,
        progress: null,
        info: { ok: true, hasUpdate: false, currentVersion: "2.4.10" },
        downloadedPath: null,
      }),
    ).toBe("current");
  });
});

describe("jsdom：更新夹具只验证结构，不测量像素，也不是原生窗口", () => {
  let root: Root | null = null;
  let host: HTMLDivElement | null = null;

  afterEach(() => {
    vi.useRealTimers();
    act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  it("长说明留在滚动区，顶栏和固定底栏不承接正文", async () => {
    const fixture = readSrc("../dev/UpdateLayoutFixture.tsx");
    expect(fixture).toContain("模拟");
    expect(fixture).not.toContain("invoke(");
    expect(fixture).not.toContain("install_downloaded_update");
    expect(readSrc("../../App.tsx")).toContain("import.meta.env.DEV");
    const css = readSrc("../settings-window.css");
    expect(css).toMatch(/\.fx-settings-footer\s*\{[^}]*height:\s*56px/s);
    expect(css).toMatch(/\.fx-fixture-page\s*\{[^}]*height:\s*100%/s);
    expect(css).toMatch(/\.fx-fixture-shell\s*\{[^}]*min-height:\s*0/s);
    expect(css).not.toMatch(/\.fx-fixture-shell\s*\{[^}]*height:\s*480px/s);
    expect(css).toMatch(/\.fx-update-release-notes\s*\{[^}]*max-height:\s*240px/s);
    expect(css).toMatch(/\.fx-update-release-notes\s*\{[^}]*white-space:\s*pre-wrap/s);

    vi.useFakeTimers();
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    await act(async () => {
      root?.render(<UpdateLayoutFixture />);
    });

    expect(host.textContent).toContain("不会请求 GitHub");
    expect(host.textContent).toContain("生产包不含此页");
    expect(host.querySelector("script")).toBeNull();

    const clickPhase = async (label: string) => {
      const button = [...host!.querySelectorAll(".fx-fixture-switch button")].find(
        (item) => item.textContent === label,
      );
      await act(async () => {
        button?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
      });
    };
    const assertNotesInScroll = () => {
      const shell = host!.querySelector(".fx-settings-window-root");
      const header = shell?.querySelector(".fx-settings-header");
      const footer = shell?.querySelector(".fx-settings-footer");
      const scroll = shell?.querySelector(".fx-settings-content-scroll");
      const notes = shell?.querySelector(".fx-update-release-notes");
      expect(scroll?.contains(footer ?? null)).toBe(false);
      expect(header?.contains(notes ?? null)).toBe(false);
      expect(footer?.contains(notes ?? null)).toBe(false);
      expect(scroll?.contains(notes ?? null)).toBe(true);
      expect(footer?.textContent ?? "").not.toContain("一二三四五六七八九十");
    };

    expect(host.textContent).toContain("<script>alert(1)</script>");
    assertNotesInScroll();

    await clickPhase("检查中");
    expect(host.querySelector('[data-update-phase="checking"]')).toBeTruthy();
    expect(host.querySelector(".fx-settings-footer")?.textContent).toContain("检查中（模拟）");

    await clickPhase("下载中");
    expect(host.querySelector('[data-update-phase="downloading"]')).toBeTruthy();

    await clickPhase("失败");
    expect(host.querySelector('[data-update-phase="error"]')).toBeTruthy();
    expect(host.textContent).toContain("模拟失败：更新源没有响应");

    await clickPhase("已取消");
    expect(host.querySelector(".fx-settings-footer")?.textContent).toContain("已取消下载（模拟）");

    await clickPhase("有新版本");
    assertNotesInScroll();

    await clickPhase("交互模拟");
    const checkButtons = [...host.querySelectorAll("button")].filter((b) => b.textContent === "检查更新");
    await act(async () => {
      checkButtons[checkButtons.length - 1]?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(host.textContent).toContain("检查中（模拟慢响应）");
    await act(async () => {
      vi.advanceTimersByTime(1200);
    });
    expect(host.textContent).toContain("发现模拟版本");

    const downloadButtons = [...host.querySelectorAll("button")].filter((b) =>
      (b.textContent ?? "").startsWith("下载"),
    );
    await act(async () => {
      downloadButtons[downloadButtons.length - 1]?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(host.textContent).toContain("下载中（模拟）");
    const cancelButtons = [...host.querySelectorAll("button")].filter((b) => b.textContent === "取消下载");
    await act(async () => {
      cancelButtons[cancelButtons.length - 1]?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(host.textContent).toContain("已取消下载（模拟）");
  });

  it("范围列表默认折叠系统挂载，回调拿到原路径", async () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    const seen: string[][] = [];
    await act(async () => {
      root?.render(
        <IndexRangeList
          paths={["/workspace/data", "/sys/kernel/debug", "/dev/mqueue"]}
          selected={["/sys/kernel/debug"]}
          onChange={(next) => seen.push(next)}
        />,
      );
    });
    const details = host.querySelector("details");
    expect(details).toBeTruthy();
    expect((details as HTMLDetailsElement).open).toBe(false);
    expect(details?.querySelector("summary")?.textContent).toContain("系统挂载");
    expect(details?.textContent).toContain("/sys/kernel/debug");
    expect(host.textContent).toContain("data");
    const boxes = [...host.querySelectorAll("input[type=checkbox]")];
    await act(async () => {
      boxes[0]?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(seen[seen.length - 1]).toEqual(["/sys/kernel/debug", "/workspace/data"]);
  });
});
