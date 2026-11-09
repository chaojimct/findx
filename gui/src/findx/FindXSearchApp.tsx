import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import type { ReactNode } from "react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import "./design-tokens.css";
import "./findx.css";
import type { AppUpdateInfo, FindxGuiSettings, UiThemePref } from "./findxGuiTypes";
import { UI_THEME_KEY, loadUiThemePref } from "./findxGuiTypes";
import { BoundedTip } from "./BoundedTip";
import { summarizeIndexStatus } from "./indexStatus";
import { IconHistory, IconPreview, IconSettings, IconSidebar } from "./icons";
import { PreviewPane } from "./PreviewPane";
import {
  applyColumnDragBetween,
  COL_INDEX,
  COL_LABEL,
  COLUMN_HEADER_MIN_PX,
  columnPixelWidths,
  PREVIEW_AUTO_HIDE_BELOW,
  resolvePanels,
  RESULT_ROW_HEIGHT_PX,
  SIDEBAR_AUTO_HIDE_BELOW,
  visibleColumnIds,
  type ColId,
} from "./resultTableLayout";
import { ResultNavigation } from "./resultNavigation";
import { scrollTopForRow } from "./resultSelection";
import { windowThemeColors } from "./themeColors";

/** 预览面板宽度（px）持久化 key；与 fx-preview 默认 320 配套。 */
const PREVIEW_WIDTH_KEY = "findx2_preview_width";
const PREVIEW_OPEN_KEY = "findx2_preview_open";
const PREVIEW_MIN_WIDTH = 240;
const PREVIEW_MAX_WIDTH = 720;
const SIDEBAR_OPEN_KEY = "findx2_sidebar_open";

type SearchHit = {
  name: string;
  path: string;
  extension: string;
  size: number;
  modifiedUnix: number;
  isDirectory: boolean;
  /** 与 findx2-core 搜索一致的文件名高亮区间 [start,end)，Unicode 标量下标 */
  nameHighlight?: [number, number][];
};

type TypeFilter = "all" | "folder" | "file" | "doc" | "image" | "video" | "audio";
type TimeFilter = "all" | "1d" | "7d" | "30d" | "365d";
type SortCol = "name" | "path" | "size" | "mtime" | null;

const HISTORY_KEY = "findx2_search_history";
const MAX_HISTORY = 30;
const COL_FRAC_KEY = "findx2_col_fractions";
const V2_MIGRATION_NOTICE_ACK_KEY = "findx2_v2_migration_notice_ack_v1";
const UPDATE_CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000;
const UPDATE_LAST_CHECK_LS = "findx2_last_update_check_ms";
const UPDATE_BANNER_DISMISS_LS = "findx2_update_banner_dismissed_for_tag";

const DEFAULT_COL_FRAC: [number, number, number, number] = [0.28, 0.38, 0.1, 0.24];

function loadColFracs(): [number, number, number, number] {
  try {
    const raw = localStorage.getItem(COL_FRAC_KEY);
    if (!raw) return [...DEFAULT_COL_FRAC];
    const a = JSON.parse(raw) as number[];
    if (
      Array.isArray(a) &&
      a.length === 4 &&
      a.every((x) => typeof x === "number" && x > 0) &&
      Math.abs(a.reduce((s, x) => s + x, 0) - 1) < 0.02
    ) {
      const s = a.reduce((x, y) => x + y, 0);
      const n = a.map((x) => x / s) as [number, number, number, number];
      return n;
    }
  } catch {
    /* ignore */
  }
  return [...DEFAULT_COL_FRAC];
}

function buildSidebarFilters(pathPrefix: string, sizeMinMb: string, sizeMaxMb: string): string {
  const parts: string[] = [];
  const pathText = pathPrefix.trim();
  if (pathText) parts.push(`path:"${pathText}"`);
  const smin = sizeMinMb.trim();
  const smax = sizeMaxMb.trim();
  if (smin && smax) parts.push(`size:${smin}mb..${smax}mb`);
  else if (smin) parts.push(`size:>=${smin}mb`);
  else if (smax) parts.push(`size:<=${smax}mb`);
  return parts.join(" ");
}

function typeFilterQuery(t: TypeFilter): string {
  switch (t) {
    case "folder":
      return "folder:";
    case "file":
      return "file:";
    case "doc":
      return "ext:doc;docx;pdf;xls;xlsx;ppt;pptx;txt;rtf;odt;csv;md";
    case "image":
      return "ext:jpg;jpeg;png;gif;bmp;svg;webp;ico;tiff;psd;raw";
    case "video":
      return "ext:mp4;avi;mkv;mov;wmv;flv;webm;m4v;ts";
    case "audio":
      return "ext:mp3;wav;flac;aac;ogg;wma;m4a;opus";
    default:
      return "";
  }
}

