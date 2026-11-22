// 答疑内容：从 Markdown 语料文件读取（Vite `?raw` 零依赖导入为字符串），不再硬编码在代码里。
//
// 维护方式：只改 `src/content/faq.<lang>.md`，随版本发布更新，无需动组件逻辑。
// 格式约定：每个二级标题「## [编号] 问题」起一条答疑，其下正文（到下一个 `## ` 或文件末尾）即答案；
//          单个 `# ` 主标题行、以及任何尚未进入某条答疑之前的说明文字都不计入内容。
// 编号规则：标题里的 `[N]` 是答疑的稳定编号（双语语料同编号 = 同一条），供外部直达（lib/faqJump）；
//          未编号的条目按出现顺序兜底。新增条目请取双语都未占用的新编号，不要在中间插号。
// 展示口径：答案按纯文本渲染（前端 pre-wrap 保留换行），暂不解析粗体/链接等 Markdown 语法。
import faqZh from "../content/faq.zh.md?raw";
import faqEn from "../content/faq.en.md?raw";

/** 一条答疑：问题与答案均为语料文件里的字面文案（非 i18n 键，语言由所选 md 决定）。 */
export interface Faq {
  id: string;
  /** 稳定编号（md 标题里的 [N]，双语一致）；UI 显示为 Q{no}，直达 API 按此定位。 */
  no: number;
  q: string;
  a: string;
}

const SOURCES: Record<string, string> = { zh: faqZh, en: faqEn };
/** 解析结果按语言缓存：md 是构建期常量，切语言/重渲染无需重复解析。 */
const cache = new Map<string, Faq[]>();

function parse(md: string): Faq[] {
  const out: Faq[] = [];
  let cur: { no: number; q: string; body: string[] } | null = null;
  const flush = () => {
    // 空标题（裸 `##` 退化行）只起收尾作用，自身不入列表（后端 knowledge.rs 同口径）。
    if (cur && cur.q) {
      // 显式 [N] 编号跨语言、跨语料增删都稳定；未编号按出现顺序兜底（从 1 起）
      const no = cur.no > 0 ? cur.no : out.length + 1;
      out.push({ no, id: `faq-${no}`, q: cur.q, a: cur.body.join("\n").trim() });
      cur = null;
    }
  };
  for (const raw of md.split(/\r?\n/)) {
    const line = raw.trimEnd();
    const m = line.match(/^##\s*(?:\[(\d+)\]\s*)?(.*)$/);
    if (m) {
      flush();
      cur = { no: m[1] ? Number(m[1]) : 0, q: m[2].trim(), body: [] };
    } else if (/^#\s+/.test(line)) {
      flush(); // 主标题：收尾当前条，本身不进入内容
    } else if (cur) {
      cur.body.push(line);
    }
  }
  flush();
  return out;
}

/** 取某语言的答疑列表；缺该语言语料时回落中文（与 i18n 兜底口径一致）。 */
export function faqsFor(lang: string): Faq[] {
  const code = SOURCES[lang] ? lang : "zh";
  let list = cache.get(code);
  if (!list) {
    list = parse(SOURCES[code] ?? "");
    cache.set(code, list);
  }
  return list;
}
