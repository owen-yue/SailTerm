// 关键字着色引擎：把 colorize 类触发规则套到**已渲染的终端缓冲行**上（客户端语义着色）。
//
// 与 `triggers.ts#scanTriggers` 的分工：那条路看的是「刚写进来的原始字节分片」，用于告警 toast
// 与瞬时闪高亮，会被写入边界切断；着色要求「一整行里无论来自几个分片都能整词上色」，且必须等
// ANSI 序列解析完才知道哪些字带色，所以这里走 `onWriteParsed` + buffer API。
//
// 三条红线（改动前请先读）：
// 1. **服务端色优先**：匹配到的片段若已带前景色（bash 提示符、ls 文件类型色、gcc 诊断色），不再覆盖；
// 2. **备用屏不插手**：vim/less/top 等 TUI 期间整体不着色并清空已有着色，绝不抢它们自己的配色；
// 3. **只挂装饰不改内容**：全程只读 buffer、只加 decoration，选中/复制/搜索拿到的永远是原文。
import type { IBufferCell, IBufferLine, IDecoration, IMarker, Terminal } from "@xterm/xterm";
import type { TriggerColor, TriggerRule } from "../types";

/** 增量扫描单批最多处理的行数：刷屏时只着色窗口内的行，宁漏不卡。 */
const LINES_PER_PARSE = 120;

/** 全量重扫回看的历史行数上限（scrollback 可达 10 万行，不能真扫到底）。 */
const RESCAN_MAX_LINES = 3000;

/** 存活着色装饰上限：超出后先清理已失效的，再 FIFO 淘汰最旧的。 */
const MAX_DECORATIONS = 2000;

/** 单行最多着色片段数：坏规则（如 `.`）刷一行时不至于挂满装饰。 */
const MAX_MATCHES_PER_LINE = 12;

/** 编译后的规则：正则 + 语义色键。 */
interface CompiledRule {
  re: RegExp;
  color: TriggerColor;
}

/** 一条着色 = marker（跟踪行在回滚中的位移）+ decoration（上色）。 */
interface Painted {
  marker: IMarker;
  deco: IDecoration;
}

/**
 * xterm 6 的 `translateToString` 支持第 4 参回填「字符串下标 → 列下标」，
 * 用于宽字符（CJK/emoji）对齐——字符串里一个汉字 1 个下标，缓冲里却占 2 列。
 * 该参数尚未进公开 typings，此处按最小面声明（取不到时按 1:1 退化，只影响宽字符行）。
 */
type LineWithColumns = {
  translateToString(trimRight?: boolean, start?: number, end?: number, outColumns?: number[]): string;
};

/** 着色引擎对外接口（由 `Terminal.tsx` 挂在 xterm 实例生命周期上）。 */
export interface TermHighlight {
  /** 每批写入解析完后调用（挂 `term.onWriteParsed`）：给新出现的行上色。 */
  onParsed(): void;
  /** 规则 / 主题 / 开关变化：清空已有着色并按当前配置回看重扫。 */
  rescan(): void;
  dispose(): void;
}

/**
 * 在给定 xterm 实例上附着关键字着色。
 *
 * 三个回调都是**惰性取值**：引擎不持有 store，配置变了下一批写入自然生效；
 * 需要立即反映到历史行时由调用方触发 [`TermHighlight.rescan`]。
 */
