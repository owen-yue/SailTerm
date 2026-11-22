// 发送条目解析（lib/sendScript）单测。
// 硬约束：发出去的字节必须和编辑区里写的一致——「分隔档怎么切」「要不要补回车」「hex 清洗口径」
// 出错的表现是终端上跑了一条肉眼看不出区别的东西，真机上最难发现，只能靠用例钉死。
import { describe, expect, it } from "vitest";
import {
  MAX_COUNT,
  MIN_INTERVAL,
  clampNum,
  hexToBytes,
  parseEntries,
  splitSegments,
} from "../sendScript";

/** 条目 → 可读串（`[...bytes]` 成十进制数组，断言失败时能直接看出差在哪一字节）。 */
const bs = (u: Uint8Array): number[] => Array.from(u);
const all = (entries: Uint8Array[]): number[][] => entries.map(bs);

const CR = 0x0d;
const enc = new TextEncoder();

describe("splitSegments", () => {
  it("按行档：换行是分隔符，`|` 属于命令本身（shell 管道不能被切开）", () => {
    expect(splitSegments("ls | grep foo\npwd", "line")).toEqual(["ls | grep foo", "pwd"]);
  });

  it("按竖线档：`|` 与换行都是分隔符（一行排多条）", () => {
    expect(splitSegments("a|b\nc|d", "pipe")).toEqual(["a", "b", "c", "d"]);
  });

  it("按字符档：逐字符一条，换行只作分隔、本身不发出去", () => {
    expect(splitSegments("ab\nc", "char")).toEqual(["a", "b", "c"]);
  });

  it("按字符档按码点切：代理对不拆散（emoji 是一个字符，不是两个乱码）", () => {
    expect(splitSegments("😀a", "char")).toEqual(["😀", "a"]);
  });

  it("三档都丢弃空白条目：编辑器尾部的空行不该变成一条「只按回车」的误发", () => {
    expect(splitSegments("a\n\n   \n", "line")).toEqual(["a"]);
    expect(splitSegments("a|| \n", "pipe")).toEqual(["a"]);
    expect(splitSegments("a \n \r", "char")).toEqual(["a"]);
  });

  it("CRLF 与 LF 混排（从 Windows 记事本粘进来的）不留下孤字符", () => {
    expect(splitSegments("a\r\nb", "line")).toEqual(["a", "b"]);
    expect(splitSegments("a\r\nb", "pipe")).toEqual(["a", "b"]);
  });
});

describe("hexToBytes", () => {
  it("容忍空格 / 冒号 / 连字符分隔与 0x 前缀，同一串字节各种写法都收", () => {
    expect(bs(hexToBytes("48 65")!)).toEqual([0x48, 0x65]);
    expect(bs(hexToBytes("48:65")!)).toEqual([0x48, 0x65]);
    expect(bs(hexToBytes("48-65")!)).toEqual([0x48, 0x65]);
    expect(bs(hexToBytes("0x48 0x65")!)).toEqual([0x48, 0x65]);
    expect(bs(hexToBytes("4865")!)).toEqual([0x48, 0x65]);
    expect(bs(hexToBytes("4A6f")!)).toEqual([0x4a, 0x6f]); // 大小写混写
  });

  it("非法字符或奇数长度回 null（由调用方归错，绝不静默补零）", () => {
    expect(hexToBytes("4g")).toBeNull();
    expect(hexToBytes("abc")).toBeNull();
    expect(hexToBytes("|")).toBeNull();
  });

  it("清洗后为空回空字节串——是「没内容」，不是「格式错」", () => {
    expect(hexToBytes("   ")).toEqual(new Uint8Array());
  });
});

describe("parseEntries：文本档", () => {
  it("追加回车补的是 `\\r` 不是 `\\n`（裸 \\n 在 PowerShell 下会成未闭合续行）", () => {
    const p = parseEntries("ls", "text", "line", true);
    expect(p.error).toBeNull();
    expect(bs(p.entries[0])).toEqual([...enc.encode("ls"), CR]);
  });

  it("关掉追加回车就一个字节都不多发", () => {
    expect(all(parseEntries("ls", "text", "line", false).entries)).toEqual([bs(enc.encode("ls"))]);
  });

  it("按字符档恒不补回车：逐键模拟里补回车等于每敲一个字符执行一次", () => {
    const p = parseEntries("ab", "text", "char", true);
    expect(all(p.entries)).toEqual([[0x61], [0x62]]);
  });

  it("UTF-8 中文按字节进终端（与后端 into_bytes 同编码）", () => {
    expect(bs(parseEntries("中文", "text", "line", false).entries[0])).toEqual(
      bs(enc.encode("中文")),
    );
  });
});

describe("parseEntries：Hex 档", () => {
  it("按行档：一行一组字节，追加回车时结尾补 0x0d", () => {
    const p = parseEntries("48 65\n6c", "hex", "line", true);
    expect(all(p.entries)).toEqual([[0x48, 0x65, CR], [0x6c, CR]]);
  });

  it("按字符档 = 逐字节：整段先合成字节流再拆，而不是把可见字符逐个当 hex", () => {
    const p = parseEntries("48 65", "hex", "char", true);
    expect(p.error).toBeNull();
    expect(all(p.entries)).toEqual([[0x48], [0x65]]);
  });

  it("出错即整段不可发送，并指出第几个条目非法（UI 据此报行号）", () => {
    const p = parseEntries("48\nzz\n65", "hex", "line", false);
    expect(p.entries).toEqual([]);
    expect(p.error).toEqual({ key: "sd.errHex", at: 1 });
  });

  it("按字符档下整段非法才报错（不存在「第几条」的定位）", () => {
    expect(parseEntries("48 zz", "hex", "char", false).error).toEqual({ key: "sd.errHex" });
  });
});

describe("parseEntries：空输入", () => {
  it("空编辑区不是错误：零条目 + error 为空（「没内容就不发」由按钮置灰表达）", () => {
    for (const p of [
      parseEntries("", "text", "line", true),
      parseEntries("\n \n", "text", "pipe", true),
      parseEntries("  ", "hex", "char", false),
    ])
      expect(p).toEqual({ entries: [], error: null });
  });

  it("hex 纯空白也不报「非法字符」：清洗后为空串属于没得发", () => {
    expect(parseEntries("   ", "hex", "line", false).error).toBeNull();
  });
});

describe("clampNum / 常量", () => {
  it("非有限数（清空输入框、粘进脏值）回落到默认值", () => {
    expect(clampNum(NaN, 1, MAX_COUNT, 1)).toBe(1);
    expect(clampNum(Infinity, 1, MAX_COUNT, 1)).toBe(1);
  });

  it("越界夹到端点，界内原样通过", () => {
    expect(clampNum(0, 1, MAX_COUNT, 1)).toBe(1);
    expect(clampNum(99999, 1, MAX_COUNT, 1)).toBe(MAX_COUNT);
    expect(clampNum(3, 1, MAX_COUNT, 1)).toBe(3);
    expect(clampNum(0.001, MIN_INTERVAL, 3600, 1)).toBe(MIN_INTERVAL);
  });

  it("间隔下限不为零：0 间隔逐条 IPC 会把 PTY 灌成一坨", () => {
    expect(MIN_INTERVAL).toBeGreaterThan(0);
  });
});
