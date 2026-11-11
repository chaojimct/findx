/**
 * 设置页「检查 / 下载更新」实际使用的请求门闩。
 * 进行中的请求会拒绝新的开始；返回时若 id 已不是当前请求，调用方必须丢弃结果。
 */

export class UpdateRequestGate {
  private seq = 0;
  private active = 0;
  busy = false;

  /** 已有请求进行中时返回 null，且不更换当前 id。 */
  tryBegin(): number | null {
    if (this.busy) return null;
    this.busy = true;
    this.seq += 1;
    this.active = this.seq;
    return this.active;
  }

  isCurrent(id: number): boolean {
    return this.busy && id !== 0 && id === this.active;
  }

  /** 只有当前且仍在进行的请求可以结束 busy。重复的结束事件返回 false。 */
  finish(id: number): boolean {
    if (!this.busy || !this.isCurrent(id)) return false;
    this.busy = false;
    return true;
  }

  /** 使所有已发出的 id 失效。用于取消下载后丢掉迟到事件。 */
  invalidate(): void {
    this.seq += 1;
    this.active = this.seq;
    this.busy = false;
  }
}

/** 生产路径：过期响应不调用 apply。 */
export function settleUpdate<T>(
  gate: UpdateRequestGate,
  id: number,
  value: T,
  apply: (value: T) => void,
): boolean {
  if (!gate.isCurrent(id)) return false;
  apply(value);
  return true;
}
