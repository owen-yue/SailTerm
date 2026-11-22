// 终端行为偏好 store：滚动缓冲、光标、本地 shell、交互行为。
// 只存非敏感偏好，绝不含凭证。变更即时热切换运行中的 xterm（见 Terminal.tsx）。
import { terminal as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { TerminalSettings, TerminalSettingsPatch } from "../types";
import { createStore } from "./store";

/** 与后端 `TerminalSettings::default()` 一致：5000 行、光标闪烁、平台默认 shell、关键字着色开。 */
const DEFAULTS: TerminalSettings = {
  scrollback: 5000,
  cursorBlink: true,
  localShell: "",
  copyOnSelect: false,
  cursorStyle: "block",
  bellStyle: "none",
  keywordHighlight: true,
};

interface TerminalState extends TerminalSettings {
  loading: boolean;
  error: string;
  loaded: boolean;
}

export const terminalStore = createStore<TerminalState>({
  ...DEFAULTS,
  loading: false,
  error: "",
  loaded: false,
});

export const terminalActions = {
  async refresh(): Promise<void> {
    terminalStore.set({ loading: true, error: "" });
    try {
      terminalStore.set({ ...(await api.get()), loading: false, loaded: true });
    } catch (e) {
      terminalStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 局部更新：乐观写入，失败回滚并报错。 */
  async update(patch: TerminalSettingsPatch): Promise<void> {
    const prev = terminalStore.get();
    terminalStore.set(patch);
    try {
      terminalStore.set({ ...(await api.set(patch)), error: "" });
    } catch (e) {
      terminalStore.set({
        scrollback: prev.scrollback,
        cursorBlink: prev.cursorBlink,
        localShell: prev.localShell,
        copyOnSelect: prev.copyOnSelect,
        cursorStyle: prev.cursorStyle,
        bellStyle: prev.bellStyle,
        error: errMsg(e),
      });
    }
  },

  async reset(): Promise<void> {
    try {
      terminalStore.set({ ...(await api.reset()), loaded: true, error: "" });
    } catch (e) {
      terminalStore.set({ error: errMsg(e) });
    }
  },

  clearError(): void {
    terminalStore.set({ error: "" });
  },
};
