// 进程管理器：SSH / Local 会话按需采样进程列表 + 结束进程（写操作，后端过策略门）。
//
// 与 system.ts 同走 exec 通道；`list` 只读、`kill` 是写。前端确认弹窗即人工审批，
// 后端再做 pid≤1 + PolicyEngine 黑名单两道兜底（见 commands/process.rs）。
import type { ProcInfo, ProcSort } from "../../types";
import { call } from "./client";

/** `process_kill` 的核验结果（POSIX 后端合并核验；Windows 无法核验 → gone/state 为 null）。 */
export interface KillResult {
  /** true=已不在进程表；false=仍存在（含僵尸态，state 区分）；null=平台不支持核验。 */
  gone: boolean | null;
  /** 核验时状态原词（S/R/Z…）；拿不到为 null。 */
  state: string | null;
}

export const proc = {
  /**
   * 拉进程列表（服务端按 `sortBy` 排序、截断到 `limit`，默认 cpu / 200）。
   * `filter` 非空时后端先对**全量**样本做命令/用户/PID 匹配，不受前 200 截断影响——
   * 搜什么就看到什么，避免 0% CPU 的进程在截断线上时隐时现。
   * FTP/Telnet/串口/Mosh 无 exec 通道，后端报 E_INVALID；调用方已按 `hasCap(monitor)` 过滤。
   */
  list: (
    sessionId: string,
    sortBy: ProcSort = "cpu",
    limit = 200,
    filter: string | null = null,
  ) => call<ProcInfo[]>("process_list", { sessionId, sortBy, limit, filter }),

  /**
   * 结束进程。`signal`（仅 POSIX 有意义）："TERM" 优雅终止 / "KILL" 强杀；Windows 走 taskkill。
   * 返回核验结果（POSIX 合并「发信号 + 等 1s + kill -0 探存活」；Windows 为 null）：
   * kill 退出码 0 只代表信号已投递，不代表进程已退出——UI 应按 gone/state 如实提示。
   * 被策略拦截返回 `err.process.forbidden`（前端 errMsg 会本地化）。
   */
  kill: (sessionId: string, pid: number, signal?: "TERM" | "KILL") =>
    call<KillResult>("process_kill", { sessionId, pid, signal: signal ?? null }),
};
