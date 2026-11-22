// 会话信息面板：连接建立后的协商快照（SSH：加密套件/压缩/服务器软件/主机密钥/隧道；
// FTP/FTPS：220 banner / SYST / FEAT 原文 / 本次呈现的证书指纹）。
// 数据源是后端 `session_info`（russh kex_done 捕获与 FTP 登录后的只读命令，均不含凭证）。
// 单实例跟随聚焦会话（停靠面板架构）：读 sessionStore.activeId，重连后随 state 变化刷新。
import { useCallback, useEffect, useState } from "react";
import { IconRefresh } from "../common/icons";
import { copyText } from "../../lib/clipboard";
import { useI18n } from "../../lib/i18n";
import { errMsg, sessions as sshApi } from "../../lib/tauri";
import { sessionStore, useStore } from "../../stores";
import { TUNNEL_KIND_LABEL, sessionLabel } from "../../types";
import { hasCap } from "../../lib/capabilities";
import type { FtpInfo, Negotiation, SessionInfo, TunnelKind } from "../../types";

/** 延迟标黄阈值（毫秒），与标签栏 RTT 徽章的 data-slow 口径一致。 */
const SLOW_MS = 200;

export default function SessionPanel() {
  const t = useI18n();
  const { tabs, activeId } = useStore(sessionStore);
  const tab = tabs.find((x) => x.id === activeId) ?? null;
  // 取快照的前提是「这个协议有协商信息可看」（能力表 negotiation）：
  // 而不是写死只认 SSH——FTP/FTPS 的 banner/FEAT/TLS 快照落地后同一行自动放行。
  const sessionId = tab && hasCap(tab.kind, "negotiation") ? tab.id : "";
  // 重连会走 disconnected → connected：state 入依赖即自动重取快照
  const stateKey = `${sessionId}:${tab?.state ?? ""}`;

  const [info, setInfo] = useState<SessionInfo | null>(null);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    if (!sessionId) {
      setInfo(null);
      return;
    }
    try {
      setInfo(await sshApi.info(sessionId));
      setError("");
    } catch (e) {
      setError(errMsg(e));
    }
  }, [sessionId]);

  useEffect(() => {
    void load();
  }, [load, stateKey]);

  // 快照按协议分两套字段（后端也是两个结构，没有公共字段）：谁有值就渲染谁。
  const neg = info?.negotiation ?? null;
  const ftp = info?.ftpInfo ?? null;

  return (
    <div className="sinfo">
      <div className="mon-head">
        <span className="fp-title">{t("sinfo.title")}</span>
        <span className="tp-sess">{tab ? sessionLabel(tab, t) : "—"}</span>
        <button
          type="button"
          className="icon-btn"
          title={t("sinfo.refresh")}
          aria-label={t("sinfo.refresh")}
          disabled={!sessionId}
          onClick={() => void load()}
        >
          <IconRefresh />
        </button>
      </div>
      {error && <div className="form-error">{error}</div>}
      {!tab && <div className="ss-empty">{t("sinfo.empty")}</div>}
      {tab && sessionId && neg && <Rows info={info!} neg={neg} />}
      {tab && sessionId && !neg && ftp && <FtpRows info={info!} ftp={ftp} />}
      {/* 有通路但本次没拿到快照：只可能是刚断开等待重连，说清状态而不是摆一句「仅 SSH」糊弄。 */}
      {tab && sessionId && !neg && !ftp && <div className="ss-empty">{t("sinfo.pending")}</div>}
    </div>
  );
}

/** 字段级复制反馈：本地瞬态，不走全局 Toast。 */
function useFieldCopy() {
  const [copied, setCopied] = useState("");
  const copy = async (key: string, text: string) => {
    if (!text || !(await copyText(text))) return;
    setCopied(key);
    window.setTimeout(() => setCopied((c) => (c === key ? "" : c)), 1500);
  };
  return { copied, copy };
}

/** FTP/FTPS 形态：banner / SYST / FEAT 原文 / 本次握手呈现的证书指纹。 */
function FtpRows({ info, ftp }: { info: SessionInfo; ftp: FtpInfo }) {
  const t = useI18n();
  const { copied, copy } = useFieldCopy();
  const lat = info.session.latencyMs;
  // FEAT 一字不改地套用后端原样拿回的清单：不做白名单过滤、不翻译——「这台服务器到底声明了
  // 什么」正是要看的原文（中文目录乱码的第一手证据就在这里）。
  const featLines = ftp.feat.map((f) => (f.param ? `${f.name} ${f.param}` : f.name));
  const utf8 = ftp.feat.some((f) => f.name.toUpperCase() === "UTF8");
  const fpr = ftp.certFingerprint ?? "";
  return (
    <div className="sinfo-list">
      <Row
        label={t("sinfo.server")}
        value={ftp.serverSoftware || t("sinfo.noReply")}
        note={t("sinfo.bannerNote")}
        copyKey="server"
        copyText={ftp.serverSoftware}
        copied={copied}
        onCopy={copy}
      />
      <Row
        label={t("sinfo.latency")}
        value={lat != null ? `${lat}ms` : "—"}
        valueClass={lat != null && lat > SLOW_MS ? "sinfo-slow" : undefined}
      />
      <Row label={t("sinfo.system")} value={ftp.system || t("sinfo.noReply")} />
      <Row
        label={t("sinfo.utf8")}
        value={utf8 ? t("sinfo.utf8Declared") : t("sinfo.utf8Undeclared")}
        tag={{ text: utf8 ? t("sinfo.on") : t("sinfo.off"), tone: utf8 ? "ok" : "off" }}
        note={t("sinfo.utf8Note")}
      />
      <Row
        label={t("sinfo.feat")}
        value={featLines.length ? "" : t("sinfo.noReply")}
        lines={featLines.length ? featLines : undefined}
      />
      <Row
        label={t("sinfo.cert")}
        value={fpr || t("sinfo.certNone")}
        copyKey="cert"
        copyText={fpr}
        copied={copied}
        onCopy={copy}
      />
    </div>
  );
}

