import type { AppUpdateInfo, UpdateProgressEvent } from "./findxGuiTypes";
import { Button, Checkbox, Hint } from "./components/UIComponents";

export type UpdatePhase =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "downloaded"
  | "current"
  | "error";

export function deriveUpdatePhase(input: {
  busy: boolean;
  progress: UpdateProgressEvent | null;
  info: AppUpdateInfo | null;
  downloadedPath: string | null;
}): UpdatePhase {
  if (input.busy && input.progress) return "downloading";
  if (input.busy) return "checking";
  if (input.downloadedPath) return "downloaded";
  if (input.info && input.info.ok === false) return "error";
  if (input.info?.hasUpdate) return "available";
  if (input.info && !input.info.hasUpdate) return "current";
  return "idle";
}

function fmtSize(b: number): string {
  return b >= 1024 * 1024 ? `${(b / 1024 / 1024).toFixed(1)} MB` : `${(b / 1024).toFixed(0)} KB`;
}

const PHASE_LABEL: Record<UpdatePhase, string | null> = {
  idle: null,
  checking: "检查中",
  downloading: "下载中",
  downloaded: "已下载，可安装",
  current: "已是最新",
  available: "有新版本",
  error: "检查失败",
};

/**
 * 设置页「应用更新」区块。生产路径把 onCheck / onDownload 接到真实命令；
 * 开发夹具传入本地状态，不请求 GitHub。
 * 发行说明按纯文本显示，不解析 Markdown。
 */
export function UpdateCard({
  busy,
  info,
  progress,
  downloadedPath,
  autoCheck,
  onAutoCheck,
  onCheck,
  onDownload,
  onCancel,
  onInstall,
}: {
  busy: boolean;
  info: AppUpdateInfo | null;
  progress: UpdateProgressEvent | null;
  downloadedPath: string | null;
  autoCheck: boolean;
  onAutoCheck: (checked: boolean) => void;
  onCheck: () => void;
  onDownload: () => void;
  onCancel: () => void;
  onInstall: () => void;
}) {
  const phase = deriveUpdatePhase({ busy, progress, info, downloadedPath });
  const label = PHASE_LABEL[phase];
  const notes = info?.hasUpdate ? info.releaseNotes?.trim() : "";
  return (
    <section className="fx-update-card" data-update-phase={phase}>
      <div className="fx-update-card-header">
        <h3 className="fx-update-card-title">应用更新</h3>
        {label && <span className="fx-update-card-status">{label}</span>}
      </div>
      <div className="fx-update-card-actions">
        <Button onClick={onCheck} disabled={busy} size="sm">
          {phase === "checking" ? "检查中..." : "检查更新"}
        </Button>
        {phase === "available" && info?.downloadUrl && (
          <Button variant="primary" size="sm" onClick={onDownload}>
            下载 {info.latestVersion}
            {info.assetSize ? ` (${fmtSize(info.assetSize)})` : ""}
          </Button>
        )}
        {phase === "downloading" && (
          <Button size="sm" onClick={onCancel}>
            取消下载
          </Button>
        )}
        {phase === "downloaded" && (
          <Button variant="primary" size="sm" onClick={onInstall}>
            安装更新并重启
          </Button>
        )}
      </div>
      {progress && phase === "downloading" && (
        <div className="fx-update-progress">
          <div className="fx-update-progress-bar-bg">
            <div
              className="fx-update-progress-bar"
              style={{ width: `${Math.min(100, Math.max(0, progress.percent))}%` }}
            />
          </div>
          <p className="fx-update-progress-text">
            {progress.total > 0
              ? `${progress.percent.toFixed(1)}% (${fmtSize(progress.downloaded)} / ${fmtSize(progress.total)})`
              : `已下载 ${fmtSize(progress.downloaded)}`}
          </p>
        </div>
      )}
      {phase === "error" && info?.error && <p className="fx-hint fx-hint--error">{info.error}</p>}
      {notes && (
        <div className="fx-update-release-notes">
          <p>
            <strong>
              新版本 {info?.latestVersion}（当前 {info?.currentVersion}）
            </strong>
          </p>
          <p>{notes}</p>
        </div>
      )}
      <Checkbox
        checked={autoCheck}
        onChange={onAutoCheck}
        label="启动时自动检查更新（仅提示，不自动下载）"
      />
      <Hint variant="info">
        更新流程：检查 → 下载（带进度）→ 静默安装（弹一次管理员授权）→ 自动重启。 也可到{" "}
        <a href="https://github.com/chaojimct/findx/releases" target="_blank" rel="noreferrer">
          GitHub Releases
        </a>{" "}
        手动下载。
      </Hint>
    </section>
  );
}
