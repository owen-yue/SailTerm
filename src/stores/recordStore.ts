// 会话录制 store：进行中状态 + 录制文件列表。
import { records as api, type RecordOverrides } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { RecordingInfo } from "../types";
import { createStore } from "./store";

interface RecordState {
  items: RecordingInfo[];
  /** 正在录制的会话 id 集合（串行化为数组便于 store）。 */
  recordingIds: string[];
  loading: boolean;
  error: string;
  /** 回放文本（空 = 未打开）；dir 用于全局库精确高亮当前项。*/
  playback: { name: string; dir: string; text: string } | null;
}

export const recordStore = createStore<RecordState>({
  items: [],
  recordingIds: [],
  loading: false,
  error: "",
  playback: null,
});

export const recordActions = {
  /** 刷新全局录制库（跨目录聚合，无需传目录）。*/
  async refresh(): Promise<void> {
    recordStore.set({ loading: true, error: "" });
    try {
      recordStore.set({ items: await api.list(), loading: false });
    } catch (e) {
      recordStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 同步各会话录制状态（打开面板时用）。 */
  async syncStatus(sessionIds: string[]): Promise<void> {
    try {
      const flags = await Promise.all(sessionIds.map((id) => api.status(id)));
      recordStore.set({
        recordingIds: sessionIds.filter((_, i) => flags[i]),
        error: "",
      });
    } catch (e) {
      recordStore.set({ error: errMsg(e) });
    }
  },

  async toggle(sessionId: string, dir?: string | null, opts?: RecordOverrides): Promise<void> {
    const { recordingIds } = recordStore.get();
    const on = recordingIds.includes(sessionId);
    try {
      if (on) {
        await api.stop(sessionId);
        recordStore.set({
          recordingIds: recordingIds.filter((x) => x !== sessionId),
          error: "",
        });
      } else {
        await api.start(sessionId, dir, opts);
        recordStore.set({ recordingIds: [...recordingIds, sessionId], error: "" });
      }
      await this.refresh();
    } catch (e) {
      recordStore.set({ error: errMsg(e) });
    }
  },

  async play(name: string, dir?: string | null): Promise<void> {
    try {
      const text = await api.read(name, dir);
      recordStore.set({ playback: { name, dir: dir ?? "", text }, error: "" });
    } catch (e) {
      recordStore.set({ error: errMsg(e) });
    }
  },

  closePlayback(): void {
    recordStore.set({ playback: null });
  },

  /** 在系统文件管理器中打开录制存放目录。 */
  async openDir(dir?: string | null): Promise<void> {
    try {
      await api.openDir(dir);
    } catch (e) {
      recordStore.set({ error: errMsg(e) });
    }
  },

  async remove(name: string, dir?: string | null): Promise<void> {
    try {
      await api.delete(name, dir);
      recordStore.set((s) => ({
        items: s.items.filter((x) => x.name !== name),
        playback: s.playback?.name === name ? null : s.playback,
        error: "",
      }));
    } catch (e) {
      recordStore.set({ error: errMsg(e) });
    }
  },

  clearError(): void {
    recordStore.set({ error: "" });
  },
};
