// 桌面壳「防浏览器化」守卫：SailTerm 是原生桌面应用，不应暴露浏览器的刷新/开发者工具等行为。
//
// 默认（开发者模式关）拦截：F5、Ctrl/Cmd+R（刷新）、F12、Ctrl/Cmd+Shift+I|J|C（检查/控制台）、
// Ctrl/Cmd+U（查看源码）、Ctrl/Cmd+P（打印）；并强制收起本窗口 devtools。
// 开发者模式开启后全部放行，供开发调试。
//
// 关键取舍：只做 `preventDefault`（取消 WebView 的浏览器动作）而**不** `stopPropagation`——
// 这样 Ctrl+R 之类的按键仍能继续传到 xterm（shell 反向搜索），既禁了刷新又不劫持终端快捷键。
import { useEffect } from "react";
import { app as appApi } from "./tauri";
import { uiPrefsStore } from "../stores/uiPrefsStore";

/** devMode 关时需要拦截的浏览器快捷键判定；返回 true = 应 preventDefault。 */
function isBrowserKey(e: KeyboardEvent): boolean {
  const mod = e.ctrlKey || e.metaKey;
  const k = e.key;
  const kl = k.length === 1 ? k.toLowerCase() : k;
  // 刷新：F5 / Ctrl+R（不含 Ctrl+Shift+R，那是应用「录制」快捷键）
  if (k === "F5" || (mod && !e.shiftKey && kl === "r")) return true;
  // 开发者工具 / 检查：F12 / Ctrl+Shift+I|J|C
  if (k === "F12" || (mod && e.shiftKey && (kl === "i" || kl === "j" || kl === "c"))) return true;
  // 查看源码 / 打印
  if (mod && (kl === "u" || kl === "p")) return true;
  return false;
}

/** 焦点是否在真正的文本输入区（输入框/文本域/可编辑元素）。xterm 的输入代理是隐藏 textarea，
 *  会命中此判定而被放行给 xterm（它自行把 ^A 送 PTY 做「行首」），故不会误拦终端的 Ctrl+A。 */
function isEditableTarget(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el || typeof el.tagName !== "string") return false;
  return el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable === true;
}

/**
 * 安装守卫：拦截浏览器快捷键 + 依 devMode 同步本窗口 devtools。
 * 主窗口（App）与弹出窗口（PopWorkspace）都应挂载——各 WebviewWindow 独立控制自身 devtools。
 * devMode 实时读取（uiPrefsStore.get()），无需随偏好变化重挂监听。
 */
export function useBrowserGuards(): void {
  // 键盘：capture 阶段先于 WebView 处理，preventDefault 取消刷新/检查，但不阻断事件继续下发到终端
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (uiPrefsStore.get().devMode) return;
      if (isBrowserKey(e)) {
        e.preventDefault();
        return;
      }
      // Ctrl/Cmd+A「全选」：WebView 默认会选中整页 DOM 文本（含终端行），在终端语境里很
      // 尴尬（Ctrl+A 本应是 shell 的「移到行首」）。仅当焦点不在真正的文本输入框时拦截，输入框内
      // 保留原生全选以便编辑；终端里放行给 xterm（其隐藏 textarea 命中可编辑判定）→ ^A 正常送 shell。
      const mod = e.ctrlKey || e.metaKey;
      if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "a" && !isEditableTarget(e.target)) {
        e.preventDefault();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  // devtools：devMode 关时强制收起（含启动即同步一次，兜住 dev 构建里可能已打开的）；
  // 开时不主动弹出，由用户按 F12 / 右键自行打开。
  useEffect(() => {
    const sync = () => {
      if (!uiPrefsStore.get().devMode) void appApi.setDevtools(false).catch(() => {});
    };
    sync();
    return uiPrefsStore.subscribe(sync);
  }, []);
}

/**
 * 右键守卫判定：
 * - 终端区（.term-wrap）：始终放行，交给 Terminal 自己的自定义复制/粘贴菜单，这里不介入。
 * - 开发者模式开：整窗（终端外）都放行 WebView 原生右键菜单——其中的「检查」是 F12 之外
 *   打开 DevTools 的备用入口（有用户键盘 F12 失灵/被输入法吞掉，右键检查最可靠）。
 * - 开发者模式关：非终端区一律屏蔽原生菜单（含输入框），杜绝「检查」入口，保持去浏览器化。
 * 返回 true = 调用方应放行原生菜单（不 preventDefault）。
 */
export function allowNativeContextMenu(target: HTMLElement | null): boolean {
  if (target && target.closest(".term-wrap")) return true; // 终端自定义复制/粘贴菜单
  if (uiPrefsStore.get().devMode) return true; // 开发者模式：整窗放行原生菜单（含「检查」）
  return false; // 普通模式：屏蔽浏览器原生右键菜单
}
