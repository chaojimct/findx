import { describe, expect, it } from "vitest";
// 测试跑在 Vitest 的 Node 环境。应用 tsconfig 不带 @types/node，这里只给这一行放行。
// @ts-expect-error node builtin is available to vitest, not to the webview tsconfig
import { readFileSync } from "node:fs";
// @ts-expect-error node builtin is available to vitest, not to the webview tsconfig
import { dirname, resolve } from "node:path";
// @ts-expect-error node builtin is available to vitest, not to the webview tsconfig
import { fileURLToPath } from "node:url";
import type { FindxGuiSettings } from "../findxGuiTypes";
import { createOnceListener } from "../onceListener";
import {
  applyColumnDrag,
  layoutColumnFractions,
  RESULT_ROW_HEIGHT_PX,
} from "../resultTableLayout";
import {
  CLOSE_DIRTY_MESSAGE,
  closeDecision,
  commitSaveIntervalSecs,
  commitSearchLimit,
  discardUnsaved,
  SEARCH_LIMIT_U32_MAX,
  serviceStartHint,
} from "../settingsDraft";
import { settleUpdate, UpdateRequestGate } from "../updateGate";

function sampleSettings(searchLimit = 5000): FindxGuiSettings {
  return {
    indexPath: "index.bin",
    volume: "C:",
    pipeName: "findx2",
    pinyinDefault: true,
    serviceExePath: "",
    searchLimit,
    autoStartService: true,
    firstIndexFullMetadata: false,
    runMode: "service",
    drives: ["C:"],
    excludedDirs: ["/tmp"],
    enableMetadataBackfill: true,
    enableEverythingIpc: true,
    saveIntervalSecs: 30,
    autoStartApp: false,
    autoCheckUpdate: true,
  };
}

describe("UpdateRequestGate（设置页检查/下载使用的同一类型）", () => {
  it("进行中时拒绝第二个请求，且不把第一个标成过期", () => {
    const gate = new UpdateRequestGate();
    const first = gate.tryBegin();
    const second = gate.tryBegin();
    expect(first).toBe(1);
    expect(second).toBeNull();
    expect(gate.isCurrent(first!)).toBe(true);
    expect(gate.busy).toBe(true);
  });

  it("过期 id 不能写入结果，也不能清掉当前请求", () => {
    const gate = new UpdateRequestGate();
    const first = gate.tryBegin()!;
    gate.invalidate();
    const applied: string[] = [];
    expect(settleUpdate(gate, first, "stale", (v) => applied.push(v))).toBe(false);
    expect(applied).toEqual([]);
    expect(gate.finish(first)).toBe(false);

    const third = gate.tryBegin()!;
    expect(settleUpdate(gate, third, "fresh", (v) => applied.push(v))).toBe(true);
    expect(applied).toEqual(["fresh"]);
    expect(gate.finish(third)).toBe(true);
    expect(gate.busy).toBe(false);
    expect(gate.finish(third)).toBe(false);
  });
});

