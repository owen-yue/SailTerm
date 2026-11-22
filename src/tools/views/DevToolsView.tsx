// 开发者工具箱：一窗多「子页签」的纯前端小工具集合（无后端、无出网）。
// 逻辑全在 devutils.ts，本文件只做输入/按钮/输出的薄渲染。命名风格与网络视图一致（.tool-* 类）。
import { useState } from "react";
import { useI18n } from "../../lib/i18n";
import { errMsg } from "../../lib/tauri/client";
import {
  b64decode,
  b64encode,
  b64urlDecode,
  hmacSha256Hex,
  md5Hex,
  randomHexToken,
  randomPassword,
  reformatJson,
  shaHex,
  toCamel,
  toConstant,
  toKebab,
  toPascal,
  toSnake,
  type PwSet,
} from "../devutils";

type TabId =
  | "base64"
  | "url"
  | "json"
  | "time"
  | "radix"
  | "uuid"
  | "password"
  | "jwt"
  | "case"
  | "hash";

const TABS: { id: TabId; label: string }[] = [
  { id: "base64", label: "Base64" },
  { id: "url", label: "URL" },
  { id: "json", label: "JSON" },
  { id: "time", label: "dv.tab.time" },
  { id: "radix", label: "dv.tab.radix" },
  { id: "uuid", label: "UUID" },
  { id: "password", label: "dv.tab.password" },
  { id: "jwt", label: "JWT" },
  { id: "case", label: "dv.tab.case" },
  { id: "hash", label: "dv.tab.hash" },
];

export default function DevToolsView() {
  const t = useI18n();
  const [tab, setTab] = useState<TabId>("base64");
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
            {x.label.startsWith("dv.") ? t(x.label) : x.label}
          </button>
        ))}
      </div>
      {tab === "base64" && <B64Tab />}
      {tab === "url" && <UrlTab />}
      {tab === "json" && <JsonTab />}
      {tab === "time" && <TimeTab />}
      {tab === "radix" && <RadixTab />}
      {tab === "uuid" && <UuidTab />}
      {tab === "password" && <PasswordTab />}
      {tab === "jwt" && <JwtTab />}
      {tab === "case" && <CaseTab />}
      {tab === "hash" && <HashTab />}
    </div>
  );
}

// —— 通用小件 ——

/** 复制按钮：点击写剪贴板，1.2s 内显示 ✓。 */
function CopyBtn({ text }: { text: string }) {
  const t = useI18n();
  const [ok, setOk] = useState(false);
  if (!text) return null;
  return (
    <button
      type="button"
      className="tool-mini-btn"
      onClick={() =>
        void navigator.clipboard.writeText(text).then(() => {
          setOk(true);
          setTimeout(() => setOk(false), 1200);
        })
      }
    >
      {ok ? "✓" : t("dv.copy")}
    </button>
  );
}

/** 单行「标签 + 值 + 复制」结果条。 */
function OutLine({ label, value }: { label: string; value: string }) {
  return (
    <div className="tool-out">
      <span style={{ color: "var(--txt-mute)" }}>{label}</span>
      <code>{value || "—"}</code>
      <CopyBtn text={value} />
    </div>
  );
}

/** 输入 / 输出双文本域（Base64、URL、JSON 共用）。 */
function TextIo({
  inp,
  setInp,
  out,
  ph,
}: {
  inp: string;
  setInp: (v: string) => void;
  out: string;
  ph: string;
}) {
  const t = useI18n();
  return (
    <div className="tool-io">
      <div className="tool-area">
        <span>{t("dv.input")}</span>
        <textarea value={inp} placeholder={ph} spellCheck={false} onChange={(e) => setInp(e.target.value)} />
      </div>
      <div className="tool-area">
        <span>
          {t("dv.output")} <CopyBtn text={out} />
        </span>
        <textarea value={out} readOnly spellCheck={false} />
      </div>
    </div>
  );
}

// —— 各页签 ——

