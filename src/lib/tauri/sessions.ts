// 会话：建连 / 重连 / 写入 / 尺寸 / 关闭，以及输出与状态事件。
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { DirectConnect, MonitorSample, SessionDto, SessionInfo, SessionStageEvent, SessionStateEvent } from "../../types";
import { b64ToBytes, call } from "./client";

/** 终端输出事件名（负载 `{ sessionId, data(base64) }`）。 */
export const EVENT_OUTPUT = "session-output";
/** 会话状态事件名（负载 `{ sessionId, state, reason? }`）。 */
export const EVENT_STATE = "session-state";
/** 建连阶段事件名（负载 `SessionStageEvent`）。 */
export const EVENT_STAGE = "session-stage";

export const sessions = {
  /** 用已保存的连接档案建连。`connectId` 非空时后端逐阶段回推 `session-stage`。 */
  connect: (profileId: string, cols: number, rows: number, connectId?: string) =>
    call<SessionDto>("session_connect", { profileId, cols, rows, connectId: connectId ?? null }),
  /** 快速连接（不保存档案）。 */
  connectDirect: (params: DirectConnect, connectId?: string) =>
    call<SessionDto>("session_connect_direct", { params, connectId: connectId ?? null }),
  /** 打开本地终端会话（本机 shell，无凭证、无主机密钥）；shell 为空用平台默认。*/
  connectLocal: (cols: number, rows: number, shell?: string | null) =>
    call<SessionDto>("session_connect_local", { cols, rows, shell: shell ?? null }),
  /** 断线后沿用同一 session_id 重连（标签与终端内容保持不变）。 */
  reconnect: (id: string, cols: number, rows: number) =>
    call<SessionDto>("session_reconnect", { id, cols, rows }),
  list: () => call<SessionDto[]>("session_list"),
  /** 会话协商信息（会话信息面板）：快照 + RTT + 活跃隧道。 */
  info: (id: string) => call<SessionInfo>("session_info", { id }),
  /** 枚举本机串口设备名（Windows COM* / Unix tty*）。 */
  serialPorts: () => call<string[]>("serial_ports"),
  /** 探测本机 WSL：installed=引擎是否存在（决定选择器是否可见），distros=可交互发行版名（已剔除 docker-desktop 等）。 */
  wslList: () => call<{ installed: boolean; distros: string[] }>("wsl_list"),
  /** TCP 探测会话目标 RTT（毫秒）。 */
  rtt: (id: string) => call<number | null>("session_rtt", { id }),
  write: (id: string, data: string) => call<void>("session_write", { id, data }),
  /**
   * 直发原始字节（发送工具专用，后端 `session_write_bytes`）。
   * `session_write` 的入参是 UTF-8 `String`，表达不了 Hex 档的 `\x00`–`\xff` 任意字节；
   * 两者落点同为 `SessionCmd::Write`，信任级别一致（用户对自己会话的按键）。
   */
  writeBytes: (id: string, data: Uint8Array) =>
    call<void>("session_write_bytes", { id, data: Array.from(data) }),
  /** 取会话终端最近输出（降噪纯文本）；群发结果聚合 / 调试用。 */
  snapshot: (id: string, maxBytes?: number) =>
    call<string>("session_snapshot", { id, maxBytes: maxBytes ?? null }),
  /** 终端补帧：环缓冲原始字节（base64，含 ANSI/换行）。 */
  ringB64: (id: string, maxBytes?: number) =>
    call<string>("session_ring_b64", { id, maxBytes: maxBytes ?? null }),
  /** 轻量资源采样：只读 exec，返回 CPU/内存/磁盘/网卡累计值。*/
  monitor: (sessionId: string) => call<MonitorSample>("monitor_sample", { sessionId }),
  /**
   * 取终端当前工作目录（文件栏「跟随终端」）：后端只在 exec 通道跑一次固定的 `pwd`。
   * 没有通用 exec 接口是有意的——那条通道会绕过 AI 侧的策略裁决与人工审批。
   */
  cwd: (id: string) => call<string>("session_cwd", { id }),
  resize: (id: string, cols: number, rows: number) => call<void>("session_resize", { id, cols, rows }),
  close: (id: string) => call<void>("session_close", { id }),

  /** 订阅指定会话的终端输出（已解码为字节）。 */
  onOutput: (sessionId: string, cb: (bytes: Uint8Array) => void): Promise<UnlistenFn> =>
    listen<{ sessionId: string; data: string }>(EVENT_OUTPUT, (e) => {
      if (e.payload.sessionId === sessionId) cb(b64ToBytes(e.payload.data));
    }),

  /** 订阅全局会话状态变更（连接/断开/关闭）。 */
  onState: (cb: (p: SessionStateEvent) => void): Promise<UnlistenFn> =>
    listen<SessionStateEvent>(EVENT_STATE, (e) => cb(e.payload)),

  /** 订阅建连阶段事件（预加载页进度/日志）。 */
  onStage: (cb: (p: SessionStageEvent) => void): Promise<UnlistenFn> =>
    listen<SessionStageEvent>(EVENT_STAGE, (e) => cb(e.payload)),
};

export type { UnlistenFn };
