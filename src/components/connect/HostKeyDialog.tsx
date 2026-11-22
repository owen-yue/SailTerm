import { hostKeyDecisions, hostKeyStore, useStore } from "../../stores";
import { useI18n } from "../../lib/i18n";

/**
 * 首次连接的主机密钥确认（TOFU）。
 * 必须由用户显式裁决：不提供遮罩关闭，超时后端按拒绝处理。
 */
export default function HostKeyDialog() {
  const t = useI18n();
  const { queue } = useStore(hostKeyStore);
  const prompt = queue[0];
  if (!prompt) return null;

  const decide = (accept: boolean) => void hostKeyDecisions.respond(prompt.requestId, accept);

  return (
    <div className="modal-mask">
      <div className="modal hk">
        <div className="modal-head">
          <h2>{t("dlg.hostKey")}</h2>
          {queue.length > 1 && <span className="hk-more">+{queue.length - 1}</span>}
        </div>

        <div className="hk-body">
          <p className="hk-host">
            {prompt.host}:{prompt.port}
          </p>
          <div className="hk-fp">{prompt.fingerprint}</div>
          <div className="hk-type">{prompt.keyType}</div>
          <p className="hk-warn">{t("dlg.hostKeyWarn")}</p>
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
