// 停靠面板注册表：id → 页签元数据（i18n 文案 + 线稿图标）与内容体渲染。
// 下栏与右侧栏共用：同一面板拖到哪条栏，页签观感与内容组件都保持一致。
import type { ReactElement, ReactNode } from "react";
import AuditPanel from "../settings/AuditPanel";
import FileBrowser from "../file/FileBrowser";
import MonitorPanel from "../terminal/MonitorPanel";
import ProcessPanel from "../terminal/ProcessPanel";
import HistoryPanel from "../terminal/HistoryPanel";
import RecordPanel from "../terminal/RecordPanel";
import ServicesPanel from "../terminal/ServicesPanel";
import SessionPanel from "../terminal/SessionPanel";
import SessionRail from "./SessionRail";
import TransferPanel from "../file/TransferPanel";
import { hasAnyCap, hasCap } from "../../lib/capabilities";
import { useI18n } from "../../lib/i18n";
import type { DockZone, Tab } from "../../stores";
import { kindIsFtp, sessionLabel, type DockPanelId, type SessionKind } from "../../types";

/** 14px 线稿图标（emoji 小尺寸发糊）；下栏 TAB_ICONS 与右栏 agent 星形图标收口于此。 */
export const DOCK_ICONS: Record<DockPanelId, ReactNode> = {
  ai: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
      <path d="M8 1.5 9.4 6 14 8l-4.6 2L8 14.5 6.6 10 2 8l4.6-2z" />
    </svg>
  ),
  transfers: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <path d="M5 13V3M5 3 3 5.2M5 3l2 2.2" />
      <path d="M11 3v10M11 13l-2-2.2M11 13l2-2.2" />
    </svg>
  ),
  monitor: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <path d="M2 9h3l2-4.5 2 7L11 9h3" />
    </svg>
  ),
  process: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <rect x="2.5" y="3" width="11" height="3" rx="0.8" />
      <rect x="2.5" y="10" width="11" height="3" rx="0.8" />
      <path d="M5 6v4M11 6v4" />
    </svg>
  ),
  services: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round">
      <rect x="2.5" y="3.5" width="11" height="4" rx="2" />
      <circle cx="10" cy="5.5" r="1" />
      <rect x="2.5" y="9" width="11" height="4" rx="2" />
      <circle cx="6" cy="11" r="1" />
    </svg>
  ),
  files: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinejoin="round">
      <path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h2.2l1.3 1.6h5.5A1.5 1.5 0 0 1 14 6.1v5.4A1.5 1.5 0 0 1 12.5 13h-9A1.5 1.5 0 0 1 2 11.5z" />
    </svg>
  ),
  audit: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <rect x="3" y="3" width="10" height="11" rx="1.5" />
      <path d="M6 3V2h4v1M5.5 7h5M5.5 9.5h5M5.5 12h3" />
    </svg>
  ),
  session: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <circle cx="8" cy="8" r="6" />
      <path d="M8 7.2v4M8 4.6v.8" />
    </svg>
  ),
  history: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
      <path d="M8 14a6 6 0 1 0-5.6-3.9" />
      <path d="M2.4 13.6 2.4 10.1 5.9 10.1" />
      <path d="M8 5.5V8l2 1.6" />
    </svg>
  ),
  record: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3">
      <circle cx="8" cy="8" r="5.5" />
      <circle cx="8" cy="8" r="2" fill="currentColor" stroke="none" />
    </svg>
  ),
};

/** 页签元数据：`label`/`hint` 存 i18n 键，渲染处用 `t()` 解析。 */
type DockMeta = {
  label: string;
  hint?: string;
  /**
   * 该面板对当前会话是否值得展示；缺省 = 恒展示。
   * 规则一律用能力表表达（不写 `kind === "ssh"`），集中在注册表里便于一眼对照与事后追溯。
   */
  visible?: (kind: SessionKind) => boolean;
};

