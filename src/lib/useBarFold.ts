import { useCallback } from "react";
import { createStore, isDetachedWindow, useStore } from "../stores/store";

/**
 * 「等你回执」卡片（澄清反问卡 / 命令审批条）的收起态，两处共用一个开关。
 *
 * 卡片挂在输入框上方、与消息列表同一列，题面一长就把对话区压到几乎没法滚；收成一条细栏
 * 能把高度还给列表，想接着翻上下文时不必先作答。默认展开（不改变现有作答习惯），收起状态
 * 记进 localStorage 下次保持。
 *
 * 与侧栏 collapseStore 同一口径：视图态只落 localStorage，不进数据库；折叠态是模块级
 * 单一状态源（同屏的审批条与反问卡共用一个开关，一处收起两处同收）；独立窗口
 *（弹出 `?pop=` / 工具窗 `?tool=`）用默认值起手、不回写共享键，与主窗口互不串味。
 */
const LS_KEY = "sailterm.aiBar.fold";

const foldStore = createStore({
  folded: isDetachedWindow() ? false : localStorage.getItem(LS_KEY) === "1",
});

export function useBarFold(): [boolean, () => void] {
  const { folded } = useStore(foldStore);
  const toggle = useCallback(() => {
    const next = !foldStore.get().folded;
    foldStore.set({ folded: next });
    if (!isDetachedWindow()) localStorage.setItem(LS_KEY, next ? "1" : "0");
  }, []);
  return [folded, toggle];
}
