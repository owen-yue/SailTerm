// 会话能力表：**「这个会话能做什么」的唯一事实源**。
//
// 为什么要有这个文件：协议差异原先以 `tab.kind === "ssh"` 的形式散落在十来个消费者里
// （监控面板、会话面板、AI 抽屉、隧道、广播、录制、页签…），每处各自决定"挡什么、怎么挡"，
// 于是同时长出两种病：
//   ① 挡得难看——面板照样占一个页签，点进去一句"仅 SSH"空文案；
//   ② 过度挡——AI 把 FTP 整个禁掉，可后端文件类工具其实对 FTP 完全可用。
// 收敛成一张表后，能力只在这里定义一次，面板过滤、工具下发、按钮显隐都来这儿查，
// 新增协议时也只改这一处。
//
// 与 Rust 侧 `SessionKind::caps()`（src-tauri/src/session.rs）同源：两边的档位含义必须一致，
// 改这张表的同时改那边，否则前端放行的能力会在后端拿不到通道。
import type { RailTab, SessionKind } from "../types";
import { isWindowsPlatform } from "./platform";

/**
 * 会话能力档位。每一项都对应"有一条真实通路"，不是"界面上想不想显示"：
 * - `pty`：有交互式终端（能敲命令、有 xterm 实例）。
 * - `exec`：有**非交互命令执行通道**（SSH exec / 本机子进程）。能跑 `pwd`、能采样、AI 能落地命令。
 *   与 `pty` 分开是有意的：Telnet/串口有终端但没有可靠的 exec 通道。
 * - `fs`：有远端文件系统后端（SSH 走 SFTP 子系统，FTP/FTPS 走控制连接），统一抽象见 `files::backend()`。
 * - `monitor`：能采样远端主机负载（CPU/内存/磁盘/网卡）。本质是 `exec` 的派生，单列是为了让面板声明意图。
 * - `negotiation`：连接建立时有协商快照可看（SSH 的 kex/算法，FTP 的 banner/FEAT/TLS）。
 * - `zmodem`：终端里能走 Zmodem 收发（仅 SSH，需要 PTY + sz/rz）。
 * - `tunnel`：能做端口转发/跳板（仅 SSH）。
 */
export type Capability =
  | "pty"
  | "exec"
  | "fs"
  | "monitor"
  | "negotiation"
  | "zmodem"
  | "tunnel";

/** 各协议的能力集合。缺项即"该协议物理上没有这条通路"。 */
const CAPS: Record<SessionKind, readonly Capability[]> = {
  ssh: ["pty", "exec", "fs", "monitor", "negotiation", "zmodem", "tunnel"],
  // FTP/FTPS 只有文件通路 + 一份协商快照（banner / FEAT / 证书指纹，见 `FtpInfo`）。
  // 无 exec 是硬约束：FTP 的命令只能说在控制连接上，拿不到退出码，也不能用来采样。
  ftp: ["fs", "negotiation"],
  ftps: ["fs", "negotiation"],
  // 本地终端：命令在本机跑，没有"远端文件后端"（本机文件用 local_* 命令，不经 fs 档位）。
  local: ["pty", "exec", "monitor"],
  telnet: ["pty"],
  serial: ["pty"],
  mosh: ["pty"],
};

/** 取某协议的能力集合（只读；调用方不该改它）。 */
export function capsOf(kind: SessionKind): ReadonlySet<Capability> {
  return new Set(CAPS[kind] ?? []);
}

/** 该协议是否具备某项能力。 */
export function hasCap(kind: SessionKind, cap: Capability): boolean {
  return (CAPS[kind] ?? []).includes(cap);
}

/** 满足任一即算可用（面板门槛常是 OR 语义：AI 面板有 exec 或有 fs 都值得开；
 * 需要 AND 时调用方自己写谓词，见 dockRegistry 的 `DOCK_META.visible`）。 */
export function hasAnyCap(kind: SessionKind, caps: readonly Capability[]): boolean {
  return caps.some((c) => hasCap(kind, c));
}

/**
 * 有 PTY 终端的会话（SSH / 本地 / Telnet / 串口 / mosh）。
 * 保留这个名字是因为调用点都在说"有没有终端"，比 `hasCap(k, "pty")` 更贴读感；实现即能力表投影。
 */
export const kindHasTerminal = (k: SessionKind): boolean => hasCap(k, "pty");

