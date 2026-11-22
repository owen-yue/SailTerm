// 会话显示名的编号口径（lib/sessionName::sessionNameOf）。
// 锁的是「同一条会话在任何地方都是同一个号」：基数必须是全量真实会话，
// 先 filter 再编号（只数可见的那些）正是页签与发送列表对不上号的成因。
import { describe, expect, it } from "vitest";
import { sessionNameOf } from "../sessionName";
import type { SessionKind } from "../../types";

const t = (k: string) => (k === "topbar.local.title" ? "Local Terminal" : k);

/** 造一条会话：只给编号口径用得上的三个字段。 */
const tab = (id: string, label: string, kind: SessionKind = "ssh", phase?: string) =>
  ({ id, kind, label, ...(phase ? { phase } : {}) });

describe("sessionNameOf 的基数", () => {
  it("重名按数组次序排 #1..#n，不重名的不加下标", () => {
    const tabs = [tab("1", "root@a"), tab("2", "root@b"), tab("3", "root@a")];
    expect(sessionNameOf(tabs, t, tabs[0])).toBe("root@a #1");
    expect(sessionNameOf(tabs, t, tabs[1])).toBe("root@b");
    expect(sessionNameOf(tabs, t, tabs[2])).toBe("root@a #2");
  });

  it("不可发送的会话（FTP / 断线）照样占号——列表把它藏了也不能改号", () => {
    // 中间那条是 FTP（发送工具按 canSend 过滤掉它），前后两条同名。
    const tabs = [tab("1", "root@a"), tab("2", "root@a", "ftp"), tab("3", "root@a")];
    expect(sessionNameOf(tabs, t, tabs[2])).toBe("root@a #3");
    // 若按「可见的那些」编号，第三条会拿到 #2，与页签上的 #3 对不上。
  });

  it("占位标签（预加载中 / 失败卡）不入基数，自己显裸名", () => {
    const tabs = [tab("1", "root@a", "ssh", "pending"), tab("2", "root@a")];
    // 占位标签只在主窗口存在、后端 session_list 不返回它：算进去两个窗口就会给出不同号。
    expect(sessionNameOf(tabs, t, tabs[1])).toBe("root@a");
    expect(sessionNameOf(tabs, t, tabs[0])).toBe("root@a");
  });

  it("本地终端按当前语言解析后再比名（两个本地终端同样要消歧）", () => {
    const tabs = [tab("1", "本地终端", "local"), tab("2", "本地终端", "local")];
    expect(sessionNameOf(tabs, t, tabs[0])).toBe("Local Terminal #1");
    expect(sessionNameOf(tabs, t, tabs[1])).toBe("Local Terminal #2");
  });
});