describe("设置草稿与关闭", () => {
  it("结果上限允许编辑中间空值，保存时才拒绝；不把 100000 当成非法", () => {
    expect(commitSearchLimit("").ok).toBe(false);
    expect(commitSearchLimit("12a").ok).toBe(false);
    expect(commitSearchLimit("0").ok).toBe(false);
    const big = commitSearchLimit("100000");
    expect(big).toEqual({ ok: true, value: 100000 });
    expect(100000).toBeLessThan(SEARCH_LIMIT_U32_MAX);
    expect(commitSearchLimit(String(SEARCH_LIMIT_U32_MAX))).toEqual({
      ok: true,
      value: SEARCH_LIMIT_U32_MAX,
    });
    expect(commitSearchLimit(String(SEARCH_LIMIT_U32_MAX + 1)).ok).toBe(false);
  });

  it("落盘间隔保存时校验，不在输入过程中夹默认值", () => {
    expect(commitSaveIntervalSecs("").ok).toBe(false);
    expect(commitSaveIntervalSecs("0").ok).toBe(false);
    expect(commitSaveIntervalSecs("15")).toEqual({ ok: true, value: 15 });
  });

  it("取消未保存时连主题预览一起还原", () => {
    const current = sampleSettings(8000);
    const original = sampleSettings(5000);
    expect(closeDecision(true, false)).toBe("stay");
    expect(closeDecision(true, true)).toBe("hide");
    expect(closeDecision(false, false)).toBe("hide");
    expect(CLOSE_DIRTY_MESSAGE.length).toBeGreaterThan(0);
    expect(
      discardUnsaved({
        settings: current,
        original,
        theme: "dark",
        originalTheme: "light",
      }),
    ).toEqual({ settings: original, theme: "light" });
  });

  it("启动命令成功的文案不宣称索引已就绪", () => {
    const hint = serviceStartHint("sent");
    expect(hint).toContain("已发送启动命令");
    expect(hint).toContain("不代表索引可用");
    expect(hint).not.toContain("已就绪");
    expect(serviceStartHint("already")).toContain("已在运行");
    expect(serviceStartHint("failed", "denied")).toContain("denied");
  });
});

describe("关闭监听只挂一次", () => {
  it("后完成的旧订阅会被立即退掉，dispose 会退掉当前监听", async () => {
    const listener = createOnceListener();
    let firstTorn = false;
    let releaseFirst!: (un: () => void) => void;
    const first = listener.arm(
      () =>
        new Promise((resolve) => {
          releaseFirst = resolve;
        }),
    );
    let secondTorn = false;
    await listener.arm(async () => {
      return () => {
        secondTorn = true;
      };
    });
    expect(listener.installed).toBe(true);
    releaseFirst(() => {
      firstTorn = true;
    });
    await first;
    expect(firstTorn).toBe(true);
    expect(secondTorn).toBe(false);
    expect(listener.installed).toBe(true);

    listener.dispose();
    expect(secondTorn).toBe(true);
    expect(listener.installed).toBe(false);
  });

  it("dispose 之后才返回的订阅不会留下监听", async () => {
    const listener = createOnceListener();
    let torn = false;
    let release!: (un: () => void) => void;
    const pending = listener.arm(
      () =>
        new Promise((resolve) => {
          release = resolve;
        }),
    );
    listener.dispose();
    release(() => {
      torn = true;
    });
    await pending;
    expect(torn).toBe(true);
    expect(listener.installed).toBe(false);
  });
});

describe("结果表布局", () => {
  const saved: [number, number, number, number] = [0.1, 0.7, 0.1, 0.1];

  it("窄表把空间分给名称列，不改排序语义", () => {
    const laid = layoutColumnFractions(saved, 280);
    expect(laid[0]).toBeGreaterThan(laid[1]);
    expect(laid[0] * 280).toBeGreaterThan(150);
    expect(laid.reduce((a, b) => a + b, 0)).toBeCloseTo(1, 5);
  });

  it("宽表沿用已保存比例", () => {
    const wide: [number, number, number, number] = [0.28, 0.38, 0.1, 0.24];
    const laid = layoutColumnFractions(wide, 900);
    expect(laid[0]).toBeCloseTo(0.28, 2);
    expect(laid[1]).toBeCloseTo(0.38, 2);
  });

  it("拖动不能把名称列压到下限以下", () => {
    const wide: [number, number, number, number] = [0.28, 0.38, 0.1, 0.24];
    const dragged = applyColumnDrag(wide, 0, -500, 800);
    expect(dragged[0] * 800).toBeGreaterThanOrEqual(160);
  });

  it("虚拟行高与 design-tokens.css 的 --fx-row-height 相同", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const css = readFileSync(resolve(here, "../design-tokens.css"), "utf8");
    const match = css.match(/--fx-row-height:\s*(\d+)px/);
    expect(match?.[1]).toBe(String(RESULT_ROW_HEIGHT_PX));
  });
});
