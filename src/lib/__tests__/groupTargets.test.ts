// 发送目标列表的分组聚合单测（types.ts::groupTargetsByConnection）。
// 口径必须与侧栏一致，否则同一个会话在两个地方落到不同组里：
// 真实分组按 groups 次序、未分组置尾、空组省略（这列答的是「发给谁」，没收件人的组头是噪声）。
import { describe, expect, it } from "vitest";
import { groupTargetsByConnection } from "../../types";

const groups = [
  { id: "g-prod", name: "生产" },
  { id: "g-test", name: "测试" },
];
const profiles = [
  { id: "p1", groupId: "g-prod" },
  { id: "p2", groupId: "g-prod" },
  { id: "p3", groupId: "g-test" },
  { id: "p4", groupId: null }, // 建了档案但没分组
  { id: "p-deleted", groupId: "不存在" }, // 档案指向已删的组
];
const UN = "未分组";

describe("groupTargetsByConnection", () => {
  it("同组会话聚到一起，组按 groups 次序，未分组置尾", () => {
    const tabs = [
      { id: "s3", profileId: "p3" },
      { id: "s1", profileId: "p1" },
      { id: "s9", profileId: null }, // 直连会话 / 本地终端：没有档案
      { id: "s2", profileId: "p2" },
      { id: "s4", profileId: "p4" },
    ];
    const out = groupTargetsByConnection(tabs, profiles, groups, UN);
    expect(out.map((g) => g.name)).toEqual(["生产", "测试", UN]);
    expect(out[0].tabIds).toEqual(["s1", "s2"]);
    expect(out[2].tabIds).toEqual(["s9", "s4"]);
  });

  it("空组不出现；档案指向已删分组时落未分组（不凭空造组头）", () => {
    const out = groupTargetsByConnection([{ id: "x", profileId: "p-deleted" }], profiles, groups, UN);
    expect(out).toEqual([{ id: "", name: UN, tabIds: ["x"] }]);
  });

  it("档案 id 查不到（档案已删）也归未分组", () => {
    const out = groupTargetsByConnection([{ id: "x", profileId: "gone" }], profiles, [], UN);
    expect(out).toEqual([{ id: "", name: UN, tabIds: ["x"] }]);
  });

  it("没有任何会话 → 空列表（连未分组桶也不建）", () => {
    expect(groupTargetsByConnection([], profiles, groups, UN)).toEqual([]);
  });

  it("未分组桶的 id 固定为空串，与真实分组不会撞 key", () => {
    const out = groupTargetsByConnection([{ id: "x", profileId: null }], [], [], UN);
    expect(out[0].id).toBe("");
  });
});
