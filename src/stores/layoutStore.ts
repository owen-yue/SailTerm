// 分屏布局 store：纯前端视图状态，不含任何凭证。
//
// 布局是一棵**嵌套二叉分屏树**：叶子 = 一个既有 session，分支 = 左右（row）/ 上下（column）
// 两个子树 + 比例。每个叶子复用现有 `Terminal` 组件渲染，后端会话 / SSH actor 完全不感知
// 分屏（零后端改动）。分屏只由「标签/窗格拖拽到目标 pane 四边」产生，没有分屏按钮。
// 树里的 session 会随标签关闭 / 弹出独立窗口而失效，所有动作都先经 `prune` 自愈对齐。
import { sessionActions, sessionStore } from "./sessionStore";
import { createStore } from "./store";

/** 分屏方向：row = 左右并排，column = 上下堆叠。 */
export type SplitDirection = "row" | "column";

/** 拖放目标边：决定新分支的方向与新叶子的位置。 */
export type DropZone = "left" | "right" | "top" | "bottom";

/** 嵌套分屏树节点：叶子（单会话）或分支（两个子树按 ratio 分割，ratio = a 侧占比）。 */
export type SplitNode =
  | { kind: "leaf"; session: string }
  | { kind: "branch"; id: string; dir: SplitDirection; ratio: number; a: SplitNode; b: SplitNode };

/** 一棵树最多容纳多少叶子（再多每格太窄不可用）。 */
export const MAX_PANES = 6;

interface LayoutState {
  /** 分屏树根；null 或单叶子 = 单屏（跟随 activeId 渲染，不显示 pane-head）。 */
  root: SplitNode | null;
}

export const layoutStore = createStore<LayoutState>({ root: null });

let seq = 0;
const nextBranchId = (): string => `br-${Date.now().toString(36)}-${++seq}`;

/** 会话是否仍归本窗口显示：标签存活且未弹出独立窗口。 */
function alive(id: string): boolean {
  const s = sessionStore.get();
  return !s.popped[id] && s.tabs.some((t) => t.id === id);
}

/** 收集树中全部叶子 session（有序）。 */
export function nodeLeaves(n: SplitNode | null): string[] {
  if (!n) return [];
  if (n.kind === "leaf") return [n.session];
  return [...nodeLeaves(n.a), ...nodeLeaves(n.b)];
}

function leafNode(s: string): SplitNode {
  return { kind: "leaf", session: s };
}

function countLeaves(n: SplitNode | null): number {
  return n ? (n.kind === "leaf" ? 1 : countLeaves(n.a) + countLeaves(n.b)) : 0;
}

/** 剪掉已关闭 / 已弹出的叶子；分支坍缩为唯一存活子树（自愈）。 */
function pruneNode(n: SplitNode | null): SplitNode | null {
  if (!n) return null;
  if (n.kind === "leaf") return alive(n.session) ? n : null;
  const a = pruneNode(n.a);
  const b = pruneNode(n.b);
  if (a && b) return { ...n, a, b };
  return a ?? b ?? null;
}

/** 把某叶子替换为子树（不可变更新）。 */
function replaceNode(n: SplitNode, session: string, sub: SplitNode): SplitNode {
  if (n.kind === "leaf") return n.session === session ? sub : n;
  return { ...n, a: replaceNode(n.a, session, sub), b: replaceNode(n.b, session, sub) };
}

/** 移除某叶子；其所在分支坍缩为兄弟子树。 */
function removeNode(n: SplitNode | null, session: string): SplitNode | null {
  if (!n) return null;
  if (n.kind === "leaf") return n.session === session ? null : n;
  if (n.a.kind === "leaf" && n.a.session === session) return n.b;
  if (n.b.kind === "leaf" && n.b.session === session) return n.a;
  return { ...n, a: removeNode(n.a, session)!, b: removeNode(n.b, session)! };
}

/** 更新某分支的比例。 */
function setNodeRatio(n: SplitNode | null, id: string, ratio: number): SplitNode | null {
  if (!n || n.kind === "leaf") return n;
  if (n.id === id) return { ...n, ratio };
  return { ...n, a: setNodeRatio(n.a, id, ratio)!, b: setNodeRatio(n.b, id, ratio)! };
}

