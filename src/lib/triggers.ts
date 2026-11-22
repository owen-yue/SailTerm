//  触发匹配：对终端输出片段做本地正则匹配，产出动作（告警 / 高亮行号）。
// 纯函数、不碰 IPC；编译失败的规则静默跳过（fail-safe，不让坏规则打断终端）。
import type { TriggerRule } from "../types";

/** 一条命中结果。 */
export interface TriggerHit {
  rule: TriggerRule;
  /** 命中的文本片段（截断展示用）。 */
  match: string;
}

/** 同一规则的告警节流窗口（毫秒），防止刷屏。 */
const ALERT_THROTTLE_MS = 3000;

const lastAlert = new Map<string, number>();

/**
 * 对一段输出跑所有启用规则，返回命中列表。
 *
 * `colorize` 规则**不在此处理**：着色要等 ANSI 解析成行后才能判断「服务端是否已发色」，
 * 走 [`termHighlight.attachTermHighlight`] 读缓冲行；这里按写入分片匹配，只负责告警与瞬时闪高亮。
 */
export function scanTriggers(text: string, rules: TriggerRule[], now = Date.now()): TriggerHit[] {
  if (!text) return [];
  const hits: TriggerHit[] = [];
  for (const rule of rules) {
    if (!rule.enabled || rule.action === "colorize") continue;
    let re: RegExp;
    try {
      re = new RegExp(rule.pattern, "i");
    } catch {
      continue;
    }
    const m = re.exec(text);
    if (!m) continue;
    // 告警节流：同一规则 3s 内只报一次；高亮不节流（本来就是就地标记）
    if (rule.action === "alert") {
      const prev = lastAlert.get(rule.id) ?? 0;
      if (now - prev < ALERT_THROTTLE_MS) continue;
      lastAlert.set(rule.id, now);
    }
    hits.push({ rule, match: m[0].slice(0, 80) });
  }
  return hits;
}
