// 开发者工具箱的纯逻辑（无 React、无 IPC）：编解码 / 格式化 / 命名转换 / 随机串 / 哈希。
// 与视图解耦，便于单测与复用；SHA/HMAC 走 Web Crypto，MD5 为自带 RFC 1321 实现。

// —— UTF-8 安全的 Base64 ——

/** 文本 → Base64（按 UTF-8 字节编码，中文安全）。 */
export function b64encode(s: string): string {
  const bytes = new TextEncoder().encode(s);
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}

/** Base64 → 文本（忽略换行/空格；非法字符抛错由调用方兜底）。 */
export function b64decode(s: string): string {
  const bin = atob(s.replace(/\s+/g, ""));
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

/** Base64URL（JWT 段）→ 文本。 */
export function b64urlDecode(s: string): string {
  let t = s.replace(/-/g, "+").replace(/_/g, "/");
  while (t.length % 4) t += "=";
  return b64decode(t);
}

// —— JSON ——

/** 解析并重新序列化 JSON；非法即抛错（调用方展示）。spaces=2 格式化，0 压缩。 */
export function reformatJson(s: string, spaces: number): string {
  return JSON.stringify(JSON.parse(s), null, spaces);
}

// —— 命名风格转换 ——

/** 拆词：兼容 camelCase / PascalCase / snake_case / kebab-case / 空格 / 点分。 */
export function splitWords(s: string): string[] {
  return s
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .split(/[\s_\-.]+/)
    .filter(Boolean)
    .map((w) => w.toLowerCase());
}

const cap = (w: string): string => w.charAt(0).toUpperCase() + w.slice(1);

export const toCamel = (s: string): string => splitWords(s).map((w, i) => (i ? cap(w) : w)).join("");
export const toPascal = (s: string): string => splitWords(s).map(cap).join("");
export const toSnake = (s: string): string => splitWords(s).join("_");
export const toKebab = (s: string): string => splitWords(s).join("-");
export const toConstant = (s: string): string => splitWords(s).join("_").toUpperCase();

// —— 随机串 ——

/** 密码字符集（剔除易混字符 0O1lI）。 */
const POOLS = {
  lower: "abcdefghijkmnopqrstuvwxyz",
  upper: "ABCDEFGHJKLMNPQRSTUVWXYZ",
  digit: "23456789",
  symbol: "!@#$%^&*-_=+",
} as const;

export type PwSet = keyof typeof POOLS;

/** 密码学随机取一个 [0,n) 整数。 */
function randInt(n: number): number {
  const buf = new Uint32Array(1);
  crypto.getRandomValues(buf);
  return buf[0] % n;
}

/** 生成随机密码：保证每个所选字符集至少出现一次，其余从全集随机取，末位打乱。 */
export function randomPassword(len: number, sets: PwSet[]): string {
  const active = sets.length ? sets : (Object.keys(POOLS) as PwSet[]);
  const all = active.map((k) => POOLS[k]).join("");
  const chars: string[] = active.map((k) => POOLS[k][randInt(POOLS[k].length)]);
  while (chars.length < len) chars.push(all[randInt(all.length)]);
  // Fisher-Yates 打乱，避免"必选字符"总在前几位。
  for (let i = chars.length - 1; i > 0; i--) {
    const j = randInt(i + 1);
    [chars[i], chars[j]] = [chars[j], chars[i]];
  }
  return chars.slice(0, len).join("");
}

/** 生成一个随机 token（hex，长度 = 字节数×2）。 */
export function randomHexToken(bytes = 32): string {
  const arr = new Uint8Array(bytes);
  crypto.getRandomValues(arr);
  return [...arr].map((b) => b.toString(16).padStart(2, "0")).join("");
}

// —— 哈希 ——

function toHex(buf: ArrayBuffer): string {
  return [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** SHA-1 / SHA-256 / SHA-512 摘要（hex）。 */
export async function shaHex(algo: "SHA-1" | "SHA-256" | "SHA-512", text: string): Promise<string> {
  const data = new TextEncoder().encode(text);
  return toHex(await crypto.subtle.digest(algo, data));
}

/** HMAC-SHA256（key + message → hex）。 */
export async function hmacSha256Hex(key: string, text: string): Promise<string> {
  const enc = new TextEncoder();
  const k = await crypto.subtle.importKey(
    "raw",
    enc.encode(key),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  return toHex(await crypto.subtle.sign("HMAC", k, enc.encode(text)));
}

// —— MD5（RFC 1321，纯前端；Web Crypto 不支持 MD5 故自带实现）——

const MD5_S = [
  7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
  4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];
const MD5_K: number[] = [];
for (let i = 0; i < 64; i++) MD5_K[i] = Math.floor(Math.abs(Math.sin(i + 1)) * 4294967296) >>> 0;

function md5Bytes(input: number[]): string {
  const msg = input.slice();
  const bitLen = input.length * 8;
  msg.push(0x80);
  while (msg.length % 64 !== 56) msg.push(0);
  for (let i = 0; i < 4; i++) msg.push((bitLen >>> (8 * i)) & 0xff);
  for (let i = 0; i < 4; i++) msg.push(0); // 高 32 位（输入 < 512MB，恒 0）

  let a0 = 0x67452301, b0 = 0xefcdab89, c0 = 0x98badcfe, d0 = 0x10325476;
  for (let off = 0; off < msg.length; off += 64) {
    const M: number[] = [];
    for (let i = 0; i < 16; i++) {
      M[i] =
        (msg[off + i * 4] |
          (msg[off + i * 4 + 1] << 8) |
          (msg[off + i * 4 + 2] << 16) |
          (msg[off + i * 4 + 3] << 24)) >>>
        0;
    }
    let A = a0, B = b0, C = c0, D = d0;
    for (let i = 0; i < 64; i++) {
      let F: number, g: number;
      if (i < 16) {
        F = (B & C) | (~B & D);
        g = i;
      } else if (i < 32) {
        F = (D & B) | (~D & C);
        g = (5 * i + 1) % 16;
      } else if (i < 48) {
        F = B ^ C ^ D;
        g = (3 * i + 5) % 16;
      } else {
        F = C ^ (B | ~D);
        g = (7 * i) % 16;
      }
      F = (F + A + MD5_K[i] + M[g]) >>> 0;
      A = D;
      D = C;
      C = B;
      B = (B + ((F << MD5_S[i]) | (F >>> (32 - MD5_S[i])))) >>> 0;
    }
    a0 = (a0 + A) >>> 0;
    b0 = (b0 + B) >>> 0;
    c0 = (c0 + C) >>> 0;
    d0 = (d0 + D) >>> 0;
  }
  const le = (n: number): string => {
    let s = "";
    for (let i = 0; i < 4; i++) s += ((n >>> (8 * i)) & 0xff).toString(16).padStart(2, "0");
    return s;
  };
  return le(a0) + le(b0) + le(c0) + le(d0);
}

/** MD5（UTF-8 文本 → hex）。 */
export function md5Hex(text: string): string {
  return md5Bytes([...new TextEncoder().encode(text)]);
}
