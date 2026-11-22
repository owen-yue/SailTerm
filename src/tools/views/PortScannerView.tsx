// 端口扫描：目标 IPv4 + 端口范围 + 超时；进度走 tools-scan-progress 事件，可中途停止。
import { useEffect, useRef, useState } from "react";
import { useI18n } from "../../lib/i18n";
import { errCode, errMsg } from "../../lib/tauri/client";
import { netApi } from "../api";
import type { OpenPort, ScanProgress } from "../types";

export default function PortScannerView() {
  const t = useI18n();
  const [target, setTarget] = useState("127.0.0.1");
  const [range, setRange] = useState("1-1024");
  const [timeout, setTimeoutMs] = useState(1000);
  const [running, setRunning] = useState(false);
  const [prog, setProg] = useState<ScanProgress | null>(null);
  const [found, setFound] = useState<OpenPort[]>([]);
  const [error, setError] = useState("");
  const [finished, setFinished] = useState(false);
  const unlisten = useRef<(() => void) | null>(null);

  // 订阅进度事件（挂载一次；卸载退订）
  useEffect(() => {
    let alive = true;
    void netApi.onScanProgress((p) => alive && setProg(p)).then((off) => {
      if (!alive) off();
      else unlisten.current = off;
    });
    return () => {
      alive = false;
      unlisten.current?.();
    };
  }, []);

  async function start() {
    setRunning(true);
    setError("");
    setFound([]);
    setFinished(false);
    setProg({ scanned: 0, total: 0, open: 0 });
    try {
      const ports = await netApi.portScan(target.trim(), range.trim(), timeout);
      setFound(ports);
      setFinished(true);
    } catch (e) {
      // 用户主动停止：不当错误，保留已发现结果
      if (errCode(e) !== "E_CANCELED") setError(errMsg(e));
      setFinished(true);
    } finally {
      setRunning(false);
    }
  }

  function stop() {
    void netApi.scanCancel();
  }

  const pct = prog && prog.total > 0 ? Math.round((prog.scanned / prog.total) * 100) : 0;

  return (
    <div className="tool-view">
      <div className="tool-form">
        <label className="tool-field">
          <span>{t("net.scan.target")}</span>
          <input value={target} placeholder={t("net.scan.targetPh")} spellCheck={false} disabled={running}
            onChange={(e) => setTarget(e.target.value)} />
        </label>
        <label className="tool-field">
          <span>{t("net.scan.range")}</span>
          <input value={range} placeholder={t("net.scan.rangePh")} spellCheck={false} disabled={running}
            onChange={(e) => setRange(e.target.value)} />
        </label>
        <label className="tool-field tool-field-sm">
          <span>{t("net.scan.timeout")}</span>
          <input type="number" min={50} max={3000} step={50} value={timeout} disabled={running}
            onChange={(e) => setTimeoutMs(Number(e.target.value) || 0)} />
        </label>
      </div>

      <div className="tool-bar">
        {!running ? (
          <button type="button" onClick={() => void start()}>{t("net.scan.start")}</button>
        ) : (
          <button type="button" className="danger" onClick={stop}>{t("net.scan.stop")}</button>
        )}
        {running && prog && (
          <span className="tool-count">
            {t("net.scan.scanning").replace("{done}", String(prog.scanned)).replace("{total}", String(prog.total))}
          </span>
        )}
      </div>

      {running && (
        <div className="tool-progress"><div className="tool-progress-fill" style={{ width: `${pct}%` }} /></div>
      )}

      <div className="tool-hint">{t("net.scan.hint")}</div>
      {error && <div className="form-error">{error}</div>}
      {finished && !error && (
        <div className="tool-result-head">
          {found.length > 0
            ? t("net.scan.open").replace("{n}", String(found.length))
            : t("net.scan.none")}
        </div>
      )}

      {found.length > 0 && (
        <table className="tool-table">
          <thead>
            <tr>
              <th className="num">{t("net.scan.col.port")}</th>
              <th>{t("net.scan.col.state")}</th>
              <th className="num">{t("net.scan.col.latency")}</th>
            </tr>
          </thead>
          <tbody>
            {found.map((p) => (
              <tr key={p.port}>
                <td className="num mono">{p.port}</td>
                <td><span className="state-open">{t("net.scan.stateOpen")}</span></td>
                <td className="num mono">{p.latencyMs}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
