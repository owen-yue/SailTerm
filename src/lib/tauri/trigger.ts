// 输出触发器：规则 CRUD。匹配与动作在前端本地完成，后端只存规则（无凭证）。
import type { TriggerInput, TriggerRule } from "../../types";
import { call } from "./client";

export const triggers = {
  list: () => call<TriggerRule[]>("trigger_list"),
  save: (input: TriggerInput) => call<TriggerRule>("trigger_save", { input }),
  delete: (id: string) => call<boolean>("trigger_delete", { id }),
};
