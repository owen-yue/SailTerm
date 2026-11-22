import { useEffect, useState } from "react";
import { IconEdit } from "../common/icons";
import { formatStamp } from "../../lib/format";
import { useI18n } from "../../lib/i18n";
import ConfirmDialog from "../dialog/ConfirmDialog";
import {
  AUDIT_RANGES,
  auditActions,
  auditSessions,
  auditStore,
  sessionStore,
  useStore,
  visibleAudit,
} from "../../stores";
import { AUDIT_DECISION, RISK } from "../ai/aiLabels";
import { sessionLabel } from "../../types";
import { IconRefresh, IconTrash } from "../common/icons";
import Select from "../common/Select";

/**
 * 审计页签：默认**跟随当前会话**，避免混看历史；表内只有脱敏命令与输出摘要。
 */
export default function AuditPanel() {
  const tr = useI18n();
  const { items, loading, sessionId, range } = useStore(auditStore);
  const { tabs, activeId } = useStore(sessionStore);
  /** 清空确认：审计日志不可恢复，居中模态强提示后再执行。 */
  const [confirmClear, setConfirmClear] = useState(false);
  const sessions = auditSessions(items);
  const rows = visibleAudit(items, sessionId);
  const labelOf = (id: string) => {
    const tab = tabs.find((t) => t.id === id);
    return tab ? sessionLabel(tab, tr) : id.slice(0, 8);
  };

  // 打开时拉一次；切到本会话过滤
  useEffect(() => {
    void auditActions.refresh();
  }, []);
  useEffect(() => {
    if (activeId) auditActions.setSession(activeId);
  }, [activeId]);

  return (
    <>
    <div className="ap">
      <div className="ap-head">
        <span className="fp-title">{tr("panel.audit")}</span>
        <label className="ap-filter">
          {tr("aud.session")}
          <Select
            className="mini"
            value={sessionId}
            options={[
              { value: "", label: tr("aud.all") },
              ...sessions.map((s) => ({ value: s, label: labelOf(s) })),
            ]}
            onChange={(v) => auditActions.setSession(v)}
          />
        </label>
        <label className="ap-filter">
          {tr("aud.time")}
          <Select
            className="mini"
            value={range}
            title={tr("aud.rangeTip")}
            options={AUDIT_RANGES.map((r) => ({ value: r.id, label: tr(r.label) }))}
            onChange={(v) => void auditActions.setRange(v)}
          />
        </label>
        <span className="ap-sum">{tr("aud.count").replace("{n}", String(rows.length))}</span>
        <span className="fp-acts">
          <button
            className="icon-btn"
            title={tr("common.refresh")}
            disabled={loading}
            onClick={() => void auditActions.refresh()}
          >
            <IconRefresh />
          </button>
          <button
            className="icon-btn danger"
            title={sessionId ? tr("aud.clearSessionTip") : tr("aud.clearAllTip")}
            disabled={items.length === 0}
            onClick={() => setConfirmClear(true)}
          >
            <IconTrash />
          </button>
        </span>
      </div>

      <div className="ap-list">
        <div className="ap-row ap-row-head">
          <span className="ap-c-time">{tr("aud.time")}</span>
          <span className="ap-c-actor">{tr("aud.actor")}</span>
          <span className="ap-c-risk">{tr("aud.risk")}</span>
          <span className="ap-c-dec">{tr("aud.dec")}</span>
          <span className="ap-c-cmd">{tr("aud.cmd")}</span>
          <span className="ap-c-exit">{tr("aud.exit")}</span>
        </div>

        {rows.map((e) => {
          const dec = e.decision ? AUDIT_DECISION[e.decision] : null;
          const risk = e.riskLevel ? RISK[e.riskLevel] : null;
          const edited = !!e.executedCommand && e.executedCommand !== e.proposedCommand;
          const cmd = edited ? e.executedCommand : e.proposedCommand;
          return (
            <div
              key={e.id}
              className="ap-row"
              title={`${tr("aud.lblSession")}${e.sessionId ?? tr("aud.global")}\n${tr("aud.lblProposed")}${
                e.proposedCommand ?? "—"
              }\n${tr("aud.lblDigest")}${e.outputDigest ?? "—"}`}
            >
              <span className="ap-c-time">{formatStamp(e.createdAt)}</span>
              <span className="ap-c-actor">{e.actor === "ai" ? "AI" : tr("aud.user")}</span>
              <span className="ap-c-risk">
                {risk ? <span className={`risk-badge ${risk.cls}`}>{tr(risk.label)}</span> : "—"}
              </span>
              <span className="ap-c-dec">
                {dec ? <span className={`ap-dec ${dec.cls}`}>{tr(dec.label)}</span> : "—"}
              </span>
              <span className="ap-c-cmd">
                <code>{cmd ?? "—"}</code>
                {edited && <b className="ap-edited"><IconEdit />{tr("aud.edited")}</b>}
              </span>
              <span className="ap-c-exit">{e.exitCode ?? "—"}</span>
            </div>
          );
        })}

        {rows.length === 0 && (
          <div className="fp-empty">
            {loading
              ? tr("aud.loading")
              : tr("aud.empty")}
          </div>
        )}
      </div>
    </div>

    {confirmClear && (
      <ConfirmDialog
        title={tr("panel.audit")}
        message={tr("aud.clearMsg")
          .replace("{sc}", tr(sessionId ? "aud.scopeSession" : "aud.scopeAll"))
          .replace("{n}", String(rows.length))}
        warning={tr("aud.clearWarn")}
        okText={tr("aud.clearBtn")}
        onOk={() => {
          setConfirmClear(false);
          void auditActions.clear();
        }}
        onClose={() => setConfirmClear(false)}
      />
    )}
    </>
  );
}