export function attachTermHighlight(
  term: Terminal,
  opts: {
    /** 全量触发规则（引擎自行筛 colorize + enabled）。 */
    rules: () => TriggerRule[];
    /** 语义色键 → 当前主题的十六进制。 */
    colorFor: (key: TriggerColor) => string;
    /** 总开关（关掉即清空并停止着色）。 */
    enabled: () => boolean;
  },
): TermHighlight {
  let painted: Painted[] = [];
  /** 规则签名 → 编译缓存；规则一改签名即变，顺带触发重扫。 */
  let cacheSig = "";
  let cache: CompiledRule[] = [];
  /** 已扫到的绝对行号；-1 = 尚未扫过。 */
  let lastCursor = -1;
  /** 光标所在行（可能被后续输入改写），其着色每批都要重画。 */
  let editLine = -1;
  let editPainted: Painted[] = [];

  /** 只留 colorize 且启用的规则；坏正则静默跳过（与 `scanTriggers` 同 fail-safe 口径）。 */
  function compile(): CompiledRule[] {
    const active = opts.rules().filter((r) => r.enabled && r.action === "colorize");
    const sig = active.map((r) => `${r.id}\u0000${r.pattern}\u0000${r.color}`).join("\u0001");
    if (sig === cacheSig) return cache;
    cacheSig = sig;
    cache = [];
    for (const r of active) {
      try {
        cache.push({ re: new RegExp(r.pattern, "gi"), color: r.color });
      } catch {
        /* 坏规则跳过，绝不打断终端渲染 */
      }
    }
    return cache;
  }

  function drop(list: Painted[]): void {
    for (const p of list) {
      if (!p.deco.isDisposed) p.deco.dispose();
      if (!p.marker.isDisposed) p.marker.dispose();
    }
  }

  function clear(): void {
    drop(painted);
    painted = [];
    drop(editPainted);
    editPainted = [];
  }

  /** 装饰总量守门：先回收已随行出缓冲而失效的，仍超限才淘汰最旧的。 */
  function trim(): void {
    if (painted.length < MAX_DECORATIONS) return;
    painted = painted.filter((p) => !p.deco.isDisposed);
    while (painted.length > MAX_DECORATIONS) {
      const old = painted.shift();
      if (old) drop([old]);
    }
  }

  /** 片段是否已被服务端上色（任一格带前景色即算）。 */
  function alreadyColored(line: IBufferLine, from: number, to: number, cell: IBufferCell): boolean {
    for (let x = from; x < to; x++) {
      const c = line.getCell(x, cell);
      if (!c || c.getWidth() === 0) continue; // 宽字符占位格，颜色归前一格
      if (!c.isFgDefault()) return true;
    }
    return false;
  }

  function overlaps(ranges: [number, number][], from: number, to: number): boolean {
    return ranges.some(([s, e]) => from < e && s < to);
  }

  /** 给一行的 [col, col+width) 上色；marker 以光标行为基准，故偏移恒 <= 0。 */
  function paint(y: number, cursor: number, col: number, width: number, color: TriggerColor): Painted | null {
    if (width <= 0 || col < 0) return null;
    const marker = term.registerMarker(y - cursor);
    if (!marker) return null;
    const deco = term.registerDecoration({
      marker,
      x: col,
      width: Math.min(width, term.cols - col),
      foregroundColor: opts.colorFor(color),
      layer: "bottom", // 画在选区之下，避免盖掉选中态
    });
    if (!deco) {
      marker.dispose();
      return null;
    }
    return { marker, deco };
  }

  /** 对 [from, to] 区间的绝对行号逐行着色。 */
  function scan(from: number, to: number, cursor: number, rules: CompiledRule[]): void {
    const buf = term.buffer.active;
    const cell = buf.getNullCell();
    for (let y = Math.max(0, from); y <= to; y++) {
      const line = buf.getLine(y);
      if (!line) continue;
      const cols: number[] = [];
      const text = (line as unknown as LineWithColumns).translateToString(true, 0, undefined, cols);
      if (!text) continue;
      const covered: [number, number][] = [];
      let n = 0;
      for (const rule of rules) {
        rule.re.lastIndex = 0;
        let m: RegExpExecArray | null;
        while (n < MAX_MATCHES_PER_LINE && (m = rule.re.exec(text))) {
          if (!m[0]) {
            rule.re.lastIndex += 1; // 零宽匹配：手动前进，防死循环
            continue;
          }
          // cols[i] = 第 i 个字符的列号；末位是行尾列，故可直接取右边界
          const s = cols[m.index] ?? m.index;
          const e = cols[m.index + m[0].length] ?? s + m[0].length;
          if (!overlaps(covered, s, e) && !alreadyColored(line, s, e, cell)) {
            const p = paint(y, cursor, s, e - s, rule.color);
            if (p) {
              (y === cursor ? editPainted : painted).push(p);
              covered.push([s, e]);
              n += 1;
            }
          } else {
            n += 1; // 冲突/已上色也算一次处理，坏规则同样不能拖住这一行
          }
        }
      }
    }
  }

  return {
    onParsed(): void {
      const buf = term.buffer.active;
      const cursor = buf.baseY + buf.cursorY;
      // 备用屏（vim/less/top）：清场退出，着色归 TUI 自己
      if (buf.type !== "normal" || !opts.enabled()) {
        clear();
        lastCursor = -1;
        editLine = -1;
        return;
      }
      const rules = compile();
      if (!rules.length) {
        lastCursor = cursor;
        return;
      }
      // 绝对行号失去单调性（清屏 / 换屏 / 收缩）：整体重扫一次
      if (cursor < lastCursor) {
        this.rescan();
        return;
      }
      // 光标行内容会被后续输入改写（打字 / 退格 / AI 落笔），每批重画它
      if (editLine !== cursor) {
        drop(editPainted);
        editPainted = [];
        editLine = cursor;
      }
      scan(Math.max(lastCursor + 1, cursor - LINES_PER_PARSE + 1), cursor - 1, cursor, rules);
      scan(cursor, cursor, cursor, rules);
      lastCursor = cursor;
      trim();
    },

    rescan(): void {
      clear();
      const buf = term.buffer.active;
      if (buf.type !== "normal" || !opts.enabled()) {
        lastCursor = -1;
        editLine = -1;
        return;
      }
      const cursor = buf.baseY + buf.cursorY;
      const rules = compile();
      scan(cursor - RESCAN_MAX_LINES + 1, cursor - 1, cursor, rules);
      scan(cursor, cursor, cursor, rules);
      lastCursor = cursor;
      editLine = cursor;
      trim();
    },

    dispose(): void {
      clear();
      cache = [];
      cacheSig = "";
    },
  };
}
