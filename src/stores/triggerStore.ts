// 输出触发器 store：规则列表 CRUD。匹配在 Terminal.tsx 本地执行。
import { triggers as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { TriggerInput, TriggerRule } from "../types";
import { createStore } from "./store";

interface TriggerState {
  items: TriggerRule[];
  loading: boolean;
  error: string;
}

export const triggerStore = createStore<TriggerState>({
  items: [],
  loading: false,
  error: "",
});

export const triggerActions = {
  async refresh(): Promise<void> {
    triggerStore.set({ loading: true, error: "" });
    try {
      triggerStore.set({ items: await api.list(), loading: false });
    } catch (e) {
      triggerStore.set({ loading: false, error: errMsg(e) });
    }
  },

  async save(input: TriggerInput): Promise<TriggerRule | null> {
    try {
      const r = await api.save(input);
      triggerStore.set((s) => ({
        items: input.id
          ? s.items.map((x) => (x.id === r.id ? r : x))
          : [...s.items, r].sort((a, b) => a.sortOrder - b.sortOrder || a.name.localeCompare(b.name)),
        error: "",
      }));
      return r;
    } catch (e) {
      triggerStore.set({ error: errMsg(e) });
      return null;
    }
  },

  async remove(id: string): Promise<void> {
    try {
      await api.delete(id);
      triggerStore.set((s) => ({ items: s.items.filter((x) => x.id !== id), error: "" }));
    } catch (e) {
      triggerStore.set({ error: errMsg(e) });
    }
  },

  clearError(): void {
    triggerStore.set({ error: "" });
  },
};
