// 审计日志 store：只读列表 + 会话/时间过滤 + 清空。
// 安全纪律：前端不能增改审计，记录只由后端 orchestrator 落库；本 store 只查询与清空。
//
// 过滤策略：时间范围走**服务端**（`since`），会话走**客户端**——下拉选项从当前结果集派生，
// 避免「选了某会话后选项收缩到只剩它自己」的死角；审计量本就不大（上限 500 条）。
import { audit as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { AuditEntry } from "../types";
import { createStore } from "./store";

/** 时间过滤预设：`ms` 为相对当前的回溯窗口，0 = 不限。 */
export const AUDIT_RANGES: { id: string; label: string; ms: number }[] = [
  { id: "all", label: "aud.rangeAll", ms: 0 },
  { id: "1h", label: "aud.range1h", ms: 3_600_000 },
  { id: "24h", label: "aud.range24h", ms: 86_400_000 },
  { id: "7d", label: "aud.range7d", ms: 604_800_000 },
];

/** 单次拉取上限（后端还会 clamp 到 1..=2000）。 */
const FETCH_LIMIT = 500;

interface AuditState {
  /** 当前时间范围内的全部条目（未按会话过滤）。 */
  items: AuditEntry[];
  loading: boolean;
  error: string;
  /** 会话过滤（空串 = 全部会话），客户端生效。 */
  sessionId: string;
  /** 时间范围 id（对应 `AUDIT_RANGES`），服务端生效。 */
  range: string;
}

export const auditStore = createStore<AuditState>({
  items: [],
  loading: false,
  error: "",
  sessionId: "",
  range: "all",
});

/** 把 range id 换算成后端 `since` 时间戳（不限则 null）。 */
function sinceMs(range: string): number | null {
  const r = AUDIT_RANGES.find((x) => x.id === range);
  return r && r.ms > 0 ? Date.now() - r.ms : null;
}

export const auditActions = {
  async refresh(): Promise<void> {
    auditStore.set({ loading: true, error: "" });
    const { range } = auditStore.get();
    try {
      const items = await api.list(null, sinceMs(range), FETCH_LIMIT);
      auditStore.set({ items, loading: false });
    } catch (e) {
      auditStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 切时间范围（服务端过滤）后重拉。 */
  async setRange(range: string): Promise<void> {
    auditStore.set({ range });
    await auditActions.refresh();
  },

  /** 切会话过滤（纯客户端，不重拉）。 */
  setSession(sessionId: string): void {
    auditStore.set({ sessionId });
  },

  /** 清空：遵循当前会话过滤（选了会话只清该会话，否则清全部），完成后重拉。 */
  async clear(): Promise<void> {
    const { sessionId } = auditStore.get();
    try {
      await api.clear(sessionId || null);
      await auditActions.refresh();
    } catch (e) {
      auditStore.set({ error: errMsg(e) });
    }
  },

  clearError(): void {
    auditStore.set({ error: "" });
  },
};

/** 结果集里出现过的会话 id（去重，供过滤下拉用）。 */
export function auditSessions(items: AuditEntry[]): string[] {
  return [...new Set(items.map((i) => i.sessionId).filter((s): s is string => !!s))];
}

/** 按客户端会话过滤后的可见条目。 */
export function visibleAudit(items: AuditEntry[], sessionId: string): AuditEntry[] {
  return sessionId ? items.filter((i) => i.sessionId === sessionId) : items;
}
