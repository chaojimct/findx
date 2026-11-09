import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import "./design-tokens.css";
import "./findx.css";
import "./settings-window.css";
import { Button, Checkbox, Input, RadioGroup, SettingsRow } from "./components/UIComponents";
import { ExcludeDirsEditor } from "./components/ExcludeDirsEditor";
import { IndexRangeList } from "./IndexRangeList";
import { indexOpFeedback } from "./indexOps";
import { windowThemeColors } from "./themeColors";
import { UpdateCard } from "./UpdateCard";
import { createOnceListener } from "./onceListener";
import {
  CLOSE_DIRTY_MESSAGE,
  closeDecision,
  commitSaveIntervalSecs,
  commitSearchLimit,
  discardUnsaved,
  formIsDirty,
  serviceStartHint,
} from "./settingsDraft";
import { settleUpdate, UpdateRequestGate } from "./updateGate";
import type {
  AppUpdateInfo,
  AutostartState,
  FindxGuiSettings,
  RunMode,
  UiThemePref,
  UpdateProgressEvent,
  UpdateFinishedEvent,
} from "./findxGuiTypes";
import { UI_THEME_KEY, loadUiThemePref } from "./findxGuiTypes";

/**
 * 独立「设置」窗口：与主窗口共用标题栏主题（sync_window_theme_command），不可最大化（见 tauri.conf）。
 */
