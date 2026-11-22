import { useEffect, useMemo, useRef, useState, type KeyboardEvent as RKeyboardEvent } from "react";
import { useI18n, useLang } from "../../lib/i18n";
import { kbdLabel } from "../../lib/kbd";
import { COMMANDS, COMMAND_GROUP_ORDER, CMD_GROUP_FAQ, fuzzyScore, type Command, type CommandCtx } from "../../lib/commands";
import { faqsFor, type Faq } from "../../lib/faq";
import { useEscClose } from "../../lib/useOverlay";

interface Props {
  open: boolean;
  onClose: () => void;
  ctx: CommandCtx;
  /** 直达某条答疑（lib/faqJump 编号）：非空时打开即切到答疑页签、展开该条并滚到可见。 */
  focusFaqNo?: number | null;
}

/** 面板页签：命令启动器 / 帮助与答疑。两块内容各自独立列表，搜索只在当前页签内过滤。 */
type Tab = "cmd" | "faq";

/**
 * 命令面板：即时过滤的命令启动器 + 帮助答疑（Ctrl+Shift+P / F1）。
 * 命令数据来自 lib/commands 的 COMMANDS（与快捷键帮助共用注册表）；
 * 答疑数据来自 lib/faq（读 md 语料，不写死在代码里）。↑↓ 移动、Enter 执行/展开、Esc 关闭。
 */
