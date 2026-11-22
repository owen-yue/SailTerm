// 全局命令注册表：命令面板（Ctrl+Shift+P / F1）与快捷键帮助（Ctrl+/）共用同一份定义，
// 加一条命令只改这里，两个入口自动同步，避免「两处各维护一份清单」迟早对不上。
//
// run 只接收 App 注入的执行上下文（各类 setter / action）——真正的状态都在 App 组件里，
// 命令层只负责「有哪些命令、怎么显示、调什么」，不持有任何 React state。
// labelKey / groupKey 走 i18n（中英跟随界面语言）；keys 是快捷键展示串，纯文本不翻译。
// feature 标注依赖某个未开放功能（见 lib/features）的命令：命令面板与快捷键帮助会自动剔除。

import { featureOn, type FeatureKey } from "./features";

/** 「打开设置并定位到某分类」的分类参数：取值须与 SettingsCenter 的 CategoryId 对齐。 */
export type SettingsCat =
  | "general"
  | "ai"
  | "memory"
  | "terminal"
  | "appearance"
  | "security"
  | "about";

/** App 注入的命令执行上下文：每个动作对应 App 里已有的一个 setter / action。 */
export interface CommandCtx {
  /** 新建连接（打开连接对话框）。 */
  openConnectNew(): void;
  /** 打开一个本地终端。 */
  newLocalTerminal(): void;
  /** 关闭当前标签页（Ctrl/Cmd+W）。 */
  closeActiveTab(): void;
  /** 展开 AI 面板并切到「聊天」子页（与 Ctrl+Shift+A 同语义，再按收起）。 */
  toggleAiChat(): void;
  /** 打开「发送」工具窗口（脚本发送；已在则聚焦，跨会话的全局工具）。 */
  openSendTool(): void;
  /** 切换终端专注模式。 */
  toggleFocus(): void;
  /** 立即锁屏（未设 PIN 时由 App 引导直接跳安全设置创建）。 */
  lockNow(): void;
  /** 打开全局用量统计弹窗。 */
  openUsageStats(): void;
  /** 打开设置中心并定位分类。 */
  openSettings(cat: SettingsCat): void;
  /** 打开命令面板自身（帮助浮层里的「命令面板」项跳转用）。 */
  openCommandPalette(): void;
  /** 切换快捷键帮助浮层。 */
  toggleShortcutHelp(): void;
}

export interface Command {
  id: string;
  /** 命令标题 i18n 键。 */
  labelKey: string;
  /** 分组标题 i18n 键（面板分区 + 帮助表分组都用它）。 */
  groupKey: string;
  /** 快捷键展示串（如 "Ctrl+Shift+A"）；无快捷键则留空，面板/帮助不显示键位。 */
  keys?: string;
  /** 绑定的功能开关：未开放（featureOn=false）时该命令从面板/帮助中剔除。留空 = 始终可用。 */
  feature?: FeatureKey;
  run: (ctx: CommandCtx) => void;
}

/** 全部命令定义（含被功能开关隐藏者）；对外用下方过滤后的 COMMANDS。 */
const ALL_COMMANDS: Command[] = [
  { id: "connect.new", labelKey: "cmd.connectNew", groupKey: "cmd.groupSession", keys: "Ctrl+N", run: (c) => c.openConnectNew() },
  // Ctrl+T 是各类终端工具的“新开一个”肌肉记忆；Ctrl+Shift+L 保留为既有键位（两者同语义）。
  { id: "terminal.local", labelKey: "cmd.terminalLocal", groupKey: "cmd.groupSession", keys: "Ctrl+T / Ctrl+Shift+L", run: (c) => c.newLocalTerminal() },
  { id: "send", labelKey: "cmd.send", groupKey: "cmd.groupSession", keys: "Ctrl+Shift+B", run: (c) => c.openSendTool() },
  { id: "ai.chat", labelKey: "cmd.aiChat", groupKey: "cmd.groupAi", keys: "Ctrl+Shift+A", run: (c) => c.toggleAiChat() },
  { id: "ai.usage", labelKey: "cmd.aiUsage", groupKey: "cmd.groupAi", run: (c) => c.openUsageStats() },
  { id: "focus", labelKey: "cmd.focus", groupKey: "cmd.groupView", keys: "Ctrl+Shift+M / F11", run: (c) => c.toggleFocus() },
  // 锁屏（应用级）：全局命令，三窗口通用；键位 Ctrl+Alt+L（Ctrl+Shift+L 已被本地终端占用）。
  { id: "lock", labelKey: "cmd.lock", groupKey: "cmd.groupView", keys: "Ctrl+Alt+L", run: (c) => c.lockNow() },
  { id: "palette", labelKey: "cmd.palette", groupKey: "cmd.groupView", keys: "Ctrl+Shift+P / F1", run: (c) => c.openCommandPalette() },
  { id: "help", labelKey: "cmd.help", groupKey: "cmd.groupView", keys: "Ctrl+/", run: (c) => c.toggleShortcutHelp() },
  { id: "settings.general", labelKey: "cmd.settingsGeneral", groupKey: "cmd.groupSettings", keys: "Ctrl+Shift+,", run: (c) => c.openSettings("general") },
  // 设置中心只留「通用」一个入口：进去就是分类导航，再为每个分类单列一条命令只会把
  // 面板撑长、搜索时四条同义条目相互撞车（`openSettings(cat)` 本身仍支持定位任意分类）。
];

