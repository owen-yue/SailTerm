// 纯前端界面偏好（uiPrefs）：不入后端 settings 表，用 localStorage 持久化。
//
// 与后端持久化的偏好（外观主题/字号、终端行为、AI/安全）分工：这里只放「本机浏览器级、
// 与业务数据无关、跨会话记忆即可」的轻量偏好——监控采样间隔、下栏默认高度、终端行高/字距。
// 安全红线：不含任何凭证。
import { createStore } from "./store";

const LS_KEY = "sailterm.uiPrefs";

export interface UiPrefs {
  /** 监控页采样间隔（秒）：越短越实时但开销越大。允许值见 MONITOR_INTERVALS。 */
  monitorInterval: number;
  /** 下栏（文件/传输/监控/审计）默认高度（px），首次展开时用；运行中拖拽不回写此处。 */
  panelHeight: number;
  /** 终端行高倍数（xterm lineHeight），1.0–2.0。 */
  lineHeight: number;
  /** 终端字距（xterm letterSpacing，px），0–4。 */
  letterSpacing: number;
  /**
   * Next 灰字补全总开关：默认关（体验待优化）。关时终端不再显示 ghost 提示，
   * →/End/Tab 照常送 PTY；开时行为见 completionHistoryScope。修改后新开终端生效。
   */
  completionEnabled: boolean;
  /**
   * ghost 补全的历史数据源范围（字段保留，设置 UI 已不再暴露；终端侧固定按 `profile` 行为）：
   * - `profile`（默认）：只取「本会话所属连接」的历史，建议更贴合当前主机、不串台。
   * - `global`：取跨所有主机的全局历史，命令复用面更广（可能带出不相干主机的命令）。
   */
  completionHistoryScope: "profile" | "global";
  /**
   * 开发者模式：默认关。关时屏蔽浏览器化快捷键（F5 刷新 / F12 / Ctrl+Shift+I 等检查）并收起 devtools、
   * 右键不弹原生「检查」；开启后放行，供开发调试。仅 UX 门禁，非安全边界（release 构建 devtools 本就未编入）。
   */
  devMode: boolean;
}

/** 监控采样间隔可选项（秒）。 */
export const MONITOR_INTERVALS = [3, 5, 10, 30] as const;

export const UIPREFS_DEFAULT: UiPrefs = {
  monitorInterval: 5,
  panelHeight: 250,
  lineHeight: 1,
  letterSpacing: 0,
  completionEnabled: false,
  completionHistoryScope: "profile",
  devMode: false,
};

const clampNum = (v: unknown, min: number, max: number, dflt: number): number => {
  const n = typeof v === "number" ? v : Number(v);
  return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : dflt;
};

/** 从 localStorage 读偏好；缺失/脏值逐项回落默认（永不因坏数据崩界面）。 */
function load(): UiPrefs {
  try {
    const raw = localStorage.getItem(LS_KEY);
    if (!raw) return { ...UIPREFS_DEFAULT };
    const o = JSON.parse(raw) as Partial<UiPrefs>;
    const interval = MONITOR_INTERVALS.includes(o.monitorInterval as (typeof MONITOR_INTERVALS)[number])
      ? (o.monitorInterval as number)
      : UIPREFS_DEFAULT.monitorInterval;
    return {
      monitorInterval: interval,
      panelHeight: clampNum(o.panelHeight, 150, 560, UIPREFS_DEFAULT.panelHeight),
      lineHeight: clampNum(o.lineHeight, 1, 2, UIPREFS_DEFAULT.lineHeight),
      letterSpacing: clampNum(o.letterSpacing, 0, 4, UIPREFS_DEFAULT.letterSpacing),
      completionEnabled: o.completionEnabled === true,
      completionHistoryScope: o.completionHistoryScope === "global" ? "global" : "profile",
      devMode: o.devMode === true,
    };
  } catch {
    return { ...UIPREFS_DEFAULT };
  }
}

function persist(s: UiPrefs): void {
  try {
    localStorage.setItem(LS_KEY, JSON.stringify(s));
  } catch {
    // localStorage 不可用（隐私模式/超限）：偏好只在本次运行生效，忽略
  }
}

export const uiPrefsStore = createStore<UiPrefs>(load());

export const uiPrefs = {
  /** 局部更新并落 localStorage。 */
  set(patch: Partial<UiPrefs>): void {
    uiPrefsStore.set((prev) => {
      const next = { ...prev, ...patch };
      persist(next);
      return next;
    });
  },

  /** 恢复默认偏好。 */
  reset(): void {
    uiPrefsStore.set(() => {
      const next = { ...UIPREFS_DEFAULT };
      persist(next);
      return next;
    });
  },
};
