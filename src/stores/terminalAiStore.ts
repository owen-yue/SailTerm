// 每条终端各自的 shell/agent 档（前端内存态，随应用重启清空）。
// 新开终端默认 agent（开）：ensure() 一律快照为 "on"，切任一终端只改这一条、不拖其他终端
// （这个开关不该是全局的，且默认应是 agent 而不是 shell）。
// 全局 settingsStore.terminalAi 不再由本 store 写：它是后端 run_command 决定命令打进 PTY
// 还是 exec 的唯一依据（后端不分会话），由 App 里的 effect 同步成「当前聚焦终端」的档。
import type { SessionKind, TerminalAi } from "../types";
import { canAgentRail, kindHasTerminal } from "../lib/capabilities";
import { settingsStore } from "./settingsStore";
import { createStore } from "./store";

interface TerminalAiState {
  /** sessionId -> 该终端当前的 shell/agent 档。缺省（尚未快照）时回落到全局默认档。 */
  bySession: Record<string, TerminalAi>;
}

export const terminalAiStore = createStore<TerminalAiState>({ bySession: {} });

export const terminalAiActions = {
  /** 新会话建标签时快照「默认档」：聊天唯一形态的会话（本地 Windows / Telnet 系，
   *  能力表 canAgentRail）注册为 off（无 Agent 形态），其余默认 agent（开）。幂等。 */
  ensure(id: string, kind?: SessionKind): void {
    if (terminalAiStore.get().bySession[id]) return;
    terminalAiStore.set((prev) => ({
      bySession: { ...prev.bySession, [id]: kind != null && !canAgentRail(kind) && kindHasTerminal(kind) ? "off" : "on" },
    }));
  },

  /** 切换某一条终端的档：只改这一条，不影响其他终端。全局 terminal_ai（后端命令路由用）
   *  由 App 里的 effect 同步成「当前聚焦终端」的档，这里不直接写全局。 */
  set(id: string, mode: TerminalAi): void {
    terminalAiStore.set((prev) => ({
      bySession: { ...prev.bySession, [id]: mode },
    }));
  },

  /** 会话结束：清掉该终端的档位快照（弹出/归还窗口不调用，回来仍保留各自选择）。 */
  drop(id: string): void {
    const cur = terminalAiStore.get().bySession;
    if (!(id in cur)) return;
    const next = { ...cur };
    delete next[id];
    terminalAiStore.set({ bySession: next });
  },
};

/** 读取某终端的有效档：本条快照优先，未快照（如整页刷新后）回落全局默认档。 */
export function effectiveTerminalAi(id: string | null | undefined): TerminalAi {
  if (!id) return settingsStore.get().terminalAi;
  return terminalAiStore.get().bySession[id] ?? settingsStore.get().terminalAi;
}
