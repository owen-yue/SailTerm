// 停靠布局 store：下栏 / 右侧栏两个停靠区各持一组面板，页签可跨区拖拽换位（左侧连接栏固定不参与）。
//
// 纯前端视图状态，不含任何凭证。展开/收起不另立状态：下栏 = panelStore.open、
// 右栏 = collapseStore.rail，与既有交互模型完全一致；本 store 只管「哪些面板在哪个区 + 右栏激活页」。
// 弹出独立窗口（PopWorkspace）不消费本 store：那个窗口的 AI 栏固定右侧。
import { DOCK_ALL, type DockPanelId } from "../types";
import { collapseActions, collapseStore } from "./collapseStore";
import { panel, panelStore } from "./panelStore";
import { createStore } from "./store";

/** 停靠区：right = 右侧栏（全高，跟聚焦会话）；bottom = 下栏。 */
export type DockZone = "right" | "bottom";

interface DockState {
  /** 右侧栏面板（顺序即页签轨展示顺序）。 */
  right: DockPanelId[];
  /** 下栏面板（顺序即页签条展示顺序）。 */
  bottom: DockPanelId[];
  /** 右栏当前激活面板（下栏激活页沿用 panelStore.tab）。 */
  activeRight: DockPanelId;
}

const LS_KEY = "sailterm.dock";

const DOCK_DEFAULTS: DockState = {
  // 文件浏览器默认与 AI 同列右栏：
  // 主区留给终端，要看文件时侧一眼就行，不必拿它跟终端抢地盘。
  right: ["ai", "files"],
  // 发送工具不在下栏：它是跨会话的全局工具，走独立工具窗口（tools/），不占停靠区。
  bottom: ["transfers", "monitor", "audit", "session"],
  activeRight: "ai",
};

/**
 * 读 localStorage 并逐项清洗：
 * - 未知 id（旧版本残留、或降级后已不存在的面板）→ **只剔这个面板**；
 * - 重复 id → 丢弃后出现的；
 * - 缺当前版本的面板 → 补到下栏末尾。
 *
 * 刻意不整份回落默认布局：早先的写法是「校验不过就 return DOCK_DEFAULTS」，
 * 增减一个面板会让老用户把整套面板拖来拖去的手动布局丢得干干净净。
 * 脏数据在真实用户那里是「少一个页签」，不应该是「回到出厂」；整份回落只留给数组结构都不成立时。
 * （`files` 一度被撤又恢复：存量布局缺它时走上面的「补下栏末尾」，不强制改用户已放的区。）
 */
function loadDock(): DockState {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) return DOCK_DEFAULTS;
    const v = JSON.parse(raw) as Partial<DockState>;
    if (!Array.isArray(v.right) || !Array.isArray(v.bottom)) return DOCK_DEFAULTS;
    const seen = new Set<DockPanelId>();
    const clean = (ids: unknown[]): DockPanelId[] =>
      ids.filter((id): id is DockPanelId => {
        if (!DOCK_ALL.includes(id as DockPanelId) || seen.has(id as DockPanelId)) return false;
        seen.add(id as DockPanelId);
        return true;
      });
    const right = clean(v.right);
    const bottom = clean(v.bottom);
    for (const id of DOCK_ALL) {
      if (!seen.has(id)) bottom.push(id);
    }
    const activeRight = right.includes(v.activeRight as DockPanelId)
      ? (v.activeRight as DockPanelId)
      : (right[0] ?? DOCK_DEFAULTS.activeRight);
    return { right, bottom, activeRight };
  } catch {
    return DOCK_DEFAULTS;
  }
}

export const dockStore = createStore<DockState>(loadDock());

function persistDock(s: DockState): void {
  localStorage.setItem(LS_KEY, JSON.stringify(s));
}

/** 面板当前所在停靠区；不在任何区（脏数据）返回 null。 */
export function dockZoneOf(id: DockPanelId): DockZone | null {
  const s = dockStore.get();
  if (s.right.includes(id)) return "right";
  if (s.bottom.includes(id)) return "bottom";
  return null;
}

export const dockActions = {
  /**
   * 把面板挪到目标停靠区的 index 位置（index 以「摘除 id 后」的目标数组为准，缺省=追加末尾）。
   * - 跨区：从原区摘除 → 插入目标区 index 处，并激活 + 展开目标区。
   * - 同区（from===to）：仅重排顺序，不改激活页、不动展开态。
   */
  move(id: DockPanelId, to: DockZone, index?: number): void {
    const s = dockStore.get();
    const from = dockZoneOf(id);
    if (!from) return;
    const right = s.right.filter((x) => x !== id);
    const bottom = s.bottom.filter((x) => x !== id);
    const dst = to === "right" ? right : bottom;
    const at = Math.max(0, Math.min(index ?? dst.length, dst.length));
    dst.splice(at, 0, id);
    const sameZone = from === to;
    // 跨区进右栏才把激活页切到它；否则保持原激活页（若已被拖走则回落剩余第一个）
    const activeRight =
      !sameZone && to === "right"
        ? id
        : right.includes(s.activeRight)
          ? s.activeRight
          : right[0] ?? s.activeRight;
    const next: DockState = { right, bottom, activeRight };
    dockStore.set(next);
    persistDock(next);
    if (sameZone) return; // 同区重排到此为止
    if (to === "right") collapseActions.set("rail", false);
    else panel.reveal(id);
    // 下栏激活页跟随：从右栏拖走时若下栏恰好停在该页（脏态），也回落到剩余第一页
    if (to === "right" && panelStore.get().tab === id && bottom.length > 0) {
      panel.reveal(bottom[0]);
    }
  },

  /** 展开并激活该面板所在停靠区（快捷键 / 程序化入口；无 toggle 语义）。 */
  reveal(id: DockPanelId): void {
    const s = dockStore.get();
    if (s.right.includes(id)) {
      dockStore.set({ activeRight: id });
      collapseActions.set("rail", false);
    } else if (s.bottom.includes(id)) {
      panel.reveal(id);
    }
  },

  /** 点右栏页签：已展开且是当前激活页 → 收起，否则激活 + 展开（与下栏 panel.show 同语义）。 */
  showRight(id: DockPanelId): void {
    const s = dockStore.get();
    if (!s.right.includes(id)) return;
    if (!collapseStore.get().rail && s.activeRight === id) {
      collapseActions.set("rail", true);
    } else {
      dockStore.set({ activeRight: id });
      collapseActions.set("rail", false);
    }
  },
};
