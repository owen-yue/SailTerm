// 极简 Markdown 解析（lib/markdown）单测：AI 回复进界面的可信度最低路径，
// 解析器安全承诺 + 流式宽容是硬要求。重点锁定：
// - XSS 面：非 http(s)/mailto 协议链接一律留作纯文本；
// - 词内 `*` 不起强调（`2*3*4`），但中文紧跟标记仍加粗；
// - 未闭合语法（半个 ``` / ** / 半张表）当普通文本，不吞后文。
import { describe, expect, it } from "vitest";
import { parseInline, parseMarkdown, type Block } from "../markdown";

const isBlock = (b: Block, k: Block["k"]) => b.k === k;

describe("parseInline 行内", () => {
  it("行内代码：内部不再解析强调", () => {
    expect(parseInline("`ls -al`")).toEqual([{ k: "code", v: "ls -al" }]);
    // 代码里的 ** 保持字面
    expect(parseInline("a `**b**` c")[1]).toEqual({ k: "code", v: "**b**" });
    // 首尾贴空白不算代码（` x ` 当文本）
    expect(parseInline("` x `").some((s) => s.k === "code")).toBe(false);
  });

  it("粗/斜/粗斜，词内星号不劫持", () => {
    expect(parseInline("**粗**")).toEqual([{ k: "bold", v: [{ k: "text", v: "粗" }] }]);
    expect(parseInline("*斜*")).toEqual([{ k: "em", v: [{ k: "text", v: "斜" }] }]);
    expect(parseInline("***粗斜***")).toEqual([
      { k: "bold", v: [{ k: "em", v: [{ k: "text", v: "粗斜" }] }] },
    ]);
    // 2*3*4 的星号是字面（前一个字符是词字符）
    expect(parseInline("2*3*4").every((s) => s.k === "text")).toBe(true);
    // 中文紧跟标记是常态：`**重要**看一下` 必须能加粗
    const segs = parseInline("**重要**看一下");
    expect(segs[0]).toEqual({ k: "bold", v: [{ k: "text", v: "重要" }] });
  });

  it("未闭合强调回落纯文本（流式半个 **）", () => {
    expect(parseInline("**没闭合").every((s) => s.k === "text")).toBe(true);
  });

  it("链接只放行 http(s)/mailto，其余协议留作文本（XSS 面为零）", () => {
    expect(parseInline("[官网](https://a.cn)")).toEqual([
      { k: "link", v: "官网", href: "https://a.cn" },
    ]);
    expect(parseInline("[x](mailto:a@b.c)")[0]).toEqual({ k: "link", href: "mailto:a@b.c", v: "x" });
    // javascript: 绝不升级成 link
    const evil = parseInline("[点我](javascript:alert(1))");
    expect(evil.some((s) => s.k === "link")).toBe(false);
    expect(evil.every((s) => s.k === "text")).toBe(true);
  });
});

describe("parseMarkdown 块级", () => {
  it("围栏代码块：闭合并标注语言；未闭合按到文末收", () => {
    const blocks = parseMarkdown("```bash\nls\necho\n```");
    expect(blocks[0]).toEqual({ k: "code", lang: "bash", body: "ls\necho" });
    // 未闭合：不吞后文，仍出一个 code 块（流式期间先到内容）
    const open = parseMarkdown("```\nline1\nline2");
    expect(open[0]).toEqual({ k: "code", lang: "", body: "line1\nline2" });
  });

  it("标题 / 分隔线 / 列表 / 引用", () => {
    expect(parseMarkdown("## 标题")[0]).toMatchObject({ k: "head", level: 2 });
    expect(parseMarkdown("---")[0]).toEqual({ k: "hr" });
    // `- - -` 也当水平线而非列表（HR 先于列表判定）
    expect(parseMarkdown("- - -")[0].k).toBe("hr");

    const ul = parseMarkdown("- a\n- b")[0];
    expect(ul).toMatchObject({ k: "list", ordered: false });
    const ol = parseMarkdown("1. a\n2. b")[0];
    expect(ol).toMatchObject({ k: "list", ordered: true });
    // 换记号即断块
    expect(parseMarkdown("- a\n1. b").filter((b) => isBlock(b, "list")).length).toBe(2);

    expect(parseMarkdown("> quote")[0]).toMatchObject({ k: "quote" });
  });

  it("GFM 表格：仅表头+分隔行同时成立才升级；解析对齐与转义竖线", () => {
    // 流式中途：只有表头行、分隔行未到 → 当普通文本，不能顶出半个表
    const headOnly = parseMarkdown("| a | b |");
    expect(headOnly.some((b) => b.k === "table")).toBe(false);

    const md = "| 名称 | 数量: |\n| :--- | ---: |\n| 苹果 | 3 |\n| 梨 \\| 混 | 2 |";
    const tbl = parseMarkdown(md).find((b) => b.k === "table");
    expect(tbl).toBeTruthy();
    if (tbl && tbl.k === "table") {
      expect(tbl.align[0]).toBe("left");
      expect(tbl.align[1]).toBe("right");
      // 转义 `\|` 留在正文，不当列分隔
      const cellText = tbl.rows[1][0].map((s) => (s.k === "text" ? s.v : "")).join("");
      expect(cellText).toContain("梨 | 混");
    }
  });

  it("CRLF 归一 + 普通段落", () => {
    const blocks = parseMarkdown("第一行\r\n第二行");
    expect(blocks[0].k).toBe("p");
  });
});
