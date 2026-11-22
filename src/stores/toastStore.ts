// toastStore：全局通知列表（右上角滑入卡片，见 components/Toasts.tsx）。
// 每条带类型：error 常驻需手动关，其余（warn/info/success）定时自动淡出。
// App 顶部告警、连接/传输反馈、触发器命中等复用同一展示。
// 通知会瞬时消失，但报错不能随之丢了：show() 同步写一份内存历史（顶栏铃铛面板查看）。
// 面板是应用级全局聚合，连接级事件靠每条自带的 source 区分。
// 历史不落盘：报错文本可能含主机名/路径等敏感信息，应用重启即清。
// OS 旁路同姿态：系统通知中心会被持久化/锁屏可见/转发其他设备，
// 横幅只发通用文案不带主机名与报错原文（见 dispatchOs）。
// OS 通知（系统横幅）是同一链路旁路：见 show() 里的 dispatchOs。
import { createStore } from "./store";
import { t } from "../lib/i18n";

/** 通知类型：决定色条/图标与是否自动消失。 */
export type ToastKind = "error" | "warn" | "info" | "success";

export interface ToastItem {
  id: number;
  message: string;
  kind: ToastKind;
  /** 来源标识：会话显示名（如 root@10.0.0.1）或功能区域名；缺省 = 应用级事件。 */
  source?: string;
}

/** 一条历史记录：同 ToastItem 加产生时刻（顶栏铃铛面板按时间展示）。 */
export interface ToastRecord extends ToastItem {
  at: number;
}

let _seq = 0;

/** 主窗口是否聚焦（App 挂 onFocusChanged 同步）：true=用户在看，假=可能切走了。 */
let _winFocused = true;
export function setWinFocused(focused: boolean): void {
  _winFocused = focused;
  // 失焦瞬间补发挂起期间的横幅，切走时也能看到攒下的事件。
  if (!focused) flushPendingOs();
}

/** OS 横幅总开关（设置项 osNotifications，settingsStore 加载/更新时同步）：关=只走应用内 toast。 */
let _osNotify = true;
export function setOsNotify(on: boolean): void {
  _osNotify = on;
}

/** OS 横幅事件类别：ToastKind 全集去掉 info（info 不发横幅）+「等你回执」两类（审批/反问）。
 *  回执类不并入 ToastKind——它们走自己的文案键，不是应用内 toast。 */
type OsNote = Exclude<ToastKind, "info"> | "approval" | "question";

/** 聚焦期间挂起的横幅：失焦时按序补发（上限 20 条，只发最新）。 */
const _pendingOs: OsNote[] = [];
const PENDING_MAX = 20;

/** 横幅通用文案键：重大事件按类别映射，一律不含主机名/正文/命令原文。 */
function osBody(k: OsNote): string {
  if (k === "success") return t("notify.osSuccess");
  if (k === "approval") return t("notify.osAwaitApproval");
  if (k === "question") return t("notify.osAwaitQuestion");
  return t("notify.osProblem");
}

function sendOs(k: OsNote): void {
  const body = osBody(k);
  import("../lib/tauri/notify")
    .then((m) => void m.notifyOs(body))
    .catch(() => {});
}

/** 挂起待失焦补发：同类别去重（后到覆盖先到），避免同一件事攒一串横幅。 */
function pushPendingOs(k: OsNote): void {
  const i = _pendingOs.indexOf(k);
  if (i >= 0) _pendingOs.splice(i, 1);
  if (_pendingOs.length >= PENDING_MAX) _pendingOs.shift();
  _pendingOs.push(k);
}

function flushPendingOs(): void {
  if (!_osNotify) { _pendingOs.length = 0; return; }
  const batch = _pendingOs.splice(0, _pendingOs.length);
  for (const k of batch) sendOs(k);
}

