// Markdown → ANSI：终端（xterm）这条通路的轻量渲染。
//
// 侧栏聊天把回复渲染成 HTML（MarkdownChat），终端只能打纯文本，此前 ``` 围栏、
// ** 星号原样落屏（代码块看着就是一堆符号）。这里做「够读」级别的转换：
// - 围栏代码块：去掉 ``` 行，整块加 `│` 左边条，首行标注语言；
// - 行内语法：**粗体**、`代码`、[链接](url) 复用 lib/markdown 的 parseInline，转成 ANSI；
// - 其余（列表、标题 #、表格）保持原样——终端里符号本身就是可读排版。
//
// 硬约束：**逐行等长映射**（输入 n 行 → 输出 n 行，围栏块除外）。writeReply 的
// clipForTerm 按行数收口、且靠「行内容」找段落边界，渲染若改变行数/换行会把截断逻辑搞乱。

import type { Inline } from "./markdown";
import { parseInline } from "./markdown";

/** 代码块左边条：青色细竖线，与 writeReply 的 `●` 同色系但不喧宾夺主。 */
const BAR = "\x1b[36m│\x1b[0m ";

/** 一行文本 → ANSI：先走侧栏同款行内解析，再把片段序列扇平为带色文本。 */
function inlineToAnsi(src: string): string {
  return segsToAnsi(parseInline(src));
}

/** 行内片段序列 → ANSI 文本。基于片段递归，能正确保留 `**粗体里的 `码`**` 嵌套样式。 */
function segsToAnsi(segs: Inline[]): string {
  let out = "";
  for (const seg of segs) {
    switch (seg.k) {
      case "code":
        out += `\x1b[36m${seg.v}\x1b[0m`;
        break;
      case "bold":
        out += `\x1b[1m${segsToAnsi(seg.v)}\x1b[0m`;
        break;
      case "em":
        // 斜体终端里辨识度低，与侧栏的弱化样式同效——按普通文本打
        out += segsToAnsi(seg.v);
        break;
      case "link":
        // 标签加下划线，URL 收进括号（终端里没法点，留全信息比留个空壳强）
        out += `\x1b[4m${seg.v}\x1b[0m(${seg.href})`;
        break;
      default:
        out += seg.v;
    }
  }
  return out;
}

/**
 * 整段回复 → ANSI 文本。逐行状态机：进围栏停渲染行内语法（代码里的 `**` 不该变粗），
 * 未闭合围栏按「到文末为止」收（与 markdown.ts 的流式口径一致，回复是完整文本但防御性处理）。
 */
export function mdToAnsi(src: string): string {
  const lines = src.split("\n");
  const out: string[] = [];
  let fence: string | null = null; // 当前围栏的起始标记（``` / ~~~ 及长度），用于配对闭合
  for (const ln of lines) {
    const m = ln.match(/^\s*(`{3,}|~{3,})\s*([^\s`]*)\s*$/);
    if (!fence && m) {
      fence = m[1];
      // 首行位置放语言标注；没有语言就是一条裸的起始线，直接留给上条的 `│` 序列开个头
      const lang = m[2] ? `\x1b[2m${m[2]}\x1b[0m` : "";
      out.push(BAR + lang);
      continue;
    }
    if (fence && m && m[1][0] === fence[0] && m[1].length >= fence.length) {
      fence = null;
      continue;
    }
    if (fence) {
      out.push(BAR + ln);
      continue;
    }
    out.push(inlineToAnsi(ln));
  }
  return out.join("\n");
}
