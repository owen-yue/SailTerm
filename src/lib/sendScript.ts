// 发送工具的条目解析：把编辑区文本按「格式 × 分隔档」拆成一串待发字节序列。
//
// 纯函数、零依赖（含零 Tauri 依赖），单测钉住分隔与补回车语义：这些细节出错的表现是
// 「发出去的东西和屏幕上写的不一样」，属真机上最难肉眼发现的一类，只能靠用例锁。
//
// 三档分隔的产品语义：
// - `line` 按行：换行是分隔符，`|` 是命令的一部分（shell 管道要用它）；
// - `pipe` 按竖线：`|` 也是分隔符，用于一行里排好几条（换行同样作分隔，只是视觉折行）；
// - `char` 按字符：逐字符一条（Hex 档下逐字节），恒不补回车——那是「模拟按键节奏」的用途。

/** 发送格式：文本按 UTF-8 编码；Hex 把 `48 65` 这类十六进制串解析成原始字节。 */
export type SendFormat = "text" | "hex";

/** 分隔档：行 / 竖线 / 字符。 */
export type SendSplit = "line" | "pipe" | "char";

/**
 * 解析失败的结构化原因：`key` 是 i18n 键（文案归前端词典，纯函数不产人类语言），
 * `at` 是 Hex 档下出错所在的条目序号（0 起），供 UI 指到那一行。
 *
 * 空内容不在这里：它不是错误，就是「没东西可发」，由 UI 把开始按钮置灰来表达。
 */
export type SendError = { key: "sd.errHex"; at?: number };

/** 解析结果：条目序列 + 失败原因（`error` 非空即不可发送）。 */
export interface SendPlan {
  entries: Uint8Array[];
  error: SendError | null;
}

/** 回车恒为 `\r`（Enter 键语义）：裸 `\n` 在 PowerShell/PSReadLine 下会被吃进多行编辑， */
/** 变成未闭合的 `>>` 续行提示，命令根本不执行。 */
const CR = 0x0d;

/** 按分隔档把原文切成条目串（不做任何编码、不去改内容本身）。 */
export function splitSegments(src: string, split: SendSplit): string[] {
  let parts: string[];
  if (split === "line") parts = src.split(/\r?\n/);
  else if (split === "pipe") parts = src.replace(/\r?\n/g, "|").split("|");
  // 逐字符：按码点切（代理对不拆散，emoji 也是一个字符）；换行只当分隔、本身不发出去。
  else parts = [...src].filter((c) => c !== "\n" && c !== "\r");
  // 空白条目一律跳过：编辑器尾部那个空行不该变成一条「只按回车」的误发。
  return parts.filter((p) => p.trim() !== "");
}

/**
 * Hex 串 → 字节。容忍空格/冒号/连字符分隔与 `0x` 前缀（人写的 hex 什么样都有），
 * 清洗后必须是偶数长度的十六进制串，否则回 `null` 由调用方归错。
 */
export function hexToBytes(seg: string): Uint8Array | null {
  const clean = seg
    .replace(/[\s:,-]+/g, "")
    .replace(/(?:0x|0X)/g, "");
  if (clean.length === 0) return new Uint8Array();
  if (clean.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(clean)) return null;
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  return out;
}

const encoder = new TextEncoder();

/** 文本条目 → 字节（UTF-8，与后端 `session_write` 的 `into_bytes()` 同编码）。 */
function textBytes(s: string): Uint8Array {
  return encoder.encode(s);
}

/** 在条目末尾追加一字节 `\r`（发送后执行的「追加回车」开关）。 */
function withCr(bytes: Uint8Array): Uint8Array {
  const out = new Uint8Array(bytes.length + 1);
  out.set(bytes);
  out[bytes.length] = CR;
  return out;
}

/**
 * 把编辑区内容解析成待发条目序列。
 *
 * `appendEnter` 只在 `line`/`pipe` 档生效：按字符档的用途是逐键模拟，补回车等于每敲一个
 * 字符就执行一次，完全不是同一件事（三档语义见文件头）。
 * 解析出错即整段不可发送（不做「发前面几条」的半途行为：脚本是整体，静默截断比报错危险）；
 * 拆出零条则不算错——空编辑区只是没得发，回 `{ entries: [], error: null }` 即可。
 */
export function parseEntries(
  src: string,
  fmt: SendFormat,
  split: SendSplit,
  appendEnter: boolean,
): SendPlan {
  // Hex × 按字符：这一格里的「字符」是字节，不是编辑区里的可见字符——必须先把整段
  // hex 合成一条字节流再逐字节拆。走 splitSegments 会把 `48 65` 切成 4/8/6/5 四个
  // 奇数长度的片段，每个都判成非法十六进制。
  if (fmt === "hex" && split === "char") {
    const stream = hexToBytes(src);
    if (stream === null) return { entries: [], error: { key: "sd.errHex" } };
    return { entries: Array.from(stream, (b) => Uint8Array.of(b)), error: null };
  }
  const segs = splitSegments(src, split);
  const enter = appendEnter && split !== "char";
  const entries: Uint8Array[] = [];
  for (let i = 0; i < segs.length; i++) {
    if (fmt === "text") {
      entries.push(enter ? withCr(textBytes(segs[i])) : textBytes(segs[i]));
      continue;
    }
    const bytes = hexToBytes(segs[i]);
    if (bytes === null) return { entries: [], error: { key: "sd.errHex", at: i } };
    entries.push(enter ? withCr(bytes) : bytes);
  }
  return { entries, error: null };
}

/** 计数上限：够覆盖「跑一千轮压一压」这类需求，又挡住把间隔计时器写成天文数字。 */
export const MAX_COUNT = 9999;

/** 间隔下限（秒）：0 间隔逐条 IPC 会把 PTY 灌成一坨，也吃满 WebView 的事件循环。 */
export const MIN_INTERVAL = 0.01;

/** 夹到合法区间；非有限数（空输入 / 粘进来的脏值）回落到 `fallback`。 */
export function clampNum(v: number, min: number, max: number, fallback: number): number {
  if (!Number.isFinite(v)) return fallback;
  return Math.min(max, Math.max(min, v));
}
