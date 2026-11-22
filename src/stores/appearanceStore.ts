// 外观/主题 store：主题 id、字号、字体、导入的自定义主题。
// 只存非敏感偏好（颜色/字体），绝不含凭证。变更即时热切换运行中的 xterm（见 Terminal.tsx）。
import { appearance as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import {
  applyAppChrome,
  DEFAULT_FONT_FAMILY,
  DEFAULT_FONT_SIZE,
  DEFAULT_THEME,
} from "../lib/themes";
import type { AppearancePatch, AppearanceSettings, DiscoveredTheme } from "../types";
import { createStore } from "./store";

/** 与后端 `AppearanceSettings::default()` 一致：默认主题、13px、Cascadia 字体族、无自定义主题。 */
const DEFAULTS: AppearanceSettings = {
  theme: DEFAULT_THEME,
  fontSize: DEFAULT_FONT_SIZE,
  fontFamily: DEFAULT_FONT_FAMILY,
  customTheme: null,
};

interface AppearanceState extends AppearanceSettings {
  loading: boolean;
  error: string;
  /** 是否已从后端拉到过（区分「默认值」与「已加载」）。 */
  loaded: boolean;
  /** 数据目录 themes/ 扫描出的可点选自定义主题（名称+路径）。 */
  discovered: DiscoveredTheme[];
  /** 扫描的主题目录绝对路径（供 UI 提示“把 *.json 放这里”）。 */
  themesDir: string;
}

export const appearanceStore = createStore<AppearanceState>({
  ...DEFAULTS,
  loading: false,
  error: "",
  loaded: false,
  discovered: [],
  themesDir: "",
});

export const appearanceActions = {
  async refresh(): Promise<void> {
    appearanceStore.set({ loading: true, error: "" });
    try {
      const s = await api.get();
      appearanceStore.set({ ...s, loading: false, loaded: true });
      applyAppChrome(s.theme, s.customTheme);
    } catch (e) {
      appearanceStore.set({ loading: false, error: errMsg(e) });
    }
  },

  /** 局部更新：乐观写入本地（终端立刻变色/变字号），落库失败回滚并报错。 */
  async update(patch: AppearancePatch): Promise<void> {
    const prev = appearanceStore.get();
    appearanceStore.set(patch);
    applyAppChrome(
      appearanceStore.get().theme,
      appearanceStore.get().customTheme,
    );
    try {
      const s = await api.set(patch);
      appearanceStore.set({ ...s, error: "" });
      applyAppChrome(s.theme, s.customTheme);
    } catch (e) {
      appearanceStore.set({
        theme: prev.theme,
        fontSize: prev.fontSize,
        fontFamily: prev.fontFamily,
        error: errMsg(e),
      });
    }
  },

  /** 从 JSON 文件导入自定义主题；成功即切到 custom 并返回新设置，失败返回 null（错误已入 store）。 */
  async importTheme(path: string): Promise<AppearanceSettings | null> {
    try {
      const s = await api.importTheme(path);
      appearanceStore.set({ ...s, loaded: true, error: "" });
      applyAppChrome(s.theme, s.customTheme);
      return s;
    } catch (e) {
      appearanceStore.set({ error: errMsg(e) });
      return null;
    }
  },

  /** 恢复默认外观（后端清除外观键）。 */
  async reset(): Promise<void> {
    try {
      const s = await api.reset();
      appearanceStore.set({ ...s, loaded: true, error: "" });
      applyAppChrome(s.theme, s.customTheme);
    } catch (e) {
      appearanceStore.set({ error: errMsg(e) });
    }
  },

  /** 扫描主题目录刷新「发现的主题」清单（失败不阻断，仅置错误信息）。 */
  async refreshThemes(): Promise<void> {
    try {
      const r = await api.scanThemes();
      appearanceStore.set({ discovered: r.themes, themesDir: r.dir });
    } catch (e) {
      appearanceStore.set({ error: errMsg(e) });
    }
  },

  clearError(): void {
    appearanceStore.set({ error: "" });
  },
};
