// 主机密钥确认（TOFU）store：把后端事件排成队列，由 HostKeyDialog 逐个呈现。
import { hostKeys as api } from "../lib/tauri";
import type { HostKeyPrompt } from "../types";
import { createStore } from "./store";

interface HostKeyState {
  queue: HostKeyPrompt[];
}

export const hostKeyStore = createStore<HostKeyState>({ queue: [] });

export const hostKeyDecisions = {
  /** 后端抛来一个待裁决请求。 */
  push(prompt: HostKeyPrompt): void {
    const { queue } = hostKeyStore.get();
    if (queue.some((q) => q.requestId === prompt.requestId)) return;
    hostKeyStore.set({ queue: [...queue, prompt] });
  },

  /** 用户裁决：先出队（防重复点击），再回后端。请求已失效则静默忽略。 */
  async respond(requestId: string, accept: boolean): Promise<void> {
    dismiss(requestId);
    try {
      await api.respond(requestId, accept);
    } catch {
      // 超时或重复应答：后端已按拒绝处理，无需打扰用户
    }
  },
};

function dismiss(requestId: string): void {
  const { queue } = hostKeyStore.get();
  hostKeyStore.set({ queue: queue.filter((q) => q.requestId !== requestId) });
}
