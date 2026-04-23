import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

/**
 * Windows 资源管理器风格的预览面板。
 *
 * 渲染策略（按优先级）：
 * 1. 文件夹/不存在 → 提示；
 * 2. 图片：前端 `<img src="data:..">`（Rust 已实现 load_preview_data_url）——
 *    比 prevhost 更轻、不会盖住后续 React 内容；
 * 3. 文本/源码/markdown 等：调 load_preview_text 读前 64KB 直接显示；
 * 4. 其它（PDF/Office/视频/带预览处理器的 zip / 7z / dwg / ai …）：
 *    调 preview_show 让 Rust 创建 STATIC 子 HWND + IPreviewHandler，与 Explorer 一模一样；
 *    我们这里仅负责把面板的客户区矩形传过去，并在 resize / 面板关闭时同步。
 *
 * 关键坑：原生子窗口会**盖在 WebView 之上**，所以：
 * - 关闭面板 / 路径变成不可预览类型 → 必须 invoke preview_hide({unload: true})；
 * - 滚动外层容器、resize 主窗口、拖动分隔条 → 必须重新算矩形并 invoke preview_set_bounds。
 */

const TEXT_EXTS = new Set([
  "txt", "log", "md", "markdown", "json", "yaml", "yml", "toml", "xml", "html", "htm", "css", "scss",
  "less", "ini", "conf", "config", "cfg", "env", "csv", "tsv", "sql", "rs", "ts", "tsx", "js", "jsx",
  "mjs", "cjs", "py", "go", "java", "c", "cc", "cpp", "h", "hpp", "cs", "kt", "swift", "rb", "php",
  "lua", "sh", "ps1", "bat", "cmd", "vue", "svelte", "ipynb", "diff", "patch", "gradle", "make",
  "mk", "dockerfile", "gitignore", "lock", "properties",
]);
const IMAGE_EXTS = new Set([
  "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico",
]);

type PreviewMode = "none" | "image" | "text" | "native";

type PreviewFallbackFileInfo = {
  size: number;
  modifiedUnix: number | null;
  createdUnix: number | null;
};

function formatPreviewFileSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes === 0) return "0 B";
  const k = 1024;
  const sizes = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(k)), sizes.length - 1);
  const n = bytes / k ** i;
  const digits = i === 0 ? 0 : n < 10 ? 2 : n < 100 ? 2 : 1;
  return `${n.toFixed(digits)} ${sizes[i]}`;
}

const REL_DIVISIONS: { amount: number; unit: Intl.RelativeTimeFormatUnit }[] = [
  { amount: 60, unit: "second" },
  { amount: 60, unit: "minute" },
  { amount: 24, unit: "hour" },
  { amount: 7, unit: "day" },
  { amount: 4.34524, unit: "week" },
  { amount: 12, unit: "month" },
  { amount: Number.POSITIVE_INFINITY, unit: "year" },
];

/** 相对时间（zh-CN）+ 日历 yyyy/M，与资源管理器风格接近 */
function formatPreviewTimestamp(unixSec: number | null | undefined): string {
  if (unixSec == null) return "—";
  let duration = unixSec - Date.now() / 1000;
  const rtf = new Intl.RelativeTimeFormat("zh-CN", { numeric: "auto" });
  const d = new Date(unixSec * 1000);
  const cal = `${d.getFullYear()}/${d.getMonth() + 1}`;
  for (const { amount, unit } of REL_DIVISIONS) {
    if (Math.abs(duration) < amount) {
      return `${rtf.format(Math.round(duration), unit)} · ${cal}`;
    }
    duration /= amount;
  }
  return cal;
}

/** 在会长时间阻塞主线程的 invoke 之前让出两帧，确保「加载中 / 云同步」遮罩先被绘制。 */
function doubleRaf(): Promise<void> {
  return new Promise((resolve) => {
    requestAnimationFrame(() => {
      requestAnimationFrame(() => resolve());
    });
  });
}

