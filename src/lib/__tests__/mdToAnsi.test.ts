// Markdown → ANSI（lib/mdToAnsi）单测：终端通路渲染。
// 硬约束：**逐行等长映射**（围栏块除外）——writeReply 的 clipForTerm 靠行数收口，
// 渲染改行数会把截断逻辑搞乱。锁定围栏加边条、行内样式转 ANSI、代码块内不解析强调。
import { describe, expect, it } from "vitest";
import { mdToAnsi } from "../mdToAnsi";

describe("mdToAnsi", () => {
  it("纯文本行逐行等长（不吞换行）", () => {
    const out = mdToAnsi("a\nb\nc");
    expect(out.split("\n")).toEqual(["a", "b", "c"]);
  });

  it("行内粗体转 ANSI 加粗码，斜体按普通文本（终端辨识度低）", () => {
    expect(mdToAnsi("**粗**")).toBe("\x1b[1m粗\x1b[0m");
    expect(mdToAnsi("*斜*")).toBe("斜"); // em 扇平为纯文本
    expect(mdToAnsi("`码`")).toBe("\x1b[36m码\x1b[0m");
  });

  it("链接：标签加下划线 + URL 收进括号", () => {
    expect(mdToAnsi("[官网](https://a.cn)")).toBe("\x1b[4m官网\x1b[0m(https://a.cn)");
  });

  it("围栏块：去 ``` 行、加左边条、首行标语言；块内 ** 不变粗", () => {
    const out = mdToAnsi("```bash\nls\n**not bold**\n```");
    const lines = out.split("\n");
    // 起始 ``` 与结束 ``` 被吃掉，只剩：语言标注行 + 两条内容行
    expect(lines.length).toBe(3);
    expect(lines[0]).toContain("bash");
    expect(lines[0]).toContain("│");
    expect(lines[1]).toContain("ls");
    // 代码块内的 ** 保持字面（不进围栏行内解析）
    expect(lines[2]).toContain("**not bold**");
    expect(lines[2]).not.toContain("\x1b[1m");
  });

  it("未闭合围栏：防御性收到文末，不报错", () => {
    const out = mdToAnsi("```\nx\ny");
    expect(out).toContain("x");
    expect(out).toContain("y");
  });
});
