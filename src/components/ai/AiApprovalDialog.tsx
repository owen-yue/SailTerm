import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { IconEdit } from "../common/icons";
import { aiChat, aiStore, sessionStore, useStore } from "../../stores";
import { useI18n, tReason } from "../../lib/i18n";
import { kbdLabel } from "../../lib/kbd";
import { useEscClose } from "../../lib/useOverlay";
import { sessionLabel } from "../../types";
import { RISK } from "./aiLabels";

/**
 * AI 命令执行前的审批弹窗。
 *
 * 编排器在真正执行「写/危险」类工具前中断并 emit 一个审批请求，这里逐个呈现，必须由用户
 * 显式裁决——沿用主机密钥 TOFU 那套「不给遮罩关闭、超时后端按拒绝」的交互纪律。卡片里的
 * summary 是后端**脱敏后**的文本，多个请求排队处理。
 *
 * 五要素要在这张卡上齐全：完整命令、风险等级、判定理由、**AI 意图（purpose）**、
 * **在哪台机器执行（会话名）**。此前弹窗只给了命令与理由——同一条 `systemctl restart nginx`
 * 在生产机与测试机上该不该批，正是靠这两条信息判断的。
 */
export default function AiApprovalDialog() {
  const t = useI18n();
  // 详情弹窗：仅当就地卡片点了「ⓘ详情」才弹；不再对每个审批自动弹出，
  // 日常裁决走终端就地 ✓/✕。关掉详情不等于裁决——后端超时按拒绝（fail-closed）。
  const { approvals, detailApprovalId } = useStore(aiStore);
  const { tabs } = useStore(sessionStore);
  const prompt = approvals.find((a) => a.requestId === detailApprovalId);

  // 编辑改判（仅 run_command）：切到编辑态，用改后命令重新提交审批
  const [editing, setEditing] = useState(false);
  const [edited, setEdited] = useState("");
  // 切换审批对象（或关闭）时复位编辑态，避免把上一条的草稿带过来
  const reqId = prompt?.requestId ?? null;
  useEffect(() => {
    setEditing(false);
    setEdited("");
  }, [reqId]);

  const cardRef = useRef<HTMLDivElement>(null);
  // 焦点落进弹窗本体：键盘用户点「ⓘ详情」后不该还得先 Tab 一圈才够到按钮
  useLayoutEffect(() => {
    if (reqId) cardRef.current?.focus();
  }, [reqId]);

  const decide = (accept: boolean) => {
    if (!prompt) return;
    void aiChat.respondApproval(prompt.requestId, accept);
  };
  // Esc = 明确拒绝（比「关掉等超时」更确定，且方向安全）
  useEscClose(() => decide(false), reqId !== null);
  // 「+N」不再只是计数：点开即翻到队列里的下一条，逐条审完不必回终端再点 ⓘ
  const gotoNext = () => {
    const next = approvals.find((a) => a.requestId !== reqId);
    if (next) aiChat.showDetail(next.requestId);
  };

  if (!prompt) return null;

  const isCommand = prompt.tool === "run_command";
  const decideEdited = () => void aiChat.respondApproval(prompt.requestId, true, edited.trim());
  const risk = RISK[prompt.risk] ?? RISK.UNKNOWN;
  const dangerous = prompt.risk === "DANGEROUS";
  const summary = prompt.summary;
  const tab = tabs.find((x) => x.id === prompt.sessionId);
  const host = tab ? sessionLabel(tab, t) : "";

  /** 进入编辑态：用当前（脱敏后）命令预填草稿。 */
  function startEdit() {
    setEdited(summary);
    setEditing(true);
  }

  return (
    <div className="modal-mask">
      <div
        className="modal ai-appr"
        role="alertdialog"
        aria-modal="true"
        aria-label={t("ad.title")}
        tabIndex={-1}
        ref={cardRef}
        onKeyDown={(e) => {
          // 只在焦点落在弹窗本体时接管，否则会把「Tab 到某个按钮后按回车」变成两次动作
          if (e.target !== e.currentTarget) return;
          if (e.key === "Enter") {
            // 高危命令不给回车批准：必须用鼠标点「仍然执行」，杜绝盲批
            if (dangerous || editing) return;
            e.preventDefault();
            decide(true);
          }
        }}
      >
        <div className="modal-head">
          <h2>{t("ad.title")}</h2>
          {host && (
            <span className="appr-host" title={t("ab.hostTip")}>
              {host}
            </span>
          )}
          {approvals.length > 1 && (
            <button
              type="button"
              className="hk-more"
              title={t("ad.moreTip")}
              onClick={gotoNext}
            >
              +{approvals.length - 1}
            </button>
          )}
          <button
            type="button"
            className="modal-x"
            title={t("ad.backTip")}
            onClick={() => aiChat.hideDetail()}
          >
            ×
          </button>
        </div>

        <div className="hk-body">
          <div className="appr-top">
            <span className={`risk-badge ${risk.cls}`}>{t(risk.label)}</span>
            <span className="appr-tool">{prompt.tool}</span>
          </div>
          {prompt.purpose && (
            <p className="appr-why">
              <span className="appr-why-k">{t("ad.intent")}</span>
              {prompt.purpose}
            </p>
          )}
          {editing ? (
            <textarea
              className="appr-edit"
              value={edited}
              autoFocus
              spellCheck={false}
              onChange={(e) => setEdited(e.target.value)}
            />
          ) : (
            <pre className="appr-cmd">{prompt.summary}</pre>
          )}
          {prompt.reason && (
            <p className="appr-reason">{tReason(prompt.reason.key, prompt.reason.args, prompt.reason.zh)}</p>
          )}
          <p className="hk-warn">
            {editing ? t("ad.regrade") : `${dangerous ? t("ad.dangerPrefix") : ""}${t("ad.runAsSession")}`}
          </p>
          <p className="appr-kbd">
            {kbdLabel(dangerous ? t("ad.kbdDanger") : t("ad.kbdHint"))}
          </p>
        </div>

        <div className="form-actions">
          {editing ? (
            <>
              <button type="button" className="ghost" onClick={() => setEditing(false)}>
                {t("ad.cancelEdit")}
              </button>
              <button type="button" disabled={!edited.trim()} onClick={decideEdited}>
                {t("ad.approveEdited")}
              </button>
            </>
          ) : (
            <>
              <button type="button" className="ghost" onClick={() => decide(false)}>
                {t("ad.deny")}
              </button>
              {isCommand && (
                <button type="button" className="ghost" onClick={startEdit} title={t("ad.editTip")}>
                  <IconEdit /> {t("ad.edit")}
                </button>
              )}
              <button type="button" className={dangerous ? "danger" : ""} onClick={() => decide(true)}>
                {dangerous ? t("ad.runAnyway") : t("ad.approveRun")}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