function B64Tab() {
  const t = useI18n();
  const [inp, setInp] = useState("");
  const [out, setOut] = useState("");
  const [err, setErr] = useState("");
  const run = (fn: (s: string) => string) => {
    try {
      setOut(fn(inp));
      setErr("");
    } catch {
      setOut("");
      setErr(t("dv.b64Bad"));
    }
  };
  return (
    <>
      <TextIo inp={inp} setInp={setInp} out={out} ph={t("dv.b64Ph")} />
      <div className="tool-bar">
        <button type="button" onClick={() => run(b64encode)}>
          {t("dv.encode")}
        </button>
        <button type="button" onClick={() => run(b64decode)}>
          {t("dv.decode")}
        </button>
        <button type="button" className="ghost" onClick={() => (setInp(""), setOut(""), setErr(""))}>
          {t("dv.clear")}
        </button>
      </div>
      {err && <div className="form-error">{err}</div>}
    </>
  );
}

function UrlTab() {
  const t = useI18n();
  const [inp, setInp] = useState("");
  const [out, setOut] = useState("");
  const [err, setErr] = useState("");
  const run = (fn: (s: string) => string) => {
    try {
      setOut(fn(inp));
      setErr("");
    } catch {
      setOut("");
      setErr(t("dv.urlBad"));
    }
  };
  return (
    <>
      <TextIo inp={inp} setInp={setInp} out={out} ph={t("dv.urlPh")} />
      <div className="tool-bar">
        <button type="button" onClick={() => run(encodeURIComponent)}>
          {t("dv.encode")}
        </button>
        <button type="button" onClick={() => run(decodeURIComponent)}>
          {t("dv.decode")}
        </button>
        <button type="button" className="ghost" onClick={() => (setInp(""), setOut(""), setErr(""))}>
          {t("dv.clear")}
        </button>
      </div>
      {err && <div className="form-error">{err}</div>}
    </>
  );
}

function JsonTab() {
  const t = useI18n();
  const [inp, setInp] = useState("");
  const [out, setOut] = useState("");
  const [err, setErr] = useState("");
  const run = (sp: number) => {
    try {
      setOut(reformatJson(inp, sp));
      setErr("");
    } catch (e) {
      setOut("");
      setErr(errMsg(e));
    }
  };
  return (
    <>
      <TextIo inp={inp} setInp={setInp} out={out} ph={t("dv.jsonPh")} />
      <div className="tool-bar">
        <button type="button" onClick={() => run(2)}>
          {t("dv.format")}
        </button>
        <button type="button" onClick={() => run(0)}>
          {t("dv.minify")}
        </button>
        <button type="button" className="ghost" onClick={() => (setInp(""), setOut(""), setErr(""))}>
          {t("dv.clear")}
        </button>
      </div>
      {err && <div className="form-error">{err}</div>}
    </>
  );
}

function TimeTab() {
  const t = useI18n();
  const [ts, setTs] = useState(String(Math.floor(Date.now() / 1000)));
  const [date, setDate] = useState("");
  // 时间戳 → 日期：自动识别秒(10 位)/毫秒(13 位)。
  const n = Number(ts.trim());
  const valid = ts.trim() !== "" && Number.isFinite(n);
  const ms = valid ? (ts.trim().length <= 11 ? n * 1000 : n) : 0;
  const d = valid ? new Date(ms) : null;
  // 日期 → 时间戳：datetime-local（本地时区）。
  const dt = date ? new Date(date) : null;
  return (
    <>
      <div className="tool-field">
        <span>{t("dv.tsToDate")}</span>
        <div className="tool-inline">
          <input value={ts} spellCheck={false} onChange={(e) => setTs(e.target.value)} />
          <button type="button" onClick={() => setTs(String(Date.now()))}>
            {t("dv.now")}
          </button>
        </div>
      </div>
      {d && !Number.isNaN(d.getTime()) ? (
        <>
          <OutLine label={t("dv.local")} value={d.toLocaleString()} />
          <OutLine label={t("dv.utc")} value={d.toISOString()} />
        </>
      ) : (
        <div className="form-error">{t("dv.numBad")}</div>
      )}

      <div className="tool-field">
        <span>{t("dv.dateToTs")}</span>
        <input type="datetime-local" value={date} onChange={(e) => setDate(e.target.value)} />
      </div>
      {dt && !Number.isNaN(dt.getTime()) ? (
        <>
          <OutLine label={t("dv.seconds")} value={String(Math.floor(dt.getTime() / 1000))} />
          <OutLine label={t("dv.millis")} value={String(dt.getTime())} />
        </>
      ) : (
        <div className="tool-hint">{t("dv.pickDate")}</div>
      )}
    </>
  );
}

