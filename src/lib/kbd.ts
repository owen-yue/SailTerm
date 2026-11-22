// 快捷键展示串的按平台映射。
//
// 背景：命令注册表与各 locale 里的键位串都以 Windows 习惯写 `Ctrl+…`，而实现侧统一是
// `e.ctrlKey || e.metaKey`（见 App.tsx / FileEditor），即 macOS 上真实按的是 ⌘。
// 直接把 "Ctrl" 展示给 Mac 用户，他会去按物理控制键并以为功能坏了。
// 这里在**渲染层**做一次转换，数据源仍保持单源 `Ctrl+…`（谁都不必为两个平台维护两份串）。
//
// 例外：`Ctrl+Space` 不转 ⌘——macOS 上 ⌘Space 是系统 Spotlight，写它会误导用户去按
// 一条不可能生效的键；按控制键符号 ⌃Space 展示（与系统「选择上一个输入源」是否冲突需真机确认）。

import { isMacOS } from "./platform";

/** Mac 上的修饰符映射（苹果人机界面惯例：符号连排、不带 `+`）。 */
const MAC_MODS: Record<string, string> = {
  "Ctrl+Shift+": "⌘⇧",
  "Ctrl+Alt+": "⌘⌥",
  "Ctrl+Space": "⌃Space",
  "Ctrl+": "⌘",
  "Shift+": "⇧",
  "Alt+": "⌥",
};

/** 按键名→符号（审批条的提示语里 `Enter` 写得下但读不动，↵ 一眼认）。 */
const MAC_KEYS: Record<string, string> = {
  Enter: "↵",
};

/**
 * 把 `Ctrl+Shift+P / F1` 这类展示串转成当前平台的正确写法。
 * 非 macOS 原样返回；macOS 把 `Ctrl` 系修饰符换成 ⌘/⇧/⌥ 符号。
 */
export function kbdLabel(raw: string | undefined): string {
  if (!raw) return "";
  if (!isMacOS()) return raw;
  // 按 key 长度降序替换，保证 "Ctrl+Shift+" 先于 "Ctrl+" 命中（split/join 免转义之忧）。
  const mods = Object.entries(MAC_MODS)
    .sort((a, b) => b[0].length - a[0].length)
    .reduce((acc, [from, to]) => acc.split(from).join(to), raw);
  // 修饰符之后再换按键名：`Space` 已在 MAC_MODS 里作为特例处理（不能变成 ⌘Space）。
  return Object.entries(MAC_KEYS).reduce((acc, [from, to]) => acc.split(from).join(to), mods);
}
