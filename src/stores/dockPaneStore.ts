// 通用「双栏分隔比例」store：任何由左右两栏组成的视图区，其分隔条位置
// 按 (区 zone, 面板 panelId) 分区记忆并持久化，「软默认」由调用方按当前区传入。
//
// 设计意图（统一驱动，非只给文件用）：
// - 本 store 只存「左栏占比」这一客观视图状态（0..1），不含任何具体面板的产品规则；
//   后续任何双栏面板接同一钩子即可复用。与 dockStore（面板在哪个区）、collapseStore（整栏收起）正交。
// - 键两侧刻意用 `string` 而非 DockZone/DockPanelId：主区（会话文件视图）也要记忆比例，
//   但它不是「停靠区」，不该挤进停靠枚举；底座保持中立，调用方自取命名空间（如 main::files）。
// - 「分区记忆」：同一面板在右栏 vs 下栏各记一份比例，互不干扰。缺键 = 未设置 → 回落调用方软默认。
// - 纯前端视图状态，不含任何凭证。
import { createStore, useStore } from "./store";

/** 比例记忆表：键 = `zone::panelId`，值 = 左栏占比（0..1）。 */
type SplitMap = Record<string, number>;

const LS_KEY = "sailterm.dockPane";

/** 自由拖动时的夹取区间：给两栏各留一点宽度，避免拖到贴边后抓不住分隔条。 */
export const SPLIT_MIN = 0.08;
export const SPLIT_MAX = 0.92;

/** 一键吸附的极值：把分隔条推到最左/最右，一栏铺满（另一栏收成 0，靠箭头或再拖回来还原）。 */
export const SPLIT_DOCK_LEFT = 0;
export const SPLIT_DOCK_RIGHT = 1;

const splitKey = (zone: string, panelId: string): string => `${zone}::${panelId}`;

function loadMap(): SplitMap {
  try {
    const raw = localStorage.getItem(LS_KEY);
    return raw ? (JSON.parse(raw) as SplitMap) : {};
  } catch {
    return {};
  }
}

export const dockPaneStore = createStore<{ splits: SplitMap }>({ splits: loadMap() });

export const dockPaneActions = {
  /** 设定某 (zone,panel) 的左栏占比并持久化（拖动 / 吸附 / 复位都走这里）。 */
  setSplit(zone: string, panelId: string, ratio: number): void {
    dockPaneStore.set({ splits: { ...dockPaneStore.get().splits, [splitKey(zone, panelId)]: ratio } });
    localStorage.setItem(LS_KEY, JSON.stringify(dockPaneStore.get().splits));
  },

  /**
   * 拖动增量：以 store 里的当前比例为基准累加（避免拖拽闭包读到过期值），clamp 到 [SPLIT_MIN, SPLIT_MAX]。
   * `fallbackRatio` = 该键尚未记忆时的基准（调用方软默认）。
   */
  nudgeSplit(
    zone: string,
    panelId: string,
    deltaPx: number,
    totalPx: number,
    fallbackRatio: number,
  ): void {
    if (totalPx <= 0) return;
    const k = splitKey(zone, panelId);
    const cur = dockPaneStore.get().splits;
    const base = k in cur ? cur[k] : fallbackRatio;
    const r = Math.min(SPLIT_MAX, Math.max(SPLIT_MIN, base + deltaPx / totalPx));
    dockPaneActions.setSplit(zone, panelId, r);
  },
};

/**
 * React 订阅式双栏驱动：面板渲染期取当前区的左栏占比 + 一组改比例回调。
 * `defaultRatio` 由调用方依区计算（软默认），本钩子只负责「有记忆用记忆、没记忆用默认」。
 */
export function useDockSplit(
  zone: string,
  panelId: string,
  defaultRatio: number,
): {
  ratio: number;
  /** 拖动：把像素增量按容器总宽换算成比例并累加。 */
  nudge: (deltaPx: number, totalPx: number) => void;
  /** 一键把分隔条推到最左（远端铺满）/ 最右（本地铺满）/ 居中复位。 */
  dockLeft: () => void;
  dockRight: () => void;
  reset: () => void;
} {
  const { splits } = useStore(dockPaneStore);
  const k = splitKey(zone, panelId);
  const ratio = k in splits ? splits[k] : defaultRatio;
  return {
    ratio,
    nudge: (deltaPx, totalPx) =>
      dockPaneActions.nudgeSplit(zone, panelId, deltaPx, totalPx, defaultRatio),
    dockLeft: () => dockPaneActions.setSplit(zone, panelId, SPLIT_DOCK_LEFT),
    dockRight: () => dockPaneActions.setSplit(zone, panelId, SPLIT_DOCK_RIGHT),
    reset: () => dockPaneActions.setSplit(zone, panelId, 0.5),
  };
}
