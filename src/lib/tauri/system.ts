// 系统信息卡片：SSH / Local 会话按需采集一次（后端同会话缓存）。
// 采集脚本与 AI 侧 `get_session_info` / `probe_session_env` 共用常量，前后端一份实现。
import type { SystemInfo } from "../../types";
import { call } from "./client";

export const system = {
  /**
   * 取系统信息卡片。`refresh` 默认 false——后端会优先回会话内缓存；用户点「刷新」
   * 按钮传 true 才重跑 exec 采集。FTP/Telnet/串口/Mosh 无 exec 通道，后端会报 E_INVALID；
   * 调用方（MonitorPanel）已按 `hasCap(monitor)` 过滤，这里不重复守卫。
   */
  info: (sessionId: string, refresh = false) =>
    call<SystemInfo>("system_info", { sessionId, refresh: refresh || null }),
};
