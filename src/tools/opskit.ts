// 运维计算工具箱的纯逻辑（无 React、无 IPC）：CIDR 子网计算 / Cron 解析与未来触发 / 行级 Diff。
// 与视图解耦，便于单测与复用；正则测试直接用 JS 原生 RegExp，在视图内驱动，无需此处。

// ————————————————————————— CIDR / 子网 —————————————————————————

/** 点分 IPv4 → 无符号 32 位整数；非法返回 null。 */
export function ipv4ToLong(ip: string): number | null {
  const parts = ip.trim().split(".");
  if (parts.length !== 4) return null;
  let n = 0;
  for (const p of parts) {
    if (!/^\d{1,3}$/.test(p)) return null;
    const v = Number(p);
    if (v > 255) return null;
    n = n * 256 + v;
  }
  return n >>> 0;
}

/** 无符号 32 位整数 → 点分 IPv4。 */
export function longToIpv4(n: number): string {
  const x = n >>> 0;
  return `${(x >>> 24) & 255}.${(x >>> 16) & 255}.${(x >>> 8) & 255}.${x & 255}`;
}

/** 解析 CIDR 写法：`192.168.1.0/24`（也允许裸 IP，默认 /32）。非法抛错（调用方兜底展示）。 */
export function parseCidr(input: string): { long: number; prefix: number } {
  const s = input.trim();
  const slash = s.indexOf("/");
  const ipPart = slash < 0 ? s : s.slice(0, slash);
  const prePart = slash < 0 ? "32" : s.slice(slash + 1);
  const long = ipv4ToLong(ipPart);
  if (long === null) throw new Error("ipv4Bad");
  if (!/^\d{1,2}$/.test(prePart)) throw new Error("prefixBad");
  const prefix = Number(prePart);
  if (prefix > 32) throw new Error("prefixBad");
  return { long, prefix };
}

/** 子网计算结果（各地址为点分字符串；计数为 number，展示时按千分位格式化）。 */
export interface CidrResult {
  cidr: string;
  network: string;
  broadcast: string;
  netmask: string;
  wildcard: string;
  prefix: number;
  /** 该网段 IP 总数 = 2^(32-prefix)。 */
  total: number;
  /** 可分配主机数（/31→2，/32→1，其余 total-2）。 */
  usable: number;
  firstHost: string;
  lastHost: string;
  /** 输入的那个 IP 在本机视角的角色：network/broadcast/host/usable 之一。 */
  inputRole: string;
}

/** 由 CIDR 串算子网信息。输入 IP 不要求是网络地址（自动按掩码归网并判定其角色）。 */
export function calcCidr(input: string): CidrResult {
  const { long, prefix } = parseCidr(input);
  const mask = prefix === 0 ? 0 : (0xffffffff << (32 - prefix)) >>> 0;
  const wildcard = (~mask) >>> 0;
  const network = (long & mask) >>> 0;
  const broadcast = (network | wildcard) >>> 0;
  const total = Math.pow(2, 32 - prefix);
  let usable: number;
  let firstHost: string;
  let lastHost: string;
  if (prefix === 32) {
    usable = 1;
    firstHost = longToIpv4(network);
    lastHost = longToIpv4(network);
  } else if (prefix === 31) {
    usable = 2;
    firstHost = longToIpv4(network);
    lastHost = longToIpv4(broadcast);
  } else {
    usable = total - 2;
    firstHost = longToIpv4(network + 1);
    lastHost = longToIpv4(broadcast - 1);
  }
  let inputRole: string;
  if (prefix >= 31) inputRole = "host";
  else if (long === network) inputRole = "network";
  else if (long === broadcast) inputRole = "broadcast";
  else inputRole = "usable";
  return {
    cidr: `${longToIpv4(network)}/${prefix}`,
    network: longToIpv4(network),
    broadcast: longToIpv4(broadcast),
    netmask: longToIpv4(mask),
    wildcard: longToIpv4(wildcard),
    prefix,
    total,
    usable,
    firstHost,
    lastHost,
    inputRole,
  };
}

// ————————————————————————— Cron 表达式 —————————————————————————

/** 五个字段的允许值集合（分/时/日/月/周）。 */
export interface CronFields {
  minute: Set<number>;
  hour: Set<number>;
  dom: Set<number>;
  month: Set<number>;
  dow: Set<number>;
  /** 日字段是否为 `*`（用于 dom/dow 的 OR 语义判定）。 */
  domStar: boolean;
  dowStar: boolean;
}

const MONTH_NAMES = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
const DOW_NAMES = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