function classifyByExt(path: string, isDir: boolean): PreviewMode {
  if (!path || isDir) return "none";
  const i = path.lastIndexOf(".");
  if (i < 0) return "native";
  const ext = path.slice(i + 1).toLowerCase();
  if (IMAGE_EXTS.has(ext)) return "image";
  if (TEXT_EXTS.has(ext)) return "text";
  return "native";
}

/** 用于预览区提示：云盘路径下 Rust 会分块水化后再调系统预览器。 */
function cloudStorageLabel(path: string): string | null {
  const lower = path.replace(/\\/g, "/").toLowerCase();
  if (lower.includes("onedrive")) return "OneDrive";
  if (lower.includes("wps cloud files")) return "WPS 云文档";
  if (lower.includes("dropbox")) return "Dropbox";
  if (lower.includes("google drive")) return "Google Drive";
  if (lower.includes("icloud")) return "iCloud";
  return null;
}

interface Props {
  /** 当前选中行的绝对路径；为 null 表示未选中。 */
  path: string | null;
  /** 是否文件夹（文件夹无预览）。 */
  isDirectory: boolean;
  /** 面板自身的 ref —— 父组件控制宽度后，面板内部 ResizeObserver 会同步给 Rust。 */
  className?: string;
}

function PreviewFallbackCard(props: {
  path: string;
  reason: string;
  meta: PreviewFallbackFileInfo | null;
}) {
  const { path, reason, meta } = props;
  const base = path.split(/[\\/]/).pop() ?? path;
  return (
    <div className="fx-preview-fallback-card">
      <div className="fx-preview-fallback-title">{base}</div>
      <div className="fx-preview-fallback-path" title={path}>
        {path}
      </div>
      <div className="fx-preview-fallback-reason">{reason}</div>
      <hr className="fx-preview-fallback-rule" />
      <dl className="fx-preview-fallback-dl">
        <div>
          <dt>大小</dt>
          <dd>{meta ? formatPreviewFileSize(meta.size) : "…"}</dd>
        </div>
        <div>
          <dt>创建</dt>
          <dd>{formatPreviewTimestamp(meta?.createdUnix ?? null)}</dd>
        </div>
        <div>
          <dt>修改</dt>
          <dd>{formatPreviewTimestamp(meta?.modifiedUnix ?? null)}</dd>
        </div>
      </dl>
    </div>
  );
}

