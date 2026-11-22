// 运维计算工具箱：一窗四「子页签」的纯前端运维小工具（无后端、无出网、不绑定会话）。
// 逻辑全在 opskit.ts（CIDR/Cron/Diff），正则测试用原生 RegExp 在本文件驱动；视图只做薄渲染。
import { useMemo, useState } from "react";
import { useI18n } from "../../lib/i18n";
import {
  calcCidr,
  cronNextRuns,
  diffLines,
  parseCron,
  type DiffRow,
} from "../opskit";

type TabId = "cidr" | "cron" | "regex" | "diff";

const TABS: { id: TabId; label: string }[] = [
  { id: "cidr", label: "ok.tab.cidr" },
  { id: "cron", label: "ok.tab.cron" },
  { id: "regex", label: "ok.tab.regex" },
  { id: "diff", label: "ok.tab.diff" },
];

export default function OpsKitView() {
  const t = useI18n();
  const [tab, setTab] = useState<TabId>("cidr");
  return (
    <div className="tool-view">
      <div className="tool-tabs">
        {TABS.map((x) => (
          <button
            key={x.id}
            type="button"
            className="tool-tab"
            data-on={tab === x.id ? "1" : undefined}
            onClick={() => setTab(x.id)}
          >
            {t(x.label)}
          </button>
        ))}
      </div>
      {tab === "cidr" && <CidrTab />}
      {tab === "cron" && <CronTab />}
      {tab === "regex" && <RegexTab />}
      {tab === "diff" && <DiffTab />}
    </div>
  );
}

/** 千分位格式化。 */
const fmt = (n: number) => n.toLocaleString();

/** 「标签 + 值」键值行（CIDR 结果表用）。 */
function Kv({ k, v, mono = true }: { k: string; v: string; mono?: boolean }) {
  return (
    <tr>
      <td className="nd-ip-key">{k}</td>
      <td className={mono ? "mono" : undefined}>{v}</td>
    </tr>
  );
}

// ————————————————————————— CIDR —————————————————————————

function CidrTab() {
  const t = useI18n();
  const [input, setInput] = useState("192.168.1.10/24");
  const res = useMemo(() => {
    if (!input.trim()) return null;
    try {
      return { ok: calcCidr(input) };
    } catch {
      return { ok: null, bad: true };
    }
  }, [input]);

  const roleLabel = (r: string) => t(`ok.cidr.role.${r}`);

  return (
    <>
      <div className="tool-form">
        <label className="tool-field">
          <span>{t("ok.cidr.input")}</span>
          <input
            value={input}
            placeholder={t("ok.cidr.inputPh")}
            spellCheck={false}
            onChange={(e) => setInput(e.target.value)}
          />
        </label>
      </div>
      {res?.bad && <div className="form-error">{t("ok.cidr.err")}</div>}
      {res?.ok && (
        <table className="tool-table">
          <tbody>
            <Kv k={t("ok.cidr.network")} v={res.ok.network} />
            <Kv k={t("ok.cidr.netmask")} v={res.ok.netmask} />
            <Kv k={t("ok.cidr.wildcard")} v={res.ok.wildcard} />
            <Kv k={t("ok.cidr.broadcast")} v={res.ok.broadcast} />
            <Kv k={t("ok.cidr.hostRange")} v={`${res.ok.firstHost} – ${res.ok.lastHost}`} />
            <Kv k={t("ok.cidr.total")} v={fmt(res.ok.total)} />
            <Kv k={t("ok.cidr.usable")} v={fmt(res.ok.usable)} />
            <Kv k={t("ok.cidr.role")} v={roleLabel(res.ok.inputRole)} mono={false} />
          </tbody>
        </table>
      )}
    </>
  );
}

// ————————————————————————— Cron —————————————————————————

function CronTab() {
  const t = useI18n();
  const [expr, setExpr] = useState("*/5 9-18 * * 1-5");
  const [count, setCount] = useState(5);
  const parsed = useMemo(() => {
    if (!expr.trim()) return null;
    try {
      const cf = parseCron(expr);
      const runs = cronNextRuns(cf, new Date(), Math.min(Math.max(count, 1), 50));
      return { err: false, runs };
    } catch {
      return { err: true, runs: [] as Date[] };
    }
  }, [expr, count]);

  const fmtDate = (d: Date) =>
    d.toLocaleString(undefined, {
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      weekday: "short",
    });

  return (
    <>
      <div className="tool-form">
        <label className="tool-field">
          <span>{t("ok.cron.expr")}</span>
          <input value={expr} placeholder={t("ok.cron.exprPh")} spellCheck={false} onChange={(e) => setExpr(e.target.value)} />
        </label>
        <label className="tool-field tool-field-sm">
          <span>{t("ok.cron.count")}</span>
          <input
            type="number"
            min={1}
            max={50}
            value={count}
            onChange={(e) => setCount(Number(e.target.value) || 1)}
          />
        </label>
      </div>
      <div className="tool-hint">{t("ok.cron.help")}</div>
      {parsed?.err && <div className="form-error">{t("ok.cron.err")}</div>}
      {parsed && !parsed.err && (
        <>
          <div className="tool-result-head">{t("ok.cron.next")}</div>
          {parsed.runs.length === 0 ? (
            <div className="tool-log">{t("ok.cron.never")}</div>
          ) : (
            <pre className="tool-log">{parsed.runs.map((d, i) => `${i + 1}.  ${fmtDate(d)}`).join("\n")}</pre>
          )}
        </>
      )}
    </>
  );
}

// ————————————————————————— 正则测试 —————————————————————————

