// 用户自定义策略规则：黑/白/写名单 CRUD。
// 规则只是命令模式（由 PolicyEngine 解释），本身不含凭证、也永不回传明文。
// 内置高危黑名单绝对优先，用户规则无法把 `rm -rf /` 之类降级放行。
import type { PolicyRule, PolicyRuleInput, RuleKind } from "../../types";
import { call } from "./client";

export const policy = {
  /** 列规则；`kind` 为空则不限类别，`enabledOnly` 为真只取启用中的。 */
  list: (kind?: RuleKind | null, enabledOnly?: boolean) =>
    call<PolicyRule[]>("policy_rule_list", {
      kind: kind ?? null,
      enabledOnly: enabledOnly ?? false,
    }),

  /** 新建或编辑；返回落库后的规则。 */
  save: (input: PolicyRuleInput) => call<PolicyRule>("policy_rule_save", { input }),

  /** 删除规则，返回是否删到。 */
  remove: (id: string) => call<boolean>("policy_rule_delete", { id }),
};
