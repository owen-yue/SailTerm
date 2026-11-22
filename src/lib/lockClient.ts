// 锁屏前端胶水：main.tsx 顶层安装一次（同 installModalDrag 的理由——本前端按 URL 分
// 三个根：主窗口 App / 会话弹出窗 PopWorkspace / 工具窗 ToolWindow，每个 WebView 独立
// document。锁态广播订阅、空闲心跳、锁定输入闸门都必须三根全覆盖，不能挂进某个根）。
//
// 分工：锁态判定与 PIN 校验全在 Rust 后端（commands::lock）；这里只做三件事——
// 1) 订阅 `lock-changed` 把锁态镜像进 lockStore（各窗口据此渲染/撤下 LockOverlay）；
// 2) 有操作时按 ≥10s 节流上报 `lock_touch` 心跳（空闲判定聚合在后端单一时钟，
//    绝不在前端做窗口级倒计时——那会「别的窗口忙着、本窗口数空闲」误锁）；
// 3) 锁定期间在 window capture 层吞掉一切不属于遮罩的按键/右键/拖放。
import { EVENT_LOCK_CHANGED, errMsg, lock as api, on } from "./tauri";
import { lockActions, lockStore } from "../stores/lockStore";
import { toast } from "../stores/toastStore";

/** 心跳节流（毫秒）：有活动且距上次上报 ≥ 此值才补一枪；空闲阈值是分钟级，够精细。 */
const TOUCH_EVERY_MS = 10_000;

export function installLockClient(): void {
  void lockActions.refresh();
  void on<boolean>(EVENT_LOCK_CHANGED, (locked) => lockStore.set({ locked }));
  installActivityBeat();
  installInputGate();
}

/**
 * 手动锁屏（顶栏按钮、命令面板与 Ctrl+Alt+L 共用）。未设 PIN：直接拉起「设置 PIN」
 * 弹窗（PinSetupDialog 在三根挂载），设完顺势锁屏——不再要求用户自己去设置页。
 */
export async function requestLock(): Promise<void> {
  if (!lockStore.get().pinSet) {
    lockStore.set({ setupOpen: true });
    return;
  }
  try {
    await lockActions.lockNow();
  } catch (e) {
    toast.error(errMsg(e));
  }
}

/** 活动采集 → 节流心跳：只记「这段时间动过」，发送由拍频决定（≤ 每窗口 0.1 次/秒）。 */
function installActivityBeat(): void {
  let pending = false;
  let lastSent = 0;
  const mark = () => {
    pending = true;
  };
  for (const ev of ["pointerdown", "keydown", "wheel"] as const) {
    window.addEventListener(ev, mark, { capture: true, passive: true });
  }
  window.setInterval(() => {
    if (!pending) return;
    const { locked, idleMinutes } = lockStore.get();
    if (locked || idleMinutes === 0) {
      pending = false;
      return;
    }
    const now = Date.now();
    // 未达节流窗：pending 保留，下一拍补发（活动不能丢，丢了会提前触发空闲锁定）。
    if (now - lastSent < TOUCH_EVERY_MS) return;
    lastSent = now;
    pending = false;
    void api.touch().catch(() => {});
  }, 5_000);
}

/**
 * 锁定输入闸门：window capture 且注册最早（main.tsx 模块期先于一切组件 effect），
 * `stopImmediatePropagation` 才能抢在 App 的 F11/命令面板等 capture 监听之前把键吞掉。
 * 例外：目标在 `.lock-overlay` 内（PIN 输入/解锁按钮）一律放行——否则连解锁都输不进字。
 */
function installInputGate(): void {
  const inOverlay = (e: Event): boolean => {
    const el = e.target as Element | null;
    return !!el && typeof el.closest === "function" && !!el.closest(".lock-overlay");
  };
  const blockIfLocked = (e: Event): void => {
    if (!lockStore.get().locked || inOverlay(e)) return;
    e.preventDefault();
    e.stopImmediatePropagation();
  };
  window.addEventListener(
    "keydown",
    (e: KeyboardEvent) => {
      if (lockStore.get().locked) {
        blockIfLocked(e);
        return;
      }
      const mod = e.ctrlKey || e.metaKey;
      // Ctrl/Cmd+Alt+L：全局手动锁屏（capture 装，抢在 xterm 把组合键打进 PTY 之前）。
      // Ctrl+Shift+L 已被「新开本地终端」占用多年，不与之抢键。
      if (mod && e.altKey && !e.shiftKey && e.key.toLowerCase() === "l") {
        e.preventDefault();
        e.stopPropagation();
        void requestLock();
      }
    },
    true,
  );
  // 右键菜单 / 文件拖入终端 / 拖放外链：锁定期间一并吞掉（pointer 已被遮罩物理挡住，
  // 这三个却走 window 级监听，不拦会从遮罩底下漏出去）。
  for (const ev of ["contextmenu", "dragover", "drop"] as const) {
    window.addEventListener(ev, blockIfLocked, true);
  }
}