export const DOCK_META: Record<DockPanelId, DockMeta> = {
  // AI 要 exec / fs / pty 之一：要么能把命令落地/读终端输出（exec），要么能摸远端文件（fs），
  // 要么至少能聊天 + 终端快照分析（pty：Telnet/串口/mosh/本地）。聊天是所有会话的保底形态，
  // 与 railTabsFor 「chat 恒注册」同源，这里不能把整栏 AI 过滤掉。
  ai: { label: "dock.ai", hint: "dock.ai.hint", visible: (k) => hasAnyCap(k, ["exec", "fs", "pty"]) },
  // 文件面板只给「有终端且有文件后端」的会话（= SSH）：FTP/FTPS 的文件已经在主区，
  // dock 里再摆一份是重复；本地/Telnet/串口无远端文件后端，本来就没东西可列。
  files: { label: "panel.files", visible: (k) => hasCap(k, "fs") && hasCap(k, "pty") },
  // 传输与审计**不设门槛**：它们是全局面板（跨会话的队列与流水），不随聚焦会话消失。
  // 若按能力过滤，聚焦本地终端时正在跑的 FTP 传输队列会看不见——那是功能回退，不是降噪。
  transfers: { label: "panel.transfers" },
  monitor: { label: "panel.monitor", visible: (k) => hasCap(k, "monitor") },
  // 进程管理器与监控同档（都是 exec 派生的只读采样）：SSH + 本地；FTP/Telnet/串口无入口。
  process: { label: "panel.process", visible: (k) => hasCap(k, "monitor") },
  // 服务面板同进程档；启停是写操作，人工确认 + 后端策略门兜底（与 kill 同构，不另提能力档）。
  services: { label: "panel.services", hint: "dock.services.hint", visible: (k) => hasCap(k, "monitor") },
  audit: { label: "panel.audit" },
  session: { label: "panel.session", visible: (k) => hasCap(k, "negotiation") },
  // 历史命令只给有终端的会话（SSH/本地/Telnet/串口）：记录通路是终端回车上报，
  // FTP/FTPS 无 PTY、敲不进命令，唤回一个永远空白的面板只是噪音（与进程/监控同档过滤思路）。
  history: { label: "panel.history", hint: "dock.history.hint", visible: (k) => hasCap(k, "pty") },
  record: { label: "panel.record", hint: "dock.record.hint", visible: (k) => hasCap(k, "pty") },
};

/**
 * 当前会话下该展示哪些面板：按能力表过滤掉「这个协议物理上做不了」的面板。
 *
 * 为什么在渲染入口过滤而不是写回 dockStore：能力是「当前聚焦会话」的属性，
 * 而 dockStore 存的是用户布局（持久化）。把过滤结果回写 store，等于让一次
 * 切标签替用户改掉他手动拖出来的布局——切回去就丢了。
 * 于是这里只过滤展示，布局本体不动。
 *
 * `kind` 为 null（还没会话）时只留无门槛的全局面板；实际调用点均已在「有标签页」时才渲染 dock。
 */
export function visiblePanels(ids: DockPanelId[], kind: SessionKind | null): DockPanelId[] {
  return ids.filter((id) => {
    const v = DOCK_META[id].visible;
    return !v || (kind !== null && v(kind));
  });
}

/**
 * 面板内容体：按 id 渲染对应组件。
 *
 * `tab` = 当前聚焦会话：AI 面板要它的 id 定位会话 actor，文件面板要它的连接态/协议/显示名；
 * 其余面板与会话无关，忽略即可（统一签名方便调用点）。
 * `zone` = 本面板当前渲染在下栏还是右栏：供「双栏默认比例」等随可用宽度而定的
 * 自适应布局消费（见 FileBrowser）；产品规则放在消费者层，通用 dockPaneStore 底座保持中立。
 */
export function DockPanelBody({
  id,
  tab,
  zone,
}: {
  id: DockPanelId;
  tab: Tab | null;
  zone: DockZone;
}): ReactElement | null {
  const tr = useI18n();
  switch (id) {
    case "ai":
      return <SessionRail sessionId={tab?.id ?? ""} />;
    case "files":
      // 无会话时给一个空分片键：FileBrowser 内部按 connected 置灰，不会去请求不存在的连接
      return (
        <FileBrowser
          session={tab?.id ?? ""}
          connected={tab?.state === "connected"}
          title={tab ? sessionLabel(tab, tr) : tr("panel.files")}
          ftp={tab ? kindIsFtp(tab.kind) : false}
          zone={zone}
        />
      );
    case "transfers":
      return <TransferPanel />;
    case "monitor":
      return <MonitorPanel />;
    case "process":
      return <ProcessPanel />;
    case "services":
      return <ServicesPanel />;
    case "audit":
      return <AuditPanel />;
    case "session":
      return <SessionPanel />;
    case "history":
      return <HistoryPanel />;
    case "record":
      return <RecordPanel tab={tab} />;
  }
}
