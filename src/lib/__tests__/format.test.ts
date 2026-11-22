// 展示层格式化（lib/format）单测：文件列表/传输队列共用，纯函数。
// 锁定历史约定：非法字节给占位符「—」、单位换算 1024 进制、非整数保留一位小数、
// total 未知时「有进度即 100%」、ETA 边界返回空串。
import { describe, expect, it } from "vitest";
import {
  baseName,
  formatBytes,
  formatEta,
  formatSpeed,
  formatStamp,
  formatTime,
  percent,
} from "../format";
import { t } from "../i18n";

describe("formatBytes / formatSpeed", () => {
  it("非法与边界值", () => {
    expect(formatBytes(-1)).toBe("—");
    expect(formatBytes(Number.NaN)).toBe("—");
    expect(formatBytes(0)).toBe("0 B");
  });

  it("1024 进制，字节取整、更大单位一位小数", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1.0 KB");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(1024 ** 3)).toBe("1.0 GB");
    // 封顶 TB：再大不再进位（1024^4 B = 1.0 TB）
    expect(formatBytes(1024 ** 4)).toBe("1.0 TB");
  });

  it("速度 = 字节/秒；0 给占位", () => {
    expect(formatSpeed(0)).toBe("—");
    expect(formatSpeed(1024)).toBe("1.0 KB/s");
  });
});

describe("formatTime / formatStamp（本地时区）", () => {
  const at = (y: number, mo: number, d: number, h: number, mi: number, s = 0) =>
    new Date(y, mo - 1, d, h, mi, s).getTime();

  it("非法/零值给占位", () => {
    expect(formatTime(null)).toBe("—");
    expect(formatTime(0)).toBe("—");
    expect(formatStamp(-5)).toBe("—");
  });

  it("完整日期与紧凑秒级两种格式", () => {
    const ms = at(2026, 11, 3, 13, 5, 9);
    expect(formatTime(ms)).toBe("2026-11-03 13:05");
    expect(formatStamp(ms)).toBe("11-03 13:05:09");
  });
});

describe("percent", () => {
  it("total 未知时按「有进度即 100%」", () => {
    expect(percent(1, 0)).toBe(100);
    expect(percent(0, 0)).toBe(0);
  });

  it("常规比例取整并夹在 0~100", () => {
    expect(percent(50, 100)).toBe(50);
    expect(percent(1, 3)).toBe(33);
    expect(percent(200, 100)).toBe(100);
    expect(percent(-5, 100)).toBe(0);
  });
});

describe("formatEta", () => {
  it("无从估算（total 未知/速度 0/已完成）返回空串", () => {
    expect(formatEta(0, 0, 10)).toBe("");
    expect(formatEta(0, 100, 0)).toBe("");
    expect(formatEta(100, 100, 10)).toBe("");
  });

  it("秒 / 分秒 / 时分三档走 i18n 模板", () => {
    expect(formatEta(0, 100, 10)).toBe(t("fm.sec").replace("{n}", "10"));
    expect(formatEta(0, 600, 10)).toBe(
      t("fm.minSec").replace("{m}", "1").replace("{s}", "0"),
    );
    expect(formatEta(0, 36000, 10)).toBe(
      t("fm.hourMin").replace("{h}", "1").replace("{m}", "0"),
    );
  });
});

describe("baseName", () => {
  it("本地 `\` 与远端 `/` 两种分隔符", () => {
    expect(baseName("C:\\Users\\a\\readme.txt")).toBe("readme.txt");
    expect(baseName("/var/log/nginx/access.log")).toBe("access.log");
    expect(baseName("plain")).toBe("plain");
    // 尾随分隔符取空段时回落原串（不给空文件名）
    expect(baseName("a/b/")).toBe("a/b/");
  });
});
