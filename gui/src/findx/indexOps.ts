/** 重建 / 压缩的可见反馈。不编造百分比，命令返回也不当成索引已经可用。 */

export type IndexOp = "rebuild" | "compact";
export type IndexOpPhase = "working" | "submitted" | "failed";

export function indexOpFeedback(op: IndexOp, phase: IndexOpPhase, detail?: string): string {
  const extra = detail?.trim();
  if (op === "rebuild" && phase === "working") {
    return "正在提交重建。期间搜索不可用；这里还没有进度百分比。";
  }
  if (op === "rebuild" && phase === "submitted") {
    return "已提交重建。进度看主窗口状态栏。这条只说明命令已发出，不代表索引已经重建完成。";
  }
  if (op === "rebuild" && phase === "failed") {
    return extra ? `重建没有开始：${extra}` : "重建没有开始。";
  }
  if (op === "compact" && phase === "working") {
    return "正在压缩索引。服务会短暂停止；完成前不要把这句当成压缩进度。";
  }
  if (op === "compact" && phase === "submitted") {
    return extra
      ? `压缩命令已返回：${extra} 若服务正在重启，是否可用以主窗口状态栏为准。`
      : "压缩命令已返回。若服务正在重启，是否可用以主窗口状态栏为准。";
  }
  return extra ? `压缩没有完成：${extra}` : "压缩没有完成。";
}