function Rows({ info, neg }: { info: SessionInfo; neg: Negotiation }) {
  const t = useI18n();
  /** 刚复制过的字段键（本地瞬态反馈，不走全局 Toast）。 */
  const { copied, copy } = useFieldCopy();

  const lat = info.session.latencyMs;
  const compressionOn = neg.compressionServer !== "" && neg.compressionServer !== "none";
  const tunnels = info.tunnels;

  return (
    <div className="sinfo-list">
      <Row label={t("sinfo.server")} value={neg.serverSoftware || "—"} copyKey="server" copyText={neg.serverSoftware} copied={copied} onCopy={copy} />
      <Row
        label={t("sinfo.latency")}
        value={lat != null ? `${lat}ms` : "—"}
        valueClass={lat != null && lat > SLOW_MS ? "sinfo-slow" : undefined}
      />
      <Row label={t("sinfo.kex")} value={neg.kex || "—"} />
      <Row label={t("sinfo.cipher")} value={neg.cipher || "—"} />
      <Row label={t("sinfo.mac")} value={`${neg.macClient || "—"} / ${neg.macServer || "—"}`} />
      <Row
        label={t("sinfo.compression")}
        value={compressionOn ? neg.compressionServer : t("sinfo.vOff")}
        tag={
          compressionOn
            ? { text: t("sinfo.on"), tone: "ok" }
            : { text: t("sinfo.off"), tone: "off" }
        }
        note={compressionOn ? undefined : t("sinfo.compClient")}
      />
      <Row
        label={t("sinfo.agent")}
        value={neg.agentForward ? t("sinfo.agentRequested") : t("sinfo.vOff")}
        note={neg.agentForward ? t("sinfo.agentHint") : undefined}
      />
      <Row
        label={t("sinfo.x11")}
        value={neg.x11Forward ? t("sinfo.agentRequested") : t("sinfo.vOff")}
        note={neg.x11Forward ? t("sinfo.x11Hint") : undefined}
      />
      <Row label={t("sinfo.keepalive")} value={`${neg.keepaliveSecs}s`} />
      <Row
        label={t("sinfo.hostKey")}
        value={`${neg.hostKeyType || "—"} ${neg.hostKeyFingerprint || ""}`.trim()}
        copyKey="fpr"
        copyText={neg.hostKeyFingerprint}
        copied={copied}
        onCopy={copy}
      />
      <Row label={t("sinfo.jumps")} value={neg.jumps.length ? neg.jumps.join(" → ") : t("sinfo.direct")} />
      <Row
        label={t("sinfo.tunnels")}
        value={
          tunnels.length
            ? ""
            : t("sinfo.none")
        }
        lines={tunnels.map((a) =>
          `${t(TUNNEL_KIND_LABEL[a.kind as TunnelKind])} ${a.localHost}:${a.localPort} → ${a.remoteHost}:${a.remotePort}${a.name ? ` (${a.name})` : ""}`,
        )}
      />
    </div>
  );
}

function Row({
  label,
  value,
  valueClass,
  tag,
  note,
  lines,
  copyKey,
  copyText: text,
  copied,
  onCopy,
}: {
  label: string;
  value: string;
  valueClass?: string;
  /** ✓/✗ 状态标记：ok 绿 / off 灰。 */
  tag?: { text: string; tone: "ok" | "off" };
  /** 归因/补充说明（弱化小字）。 */
  note?: string;
  /** 多行值（隧道列表）；给了则优先于 value 渲染。 */
  lines?: string[];
  copyKey?: string;
  copyText?: string;
  copied?: string;
  onCopy?: (key: string, text: string) => void;
}) {
  const t = useI18n();
  const canCopy = Boolean(copyKey && text);
  return (
    <div className="sinfo-row">
      <span className="sinfo-k">{label}</span>
      <span className={`sinfo-v ${valueClass ?? ""}`.trim()}>
        {lines ? (
          <span className="sinfo-lines">
            {lines.map((l) => (
              <span key={l} className="sinfo-line">
                {l}
              </span>
            ))}
          </span>
        ) : (
          value
        )}
        {tag && (
          <span className="sinfo-tag" data-tone={tag.tone}>
            {tag.tone === "ok" ? "✓" : "✗"} {tag.text}
          </span>
        )}
        {note && <span className="sinfo-note">{note}</span>}
        {canCopy && (
          <button
            type="button"
            className="sinfo-copy"
            onClick={() => onCopy?.(copyKey!, text!)}
          >
            {copied === copyKey ? t("sinfo.copied") : t("sinfo.copy")}
          </button>
        )}
      </span>
    </div>
  );
}
