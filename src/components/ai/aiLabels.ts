import type { RiskLevel } from "../../types";

/**
 * AI 展示层共用的标签/配色映射（抽屉、就地面板、审批弹窗三处共用，避免各写一份漂移）。
 * 与后端 `RiskLevel`（SCREAMING_SNAKE_CASE）和 `ToolRegistry::with_builtins` 的内建工具对齐。
 */

/** 风险级别 → 标签键 + 配色 class（渲染处用 t()）。 */
export const RISK: Record<RiskLevel, { label: string; cls: string }> = {
  READ_ONLY: { label: "ai.riskReadonly", cls: "ok" },
  WRITE: { label: "ai.riskWrite", cls: "warn" },
  DANGEROUS: { label: "ai.riskDangerous", cls: "bad" },
  UNKNOWN: { label: "ai.riskUnknown", cls: "warn" },
};

/** 内建工具 id → 标签键。 */
export const TOOL_LABEL: Record<string, string> = {
  write_plan: "ai.tool.writePlan",
  run_command: "ai.tool.runCommand",
  get_session_info: "ai.tool.sessionInfo",
  list_services: "ai.tool.listServices",
  get_terminal_snapshot: "ai.tool.termSnapshot",
  web_search: "ai.tool.webSearch",
  fetch_url: "ai.tool.fetchUrl",
  list_dir: "ai.tool.listDir",
  read_file: "ai.tool.readFile",
  search_files: "ai.tool.searchFiles",
  transfer_file: "ai.tool.transferFile",
};

/** 审计裁决 → 标签键 + 配色 class（审计页签用）。 */
export const AUDIT_DECISION: Record<string, { label: string; cls: string }> = {
  AUTO: { label: "ai.decAuto", cls: "ok" },
  APPROVED: { label: "ai.decApproved", cls: "ok" },
  DENIED: { label: "ai.decDenied", cls: "warn" },
  BLOCKED: { label: "ai.decBlocked", cls: "bad" },
  EDITED: { label: "ai.decEdited", cls: "info" },
  EXPIRED: { label: "ai.decExpired", cls: "warn" },
};

/** 规则类别 → 标签键 + 一句话说明键（安全设置用）。 */
export const RULE_KIND: Record<string, { label: string; hint: string }> = {
  readonly_whitelist: { label: "ai.kindReadonlyWl", hint: "ai.kindReadonlyWlHint" },
  write_list: { label: "ai.kindWriteList", hint: "ai.kindWriteListHint" },
  blacklist: { label: "ai.kindBlacklist", hint: "ai.kindBlacklistHint" },
};

/** 规则动作 → 标签键（留空=按类别隐含）。 */
export const RULE_ACTION: Record<string, string> = {
  allow: "ai.actAllow",
  approval: "ai.actApproval",
  block: "ai.actBlock",
};
