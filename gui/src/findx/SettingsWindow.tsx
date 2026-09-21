import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useLayoutEffect, useMemo, useState } from "react";
import "./findx.css";
import type { AppUpdateInfo, AutostartState, FindxGuiSettings, RunMode, UiThemePref } from "./findxGuiTypes";
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
  });
  const [settingsTab, setSettingsTab] = useState<"index" | "search" | "service" | "advanced">(
    "index",
  );
  const [rebuildBusy, setRebuildBusy] = useState(false);
  const [availableDrives, setAvailableDrives] = useState<string[]>([]);
  const [hostOs, setHostOs] = useState<string>("windows");
  const [uiThemePref, setUiThemePref] = useState<UiThemePref>(() => loadUiThemePref());
  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  const [hint, setHint] = useState("");
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

  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const fn = () => setSystemDark(mq.matches);
    mq.addEventListener("change", fn);
    return () => mq.removeEventListener("change", fn);
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
    const isDark = effectiveTheme === "dark";
    const bg = isDark ? "#000000" : "#fafafa";
    const titleBar = isDark ? "#000000" : "#ebebeb";
    const titleText = isDark ? "#e8eaed" : "#1f1f1f";
    void invoke("sync_window_theme_command", {
      themeMode: isDark ? "dark" : "light",
      backgroundColor: bg,
      titleBarColor: titleBar,
      titleBarTextColor: titleText,
    }).catch(() => {});
  }, [effectiveTheme]);

  const loadSettings = useCallback(async () => {
    try {
      const s = await invoke<FindxGuiSettings>("load_findx_settings");
      setSettings(s);
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
    try {
      await invoke("save_findx_settings", { settings });
      await emit("findx2-settings-saved", {});
      setHint("设置已保存");
      window.setTimeout(() => setHint(""), 2500);
    } catch (e) {
      setHint(`保存失败: ${String(e)}`);
    }
  };

  const startSvc = async () => {
    try {
      await invoke("start_findx_service");
      setHint("已启动 findx2-service");
    } catch (e) {
      setHint(String(e));
    }
  };

  const stopSvc = async () => {
    try {
      await invoke("stop_findx_service");
      setHint("已请求停止服务进程");
    } catch (e) {
      setHint(String(e));
    }
  };

  const checkUpdateFromGithub = async () => {
    setHint("正在从 GitHub 检查更新…");
    try {
      const info = await invoke<AppUpdateInfo>("check_app_update");
      if (!info.ok) {
        setHint(info.error ?? "检查失败");
        return;
      }
      if (info.hasUpdate && info.releasePageUrl) {
        setHint(`发现新版本 ${info.latestVersion ?? ""}，正在打开发行页。`);
        await invoke("open_external_url", { url: info.releasePageUrl });
        return;
      }
      if (info.error) {
        setHint(info.error);
        return;
      }
      setHint(`当前 ${info.currentVersion} 已是最新，或发行标签无法解析为语义化版本。`);
    } catch (e) {
      setHint(String(e));
    }
  };

  const rebuildIdx = async () => {
    if (rebuildBusy) return;
    if (!confirm("将停止索引服务并重新扫描所选磁盘，期间搜索不可用。继续？")) return;
    setRebuildBusy(true);
    try {
      await invoke("save_findx_settings", { settings });
      await invoke("rebuild_index");
      setHint("已开始重建索引（进度见主窗口状态条）");
    } catch (e) {
      setHint(`重建失败: ${String(e)}`);
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
    setHint("正在压缩索引…（完成后会自动重启索引服务）");
    try {
      const r = await invoke<{ ok: boolean; message: string }>("compact_index");
      setHint(r.message);
      if (!r.ok) window.setTimeout(() => setHint(""), 10000);
    } catch (e) {
      setHint(`压缩失败: ${String(e)}`);
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

  /** 禁止 close() 销毁窗口（否则再次 show_settings_window 会失败）；隐藏即可反复打开 */
  const closeWindow = () => {
    void (async () => {
      try {
        await invoke("hide_settings_window");
        return;
      } catch {
        /* fallback */
      }
      void getCurrentWindow().hide();
    })();
  };

  return (
    <div className="fx-settings-window-root">
      <div className="fx-settings fx-settings--dialog">
        <h2>FindX2 设置</h2>
        {hint ? (
          <p className="fx-hint" style={{ marginTop: -4, marginBottom: 8 }}>
            {hint}
          </p>
        ) : null}
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

        <div className="fx-settings-scroll">
        {settingsTab === "index" && (
          <div>
            <label>{hostOs === "windows" ? "索引磁盘（不勾选 = 全盘）" : "索引范围（不勾选 = 默认挂载点）"}</label>
            <div className="fx-drives">
              {availableDrives.length === 0 && (
                <span className="fx-hint">（加载磁盘列表中…）</span>
              )}
              {availableDrives.map((d) => {
                const drives = settings.drives ?? [];
                const checked = drives.includes(d);
                return (
                  <label key={d}>
                    <input
                      type="checkbox"
                      checked={checked}
                      onChange={(e) => {
                        setSettings((s) => {
                          const cur = new Set(s.drives ?? []);
                          if (e.target.checked) cur.add(d);
                          else cur.delete(d);
                          return { ...s, drives: Array.from(cur) };
                        });
                      }}
                    />
                    {d}
                  </label>
                );
              })}
            </div>

            <label>
              排除目录（每行一个完整路径，例如{" "}
              {hostOs === "windows"
                ? "C:\\Windows\\WinSxS"
                : hostOs === "darwin" || hostOs === "macos"
                  ? "/System/Volumes/Data/private/var/folders"
                  : "/usr/share"}
              ）
            </label>
            <textarea
              value={(settings.excludedDirs ?? []).join("\n")}
              onChange={(e) =>
                setSettings((s) => ({
                  ...s,
                  excludedDirs: e.target.value
                    .split(/\r?\n/)
                    .map((x) => x.trim())
                    .filter(Boolean),
                }))
              }
              rows={4}
            />
            <p className="fx-hint">
              排除规则会写入 index.exclude.json，服务启动与增量监听都会过滤；
              修改后需要点「重建索引」才能彻底清掉历史已入库条目。
            </p>
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

            <label>
              <input
                type="checkbox"
                checked={settings.enableMetadataBackfill ?? true}
                onChange={(e) =>
                  setSettings((s) => ({ ...s, enableMetadataBackfill: e.target.checked }))
                }
              />{" "}
              开启时间/大小元数据回填（默认开启）
            </label>
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
            <label>
              <input
                type="checkbox"
                checked={settings.firstIndexFullMetadata ?? false}
                onChange={(e) =>
                  setSettings((s) => ({ ...s, firstIndexFullMetadata: e.target.checked }))
                }
              />{" "}
              首次建库直接读全量元数据（更慢，但即时可用）
            </label>
            {hostOs !== "windows" && (
              <p className="fx-hint">
                开启后扫描时同步 statx / getattrlist，建库更慢，但按大小/时间筛选立刻准确。
              </p>
            )}

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
                </>
              );
            })()}
          </div>
        )}

        {settingsTab === "search" && (
          <div>
            <label>
              <input
                type="checkbox"
                checked={settings.pinyinDefault}
                onChange={(e) =>
                  setSettings((s) => ({ ...s, pinyinDefault: e.target.checked }))
                }
              />{" "}
              默认拼音匹配
            </label>
            <p className="fx-hint">
              与 Windows 相同：全拼 <code>beijing</code>、简拼 <code>bj</code>、
              <code>;py</code> 强制拼音、<code>;en</code>/<code>;np</code> 关拼音。
              <code>ext:</code> <code>file:</code> <code>folder:</code> <code>startwith:</code>{" "}
              <code>endwith:</code> <code>path:</code> <code>parent:</code> <code>|</code>{" "}
              <code>!</code> 均可叠拼音。Unix 路径用 <code>/home/foo</code> 或{" "}
              <code>~/Documents</code>，不要写成盘符。
            </p>
            <label>结果条数上限</label>
            <input
              type="text"
              value={String(settings.searchLimit)}
              onChange={(e) =>
                setSettings((s) => ({
                  ...s,
                  searchLimit: parseInt(e.target.value, 10) || 500,
                }))
              }
            />
          </div>
        )}

        {settingsTab === "service" && (
          <div>
            {hostOs === "windows" ? (
              <>
            <label>启动模式</label>
            <div className="fx-settings-row" style={{ marginTop: 0, marginBottom: 12 }}>
              <label style={{ fontWeight: "normal", margin: 0 }}>
                <input
                  type="radio"
                  name="run-mode"
                  checked={(settings.runMode ?? "service") === "service"}
                  onChange={() => void applyRunMode("service")}
                />{" "}
                服务模式（默认；不弹 UAC，开机自启）
              </label>
              <label style={{ fontWeight: "normal", margin: 0 }}>
                <input
                  type="radio"
                  name="run-mode"
                  checked={settings.runMode === "standalone"}
                  onChange={() => void applyRunMode("standalone")}
                />{" "}
                单体 UAC 模式（每次启动会请求管理员授权）
              </label>
            </div>
              </>
            ) : (
              <p className="fx-hint">
                当前由 GUI 以当前用户拉起 findx2-service，并不是 LaunchAgent / systemd 安装。
                退出应用时会结束该进程；需要开机自启请自行添加登录项或用户级 systemd。
                无完全磁盘访问或 CAP_SYS_ADMIN 时只能索引可见目录，增量可能降级。
              </p>
            )}

            {hostOs === "windows" && (
            <label>
              <input
                type="checkbox"
                checked={settings.enableEverythingIpc ?? true}
                onChange={(e) =>
                  setSettings((s) => ({ ...s, enableEverythingIpc: e.target.checked }))
                }
              />{" "}
              开启 Everything SDK 兼容窗口（IbEverythingExt 等老客户端依赖）
            </label>
            )}

            <label>
              <input
                type="checkbox"
                checked={settings.autoStartService ?? true}
                onChange={(e) =>
                  setSettings((s) => ({ ...s, autoStartService: e.target.checked }))
                }
              />{" "}
              GUI 启动时自动拉起索引服务
            </label>

            <label>
              <input
                type="checkbox"
                checked={autostart.enabled}
                disabled={!autostart.supported || autostartBusy}
                onChange={(e) => void toggleAutostart(e.target.checked)}
              />{" "}
              随系统启动（登录时自动启动 FindX 托盘）
            </label>
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

            <div className="fx-settings-row">
              <button type="button" onClick={() => void startSvc()}>
                启动服务
              </button>
              <button type="button" onClick={() => void stopSvc()}>
                停止服务
              </button>
            </div>
          </div>
        )}

        {settingsTab === "advanced" && (
          <div>
            <label>应用更新</label>
            <div className="fx-settings-row" style={{ marginBottom: 8 }}>
              <button type="button" onClick={() => void checkUpdateFromGithub()}>
                从 GitHub 检查更新
              </button>
            </div>
            <p className="fx-hint" style={{ marginTop: -6, marginBottom: 16 }}>
              请求 GitHub API 对比本程序版本与仓库{" "}
              <a
                href="https://github.com/chaojimct/findx/releases"
                target="_blank"
                rel="noreferrer"
              >
                chaojimct/findx
              </a>{" "}
              的最新 Release（需联网）。
            </p>

            <label>界面主题</label>
            <div className="fx-settings-row" style={{ marginTop: 4, marginBottom: 14 }}>
              <label style={{ fontWeight: "normal", margin: 0 }}>
                <input
                  type="radio"
                  name="ui-theme"
                  checked={uiThemePref === "light"}
                  onChange={() => setUiThemePref("light")}
                />{" "}
                浅色
              </label>
              <label style={{ fontWeight: "normal", margin: 0 }}>
                <input
                  type="radio"
                  name="ui-theme"
                  checked={uiThemePref === "dark"}
                  onChange={() => setUiThemePref("dark")}
                />{" "}
                深色（全黑）
              </label>
              <label style={{ fontWeight: "normal", margin: 0 }}>
                <input
                  type="radio"
                  name="ui-theme"
                  checked={uiThemePref === "system"}
                  onChange={() => setUiThemePref("system")}
                />{" "}
                跟随系统
              </label>
            </div>
            <p className="fx-hint" style={{ marginTop: -8 }}>
              深色模式背景为纯黑 #000；主窗口列表列宽可在表头分隔条上拖拽，比例会记住。
            </p>

            <label>
              {hostOs === "windows"
                ? "索引文件（默认 index.bin，与程序同目录；正式安装走 ProgramData）"
                : "索引文件（默认写入用户数据目录，勿放在 .app 包内）"}
            </label>
            <input
              type="text"
              value={settings.indexPath}
              onChange={(e) => setSettings((s) => ({ ...s, indexPath: e.target.value }))}
            />
            <label>{hostOs === "windows" ? "命名管道名" : "IPC 套接字名"}</label>
            <input
              type="text"
              value={settings.pipeName}
              onChange={(e) => setSettings((s) => ({ ...s, pipeName: e.target.value }))}
            />
            <label>findx2-service 路径（可空）</label>
            <input
              type="text"
              value={settings.serviceExePath}
              onChange={(e) =>
                setSettings((s) => ({ ...s, serviceExePath: e.target.value }))
              }
            />
            <label>增量落盘间隔（秒）</label>
            <input
              type="number"
              min={1}
              value={settings.saveIntervalSecs ?? 30}
              onChange={(e) =>
                setSettings((s) => ({
                  ...s,
                  saveIntervalSecs: Math.max(1, parseInt(e.target.value, 10) || 30),
                }))
              }
            />
          </div>
        )}
        </div>

        <footer className="fx-settings-footer-bar">
          <div className="fx-settings-footer-actions">
            <button type="button" className="primary" onClick={() => void saveSettings()}>
              保存
            </button>
            <button type="button" onClick={closeWindow}>
              关闭
            </button>
          </div>
          {/* <p className="fx-hint fx-settings-footer-note">
            侧栏条件与搜索框关键词以空格组合。建索引与启动服务需访问卷设备时会弹出 Windows UAC 授权提权。
          </p> */}
        </footer>
      </div>
    </div>
  );
}
