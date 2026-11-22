// 极简 Markdown 子集解析：纯函数、零依赖、不碰 DOM。
//
// 为什么不引 marked / markdown-it：AI 回复进界面是本项目可信度最低的一条数据路径，
// 自己解析就天然不需要 innerHTML + 净化层（XSS 面为零）；而运维问答实际会用到的语法
// 只有那么几种，全量 CommonMark 于我们是负担而非能力。
//
// 代价必须写清楚：**只支持下面这些**。嵌套列表、HTML 标签、`_斜体_` 一律按普通
// 文本露出。表格支持 GFM 那一种（`|` 列 + 分隔行），因为附件里的 CSV/Excel 抽出来就
// 是 Markdown 表。所以系统提示词里同步说明可用排版（见 src-tauri/src/ai/context.rs
// 的 STYLE_BASE），两边必须一起改，否则模型会吐出这里渲染不了的语法。
//
// 流式宽容是硬要求：回复是一个字一个字到的，任何未闭合语法（半个 ``` 块、半个 `**`）
// 都要渲染成「目前这些字符该有的样子」，不能闪断、不能吞掉后文——所以每种语法都只在
// **找到闭合标记时**才升级成节点，否则原样当文本走。

/** 行内片段。`text` 里可含 `\n`，由渲染侧的 pre-wrap 保留。 */
export type Inline =
  | { k: "text"; v: string }
  | { k: "code"; v: string }
  | { k: "bold"; v: Inline[] }
  | { k: "em"; v: Inline[] }
  | { k: "link"; v: string; href: string };

/** 块级节点。 */
export type Block =
  | { k: "code"; lang: string; body: string }
  | { k: "head"; level: number; v: Inline[] }
  | { k: "list"; ordered: boolean; items: Inline[][] }
  | { k: "quote"; v: Inline[] }
  | { k: "hr" }
  | { k: "table"; align: Align[]; head: Inline[][]; rows: Inline[][][] }
  | { k: "p"; v: Inline[] };

/** 列对齐：`:-` 左、`-:` 右、`:-:` 中；不带冒号时 `null`（交给 CSS 默认）。 */
export type Align = "left" | "right" | "center" | null;

/**
 * 紧邻标记符算「词内字符」的判定：`2*3*4` 里的星号不该起强调。
 * 刻意**不含中文**：`**重要**看一下` 这种中文紧跟标记是常态，把汉字算进去会把所有
 * 中文粗体都判死（踩过的坑：初版带 `\u4e00-\u9fff`，中文回复一条都没加粗）。
 * 标识符里的下划线不在此列——`_` 家族本就不支持强调（见 parseInline）。
 */
const WORD_CHAR = /[A-Za-z0-9_]/;

/** 链接只放行这几个协议：其余（`javascript:`、`data:`、`file:`…）原样留作文本。 */
function safeHref(h: string): boolean {
  return /^https?:\/\/\S+$/i.test(h) || /^mailto:[^\s<>]+$/i.test(h);
}