/** 单个 cron 字段 → 值集合。支持 `*` `,` `-` `/` 与月/周英文名。越界抛错（调用方兜底）。 */
function parseCronField(field: string, min: number, max: number, names?: string[], isDow?: boolean): Set<number> {
  const set = new Set<number>();
  const nameMap = new Map<string, number>();
  // 英文名按序号映射到值：月 min=1 → JAN(idx0)=1；周 min=0 → SUN(idx0)=0。统一 idx+min。
  if (names) names.forEach((n, idx) => nameMap.set(n, idx + min));
  const resolve = (tok: string): number => {
    const t = tok.trim().toUpperCase();
    if (nameMap.has(t)) return nameMap.get(t)!;
    if (!/^\d+$/.test(t)) throw new Error("cronFieldBad");
    let v = Number(t);
    if (isDow && v === 7) v = 0; // 7 视为周日
    return v;
  };
  for (const part of field.split(",")) {
    if (part === "") throw new Error("cronFieldBad");
    const [rangeStr, stepStr] = part.split("/");
    const step = stepStr === undefined ? 1 : Number(stepStr);
    if (!Number.isInteger(step) || step <= 0) throw new Error("cronFieldBad");
    let lo: number;
    let hi: number;
    if (rangeStr === "*") {
      lo = min;
      hi = max;
    } else if (rangeStr.includes("-")) {
      const [a, b] = rangeStr.split("-");
      lo = resolve(a);
      hi = resolve(b);
    } else {
      lo = resolve(rangeStr);
      // `a/n` → 从 a 到 max 按 n 步进；单独 `a` → 仅 a。
      hi = stepStr === undefined ? lo : max;
    }
    if (lo < min || hi > max || lo > hi) throw new Error("cronFieldBad");
    for (let v = lo; v <= hi; v += step) set.add(v);
  }
  return set;
}

/** 解析 5 字段 cron；字段数不符或值越界抛错。 */
export function parseCron(expr: string): CronFields {
  const parts = expr.trim().split(/\s+/);
  if (parts.length !== 5) throw new Error("cronLen");
  const [mi, hr, dom, mon, dow] = parts;
  return {
    minute: parseCronField(mi, 0, 59),
    hour: parseCronField(hr, 0, 23),
    dom: parseCronField(dom, 1, 31),
    month: parseCronField(mon, 1, 12, MONTH_NAMES),
    dow: parseCronField(dow, 0, 6, DOW_NAMES, true),
    domStar: dom === "*",
    dowStar: dow === "*",
  };
}

/** 判断某 Date 是否命中 cron（含 dom/dow 双限制时的 OR 语义）。 */
function cronMatch(d: Date, cf: CronFields): boolean {
  if (!cf.month.has(d.getMonth() + 1)) return false;
  const domOk = cf.dom.has(d.getDate());
  const dowOk = cf.dow.has(d.getDay());
  // 日与周都被限制（都非 *）时：满足其一即可（vixie cron 语义）。
  if (!cf.domStar && !cf.dowStar) return domOk || dowOk;
  return domOk && dowOk;
}

/** 从 `from` 起算未来 `count` 次触发时间（本地时区的 Date 数组，严格晚于 from）。 */
export function cronNextRuns(cf: CronFields, from: Date, count: number): Date[] {
  const out: Date[] = [];
  const cur = new Date(from);
  cur.setSeconds(0, 0);
  cur.setMinutes(cur.getMinutes() + 1); // 严格从下一整分钟起
  const guard = 500_000; // 最多推进 50 万次循环（含按日/月跳跃，实际远小于此），防永久不匹配死循环
  let steps = 0;
  while (out.length < count && steps < guard) {
    steps++;
    if (!cf.month.has(cur.getMonth() + 1)) {
      cur.setMonth(cur.getMonth() + 1, 1);
      cur.setHours(0, 0, 0, 0);
      continue;
    }
    if (!cronMatch(cur, cf)) {
      cur.setDate(cur.getDate() + 1);
      cur.setHours(0, 0, 0, 0);
      continue;
    }
    if (!cf.hour.has(cur.getHours())) {
      cur.setHours(cur.getHours() + 1, 0, 0, 0);
      continue;
    }
    if (!cf.minute.has(cur.getMinutes())) {
      cur.setMinutes(cur.getMinutes() + 1, 0, 0);
      continue;
    }
    out.push(new Date(cur));
    cur.setMinutes(cur.getMinutes() + 1, 0, 0);
  }
  return out;
}

// ————————————————————————— 行级文本 Diff —————————————————————————

export type DiffKind = "same" | "add" | "del";

export interface DiffRow {
  kind: DiffKind;
  text: string;
  /** 旧文行号（add 行为 null）。 */
  oldNo: number | null;
  /** 新文行号（del 行为 null）。 */
  newNo: number | null;
}

/** 经典 LCS 行级 diff：返回按序的 same/del/add 行。过大输入自动降为朴素尾部对比以防卡顿。 */
export function diffLines(aText: string, bText: string): { rows: DiffRow[]; added: number; removed: number } {
  const a = aText.split("\n");
  const b = bText.split("\n");
  const rows = lcsDiff(a, b);
  let added = 0;
  let removed = 0;
  for (const r of rows) {
    if (r.kind === "add") added++;
    else if (r.kind === "del") removed++;
  }
  return { rows, added, removed };
}

/** LCS 动态规划回溯生成 diff 行（带 oldNo/newNo）。 */
function lcsDiff(a: string[], b: string[]): DiffRow[] {
  const n = a.length;
  const m = b.length;
  // dp[i][j] = a[i:] 与 b[j:] 的 LCS 长度
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }
  const rows: DiffRow[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      rows.push({ kind: "same", text: a[i], oldNo: i + 1, newNo: j + 1 });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      rows.push({ kind: "del", text: a[i], oldNo: i + 1, newNo: null });
      i++;
    } else {
      rows.push({ kind: "add", text: b[j], oldNo: null, newNo: j + 1 });
      j++;
    }
  }
  while (i < n) rows.push({ kind: "del", text: a[i], oldNo: i + 1, newNo: null }), i++;
  while (j < m) rows.push({ kind: "add", text: b[j], oldNo: null, newNo: j + 1 }), j++;
  return rows;
}
