// AI/安全/通用设置 store：出网开关、审批模式、扫描层级、界面语言、回复详略。
// 只存非敏感偏好，绝不含凭证。变更即时影响下一轮 AI 编排的出网闸门与分级严格度。
import { setUiLanguage } from "../lib/i18n";
import { settings as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type { AiSettings, AiSettingsPatch } from "../types";
import { createStore } from "./store";
import { setOsNotify } from "./toastStore";

/** 与后端 `AiSettings::default()` 一致：出网开、只读自动、标准扫描、记忆开+自动提取+永久保留、回复精简、聊天页签。 */
const DEFAULTS: AiSettings = {
  approvalMode: "auto_readonly",
  networkEnabled: true,
  scanLevel: "standard",
  memoryEnabled: true,
  memoryAutoExtract: true,
  memoryRetentionDays: 0,
  terminalAi: "on",
  // 执行通道不再是独立设置：Agent 页签下 AI 命令恒打进终端 PTY，聊天页签恒静默 exec。
  // 默认落聊天页签（与后端 `RailTab::default()` 同档）。
  rail: "chat",
  uiLanguage: "system",
  replyVerbosity: "concise",
  allowPlaintextPassword: true,
  maxToolRounds: 100,
  chatMode: "agent",
  osNotifications: true,
};

interface SettingsState extends AiSettings {
  loading: boolean;
  error: string;
  /** 是否已从后端拉到过（区分「默认值」与「已加载」）。 */
  loaded: boolean;
}

export const settingsStore = createStore<SettingsState>({
  ...DEFAULTS,
  loading: false,
  error: "",
  loaded: false,
});

export const settingsActions = {
  async refresh(): Promise<void> {
    settingsStore.set({ loading: true, error: "" });
    try {
      const s = await api.get();
      settingsStore.set({ ...s, loading: false, loaded: true });
      setUiLanguage(s.uiLanguage);
      // OS 横幅开关同步给 toastStore（dispatchOs 在通知链路里，不能反向依赖本 store）
      setOsNotify(s.osNotifications);
    } catch (e) {
      settingsStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 局部更新：乐观写入本地，落库失败回滚到上一份并报错。 */
  async update(patch: AiSettingsPatch): Promise<void> {
    const prev = settingsStore.get();
    settingsStore.set(patch);
    try {
      const s = await api.set(patch);
      // 后端回的是完整设置，整体覆盖会把 rail 冲回「最后持久化的全局值」。
      // 但 rail 现兼作「按会话的视图态」（collapseStore.restoreRailMode 只改本地不落库），
      // 若在这里被覆盖，切标签恢复的 Agent/聊天会在下一次任意设置更新（如终端 AI 档同步）
      // 时被冲掉，表现为终端上的 shell/agent 开关「突然消失」。故回填时保留当前实时 rail。
      settingsStore.set({ ...s, rail: settingsStore.get().rail, error: "" });
      setUiLanguage(s.uiLanguage);
      setOsNotify(s.osNotifications);
    } catch (e) {
      settingsStore.set({
        approvalMode: prev.approvalMode,
        networkEnabled: prev.networkEnabled,
        scanLevel: prev.scanLevel,
        memoryEnabled: prev.memoryEnabled,
        memoryAutoExtract: prev.memoryAutoExtract,
        memoryRetentionDays: prev.memoryRetentionDays,
        terminalAi: prev.terminalAi,
        rail: prev.rail,
        uiLanguage: prev.uiLanguage,
        replyVerbosity: prev.replyVerbosity,
        allowPlaintextPassword: prev.allowPlaintextPassword,
        maxToolRounds: prev.maxToolRounds,
        chatMode: prev.chatMode,
        osNotifications: prev.osNotifications,
        error: errMsg(e),
      });
    }
  },

  /** 恢复默认（后端清除已存设置键）。 */
  async reset(): Promise<void> {
    try {
      const s = await api.reset();
      // rail 保留实时视图态（与 update 同理，见上方注释）；OS 横幅开关同步给 toastStore，
      // 漏了会导致「恢复默认后设置页显示开、实际仍静默」的背离。
      settingsStore.set({ ...s, rail: settingsStore.get().rail, loaded: true, error: "" });
      setUiLanguage(s.uiLanguage);
      setOsNotify(s.osNotifications);
    } catch (e) {
      settingsStore.set({ error: errMsg(e) });
    }
  },

  clearError(): void {
    settingsStore.set({ error: "" });
  },
};
