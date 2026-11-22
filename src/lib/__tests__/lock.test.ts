// 锁屏前端胶水单测（lockStore / lockClient）。
// 锁定：store 与后端回包的状态同步、refresh 失败静默（fail-open 于可用性）、
// `lock-changed` 广播翻态、Ctrl+Alt+L 热键、锁定期间输入闸门吞键但放行遮罩内、
// 未设 PIN 的手动锁屏只提示不发命令。
// 边界：PIN 校验与退避逻辑全在后端（Rust 侧 lock.rs 单测覆盖），这里只验 IPC 编排。
import { beforeEach, describe, expect, it, vi } from "vitest";
// 测桩专用：直引 client 模块取 invoke/listen 同一引用（不走 barrel，保公共出口只具规范化 API；
// setup.ts 的 vi.mock 挂在 @tauri-apps 包上，重导出链仍是同一 mock 函数）
import { invoke, listen } from "../tauri/client";
import { installLockClient, requestLock } from "../lockClient";
import { lockActions, lockStore } from "../../stores/lockStore";
import { toast, toastStore } from "../../stores/toastStore";

const invokeMock = vi.mocked(invoke);
const listenMock = vi.mocked(listen);

/** 按命令名分发的假后端；测试直接改字段，实现闭包始终读最新值。 */
const backend: {
  status: () => unknown;
  configSet: () => unknown;
  fail: (cmd: string) => boolean;
} = {
  status: () => ({ locked: false, pinSet: true, idleMinutes: 5, secqSet: true }),
  configSet: () => ({ locked: false, pinSet: true, idleMinutes: 30, secqSet: true }),
  fail: () => false,
};

/** listen 捕获到的事件回调：eventName → payload 手动推一发，模拟后端 emit。 */
const handlers = new Map<string, (e: { payload: unknown }) => void>();

type ListenCb = (e: { payload: unknown }) => void;

/** 微任务/定时器排干（installLockClient 的 refresh、requestLock 的 async 链路）。 */
const flush = () => new Promise((r) => setTimeout(r, 0));

beforeEach(async () => {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (backend.fail(cmd)) throw { code: "E_TEST", message: "boom" };
    if (cmd === "lock_status") return backend.status();
    if (cmd === "lock_config_set") return backend.configSet();
    if (cmd === "lock_pin_reset")
      return {
        ok: true,
        blocked: false,
        retryAfterSecs: 0,
        wrong: false,
        noSecq: false,
        status: { locked: false, pinSet: false, idleMinutes: 5, secqSet: false },
      };
    if (cmd === "lock_secq_question") return "你出生的城市？";
    return undefined;
  });
  listenMock.mockImplementation(((event: string, cb: ListenCb) => {
    handlers.set(event, cb);
    return Promise.resolve(() => {
      handlers.delete(event);
    });
  }) as typeof listen);
  // 每条用例开头都复位假后端：否则某条用例中途抛错（末尾的 cleanup 不执行）会把 fail 态泄漏给下一条。
  backend.fail = () => false;
  toast.clear();
});

/**
 * 闸门/订阅是 window 级监听、无法退订——整套用例只装一次（且避开 setup 期默认的
 * undefined invoke stub：先配好假后端再装）。install 内的 refresh 是异步的，装完排干。
 */
let installed = false;
async function ensureClient(): Promise<void> {
  if (installed) return;
  installLockClient();
  installed = true;
  await flush();
}

