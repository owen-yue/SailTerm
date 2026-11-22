// 会话能力表（lib/capabilities）+ 连接类型元数据（lib/kindMeta）单测。
// 两者是「会话能做什么 / 表单怎么画」的唯一事实源，前端放行而后端无通道的漂移
// 会造成 AI 工具下发落空——这里锁死档位定义与派生谓词的一致性。
import { describe, expect, it } from "vitest";
import {
  canAgentRail,
  capsOf,
  hasAnyCap,
  hasCap,
  hasQuickBar,
  kindHasFiles,
  kindHasTerminal,
  railTabsFor,
} from "../capabilities";
import { kindMetaOf } from "../kindMeta";

describe("能力档位表", () => {
  it("SSH 全能力；FTP 只有 fs+negotiation（无 exec 是硬约束）", () => {
    expect(capsOf("ssh").size).toBe(7);
    expect(hasCap("ftp", "exec")).toBe(false);
    expect(hasCap("ftp", "fs")).toBe(true);
    expect(hasCap("ftps", "negotiation")).toBe(true);
  });

  it("Telnet/串口/mosh 仅 pty；本地无 fs（本机走 local_* 命令）", () => {
    for (const k of ["telnet", "serial", "mosh"] as const) {
      expect([...capsOf(k)]).toEqual(["pty"]);
    }
    expect(hasCap("local", "fs")).toBe(false);
    expect(hasCap("local", "exec")).toBe(true);
  });

  it("hasAnyCap 是 OR 语义（面板门槛常见）", () => {
    expect(hasAnyCap("ftp", ["exec", "fs"])).toBe(true);
    expect(hasAnyCap("telnet", ["fs", "monitor"])).toBe(false);
  });

  it("派生谓词与档位表自洽", () => {
    expect(kindHasTerminal("ssh")).toBe(true);
    expect(kindHasTerminal("ftp")).toBe(false);
    expect(kindHasFiles("ftps")).toBe(true);
    expect(kindHasFiles("local")).toBe(false);
  });
});

describe("QuickBar / Agent 页签注册规则", () => {
  it("QuickBar：SSH/本地/Telnet/mosh 可用，串口/FTP 系不可", () => {
    expect(hasQuickBar("ssh")).toBe(true);
    expect(hasQuickBar("mosh")).toBe(true);
    expect(hasQuickBar("serial")).toBe(false); // 一期不纳入
    expect(hasQuickBar("ftp")).toBe(false);
    expect(hasQuickBar(null)).toBe(false);
  });

  it("Agent 页签：有终端且非聊天唯一形态才注册", () => {
    // Linux/mac 本地终端非 Windows，canAgentRail 依据 isWindowsPlatform（jsdom 默认非 Win）
    expect(canAgentRail("ssh")).toBe(true);
    expect(canAgentRail("telnet")).toBe(false); // 聊天唯一形态
    expect(canAgentRail("ftp")).toBe(false); // 无终端
    expect(railTabsFor("ssh")).toEqual(["agent", "chat"]);
    expect(railTabsFor("telnet")).toEqual(["chat"]);
    expect(railTabsFor(null)).toEqual(["chat"]);
  });
});

describe("kindMetaOf 表单元数据", () => {
  it("SSH：用户名必填 + 私钥/协商 + sshExclusive + keepalive", () => {
    const m = kindMetaOf("ssh");
    expect(m.usernameRequired).toBe(true);
    expect(m.authModes).toEqual(["password", "privateKey", "auto"]);
    expect(m.sshExclusive).toBe(true);
    expect(m.ftpExclusive).toBe(false);
  });

  it("FTP 家族：仅密码、可空（匿名）、ftpExclusive、有保活", () => {
    const m = kindMetaOf("ftp");
    expect(m.authModes).toEqual(["password"]);
    expect(m.passwordOptional).toBe(true);
    expect(m.ftpExclusive).toBe(true);
    expect(m.keepalive).toBe(true);
  });

  it("无认证协议（Telnet/mosh/local）不收预认证材料；串口按设备画", () => {
    expect(kindMetaOf("telnet").hasAuth).toBe(false);
    expect(kindMetaOf("mosh").hasAuth).toBe(false);
    const serial = kindMetaOf("serial");
    expect(serial.hostLabelIsDevice).toBe(true);
    expect(serial.serialParams).toBe(true);
    expect(serial.hasAuth).toBe(false);
  });
});