function RegexTab() {
  const t = useI18n();
  const [pattern, setPattern] = useState("");
  const [flags, setFlags] = useState("gm");
  const [text, setText] = useState("");

  const result = useMemo(() => {
    if (!pattern) return null;
    let re: RegExp;
    try {
      // matchAll 需要 g 标志；用户未加则补，其余标志原样尊重。
      const f = flags.includes("g") ? flags : flags + "g";
      re = new RegExp(pattern, f);
    } catch {
      return { err: true, nodes: [] as Node[], matches: [] as RegExpExecArray[] };
    }
    const matches: RegExpExecArray[] = [];
    let m: RegExpExecArray | null;
    re.lastIndex = 0;
    let guard = 0;
    // 手动 exec 循环（不依赖 ES2020 matchAll）：零宽匹配手动推进 lastIndex 防死循环，并设上限。
    while ((m = re.exec(text)) !== null && matches.length < 2000 && guard++ < 100000) {
      matches.push(m);
      if (m[0] === "") re.lastIndex += 1;
    }
    return { err: false, matches };
  }, [pattern, flags, text]);

  // 按匹配位置把文本切成 [普通段, 匹配段, ...]，匹配段用 <mark> 高亮。
  const nodes = useMemo(() => {
    if (!result || result.err || !result.matches.length) return null;
    const out: React.ReactNode[] = [];
    let last = 0;
    result.matches.forEach((m, idx) => {
      const start = m.index ?? 0;
      const str = m[0];
      if (start > last) out.push(text.slice(last, start));
      // 空匹配（如 a*）：避免零宽无限循环，占位标记仍推进 last。
      if (str === "") {
        out.push(<mark className="ok-mark ok-mark-empty" key={`m${idx}`}>&#8203;</mark>);
        last = start + 1;
      } else {
        out.push(<mark className="ok-mark" key={`m${idx}`}>{str}</mark>);
        last = start + str.length;
      }
    });
    if (last < text.length) out.push(text.slice(last));
    return out;
  }, [result, text]);

  return (
    <>
      <div className="tool-form">
        <label className="tool-field">
          <span>{t("ok.regex.pattern")}</span>
          <input value={pattern} placeholder={t("ok.regex.patternPh")} spellCheck={false} onChange={(e) => setPattern(e.target.value)} />
        </label>
        <label className="tool-field tool-field-sm">
          <span>{t("ok.regex.flags")}</span>
          <input value={flags} placeholder="gimsuy" spellCheck={false} onChange={(e) => setFlags(e.target.value)} />
        </label>
      </div>
      <div className="tool-area">
        <span>{t("ok.regex.text")}</span>
        <textarea value={text} placeholder={t("ok.regex.textPh")} spellCheck={false} onChange={(e) => setText(e.target.value)} />
      </div>
      {result?.err && <div className="form-error">{t("ok.regex.err")}</div>}
      {result && !result.err && result.matches.length > 0 && (
        <div className="tool-count">{t("ok.regex.count").replace("{n}", String(result.matches.length))}</div>
      )}
      {result && !result.err && pattern && result.matches.length === 0 && text && (
        <div className="tool-count">{t("ok.regex.none")}</div>
      )}
      {nodes && <div className="ok-hl">{nodes}</div>}
      {result && !result.err && result.matches.length > 0 && (
        <div className="ok-groups">
          <div className="tool-result-head">{t("ok.regex.groups")}</div>
          <table className="tool-table">
            <thead>
              <tr>
                <th className="num">#</th>
                <th className="mono">@</th>
                <th className="mono">{t("ok.regex.groups")}</th>
              </tr>
            </thead>
            <tbody>
              {result.matches.slice(0, 100).map((m, i) => (
                <tr key={i}>
                  <td className="num mono">{i + 1}</td>
                  <td className="num mono">{m.index}</td>
                  <td className="mono">
                    {m.length > 1
                      ? Array.from({ length: m.length - 1 }, (_, g) => m[g + 1] ?? "∅").join(" | ")
                      : "—"}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  );
}

// ————————————————————————— 文本 Diff —————————————————————————

function DiffTab() {
  const t = useI18n();
  const [a, setA] = useState("");
  const [b, setB] = useState("");
  const diff = useMemo(() => diffLines(a, b), [a, b]);
  const empty = !a && !b;

  return (
    <>
      <div className="tool-io">
        <div className="tool-area">
          <span>{t("ok.diff.a")}</span>
          <textarea value={a} placeholder={t("ok.diff.aPh")} spellCheck={false} onChange={(e) => setA(e.target.value)} />
        </div>
        <div className="tool-area">
          <span>{t("ok.diff.b")}</span>
          <textarea value={b} placeholder={t("ok.diff.bPh")} spellCheck={false} onChange={(e) => setB(e.target.value)} />
        </div>
      </div>
      {empty ? (
        <div className="tool-count">{t("ok.diff.empty")}</div>
      ) : (
        <>
          <div className="tool-count">
            {t("ok.diff.stat").replace("{add}", String(diff.added)).replace("{del}", String(diff.removed))}
          </div>
          <div className="ok-diff">
            {diff.rows.map((r: DiffRow, i) => (
              <div className="ok-line" key={i} data-kind={r.kind}>
                <span className="ok-sign">{r.kind === "add" ? "+" : r.kind === "del" ? "−" : " "}</span>
                <span className="ok-no">{r.oldNo ?? ""}</span>
                <span className="ok-no">{r.newNo ?? ""}</span>
                <span className="ok-text">{r.text || " "}</span>
              </div>
            ))}
          </div>
        </>
      )}
    </>
  );
}
