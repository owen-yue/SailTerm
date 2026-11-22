import { sshKbd as api } from "../lib/tauri";
import type { KbdPrompt } from "../types";
import { createStore } from "./store";

interface KbdState {
  queue: KbdPrompt[];
}

export const kbdStore = createStore<KbdState>({ queue: [] });

export const kbdDecisions = {
  /** 后端抛来一轮待作答的提示（同 requestId 去重，防事件重放）。 */
  push(prompt: KbdPrompt): void {
    const { queue } = kbdStore.get();
    if (queue.some((q) => q.requestId === prompt.requestId)) return;
    kbdStore.set({ queue: [...queue, prompt] });
  },

  /** 用户提交答案：先出队（防重复提交），再回后端。请求已失效则静默忽略。 */
  async respond(requestId: string, answers: string[]): Promise<void> {
    dismiss(requestId);
    try {
      await api.respond(requestId, answers);
    } catch {
      // 超时或重复应答：后端已按失败处理，无需打扰用户
    }
  },

  /** 用户取消：出队并回 `null`（后端按主动放弃短路，绝不向服务端提交任何东西）。 */
  async cancel(requestId: string): Promise<void> {
    dismiss(requestId);
    try {
      await api.respond(requestId, null);
    } catch {
      // 同上，静默
    }
  },
};

function dismiss(requestId: string): void {
  const { queue } = kbdStore.get();
  kbdStore.set({ queue: queue.filter((q) => q.requestId !== requestId) });
}