export default function CommandPalette({ open, onClose, ctx, focusFaqNo }: Props) {
  const tr = useI18n();
  const lang = useLang();
  const [q, setQ] = useState("");
  const [tab, setTab] = useState<Tab>("cmd");
  const [active, setActive] = useState(0);
  // 展开的答疑条目 id（面板内联展开，不关闭面板）。
  const [openFaq, setOpenFaq] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // 统一条目：命令与答疑共用一套搜索/排序/键盘导航，渲染与激活按 kind 分流。
  type Entry =
    | { kind: "cmd"; key: string; groupKey: string; cmd: Command }
    | { kind: "faq"; key: string; groupKey: string; faq: Faq };

  const entryText = (e: Entry): string =>
    e.kind === "cmd"
      ? `${tr(e.cmd.labelKey)} ${tr(e.cmd.groupKey)} ${e.cmd.keys ?? ""} ${e.cmd.id}`
      : `${e.faq.q} ${e.faq.a}`;

  // 当前页签的候选条目：命令来自注册表，答疑来自 md 语料（切语言重取）。
  const entries = useMemo<Entry[]>(() => {
    if (tab === "cmd")
      return COMMANDS.map((cmd) => ({ kind: "cmd" as const, key: cmd.id, groupKey: cmd.groupKey, cmd }));
    return faqsFor(lang).map((faq) => ({ kind: "faq" as const, key: faq.id, groupKey: CMD_GROUP_FAQ, faq }));
  }, [tab, lang, tr]);

  // 过滤 + 排序：命中分降序、同分按原序；命令页空查询按分组顺序铺全量，答疑页保持语料原序。
  const results = useMemo<Entry[]>(() => {
    const scored = entries
      .map((e, i) => ({ e, i, s: fuzzyScore(q, entryText(e)) }))
      .filter((r) => r.s >= 0);
    if (q.trim()) {
      scored.sort((a, b) => b.s - a.s || a.i - b.i);
    } else if (tab === "cmd") {
      const order = new Map(COMMAND_GROUP_ORDER.map((g, idx) => [g, idx]));
      scored.sort(
        (a, b) => (order.get(a.e.groupKey) ?? 99) - (order.get(b.e.groupKey) ?? 99) || a.i - b.i,
      );
    }
    return scored.map((r) => r.e);
  }, [entries, q, tab, tr]);

  // 打开时重置查询/页签/高亮/展开态并抢焦点（等一帧，遮罩刚挂载 input 尚不可聚焦）；
  // 直达模式例外：切答疑页签、展开指定编号并滚到居中——展开排在 tab 切换引发的收起之后，不会被误清
  useEffect(() => {
    if (!open) return;
    setQ("");
    setActive(0);
    if (focusFaqNo != null) {
      setTab("faq");
      requestAnimationFrame(() => {
        setOpenFaq(`faq-${focusFaqNo}`);
        requestAnimationFrame(() => {
          document
            .querySelector(`[data-faq="faq-${focusFaqNo}"]`)
            ?.scrollIntoView({ block: "center" });
        });
      });
    } else {
      setTab("cmd");
      setOpenFaq(null);
      requestAnimationFrame(() => inputRef.current?.focus());
    }
  }, [open, focusFaqNo]);

  // 查询或页签变化时收起已展开的答疑，避免结果重排后展开项错位。
  useEffect(() => {
    setOpenFaq(null);
  }, [q, tab]);

  // 结果数变化后把高亮夹回有效范围（过滤到更少项时避免指针越界）。
  useEffect(() => {
    setActive((a) => (a >= results.length ? 0 : a));
  }, [results.length]);

  // 统一 Esc 栈：面板开着时登记为最上层，保证与其他弹框叠加时 Esc 先关本面板（而非下层）。
  useEscClose(onClose, open);

  if (!open) return null;

  const switchTab = (next: Tab) => {
    if (next === tab) return;
    setTab(next);
    setQ("");
    setActive(0);
  };

  const activate = (idx: number) => {
    const e = results[idx];
    if (!e) return;
    if (e.kind === "cmd") {
      onClose();
      e.cmd.run(ctx);
    } else {
      setOpenFaq((prev) => (prev === e.faq.id ? null : e.faq.id));
    }
  };

  const onKeyDown = (e: RKeyboardEvent<HTMLDivElement>) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((a) => (results.length ? (a + 1) % results.length : 0));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => (results.length ? (a - 1 + results.length) % results.length : 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      activate(active);
    }
  };

  // 命令页：线性铺结果、按 groupKey 归并成分区；每项记住它在 results 里的下标（供高亮/执行对齐）。
  const groups: { key: string; items: { e: Entry; index: number }[] }[] = [];
  const seen = new Map<string, number>();
  results.forEach((e, index) => {
    let gi = seen.get(e.groupKey);
    if (gi === undefined) {
      gi = groups.length;
      seen.set(e.groupKey, gi);
      groups.push({ key: e.groupKey, items: [] });
    }
    groups[gi].items.push({ e, index });
  });

  const faqCount = tab === "faq" ? faqsFor(lang).length : 0;

  const renderFaq = (e: Entry, index: number) =>
    e.kind === "faq" ? (
      <div className="cmdp-faq" key={e.faq.id} data-faq={e.faq.id} data-open={openFaq === e.faq.id}>
        <button
          type="button"
          className="cmdp-item cmdp-faq-q"
          data-active={index === active}
          onMouseEnter={() => setActive(index)}
          onClick={() => activate(index)}
        >
          <span className="cmdp-faq-mark" aria-hidden>{`Q${e.faq.no}`}</span>
          <span className="cmdp-label">{e.faq.q}</span>
          <span className="cmdp-faq-chev" aria-hidden>
            {openFaq === e.faq.id ? "\u25be" : "\u25b8"}
          </span>
        </button>
        {openFaq === e.faq.id && <div className="cmdp-faq-a">{e.faq.a}</div>}
      </div>
    ) : null;

  return (
    <div className="cmdp-mask" onMouseDown={onClose}>
      <div
        className="cmdp"
        role="dialog"
        aria-modal="true"
        aria-label={tr("cmd.palette")}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="cmdp-tabs" role="tablist">
          <button
            type="button"
            role="tab"
            className="cmdp-tab"
            data-active={tab === "cmd"}
            aria-selected={tab === "cmd"}
            onClick={() => switchTab("cmd")}
          >
            {tr("cmd.tabCommands")}
          </button>
          <button
            type="button"
            role="tab"
            className="cmdp-tab"
            data-active={tab === "faq"}
            aria-selected={tab === "faq"}
            onClick={() => switchTab("faq")}
          >
            {tr(CMD_GROUP_FAQ)}
          </button>
          <button
            type="button"
            className="cmdp-close"
            onClick={onClose}
            aria-label={tr("common.close")}
          >
            ×
          </button>
        </div>
        <input
          ref={inputRef}
          className="cmdp-input"
          value={q}
          spellCheck={false}
          autoComplete="off"
          placeholder={tr("cmd.palettePh")}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={onKeyDown}
        />
        <div className="cmdp-list" onKeyDown={onKeyDown}>
          {tab === "faq" && faqCount === 0 ? (
            <div className="cmdp-empty">{tr("cmd.faqEmpty")}</div>
          ) : results.length === 0 ? (
            <div className="cmdp-empty">{tr("cmd.paletteEmpty")}</div>
          ) : tab === "faq" ? (
            results.map((e, index) => renderFaq(e, index))
          ) : (
            groups.map((g) => (
              <div className="cmdp-group" key={g.key}>
                <div className="cmdp-group-title">{tr(g.key)}</div>
                {g.items.map(({ e, index }) =>
                  e.kind === "cmd" ? (
                    <button
                      type="button"
                      key={e.cmd.id}
                      className="cmdp-item"
                      data-active={index === active}
                      onMouseEnter={() => setActive(index)}
                      onClick={() => activate(index)}
                    >
                      <span className="cmdp-label">{tr(e.cmd.labelKey)}</span>
                      {e.cmd.keys && <kbd className="cmdp-keys">{kbdLabel(e.cmd.keys)}</kbd>}
                    </button>
                  ) : null,
                )}
              </div>
            ))
          )}
        </div>
        <div className="cmdp-foot">{kbdLabel(tr("cmd.paletteHint"))}</div>
      </div>
    </div>
  );
}
