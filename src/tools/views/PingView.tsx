// Ping 主机：输入主机 + 次数，逐包实时上屏（后端每解析出一个应答包即 emit），命令结束补汇总统计。
import { useState } from "react";
import type { UnlistenFn } from "../../lib/tauri/client";
import { useI18n } from "../../lib/i18n";
import { isMacOS } from "../../lib/platform";
import { errMsg } from "../../lib/tauri/client";
import { netApi } from "../api";
import type { PingReply, PingStats } from "../types";

/** 把一个应答包渲染成一行回显文本（本地化、无系统输出乱码）。 */
function replyLine(r: PingReply): string {
  const parts = [`#${r.seq}`, r.ip || "-"];
  if (r.bytes) parts.push(`bytes=${r.bytes}`);
  parts.push(`time=${r.timeLess ? "<1" : r.timeMs}ms`, `TTL=${r.ttl}`);
  return parts.join("  ");
}

export default function PingView() {
  const t = useI18n();
  const [host, setHost] = useState("127.0.0.1");
  const [count, setCount] = useState(4);
  const [running, setRunning] = useState(false);
  const [packets, setPackets] = useState<PingReply[]>([]);
  const [stats, setStats] = useState<PingStats | null>(null);
  const [error, setError] = useState("");

  async function run() {
    setRunning(true);
    setError("");
    setStats(null);
    setPackets([]);
    let unlisten: UnlistenFn | undefined;
    try {
      // 先订阅再发起命令，确保逐包 emit 不漏接；命令 resolve 时携带汇总统计。
      unlisten = await netApi.onPingReply((r) => setPackets((prev) => [...prev, r]));
      setStats(await netApi.ping(host.trim(), count));
    } catch (e) {
      setError(errMsg(e));
    } finally {
      unlisten?.();
      setRunning(false);
    }
  }

  /** 权限拦截时一键打开 macOS「本地网络」设置面板（mac 以外后端报错，回显到 error）。 */
  async function openSettings() {
    try {
      await netApi.openLocalNetworkSettings();
    } catch (e) {
      setError(errMsg(e));
    }
  }

  return (
    <div className="tool-view">
      <div className="tool-form">
        <label className="tool-field">
          <span>{t("net.ping.host")}</span>
          <input value={host} placeholder={t("net.ping.hostPh")} spellCheck={false} disabled={running}
            onChange={(e) => setHost(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && !running && void run()} />
        </label>
        <label className="tool-field tool-field-sm">
          <span>{t("net.ping.count")}</span>
          <input type="number" min={1} max={10} value={count} disabled={running}
            onChange={(e) => setCount(Number(e.target.value) || 1)} />
        </label>
      </div>

      <div className="tool-bar">
        <button type="button" onClick={() => void run()} disabled={running || !host.trim()}>
          {running ? t("net.ping.running") : t("net.ping.run")}
        </button>
      </div>

      {error && <div className="form-error">{error}</div>}
      {stats && (
        <>
          <div className={`tool-result-head ${stats.ok ? "ok" : "bad"}`}>
            {t(stats.ok ? "net.ping.reach" : "net.ping.unreach")}
          </div>
          {/* 无应答：归类命中给可操作提示；未命中但有原始输出则回退展示真实报错（避免只剩「不可达」）。 */}
          {!stats.ok && (stats.reason === "permission" || stats.reason === "unknown_host") && (
            <div className="form-error">{t(`net.ping.reason.${stats.reason}`)}</div>
          )}
          {/* 命中 macOS 本地网络权限拦截：给一键跳转系统设置的按钮（dev 不弹窗时尤其有用）。仅 mac 有该面板。 */}
          {!stats.ok && stats.reason === "permission" && isMacOS() && (
            <div className="tool-bar">
              <button type="button" onClick={() => void openSettings()}>{t("net.ping.openSettings")}</button>
            </div>
          )}
          {!stats.ok && stats.reason !== "permission" && stats.reason !== "unknown_host" && stats.detail && (
            <div className="form-error">{t("net.ping.detail").replace("{detail}", stats.detail)}</div>
          )}
          <div className="tool-count">
            {t("net.ping.stat")
              .replace("{sent}", String(stats.sent))
              .replace("{recv}", String(stats.received))
              .replace("{loss}", String(stats.lossPct))
              .replace("{min}", stats.minMs.toFixed(2))
              .replace("{avg}", stats.avgMs.toFixed(2))
              .replace("{max}", stats.maxMs.toFixed(2))}
          </div>
        </>
      )}
      {(packets.length > 0 || running) && (
        <pre className="tool-log">
          {packets.map(replyLine).join("\n")}
          {running && (packets.length > 0 ? "\n" : "") + (packets.length < count ? t("net.ping.wait") : "")}
        </pre>
      )}
    </div>
  );
}
