// 工具模块自带状态（可分离：仅借用主代码通用 createStore 基建，不挤进 stores/ 索引）。
//
// leftTab = 当前内容区（唯一真源）：connections = 会话工作区，tools = 工具区（左启动器 + 右工具面板），
// keys = 密钥库整页。三区完全独立，由左侧图标栏（会话 / 密钥库 / 工具）单点切换，互不遮挡。
// activeTool：tools 区当前展示的工具 id（null = 只显示启动器 + 选择提示）。
// 两者持久化到 localStorage，重启记忆；独立窗口（?pop / ?tool）用固定默认起手、不回写共享键。
import { createStore, isDetachedWindow } from "../stores/store";

export type LeftTab = "connections" | "tools" | "keys";

interface ToolsState {
  leftTab: LeftTab;
  activeTool: string | null;
}

const LS_TAB = "sailterm.leftTab";
const LS_TOOL = "sailterm.activeTool";

function initialTab(): LeftTab {
  const v = localStorage.getItem(LS_TAB);
  return v === "tools" || v === "keys" ? v : "connections";
}
function initialTool(): string | null {
  const v = localStorage.getItem(LS_TOOL);
  return v && v !== "null" ? v : null;
}

export const toolsStore = createStore<ToolsState>({
  leftTab: isDetachedWindow() ? "connections" : initialTab(),
  activeTool: isDetachedWindow() ? null : initialTool(),
});

export const toolsActions = {
  /** 切换内容区（会话 / 工具 / 密钥库）。 */
  setLeftTab(tab: LeftTab): void {
    toolsStore.set({ leftTab: tab });
    if (!isDetachedWindow()) localStorage.setItem(LS_TAB, tab);
  },
  /** 打开某工具：进入工具区并选中它（整页展示该工具）。 */
  openTool(id: string): void {
    toolsStore.set({ leftTab: "tools", activeTool: id });
    if (!isDetachedWindow()) {
      localStorage.setItem(LS_TAB, "tools");
      localStorage.setItem(LS_TOOL, id);
    }
  },
  /** 取消当前工具：回到「从左侧选择工具」提示（仍在工具区）。 */
  clearTool(): void {
    toolsStore.set({ activeTool: null });
    if (!isDetachedWindow()) localStorage.setItem(LS_TOOL, "null");
  },
};
