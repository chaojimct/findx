/**
 * 主窗口底栏只用由真实字段归纳出的短句。
 * 技术原文（例如 CAP_SYS_ADMIN / inotify）放进详情，不用省略号把半句留在底栏。
 */

export type IndexStatusSnapshot = {
  indexing?: boolean;
  indexedCount?: number;
  metadataReady?: boolean;
  backfillDone?: number;
  backfillTotal?: number;
  indexingPhase?: string;
  indexingVolumesTotal?: number;
  indexingVolumesDone?: number;
  indexingMessage?: string;
  indexingEntriesIndexed?: number;
  indexingCurrentVolume?: string;
  loadingStage?: string;
  loadingElapsedSecs?: number;
  lastError?: string;
  enableMetadataBackfill?: boolean;
};

export type IndexStatusView = {
  summary: string;
  detail: string | null;
};

function countLabel(n: number): string {
  const v = Number.isFinite(n) ? Math.max(0, Math.floor(n)) : 0;
  return `索引 ${v.toLocaleString("zh-CN")} 项`;
}

/** Linux 监视降级的原文同时提到特权和 inotify。其它错误不走这句短摘要。 */
export function isWatchDegraded(error: string): boolean {
  return /CAP_SYS_ADMIN/.test(error) && /inotify/.test(error);
}

function phaseZh(phase: string | undefined): string {
  switch (phase) {
    case "starting":
      return "准备";
    case "scanning":
      return "扫描";
    case "merging":
      return "合并";
    case "writing":
      return "写入";
    default:
      return phase ?? "";
  }
}

function indexingDetail(st: IndexStatusSnapshot): string {
  const stage = st.loadingStage?.trim();
  const phase = st.indexingPhase;
  const entries = st.indexingEntriesIndexed;
  if (stage && !phase && entries == null) {
    return `索引: 加载中 · ${stage}`;
  }
  if (!phase && entries == null && st.loadingElapsedSecs != null) {
    return `索引: 加载中（已 ${st.loadingElapsedSecs}s）· 完成后自动可用`;
  }
  const segs: string[] = ["索引: 建库中"];
  const zh = phaseZh(phase);
  if (zh) segs.push(`阶段 ${zh}`);
  if (entries != null && !Number.isNaN(entries)) {
    segs.push(`已收录约 ${entries.toLocaleString("zh-CN")} 条`);
  }
  if (st.indexingVolumesTotal != null && st.indexingVolumesTotal > 0 && st.indexingVolumesDone != null) {
    segs.push(`卷进度 ${st.indexingVolumesDone}/${st.indexingVolumesTotal}`);
  }
  const curVol = st.indexingCurrentVolume?.trim();
  if (curVol) segs.push(`当前卷 ${curVol}`);
  const msg = st.indexingMessage?.trim();
  if (msg) segs.push(msg);
  if (stage) segs.push(stage);
  if (segs.length === 1) {
    segs.push("（读取 index.indexing.json，完成后将自动启动服务）");
  }
  return segs.join(" · ");
}

function indexingSummary(st: IndexStatusSnapshot): string {
  const stage = st.loadingStage?.trim();
  const phase = st.indexingPhase;
  const entries = st.indexingEntriesIndexed;
  if (stage && !phase && entries == null) return "索引 · 加载中";
  if (!phase && entries == null && st.loadingElapsedSecs != null) return "索引 · 加载中";
  const zh = phaseZh(phase);
  return zh ? `索引 · 建库中 · ${zh}` : "索引 · 建库中";
}

export function summarizeIndexStatus(st: IndexStatusSnapshot): IndexStatusView {
  const base = countLabel(st.indexedCount ?? 0);
  if (st.indexing) {
    const summary = indexingSummary(st);
    const detail = indexingDetail(st);
    return { summary, detail: detail === summary ? null : detail };
  }
  const err = st.lastError?.trim();
  if (err) {
    if (isWatchDegraded(err)) {
      return { summary: `${base} · 实时监控已降级`, detail: err };
    }
    return { summary: `${base} · 有警告`, detail: err };
  }
  if (st.metadataReady === false) {
    const bd = st.backfillDone ?? 0;
    const bt = st.backfillTotal ?? 0;
    const pct = bt > 0 ? Math.min(100, Math.round((bd / bt) * 100)) : null;
    if (st.enableMetadataBackfill === false) {
      return {
        summary: `${base} · 元数据回填已关闭`,
        detail: "元数据回填已关闭，可在设置中开启。",
      };
    }
    if (pct !== null) {
      return {
        summary: `${base} · 元数据回填 ${pct}%`,
        detail: `已回填 ${bd.toLocaleString("zh-CN")} / ${bt.toLocaleString("zh-CN")}。`,
      };
    }
    return { summary: `${base} · 元数据回填中`, detail: null };
  }
  return { summary: base, detail: null };
}