export function PreviewPane({ path, isDirectory, className }: Props) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  /** 原生预览容器需要占位的 div：Rust 会把 STATIC 子窗口移到它的客户区位置。 */
  const nativeMountRef = useRef<HTMLDivElement | null>(null);
  const [imgUrl, setImgUrl] = useState<string | null>(null);
  const [textBody, setTextBody] = useState<string | null>(null);
  const [errMsg, setErrMsg] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [fallbackMeta, setFallbackMeta] = useState<PreviewFallbackFileInfo | null>(null);

  const mode: PreviewMode = useMemo(() => classifyByExt(path ?? "", isDirectory), [path, isDirectory]);
  const cloudLabel = useMemo(() => (path ? cloudStorageLabel(path) : null), [path]);

  /** 拖动分隔条时 `preview_set_bounds` 极高频；合并到下一帧，减少与 prevhost 的交错重入导致的花屏/白块。 */
  const boundsRafRef = useRef<number | null>(null);

  // -------- 原生预览：把 nativeMountRef 的矩形发给 Rust（CSS 逻辑像素，与 getBoundingClientRect 一致）--------
  // 物理像素与 DPI 换算在 Rust 侧用 `GetDpiForWindow(实际 WebView HWND)` 完成，避免与扩展屏/混用 DPI 下的
  // `window.devicePixelRatio` 或 Tauri scaleFactor 与 Win32 `ClientToScreen` 不一致。
  const pushBounds = useCallback(() => {
    const el = nativeMountRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) return;
    // dpr 是 webview 实际使用的 device-pixel-ratio——这是 CSS→物理像素 唯一权威值。
    // 由前端报告而不是 Rust 用 GetDpiForWindow 猜，避免跨屏 / 启动瞬间监视器 DPI 与 webview dpr 不一致。
    void invoke("preview_set_bounds", {
      x: r.left,
      y: r.top,
      w: r.width,
      h: r.height,
      dpr: window.devicePixelRatio || 1,
    }).catch(() => {});
  }, []);

  const schedulePushBounds = useCallback(() => {
    if (boundsRafRef.current != null) cancelAnimationFrame(boundsRafRef.current);
    boundsRafRef.current = requestAnimationFrame(() => {
      boundsRafRef.current = null;
      pushBounds();
    });
  }, [pushBounds]);

  // -------- 路径或模式变化 → 切换预览源 --------
  // useLayoutEffect 保证 DOM commit 之后、浏览器绘制之前跑：mount div 一定已经 attach，ref 就绪。
  useLayoutEffect(() => {
    let alive = true;
    setErrMsg(null);
    setImgUrl(null);
    setTextBody(null);

    if (!path || mode === "none") {
      void invoke("preview_hide", { unload: true }).catch(() => {});
      return;
    }

    if (mode === "image") {
      void invoke("preview_hide", { unload: true }).catch(() => {});
      setBusy(true);
      invoke<string>("load_preview_data_url", { path })
        .then((url) => {
          if (alive) setImgUrl(url);
        })
        .catch((e) => {
          if (alive) setErrMsg(String(e));
        })
        .finally(() => alive && setBusy(false));
      return;
    }

    if (mode === "text") {
      void invoke("preview_hide", { unload: true }).catch(() => {});
      setBusy(true);
      invoke<string>("load_preview_text", { path, maxBytes: 64 * 1024 })
        .then((body) => {
          if (alive) setTextBody(body);
        })
        .catch((e) => {
          if (alive) setErrMsg(String(e));
        })
        .finally(() => alive && setBusy(false));
      return;
    }

    // native：先 250ms 防抖（用户快速划列表时不要每次都启 prevhost），再用 rAF 稳一帧后读矩形。
    // useLayoutEffect 已能拿到 ref，但有时 layout 尺寸还未稳定（父级 grid/flex），
    // 用 rAF 稳一帧并加重试，避免发出 0×0 矩形导致 prevhost 渲染到不可见区域。
    setBusy(true);
    const debounce = window.setTimeout(() => {
      if (!alive) return;
      let attempts = 0;
      const tryShow = async () => {
        if (!alive) return;
        const el = nativeMountRef.current;
        if (!el) {
          if (attempts++ < 10) requestAnimationFrame(() => void tryShow());
          else if (alive) {
            setBusy(false);
            setErrMsg("预览容器未就绪");
          }
          return;
        }
        const r = el.getBoundingClientRect();
        if (r.width < 8 || r.height < 8) {
          if (attempts++ < 10) {
            requestAnimationFrame(() => void tryShow());
            return;
          }
        }
        const args = {
          path,
          x: r.left,
          y: r.top,
          w: Math.max(1, r.width),
          h: Math.max(1, r.height),
          dpr: window.devicePixelRatio || 1,
        };
        await doubleRaf();
        if (!alive) return;
        try {
          await invoke("preview_show", args);
          if (alive) setErrMsg(null);
        } catch (e) {
          if (alive) {
            setErrMsg(String(e));
            void invoke("preview_hide", { unload: true }).catch(() => {});
          }
        } finally {
          if (alive) setBusy(false);
        }
      };
      requestAnimationFrame(() => void tryShow());
    }, 250);

    return () => {
      alive = false;
      window.clearTimeout(debounce);
    };
  }, [path, mode]);

  // -------- 面板尺寸 / 滚动 / 主窗口 resize / 主窗口移动时跟随 --------
  // 因为预览宿主是独立顶级 WS_POPUP（被 WebView2 DComp 合成层逼出来的唯一可行方案），
  // 所以主窗口移动时 popup 不会自动跟随，必须监听 Tauri onMoved / onResized 重发坐标。
  useEffect(() => {
    if (mode !== "native") return;
    const el = nativeMountRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => schedulePushBounds());
    ro.observe(el);
    const onResize = () => schedulePushBounds();
    window.addEventListener("resize", onResize);
    window.addEventListener("scroll", onResize, true);

    const win = getCurrentWindow();
    let unlistenMoved: (() => void) | null = null;
    let unlistenResized: (() => void) | null = null;
    win.onMoved(() => schedulePushBounds()).then((u) => { unlistenMoved = u; }).catch(() => {});
    win.onResized(() => schedulePushBounds()).then((u) => { unlistenResized = u; }).catch(() => {});
    schedulePushBounds();

    return () => {
      if (boundsRafRef.current != null) cancelAnimationFrame(boundsRafRef.current);
      boundsRafRef.current = null;
      ro.disconnect();
      window.removeEventListener("resize", onResize);
      window.removeEventListener("scroll", onResize, true);
      unlistenMoved?.();
      unlistenResized?.();
    };
  }, [mode, path, schedulePushBounds]);

  // -------- 预览失败：从磁盘读取元数据，用于降级信息卡片 --------
  useEffect(() => {
    if (!path || isDirectory || !errMsg) {
      setFallbackMeta(null);
      return;
    }
    let alive = true;
    invoke<PreviewFallbackFileInfo>("preview_fallback_file_info", { path })
      .then((info) => {
        if (alive) setFallbackMeta(info);
      })
      .catch(() => {
        if (alive) setFallbackMeta(null);
      });
    return () => {
      alive = false;
    };
  }, [path, isDirectory, errMsg]);

  // -------- 组件卸载 → 彻底释放 prevhost --------
  useEffect(() => {
    return () => {
      void invoke("preview_hide", { unload: true }).catch(() => {});
    };
  }, []);

  return (
    <div ref={containerRef} className={`fx-preview ${className ?? ""}`}>
      <div className="fx-preview-header">
        <span className="fx-preview-title" title={path ?? ""}>
          {path ? path.split(/[\\/]/).pop() : "预览"}
        </span>
        {busy && (
          <span className="fx-preview-busy">{cloudLabel ? "云同步中…" : "加载中…"}</span>
        )}
      </div>
      <div className="fx-preview-body">
        {!path && <div className="fx-preview-empty">选中文件以预览</div>}
        {path && isDirectory && <div className="fx-preview-empty">文件夹无可用预览</div>}
        {path && !isDirectory && mode === "image" && imgUrl && (
          <img className="fx-preview-img" src={imgUrl} alt="" />
        )}
        {path && !isDirectory && mode === "text" && textBody !== null && (
          <pre className="fx-preview-text">{textBody}</pre>
        )}
        {path && !isDirectory && mode === "native" && (
          <div className="fx-preview-native-stack">
            {/* mount 仅在 native 栈内挂载，Rust 把 HWND 对齐到此矩形 */}
            <div ref={nativeMountRef} className="fx-preview-native-mount" />
            {busy && (
              <div className="fx-preview-native-overlay" aria-live="polite">
                <span className="fx-preview-spin" aria-hidden />
                <div className="fx-preview-native-overlay-text">
                  {cloudLabel ? (
                    <>
                      <span className="fx-preview-native-overlay-title">{cloudLabel}</span>
                      <span className="fx-preview-native-overlay-desc">
                        正在将文件同步到本地，完成后自动加载系统预览；大文件请稍候。
                      </span>
                    </>
                  ) : (
                    <>
                      <span className="fx-preview-native-overlay-title">预览</span>
                      <span className="fx-preview-native-overlay-desc">正在加载系统预览…</span>
                    </>
                  )}
                </div>
              </div>
            )}
            {errMsg && path && (
              <div className="fx-preview-fallback-host fx-preview-fallback-host--native">
                <PreviewFallbackCard path={path} reason={errMsg} meta={fallbackMeta} />
              </div>
            )}
          </div>
        )}

        {errMsg && path && mode !== "native" && (
          <div className="fx-preview-fallback-host">
            <PreviewFallbackCard path={path} reason={errMsg} meta={fallbackMeta} />
          </div>
        )}
      </div>
    </div>
  );
}
