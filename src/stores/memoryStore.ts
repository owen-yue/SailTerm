// 长期记忆 store：查看 / 手动增改 / 删除 / 清空。
// 安全红线：content 只存脱敏后的偏好/环境事实，store 绝不含凭证；后端入库前再校验，
// 命中密码/密钥/令牌即拒存。召回注入由后端按本轮层级再脱敏，前端只读展示。
import { memory as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { Memory, MemoryInput, MemoryScope } from "../types";
import { createStore } from "./store";

interface MemoryState {
  items: Memory[];
  loading: boolean;
  error: string;
  /** 当前筛选：作用域（默认 global；null 仅保留给「不限作用域」的内部语义）。 */
  scope: MemoryScope | null;
  /** 当前筛选：连接 id（配合 scope=server 只看某台）。 */
  connectionId: string | null;
}

export const memoryStore = createStore<MemoryState>({
  items: [],
  loading: false,
  error: "",
  scope: "global",
  connectionId: null,
});

export const memoryActions = {
  /** 拉取记忆；显式传 scope/connectionId 则同时更新筛选态，否则沿用当前筛选。 */
  async refresh(scope?: MemoryScope | null, connectionId?: string | null): Promise<void> {
    const cur = memoryStore.get();
    const s = scope === undefined ? cur.scope : scope;
    const c = connectionId === undefined ? cur.connectionId : connectionId;
    memoryStore.set({ loading: true, error: "", scope: s, connectionId: c });
    try {
      memoryStore.set({ items: await api.list(s, c), loading: false });
    } catch (e) {
      memoryStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 新建或编辑；成功返回落库 DTO，失败返回 null（错误已入 store，如「含凭证」被后端拒）。 */
  async save(input: MemoryInput): Promise<Memory | null> {
    try {
      const m = await api.save(input);
      memoryStore.set((prev) => ({
        items: prev.items.some((x) => x.id === m.id)
          ? prev.items.map((x) => (x.id === m.id ? m : x))
          : [m, ...prev.items],
        error: "",
      }));
      return m;
    } catch (e) {
      memoryStore.set({ error: errMsg(e) });
      return null;
    }
  },

  async remove(id: string): Promise<void> {
    try {
      await api.remove(id);
      memoryStore.set((prev) => ({ items: prev.items.filter((x) => x.id !== id) }));
    } catch (e) {
      memoryStore.set({ error: errMsg(e) });
    }
  },

  /** 按当前筛选清空（scope/connectionId 均空=清全部）；返回删除条数。 */
  async clear(): Promise<number> {
    const { scope, connectionId } = memoryStore.get();
    const matches = (x: Memory) =>
      (scope === null || x.scope === scope) &&
      (connectionId === null || x.connectionId === connectionId);
    try {
      const n = await api.clear(scope, connectionId);
      memoryStore.set((prev) => ({ items: prev.items.filter((x) => !matches(x)), error: "" }));
      return n;
    } catch (e) {
      memoryStore.set({ error: errMsg(e) });
      return 0;
    }
  },

  clearError(): void {
    memoryStore.set({ error: "" });
  },
};
