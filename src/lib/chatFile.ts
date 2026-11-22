// 聊天输入区的本地文档：待发区 chip 的「选完就知道」预探与退化说明的界面文案。
//
// 为什么不在这儿自己判类型/解码：判型（magic bytes → 扩展名 → 有无 NUL）、编码兜底、抽取
// 都在后端 `ai::attach` 做（那里有 infer / mime_guess / encoding_rs / csv 这些现成 crate），
// 前端只做「把后端给出的事实显示出来」。在 JS 里再实现一遍抽取器，就是两条迟早漂移的通路。
//
// 边界：本模块不管字节读取（文档只递路径），也不管归一（图片那条在 chatImage.ts）。
import type { AiAttachKind } from "../types";

/** 每轮文档个数上限；与后端 `attach::MAX_FILES_PER_TURN` 同值，前端先拦一层免得白跑 IPC。 */
export const FILE_MAX_COUNT = 5;

/**
 * `degraded` 的机读前缀 → i18n 键（后端串形如 `rows-truncated:共 5000 行仅列 200 行`）。
 *
 * 后端那句中文是给**模型**看的（进 UNTRUSTED_FILE 块首），界面不能直接借它：界面语言
 * 可能是 en/ja。故只取冒号前的代码自己出文案，认不出的代码回落原文——宁可文案生硬，
 * 也不能把「内容被丢了」这件事静默掉。
 */
const DEGRADED_KEYS: Record<string, string> = {
  binary: "aw.degBinary",
  oversize: "aw.degOversize",
  truncated: "aw.degTruncated",
  "rows-truncated": "aw.degRowsTruncated",
  "lossy-gb18030": "aw.degLossy",
  unsupported: "aw.degUnsupported",
  "parse-failed": "aw.degParseFailed",
  empty: "aw.degEmpty",
  scanned: "aw.degScanned",
  garbled: "aw.degGarbled",
  "layout-lossy": "aw.degLayoutLossy",
  shallow: "aw.degShallow",
};

/**
 * 退化细节（各段冒号后的原文，含数字），供 tooltip；一句里没细节则只回标签。
 * 认不出的代码（后端新增而前端未同步）整段就是细节，不会丢。
 */
export function degradedDetail(degraded?: string | null): string {
  if (!degraded) return "";
  return splitDegraded(degraded)
    .map(([code, detail]) => {
      if (!DEGRADED_KEYS[code]) return detail;
      const rest = detail.slice(code.length).replace(/^:\s*/, "").trim();
      return rest || null;
    })
    .filter((s): s is string => !!s)
    .join(" · ");
}

/**
 * 拆后端的多因串：`a:x; b:y` → `[["a","a:x"], ["b","b:y"]]`。
 *
 * 不能直接 `split(":")` 取首段——后端说明句里带的是全角冒号（`：`），而代码分隔用半角；
 * 真正的坑是「一句说明里含 `;`」或「不含代码前缀」（如 IR 已不在磁盘上那句），
 * 所以只在切出的头段确实是已知代码时才认它，否则整段回落原文。丢了哪一段都比认错强。
 */
function splitDegraded(s: string): [string, string][] {
  const out: [string, string][] = [];
  let rest = s;
  while (rest.length) {
    const cut = rest.indexOf("; ");
    const seg = cut < 0 ? rest : rest.slice(0, cut);
    rest = cut < 0 ? "" : rest.slice(cut + 2);
    const idx = seg.indexOf(":");
    out.push([idx < 0 ? seg : seg.slice(0, idx), seg]);
  }
  return out;
}

/**
 * 附件大类的短标签（chip 上的类型徽标）。只认后端给的 `kind`，不从扩展名猜：
 * 判型的唯一权威在后端（不信扩展名是那条链的前提，前端再猜一次就把它破坏了）。
 */
export function kindLabel(tr: (key: string) => string, kind: AiAttachKind | undefined): string {
  switch (kind) {
    case "table":
      return tr("aw.kindTable");
    case "doc":
      return tr("aw.kindDoc");
    case "binary":
      return tr("aw.kindBinary");
    case "text":
      return tr("aw.kindText");
    default:
      return tr("aw.kindImage");
  }
}