const BASES: { v: number; label: string }[] = [
  { v: 10, label: "10" },
  { v: 16, label: "16" },
  { v: 2, label: "2" },
  { v: 8, label: "8" },
];

function RadixTab() {
  const t = useI18n();
  const [inp, setInp] = useState("255");
  const [base, setBase] = useState(10);
  let big: bigint | null = null;
  const clean = inp.trim().replace(/^0[xob]/i, "").replace(/_/g, "");
  try {
    big = base === 10 ? BigInt(clean) : base === 16 ? BigInt("0x" + clean) : base === 2 ? BigInt("0b" + clean) : BigInt("0o" + clean);
  } catch {
    big = null;
  }
  return (
    <>
      <div className="tool-field">
        <span>{t("dv.input")}</span>
        <div className="tool-inline">
          <input value={inp} spellCheck={false} onChange={(e) => setInp(e.target.value)} />
          <label>
            {t("dv.base")}
            <select value={base} onChange={(e) => setBase(Number(e.target.value))}>
              {BASES.map((b) => (
                <option key={b.v} value={b.v}>
                  {b.label}
                </option>
              ))}
            </select>
          </label>
        </div>
      </div>
      {big !== null ? (
        <>
          <OutLine label={t("dv.dec")} value={big.toString(10)} />
          <OutLine label={t("dv.hex")} value={big.toString(16).toUpperCase()} />
          <OutLine label={t("dv.oct")} value={big.toString(8)} />
          <OutLine label={t("dv.bin")} value={big.toString(2)} />
        </>
      ) : (
        <div className="form-error">{t("dv.numBad")}</div>
      )}
    </>
  );
}

function UuidTab() {
  const t = useI18n();
  const [list, setList] = useState<string[]>([]);
  const gen = () => setList((p) => [crypto.randomUUID(), ...p].slice(0, 8));
  return (
    <>
      <div className="tool-bar">
        <button type="button" onClick={gen}>
          {t("dv.genUuid")}
        </button>
      </div>
      {list.length === 0 ? (
        <div className="tool-empty">{t("dv.uuidEmpty")}</div>
      ) : (
        list.map((u, i) => <OutLine key={u} label={`#${list.length - i}`} value={u} />)
      )}
    </>
  );
}

const PW_SETS: { k: PwSet; label: string }[] = [
  { k: "lower", label: "dv.lower" },
  { k: "upper", label: "dv.upper" },
  { k: "digit", label: "dv.digit" },
  { k: "symbol", label: "dv.symbol" },
];

function PasswordTab() {
  const t = useI18n();
  const [len, setLen] = useState(16);
  const [sets, setSets] = useState<PwSet[]>(["lower", "upper", "digit", "symbol"]);
  const [out, setOut] = useState("");
  const [token, setToken] = useState("");
  const toggle = (k: PwSet) =>
    setSets((p) => (p.includes(k) ? p.filter((x) => x !== k) : [...p, k]));
  return (
    <>
      <div className="tool-inline">
        <label>
          {t("dv.len")}
          <input
            type="number"
            min={4}
            max={128}
            value={len}
            style={{ width: 64 }}
            onChange={(e) => setLen(Math.min(128, Math.max(4, Number(e.target.value) || 4)))}
          />
        </label>
        {PW_SETS.map((s) => (
          <label key={s.k}>
            <input type="checkbox" checked={sets.includes(s.k)} onChange={() => toggle(s.k)} />
            {t(s.label)}
          </label>
        ))}
      </div>
      <div className="tool-bar">
        <button type="button" onClick={() => setOut(randomPassword(len, sets))}>
          {t("dv.genPw")}
        </button>
        <button type="button" className="ghost" onClick={() => setToken(randomHexToken(32))}>
          {t("dv.genToken")}
        </button>
      </div>
      {out && <OutLine label={t("dv.pw")} value={out} />}
      {token && <OutLine label="Token" value={token} />}
    </>
  );
}