/** 行内解析。注意 `*` 家族支持粗/斜，`_` 家族**刻意不支持**：运维文本里标识符下划线太密。 */
export function parseInline(src: string): Inline[] {
  const out: Inline[] = [];
  let text = "";
  let i = 0;
  const flush = () => {
    if (text) {
      out.push({ k: "text", v: text });
      text = "";
    }
  };

  while (i < src.length) {
    const c = src[i];

    // 行内代码：同一段内、首尾不贴空白，内部不再解析强调（`**` 出现在代码里就该是星号）
    if (c === "`") {
      const close = src.indexOf("`", i + 1);
      const body = close > i + 1 ? src.slice(i + 1, close) : "";
      if (close > i + 1 && body && !body.includes("\n") && !/^\s|\s$/.test(body)) {
        flush();
        out.push({ k: "code", v: body });
        i = close + 1;
        continue;
      }
      text += c;
      i++;
      continue;
    }

    // 强调：`*斜*` / `**粗**` / `***粗斜***`（三连必须整对匹配，否则内部星号会漏成文本）
    if (c === "*") {
      const prev = i > 0 ? src[i - 1] : "";
      const run = src[i + 1] === "*" ? (src[i + 2] === "*" ? 3 : 2) : 1;
      const marker = "*".repeat(run);
      const start = i + run;
      const close =
        start < src.length && !WORD_CHAR.test(prev) && !/\s/.test(src[start])
          ? src.indexOf(marker, start)
          : -1;
      const inner = close > 0 ? src.slice(start, close) : "";
      const after = close > 0 ? src[close + run] ?? "" : "";
      if (inner && !inner.includes("\n") && !/\s$/.test(inner) && !WORD_CHAR.test(after)) {
        flush();
        const body = parseInline(inner);
        out.push(
          run === 3
            ? { k: "bold", v: [{ k: "em", v: body }] }
            : run === 2
              ? { k: "bold", v: body }
              : { k: "em", v: body }
        );
        i = close + run;
        continue;
      }
      text += c;
      i++;
      continue;
    }

    // 链接：`[说明](url)`，标签取纯文本（不再嵌强调）
    if (c === "[") {
      const rb = src.indexOf("]", i + 1);
      const rp = rb > i + 1 && src[rb + 1] === "(" ? src.indexOf(")", rb + 2) : -1;
      const label = rb > i + 1 && rp > 0 ? src.slice(i + 1, rb) : "";
      const href = rp > 0 ? src.slice(rb + 2, rp).trim() : "";
      if (label && rp > 0 && !label.includes("\n") && safeHref(href)) {
        flush();
        out.push({ k: "link", v: label, href });
        i = rp + 1;
        continue;
      }
      text += c;
      i++;
      continue;
    }

    text += c;
    i++;
  }
  flush();
  return out;
}

const RE_FENCE = /^\s*(`{3,}|~{3,})\s*([^\s`]*)\s*$/;
const RE_HEAD = /^(#{1,6})\s+(.*)$/;
const RE_HR = /^\s*(?:([-*_])\s*){3,}$/;
const RE_UL = /^\s*[-*+]\s+(.*)$/;
const RE_OL = /^\s*\d+[.)]\s+(.*)$/;
const RE_QUOTE = /^\s*>\s?(.*)$/;

/** 表格分隔行：`| --- | :-: | ---: |`（每列至少一个 `-`，可选左/右冒号）。 */
const RE_TABLE_DELIM =
  /^\s*\|?\s*:?-+:?\s*(?:\|\s*:?-+:?\s*)*\|?\s*$/;

/**
 * 拆一行表格单元格：去头尾 `|`，按未转义的 `|` 切开（`\|` 留在正文里当字）。
 * 不先 `trim()` 整行：首列的空与否正是 GFM 用来判「这行算不算表格」的信息。
 */
function splitRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);
  const cells: string[] = [];
  let cur = "";
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\" && s[i + 1] === "|") {
      cur += "|";
      i++;
      continue;
    }
    if (s[i] === "|") {
      cells.push(cur.trim());
      cur = "";
      continue;
    }
    cur += s[i];
  }
  cells.push(cur.trim());
  return cells;
}

/** 分隔行 → 每列对齐。`:-:` 中、`:-` 左、`-:` 右、`-` 为 `null`。 */
function alignsOf(cells: string[]): Align[] {
  return cells.map((c) => {
    const l = c.startsWith(":");
    const r = c.endsWith(":");
    return l && r ? "center" : l ? "left" : r ? "right" : null;
  });
}

/**
 * 尝试从 `lines[i]` 起读一张表（GFM：表头行 + 分隔行 + 若干部数据行）。
 *
 * 不够一个「表头 + 分隔行」的结构就返回 `null` 并交回原行继续当普通文本：
 * 流式期间表头先到、分隔行后到，中途不能把它升级成表（否则半个表会顶掉整段排版）。
 */