/** 本地日历日 yyyy-MM-dd，避免 toISOString() UTC 导致日期偏移一天 */
function formatLocalYmd(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

function timeFilterQuery(t: TimeFilter): string {
  if (t === "all") return "";
  const today = new Date();
  const d = new Date(today);
  if (t === "1d") d.setDate(d.getDate() - 1);
  else if (t === "7d") d.setDate(d.getDate() - 7);
  else if (t === "30d") d.setDate(d.getDate() - 30);
  else if (t === "365d") d.setDate(d.getDate() - 365);
  return `dm:>${formatLocalYmd(d)}`;
}

/** 修饰符必须排在裸词之前，否则 `try_parse_func` 会按「第一个冒号」切片，导致 `关键词 folder:` 解析失败。 */
function buildFullQuery(
  raw: string,
  typeF: TypeFilter,
  timeF: TimeFilter,
  pathPrefix: string,
  sizeMin: string,
  sizeMax: string,
): string {
  const parts: string[] = [];
  const tf = typeFilterQuery(typeF);
  if (tf) parts.push(tf);
  const tm = timeFilterQuery(timeF);
  if (tm) parts.push(tm);
  const side = buildSidebarFilters(pathPrefix, sizeMin, sizeMax);
  if (side) parts.push(side);
  const r = raw.trim();
  if (r) parts.push(r);
  return parts.join(" ");
}

function formatSize(n: number, isDir: boolean): string {
  if (isDir) return "";
  if (n < 1024) return `${n} B`;
  let v = n;
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return i === 0 ? `${n} B` : `${v.toFixed(v >= 10 || i === 1 ? 0 : 2)} ${u[i]}`;
}

function formatTime(unix: number): string {
  if (!unix) return "—";
  const d = new Date(unix * 1000);
  const pad = (x: number) => x.toString().padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function mergeHalfOpenRanges(ranges: [number, number][]): [number, number][] {
  if (ranges.length === 0) return [];
  const sorted = [...ranges].sort((a, b) => a[0] - b[0]);
  const out: [number, number][] = [];
  let [cs, ce] = sorted[0];
  for (let k = 1; k < sorted.length; k++) {
    const [s, e] = sorted[k];
    if (s <= ce) ce = Math.max(ce, e);
    else {
      out.push([cs, ce]);
      cs = s;
      ce = e;
    }
  }
  out.push([cs, ce]);
  return out;
}

function mergeCharRanges(ranges: [number, number][]): [number, number][] {
  if (ranges.length === 0) return [];
  const sorted = [...ranges].sort((a, b) => a[0] - b[0]);
  const out: [number, number][] = [];
  let [cs, ce] = sorted[0];
  for (let k = 1; k < sorted.length; k++) {
    const [s, e] = sorted[k];
    if (s <= ce) ce = Math.max(ce, e);
    else {
      out.push([cs, ce]);
      cs = s;
      ce = e;
    }
  }
  out.push([cs, ce]);
  return out;
}

/**
 * 优先使用服务端 `name_highlight`（与搜索层 ib_matcher 一致）；
 * 无则回退为搜索框分词字面高亮（路径列等）。
 */
function HighlightText({
  text,
  qRaw,
  serverRanges,
}: {
  text: string;
  qRaw: string;
  serverRanges?: [number, number][];
}) {
  if (serverRanges && serverRanges.length > 0) {
    const chars = [...text];
    const merged = mergeCharRanges(serverRanges);
    const nodes: ReactNode[] = [];
    let last = 0;
    let key = 0;
    for (const [s, e] of merged) {
      const ss = Math.max(0, s);
      const ee = Math.min(chars.length, e);
      if (ss > last) nodes.push(chars.slice(last, ss).join(""));
      nodes.push(
        <mark key={`hit-${key++}`} className="fx-hit">
          {chars.slice(ss, ee).join("")}
        </mark>,
      );
      last = ee;
    }
    if (last < chars.length) nodes.push(chars.slice(last).join(""));
    return <>{nodes}</>;
  }

  const tokens = [...new Set(qRaw.trim().split(/\s+/).filter(Boolean))];
  if (tokens.length === 0) return <>{text}</>;

  const ranges: [number, number][] = [];
  for (const token of tokens) {
    const re = new RegExp(escapeRegExp(token), "gi");
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) {
      ranges.push([m.index, m.index + m[0].length]);
    }
  }

  const merged = mergeHalfOpenRanges(ranges);
  if (merged.length === 0) return <>{text}</>;

  const nodes: ReactNode[] = [];
  let last = 0;
  let key = 0;
  for (const [s, e] of merged) {
    if (s > last) nodes.push(text.slice(last, s));
    nodes.push(
      <mark key={`hit-${key++}`} className="fx-hit">
        {text.slice(s, e)}
      </mark>,
    );
    last = e;
  }
  if (last < text.length) nodes.push(text.slice(last));
  return <>{nodes}</>;
}

export default function FindXSearchApp() {
  const [q, setQ] = useState("");
  const [typeF, setTypeF] = useState<TypeFilter>("all");
  const [timeF, setTimeF] = useState<TimeFilter>("all");
  const [pathPrefix, setPathPrefix] = useState("");
  const [sizeMin, setSizeMin] = useState("");
  const [sizeMax, setSizeMax] = useState("");
  const [hits, setHits] = useState<SearchHit[]>([]);
  // 服务端匹配总数（分页时 > hits.length；无限滚动用它判断还有没有下一页）。
  const [matchTotal, setMatchTotal] = useState(0);
  // 翻页请求单 flight 守卫（与 searchSeq 配合：新查询会让旧翻页回包作废）。
  const loadingMoreRef = useRef(false);
  const [loading, setLoading] = useState(false);
  const [status, setStatus] = useState("就绪");
  const [indexSummary, setIndexSummary] = useState("索引 …");
  const [indexDetail, setIndexDetail] = useState<string | null>(null);
  const [showIndexDetail, setShowIndexDetail] = useState(false);
  const [selected, setSelected] = useState<number | null>(null);
  // 预览面板状态：默认开启 + 持久化宽度
  const [previewOpen, setPreviewOpen] = useState<boolean>(() => {
    try {
      const v = localStorage.getItem(PREVIEW_OPEN_KEY);
      return v === null ? true : v === "1";
    } catch {
      return true;
    }
  });
  const [previewWidth, setPreviewWidth] = useState<number>(() => {
    try {
      const v = Number(localStorage.getItem(PREVIEW_WIDTH_KEY));
      if (Number.isFinite(v) && v >= PREVIEW_MIN_WIDTH && v <= PREVIEW_MAX_WIDTH) return v;
    } catch {
      /* ignore */
    }
    return 360;
  });
  const [sidebarOpen, setSidebarOpen] = useState<boolean>(() => {
    try {
      const v = localStorage.getItem(SIDEBAR_OPEN_KEY);
      return v === null ? true : v === "1";
    } catch {
      return true;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(PREVIEW_OPEN_KEY, previewOpen ? "1" : "0");
    } catch {
      /* ignore */
    }
  }, [previewOpen]);
  useEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_OPEN_KEY, sidebarOpen ? "1" : "0");
    } catch {
      /* ignore */
    }
  }, [sidebarOpen]);
  useEffect(() => {
    try {
      localStorage.setItem(PREVIEW_WIDTH_KEY, String(previewWidth));
    } catch {
      /* ignore */
    }
  }, [previewWidth]);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [contentWidth, setContentWidth] = useState(() =>
    typeof document === "undefined" ? 0 : document.documentElement.clientWidth,
  );
  const [sidebarForce, setSidebarForce] = useState<"show" | "hide" | null>(null);
  const [previewForce, setPreviewForce] = useState<"show" | "hide" | null>(null);
  const prevContentWidth = useRef(contentWidth);
  useLayoutEffect(() => {
    const root = rootRef.current;
    const read = () => {
      const rootW = root?.clientWidth ?? 0;
      const docW = document.documentElement.clientWidth;
      const next = rootW > 0 && docW > 0 ? Math.min(rootW, docW) : rootW || docW;
      setContentWidth(next);
    };
    read();
    const ro = new ResizeObserver(read);
    if (root) ro.observe(root);
    ro.observe(document.documentElement);
    return () => ro.disconnect();
  }, []);
  useEffect(() => {
    const prev = prevContentWidth.current;
    if (prev < PREVIEW_AUTO_HIDE_BELOW && contentWidth >= PREVIEW_AUTO_HIDE_BELOW) {
      setPreviewForce(null);
    }
    if (prev < SIDEBAR_AUTO_HIDE_BELOW && contentWidth >= SIDEBAR_AUTO_HIDE_BELOW) {
      setSidebarForce(null);
    }
    prevContentWidth.current = contentWidth;
  }, [contentWidth]);
  const panels = resolvePanels({
    contentWidth,
    previewWidth,
    savedSidebar: sidebarOpen,
    savedPreview: previewOpen,
    sidebarForce,
    previewForce,
  });
  const sidebarShown = panels.sidebar;
  const previewShown = panels.preview;
  const [sortCol, setSortCol] = useState<SortCol>(null);
  const [sortAsc, setSortAsc] = useState(true);
  const [histOpen, setHistOpen] = useState(false);
  const [history, setHistory] = useState<string[]>([]);
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
  const [showV2MigrationNotice, setShowV2MigrationNotice] = useState(false);
  /** GitHub Release 有新版本时顶部提示条 */
  const [updateBanner, setUpdateBanner] = useState<AppUpdateInfo | null>(null);
  /** 界面主题：浅 / 深 / 跟随系统。深色是分层深灰。 */
  const [uiThemePref, setUiThemePref] = useState<UiThemePref>(() => loadUiThemePref());
  const [systemDark, setSystemDark] = useState(
    () => window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  /** 列表列宽比例（4 列之和恒为 1），持久化，表头可拖拽分隔条调节 */
  const [colFrac, setColFrac] = useState<[number, number, number, number]>(() => loadColFracs());
  const colFracRef = useRef(colFrac);
  colFracRef.current = colFrac;
  const colResizeRef = useRef<{ i: number; right: number; startX: number; base: number[] } | null>(null);

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

  /** 设置窗口在另一 Webview 中改主题时，通过 storage 事件同步主窗 UI */
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === UI_THEME_KEY) {
        setUiThemePref(loadUiThemePref());
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  /** 与页面 data-fx-theme 一致，同步 WebView2 / Windows 标题栏与窗口背景 */
  useEffect(() => {
    const isDark = effectiveTheme === "dark";
    const colors = windowThemeColors(isDark);
    void invoke("sync_window_theme_command", {
      themeMode: isDark ? "dark" : "light",
      backgroundColor: colors.backgroundColor,
      titleBarColor: colors.titleBarColor,
      titleBarTextColor: colors.titleBarTextColor,
    }).catch(() => {
      /* 浏览器预览或非 Tauri 环境 */
    });
  }, [effectiveTheme]);

  /** 历史下拉：点击搜索行外关闭 */
  useEffect(() => {
    if (!histOpen) return;
    const onDown = (e: MouseEvent) => {
      if (searchBarRowRef.current?.contains(e.target as Node)) return;
      setHistOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [histOpen]);

  const onColResizeStart = useCallback((leftIdx: number, rightIdx: number, e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    colResizeRef.current = {
      i: leftIdx,
      right: rightIdx,
      startX: e.clientX,
      base: [...colFracRef.current],
    };
    const onMove = (ev: MouseEvent) => {
      const st = colResizeRef.current;
      const wrap = tableScrollRef.current;
      if (!st || !wrap) return;
      const tw = wrap.clientWidth;
      if (tw <= 0) return;
      const deltaPx = ev.clientX - st.startX;
      const next = applyColumnDragBetween(
        st.base as [number, number, number, number],
        st.i,
        st.right,
        deltaPx,
        tw,
      );
      colFracRef.current = next;
      setColFrac(next);
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      colResizeRef.current = null;
      try {
        localStorage.setItem(COL_FRAC_KEY, JSON.stringify(colFracRef.current));
      } catch {
        /* ignore */
      }
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  }, []);

  const [indexMeta, setIndexMeta] = useState({
    indexing: false,
    metadataReady: true,
  });

  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchSeq = useRef(0);

  // === 虚拟滚动状态 ===
  // 固定行高保证 5000+ 条命中也能 60fps 滚动；不上 react-window 是因为 GUI 已经塞满了，
  // 自实现 30 行就够：监听 scrollTop / clientHeight 推算可见 [start,end)，前后用 spacer <tr> 占位。
  const tableScrollRef = useRef<HTMLDivElement | null>(null);
  const searchBarRowRef = useRef<HTMLDivElement | null>(null);
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const ROW_HEIGHT = RESULT_ROW_HEIGHT_PX;
  const OVERSCAN = 8;
  // 分页大小：首屏 + 滚动预取。服务端排序后按 [offset, offset+PAGE_SIZE) 切片；
  // 滚动接近底部自动追加（无限滚动），searchLimit 作为自动加载总上限。
  const PAGE_SIZE = 250;
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(600);
  const [tableWidth, setTableWidth] = useState(800);
  useEffect(() => {
    const el = tableScrollRef.current;
    if (!el) return;
    const onScroll = () => setScrollTop(el.scrollTop);
    const update = () => {
      setViewportH(el.clientHeight);
      setTableWidth(el.clientWidth);
    };
    update();
    el.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => {
      el.removeEventListener("scroll", onScroll);
      ro.disconnect();
    };
  }, []);

  /** Ctrl+F / Cmd+F：聚焦主检索框（避免 WebView 默认「在页面中查找」抢焦点） */
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "f" && e.key !== "F") return;
      if (!(e.ctrlKey || e.metaKey) || e.altKey || e.shiftKey) return;
      e.preventDefault();
      e.stopPropagation();
      const input = searchInputRef.current;
      if (!input) return;
      const hadFocus = document.activeElement === input;
      input.focus();
      if (!hadFocus) input.select();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, []);

  /** 启动数秒后检查 GitHub 最新 Release（每 24h 最多请求一次） */
  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      try {
        const raw = localStorage.getItem(UPDATE_LAST_CHECK_LS);
        if (raw) {
          const last = parseInt(raw, 10);
          if (Number.isFinite(last) && Date.now() - last < UPDATE_CHECK_INTERVAL_MS) {
            return;
          }
        }
      } catch {
        /* ignore */
      }
      // 设置开关 autoCheckUpdate（默认开）：关掉后启动不自动检查（手动检查仍可用）。
      try {
        const s = await invoke<FindxGuiSettings>("load_findx_settings");
        if (s.autoCheckUpdate === false) return;
      } catch {
        /* 读取失败按默认开启继续 */
      }
      try {
        const info = await invoke<AppUpdateInfo>("check_app_update");
        if (cancelled) return;
        try {
          localStorage.setItem(UPDATE_LAST_CHECK_LS, String(Date.now()));
        } catch {
          /* ignore */
        }
        if (!info.ok || !info.hasUpdate || !info.latestVersion) return;
        try {
          if (localStorage.getItem(UPDATE_BANNER_DISMISS_LS) === info.latestVersion) return;
        } catch {
          /* ignore */
        }
        setUpdateBanner(info);
      } catch {
        /* ignore */
      }
    };
    const tid = window.setTimeout(() => void run(), 3500);
    return () => {
      cancelled = true;
      window.clearTimeout(tid);
    };
  }, []);

  const fullQuery = useMemo(
    () => buildFullQuery(q, typeF, timeF, pathPrefix, sizeMin, sizeMax),
    [q, typeF, timeF, pathPrefix, sizeMin, sizeMax],
  );

  const loadSettings = useCallback(async () => {
    try {
      const s = await invoke<FindxGuiSettings>("load_findx_settings");
      setSettings(s);
    } catch {
      /* ignore */
    }
  }, []);

  const refreshIndexStatus = useCallback(async () => {
    try {
      const st = await invoke<{
        indexing: boolean;
        ready: boolean;
        indexedCount: number;
        metadataReady?: boolean;
        backfillDone?: number;
        backfillTotal?: number;
        indexingPhase?: string;
        indexingVolumesTotal?: number;
        indexingVolumesDone?: number;
        indexingMessage?: string;
        indexingEntriesIndexed?: number;
        indexingCurrentVolume?: string;
        /** service 侧 index.bin 加载阶段文案（旧版 service 为 undefined） */
        loadingStage?: string;
        loadingElapsedSecs?: number;
        loadingPhaseDone?: number;
        loadingPhaseTotal?: number;
        lastError?: string;
      }>("index_status");
      const raw = st as Record<string, unknown>;
      const indexedCount = Number(
        st.indexedCount ?? (typeof raw.indexed_count === "number" ? raw.indexed_count : 0),
      );
      setIndexMeta({
        indexing: st.indexing === true,
        metadataReady: st.metadataReady !== false,
      });
      const entries =
        st.indexingEntriesIndexed ??
        (typeof raw.indexing_entries_indexed === "number" ? raw.indexing_entries_indexed : undefined);
      const view = summarizeIndexStatus({
        indexing: st.indexing === true,
        indexedCount,
        metadataReady: st.metadataReady,
        backfillDone: st.backfillDone,
        backfillTotal: st.backfillTotal,
        indexingPhase: st.indexingPhase,
        indexingVolumesTotal: st.indexingVolumesTotal,
        indexingVolumesDone: st.indexingVolumesDone,
        indexingMessage: st.indexingMessage,
        indexingEntriesIndexed: entries,
        indexingCurrentVolume: st.indexingCurrentVolume,
        loadingStage: st.loadingStage,
        loadingElapsedSecs: st.loadingElapsedSecs,
        lastError: st.lastError,
        enableMetadataBackfill: settings.enableMetadataBackfill ?? true,
      });
      setIndexSummary(view.summary);
      setIndexDetail(view.detail);
    } catch (e) {
      const view = summarizeIndexStatus({ lastError: String(e), indexedCount: 0 });
      setIndexSummary(view.summary);
      setIndexDetail(view.detail);
    }
  }, [settings.enableMetadataBackfill]);

  useEffect(() => {
    try {
      const raw = localStorage.getItem(HISTORY_KEY);
      if (raw) setHistory(JSON.parse(raw) as string[]);
    } catch {
      /* ignore */
    }
    void (async () => {
      let acknowledged = false;
      try {
        acknowledged = localStorage.getItem(V2_MIGRATION_NOTICE_ACK_KEY) === "1";
      } catch {
        acknowledged = false;
      }
      if (acknowledged) {
        setShowV2MigrationNotice(false);
        return;
      }
      try {
        const hasLegacyV1 = await invoke<boolean>("detect_legacy_v1_installation");
        setShowV2MigrationNotice(hasLegacyV1 === true);
      } catch {
        setShowV2MigrationNotice(false);
      }
    })();
    void loadSettings();
    void refreshIndexStatus();
  }, [loadSettings, refreshIndexStatus]);

  useEffect(() => {
    const fast = indexMeta.indexing || !indexMeta.metadataReady;
    const ms = fast ? 350 : 800;
    const t = window.setInterval(() => void refreshIndexStatus(), ms);
    return () => window.clearInterval(t);
  }, [indexMeta.indexing, indexMeta.metadataReady, refreshIndexStatus]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen("findx2-settings-saved", () => {
      void loadSettings();
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [loadSettings]);

  const runSearch = useCallback(async (offset: number, append: boolean) => {
    const query = fullQuery.trim();
    if (!query) {
      setHits([]);
      setMatchTotal(0);
      setStatus("就绪");
      setSelected(null);
      return;
    }
    const seq = ++searchSeq.current;
    if (!append) {
      setLoading(true);
      setStatus("搜索中…");
      // 每次重新搜索（含防抖自动搜）时清空列表选中，避免仍指向旧排序/旧结果中的行号。
      setSelected(null);
    }
    try {
      // 表头排序走服务端（`sort:` 修饰），保证分页全局有序；
      // 不再前端排序（分页下前端只能看到子集，排出来是错的）。
      // 服务端默认 Name 升序，与 sortCol=null 时的历史行为一致。
      // 注意：查询语言要求修饰符在裸词之前（`test sort:path` 会解析失败），
      // 所以拼在最前面；用户手写的 sort: 会被表头选择覆盖（去掉再拼）。
      const sortToken =
        sortCol === null
          ? ""
          : `sort:${sortCol === "mtime" ? "modified" : sortCol}${sortAsc ? "" : ":desc"}`;
      const serverQuery = sortToken
        ? `${sortToken} ${query.replace(/(^|\s)sort:\S+("[^"]*")?/g, "$1").trim().replace(/\s+/g, " ")}`
        : query;
      const resp = await invoke<{ hits: SearchHit[]; total: number; elapsedMs: number }>(
        "search_files",
        {
          query: serverQuery,
          pinyin: settings.pinyinDefault,
          limit: PAGE_SIZE,
          offset,
        },
      );
      if (seq !== searchSeq.current) return;
      const rows = resp.hits;
      const total = Number.isFinite(resp.total) ? resp.total : rows.length;
      const shown = (append ? hits.length : 0) + rows.length;
      if (append) {
        setHits((prev) => [...prev, ...rows]);
      } else {
        setHits(rows);
        // 新查询回到顶部（虚拟列表按 scrollTop 切片，不重置会停在旧位置）。
        const el = tableScrollRef.current;
        if (el) el.scrollTop = 0;
        setScrollTop(0);
      }
      setMatchTotal(total);
      const ms = Number.isFinite(resp.elapsedMs) ? resp.elapsedMs : 0;
      // 与 Everything 左下角语义一致：「匹配 N 条」是真实总数；已显示是已加载页累计。
      const ceiling = settings.searchLimit > 0 ? settings.searchLimit : Number.MAX_SAFE_INTEGER;
      const hasMore = shown < total && shown < ceiling;
      const head =
        total === 0
          ? "无匹配"
          : shown >= total || !hasMore
            ? `匹配 ${total.toLocaleString()} 条`
            : `匹配 ${total.toLocaleString()} 条 · 已显示 ${shown.toLocaleString()} 条 · 下滑加载更多`;
      const capped = shown < total && !hasMore && shown >= ceiling;
      setStatus(ms > 0 ? `${head}${capped ? " · 已达加载上限" : ""} · ${ms} ms` : head);
      const rawOnly = q.trim();
      if (rawOnly && !append) {
        setHistory((prev) => {
          const next = [rawOnly, ...prev.filter((x) => x !== rawOnly)].slice(0, MAX_HISTORY);
          localStorage.setItem(HISTORY_KEY, JSON.stringify(next));
          return next;
        });
      }
    } catch (e) {
      if (seq === searchSeq.current) {
        if (!append) {
          setHits([]);
          setMatchTotal(0);
          setSelected(null);
        }
        setStatus(`错误: ${String(e)}`);
      }
    } finally {
      if (seq === searchSeq.current && !append) setLoading(false);
    }
  }, [fullQuery, q, settings.pinyinDefault, settings.searchLimit, sortCol, sortAsc]);

  useEffect(() => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    // 搜索延迟已到亚毫秒级（trigram 剪枝后 0.3–3ms），250ms 是为慢全表扫描设计的；
    // 60ms 既跟手又能在连续击键间合并掉中间态（Everything 同类即时刷新手感）。
    debounceRef.current = setTimeout(() => {
      void runSearch(0, false);
    }, 60);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [fullQuery, runSearch]);

  // 无限滚动：接近底部且服务端还有更多时自动取下一页（同 query，offset=hits.length）。
  // 服务端按 revision 缓存无序命中，翻页只做 top-(offset+PAGE) 选择 + 当页构建，不重扫。
  useEffect(() => {
    const ceiling = settings.searchLimit > 0 ? settings.searchLimit : Number.MAX_SAFE_INTEGER;
    const hasMore = hits.length < matchTotal && hits.length < ceiling;
    if (!hasMore || loadingMoreRef.current || hits.length === 0) return;
    const totalH = hits.length * ROW_HEIGHT;
    if (scrollTop + viewportH > totalH - ROW_HEIGHT * 24) {
      loadingMoreRef.current = true;
      void runSearch(hits.length, true).finally(() => {
        loadingMoreRef.current = false;
      });
    }
  }, [scrollTop, viewportH, hits, matchTotal, settings.searchLimit, runSearch]);

  // 排序走服务端（runSearch 里拼 `sort:` 修饰），此处直接用服务端顺序；
  // 分页下前端只能看到子集，前端再排会破坏全局有序，故不再前端排序。
  // （行为变化：点击列头 name 升序原来是前端 toLowerCase 排，现在是服务端字节序，
  // 与未点击时的默认 Name 排序一致。）
  const sortedHits = hits;

  const onHeaderClick = (col: SortCol) => {
    if (col === null) return;
    if (sortCol === col) setSortAsc(!sortAsc);
    else {
      setSortCol(col);
      setSortAsc(true);
    }
  };

  const openSelected = async () => {
    if (selected === null) return;
    const row = sortedHits[selected];
    if (!row) return;
    try {
      await invoke("open_file", { path: row.path });
    } catch (e) {
      setStatus(String(e));
    }
  };

  const revealSelected = async () => {
    if (selected === null) return;
    const row = sortedHits[selected];
    if (!row) return;
    try {
      await invoke("reveal_in_folder", { path: row.path });
    } catch (e) {
      setStatus(String(e));
    }
  };

  useEffect(() => {
    if (selected == null) return;
    const el = tableScrollRef.current;
    if (!el) return;
    const next = scrollTopForRow(selected, ROW_HEIGHT, el.scrollTop, el.clientHeight);
    if (next !== el.scrollTop) el.scrollTop = next;
  }, [selected, ROW_HEIGHT]);

  const acknowledgeV2MigrationNotice = () => {
    try {
      localStorage.setItem(V2_MIGRATION_NOTICE_ACK_KEY, "1");
    } catch {
      /* ignore */
    }
    setShowV2MigrationNotice(false);
  };

  return (
    <div className="fx-root" ref={rootRef}>
      <ResultNavigation
        count={sortedHits.length}
        selected={selected}
        onSelect={setSelected}
        onOpen={() => void openSelected()}
        searchInputRef={searchInputRef}
      />
      {updateBanner?.releasePageUrl && updateBanner.latestVersion && (
        <div className="fx-update-banner" role="status">
          <span>
            发现新版本 <strong>{updateBanner.latestVersion}</strong>（当前 {updateBanner.currentVersion}）
          </span>
          <span className="fx-update-banner-actions">
            <button
              type="button"
              className="fx-btn-icon fx-btn-search"
              onClick={() => {
                // 跳设置页「高级」标签完成下载/安装（带进度条与静默安装），不再直接跳浏览器。
                void emit("findx2-open-settings-tab", { tab: "advanced" })
                  .catch(() => {})
                  .then(() => invoke("show_settings_window"))
                  .catch(() => {});
              }}
            >
              去更新
            </button>
            <button
              type="button"
              className="fx-btn-icon"
              onClick={() => {
                try {
                  localStorage.setItem(UPDATE_BANNER_DISMISS_LS, updateBanner.latestVersion ?? "");
                } catch {
                  /* ignore */
                }
                setUpdateBanner(null);
              }}
            >
              忽略
            </button>
          </span>
        </div>
      )}
      <div className="fx-search-bar">
        <div className="fx-search-row" ref={searchBarRowRef}>
          <div className="fx-search-combo">
            <div className="fx-search-input-wrap">
              {!q && <span className="fx-search-placeholder">搜索文件和文件夹…</span>}
              <input
                ref={searchInputRef}
                className="fx-search-input"
                value={q}
                onChange={(e) => setQ(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void runSearch(0, false);
                  if (e.key === "Escape") {
                    if (q) setQ("");
                    else setHistOpen(false);
                  }
                }}
              />
            </div>
            {histOpen && (
              <div className="fx-hist-dropdown" role="listbox" aria-label="搜索历史">
                {history.length === 0 ? (
                  <div className="fx-hist-dropdown-empty-tip">暂无搜索历史</div>
                ) : (
                  history.map((h) => (
                    <button
                      type="button"
                      className="fx-hist-row"
                      key={h}
                      onClick={() => {
                        setQ(h);
                        setHistOpen(false);
                      }}
                    >
                      {h}
                    </button>
                  ))
                )}
              </div>
            )}
          </div>
          <button
            type="button"
            className="fx-btn-icon"
            title="搜索历史"
            aria-expanded={histOpen}
            aria-label="搜索历史"
            onClick={() => setHistOpen(!histOpen)}
          >
            <IconHistory />
          </button>
          <button type="button" className="fx-btn-icon fx-btn-search" onClick={() => void runSearch(0, false)}>
            搜索
          </button>
        </div>
      </div>

      <div className="fx-main">
        {sidebarShown && <aside className="fx-sidebar">
          <h3>筛选</h3>
          {(
            [
              ["all", "全部"],
              ["folder", "文件夹"],
              ["file", "文件"],
              ["doc", "文档"],
              ["image", "图片"],
              ["video", "视频"],
              ["audio", "音频"],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              className={`fx-filter ${typeF === id ? "active" : ""}`}
              onClick={() => setTypeF(id)}
            >
              {label}
            </button>
          ))}
          <div className="fx-sep" />
          <h3>最近修改</h3>
          {(
            [
              ["all", "全部"],
              ["1d", "最近 1 天"],
              ["7d", "最近 7 天"],
              ["30d", "最近 30 天"],
              ["365d", "最近 365 天"],
            ] as const
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              className={`fx-time ${timeF === id ? "active" : ""}`}
              onClick={() => setTimeF(id)}
            >
              <span className="fx-dot" />
              {label}
            </button>
          ))}
          <div className="fx-sep" />
          <h3>路径</h3>
          <input
            className="fx-side-input"
            style={{ margin: "0 8px 8px" }}
            placeholder="路径前缀"
            value={pathPrefix}
            onChange={(e) => setPathPrefix(e.target.value)}
          />
          <div className="fx-sep" />
          <h3>大小 (MB)</h3>
          <div className="fx-size-row">
            <input
              className="fx-side-input"
              placeholder="最小"
              value={sizeMin}
              onChange={(e) => setSizeMin(e.target.value)}
            />
            <span style={{ textAlign: "center", color: "#999" }}>~</span>
            <input
              className="fx-side-input"
              placeholder="最大"
              value={sizeMax}
              onChange={(e) => setSizeMax(e.target.value)}
            />
          </div>
        </aside>}

        <div className="fx-results">
          <div
            className="fx-table-wrap"
            ref={tableScrollRef}
            tabIndex={0}
            onMouseDown={() => tableScrollRef.current?.focus()}
          >
            {(() => {
              // 计算可见窗口；为了避免 5000 行一次性渲染卡死浏览器主线程，仅渲染 [start, end)。
              const total = sortedHits.length;
              const startIdx = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
              const endIdx = Math.min(
                total,
                Math.ceil((scrollTop + viewportH) / ROW_HEIGHT) + OVERSCAN,
              );
              const padTop = startIdx * ROW_HEIGHT;
              const padBottom = Math.max(0, (total - endIdx) * ROW_HEIGHT);
              const slice = sortedHits.slice(startIdx, endIdx);
              const visible = visibleColumnIds(tableWidth);
              const widths = columnPixelWidths(colFrac, tableWidth, visible);
              const tablePx = visible.reduce((sum, id) => sum + widths[id], 0);
              const sortInd = (col: Exclude<SortCol, null>) =>
                sortCol === col ? (
                  <span className="fx-sort-ind" aria-hidden>
                    {sortAsc ? "▲" : "▼"}
                  </span>
                ) : null;
              const cell = (id: ColId, row: SearchHit) => {
                if (id === "name") {
                  return (
                    <td key={id} className="fx-name">
                      {row.isDirectory ? "📁 " : ""}
                      <HighlightText text={row.name} qRaw={q} serverRanges={row.nameHighlight} />
                    </td>
                  );
                }
                if (id === "path") {
                  return (
                    <td key={id} className="fx-path">
                      <BoundedTip text={row.path}>
                        <HighlightText text={row.path} qRaw={q} />
                      </BoundedTip>
                    </td>
                  );
                }
                if (id === "size") {
                  return (
                    <td key={id} className="fx-col-size">
                      {!indexMeta.metadataReady && !row.isDirectory && row.size === 0
                        ? "—"
                        : formatSize(row.size, row.isDirectory)}
                    </td>
                  );
                }
                return (
                  <td key={id}>
                    {!row.isDirectory && row.modifiedUnix === 0 ? "—" : formatTime(row.modifiedUnix)}
                  </td>
                );
              };
              return (
                <table
                  className="fx-table"
                  style={{
                    ["--fx-row-height" as string]: `${ROW_HEIGHT}px`,
                    width: Math.max(tableWidth, tablePx),
                    minWidth: tablePx,
                  }}
                >
                  <colgroup>
                    {visible.map((id) => (
                      <col key={id} style={{ width: widths[id] }} />
                    ))}
                  </colgroup>
                  <thead>
                    <tr>
                      {visible.map((id, index) => {
                        const next = visible[index + 1];
                        return (
                          <th
                            key={id}
                            scope="col"
                            className={id === "size" ? "fx-col-size" : undefined}
                            style={{ minWidth: COLUMN_HEADER_MIN_PX[id] }}
                          >
                            <div className="fx-th-cell">
                              <button type="button" className="fx-th-btn" onClick={() => onHeaderClick(id)}>
                                <span>{COL_LABEL[id]}</span>
                                {sortInd(id)}
                              </button>
                              {next && (
                                <div
                                  className="fx-col-resize"
                                  role="separator"
                                  aria-orientation="vertical"
                                  aria-hidden
                                  onMouseDown={(e) => onColResizeStart(COL_INDEX[id], COL_INDEX[next], e)}
                                />
                              )}
                            </div>
                          </th>
                        );
                      })}
                    </tr>
                  </thead>
                  <tbody>
                    {padTop > 0 && (
                      <tr aria-hidden="true" style={{ height: padTop }}>
                        <td colSpan={visible.length} style={{ padding: 0, border: 0 }} />
                      </tr>
                    )}
                    {slice.map((row, j) => {
                      const i = startIdx + j;
                      return (
                        <tr
                          key={`${row.path}-${i}`}
                          className={selected === i ? "selected" : ""}
                          style={{ height: ROW_HEIGHT }}
                          onMouseDown={(e) => {
                            if (e.button === 0) e.preventDefault();
                          }}
                          onClick={() => {
                            setSelected(i);
                            tableScrollRef.current?.focus();
                          }}
                          onDoubleClick={() => {
                            setSelected(i);
                            void invoke("open_file", { path: row.path }).catch((e) =>
                              setStatus(String(e)),
                            );
                          }}
                          onContextMenu={(e) => {
                            e.preventDefault();
                            setSelected(i);
                            void invoke("show_hits_context_menu", {
                              path: row.path,
                              screenX: e.screenX,
                              screenY: e.screenY,
                            }).catch((err) => setStatus(String(err)));
                          }}
                        >
                          {visible.map((id) => cell(id, row))}
                        </tr>
                      );
                    })}
                    {padBottom > 0 && (
                      <tr aria-hidden="true" style={{ height: padBottom }}>
                        <td colSpan={visible.length} style={{ padding: 0, border: 0 }} />
                      </tr>
                    )}
                  </tbody>
                </table>
              );
            })()}
          </div>
        </div>

        {previewShown && (
          <>
            <div
              className="fx-preview-resizer"
              role="separator"
              aria-orientation="vertical"
              title="拖动调整预览宽度"
              onMouseDown={(e) => {
                // 避免在拖动过程中表格列宽 / 行选中等其他 mousemove 干扰：捕获到 window 上。
                const startX = e.clientX;
                const startW = previewWidth;
                const onMove = (ev: MouseEvent) => {
                  // 分隔条在预览面板的左边缘，向左拖宽变大、向右拖宽变小。
                  const next = Math.min(
                    PREVIEW_MAX_WIDTH,
                    Math.max(PREVIEW_MIN_WIDTH, startW - (ev.clientX - startX)),
                  );
                  setPreviewWidth(next);
                };
                const onUp = () => {
                  window.removeEventListener("mousemove", onMove);
                  window.removeEventListener("mouseup", onUp);
                };
                window.addEventListener("mousemove", onMove);
                window.addEventListener("mouseup", onUp);
              }}
            />
            <div className="fx-preview-wrap" style={{ width: previewWidth, flex: `0 0 ${previewWidth}px` }}>
              <PreviewPane
                path={selected !== null ? sortedHits[selected]?.path ?? null : null}
                isDirectory={selected !== null ? !!sortedHits[selected]?.isDirectory : false}
              />
            </div>
          </>
        )}
      </div>

      <div className="fx-status">
        <div className="fx-status-left">
          {loading && (
            <span className="fx-status-spin" role="status" aria-live="polite" aria-label="搜索中" />
          )}
          <span>{status}</span>
        </div>
        <span className="fx-status-right">
          <span className="fx-index-line">{indexSummary}</span>
          {indexDetail && (
            <button
              type="button"
              className="fx-btn-icon"
              title="查看详细状态"
              onClick={() => setShowIndexDetail(true)}
            >
              详情
            </button>
          )}
          {selected !== null && (
            <>
              <button type="button" className="fx-btn-icon" onClick={() => void openSelected()}>
                打开
              </button>
              <button type="button" className="fx-btn-icon" onClick={() => void revealSelected()}>
                打开所在文件夹
              </button>
            </>
          )}
          <button
            type="button"
            className={`fx-btn-icon ${sidebarShown ? "active" : ""}`}
            title={sidebarShown ? "隐藏侧栏" : "显示侧栏"}
            aria-pressed={sidebarShown}
            aria-label={sidebarShown ? "隐藏侧栏" : "显示侧栏"}
            onClick={() => {
              const next = !sidebarShown;
              setSidebarOpen(next);
              setSidebarForce(contentWidth < SIDEBAR_AUTO_HIDE_BELOW ? (next ? "show" : "hide") : null);
            }}
          >
            <IconSidebar />
          </button>
          <button
            type="button"
            className={`fx-btn-icon ${previewShown ? "active" : ""}`}
            title={previewShown ? "关闭预览面板" : "打开预览面板"}
            aria-pressed={previewShown}
            aria-label={previewShown ? "关闭预览面板" : "打开预览面板"}
            onClick={() => {
              const next = !previewShown;
              setPreviewOpen(next);
              setPreviewForce(contentWidth < PREVIEW_AUTO_HIDE_BELOW ? (next ? "show" : "hide") : null);
            }}
          >
            <IconPreview />
          </button>
          <button
            type="button"
            className="fx-btn-icon"
            title="设置"
            aria-label="设置"
            onClick={() => {
              void invoke("show_settings_window");
            }}
          >
            <IconSettings />
          </button>
        </span>
      </div>
      {showIndexDetail && (
        <div
          className="fx-v2-notice-backdrop"
          role="dialog"
          aria-modal="true"
          aria-label="索引状态详情"
          onClick={() => setShowIndexDetail(false)}
        >
          <div
            className="fx-v2-notice-card"
            onClick={(e) => e.stopPropagation()}
            style={{ maxWidth: "720px" }}
          >
            <h3>索引状态详情</h3>
            <p style={{ whiteSpace: "pre-wrap", wordBreak: "break-word", lineHeight: "1.6" }}>
              {indexSummary}
            </p>
            {indexDetail && (
              <p style={{ whiteSpace: "pre-wrap", wordBreak: "break-word", lineHeight: "1.6" }}>
                {indexDetail}
              </p>
            )}
            <div className="fx-v2-notice-actions">
              <button
                type="button"
                className="fx-btn-icon fx-btn-search"
                onClick={() => setShowIndexDetail(false)}
              >
                关闭
              </button>
            </div>
          </div>
        </div>
      )}
      {showV2MigrationNotice && (
        <div className="fx-v2-notice-backdrop" role="dialog" aria-modal="true" aria-label="FindX2 升级说明">
          <div className="fx-v2-notice-card">
            <h3>FindX2 重大升级说明</h3>
            <p>
              当前版本为 <strong>FindX2 全面重构版</strong>。从 v1 升级后，默认采用服务模式并自动处理索引服务启动。
            </p>
            <ul>
              <li>FindX v1 已暂停支持，不再提供功能更新。</li>
              <li>建议升级完成后卸载旧版组件，避免环境冲突。</li>
              <li>安装后会默认将命令行工具目录加入用户 PATH（新终端生效）。</li>
              <li>如遇异常，可在设置中执行“重建索引”。</li>
            </ul>
            <div className="fx-v2-notice-actions">
              <button
                type="button"
                className="fx-btn-icon"
                onClick={() => {
                  void invoke("show_settings_window");
                  acknowledgeV2MigrationNotice();
                }}
              >
                打开设置并继续
              </button>
              <button type="button" className="fx-btn-icon fx-btn-search" onClick={acknowledgeV2MigrationNotice}>
                我已知晓
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
