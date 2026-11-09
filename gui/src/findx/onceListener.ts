export type Unlisten = () => void;

/**
 * 关闭监听只保留最后一次成功注册。
 * 若订阅回来时组件已经卸载或已经有更新的一次注册，立刻调用退订，避免叠多个 confirm。
 */
export function createOnceListener() {
  let generation = 0;
  let current: Unlisten | undefined;

  return {
    get installed(): boolean {
      return current != null;
    },
    async arm(subscribe: () => Promise<Unlisten>): Promise<void> {
      const gen = ++generation;
      const unlisten = await subscribe();
      if (gen !== generation) {
        unlisten();
        return;
      }
      current?.();
      current = unlisten;
    },
    dispose(): void {
      generation += 1;
      current?.();
      current = undefined;
    },
  };
}
