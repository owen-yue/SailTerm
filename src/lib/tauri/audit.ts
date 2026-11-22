// 审计日志：只读查询 + 清空。
// 安全纪律：记录只由后端 orchestrator 在命令生命周期内落库，前端**不能增改**，
// 返回体只含脱敏命令与输出摘要（哈希），绝无原始敏感输出/凭证。
import type { AuditEntry } from "../../types";
import { call } from "./client";

export const audit = {
  /** 列审计（最新在前）；`sessionId`/`since` 为空则不过滤该维度，`limit` 缺省 200（后端 clamp 1..=2000）。 */
  list: (sessionId?: string | null, since?: number | null, limit?: number) =>
    call<AuditEntry[]>("audit_list", {
      sessionId: sessionId ?? null,
      since: since ?? null,
      limit: limit ?? null,
    }),

  /** 清空审计（`sessionId` 为空则清全部），返回删除条数。 */
  clear: (sessionId?: string | null) =>
    call<number>("audit_clear", { sessionId: sessionId ?? null }),
};
