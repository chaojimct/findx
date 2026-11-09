import { useRef, useState, type ReactNode } from "react";
import "../design-tokens.css";
import "../settings-window.css";
import type { AppUpdateInfo, UpdateProgressEvent } from "../findxGuiTypes";
import { UpdateCard } from "../UpdateCard";

/**
 * 开发/测试布局夹具。
 * 只在 import.meta.env.DEV 且查询参数 fxFixture=update 时由 App 挂上。
 * 状态是写死的模拟：不调用更新命令，不下载，不安装，不改生产更新来源。
 * 发行说明是纯文本。
 */

export const FIXTURE_BANNER =
  "模拟数据。不是 GitHub 更新，不会请求 GitHub，不会下载或安装。生产包不含此页。";

export type FixturePhase = "checking" | "available" | "downloading" | "error" | "cancelled" | "live";

const PHASES: { id: FixturePhase; label: string }[] = [
  { id: "checking", label: "检查中" },
  { id: "available", label: "有新版本" },
  { id: "downloading", label: "下载中" },
  { id: "error", label: "失败" },
  { id: "cancelled", label: "已取消" },
  { id: "live", label: "交互模拟" },
];

const LONG_NOTES = [
  "修复了设置页在窄窗口里底栏被内容顶走的问题。保存和关闭留在固定底栏。",
  "",
  "索引服务在没有额外权限时会退回到目录监听。完整原因写在主窗口的状态详情里，底栏只保留短句。",
  "",
  "这一段是多段纯文本说明，用来确认发行说明留在内容滚动区，不会把标题栏或保存按钮挤出窗口。",
  "一二三四五六七八九十".repeat(18),
  "原文里的 <script>alert(1)</script> 必须按文字显示，不能执行。",
].join("\n");

const AVAILABLE: AppUpdateInfo = {
  ok: true,
  hasUpdate: true,
  currentVersion: "2.4.10",
  latestVersion: "2.4.11-fixture",
  downloadUrl: "https://example.invalid/findx-fixture",
  assetSize: 12 * 1024 * 1024,
  releaseNotes: LONG_NOTES,
};

const FAILED: AppUpdateInfo = {
  ok: false,
  hasUpdate: false,
  currentVersion: "2.4.10",
  error: "模拟失败：更新源没有响应。这不是一次真实的检查。",
};

function Shell({
  hint,
  children,
}: {
  hint: string;
  children: ReactNode;
}) {
  return (
    <section className="fx-fixture-shell">
      <div className="fx-settings-window-root">
        <div className="fx-settings-window-container">
          <div className="fx-settings-header">
            <h2 className="fx-settings-title">FindX2 设置</h2>
            <div className="fx-settings-tabs" role="tablist">
              <button type="button" className="fx-settings-tab" role="tab" aria-selected="true">
                高级
              </button>
            </div>
          </div>
          <div className="fx-settings-content-scroll">
            <div>{children}</div>
          </div>
          <div className="fx-settings-footer">
            <p className="fx-settings-footer-hint">{hint}</p>
            <div className="fx-settings-footer-actions">
              <button type="button" className="fx-btn fx-btn--primary fx-btn--md">
                保存
              </button>
              <button type="button" className="fx-btn fx-btn--md">
                关闭
              </button>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}

const noop = () => {};

export function LiveUpdateSim() {
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<UpdateProgressEvent | null>(null);
  const [info, setInfo] = useState<AppUpdateInfo | null>(null);
  const [downloadedPath, setDownloadedPath] = useState<string | null>(null);
  const [hint, setHint] = useState("尚未开始（模拟）");
  const timer = useRef<number | null>(null);

  const clearTimer = () => {
    if (timer.current != null) {
      window.clearTimeout(timer.current);
      timer.current = null;
    }
  };

  const check = () => {
    clearTimer();
    setBusy(true);
    setProgress(null);
    setDownloadedPath(null);
    setInfo(null);
    setHint("检查中（模拟慢响应）");
    timer.current = window.setTimeout(() => {
      setBusy(false);
      setInfo(AVAILABLE);
      setHint("发现模拟版本。说明在上方滚动区。");
    }, 1200);
  };

  const download = () => {
    clearTimer();
    setBusy(true);
    setProgress({ downloaded: 4 * 1024 * 1024, total: 12 * 1024 * 1024, percent: 33.3 });
    setHint("下载中（模拟）");
  };

  const cancel = () => {
    clearTimer();
    setBusy(false);
    setProgress(null);
    setHint("已取消下载（模拟）");
  };

  const fail = () => {
    clearTimer();
    setBusy(false);
    setProgress(null);
    setDownloadedPath(null);
    setInfo(FAILED);
    setHint("检查失败（模拟）");
  };

  return (
    <Shell hint={hint}>
      <p className="fx-hint">这一块可以切换检查、慢响应、失败和取消。按钮不会安装任何文件。</p>
      <div className="fx-update-card-actions">
        <button type="button" onClick={fail}>
          模拟失败
        </button>
      </div>
      <UpdateCard
        busy={busy}
        info={info}
        progress={progress}
        downloadedPath={downloadedPath}
        autoCheck
        onAutoCheck={noop}
        onCheck={check}
        onDownload={download}
        onCancel={cancel}
        onInstall={() => setHint("夹具不会安装更新")}
      />
    </Shell>
  );
}

function cardFor(phase: FixturePhase) {
  if (phase === "checking") {
    return { busy: true, info: null, progress: null, downloadedPath: null, hint: "检查中（模拟）" };
  }
  if (phase === "downloading") {
    return {
      busy: true,
      info: AVAILABLE,
      progress: { downloaded: 5 * 1024 * 1024, total: 12 * 1024 * 1024, percent: 41.7 },
      downloadedPath: null,
      hint: "下载中（模拟）",
    };
  }
  if (phase === "error") {
    return { busy: false, info: FAILED, progress: null, downloadedPath: null, hint: "检查失败（模拟）" };
  }
  if (phase === "cancelled") {
    return { busy: false, info: AVAILABLE, progress: null, downloadedPath: null, hint: "已取消下载（模拟）" };
  }
  return {
    busy: false,
    info: AVAILABLE,
    progress: null,
    downloadedPath: null,
    hint: "发现模拟版本。说明在上方滚动区。",
  };
}

function phaseFromLocation(): FixturePhase {
  if (typeof window === "undefined") return "available";
  const value = new URLSearchParams(window.location.search).get("phase");
  return PHASES.some((item) => item.id === value) ? (value as FixturePhase) : "available";
}

export function UpdateLayoutFixture() {
  const [phase, setPhase] = useState<FixturePhase>(phaseFromLocation);
  const card = cardFor(phase);
  return (
    <div className="fx-fixture-page">
      <p className="fx-fixture-banner">{FIXTURE_BANNER}</p>
      <div className="fx-fixture-switch" role="tablist" aria-label="模拟更新状态">
        {PHASES.map((item) => (
          <button
            key={item.id}
            type="button"
            role="tab"
            aria-selected={phase === item.id}
            className={phase === item.id ? "fx-fixture-switch-on" : ""}
            onClick={() => setPhase(item.id)}
          >
            {item.label}
          </button>
        ))}
      </div>
      {phase === "live" ? (
        <LiveUpdateSim />
      ) : (
        <Shell hint={card.hint}>
          <UpdateCard
            busy={card.busy}
            info={card.info}
            progress={card.progress}
            downloadedPath={card.downloadedPath}
            autoCheck
            onAutoCheck={noop}
            onCheck={noop}
            onDownload={noop}
            onCancel={noop}
            onInstall={() => {}}
          />
        </Shell>
      )}
    </div>
  );
}
