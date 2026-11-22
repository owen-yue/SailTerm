import { useState } from "react";
import { kbdDecisions, kbdStore, useStore } from "../../stores";
import { useI18n } from "../../lib/i18n";
import type { KbdPrompt } from "../../types";

/**
 * SSH keyboard-interactive 认证作答弹窗。
 * 服务端逐轮下发提示（OTP/口令/问答），按 echo 标志渲染密码框或文本框。
 * 与主机密钥弹窗同规则：不提供遮罩关闭，超时后端按失败处理（fail-safe）。
 */
export default function KbdAuthDialog() {
  const t = useI18n();
  const { queue } = useStore(kbdStore);
  const prompt = queue[0];
  if (!prompt) return null;

  return (
    <div className="modal-mask">
      <div className="modal hk">
        <div className="modal-head">
          <h2>{t("dlg.kbdTitle")}</h2>
          {queue.length > 1 && <span className="hk-more">+{queue.length - 1}</span>}
        </div>

        {/* key 挂在本轮 id 上：换轮即重建子组件，答案 state 不可能跨轮存活——
            靠 useEffect 复位会晚一帧，上一轮口令会在新弹窗里闪现若干毫秒 */}
        <KbdForm key={prompt.requestId} prompt={prompt} />
      </div>
    </div>
  );
}

/** 单轮表单：答案只存活于组件 state，随 key 重建而复位。 */
function KbdForm({ prompt }: { prompt: KbdPrompt }) {
  const t = useI18n();
  const [answers, setAnswers] = useState<string[]>(() =>
    Array(prompt.prompts.length).fill("")
  );

  const submit = () => {
    void kbdDecisions.respond(prompt.requestId, answers);
  };
  const cancel = () => {
    void kbdDecisions.cancel(prompt.requestId);
  };

  return (
    <>
      <div className="hk-body">
        <p className="hk-host">{prompt.host}</p>
        {prompt.banner && <div className="kbd-banner">{prompt.banner}</div>}
        {prompt.instructions && <p className="kbd-instr">{prompt.instructions}</p>}
        {prompt.prompts.length === 0 ? (
          <p className="hk-warn">{t("dlg.kbdBannerOnly")}</p>
        ) : (
          prompt.prompts.map((p, i) => (
            <label key={i} className="kbd-field">
              {p.prompt}
              <input
                type={p.echo ? "text" : "password"}
                value={answers[i] ?? ""}
                onChange={(e) =>
                  setAnswers((prev) => prev.map((v, j) => (j === i ? e.target.value : v)))
                }
                autoComplete="off"
                autoFocus={i === 0}
              />
            </label>
          ))
        )}
      </div>

      <div className="form-actions">
        <button type="button" className="ghost" onClick={cancel}>
          {t("dlg.kbdCancel")}
        </button>
        <button type="button" onClick={submit}>
          {t("dlg.kbdSubmit")}
        </button>
      </div>
    </>
  );
}
