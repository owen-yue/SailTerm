// 底部面板 store：折叠状态、当前标签页与面板高度。
//
// 「传输」、「监控」与「审计」已落地（「文件」已改成会话主区视图，不再占停靠面板）。
// 面板列表本身改由 dockStore 管理（可拖到右侧栏），这里只留下栏的开关/高度与当前激活页。
import { createStore, isDetachedWindow } from "./store";
import { uiPrefsStore } from "./uiPrefsStore";
import { DOCK_ALL, type DockPanelId } from "../types";

/** 下栏激活页：与停靠面板同一套 id（AI 拖到下栏后也能成为激活页）。 */
export type PanelTab = DockPanelId;

/** 存量 tab 读到废弃 id（如早期的 files）时不带着脏值启动，回落到下栏默认首页。 */
function loadPanelTab(): PanelTab {
  // 独立窗口不回读共享 localStorage（避免继承别的窗口的激活页）。
  if (isDetachedWindow()) return "transfers";
  const saved = localStorage.getItem("sailterm.panelTab") as PanelTab | null;
  return saved && DOCK_ALL.includes(saved) ? saved : "transfers";
}

/** 面板高度上下限（px）：太矮看不清列表，太高会挤掉终端。 */
export const PANEL_MIN = 150;
export const PANEL_MAX = 560;
export const PANEL_DEFAULT = 250;

interface PanelState {
  open: boolean;
  tab: PanelTab;
  height: number;
}

// 下栏状态按「会话标签」独立记忆：同一窗口内每个标签各记自己的展开/收起与激活页，
// 切标签时快照旧标签、恢复新标签（rememberPanel / restorePanel，由 sessionStore 订阅驱动）。
// 新开连接默认收起（PANEL_BASE），把纵向空间让给终端，且不影响其它已展开的标签。
// 高度保持全局（uiPrefs.panelHeight 起手、运行期共享）：避免切换标签时分割条忽高忽低。
// 不再读写 localStorage 全局开关键——per-session 内存记忆优先，重启后各标签回落收起默认。
export const panelStore = createStore<PanelState>({
  open: false,
  tab: loadPanelTab(),
  height: uiPrefsStore.get().panelHeight || PANEL_DEFAULT,
});

export const panel = {
  toggle(): void {
    panelStore.set((prev) => ({ open: !prev.open }));
  },

  /** 展开并切到指定页；点已展开的当前页则收起（与常见 IDE 行为一致）。 */
  show(tab: PanelTab): void {
    panelStore.set((prev) => {
      const open = !(prev.open && prev.tab === tab);
      return open ? { open, tab } : { open };
    });
  },

  /** 无条件展开并切到指定页（快捷键 / 停靠移动等程序化入口；无 show 的 toggle 语义）。 */
  reveal(tab: PanelTab): void {
    panelStore.set({ open: true, tab });
  },

  close(): void {
    panelStore.set({ open: false });
  },

  /** 拖拽调整高度（自动夹在上下限之间）。 */
  setHeight(height: number): void {
    panelStore.set({ height: Math.min(PANEL_MAX, Math.max(PANEL_MIN, Math.round(height))) });
  },
};

/** 每个会话标签的下栏记忆（open + 激活页）；height 不入本表，保持全局共享。 */
const perSession = new Map<string, { open: boolean; tab: PanelTab }>();

/** 会话默认下栏态：收起 + 回落首页。新开连接 / 重启后无记忆时都走这里。 */
const PANEL_BASE: { open: boolean; tab: PanelTab } = { open: false, tab: "transfers" };

/** 切走某会话前，快照它当前的下栏态，供切回时还原。 */
export function rememberPanel(id: string): void {
  const { open, tab } = panelStore.get();
  perSession.set(id, { open, tab });
}

/** 切入某会话：恢复它记忆的下栏态；无记忆（新会话 / 重启后）回落收起默认。 */
export function restorePanel(id: string | null): void {
  panelStore.set(!id ? PANEL_BASE : (perSession.get(id) ?? PANEL_BASE));
}

/** 会话真正结束：清除其下栏记忆，避免 perSession 随会话堆积泄漏。 */
export function forgetPanel(id: string): void {
  perSession.delete(id);
}