/**
 * OS 横幅分发策略：warn/error/success 发横幅，info 不发（闲聊级不进系统通知中心）。
 * 时序门控（同一事件永远只有一条通道）：失焦立即发；聚焦时先挂起，
 * 失焦瞬间补发——修复「聚焦时出的错、切走后永远看不到横幅」的盲区。
 * 安全：横幅只发通用文案，正文/主机名不进系统通知中心（它会被持久化、锁屏可见、
 * 跨设备转发）；详情回应用内 toast/历史看。
 * 动态 import：单测/浏览器环境没有 Tauri 插件时静默降级。
 */
function dispatchOs(kind: OsNote): void {
  if (!_osNotify) return;
  if (_winFocused) {
    pushPendingOs(kind);
    return;
  }
  sendOs(kind);
}

/** 不走 toast 队列的错误通道（会话/连接/传输的 error 字段等）补发 OS 横幅：
 *  门控与 dispatchOs 同策略；同样只发通用文案；重复抑制由调用方负责。 */
export function notifyOsBypass(): void {
  dispatchOs("error");
}

/** AI 回复完成时发 OS 通知：仅窗口失焦时才发（聚焦时用户已经能看到结果，不打断）。 */
export function notifyOsReply(): void {
  if (!_osNotify || _winFocused) return;
  const body = t("ast.replyDoneNotify");
  import("../lib/tauri/notify")
    .then((m) => void m.notifyOs(body))
    .catch(() => {});
}

/** 「等你回执」类卡片（澄清反问 / 命令审批）入队时发 OS 通知：
 *  与 dispatchOs 同时序门控——失焦立即发；聚焦时挂起、失焦瞬间补发。
 *  早前版本聚焦时直接丢弃，但反问恰恰常在用户盯着 SailTerm 时到达，切走的瞬间
 *  最该看到「有问题等你回答」——挂起补发才对齐「离屏提醒」的本意。
 *  尊重 osNotifications 总开关；只发通用文案，问题/命令原文不进系统通知中心。 */
export function notifyOsAwait(kind: "approval" | "question"): void {
  dispatchOs(kind);
}

export const toastStore = createStore<{ items: ToastItem[]; history: ToastRecord[]; readAt: number }>({
  items: [],
  history: [],
  readAt: 0,
});

/** 同屏最多保留的通知条数，超出丢最早的，避免刷屏。 */
const MAX = 5;
/** 历史上限：够回溯一阵子的报错，又不至于无界增长。 */
const MAX_HISTORY = 100;

export const toast = {
  /** 推入一条通知；`kind` 默认 info（轻量提示）。error 会常驻，其余自动淡出。
   *  `source` 为产生这条通知的会话/区域名，历史面板按它标注来源；连接级事件应传。 */
  show(message: string, kind: ToastKind = "info", source?: string): void {
    const id = ++_seq;
    const at = Date.now();
    toastStore.set((prev) => ({
      items: [...prev.items.slice(-(MAX - 1)), { id, message, kind, source }],
      history: [...prev.history.slice(-(MAX_HISTORY - 1)), { id, message, kind, source, at }],
    }));
    // 安全红线：info 级闲聊不发系统横幅（对齐旧行为，过滤收口在这里）。
    if (kind !== "info") dispatchOs(kind);
  },
  error(message: string, source?: string): void {
    toast.show(message, "error", source);
  },
  warn(message: string, source?: string): void {
    toast.show(message, "warn", source);
  },
  success(message: string, source?: string): void {
    toast.show(message, "success", source);
  },
  /** 撤掉指定一条（卡片淡出动画结束后调用）。 */
  dismiss(id: number): void {
    toastStore.set((prev) => ({ items: prev.items.filter((t) => t.id !== id) }));
  },
  /** 清空同屏卡片（切换连接/新建会话等场景抹掉历史残留）；不动历史记录。 */
  clear(): void {
    toastStore.set({ items: [] });
  },
  /** 清空历史记录（铃铛面板的「清空」）。 */
  clearHistory(): void {
    toastStore.set({ history: [] });
  },
  /** 铃铛面板打开时记一次已读时刻，未读数 = at > readAt 的条数。 */
  markRead(): void {
    toastStore.set({ readAt: Date.now() });
  },
};