/** 对用户开放的命令：自动剔除功能未开放（feature 关）的条目。面板与帮助共用此单源。 */
export const COMMANDS: Command[] = ALL_COMMANDS.filter(
  (c) => !c.feature || featureOn(c.feature),
);

/** 面板分组渲染顺序（未列出的分组排最后）。答疑已拆为独立页签，不在此列。 */
export const COMMAND_GROUP_ORDER: string[] = [
  "cmd.groupSession",
  "cmd.groupAi",
  "cmd.groupView",
  "cmd.groupSettings",
];

/** 答疑页签的分组键（其标签走 i18n `cmd.groupFaq`）；答疑条目本身由 lib/faq 从 md 语料读取。 */
export const CMD_GROUP_FAQ = "cmd.groupFaq";

/** 帮助浮层里额外展示的纯键位（只读、不可执行）：终端内建与导航类快捷键。 */
export interface ShortcutRow {
  labelKey: string;
  keys: string;
  groupKey: string;
}
export const EXTRA_SHORTCUTS: ShortcutRow[] = [
  { labelKey: "cmd.tabSwitch", keys: "Ctrl+1…9", groupKey: "cmd.groupNav" },
  { labelKey: "cmd.tabClose", keys: "Ctrl+W", groupKey: "cmd.groupNav" },
  { labelKey: "cmd.fontZoom", keys: "Ctrl+ = / − / 0", groupKey: "cmd.groupNav" },
  { labelKey: "cmd.termSearch", keys: "Ctrl+F", groupKey: "cmd.groupTerminal" },
  { labelKey: "cmd.focusChat", keys: "Ctrl+Space", groupKey: "cmd.groupTerminal" },
  // 审批条的键盘退路：只在有待裁决项时生效（平时一个按键也不多接），见 ApprovalBar。
  { labelKey: "cmd.apprApprove", keys: "Ctrl+Enter", groupKey: "cmd.groupAi" },
  { labelKey: "cmd.apprDeny", keys: "Ctrl+Shift+Enter", groupKey: "cmd.groupAi" },
];

/** 帮助浮层分组顺序（命令分组 + 额外的导航/终端分组）。 */
export const HELP_GROUP_ORDER: string[] = [
  "cmd.groupSession",
  "cmd.groupAi",
  "cmd.groupView",
  "cmd.groupSettings",
  "cmd.groupNav",
  "cmd.groupTerminal",
];

/**
 * 极简模糊匹配：查询词按空格切词，每个词须作为子串出现在「候选文本」里（大小写不敏感）。
 * 命中返回一个用于排序的分（越靠前命中越高分），未命中返回 -1。
 * 够用于几十条命令的即时过滤，不引第三方 fuzzy 库。
 */
export function fuzzyScore(query: string, text: string): number {
  const q = query.trim().toLowerCase();
  if (!q) return 0;
  const t = text.toLowerCase();
  const terms = q.split(/\s+/).filter(Boolean);
  let score = 0;
  for (const term of terms) {
    const idx = t.indexOf(term);
    if (idx < 0) return -1;
    // 词首命中加权，靠前的整体更高分；越靠文本前部越好
    score += 100 - Math.min(idx, 60);
    if (idx === 0 || /[\s./,;（(]/.test(t[idx - 1] ?? " ")) score += 30;
  }
  return score;
}
