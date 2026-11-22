// 工具模块自带 IPC 客户端（可分离：只依赖主代码 `lib/tauri/client` 的 call()/on() 基座，
// 与 transfer.ts 同层同规——本文件即工具与后端 commands/net.rs 的契约，摘走时契约随走）。
import { call, on, type UnlistenFn } from "../lib/tauri/client";
import type {
  DnsRecord,
  ListeningPort,
  OpenPort,
  PingReply,
  PingStats,
  PublicInfo,
  ScanProgress,
  TraceHop,
} from "./types";

/** 扫描进度事件名（与后端 net.rs `app.emit` 一致）。 */
export const EVENT_SCAN_PROGRESS = "tools-scan-progress";
/** ping 逐包应答事件名（与后端 net.rs `app.emit` 一致）。 */
export const EVENT_PING_REPLY = "tools-ping-reply";
/** traceroute 逐跳事件名（与后端 net.rs `app.emit` 一致）。 */
export const EVENT_TRACE_HOP = "tools-trace-hop";

export const netApi = {
  /** 本机监听/绑定端口列表。 */
  listeningPorts: () => call<ListeningPort[]>("net_listening_ports"),

  /** TCP connect 端口扫描：target 需 IPv4 字面量；portRange 形如 "20-1024,3306"。 */
  portScan: (target: string, portRange: string, timeoutMs: number) =>
    call<OpenPort[]>("net_port_scan", { target, portRange, timeoutMs }),

  /** 请求中止在途扫描（置后端原子标志）。 */
  scanCancel: () => call<void>("net_scan_cancel"),

  /** 订阅扫描进度事件。 */
  onScanProgress: (cb: (p: ScanProgress) => void): Promise<UnlistenFn> =>
    on<ScanProgress>(EVENT_SCAN_PROGRESS, cb),

  /** 订阅 ping 逐包应答事件（后端每解析出一个应答包即推送，前端实时上屏）。 */
  onPingReply: (cb: (r: PingReply) => void): Promise<UnlistenFn> =>
    on<PingReply>(EVENT_PING_REPLY, cb),

  /** Ping 主机（系统命令）：逐包经 onPingReply 实时推送，命令结束时返回汇总统计。 */
  ping: (host: string, count?: number) => call<PingStats>("net_ping", { host, count: count ?? null }),

  /** DNS 解析（系统解析器）：返回 A/AAAA 地址列表。 */
  dnsLookup: (domain: string) => call<DnsRecord[]>("net_dns_lookup", { domain }),

  /** 订阅 traceroute 逐跳事件（后端每解析出一跳即推送，前端实时上屏）。 */
  onTraceHop: (cb: (h: TraceHop) => void): Promise<UnlistenFn> =>
    on<TraceHop>(EVENT_TRACE_HOP, cb),

  /** Traceroute 主机（系统命令）：逐跳经 onTraceHop 实时推送，命令结束时返回全部跳。 */
  traceroute: (host: string, maxHops?: number) =>
    call<TraceHop[]>("net_traceroute", { host, maxHops: maxHops ?? null }),

  /** 本机公网出口 IP 与地理/ISP（经公共回显服务查询）。 */
  publicIp: () => call<PublicInfo>("net_public_ip"),

  /** 一键打开 macOS「隐私与安全性 → 本地网络」设置面板（Ping 命中权限拦截时的跳转）。 */
  openLocalNetworkSettings: () => call<void>("net_open_local_network_settings"),
};
