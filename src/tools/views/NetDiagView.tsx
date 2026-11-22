// 网络诊断：一窗三「子页签」——DNS 解析 / Traceroute / 公网 IP。
// DNS 与公网 IP 为「请求-响应」；Traceroute 逐跳经 tools-trace-hop 事件流式上屏（同 ping 套路）。
import { useEffect, useRef, useState } from "react";
import type { UnlistenFn } from "../../lib/tauri/client";
import { useI18n } from "../../lib/i18n";
import { errMsg } from "../../lib/tauri/client";
import { netApi } from "../api";
import type { DnsRecord, PublicInfo, TraceHop } from "../types";

type TabId = "dns" | "trace" | "ip";

const TABS: { id: TabId; label: string }[] = [
  { id: "dns", label: "nd.tab.dns" },
  { id: "trace", label: "nd.tab.trace" },
  { id: "ip", label: "nd.tab.ip" },
];

export default function NetDiagView() {
  const t = useI18n();
  const [tab, setTab] = useState<TabId>("dns");
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
      {tab === "dns" && <DnsTab />}
      {tab === "trace" && <TraceTab />}
      {tab === "ip" && <IpTab />}
    </div>
  );
}

// ————————————————————————— DNS —————————————————————————

function DnsTab() {
  const t = useI18n();
  const [domain, setDomain] = useState("");
  const [running, setRunning] = useState(false);
  const [rows, setRows] = useState<DnsRecord[]>([]);
  const [error, setError] = useState("");
  const [done, setDone] = useState(false);

  async function run() {
    const d = domain.trim();
    if (!d) return;
    setRunning(true);
    setError("");
    setRows([]);
    setDone(false);
    try {
      setRows(await netApi.dnsLookup(d));
      setDone(true);
    } catch (e) {
      setError(errMsg(e));
      setDone(true);
    } finally {
      setRunning(false);
    }
  }

  return (
    <>
      <div className="tool-form">
        <label className="tool-field">
          <span>{t("nd.dns.domain")}</span>
          <input
            value={domain}
            placeholder={t("nd.dns.domainPh")}
            spellCheck={false}
            disabled={running}
            onChange={(e) => setDomain(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && !running && void run()}
          />
        </label>
      </div>
      <div className="tool-bar">
        <button type="button" onClick={() => void run()} disabled={running || !domain.trim()}>
          {running ? t("nd.running") : t("nd.dns.run")}
        </button>
      </div>
      {error && <div className="form-error">{error}</div>}
      {done && !error && rows.length === 0 && <div className="tool-result-head">{t("nd.dns.empty")}</div>}
      {rows.length > 0 && (
        <table className="tool-table">
          <thead>
            <tr>
              <th>{t("nd.dns.col.kind")}</th>
              <th>{t("nd.dns.col.addr")}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r, i) => (
              <tr key={`${r.kind}-${r.address}-${i}`}>
                <td className="mono">{r.kind}</td>
                <td className="mono">{r.address}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}

// ————————————————————————— Traceroute —————————————————————————

function TraceTab() {
  const t = useI18n();
  const [host, setHost] = useState("");
  const [maxHops, setMaxHops] = useState(20);
  const [running, setRunning] = useState(false);
  const [hops, setHops] = useState<TraceHop[]>([]);
  const [error, setError] = useState("");
  const unlisten = useRef<UnlistenFn | null>(null);

  // 卸载退订逐跳事件，避免残留监听。
  useEffect(() => () => unlisten.current?.(), []);

  async function run() {
    const h = host.trim();
    if (!h) return;
    setRunning(true);
    setError("");
    setHops([]);
    try {
      // 先订阅再发起，确保逐跳 emit 不漏接。
      unlisten.current = await netApi.onTraceHop((hop) => setHops((prev) => [...prev, hop]));
      const all = await netApi.traceroute(h, maxHops);
      setHops(all); // 以命令返回为准，兜底事件可能的乱序/漏包
    } catch (e) {
      setError(errMsg(e));
    } finally {
      unlisten.current?.();
      unlisten.current = null;
      setRunning(false);
    }
  }

  return (
    <>
      <div className="tool-form">
        <label className="tool-field">
          <span>{t("nd.trace.host")}</span>
          <input
            value={host}
            placeholder={t("nd.trace.hostPh")}
            spellCheck={false}
            disabled={running}
            onChange={(e) => setHost(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && !running && void run()}
          />
        </label>
        <label className="tool-field tool-field-sm">
          <span>{t("nd.trace.maxHops")}</span>
          <input
            type="number"
            min={1}
            max={30}
            value={maxHops}
            disabled={running}
            onChange={(e) => setMaxHops(Number(e.target.value) || 1)}
          />
        </label>
      </div>
      <div className="tool-bar">
        <button type="button" onClick={() => void run()} disabled={running || !host.trim()}>
          {running ? t("nd.running") : t("nd.trace.run")}
        </button>
        {running && <span className="tool-count">{t("nd.trace.wait")}</span>}
      </div>
      {error && <div className="form-error">{error}</div>}
      {(hops.length > 0 || running) && (
        <table className="tool-table">
          <thead>
            <tr>
              <th className="num">{t("nd.trace.col.hop")}</th>
              <th>{t("nd.trace.col.addr")}</th>
              <th className="num">{t("nd.trace.col.rtt")}</th>
            </tr>
          </thead>
          <tbody>
            {hops.map((hp) => (
              <tr key={hp.hop}>
                <td className="num mono">{hp.hop}</td>
                <td className="mono">
                  {hp.ip || <span className="nd-timeout">{t("nd.trace.timeout")}</span>}
                  {hp.host ? <span className="nd-host"> {hp.host}</span> : null}
                </td>
                <td className="num mono">
                  {hp.timesMs.length > 0 ? `${hp.timesMs.map((v) => v.toFixed(1)).join(" / ")} ms` : "-"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}

// ————————————————————————— 公网 IP —————————————————————————

function IpTab() {
  const t = useI18n();
  const [running, setRunning] = useState(false);
  const [info, setInfo] = useState<PublicInfo | null>(null);
  const [error, setError] = useState("");

  async function run() {
    setRunning(true);
    setError("");
    setInfo(null);
    try {
      setInfo(await netApi.publicIp());
    } catch (e) {
      setError(errMsg(e));
    } finally {
      setRunning(false);
    }
  }

  // 首次进入自动查询一次（公网 IP 无入参，打开即想看）。
  useEffect(() => {
    void run();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const rows: [string, string][] = info
    ? [
        [t("nd.ip.ip"), info.ip],
        [t("nd.ip.country"), info.country],
        [t("nd.ip.city"), info.city],
        [t("nd.ip.org"), info.org],
      ]
    : [];

  return (
    <>
      <div className="tool-bar">
        <button type="button" onClick={() => void run()} disabled={running}>
          {running ? t("nd.running") : t("nd.ip.run")}
        </button>
      </div>
      {error && <div className="form-error">{error}</div>}
      {rows.length > 0 && (
        <table className="tool-table">
          <tbody>
            {rows.map(([k, v]) => (
              <tr key={k}>
                <td className="nd-ip-key">{k}</td>
                <td className="mono">{v || "-"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}
