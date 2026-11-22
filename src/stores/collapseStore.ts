// 边栏收起/展开统一收口（bug1）：左「连接」侧栏、右「AI」侧栏共用同一套交互模型——
// 收起按钮在各边栏自身头部，收起后留一条 28px 窄轨（点击即展开）。
// 状态集中在这里持久化 localStorage，App / PopWorkspace 消费，不再各处各做各的。
// （下边栏的展开/收起语义是「面板开关」，仍由 panelStore 管理。）
import { createStore, isDetachedWindow } from "./store";
import { settingsStore } from "./settingsStore";
import { canAgentRail, kindHasTerminal } from "../lib/capabilities";
import type { RailTab, SessionKind } from "../types";

/** 可收起的边栏：side = 左侧连接栏；rail = 会话右侧 AI/Agent 栏。 */
export type CollapseKey = "side" | "rail";

const LS_KEY: Record<CollapseKey, string> = {
  side: "sailterm.sideCol",
  rail: "sailterm.railCol",
};

interface CollapseState {
  side: boolean;
  rail: boolean;
}

export const collapseStore = createStore<CollapseState>({
  // 独立窗口用固定默认值起手（左栏/右栏都展开）、不回写共享键，与主窗口互不串味。
  side: isDetachedWindow() ? false : localStorage.getItem(LS_KEY.side) === "1",
  rail: isDetachedWindow() ? false : localStorage.getItem(LS_KEY.rail) === "1",
});

export const collapseActions = {
  toggle(key: CollapseKey): void {
    const next = !collapseStore.get()[key];
    collapseStore.set({ [key]: next });
    if (!isDetachedWindow()) localStorage.setItem(LS_KEY[key], next ? "1" : "0");
  },

  set(key: CollapseKey, collapsed: boolean): void {
    collapseStore.set({ [key]: collapsed });
    if (!isDetachedWindow()) localStorage.setItem(LS_KEY[key], collapsed ? "1" : "0");
  },
};

// —— 右「AI」栏按会话标签独立记忆（对齐下栏 panelStore 的模型）——
// 新开终端默认收起右栏（把横向空间先让给预加载页/终端），切标签时快照旧标签、恢复新标签各自记忆。
// 左「连接」栏仍走全局持久化（不参与 per-session）。由 sessionStore 末尾的统一订阅驱动。
const railMemory = new Map<string, boolean>();
/** 新会话右栏默认态：收起（rail=true 即 collapsed），与下栏「新连接默认收起」一致。 */
const RAIL_BASE = true;

/** 切走某会话前，快照它当前的右栏收起态（独立窗口不参与，弹出窗口 AI 栏固定展开）。 */
export function rememberRail(id: string): void {
  if (isDetachedWindow()) return;
  railMemory.set(id, collapseStore.get().rail);
}

/** 切入某会话：恢复记忆的右栏态；无记忆（新会话 / 重启后）回落收起默认。不改写 localStorage（per-session 优先）。 */
export function restoreRail(id: string | null): void {
  if (isDetachedWindow() || !id) return;
  const collapsed = railMemory.get(id) ?? RAIL_BASE;
  if (collapsed !== collapseStore.get().rail) collapseStore.set({ rail: collapsed });
}

/** 会话真正结束：清除其右栏记忆，避免 railMemory 随会话堆积泄漏。 */
export function forgetRail(id: string): void {
  railMemory.delete(id);
}

// —— AI 主画布页签（Agent / 聊天）按会话标签独立记忆（与右栏收起记忆同族）——
// 新开连接默认落在聊天页签，切标签时快照旧标签、恢复新标签各自选择。
// 与下栏/右栏「新连接默认态」一起，构成新建连接后的默认视图状态。
// 只本地写 settingsStore.rail（不走 settingsActions.update），避免每次切标签都回写后端持久化的全局档。
const railModeMemory = new Map<string, RailTab>();
/** 新会话 AI 页签默认档：聊天（与右栏/下栏「新连接默认态」同族注册）。 */
const RAIL_MODE_BASE: RailTab = "chat";

/** 切走某会话前，快照它当前的 AI 页签（独立窗口不参与，弹出窗口 AI 栏固定）。 */
export function rememberRailMode(id: string): void {
  if (isDetachedWindow()) return;
  railModeMemory.set(id, settingsStore.get().rail);
}

/** 切入某会话：恢复记忆的 AI 页签；无记忆（新会话 / 重启后）回落聊天页签。仅本地写、不落库。
 * 聊天唯一形态的会话（本地 Windows / Telnet 系，能力表 canAgentRail）只注册聊天：
 * 恒 chat、不恢复记忆（仅限有终端的会话；FTP 系无终端，rail 不由它支配）。 */
export function restoreRailMode(id: string | null, kind?: SessionKind | null): void {
  if (isDetachedWindow() || !id) return;
  if (kind != null && kindHasTerminal(kind) && !canAgentRail(kind)) {
    if (settingsStore.get().rail !== "chat") settingsStore.set({ rail: "chat" });
    return;
  }
  const mode = railModeMemory.get(id) ?? RAIL_MODE_BASE;
  if (mode !== settingsStore.get().rail) settingsStore.set({ rail: mode });
}

/** 会话真正结束：清除其 AI 页签记忆，避免 railModeMemory 随会话堆积泄漏。 */
export function forgetRailMode(id: string): void {
  railModeMemory.delete(id);
}
