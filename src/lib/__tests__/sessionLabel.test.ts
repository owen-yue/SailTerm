// 会话显示名单测：sessionLabel / disambiguateLabels（都在 types.ts，纯函数无副作用）。
// 锁定发送目标列表最关键的一条：同主机多会话时，重名**一律**带 #n（含第一个）——
// 这一列答的是「往哪个会话灌命令」，让用户去推断谁是一号就会点错机器。
import { describe, expect, it } from "vitest";
import { disambiguateLabels, sessionLabel } from "../../types";

// 真实词典里 topbar.local.title = 「本地终端」；测试用简化翻译函数即可。
const t = (k: string) => (k === "topbar.local.title" ? "Local Terminal" : k);

describe("sessionLabel", () => {
  it("本地终端的名字是后端写死的数据字段，按当前语言实时解析", () => {
    expect(sessionLabel({ kind: "local", label: "本地终端" }, t)).toBe("Local Terminal");
  });

  it("其余协议原样展示后端 label（user@host 等真实数据）", () => {
    expect(sessionLabel({ kind: "ssh", label: "root@10.0.0.1" }, t)).toBe("root@10.0.0.1");
  });
});

describe("disambiguateLabels", () => {
  it("不重名时不加任何下标", () => {
    const out = disambiguateLabels(
      [
        { id: "1", kind: "ssh", label: "root@a" },
        { id: "2", kind: "ssh", label: "root@b" },
      ],
      t,
    );
    expect(out.get("1")).toBe("root@a");
    expect(out.get("2")).toBe("root@b");
  });

  it("重名一律带 #n，第一个也带（不做「第一个不标」）", () => {
    const out = disambiguateLabels(
      [
        { id: "1", kind: "ssh", label: "root@a" },
        { id: "2", kind: "ssh", label: "root@a" },
        { id: "3", kind: "ssh", label: "root@a" },
      ],
      t,
    );
    expect(out.get("1")).toBe("root@a #1");
    expect(out.get("2")).toBe("root@a #2");
    expect(out.get("3")).toBe("root@a #3");
  });

  it("序号按组独立计（各自从 1 起），交错输入也不串号", () => {
    const out = disambiguateLabels(
      [
        { id: "1", kind: "ssh", label: "root@a" },
        { id: "2", kind: "ssh", label: "root@b" },
        { id: "3", kind: "ssh", label: "root@a" },
        { id: "4", kind: "ssh", label: "root@b" },
      ],
      t,
    );
    expect([...out.values()]).toEqual(["root@a #1", "root@b #1", "root@a #2", "root@b #2"]);
  });

  it("两个本地终端同名（都解析成同一 i18n 文案）同样要消歧", () => {
    const out = disambiguateLabels(
      [
        { id: "1", kind: "local", label: "本地终端" },
        { id: "2", kind: "local", label: "本地终端" },
      ],
      t,
    );
    expect(out.get("1")).toBe("Local Terminal #1");
    expect(out.get("2")).toBe("Local Terminal #2");
  });

  it("空列表 → 空 Map", () => {
    expect(disambiguateLabels([], t).size).toBe(0);
  });
});
