// 开放端口列表：进页即拉取，手动刷新；纯只读，不落库。
import { useCallback, useEffect, useState } from "react";
import { useI18n } from "../../lib/i18n";
import { errMsg } from "../../lib/tauri/client";
import { netApi } from "../api";
import { IconRefresh } from "../../components/common/icons";
import type { ListeningPort } from "../types";

export default function PortListView() {
  const t = useI18n();
  const [rows, setRows] = useState<ListeningPort[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      setRows(await netApi.listeningPorts());
    } catch (e) {
      setError(errMsg(e));
      setRows([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="tool-view">
      <div className="tool-bar">
        <button
          type="button"
          className="icon-btn"
          title={t("net.ports.refresh")}
          aria-label={t("net.ports.refresh")}
          onClick={() => void load()}
          disabled={loading}
        >
          <IconRefresh />
        </button>
        {!loading && !error && (
          <span className="tool-count">{t("net.ports.count").replace("{n}", String(rows.length))}</span>
        )}
      </div>
      {error && <div className="form-error">{error}</div>}
      {!error && rows.length === 0 && !loading && <div className="tool-empty">{t("net.ports.empty")}</div>}
      {rows.length > 0 && (
        <table className="tool-table">
          <thead>
            <tr>
              <th>{t("net.ports.col.proto")}</th>
              <th>{t("net.ports.col.addr")}</th>
              <th className="num">{t("net.ports.col.port")}</th>
              <th className="num">{t("net.ports.col.pid")}</th>
              <th>{t("net.ports.col.proc")}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r, i) => (
              <tr key={`${r.proto}-${r.localAddr}-${r.localPort}-${r.pid}-${i}`}>
                <td><span className={`proto-tag ${r.proto}`}>{r.proto}</span></td>
                <td className="mono">{r.localAddr}</td>
                <td className="num mono">{r.localPort}</td>
                <td className="num mono">{r.pid || "—"}</td>
                <td>{r.process || "—"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
