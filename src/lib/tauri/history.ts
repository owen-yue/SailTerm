// 命令历史：后端入库前已脱敏，前端只做展示与上报。
import type { HistoryEntry } from "../../types";
import { call } from "./client";

export const history = {
  list: (profileId: string | null = null, limit = 100) =>
    call<HistoryEntry[]>("history_list", { profileId, limit }),
  /** 上报用户敲下的一行命令（终端回车时调用）。 */
  record: (sessionId: string, command: string) =>
    call<void>("history_record", { sessionId, command }),
  /** 删除单条历史（面板行悬停 ×）。 */
  remove: (entryId: string) => call<number>("history_delete", { entryId }),
  clear: (profileId: string | null = null) => call<number>("history_clear", { profileId }),
};