/**
 * 快捷命令栏（QuickBar）可用范围：SSH / 本地 / Telnet / mosh。
 *
 * 注册依据：QuickBar 单击即发的语义 = 往 PTY 写入 `命令 + \n` 让 shell 直接执行。
 * 串口虽也有 pty，但目标通常是路由器/交换机 CLI 或嵌入式设备，行缓冲 / echo / 提示符
 * 时序与通用 shell 不同，一期先不纳入，避免误发一整个脚本连敲十几条命令到设备。
 * FTP/FTPS 无 pty，天然排除（走 `hasCap(k,"pty")` 也返回 false，此处显式列出更清晰）。
 */
export const hasQuickBar = (k: SessionKind | null | undefined): boolean =>
  k === "ssh" || k === "local" || k === "telnet" || k === "mosh";

/**
 * 该会话是否有远端文件后端（能浏览/传输远端文件）：
 * SSH 走 SFTP 子系统、FTP/FTPS 走 FTP 控制连接；本地终端/Telnet/串口/mosh 没有。
 */
export const kindHasFiles = (k: SessionKind): boolean => hasCap(k, "fs");

/**
 * 有终端但只注册聊天形态的会话（Telnet / 串口 / mosh）。
 *
 * 注册依据：这类会话只有 PTY、没有可靠 exec 通道（见 CAPS 注释），后端按能力过滤后
 * AI 拿不到 run_command——Agent 档「AI 在终端落笔帮你执行」的兑现只剩落笔没有执行，
 * UI 承诺大于真实能力。故与本地 Windows 同族收敛为聊天形态（问答/终端快照分析仍在），
 * 让页签与开关的显隐判据对齐后端的工具下发过滤。
 */
export const isTerminalChatOnly = (k: SessionKind | null | undefined): boolean =>
  k === "telnet" || k === "serial" || k === "mosh";

/**
 * 本地 Windows 会话：聊天是唯一形态（无 Agent 页签、终端纯 shell、AI 页签恒聊天）。
 *
 * 注册依据：ConPTY/PSReadLine 的绝对定位重绘与终端 AI 落笔（绿字问句、多行回复）
 * 结构性冲突——前端直写 xterm 的行对 ConPTY 不可见，用户随后敲键即按过期坐标重绘、
 * 覆盖错乱；后端 run_command 对本地会话也强制走本机 exec、不写 PTY（tools.rs），
 * 故本地 Windows 上的 Agent 形态既错乱也无必要，统一收敛为聊天（静默 exec）。
 *
 * 与 Rust 侧无对应字段：这是纯前端呈现层的注册规则，能力表只在前端消费。
 */
export const isLocalWindows = (k: SessionKind | null | undefined): boolean =>
  k === "local" && isWindowsPlatform();

/**
 * 该会话能否开 Agent 形态（右栏 Agent 页签 / 终端 shell-agent 开关）：
 * 有终端、且不属于任一「聊天唯一形态」的注册例外（本地 Windows、Telnet/串口/mosh）。
 * 呈现层各判据点（页签注册、终端档推导、开关显隐）统一走这里，避免再漂移。
 */
export const canAgentRail = (k: SessionKind | null | undefined): boolean =>
  k != null && kindHasTerminal(k) && !isLocalWindows(k) && !isTerminalChatOnly(k);

/**
 * 会话在右栏 AI 面板注册的页签集合（数组顺序即显示顺序）——「哪些页签可见」的唯一事实源。
 * - `chat`：所有会话都有（AI 问答是保底形态）。
 * - `agent`：仅 `canAgentRail` 通过时注册。Agent 档的语义是「AI 直接在终端落笔」，
 *   无终端（FTP/FTPS）无可落；本地 Windows（ConPTY 重绘冲突）与 Telnet/串口/mosh
 *   （无 exec、拿不到执行工具）也不注册（见 isLocalWindows / isTerminalChatOnly）。
 * 调用方（SessionRail）另用 kindHasTerminal 决定是否画页签栏：终端类会话恒画栏（聊天唯一
 * 形态的会话只是少一枚 Agent、仍留「聊天」），无终端会话单视图不画栏。
 */
export const railTabsFor = (k: SessionKind | null): RailTab[] =>
  canAgentRail(k) ? ["agent", "chat"] : ["chat"];