export default function SettingsWindow() {
  const [settings, setSettings] = useState<FindxGuiSettings>({
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
  });
  const [originalSettings, setOriginalSettings] = useState<FindxGuiSettings | null>(null);
  const settingsRef = useRef(settings);
  const [settingsTab, setSettingsTab] = useState<"index" | "search" | "service" | "advanced">(
    "index",
  );
  const [rebuildBusy, setRebuildBusy] = useState(false);
  const [availableDrives, setAvailableDrives] = useState<string[]>([]);
  const [hostOs, setHostOs] = useState<string>("windows");
  const [uiThemePref, setUiThemePref] = useState<UiThemePref>(() => loadUiThemePref());
  const [originalUiThemePref, setOriginalUiThemePref] = useState<UiThemePref>(() => loadUiThemePref());
  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  const [hint, setHint] = useState("");
  const [showSyntaxHelp, setShowSyntaxHelp] = useState(false);
  const [serviceBusy, setServiceBusy] = useState(false);
  const [searchLimitInput, setSearchLimitInput] = useState(String(settings.searchLimit));
  const [saveIntervalInput, setSaveIntervalInput] = useState(String(settings.saveIntervalSecs ?? 30));
  const updateGateRef = useRef(new UpdateRequestGate());
  const updateOpIdRef = useRef(0);
  const isDirtyRef = useRef(false);
  const originalRef = useRef<FindxGuiSettings | null>(null);
  const themeRef = useRef(uiThemePref);
  const originalThemeRef = useRef(originalUiThemePref);
  /**
   * 「随系统启动」的真源是注册表，不是设置文件。
   * 因此单独存一份状态，进入设置页时从后端回读，避免显示与实际相反。
   */
  const [autostart, setAutostart] = useState<AutostartState>({
    enabled: false,
    supported: true,
  });
  const [autostartBusy, setAutostartBusy] = useState(false);
  /** 索引体检（墓碑统计）：进入设置页与压缩完成后各拉一次。null = 服务未上报/未运行 */
  const [idxEntries, setIdxEntries] = useState<number | null>(null);
  const [tombCount, setTombCount] = useState<number | null>(null);
  const [compactBusy, setCompactBusy] = useState(false);
  const [indexOp, setIndexOp] = useState("");
  /** 应用更新：检测结果；下载走事件（findx2-update-progress/finished）驱动进度条 */
  const [updInfo, setUpdInfo] = useState<AppUpdateInfo | null>(null);
  /** true = 检查中或下载中（下载中由 dlProgress 非空区分） */
  const [updBusy, setUpdBusy] = useState(false);
  const [dlProgress, setDlProgress] = useState<UpdateProgressEvent | null>(null);
  /** 下载完成后的安装包本地路径；非空时显示「安装更新并重启」 */
  const [downloadedPath, setDownloadedPath] = useState<string | null>(null);

  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const fn = () => setSystemDark(mq.matches);
    mq.addEventListener("change", fn);
    return () => mq.removeEventListener("change", fn);
  }, []);

  const dirty = formIsDirty({
    settings,
    original: originalSettings,
    theme: uiThemePref,
    originalTheme: originalUiThemePref,
    searchLimitText: searchLimitInput,
    saveIntervalText: saveIntervalInput,
  });
  isDirtyRef.current = dirty;
  originalRef.current = originalSettings;
  themeRef.current = uiThemePref;
  originalThemeRef.current = originalUiThemePref;

  /** 应用更新：订阅后端下载进度/结束事件（下载线程 emit，200ms 节流）。 */
  useEffect(() => {
    const unProgress = listen<UpdateProgressEvent>("findx2-update-progress", (e) => {
      if (!updateGateRef.current.isCurrent(updateOpIdRef.current)) return;
      setDlProgress(e.payload);
    });
    const unFinished = listen<UpdateFinishedEvent>("findx2-update-finished", (e) => {
      const id = updateOpIdRef.current;
      if (!updateGateRef.current.finish(id)) return;
      setDlProgress(null);
      setUpdBusy(false);
      if (e.payload.ok && e.payload.path) {
        setDownloadedPath(e.payload.path);
        setHint("下载完成。点击「安装更新并重启」开始安装（会弹一次管理员授权）。");
      } else {
        setDownloadedPath(null);
        setHint(e.payload.error ?? "下载失败");
      }
    });
    return () => {
      void unProgress.then((f) => f());
      void unFinished.then((f) => f());
    };
  }, []);

  /** 主窗口 banner「去更新」跳转：切到「高级」标签（下载/安装 UI 所在处）。 */
  useEffect(() => {
    const un = listen<{ tab: "index" | "search" | "service" | "advanced" }>(
      "findx2-open-settings-tab",
      (e) => {
        setSettingsTab(e.payload.tab);
      },
    );
    return () => {
      void un.then((f) => f());
    };
  }, []);

  const effectiveTheme = useMemo<"light" | "dark">(() => {
    if (uiThemePref === "dark") return "dark";
    if (uiThemePref === "light") return "light";
    return systemDark ? "dark" : "light";
  }, [uiThemePref, systemDark]);

  useLayoutEffect(() => {
    document.documentElement.setAttribute("data-fx-theme", effectiveTheme);
    try {
      localStorage.setItem(UI_THEME_KEY, uiThemePref);
    } catch {
      /* ignore */
    }
  }, [effectiveTheme, uiThemePref]);

  useEffect(() => {
    settingsRef.current = settings;
  }, [settings]);

  useEffect(() => {
    const isDark = effectiveTheme === "dark";
    const colors = windowThemeColors(isDark);
    void invoke("sync_window_theme_command", {
      themeMode: isDark ? "dark" : "light",
      backgroundColor: colors.backgroundColor,
      titleBarColor: colors.titleBarColor,
      titleBarTextColor: colors.titleBarTextColor,
    }).catch(() => {});
  }, [effectiveTheme]);

  const loadSettings = useCallback(async () => {
    try {
      const s = await invoke<FindxGuiSettings>("load_findx_settings");
      setSettings(s);
      setOriginalSettings(s);
      setSearchLimitInput(String(s.searchLimit));
      setSaveIntervalInput(String(s.saveIntervalSecs ?? 30));
    } catch {
      /* ignore */
    }
  }, []);

  /**
   * 从后端回读自启真值（注册表）。与 loadSettings 分开，因为二者数据源不同：
   * 设置文件可能被别处改过，而注册表才是「按下去会不会真开机启动」的答案。
   */
  const loadAutostart = useCallback(async () => {
    try {
      setAutostart(await invoke<AutostartState>("get_autostart_state"));
    } catch {
      /* 读不到就保持上一次状态，不臆造 enabled=true */
    }
  }, []);

  /**
   * 拉取墓碑统计（来自 service 内存里的 IndexStore，经 IPC Status 透传）。
   * 服务不在跑 / 旧版 service 未上报时保持 null，UI 显示「未知」而不是编造数字。
   */
  const loadIndexHealth = useCallback(async () => {
    try {
      const st = await invoke<{
        ready?: boolean;
        indexedCount?: number;
        tombstoneCount?: number | null;
      }>("index_status");
      setIdxEntries(typeof st.indexedCount === "number" ? st.indexedCount : null);
      setTombCount(typeof st.tombstoneCount === "number" ? st.tombstoneCount : null);
    } catch {
      /* 保持上一次状态 */
    }
  }, []);

  const toggleAutostart = async (enable: boolean) => {
    if (autostartBusy) return;
    setAutostartBusy(true);
    try {
      const s = await invoke<AutostartState>("set_autostart", { enable });
      setAutostart(s);
      setHint(
        s.enabled
          ? "已开启：下次登录 Windows 时自动启动 FindX 托盘"
          : "已关闭：登录时不再自动启动 FindX",
      );
      window.setTimeout(() => setHint(""), 3000);
    } catch (e) {
      setHint(`设置开机启动失败: ${String(e)}`);
      // 失败后回读真值，避免 UI 停留在一个假的勾选态。
      await loadAutostart();
    } finally {
      setAutostartBusy(false);
    }
  };

  useEffect(() => {
    void loadSettings();
    void loadAutostart();
    void loadIndexHealth();
    let unlisten: (() => void) | undefined;
    void listen("findx2-settings-reload", () => {
      void loadSettings();
      void loadAutostart();
      void loadIndexHealth();
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [loadSettings, loadAutostart, loadIndexHealth]);

  useEffect(() => {
    let aborted = false;
    void (async () => {
      try {
        const [drives, os] = await Promise.all([
          invoke<Array<{ letter: string; canOpenVolume?: boolean }>>("list_drives"),
          invoke<string>("host_platform").catch(() => "windows"),
        ]);
        if (!aborted) {
          setHostOs(os);
          setAvailableDrives(
            drives
              .map((d) => d.letter.trim())
              .filter(Boolean)
              .map((s) => {
                if (os === "windows") {
                  return s.endsWith(":") ? s : `${s}:`;
                }
                return s;
              }),
          );
        }
      } catch {
        /* ignore */
      }
    })();
    return () => {
      aborted = true;
    };
  }, []);

  const saveSettings = async () => {
    const limit = commitSearchLimit(searchLimitInput);
    if (!limit.ok) {
      setHint(limit.message);
      return;
    }
    const interval = commitSaveIntervalSecs(saveIntervalInput);
    if (!interval.ok) {
      setHint(interval.message);
      return;
    }
    const next: FindxGuiSettings = {
      ...settings,
      searchLimit: limit.value,
      saveIntervalSecs: interval.value,
    };
    try {
      await invoke("save_findx_settings", { settings: next });
      await emit("findx2-settings-saved", {});
      setSettings(next);
      setOriginalSettings(next);
      setOriginalUiThemePref(uiThemePref);
      setSearchLimitInput(String(next.searchLimit));
      setSaveIntervalInput(String(next.saveIntervalSecs ?? 30));
      setHint("设置已保存");
      window.setTimeout(() => setHint(""), 2500);
    } catch (e) {
      setHint(`保存失败: ${String(e)}`);
    }
  };

  const startSvc = async () => {
    if (serviceBusy) {
      setHint("服务操作正在进行中，请稍候…");
      return;
    }
    setServiceBusy(true);
    setHint("正在启动服务…");
    try {
      await invoke("start_findx_service");
      setHint(serviceStartHint("sent"));
    } catch (e) {
      const errMsg = String(e);
      if (errMsg.includes("already running") || errMsg.includes("已运行")) {
        setHint(serviceStartHint("already"));
      } else {
        setHint(serviceStartHint("failed", errMsg));
      }
    } finally {
      setTimeout(() => setServiceBusy(false), 1000);
    }
  };

  const stopSvc = async () => {
    if (serviceBusy) {
      setHint("服务操作正在进行中，请稍候…");
      return;
    }
    if (
      !confirm(
        "确定要停止索引服务吗？\n\n" +
          "停止后将无法搜索文件，增量索引也会暂停。\n" +
          "需要重新启动服务才能恢复功能。"
      )
    ) {
      return;
    }
    setServiceBusy(true);
    setHint("正在停止服务…");
    try {
      await invoke("stop_findx_service");
      setHint("已请求停止服务进程，服务将在处理完当前任务后退出。");
    } catch (e) {
      const errMsg = String(e);
      if (errMsg.includes("not running") || errMsg.includes("未运行")) {
        setHint("服务未在运行。");
      } else {
        setHint(`停止失败：${errMsg}`);
      }
    } finally {
      setTimeout(() => setServiceBusy(false), 1000);
    }
  };

  const checkUpdateFromGithub = async () => {
    const requestId = updateGateRef.current.tryBegin();
    if (requestId == null) {
      setHint("检查正在进行中，请稍候…");
      return;
    }
    updateOpIdRef.current = requestId;
    setUpdBusy(true);
    setHint("正在从 GitHub 检查更新…");
    try {
      const info = await invoke<AppUpdateInfo>("check_app_update");
      if (!settleUpdate(updateGateRef.current, requestId, info, setUpdInfo)) return;
      if (!info.ok) {
        setHint(info.error ?? "检查失败");
        return;
      }
      if (info.hasUpdate) {
        setHint(
          info.downloadUrl
            ? `发现新版本 ${info.latestVersion ?? ""}，可直接下载安装。`
            : `发现新版本 ${info.latestVersion ?? ""}，本平台需手动下载，正在打开发行页。`,
        );
        if (!info.downloadUrl && info.releasePageUrl) {
          await invoke("open_external_url", { url: info.releasePageUrl });
        }
        return;
      }
      if (info.error) {
        setHint(info.error);
        return;
      }
      setHint(`当前 ${info.currentVersion} 已是最新。`);
    } catch (e) {
      if (updateGateRef.current.isCurrent(requestId)) {
        setHint(String(e));
      }
    } finally {
      if (updateGateRef.current.finish(requestId)) {
        setUpdBusy(false);
      }
    }
  };

  /** 开始下载安装包；进度由 findx2-update-progress 事件驱动。 */
  const startDownload = async () => {
    if (!updInfo?.downloadUrl) return;
    const requestId = updateGateRef.current.tryBegin();
    if (requestId == null) {
      setHint("操作正在进行中，请稍候…");
      return;
    }
    updateOpIdRef.current = requestId;
    setUpdBusy(true);
    setDownloadedPath(null);
    setDlProgress({ downloaded: 0, total: updInfo.assetSize ?? 0, percent: 0 });
    try {
      await invoke("download_app_update", {
        url: updInfo.downloadUrl,
        expectedSize: updInfo.assetSize ?? null,
      });
      if (updateGateRef.current.isCurrent(requestId)) {
        setHint("正在下载更新…");
      }
    } catch (e) {
      if (updateGateRef.current.finish(requestId)) {
        setUpdBusy(false);
        setDlProgress(null);
        setHint(String(e));
      }
    }
  };

  const cancelDownload = async () => {
    updateGateRef.current.invalidate();
    updateOpIdRef.current = 0;
    setUpdBusy(false);
    setDlProgress(null);
    try {
      await invoke("cancel_update_download");
      setHint("正在取消下载…");
    } catch (e) {
      setHint(String(e));
    }
  };

  /** 以管理员授权运行静默安装器；安装器会自动重启 FindX（见 installer/FindX.iss）。 */
  const installUpdate = async () => {
    if (!downloadedPath) return;
    setHint("正在启动安装…（请同意管理员授权；期间 FindX 自动关闭并在完成后重新启动）");
    try {
      await invoke("install_downloaded_update", { path: downloadedPath });
      // wait_for_exit=true：走到这里说明安装器已退出且 GUI 未被杀（异常路径），提示手动处理。
      setHint("安装器已退出但窗口仍在，请手动重启 FindX 生效。");
    } catch (e) {
      setHint(`未开始安装：${String(e)}`);
    }
  };

  const rebuildIdx = async () => {
    if (rebuildBusy) return;
    if (!confirm("将停止索引服务并重新扫描所选磁盘，期间搜索不可用。继续？")) return;
    setRebuildBusy(true);
    setIndexOp(indexOpFeedback("rebuild", "working"));
    try {
      await invoke("save_findx_settings", { settings });
      await invoke("rebuild_index");
      setIndexOp(indexOpFeedback("rebuild", "submitted"));
    } catch (e) {
      setIndexOp(indexOpFeedback("rebuild", "failed", String(e)));
    } finally {
      setRebuildBusy(false);
    }
  };

  /** 一键压缩索引：后端编排「停服务 → 提权 findx2 compact → 重启服务」，返回结果摘要。 */
  const compactIdx = async () => {
    if (compactBusy) return;
    if (
      !confirm(
        "压缩会短暂停止索引服务（搜索暂时不可用），并请求一次管理员授权。" +
          "有效数据不会被删除，只物理移除墓碑残留。继续？",
      )
    )
      return;
    setCompactBusy(true);
    setIndexOp(indexOpFeedback("compact", "working"));
    try {
      const r = await invoke<{ ok: boolean; message: string }>("compact_index");
      setIndexOp(indexOpFeedback("compact", r.ok ? "submitted" : "failed", r.message));
    } catch (e) {
      setIndexOp(indexOpFeedback("compact", "failed", String(e)));
    } finally {
      setCompactBusy(false);
      // 服务重启后要重新加载 index.bin（秒级），立即查多半是 loading，延迟再拉。
      window.setTimeout(() => void loadIndexHealth(), 4000);
    }
  };

  const applyRunMode = async (target: RunMode) => {
    try {
      const next = { ...settings, runMode: target };
      setSettings(next);
      await invoke("save_findx_settings", { settings: next });
      await invoke("apply_run_mode_change", { target });
      if (
        confirm(
          target === "service"
            ? "已注册为系统服务（开机自启、不再弹 UAC）。重启 FindX2 生效？"
            : "已卸载系统服务。重启 FindX2 后将以 UAC 单体模式运行？",
        )
      ) {
        await invoke("restart_app");
      }
    } catch (e) {
      setHint(`切换模式失败: ${String(e)}`);
    }
  };

  const revertUnsaved = useCallback(() => {
    const reverted = discardUnsaved({
      settings: settingsRef.current,
      original: originalRef.current,
      theme: themeRef.current,
      originalTheme: originalThemeRef.current,
    });
    setSettings(reverted.settings);
    setUiThemePref(reverted.theme);
    setSearchLimitInput(String(reverted.settings.searchLimit));
    setSaveIntervalInput(String(reverted.settings.saveIntervalSecs ?? 30));
  }, []);

  const hideSettings = useCallback(() => {
    void (async () => {
      try {
        await invoke("hide_settings_window");
        return;
      } catch {
        /* fallback */
      }
      void getCurrentWindow().hide();
    })();
  }, []);

  /** 关闭按钮与系统关闭共用同一判断：脏表单需确认，取消时还原主题预览。 */
  const closeWindow = useCallback(() => {
    const decision = closeDecision(
      isDirtyRef.current,
      isDirtyRef.current ? confirm(CLOSE_DIRTY_MESSAGE) : true,
    );
    if (decision === "stay") return false;
    if (isDirtyRef.current) revertUnsaved();
    hideSettings();
    return true;
  }, [hideSettings, revertUnsaved]);

  /** 系统关闭只注册一次。Rust 侧只 prevent_close，由这里决定隐藏还是留下。 */
  useEffect(() => {
    const listener = createOnceListener();
    void listener.arm(() =>
      getCurrentWindow().onCloseRequested((event) => {
        event.preventDefault();
        closeWindow();
      }),
    );
    return () => listener.dispose();
  }, [closeWindow]);

  return (
    <div className="fx-settings-window-root">
      <div className="fx-settings-window-container">
        {/* 固定头部:标题+标签栏 */}
        <div className="fx-settings-header">
          <div className="fx-settings-title-row">
            <h2 className="fx-settings-title">FindX2 设置</h2>
            {dirty && <span className="fx-settings-dirty-indicator" title="有未保存的更改">●</span>}
          </div>
          <div className="fx-settings-tabs" role="tablist">
            {(
              [
                ["index", "索引"],
                ["search", "搜索"],
                ["service", "服务模式"],
                ["advanced", "高级"],
              ] as const
            ).map(([k, label]) => (
              <button
                key={k}
                type="button"
                role="tab"
                aria-selected={settingsTab === k}
                className="fx-settings-tab"
                onClick={() => setSettingsTab(k)}
              >
                {label}
              </button>
            ))}
          </div>
        </div>

        {/* 可滚动内容区 */}
        <div className="fx-settings-content-scroll">
        {settingsTab === "index" && (
          <div>
            <fieldset className="fx-settings-group">
              <legend>索引范围</legend>
              <p className="fx-hint">
                {hostOs === "windows"
                  ? "勾选要扫描的盘。一个都不勾 = 全盘，与命令行默认一致。"
                  : "勾选要扫描的挂载点。一个都不勾 = 默认挂载点。"}
                {availableDrives.length > 0 && (
                  <>
                    {" "}
                    当前已选 {(settings.drives ?? []).length} / {availableDrives.length}。
                  </>
                )}
              </p>
              {availableDrives.length === 0 ? (
                <p className="fx-hint">（加载磁盘列表中…）</p>
              ) : (
                <IndexRangeList
                  paths={availableDrives}
                  selected={settings.drives ?? []}
                  onChange={(drives) => setSettings((s) => ({ ...s, drives }))}
                />
              )}
            </fieldset>

            <fieldset className="fx-settings-group">
              <legend>排除目录</legend>
              <p className="fx-hint">
                每行一个完整路径，例如{" "}
                {hostOs === "windows"
                  ? "C:\\Windows\\WinSxS"
                  : hostOs === "darwin" || hostOs === "macos"
                    ? "/System/Volumes/Data/private/var/folders"
                    : "/usr/share"}
                。规则写入 index.exclude.json；改完要「重建索引」才会清掉已经入库的条目。
              </p>
              <ExcludeDirsEditor
                value={settings.excludedDirs ?? []}
                onChange={(dirs) => setSettings((s) => ({ ...s, excludedDirs: dirs }))}
                hostOs={hostOs}
              />
            </fieldset>
            {(hostOs === "darwin" || hostOs === "macos") && (
              <p className="fx-hint">
                macOS 扫整盘需要「系统设置 → 隐私与安全性 → 完全磁盘访问权限」勾选 FindX；
                未授权时会改扫家目录，状态栏会提示原因。
              </p>
            )}
            {hostOs === "linux" && (
              <p className="fx-hint">
                Linux 默认跳过 /proc、/sys、/dev 等虚拟挂载。无 CAP_SYS_ADMIN 时增量会降级为
                inotify（非整盘实时），状态栏会提示。
              </p>
            )}

            <fieldset className="fx-settings-group">
              <legend>元数据</legend>
            <Checkbox
              checked={settings.enableMetadataBackfill ?? true}
              label="开启时间/大小元数据回填（默认开启）"
              onChange={(checked) =>
                setSettings((s) => ({ ...s, enableMetadataBackfill: checked }))
              }
            />
            <p className="fx-hint">
              {hostOs === "windows"
                ? "Windows：fast 首遍走 MFT，后台按目录补齐大小与时间。"
                : "macOS / Linux：fast 首遍只收文件名（不 stat），后台再按路径补齐大小与时间。"}
            </p>
            {!(settings.enableMetadataBackfill ?? true) && (
              <p className="fx-warn">
                ⚠ 关闭后 fast 首遍扫到的文件 size/mtime 将一直为 0，「按大小/时间」筛选与排序失效；
                可换来更低的 CPU/磁盘 IO 占用。
              </p>
            )}
            <Checkbox
              checked={settings.firstIndexFullMetadata ?? false}
              label="首次建库直接读全量元数据（更慢，但即时可用）"
              onChange={(checked) =>
                setSettings((s) => ({ ...s, firstIndexFullMetadata: checked }))
              }
            />
            {hostOs !== "windows" && (
              <p className="fx-hint">
                开启后扫描时同步 statx / getattrlist，建库更慢，但按大小/时间筛选立刻准确。
              </p>
            )}
            </fieldset>

            <div className="fx-settings-row">
              <button
                type="button"
                className="primary"
                onClick={() => void rebuildIdx()}
                disabled={rebuildBusy}
              >
                {rebuildBusy ? "重建中…" : "重建索引"}
              </button>
              <span className="fx-hint" style={{ alignSelf: "center" }}>
                会停止服务、删除现有 index.bin、按当前设置重新扫描。
              </span>
            </div>

            {(() => {
              const tomb = tombCount;
              const entries = idxEntries;
              const ratio =
                tomb != null && entries != null && entries > 0 ? tomb / entries : null;
              const suggest =
                ratio != null && entries != null && entries >= 100_000 && ratio > 0.25;
              return (
                <>
                  <div className="fx-settings-row">
                    <button
                      type="button"
                      onClick={() => void compactIdx()}
                      disabled={compactBusy || rebuildBusy || tomb === 0}
                    >
                      {compactBusy ? "压缩中…" : "压缩索引"}
                    </button>
                    <span className="fx-hint" style={{ alignSelf: "center" }}>
                      {tomb == null
                        ? "墓碑情况未知（服务未运行或旧版本未上报）；压缩可物理移除删除/排除留下的占位残留。"
                        : tomb === 0
                          ? "没有墓碑，无需压缩。"
                          : `共 ${(entries ?? 0).toLocaleString()} 条，其中墓碑 ${tomb.toLocaleString()}（${((ratio ?? 0) * 100).toFixed(1)}%）。`}
                    </span>
                  </div>
                  {suggest && (
                    <p className="fx-warn">
                      ⚠ 墓碑占比已超过 25%，占着内存与文件体积还拖慢加载，建议压缩回收。
                    </p>
                  )}
                  {indexOp && (
                    <p className="fx-settings-op" role="status">
                      {indexOp}
                    </p>
                  )}
                </>
              );
            })()}
          </div>
        )}

        {settingsTab === "search" && (
          <div>
            <Checkbox
              checked={settings.pinyinDefault}
              label="默认拼音匹配"
              onChange={(checked) => setSettings((s) => ({ ...s, pinyinDefault: checked }))}
            />
            <p className="fx-hint">
              与 Windows 相同：全拼 <code>beijing</code>、简拼 <code>bj</code>、
              <code>;py</code> 强制拼音、<code>;en</code>/<code>;np</code> 关拼音。
              <code>ext:</code> <code>file:</code> <code>folder:</code> <code>startwith:</code>{" "}
              <code>endwith:</code> <code>path:</code> <code>parent:</code> <code>|</code>{" "}
              <code>!</code> 均可叠拼音。Unix 路径用 <code>/home/foo</code> 或{" "}
              <code>~/Documents</code>，不要写成盘符。
            </p>
            <SettingsRow
              label="结果条数上限"
              hint="编辑时可以暂时留空。点保存时检查：须为 1 到 4294967295 的整数，与设置字段 u32 一致。默认 5000。没有另行规定的性能上限。"
            >
              <div className="fx-settings-inline">
                <Input
                  value={searchLimitInput}
                  onChange={setSearchLimitInput}
                  placeholder="例如 5000"
                  className="fx-input-narrow"
                />
                <Button size="sm" onClick={() => setShowSyntaxHelp(true)} title="查看搜索语法帮助">
                  语法帮助
                </Button>
              </div>
            </SettingsRow>
          </div>
        )}

        {settingsTab === "service" && (
          <div>
            {hostOs === "windows" ? (
              <SettingsRow label="启动模式">
                <RadioGroup
                  name="run-mode"
                  value={settings.runMode ?? "service"}
                  onChange={(value) => void applyRunMode(value)}
                  options={[
                    { value: "service", label: "服务模式（默认；不弹 UAC，开机自启）" },
                    { value: "standalone", label: "单体 UAC 模式（每次启动会请求管理员授权）" },
                  ]}
                />
              </SettingsRow>
            ) : (
              <p className="fx-hint">
                当前由 GUI 以当前用户拉起 findx2-service，并不是 LaunchAgent / systemd 安装。
                退出应用时会结束该进程；需要开机自启请自行添加登录项或用户级 systemd。
                无完全磁盘访问或 CAP_SYS_ADMIN 时只能索引可见目录，增量可能降级。
              </p>
            )}

            {hostOs === "windows" ? (
              <Checkbox
                checked={settings.enableEverythingIpc ?? true}
                label="开启 Everything SDK 兼容窗口（IbEverythingExt 等老客户端依赖）"
                onChange={(checked) =>
                  setSettings((s) => ({ ...s, enableEverythingIpc: checked }))
                }
              />
            ) : (
              <Checkbox
                checked={false}
                disabled
                label="Everything SDK 兼容窗口仅 Windows 可用"
                onChange={() => {}}
              />
            )}

            <Checkbox
              checked={settings.autoStartService ?? true}
              label="GUI 启动时自动拉起索引服务"
              onChange={(checked) => setSettings((s) => ({ ...s, autoStartService: checked }))}
            />

            <Checkbox
              checked={autostart.enabled}
              disabled={!autostart.supported || autostartBusy}
              label="随系统启动（登录时自动启动 FindX 托盘）"
              onChange={(checked) => void toggleAutostart(checked)}
            />
            {autostart.supported ? (
              <p className="fx-hint" style={{ marginTop: -4, marginBottom: 8 }}>
                {hostOs === "windows"
                  ? `写入当前用户的登录启动项（HKCU\\...\\Run），不需要管理员权限。${
                      autostart.command ? `当前记录：${autostart.command}` : ""
                    }`
                  : "由本程序管理开机启动。"}
              </p>
            ) : (
              <p className="fx-hint" style={{ marginTop: -4, marginBottom: 8 }}>
                {autostart.unsupportedReason ?? "当前平台暂不支持此项。"}
              </p>
            )}

            <p className="fx-hint" style={{ marginTop: 8, marginBottom: 0 }}>
              注意：本开关只管 FindX 的托盘界面。索引服务在「服务模式」下由 Windows
              服务管理器随系统启动，与这里的设置无关。
            </p>

            <div className="fx-settings-row" style={{ gap: "12px", alignItems: "center" }}>
              <Button onClick={() => void startSvc()} disabled={serviceBusy}>
                {serviceBusy ? "操作中…" : "启动服务"}
              </Button>
              <Button onClick={() => void stopSvc()} disabled={serviceBusy}>
                {serviceBusy ? "操作中…" : "停止服务"}
              </Button>
              {serviceBusy && (
                <span className="fx-hint" style={{ margin: 0 }}>
                  服务状态变更中，请稍候…
                </span>
              )}
            </div>
            {hostOs !== "windows" && (
              <p className="fx-hint">
                ⚠️ 非 Windows 平台：服务控制仅适用于当前 GUI 启动的进程。
                如使用 systemd 或 LaunchAgent 托管，请使用系统命令管理。
              </p>
            )}
          </div>
        )}

        {settingsTab === "advanced" && (
          <div>
            {/* 应用更新卡片 */}
            <UpdateCard
              busy={updBusy}
              info={updInfo}
              progress={dlProgress}
              downloadedPath={downloadedPath}
              autoCheck={settings.autoCheckUpdate ?? true}
              onAutoCheck={(checked) => setSettings((s) => ({ ...s, autoCheckUpdate: checked }))}
              onCheck={() => void checkUpdateFromGithub()}
              onDownload={() => void startDownload()}
              onCancel={() => void cancelDownload()}
              onInstall={() => void installUpdate()}
            />

            <SettingsRow
              label="界面主题"
              hint="改主题会立刻预览。未保存就关闭时，会回到打开设置页时的主题。"
            >
              <RadioGroup
                name="ui-theme"
                direction="row"
                value={uiThemePref}
                onChange={setUiThemePref}
                options={[
                  { value: "light", label: "浅色" },
                  { value: "dark", label: "深色" },
                  { value: "system", label: "跟随系统" },
                ]}
              />
            </SettingsRow>

            <fieldset className="fx-settings-group">
              <legend>路径</legend>
              <SettingsRow
                label={
                  hostOs === "windows"
                    ? "索引文件（默认 index.bin，与程序同目录；正式安装走 ProgramData）"
                    : "索引文件（默认写入用户数据目录，勿放在 .app 包内）"
                }
              >
                <Input
                  value={settings.indexPath}
                  onChange={(value) => setSettings((s) => ({ ...s, indexPath: value }))}
                />
              </SettingsRow>
            </fieldset>

            <details className="fx-expert-fold">
              <summary>专家选项</summary>
              <SettingsRow label={hostOs === "windows" ? "命名管道名" : "IPC 套接字名"}>
                <Input
                  value={settings.pipeName}
                  onChange={(value) => setSettings((s) => ({ ...s, pipeName: value }))}
                />
              </SettingsRow>
              <SettingsRow label="findx2-service 路径（可空）">
                <Input
                  value={settings.serviceExePath}
                  onChange={(value) => setSettings((s) => ({ ...s, serviceExePath: value }))}
                />
              </SettingsRow>
              <SettingsRow
                label="增量落盘间隔（秒）"
                hint="可以先清空再重填。保存时才要求是不小于 1 的整数秒。"
              >
                <Input
                  value={saveIntervalInput}
                  onChange={setSaveIntervalInput}
                  className="fx-input-narrow"
                />
              </SettingsRow>
            </details>
          </div>
        )}
        </div>

        {/* 固定底栏:保存/关闭按钮 */}
        <div className="fx-settings-footer">
          <p className="fx-settings-footer-hint" title={hint || undefined}>
            {hint}
          </p>
          <div className="fx-settings-footer-actions">
            <Button variant="primary" onClick={() => void saveSettings()}>
              保存
            </Button>
            <Button onClick={closeWindow}>
              关闭
            </Button>
          </div>
        </div>
      </div>

      {showSyntaxHelp && (
        <div className="fx-modal-overlay" onClick={() => setShowSyntaxHelp(false)}>
          <div className="fx-modal-dialog" onClick={(e) => e.stopPropagation()}>
            <div className="fx-modal-header">
              <h3>搜索语法帮助</h3>
              <button
                type="button"
                className="fx-btn-close"
                onClick={() => setShowSyntaxHelp(false)}
                title="关闭"
              >
                ✕
              </button>
            </div>
            <div className="fx-modal-body">
              <h4>基础搜索</h4>
              <ul>
                <li>
                  <code>beijing</code> 或 <code>bj</code> — 拼音全拼或简拼匹配
                </li>
                <li>
                  <code>project</code> — 直接输入英文名称
                </li>
              </ul>

              <h4>搜索修饰符</h4>
              <ul>
                <li>
                  <code>;py</code> — 强制拼音匹配
                </li>
                <li>
                  <code>;en</code> 或 <code>;np</code> — 关闭拼音，仅英文匹配
                </li>
              </ul>

              <h4>路径和文件类型</h4>
              <ul>
                <li>
                  <code>ext:pdf</code> — 扩展名匹配（仅 PDF 文件）
                </li>
                <li>
                  <code>file:report</code> — 文件名包含"report"
                </li>
                <li>
                  <code>folder:docs</code> — 文件夹名包含"docs"
                </li>
                <li>
                  <code>startwith:2024</code> — 文件名以"2024"开头
                </li>
                <li>
                  <code>endwith:backup</code> — 文件名以"backup"结尾
                </li>
                <li>
                  <code>path:/home/user</code> — 路径包含"/home/user"
                </li>
                <li>
                  <code>parent:Documents</code> — 父目录名包含"Documents"
                </li>
              </ul>

              <h4>组合与排除</h4>
              <ul>
                <li>
                  <code>report | document</code> — 或操作（匹配任一关键词）
                </li>
                <li>
                  <code>!temp</code> — 排除包含"temp"的结果
                </li>
                <li>
                  <code>ext:txt !backup</code> — TXT 文件但排除"backup"
                </li>
              </ul>

              <h4>路径格式说明</h4>
              <ul>
                <li>
                  <strong>Windows</strong>: <code>C:\Users\Name\Documents</code>
                </li>
                <li>
                  <strong>Unix/Linux</strong>: <code>/home/user/documents</code> 或{" "}
                  <code>~/Documents</code>
                </li>
                <li>
                  ⚠️ Unix 路径不要写成盘符格式（例如不要写 <code>C:</code>）
                </li>
              </ul>
            </div>
            <div className="fx-modal-footer">
              <button type="button" onClick={() => setShowSyntaxHelp(false)}>
                关闭
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
