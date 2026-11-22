// SSH 隧道 / 端口转发 IPC：配置 CRUD + 运行时开 / 关 / 列活动转发。
// 只传地址端口，绝不含凭证；开启复用会话已认证连接，绝不二次索取凭证。
import type { ActiveTunnel, TunnelConfig, TunnelInput } from "../../types";
import { call } from "./client";

export const tunnels = {
  /** 列已保存配置；`profileId` 为空列全部，否则只列该连接的。 */
  list: (profileId?: string | null) =>
    call<TunnelConfig[]>("tunnel_list", { profileId: profileId ?? null }),

  /** 新建或编辑一条配置；返回落库后的配置。 */
  save: (input: TunnelInput) => call<TunnelConfig>("tunnel_save", { input }),

  /** 删除一条配置（不影响已在跑的转发），返回是否删到。 */
  remove: (id: string) => call<boolean>("tunnel_delete", { id }),

  /** 在某条已连接会话上开启本地转发；返回运行态（含实际本地端口）。 */
  open: (sessionId: string, tunnelId: string) =>
    call<ActiveTunnel>("tunnel_open", { sessionId, tunnelId }),

  /** 关闭一条运行中的转发（按 runId），返回是否确实关掉。 */
  close: (runId: string) => call<boolean>("tunnel_close", { runId }),

  /** 列运行中的转发；`sessionId` 为空列全部，否则只列该会话的。 */
  active: (sessionId?: string | null) =>
    call<ActiveTunnel[]>("tunnel_active", { sessionId: sessionId ?? null }),
};
