// FTPS 证书确认（TOFU）store：把后端事件排成队列，由 FtpsCertDialog 逐个呈现。
import { ftpsCerts as api } from "../lib/tauri";
import type { FtpsCertPrompt } from "../types";
import { createStore } from "./store";

interface FtpsCertState {
  queue: FtpsCertPrompt[];
}

export const ftpsCertStore = createStore<FtpsCertState>({ queue: [] });

export const ftpsCertDecisions = {
  /** 后端抛来一个待裁决请求。 */
  push(prompt: FtpsCertPrompt): void {
    const { queue } = ftpsCertStore.get();
    if (queue.some((q) => q.requestId === prompt.requestId)) return;
    ftpsCertStore.set({ queue: [...queue, prompt] });
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
  const { queue } = ftpsCertStore.get();
  ftpsCertStore.set({ queue: queue.filter((q) => q.requestId !== requestId) });
}
