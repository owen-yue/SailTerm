// 窗口能力收口（`lib/tauri` 适配层）：独立窗口创建/聚焦、首帧揭示、主题与当前窗口操作。
//
// 这里包装 Tauri 的窗口模型（WebviewWindow / getCurrentWindow），业务层只见平台无关语义。
// 保留两条踩坑固化模式（改动前务必读）：
// 1. **程序化创建的独立窗口不受 tauri.macos.conf.json 覆盖**，必须在构造参数里按平台
//    自设 Overlay 红绿灯（chromeOptions），否则 mac 下原生红绿灯消失而自绘控件在 mac 又
//    返回 null → 窗口没有任何控制按钮。
// 2. **WKWebView 首帧白闪根治 = 隐藏建窗 + 首帧后 show**。揭示绝不能用 rAF 等待：窗口
//    occluded 时 WKWebView 不调度 rAF，会拖到兜底超时才亮窗。revealWindow 直接
//    setTimeout(show, 0)——主题已由 main.tsx 从 localStorage 缓存同步预着色，亮窗即合色。
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { getCurrentWindow, type WindowOptions } from "@tauri-apps/api/window";
import { isMacOS } from "../platform";
import type { UnlistenFn } from "./client";

/** Tauri 窗口背景色：RGBA 元组（各分量 0-255）。 */
export type Rgba = [number, number, number, number];

/** 独立窗口的标题栏装饰选项，按平台分流（见文件头坑 1）。 */
function chromeOptions(): Pick<WindowOptions, "decorations" | "titleBarStyle" | "hiddenTitle"> {
  return isMacOS()
    ? { decorations: true, titleBarStyle: "overlay", hiddenTitle: true }
    : { decorations: false };
}

/** 延迟揭示·创建侧（见文件头坑 2）：macOS 先隐藏新建窗口，由窗口内前端 revealWindow 亮。 */
function deferredRevealOptions(): Pick<WindowOptions, "visible"> {
  return isMacOS() ? { visible: false } : {};
}

/** 独立窗口创建参数：平台 chrome/揭示在内部叠加，调用方不接触 Tauri WindowOptions。 */
export interface DetachedWindowSpec {
  /** 窗口 label（capability 按前缀通配授权，如 `sess-*` / `tool-*`）。 */
  label: string;
  /** 应用内导航地址，如 `/?pop=<id>` / `/?tool=<id>`。 */
  url: string;
  title: string;
  width: number;
  height: number;
  minWidth: number;
  minHeight: number;
  /** 固定居中弹出：不指定时由 OS 决定初始位置。 */
  center?: boolean;
  /** 原生背景预设（当前主题底色）：内容绘制前不闪白屏（仅 macOS 外的首帧保障，mac 靠隐藏揭示）。 */
  backgroundColor?: Rgba;
  /** 窗口销毁回调（在创建方上下文触发）：会话归还等业务挂这里。 */
  onDestroyed?: (label: string) => void;
  /** 建窗失败回调。 */
  onError?: (payload: unknown) => void;
}

/** 独立窗口已存在则聚焦并返回 true（label 冲突 = 已在，不重复建窗是强约定）。 */
export async function focusDetachedWindow(label: string): Promise<boolean> {
  const win = await WebviewWindow.getByLabel(label);
  if (!win) return false;
  await win.setFocus().catch(() => {});
  return true;
}

/** 创建独立窗口：叠加 chromeOptions/deferredRevealOptions/shadow，并挂 destroyed/error 监听。 */
export function createDetachedWindow(spec: DetachedWindowSpec): void {
  const win = new WebviewWindow(spec.label, {
    url: spec.url,
    title: spec.title,
    width: spec.width,
    height: spec.height,
    minWidth: spec.minWidth,
    minHeight: spec.minHeight,
    ...(spec.center ? { center: true } : {}),
    // macOS：原生 Overlay 红绿灯（同主窗口），其余：decorations:false 自绘控件
    ...chromeOptions(),
    // macOS：先隐藏建窗，由新窗口挂载后 revealWindow 立即亮（主题已同步预着色，不闪白）
    ...deferredRevealOptions(),
    shadow: true,
    ...(spec.backgroundColor ? { backgroundColor: spec.backgroundColor } : {}),
  });
  if (spec.onDestroyed) {
    const cb = spec.onDestroyed;
    void win.once("tauri://destroyed", () => cb(spec.label));
  }
  win.once("tauri://error", (e) => spec.onError?.(e.payload));
}

/**
 * 延迟揭示·揭示侧：新窗口自身组件挂载后调用（建窗时经 deferredRevealOptions 隐藏）。
 * 切记不用 requestAnimationFrame（见文件头坑 2）。show 本身经 IPC 异步生效，
 * 那一帧前 WebView 已把预着色的 DOM 画好，无需额外等待。
 */
export function revealWindow(): void {
  const win = getCurrentWindow();
  // 让出一个任务再 show：确保 React 本次 commit（顶栏/主体 DOM）已落地，亮窗即见骨架。
  setTimeout(() => {
    void win.show().catch(() => {});
    void win.setFocus().catch(() => {});
  }, 0);
}

/** 原生窗口标题栏跟 UI 亮暗一致（Windows 浅色标题栏会显得突兀）。 */
export async function setWindowTheme(mode: "light" | "dark"): Promise<void> {
  await getCurrentWindow().setTheme(mode);
}

/** 当前窗口操作（自绘窗口控件 WindowControls 消费）。 */
export const selfWindow = {
  minimize: (): Promise<void> => getCurrentWindow().minimize(),
  toggleMaximize: (): Promise<void> => getCurrentWindow().toggleMaximize(),
  close: (): Promise<void> => getCurrentWindow().close(),
  isMaximized: (): Promise<boolean> => getCurrentWindow().isMaximized(),
  onResized: (cb: () => void): Promise<UnlistenFn> => getCurrentWindow().onResized(cb),
  /**
   * 焦点进出（true = 获得焦点）：非终端的独立窗口靠它补拉「别处已经变了」的状态——
   * 后端初始建连不发 `session-state`（结果直接随命令返回），新开的会话在本窗口里没有任何事件可跟。
   */
  onFocusChanged: (cb: (focused: boolean) => void): Promise<UnlistenFn> =>
    getCurrentWindow().onFocusChanged(({ payload: focused }) => cb(focused)),
};
