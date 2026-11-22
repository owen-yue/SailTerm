// 轻量 i18n（「通用 → 界面语言」）——语料包注册制。
//
// 生效范围（渐进式）：设置中心框架（分类/标题/搜索）、「通用」分类、「终端」分类新增的交互行为项。
// 其余界面文案仍在逐步接入；终端输出（shell/远端内容）永不受语言切换影响。
//
// 架构：语言不再硬编码枚举，而是「语料包（LocalePack）注册表」——内置 zh/en 拆到 `src/locales/`，
// 插件可经 manifest.locales 提供语料 JSON，启动时由 App 拉取并 `registerLocale` 注入，与内置语言同权。
// 语言偏好由后端 `ui.language`（settings 表，任意 code 或 "system"）持久化，
// `settingsStore` 拉取/更新后调用 `setUiLanguage` 同步。
import { useSyncExternalStore } from "react";
import { plugins as pluginApi } from "./tauri";
import { setErrTranslator } from "./tauri/client";
import { enLocale } from "../locales/en";
import { zhLocale } from "../locales/zh";

/** 一个语言的完整供给：code（BCP-47 主子标签，如 zh/en/ja）、展示名、键值字典。 */
export interface LocalePack {
  code: string;
  /** 展示名，用该语言自身书写（如「简体中文」/「English」），切语言后选项名不漂移。 */
  label: string;
  dict: Record<string, string>;
}

/** 注册表：code → 语料包（Map 保持注册顺序 = 设置页展示顺序）。 */
const packs = new Map<string, LocalePack>();

/** 缺键回退链的兜底语言（内置中文，字典最全）。 */
const FALLBACK = "zh";

let lang: string = FALLBACK;
const listeners = new Set<() => void>();

function notify(): void {
  listeners.forEach((fn) => fn());
}

/** 把偏好值解析成实际语言 code：已注册 code 直用；"system" 按 navigator.language 匹配。 */
export function resolveLang(v: string): string {
  const code = v.trim().toLowerCase();
  if (code !== "system" && packs.has(code)) return code;
  const nav = navigator.language.toLowerCase();
  if (packs.has(nav)) return nav; // 完整匹配（如 zh-cn 恰好注册过）
  const prefix = nav.split("-")[0];
  if (packs.has(prefix)) return prefix; // 主子标签匹配（zh-CN → zh）
  return packs.has(FALLBACK) ? FALLBACK : (packs.keys().next().value ?? FALLBACK);
}

/**
 * 注册（或覆盖）一个语料包：内置语言启动即注册；插件语料由 App 启动流程异步注入。
 * 注册后通知订阅者——语言列表是动态的，设置页/跟随系统解析都随注册表变化。
 */
export function registerLocale(pack: LocalePack): void {
  const code = pack.code.trim().toLowerCase();
  if (!code || code === "system") return; // "system" 是保留偏好值，不允许占用
  packs.set(code, { ...pack, code });
  notify();
}

/**
 * 向一个**已注册**的语料包增量合并键值（不整包替换）：供功能模块自带文案、
 * 与内置语言同权注册而不覆盖既有键。目标语言未注册时静默跳过（不凭空造半截语言）。
 */
export function extendLocale(code: string, dict: Record<string, string>): void {
  const key = code.trim().toLowerCase();
  const pack = packs.get(key);
  if (!pack) return;
  packs.set(key, { ...pack, dict: { ...pack.dict, ...dict } });
  notify();
}

/** 当前可选语言列表（设置页动态渲染下拉/分段用；不含「跟随系统」，由 UI 自行前置）。 */
export function availableLocales(): LocalePack[] {
  return [...packs.values()];
}

/** 应用语言偏好（settingsStore 拉取/更新成功后调用；注册新语料包后也应重放一次）。 */
export function setUiLanguage(v: string): void {
  const next = resolveLang(v);
  if (next === lang && document.documentElement.lang) return;
  lang = next;
  document.documentElement.lang = packs.get(next)?.code ?? next;
  notify();
}

/** 取当前语言文案；缺键回退中文语料，再缺则返回键名本身（保证不因缺词而空白）。 */
export function t(key: string): string {
  return packs.get(lang)?.dict[key] ?? packs.get(FALLBACK)?.dict[key] ?? key;
}

/**
 * 当前「界面有效语言」的展示名（已解析 system→OS→已装语料包后的实际生效语言的 label，
 * 如「日本語」「English」「简体中文」；无 label 时回落 code）。供 AI 每轮随消息发给后端，
 * 使回答语言严格跟随界面实际生效的语言。
 */
export function getUiLangName(): string {
  return packs.get(lang)?.label ?? lang;
}

/** 订阅语言/注册表变化并返回 `t`（getSnapshot 为原始值，变化时触发重渲染）。 */
export function useI18n(): (key: string) => string {
  useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => lang + "|" + packs.size,
  );
  return t;
}

/** 订阅并返回当前语言 code：需要按语言切换数据源（如答疑 md 语料）的组件用它在切语言时重渲染。 */
export function useLang(): string {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => lang,
  );
}

/**
 * 渲染后端下发的策略裁决理由（机制 A）：按 `key` 取当前语言模板，用 `args` 回填 `{占位符}`。
 * 词典未命中（`t` 原样返回键名）时回落 `zh`（后端中文兜底），保证绝不空白。
 */
export function tReason(
  key: string,
  args: Record<string, string>,
  zh: string,
): string {
  let txt = t(key);
  if (txt === key) return zh;
  for (const [k, v] of Object.entries(args ?? {})) txt = txt.split(`{${k}}`).join(v);
  return txt;
}

// 内置语料包：先 zh 后 en（注册顺序 = 设置页展示顺序；FALLBACK=zh 必须最先在册）。
registerLocale(zhLocale);
registerLocale(enLocale);

// 注册后端错误本地化器（供 client.errMsg 依赖注入调用）：按后端 `err.*` 归类键查词典，
// 命中则用当前语言渲染类别文案，再拼上不可翻译的动态细节（`{d}` 占位优先，否则「 · 」追加）；
// `t` 对缺键会原样返回键名，据此判断未命中 → 回落后端中文 message（含前缀与细节的整串）。
setErrTranslator((key, message, detail) => {
  const label = t(key);
  if (label === key) return message;
  if (!detail) return label;
  return label.includes("{d}") ? label.replace("{d}", detail) : `${label} · ${detail}`;
});

/**
 * 插件语料供给（manifest.locales）：遍历已安装插件声明的语料，经后端 `plugin_locale_get`
 * 读取插件目录内的 JSON 字典并注册。单个插件语料失败只静默跳过，不影响启动与其他语言。
 * App 启动 effect 调用一次；插件安装/卸载后可再调（reload 语义 = 重新注册覆盖）。
 */
export async function registerPluginLocales(): Promise<void> {
  try {
    const list = await pluginApi.list();
    for (const p of list) {
      for (const loc of p.manifest.locales ?? []) {
        try {
          const dict = await pluginApi.locale(p.manifest.id, loc.code);
          registerLocale({ code: loc.code, label: loc.label || loc.code, dict });
        } catch {
          // 语料文件缺失/损坏：跳过该语言，插件其余能力不受影响
        }
      }
    }
  } catch {
    // 插件系统不可用（如后端未就绪）：内置语言已注册，忽略
  }
}