/** 树外第一个「尚未显示」的可分屏 session（优先 SSH，FTP/FTPS 无终端排后）。 */
function nextUnshown(shown: string[]): string | null {
  const set = new Set(shown);
  const pool = sessionStore.get().tabs.filter((t) => !set.has(t.id) && !sessionStore.get().popped[t.id]);
  return (pool.find((t) => t.kind === "ssh") ?? pool[0])?.id ?? null;
}

export const layoutActions = {
  /** 对齐存活标签 / 弹出状态，自愈整棵树。 */
  prune(): void {
    const next = pruneNode(layoutStore.get().root);
    if (next !== layoutStore.get().root) layoutStore.set({ root: next });
  },

  /**
   * 当前应分屏显示的树：叶子 ≥2 且 activeId 在树内才分屏，否则 null（单屏）。
   * 护栏避免「聚焦项不在可见组」的一帧错位。
   */
  splitTree(activeId: string | null): SplitNode | null {
    const root = pruneNode(layoutStore.get().root);
    if (countLeaves(root) > 1 && activeId && nodeLeaves(root).includes(activeId)) return root;
    return null;
  },

  /**
   * 把 dragId 放到 targetId 叶子的 zone 边：left/right = 左右，top/bottom = 上下。
   * 单屏（root 为 null）时以 activeId 为目标叶子起树。dragId 缺省取树外下一个未显示会话。
   * dragId 已在树内 = 换位（先摘除再插入）；插入后超过叶子上限则整体忽略。
   */
  splitAt(
    targetId: string | null,
    zone: DropZone,
    dragId?: string | null,
  ): void {
    const root = pruneNode(layoutStore.get().root);
    const shown = nodeLeaves(root);
    const src = dragId || nextUnshown(shown);
    if (!src) return;
    // 目标叶子：显式 target，或单屏时的 activeId
    const target = targetId && shown.includes(targetId)
      ? targetId
      : root?.kind === "leaf"
        ? root.session
        : sessionStore.get().activeId;
    if (!target || target === src || !alive(src)) return;

    let base = root ?? leafNode(target);
    const moving = shown.includes(src);
    if (moving) base = removeNode(base, src)!;
    if (countLeaves(base) + 1 > MAX_PANES) return;
    if (!nodeLeaves(base).includes(target)) return; // 换位摘除后目标失效（理论上不会）

    const dir: SplitDirection = zone === "left" || zone === "right" ? "row" : "column";
    const first = zone === "left" || zone === "top";
    const pair: SplitNode = {
      kind: "branch",
      id: nextBranchId(),
      dir,
      ratio: 0.5,
      a: first ? leafNode(src) : leafNode(target),
      b: first ? leafNode(target) : leafNode(src),
    };
    layoutStore.set({ root: replaceNode(base, target, pair) });
    sessionActions.select(src);
  },

  /** 移除某叶子（会话标签本身保留）；分支坍缩为兄弟。焦点在被移除项时交给树内其余叶子。 */
  closeLeaf(id: string): void {
    const root = pruneNode(layoutStore.get().root);
    const next = removeNode(root, id);
    layoutStore.set({ root: countLeaves(next) > 1 ? next : null });
    if (sessionStore.get().activeId === id) {
      const rest = nodeLeaves(next);
      if (rest.length > 0) sessionActions.select(rest[0]);
    }
  },

  /** 拖动分割条：deltaPx / totalPx 换算成比例增量，clamp 0.1–0.9。 */
  nudgeRatio(branchId: string, deltaPx: number, totalPx: number): void {
    if (totalPx <= 0) return;
    const root = layoutStore.get().root;
    if (!root) return;
    const find = (n: SplitNode | null): Extract<SplitNode, { kind: "branch" }> | null => {
      if (!n || n.kind === "leaf") return null;
      if (n.id === branchId) return n;
      return find(n.a) ?? find(n.b);
    };
    const br = find(root);
    if (!br) return;
    const ratio = Math.min(0.9, Math.max(0.1, br.ratio + deltaPx / totalPx));
    layoutStore.set({ root: setNodeRatio(root, branchId, ratio) });
  },

  /** 退出分屏，回到单屏。 */
  reset(): void {
    if (layoutStore.get().root) layoutStore.set({ root: null });
  },
};
