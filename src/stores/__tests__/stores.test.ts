// 状态层（stores）纯逻辑单测：createStore 原语 + 三个 localStorage/无后端的 store。
// 锁定：合并补丁语义、toast 同屏上限、uiPrefs 脏值逐项回落默认（永不因坏数据崩界面）、
// quickBar 关联表只存 id 引用与顺序（安全红线：绝不缓存 snippet.content）。
import { beforeEach, describe, expect, it } from "vitest";
import { createStore } from "../store";
import { notifyOsBypass, setOsNotify, setWinFocused, toast, toastStore } from "../toastStore";
import { awaitingTracks } from "../aiStore";
import type { AiApprovalPrompt, AiQuestionPrompt } from "../../types";
import { uiPrefs, uiPrefsStore, UIPREFS_DEFAULT } from "../uiPrefsStore";
import { profileKeyOf, quickBarActions, quickBarStore } from "../quickBarStore";

describe("createStore 原语", () => {
  it("对象补丁与函数补丁都合并进新状态并通知订阅者", () => {
    const s = createStore({ a: 1, b: 2 });
    let notified = 0;
    const off = s.subscribe(() => (notified += 1));
    s.set({ a: 5 });
    expect(s.get()).toEqual({ a: 5, b: 2 });
    s.set((prev) => ({ b: prev.a + 1 }));
    expect(s.get().b).toBe(6);
    expect(notified).toBe(2);
    off();
    s.set({ a: 0 });
    expect(notified).toBe(2); // 退订后不再收
  });
});

describe("toastStore", () => {
  beforeEach(() => {
    toast.clear();
    toast.clearHistory();
  });

  it("默认 info；error/warn/success 便捷方法带类型", () => {
    toast.show("hi");
    toast.error("boom");
    expect(toastStore.get().items.map((t) => t.kind)).toEqual(["info", "error"]);
  });

  it("同屏上限 5，超出丢最早的（防刷屏）", () => {
    for (let i = 0; i < 8; i++) toast.show(`m${i}`);
    const items = toastStore.get().items;
    expect(items.length).toBe(5);
    expect(items[0].message).toBe("m3"); // m0..m2 被淘汰
  });

  it("dismiss 撤单条、clear 清空", () => {
    toast.show("a");
    toast.show("b");
    const idA = toastStore.get().items[0].id;
    toast.dismiss(idA);
    expect(toastStore.get().items.map((t) => t.message)).toEqual(["b"]);
    toast.clear();
    expect(toastStore.get().items).toEqual([]);
  });

  it("show 同步写历史；clear 只清同屏不动历史；clearHistory 另清", () => {
    toast.show("a", "error");
    expect(toastStore.get().history.map((h) => [h.message, h.kind])).toEqual([["a", "error"]]);
    expect(toastStore.get().history[0].at).toBeGreaterThan(0);
    toast.clear();
    expect(toastStore.get().items).toEqual([]);
    expect(toastStore.get().history.length).toBe(1);
    toast.clearHistory();
    expect(toastStore.get().history).toEqual([]);
  });

  it("source 透传到同屏与历史；缺省不污染字段", () => {
    toast.error("boom", "root@10.0.0.1");
    toast.show("plain");
    expect(toastStore.get().items[0].source).toBe("root@10.0.0.1");
    expect(toastStore.get().history[0].source).toBe("root@10.0.0.1");
    expect(toastStore.get().history[1].source).toBeUndefined();
  });

  it("setOsNotify 只拦 OS 横幅旁路，应用内 toast 与历史不受影响", () => {
    setOsNotify(false);
    toast.error("off-switch", "root@10.0.0.1");
    expect(toastStore.get().items.length).toBe(1);
    expect(toastStore.get().history.length).toBe(1);
    setOsNotify(true);
  });

  it("聚焦时横幅挂起、失焦补发不炸；旁路不进应用内队列", () => {
    setWinFocused(true);
    toast.error("focused-err");
    notifyOsBypass(); // 只进挂起/发送通道，不新增 toast 条目
    setWinFocused(false); // 失焦瞬间 flush 挂起项（jsdom 无 Tauri 插件，发送静默降级）
    setWinFocused(true);
    expect(toastStore.get().items.length).toBe(1);
  });

  it("历史上限 100 丢最早；markRead 置已读时刻", () => {
    for (let i = 0; i < 105; i++) toast.show(`m${i}`);
    const h = toastStore.get().history;
    expect(h.length).toBe(100);
    expect(h[0].message).toBe("m5"); // m0..m4 被淘汰
    expect(h[99].message).toBe("m104");
    toast.markRead();
    expect(toastStore.get().readAt).toBeGreaterThanOrEqual(h[99].at);
  });
});

