// 「终端 | 文件」主区分屏（大视图）按会话标签独立记忆。
//
// 定位：停靠面板（右栏/下栏）保留原样，本开关只决定**主区**是否在终端右侧并排挂一份
// 文件浏览器——与 dockStore（面板在哪个区）、collapseStore（整栏收起）正交。
// 作用域取「跨会话共享的持久化」（同侧栏 collapseStore 模型）：开着分屏时新开连接也自动
// 带上，符合「以上传为主」的用户预期。弹出/工具窗口（isDetachedWindow）不参与——
// PopWorkspace 不消费本 store，读写共享键会污染主窗口布局。纯前端视图状态，不含任何凭证。
import { hasCap } from "../lib/capabilities";
import { sessionStore } from "./sessionStore";
import { createStore, isDetachedWindow } from "./store";

const LS_KEY = "sailterm.termSplit";

/**
 * 「终端 | 文件」主区分屏（大视图）总开关：false = 功能暂缓上线。
 * 关掉后停靠位不给「展开到大视图」入口、已持久化的分屏态也不渲染，并且
 * 不再顶掉停靠区的「文件」页签（三者共用一个谓词，藏一半留一半会留下无入口的残留态）。
 * 状态机 / 分屏布局 / 落点路由均保留，交互打磨完把这行改回 true 即整体恢复。
 */
export const MAIN_SPLIT_ENABLED = false;

/**
 * 已开启主区分屏的会话 id 集合。
 * ⚠️ 必须用对象包一层 Set：store 底座 `set` 是 `{...state, ...patch}` 合并语义，
 * 裸 Set 会被展开成普通对象丢尽方法（.has 变 undefined）——集合态一律走本包装。
 */
interface SplitState {
  ids: Set<string>;
}

export const termSplitStore = createStore<SplitState>({
  ids: new Set(isDetachedWindow() ? [] : (JSON.parse(localStorage.getItem(LS_KEY) || "[]") as string[])),
});

export const splitActions = {
  /** 开/关某会话的主区分屏；变更即回写持久化（独立窗口不回写）。 */
  toggle(id: string): void {
    const next = new Set(termSplitStore.get().ids);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    termSplitStore.set({ ids: next });
    if (!isDetachedWindow()) localStorage.setItem(LS_KEY, JSON.stringify([...next]));
  },

  isSplit(id: string): boolean {
    return termSplitStore.get().ids.has(id);
  },

  /** 该会话能否开主区分屏：有远端文件后端且有终端（= SSH 系；FTP 文件已在主区，本地/Telnet 无文件后端）。 */
  isSplittable(id: string): boolean {
    const t = sessionStore.get().tabs.find((x) => x.id === id);
    return !!t && hasCap(t.kind, "fs") && hasCap(t.kind, "pty");
  },

  /** 会话关闭：从集合摘除并回写（App 在 tabs 变化时自愈调用）。 */
  forget(id: string): void {
    if (!termSplitStore.get().ids.has(id)) return;
    const next = new Set(termSplitStore.get().ids);
    next.delete(id);
    termSplitStore.set({ ids: next });
    if (!isDetachedWindow()) localStorage.setItem(LS_KEY, JSON.stringify([...next]));
  },
};
