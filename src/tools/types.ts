// 工具模块自带类型（可分离：不引用主代码 types.ts，摘走时类型随本目录一起走）。
//
// 网络小工具的后端返回形态与 `src-tauri/src/commands/net.rs` 的 serde(camelCase) 一一对应；
// 连接类工具（密钥 / 隧道 / 录制）复用主代码既有组件与其类型，不在本文件重复定义。
import type { ReactNode } from "react";

/** 一条本机监听/绑定端口（对应 net.rs::ListeningPort）。 */
export interface ListeningPort {
  proto: "tcp" | "udp";
  localAddr: string;
  localPort: number;
  pid: number;
  process: string;
}

/** 一个被探测为开放的端口（对应 net.rs::OpenPort）。 */
export interface OpenPort {
  port: number;
  state: string;
  latencyMs: number;
}

/** 扫描进度事件负载（事件名 `tools-scan-progress`，对应 net.rs::ScanProgress）。 */
export interface ScanProgress {
  scanned: number;
  total: number;
  open: number;
}

/** 单个 ping 应答包（对应 net.rs::PingReply，后端结构化解析，前端本地化渲染）。 */
export interface PingReply {
  seq: number;
  ip: string;
  /** 应答字节数；中文回显取不到时为 0（前端隐藏）。 */
  bytes: number;
  ttl: number;
  timeMs: number;
  /** 耗时是否形如 `<1ms`（Windows 常见）。 */
  timeLess: boolean;
}

/** ping 统计（对应 net.rs::PingStats）。 */
export interface PingStats {
  ok: boolean;
  sent: number;
  received: number;
  lossPct: number;
  minMs: number;
  avgMs: number;
  maxMs: number;
  packets: PingReply[];
  /** 无应答时的失败原因 code（"permission" | "unknown_host" | null）；前端据此细化「不可达」。 */
  reason: string | null;
  /** 无应答时系统 ping 的原始输出（reason 未命中时的回退展示）。 */
  detail: string | null;
}

/** 一条 DNS 解析记录（对应 net.rs::DnsRecord）。 */
export interface DnsRecord {
  /** "A"（IPv4）| "AAAA"（IPv6）。 */
  kind: string;
  /** 解析出的 IP 字面量。 */
  address: string;
  /** 系统解析器回显的服务端口（一般不展示）。 */
  port: number;
}

/** 一跳 traceroute 结果（对应 net.rs::TraceHop）。 */
export interface TraceHop {
  /** 跳序号（从 1 起）。 */
  hop: number;
  /** 该跳 IP；超时/取不到为空串（前端显示 `* * *`）。 */
  ip: string;
  /** 主机名（Unix 带括号名时有值，否则空串）。 */
  host: string;
  /** 各次探测往返时延（毫秒）。 */
  timesMs: number[];
}

/** 本机公网出口信息（对应 net.rs::PublicInfo）。 */
export interface PublicInfo {
  ip: string;
  country: string;
  city: string;
  org: string;
}

/** 分类展示顺序已在 registry 定义；此处仅保留分类枚举。 */
export type ToolCategory = "connection" | "network" | "ops" | "dev";

/** 一个工具在注册表里的描述。所有工具统一以独立窗口打开（见 openWindow / ToolWindow）。 */
export interface ToolDef {
  id: string;
  category: ToolCategory;
  /** i18n 键前缀；实际键为 `tool.<id>.name` / `tool.<id>.desc`。 */
  name: string;
  desc: string;
  /** 启动器 / 工具窗口头部图标（24×24 线稿，随本模块自带）。 */
  icon: ReactNode;
  /** 独立窗口默认尺寸（宽栏两栏布局的工具如录制库需更宽）；省略则用全局默认 860×640。 */
  winSize?: { width: number; height: number };
  /** 从工具启动器列表隐藏（仍注册在案，供独立窗口 / 顶栏快捷键调用）：如「发送」只走弹窗、不占工具区。 */
  launcherHidden?: boolean;
}
