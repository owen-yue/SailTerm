// 应用级命令：启动信息、凭证后端状态与 🛡 安全状态灯。
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppInfo, SecurityStatus } from "../../types";
import { call } from "./client";

/** 数据目录即时迁移的进度事件载荷（与后端 MoveProgress 对齐）。 */
export interface MoveProgress {
  phase: "files" | "database" | "done";
  done: number;
  total: number;
  percent: number;
  current: string;
}

export const app = {
  info: () => call<AppInfo>("app_info"),
  /** 🛡 顶栏安全状态灯：出网/脱敏/凭证隔离/会话统计四维态势（只读，不含凭证）。 */
  securityStatus: () => call<SecurityStatus>("security_status"),
  /** 开发者模式：运行时开/关本窗口 devtools（release 无 devtools feature 时后端空操作）。 */
  setDevtools: (open: boolean) => call<void>("set_devtools", { open }),
  /** 数据目录迁移目标**预检**：返回拒绝原因码（same/nested/hasData/empty）或 null（可用）。 */
  dataDirCheck: (target: string) => call<string | null>("data_dir_check", { target }),
  /** 数据目录即时迁移：实时把当前数据拷到 `target`，进度走 `onMoveProgress` 事件；成功后需重启生效。 */
  dataDirMove: (target: string) => call<void>("data_dir_move", { target }),
  /** 订阅数据目录迁移进度（files→database→done）。 */
  onMoveProgress: (cb: (p: MoveProgress) => void): Promise<UnlistenFn> =>
    listen<MoveProgress>("data-dir-progress", (e) => cb(e.payload)),
  /** 在系统文件管理器中打开当前数据目录（走后端，不受 opener 前端 scope 限制）。 */
  openDataDir: () => call<void>("open_data_dir"),
  /** 重启应用（迁移完成后一键切到新数据目录）。 */
  restart: () => call<void>("restart_app"),
};
