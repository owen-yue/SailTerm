// 独立会话窗口（多窗口 EXT）：把某个会话弹出成单独的子窗口（窗口能力经 `lib/tauri/window` 适配）。
//
// 新窗口加载同一前端（`/?pop=<sessionId>`），main.tsx 据此路由到 PopWorkspace 极简工作区。
// 每个 WebView 有独立 JS 上下文 → panelStore/fileStore 等天然每窗口一份（独立下栏）。
// 后端 SSH 会话是全局 Rust 状态，两窗口通过 sessionId 访问同一会话；主窗口对 popped
// 会话不渲染 Terminal，避免双挂。窗口销毁事件在**创建方**（主窗口）上下文触发，
// 由这里传的 onDestroyed 回调负责 `unpop` 把会话归还主窗口。
import { sessionActions, sessionStore } from "../stores/sessionStore";
import { toast } from "../stores/toastStore";
import { t } from "./i18n";
import { sessionNameOf } from "./sessionName";
import { createDetachedWindow, focusDetachedWindow } from "./tauri/window";
import type { Tab } from "../stores/sessionStore";

/** 本 WebView 是否为弹出窗口（main.tsx 路由前写入）。 */
let popSid: string | null = null;
export function setPopSession(sid: string | null): void {
  popSid = sid;
}
export function getPopSession(): string | null {
  return popSid;
}

/**
 * 会话归属判定：弹出窗口只认自己的 sid，主窗口认所有未弹出的会话。
 * 用于 AI 审批 / zmodem 等全局事件的监听过滤，避免两窗同时弹同一对话框。
 */
export function ownsSession(sid: string): boolean {
  if (popSid) return sid === popSid;
  return !sessionStore.get().popped[sid];
}

/** 会话窗口 label：capability `pop` 按 `sess-*` 通配授权。 */
export function sessionWindowLabel(sessionId: string): string {
  return `sess-${sessionId}`;
}

/** 弹出（或重新弹出）会话为独立窗口；label 冲突 = 窗口已在，直接聚焦。 */
export async function popSessionWindow(sessionId: string, title: string): Promise<void> {
  const label = sessionWindowLabel(sessionId);
  if (await focusDetachedWindow(label)) return;
  sessionActions.pop(sessionId);
  createDetachedWindow({
    label,
    url: `/?pop=${encodeURIComponent(sessionId)}`,
    title,
    width: 1280,
    height: 800,
    minWidth: 720,
    minHeight: 480,
    // 独立窗口关闭 → 会话归还主窗口（终端重挂，靠 ring 缓冲回放恢复近期屏幕）
    onDestroyed: () => sessionActions.unpop(sessionId),
    onError: (payload) => {
      toast.show(t("wd.popFail").replace("{m}", JSON.stringify(payload)));
      sessionActions.unpop(sessionId);
    },
  });
}

/** 聚焦某会话的独立窗口（不在则忽略）。 */
export async function focusSessionWindow(sessionId: string): Promise<void> {
  await focusDetachedWindow(sessionWindowLabel(sessionId));
}

/** 从标签/窗格弹出：窗口标题用会话显示名（重名带 #n，与主窗口页签同口径；本地终端名按当前语言解析）。 */
export function popTabWindow(tab: Tab): void {
  void popSessionWindow(tab.id, `${sessionNameOf(sessionStore.get().tabs, t, tab)} — SailTerm`);
}
