import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useLayoutEffect, useMemo, useState } from "react";
import "./findx.css";
import type { AppUpdateInfo, FindxGuiSettings, RunMode, UiThemePref } from "./findxGuiTypes";
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
  });
  const [settingsTab, setSettingsTab] = useState<"index" | "search" | "service" | "advanced">(
    "index",
  );
  const [rebuildBusy, setRebuildBusy] = useState(false);
  const [availableDrives, setAvailableDrives] = useState<string[]>([]);
  const [uiThemePref, setUiThemePref] = useState<UiThemePref>(() => loadUiThemePref());
  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  const [hint, setHint] = useState("");

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

  useEffect(() => {
    void loadSettings();
    let unlisten: (() => void) | undefined;
    void listen("findx2-settings-reload", () => {
      void loadSettings();
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [loadSettings]);

  useEffect(() => {
    let aborted = false;
    void (async () => {
      try {
        const drives = await invoke<Array<{ letter: string; canOpenVolume?: boolean }>>("list_drives");
        if (!aborted) {
          setAvailableDrives(
            drives
              .map((d) => d.letter.trim())
              .filter(Boolean)
              .map((s) => (s.endsWith(":") ? s : `${s}:`)),
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
            <label>索引磁盘（不勾选 = 全盘）</label>
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

            <label>排除目录（每行一个完整路径，例如 C:\Windows\WinSxS）</label>
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
              排除规则会写入 index.exclude.json，service 启动 / USN 增量都会过滤；
              修改后需要点「重建索引」才能彻底清掉历史已入库条目。
            </p>

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

            <label>索引文件（默认 index.bin，与程序同目录）</label>
            <input
              type="text"
              value={settings.indexPath}
              onChange={(e) => setSettings((s) => ({ ...s, indexPath: e.target.value }))}
            />
            <label>命名管道名</label>
            <input
              type="text"
              value={settings.pipeName}
              onChange={(e) => setSettings((s) => ({ ...s, pipeName: e.target.value }))}
            />
            <label>findx2-service.exe 路径（可空）</label>
            <input
              type="text"
              value={settings.serviceExePath}
              onChange={(e) =>
                setSettings((s) => ({ ...s, serviceExePath: e.target.value }))
              }
            />
            <label>USN 落盘间隔（秒）</label>
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