function JwtTab() {
  const t = useI18n();
  const [inp, setInp] = useState("");
  const parts = inp.trim().split(".");
  let header = "";
  let payload = "";
  let err = "";
  let exp = "";
  let iat = "";
  if (inp.trim()) {
    try {
      if (parts.length < 2) throw new Error("bad");
      const h = JSON.parse(b64urlDecode(parts[0]));
      const p = JSON.parse(b64urlDecode(parts[1]));
      header = JSON.stringify(h, null, 2);
      payload = JSON.stringify(p, null, 2);
      if (typeof p.exp === "number") exp = new Date(p.exp * 1000).toLocaleString();
      if (typeof p.iat === "number") iat = new Date(p.iat * 1000).toLocaleString();
    } catch {
      err = t("dv.jwtBad");
    }
  }
  return (
    <>
      <div className="tool-area">
        <span>{t("dv.jwtPh")}</span>
        <textarea value={inp} placeholder="eyJhbGciOi..." spellCheck={false} onChange={(e) => setInp(e.target.value)} />
      </div>
      {err && <div className="form-error">{err}</div>}
      {header && (
        <>
          <div className="tool-result-head">{t("dv.jwtHeader")}</div>
          <pre className="tool-log">{header}</pre>
          <div className="tool-result-head">{t("dv.jwtPayload")}</div>
          <pre className="tool-log">{payload}</pre>
          {iat && <OutLine label={t("dv.iat")} value={iat} />}
          {exp && <OutLine label={t("dv.exp")} value={exp} />}
          {!exp && <div className="tool-hint">{t("dv.noExp")}</div>}
        </>
      )}
    </>
  );
}

function CaseTab() {
  const t = useI18n();
  const [inp, setInp] = useState("user login count");
  if (!inp.trim()) return <div className="tool-empty">{t("dv.casePh")}</div>;
  return (
    <>
      <div className="tool-field">
        <span>{t("dv.input")}</span>
        <input value={inp} spellCheck={false} onChange={(e) => setInp(e.target.value)} />
      </div>
      <OutLine label="camelCase" value={toCamel(inp)} />
      <OutLine label="PascalCase" value={toPascal(inp)} />
      <OutLine label="snake_case" value={toSnake(inp)} />
      <OutLine label="kebab-case" value={toKebab(inp)} />
      <OutLine label="CONSTANT_CASE" value={toConstant(inp)} />
    </>
  );
}

function HashTab() {
  const t = useI18n();
  const [text, setText] = useState("");
  const [key, setKey] = useState("");
  const [res, setRes] = useState<{ label: string; value: string }[]>([]);
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      const rows = [
        { label: "MD5", value: md5Hex(text) },
        { label: "SHA-1", value: await shaHex("SHA-1", text) },
        { label: "SHA-256", value: await shaHex("SHA-256", text) },
        { label: "SHA-512", value: await shaHex("SHA-512", text) },
      ];
      if (key) rows.push({ label: "HMAC-SHA256", value: await hmacSha256Hex(key, text) });
      setRes(rows);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <div className="tool-area">
        <span>{t("dv.text")}</span>
        <textarea value={text} spellCheck={false} onChange={(e) => setText(e.target.value)} />
      </div>
      <div className="tool-field">
        <span>{t("dv.key")}</span>
        <input value={key} placeholder={t("dv.keyPh")} spellCheck={false} onChange={(e) => setKey(e.target.value)} />
      </div>
      <div className="tool-bar">
        <button type="button" disabled={busy} onClick={() => void run()}>
          {busy ? t("dv.running") : t("dv.calc")}
        </button>
      </div>
      {res.map((r) => (
        <OutLine key={r.label} label={r.label} value={r.value} />
      ))}
    </>
  );
}