function readTable(
  lines: string[],
  i: number
): { table: Extract<Block, { k: "table" }>; next: number } | null {
  const line = lines[i];
  const delim = lines[i + 1];
  // 表头行含 `|` 且下一行是分隔行，才是表：这两条同时成立才升级（与围栏「找到闭合才升级」同口径）
  if (!line.includes("|") || delim === undefined || !RE_TABLE_DELIM.test(delim)) return null;
  const head = splitRow(line);
  const align = alignsOf(splitRow(delim));
  // 列数取两者较大：分隔行比表头短（模型少写一列）也不能把表头多出的列弄丢
  const cols = Math.max(head.length, align.length);
  while (align.length < cols) align.push(null);
  const toRow = (cells: string[]): Inline[][] => {
    const row = cells.slice(0, cols).map((c) => parseInline(c));
    while (row.length < cols) row.push([]);
    return row;
  };
  const rows: Inline[][][] = [];
  let j = i + 2;
  for (; j < lines.length; j++) {
    const r = lines[j];
    if (!r.trim() || !r.includes("|")) break;
    rows.push(toRow(splitRow(r)));
  }
  return {
    // head/rows 的统一形状：行 = `Inline[][]`（一行里每格一串行内片段）
    table: { k: "table", align, head: toRow(head), rows },
    next: j - 1,
  };
}

/** 整段文本 → 块序列。流式期间对未闭合围栏按「到文末为止」处理。 */
export function parseMarkdown(src: string): Block[] {
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  const out: Block[] = [];
  let para: string[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  let quote: string[] | null = null;

  const flushPara = () => {
    if (para.length) out.push({ k: "p", v: parseInline(para.join("\n")) });
    para = [];
  };
  const flushList = () => {
    if (list) {
      out.push({ k: "list", ordered: list.ordered, items: list.items.map(parseInline) });
      list = null;
    }
  };
  const flushQuote = () => {
    if (quote) {
      out.push({ k: "quote", v: parseInline(quote.join("\n")) });
      quote = null;
    }
  };
  const flushAll = () => {
    flushPara();
    flushList();
    flushQuote();
  };

  for (let i = 0; i < lines.length; i++) {
    const ln = lines[i];

    const fence = ln.match(RE_FENCE);
    if (fence) {
      flushAll();
      const mark = fence[1][0];
      const len = fence[1].length;
      const closeRe = new RegExp(`^\\s*${mark === "`" ? "`" : "~"}{${len},}\\s*$`);
      const body: string[] = [];
      let j = i + 1;
      for (; j < lines.length; j++) {
        if (closeRe.test(lines[j])) break;
        body.push(lines[j]);
      }
      out.push({ k: "code", lang: fence[2].toLowerCase(), body: body.join("\n") });
      i = j; // 未闭合时 j 已到末尾，循环自然结束
      continue;
    }

    if (!ln.trim()) {
      flushAll();
      continue;
    }

    // 分隔线要在列表之前判：`- - -` 也长得像项目符号
    if (RE_HR.test(ln)) {
      flushAll();
      out.push({ k: "hr" });
      continue;
    }

    const head = ln.match(RE_HEAD);
    if (head) {
      flushAll();
      out.push({ k: "head", level: head[1].length, v: parseInline(head[2].trim()) });
      continue;
    }

    const q = ln.match(RE_QUOTE);
    if (q) {
      flushPara();
      flushList();
      quote = quote ?? [];
      quote.push(q[1]);
      continue;
    }

    // 表格放在列表之前、分隔线之后：`| a | b |` 不与其他语法同形（含 `|` 的行也不会被 RE_HR 当成水平线）
    const tbl = readTable(lines, i);
    if (tbl) {
      flushAll();
      out.push(tbl.table);
      i = tbl.next;
      continue;
    }

    const ul = ln.match(RE_UL);
    const ol = ul ? null : ln.match(RE_OL);
    const bullet = ul ?? ol;
    if (bullet) {
      flushPara();
      flushQuote();
      const ordered = !!ol;
      // 有序/无序各自成块：换了记号就断块（嵌套不支持，缩进当普通内容）
      if (!list || list.ordered !== ordered) {
        flushList();
        list = { ordered, items: [] };
      }
      list.items.push(bullet[1]);
      continue;
    }

    flushList();
    flushQuote();
    para.push(ln.trim());
  }
  flushAll();
  return out;
}
