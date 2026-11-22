// 全局命令注册表（lib/commands）单测：命令面板与快捷键帮助共用的单一事实源。
// 锁定三件事：feature 关的命令必须被剔除、分组顺序里的键都有归属、fuzzyScore 的
// 过滤/排序语义（面板即时搜索全靠它）。
import { describe, expect, it } from "vitest";
import {
  COMMANDS,
  COMMAND_GROUP_ORDER,
  EXTRA_SHORTCUTS,
  HELP_GROUP_ORDER,
  fuzzyScore,
  type CommandCtx,
} from "../commands";
import { FEATURES } from "../features";

describe("COMMANDS 注册表单源", () => {
  it("id 唯一，labelKey/groupKey 齐备", () => {
    const ids = COMMANDS.map((c) => c.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const c of COMMANDS) {
      expect(c.labelKey).toBeTruthy();
      expect(c.groupKey).toBeTruthy();
    }
  });

  it("feature 关闭的命令不出现在用户可见列表", () => {
    const gated = Object.keys(FEATURES).filter((k) => !FEATURES[k as keyof typeof FEATURES]);
    for (const id of gated) {
      expect(COMMANDS.some((c) => c.feature === id)).toBe(false);
    }
  });

  it("每条命令的 groupKey 都在面板分组顺序里（否则分区渲染漏组）", () => {
    for (const c of COMMANDS) {
      expect(COMMAND_GROUP_ORDER).toContain(c.groupKey);
    }
  });

  it("设置分组只保留「通用」一个入口（分类导航在设置中心内部）", () => {
    expect(COMMANDS.filter((c) => c.groupKey === "cmd.groupSettings").map((c) => c.id)).toEqual([
      "settings.general",
    ]);
  });

  it("run 只经注入上下文执行，按 id 调用对应动作", () => {
    const calls: string[] = [];
    const ctx = new Proxy(
      {},
      {
        get: (_target, prop: string) => () => {
          calls.push(prop);
        },
      },
    ) as unknown as CommandCtx;
    COMMANDS.find((c) => c.id === "settings.general")!.run(ctx);
    expect(calls).toEqual(["openSettings"]);
  });
});

describe("EXTRA_SHORTCUTS / 帮助分组", () => {
  it("纯键位行的分组都在 HELP_GROUP_ORDER 内", () => {
    for (const r of EXTRA_SHORTCUTS) expect(HELP_GROUP_ORDER).toContain(r.groupKey);
  });
});

describe("fuzzyScore", () => {
  it("空查询恒命中（得分 0，用于面板初始全量展示）", () => {
    expect(fuzzyScore("", "anything")).toBe(0);
    expect(fuzzyScore("   ", "anything")).toBe(0);
  });

  it("任一词未命中即整体淘汰（-1），大小写不敏感", () => {
    expect(fuzzyScore("settings", "打开设置中心")).toBe(-1);
    expect(fuzzyScore("SETTINGS", "settings.general 设置")).toBeGreaterThan(0);
    expect(fuzzyScore("settings", "cmd.settingsGeneral")).toBeGreaterThan(0);
  });

  it("多词按空格切分、都得命中；越靠文本前部得分越高", () => {
    const hit = fuzzyScore("ai chat", "cmd.aiChat 打开 AI 聊天");
    expect(hit).toBeGreaterThan(0);
    expect(fuzzyScore("ai zzz", "cmd.aiChat")).toBe(-1);
    expect(fuzzyScore("set", "settings.general")).toBeGreaterThan(fuzzyScore("set", "x".repeat(70) + "settings"));
  });
});
