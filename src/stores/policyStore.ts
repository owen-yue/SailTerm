// 策略规则 store：黑/白/写名单 CRUD。
// 安全红线：规则只是命令模式，store 不含任何凭证；内置高危黑名单绝对优先。
import { policy as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { PolicyRule, PolicyRuleInput } from "../types";
import { createStore } from "./store";

interface PolicyState {
  items: PolicyRule[];
  loading: boolean;
  error: string;
}

export const policyStore = createStore<PolicyState>({
  items: [],
  loading: false,
  error: "",
});

export const policyActions = {
  async refresh(): Promise<void> {
    policyStore.set({ loading: true, error: "" });
    try {
      policyStore.set({ items: await api.list(), loading: false });
    } catch (e) {
      policyStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 新建或编辑；成功返回落库 DTO，失败返回 null（错误已入 store）。 */
  async save(input: PolicyRuleInput): Promise<PolicyRule | null> {
    try {
      const r = await api.save(input);
      policyStore.set((prev) => ({
        items: prev.items.some((x) => x.id === r.id)
          ? prev.items.map((x) => (x.id === r.id ? r : x))
          : [...prev.items, r],
        error: "",
      }));
      return r;
    } catch (e) {
      policyStore.set({ error: errMsg(e) });
      return null;
    }
  },

  async remove(id: string): Promise<void> {
    try {
      await api.remove(id);
      policyStore.set((prev) => ({ items: prev.items.filter((x) => x.id !== id) }));
    } catch (e) {
      policyStore.set({ error: errMsg(e) });
    }
  },

  /** 快捷启用/停用：显式重组入参落库（不带 createdAt 等非入参字段）。 */
  async toggleEnabled(rule: PolicyRule): Promise<void> {
    await policyActions.save({
      id: rule.id,
      kind: rule.kind,
      pattern: rule.pattern,
      action: rule.action,
      note: rule.note,
      enabled: !rule.enabled,
      sortOrder: rule.sortOrder,
    });
  },

  clearError(): void {
    policyStore.set({ error: "" });
  },
};
