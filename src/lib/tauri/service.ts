// 服务管理：SSH / Local 会话按需采样服务列表 + 启停（写操作，后端过策略门）。
//
// 与 process.ts 同走 exec 通道；`list` 只读、`control` 是写。前端确认弹窗即人工审批，
// 后端再做服务名白名单 + PolicyEngine 黑名单两道兜底（见 commands/service.rs）。
import type { SvcAction, SvcInfo } from "../../types";
import { call } from "./client";

/** `service_control` 的核验结果（macOS 后端合并核验；Linux systemd 同步收敛不核验 → settled/state 为 null）。 */
export interface SvcResult {
  /** true=达到预期状态；false=未达到（如 stop 后被 KeepAlive 拉起）；null=平台未核验。 */
  settled: boolean | null;
  /** 核验时的实际状态描述（如 `running:25739` / `stopped`）；拿不到为 null。 */
  state: string | null;
}

export const svc = {
  /**
   * 拉服务列表（后端按名称升序全量返回，无截断口径；Linux systemd 失败自动降级 /etc/init.d，macOS 走 launchctl）。
   * FTP/Telnet/串口/Mosh 无 exec 通道，后端报 E_INVALID；调用方已按 `hasCap(monitor)` 过滤。
   */
  list: (sessionId: string) => call<SvcInfo[]>("service_list", { sessionId }),

  /**
   * 启/停/重启/重载服务。Linux 上 `name` 不带 `.service` 后缀时后端自动补；macOS 上 `name` 为 launchd label（不补后缀）。
   * 返回核验结果（macOS 合并「操作 + 等 1s + launchctl list 查 PID」；Linux 为 null）：
   * `launchctl stop` 退出码 0 只代表信号已投递，KeepAlive 作业会被 launchd 立刻拉起——UI 应按 settled/state 如实提示。
   * 被策略拦截返回 `err.service.forbidden`，Windows 返回 `err.service.unsupportedOs`，
   * macOS 重载返回 `err.service.reloadUnsupported`（前端 errMsg 会本地化）。
   */
  control: (sessionId: string, name: string, action: SvcAction) =>
    call<SvcResult>("service_control", { sessionId, name, action }),
};