describe("uiPrefsStore 脏值回落", () => {
  beforeEach(() => uiPrefs.reset());

  it("合法更新落盘并可读回", () => {
    uiPrefs.set({ monitorInterval: 10, devMode: true });
    expect(uiPrefsStore.get().monitorInterval).toBe(10);
    expect(uiPrefsStore.get().devMode).toBe(true);
  });

  it("reset 恢复到默认常量（脏数据兜底路径的等价断言）", () => {
    uiPrefs.set({ panelHeight: 99999, lineHeight: 0, completionHistoryScope: "global" });
    uiPrefs.reset();
    expect(uiPrefsStore.get()).toEqual(UIPREFS_DEFAULT);
    localStorage.removeItem("sailterm.uiPrefs");
  });
});

describe("quickBarStore 关联表", () => {
  beforeEach(() => {
    quickBarStore.set({ byProfile: {}, collapsed: {} });
    localStorage.removeItem("sailterm.quickBar");
  });

  it("profileKeyOf：有档案走 profileId，否则 session 兜底", () => {
    expect(profileKeyOf({ profileId: "p1", id: "s1" } as never)).toBe("p1");
    expect(profileKeyOf({ profileId: null, id: "s1" } as never)).toBe("session:s1");
    expect(profileKeyOf(null)).toBe("");
  });

  it("addSnippet 追加去重、removeSnippet 空列表删键、reorder 移动", () => {
    quickBarActions.addSnippet("p", "a");
    quickBarActions.addSnippet("p", "b");
    quickBarActions.addSnippet("p", "a"); // 重复忽略
    expect(quickBarActions.listFor("p")).toEqual(["a", "b"]);
    quickBarActions.reorder("p", 0, 1);
    expect(quickBarActions.listFor("p")).toEqual(["b", "a"]);
    quickBarActions.removeSnippet("p", "b");
    quickBarActions.removeSnippet("p", "a");
    expect(quickBarStore.get().byProfile["p"]).toBeUndefined(); // 清空后删键
  });

  it("removeMany：部分移出保留键，移空同样删键", () => {
    quickBarActions.addMany("p", ["a", "b", "c"]);
    quickBarActions.removeMany("p", ["b"]);
    expect(quickBarActions.listFor("p")).toEqual(["a", "c"]);
    quickBarActions.removeMany("p", ["a", "c"]);
    expect(quickBarStore.get().byProfile["p"]).toBeUndefined();
  });

  it("空 key/id 不动作；forgetSession 只清 session 兜底键", () => {
    quickBarActions.addSnippet("", "x");
    quickBarActions.addSnippet("p", "");
    expect(quickBarStore.get().byProfile).toEqual({});
    quickBarActions.addSnippet("session:s1", "z");
    quickBarActions.forgetSession("s1");
    expect(quickBarStore.get().byProfile["session:s1"]).toBeUndefined();
  });
});

describe("awaitingTracks（等你回执角标判据）", () => {
  const appr = (sessionId: string, surface?: "agent" | "chat") =>
    ({ requestId: `a-${sessionId}-${surface}`, sessionId, surface }) as unknown as AiApprovalPrompt;
  const ques = (sessionId: string, surface?: "agent" | "chat") =>
    ({ requestId: `q-${sessionId}-${surface}`, sessionId, surface }) as unknown as AiQuestionPrompt;

  it("反问与审批同族：两类都计入所在轨", () => {
    const s = awaitingTracks([appr("s1", "agent")], [ques("s1", "chat")]);
    expect([...s].sort()).toEqual(["agent", "chat"]);
  });

  it("按会话过滤：别的会话在等不该点亮本会话的角标", () => {
    expect(awaitingTracks([appr("s2", "chat")], [], "s1").size).toBe(0);
    expect(awaitingTracks([appr("s2", "chat")], [], "s2").has("chat")).toBe(true);
  });

  it("省略 sessionId = 全量聚合；surface 缺失（未经 surfaceOf 归一的脏数据）不计入", () => {
    expect(awaitingTracks([appr("s1")], [], null).size).toBe(0);
    expect(awaitingTracks([], [ques("s1", "agent")]).has("agent")).toBe(true);
  });
});
