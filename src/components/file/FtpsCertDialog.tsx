import { ftpsCertDecisions, ftpsCertStore, useStore } from "../../stores";
import type { CertReason } from "../../types";
import { useI18n } from "../../lib/i18n";

/** 拒因稳定码 → 文案键。后端只给码，本地化在这张表里完成。 */
const REASON_LABEL: Record<CertReason, string> = {
  self_signed: "dlg.certReasonSelfSigned",
  legacy_version: "dlg.certReasonLegacy",
  expired: "dlg.certReasonExpired",
  not_yet_valid: "dlg.certReasonNotYetValid",
  untrusted: "dlg.certReasonUntrusted",
};

/**
 * FTPS 服务端证书确认（TOFU）。
 *
 * 只在证书过不了常规信任链时出现（自签 / 非 v3 / 过期）：接受即按 host:port 固定指纹，
 * 之后指纹变了直接拒。与 HostKeyDialog 同构——必须由用户显式裁决，不提供遮罩关闭，
 * 超时后端按拒绝处理。
 */
export default function FtpsCertDialog() {
  const t = useI18n();
  const { queue } = useStore(ftpsCertStore);
  const prompt = queue[0];
  if (!prompt) return null;

  const decide = (accept: boolean) => void ftpsCertDecisions.respond(prompt.requestId, accept);

  return (
    <div className="modal-mask">
      <div className="modal hk">
        <div className="modal-head">
          <h2>{t("dlg.ftpsCert")}</h2>
          {queue.length > 1 && <span className="hk-more">+{queue.length - 1}</span>}
        </div>

        <div className="hk-body">
          <p className="hk-host">
            {prompt.host}:{prompt.port}
          </p>
          {prompt.subject && <div className="hk-type">{prompt.subject}</div>}
          <div className="hk-type">{prompt.validity}</div>
          <div className="hk-fp">{prompt.fingerprint}</div>
          <div className="hk-type">
            {prompt.reasons.map((r) => t(REASON_LABEL[r] ?? r)).join(" · ")}
          </div>
          <p className="hk-warn">{t("dlg.ftpsCertWarn")}</p>
        </div>

        <div className="form-actions">
          <button type="button" className="ghost" onClick={() => decide(false)}>
            {t("dlg.reject")}
          </button>
          <button type="button" onClick={() => decide(true)}>
            {t("dlg.trust")}
          </button>
        </div>
      </div>
    </div>
  );
}
