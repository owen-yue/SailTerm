// 锁屏（应用级）IPC 适配：状态 / 手动锁 / 活动心跳 / 解锁 / PIN 设置与清除 / 空闲配置。
// 锁态单源在 Rust 后端（多 WebView 独立 JS 上下文，只有后端能保证「一锁全锁」），
// 这里只做命令收口；`lock-changed` 广播的消费者在 `lib/lockClient`。
// 安全红线：明文 PIN 只作命令入参瞬时直送后端，本层与任何 store 都不留存。
import type { LockStatus, ResetOutcome, UnlockOutcome } from "../../types";
import { call } from "./client";

/** 锁态变更广播事件（payload=是否锁定），全窗口可达（含 `sess-*` 弹出窗与工具窗）。 */
export const EVENT_LOCK_CHANGED = "lock-changed";

export const lock = {
  /** 状态快照：纯原子读，同步返回，无超时风险。 */
  status: () => call<LockStatus>("lock_status"),

  /** 用户活动心跳（`lockClient` 按 ≥10s 节流上报；后端空闲看门狗据此判定）。 */
  touch: () => call<void>("lock_touch"),

  /** 立即锁屏；未设 PIN 报 `err.lock.noPin`。成功后锁定经事件回推，无需本地翻转。 */
  now: () => call<void>("lock_now"),

  /** 用 PIN 解锁；预期结果（错码/退避）走返回值而非错误通道。 */
  unlock: (pin: string) => call<UnlockOutcome>("lock_unlock", { pin }),

  /** 设置/修改 PIN（4-6 位数字）；已存在时必带旧码。首次建 PIN 必带密保问答（question/answer）。 */
  pinSet: (
    pin: string,
    oldPin: string | null,
    secqQuestion?: string | null,
    secqAnswer?: string | null,
  ) => call<LockStatus>("lock_pin_set", { pin, oldPin, secqQuestion, secqAnswer }),

  /** 移除 PIN = 关闭锁屏（须验旧；若正锁定会一并解锁）。 */
  pinClear: (oldPin: string) => call<LockStatus>("lock_pin_clear", { oldPin }),

  /** 读密保问题原文（找回时展示）；未配时后端返 null。 */
  secqQuestion: () => call<string | null>("lock_secq_question"),

  /** 忘记 PIN 的找回重置（不验旧、验密保答案）：预期结果（错码/退避/无密保）走返回值，绝不误伤其他凭证。 */
  pinReset: (answer: string) => call<ResetOutcome>("lock_pin_reset", { answer }),

  /** 空闲自动锁屏阈值（分钟，0=关闭，≤240）。 */
  configSet: (idleMinutes: number) => call<LockStatus>("lock_config_set", { idleMinutes }),
};