describe("lockStore / lockActions", () => {
  beforeEach(() => lockStore.set({ locked: false, pinSet: false, idleMinutes: 0, secqSet: false, setupOpen: false }));

  it("refresh 成功后同步后端三字段，本地 setupOpen 视图位不被刷掉", async () => {
    lockStore.set({ setupOpen: true });
    await lockActions.refresh();
    expect(lockStore.get()).toMatchObject({ locked: false, pinSet: true, idleMinutes: 5 });
    expect(lockStore.get().setupOpen).toBe(true);
    lockStore.set({ setupOpen: false });
  });

  it("refresh 失败静默：不抛错、不动状态（遮罩宁可不弹）", async () => {
    backend.fail = (cmd) => cmd === "lock_status";
    await expect(lockActions.refresh()).resolves.toBeUndefined();
    expect(lockStore.get()).toEqual({ locked: false, pinSet: false, idleMinutes: 0, secqSet: false, setupOpen: false });
    backend.fail = () => false;
  });

  it("setConfig 用回包同步 idleMinutes", async () => {
    await lockActions.setConfig(30);
    expect(lockStore.get().idleMinutes).toBe(30);
    expect(invokeMock).toHaveBeenCalledWith("lock_config_set", { idleMinutes: 30 });
  });

  it("resetPin（忘密码找回）带答案发命令、ok 回包翻锁+清 pinSet/secqSet 并透传 outcome", async () => {
    lockStore.set({ locked: true, pinSet: true, secqSet: true, idleMinutes: 5, setupOpen: false });
    const out = await lockActions.resetPin("beijing");
    expect(invokeMock).toHaveBeenCalledWith("lock_pin_reset", { answer: "beijing" });
    expect(out.ok).toBe(true);
    expect(lockStore.get()).toMatchObject({ locked: false, pinSet: false, secqSet: false, idleMinutes: 5 });
  });

  it("resetPin 退避：blocked 回包不翻锁、透传 retryAfterSecs 供倒计时", async () => {
    lockStore.set({ locked: true, pinSet: true, secqSet: true, idleMinutes: 5, setupOpen: false });
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === "lock_pin_reset"
        ? {
            ok: false,
            blocked: true,
            retryAfterSecs: 15,
            wrong: false,
            noSecq: false,
            status: { locked: true, pinSet: true, idleMinutes: 5, secqSet: true },
          }
        : undefined,
    );
    const out = await lockActions.resetPin("guess");
    expect(out.blocked).toBe(true);
    expect(out.retryAfterSecs).toBe(15);
    // blocked 时不应用 status（仍保持锁定）
    expect(lockStore.get().locked).toBe(true);
  });

  it("secqQuestion 直透后端问题原文供找回面板展示", async () => {
    expect(await lockActions.secqQuestion()).toBe("你出生的城市？");
    expect(invokeMock).toHaveBeenCalledWith("lock_secq_question", undefined);
  });
});

describe("lockClient 事件与热键", () => {
  it("启动即拉权威快照", async () => {
    await ensureClient();
    expect(lockStore.get().idleMinutes).toBe(5);
  });

  it("后端广播 lock-changed：各窗口镜像锁态翻态", async () => {
    await ensureClient();
    handlers.get("lock-changed")!({ payload: true });
    expect(lockStore.get().locked).toBe(true);
    handlers.get("lock-changed")!({ payload: false });
    expect(lockStore.get().locked).toBe(false);
  });

  it("未锁定时 Ctrl+Alt+L 触发后端 lock_now", async () => {
    await ensureClient();
    lockStore.set({ locked: false, pinSet: true, idleMinutes: 5 });
    invokeMock.mockClear();
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "l", ctrlKey: true, altKey: true, cancelable: true }),
    );
    await flush();
    // call() 无参命令也会把 undefined 透传给 invoke，断言对齐真实调用形参
    expect(invokeMock).toHaveBeenCalledWith("lock_now", undefined);
  });
});

describe("锁定输入闸门", () => {
  beforeEach(async () => {
    await ensureClient();
    lockStore.set({ locked: true, pinSet: true, idleMinutes: 5 });
  });

  it("锁定时遮罩外的按键被吞（preventDefault）", () => {
    const ev = new KeyboardEvent("keydown", { key: "a", cancelable: true, bubbles: true });
    window.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
  });

  it("遮罩内的输入放行——否则 PIN 都输不进去", () => {
    const overlay = document.createElement("div");
    overlay.className = "lock-overlay";
    const input = document.createElement("input");
    overlay.appendChild(input);
    document.body.appendChild(overlay);
    const ev = new KeyboardEvent("keydown", { key: "1", cancelable: true, bubbles: true });
    input.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false);
    overlay.remove();
  });

  it("锁定时 contextmenu 一并吞掉", () => {
    const ev = new MouseEvent("contextmenu", { cancelable: true, bubbles: true });
    window.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
  });
});

describe("requestLock 前置校验", () => {
  beforeEach(async () => {
    await ensureClient();
  });

  it("未设 PIN：拉起「设置 PIN」弹窗视图位，不向后端发 lock_now、不弹 toast", async () => {
    lockStore.set({ locked: false, pinSet: false, idleMinutes: 5, setupOpen: false });
    invokeMock.mockClear();
    await requestLock();
    expect(invokeMock).not.toHaveBeenCalledWith("lock_now", undefined);
    expect(lockStore.get().setupOpen).toBe(true);
    expect(toastStore.get().items).toHaveLength(0);
    lockStore.set({ setupOpen: false });
  });

  it("已设 PIN：直接发 lock_now（锁态翻转靠事件回推，本地不抢跑）", async () => {
    lockStore.set({ locked: false, pinSet: true, idleMinutes: 5, setupOpen: false });
    invokeMock.mockClear();
    await requestLock();
    expect(invokeMock).toHaveBeenCalledWith("lock_now", undefined);
    expect(toastStore.get().items).toHaveLength(0);
  });
});
