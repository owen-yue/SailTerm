// 锁屏 store：只镜像后端锁态快照（单源在 Rust，见 commands::lock），供各窗口渲染遮罩。
// 事件订阅与空闲心跳不在这里（在 lib/lockClient——main.tsx 顶层安装才能覆盖三个根）；
// 本 store 是纯视图态：后端推 `lock-changed` / 命令回包时由外部 set。
// 安全红线：PIN 明文只作为 unlock/setPin 的瞬时入参直送后端，绝不落这里。
import { lock as api } from "../lib/tauri";
import type { LockStatus, ResetOutcome, UnlockOutcome } from "../types";
import { createStore } from "./store";

type LockUiState = LockStatus;

export const lockStore = createStore<LockUiState>({
  locked: false,
  pinSet: false,
  idleMinutes: 0,
  secqSet: false,
  setupOpen: false,
});

export const lockActions = {
  /** 启动/进设置页时拉权威快照（失败静默：默认未锁、未启用，不阻断 UI）。 */
  async refresh(): Promise<void> {
    try {
      const s = await api.status();
      // 后端无 setupOpen 字段（整包替换会把弹窗闪掉），保留本地视图位。
      lockStore.set({ ...s, setupOpen: lockStore.get().setupOpen });
    } catch {
      // 后端未就绪/命令异常：遮罩宁可不弹（fail-open 于可用性），PIN 校验仍在后端兜底
    }
  },

  /** 立即锁屏（事件会回推 locked=true，本地不翻态）。未设 PIN 时后端报 err.lock.noPin。 */
  lockNow: () => api.now(),

  /** 解锁：预期结果（错码/退避）走返回值；成功时 locked 同样靠事件回推统一路径。 */
  unlock(pin: string): Promise<UnlockOutcome> {
    return api.unlock(pin);
  },

  /** 设置/修改 PIN；用回包同步 pinSet。首次建立需带密保问答。 */
  async setPin(
    pin: string,
    oldPin: string | null,
    secqQuestion?: string | null,
    secqAnswer?: string | null,
  ): Promise<void> {
    lockStore.set(await api.pinSet(pin, oldPin, secqQuestion, secqAnswer));
  },

  /** 移除 PIN = 关闭锁屏；正锁定时后端会一并解锁并广播。 */
  async clearPin(oldPin: string): Promise<void> {
    lockStore.set(await api.pinClear(oldPin));
  },

  /**
   * 忘记 PIN 的找回重置（锁屏遮罩答题后调）：不验旧、验密保答案，答案对才清 PIN 并解锁。
   * 预期结果（ok/blocked/wrong/noSecq）走返回值：ok 时用快照翻态（pinSet/secqSet/locked 转 false），
   * 其余分支由调用方（答题面板）展示倒计时/错码，不再靠报错。
   */
  async resetPin(answer: string): Promise<ResetOutcome> {
    const out = await api.pinReset(answer);
    if (out.ok) lockStore.set(out.status);
    return out;
  },

  /** 读密保问题原文（找回面板展示用）；未配时返回 null。 */
  secqQuestion: (): Promise<string | null> => api.secqQuestion(),

  /** 空闲阈值（分钟，0=关闭）。 */
  async setConfig(idleMinutes: number): Promise<void> {
    lockStore.set(await api.configSet(idleMinutes));
  },
};
