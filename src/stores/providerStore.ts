// AI Provider store：列表 + 默认选择 + CRUD + 连通性自检。
//
// 安全红线：store **只存后端 DTO**（含 `hasKey` 布尔，不含 Key 明文）；
// API Key 仅在 `save` 的入参里一过，写入保险库后不在此留存。
import { providers as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { AiProvider, ProviderInput, ProviderTestResult } from "../types";
import { createStore } from "./store";

interface ProviderStoreState {
  items: AiProvider[];
  loading: boolean;
  error: string;
  /** 正在自检的 provider id（测试按钮 loading）；空串表示无。 */
  testing: string;
}

export const providerStore = createStore<ProviderStoreState>({
  items: [],
  loading: false,
  error: "",
  testing: "",
});

/** `testing` 的哨兵值：表单草稿自检中（尚未落库、无 id）；区分于具体 provider id。 */
export const TESTING_DRAFT = "__draft__";

export const providerActions = {
  async refresh(): Promise<void> {
    providerStore.set({ loading: true, error: "" });
    try {
      providerStore.set({ items: await api.list(), loading: false });
    } catch (e) {
      providerStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 新建或编辑；成功返回落库 DTO，失败返回 null（错误已入 store）。 */
  async save(input: ProviderInput): Promise<AiProvider | null> {
    try {
      const p = await api.save(input);
      upsert(p);
      return p;
    } catch (e) {
      providerStore.set({ error: errMsg(e) });
      return null;
    }
  },

  async remove(id: string): Promise<void> {
    try {
      await api.remove(id);
      providerStore.set((prev) => ({ items: prev.items.filter((p) => p.id !== id) }));
    } catch (e) {
      providerStore.set({ error: errMsg(e) });
    }
  },

  /** 设为默认：本地先乐观置位，落库失败再回滚刷新。 */
  async setDefault(id: string): Promise<void> {
    providerStore.set((prev) => ({
      items: prev.items.map((p) => ({ ...p, isDefault: p.id === id })),
    }));
    try {
      await api.setDefault(id);
    } catch (e) {
      providerStore.set({ error: errMsg(e) });
      await providerActions.refresh();
    }
  },

  /** 连通性自检 + 顺带探测图片能力；返回结果（成功）或 null（失败，错误已入 store）。
   *  后端已把探测结论写回该 provider 的 `supportsVision`，调用方刷新列表即可拿到。 */
  async test(id: string): Promise<ProviderTestResult | null> {
    providerStore.set({ testing: id, error: "" });
    try {
      return await api.test(id);
    } catch (e) {
      providerStore.set({ error: errMsg(e) });
      return null;
    } finally {
      providerStore.set({ testing: "" });
    }
  },

  /** 用表单草稿直连自检（不必先保存）：新建模态下无 id 也能测；`testing` 置为草稿哨兵。
   *  探测结果**不落库**（未保存态），由调用方存进表单，保存时随 `ProviderInput` 写入。 */
  async testDraft(input: ProviderInput): Promise<ProviderTestResult | null> {
    providerStore.set({ testing: TESTING_DRAFT, error: "" });
    try {
      return await api.testDraft(input);
    } catch (e) {
      providerStore.set({ error: errMsg(e) });
      return null;
    } finally {
      providerStore.set({ testing: "" });
    }
  },

  clearError(): void {
    providerStore.set({ error: "" });
  },
};

/** 新增或就地更新一行；若它被设为默认，其余取消默认（与后端 upsert 的互斥一致）。 */
function upsert(p: AiProvider): void {
  providerStore.set((prev) => {
    const items = prev.items.map((x) =>
      x.id === p.id ? p : p.isDefault ? { ...x, isDefault: false } : x,
    );
    return { items: prev.items.some((x) => x.id === p.id) ? items : [...items, p] };
  });
}

/** 对话兜底用的 provider id：显式默认优先，否则第一个；一个都没有则 null。 */
export function defaultProviderId(items: AiProvider[]): string | null {
  return (items.find((p) => p.isDefault) ?? items[0])?.id ?? null;
}
