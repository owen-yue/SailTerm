import { useEffect, useState } from "react";
import { errMsg } from "../../lib/tauri";
import { useI18n } from "../../lib/i18n";
import { lockActions, lockStore, useStore } from "../../stores";
import { toast } from "../../stores/toastStore";
import FormDialog from "../dialog/FormDialog";

/**
 * 「设置 PIN」弹窗：从锁屏入口（顶栏按钮 / Ctrl+Alt+L / 命令面板）直接拉起——
 * 未设 PIN 时不再把用户支去设置页，就地创建、设完顺势锁屏（即本功能的完整动线）。
 *
 * 挂载在 main.tsx 三根旁（与 LockOverlay 同款理由：弹出会话窗/工具窗也要能弹）；
 * `locked` 时不渲染——正锁着就没有建 PIN 的入口，解锁走遮罩。
 * 安全红线与设置页同款：PIN 明文只作 setPin 入参瞬时直送后端，绝不落 store/日志。
 */
export default function PinSetupDialog() {
  const { setupOpen, locked } = useStore(lockStore);
  const tr = useI18n();
  const [pin, setPin] = useState("");
  const [pin2, setPin2] = useState("");
  const [question, setQuestion] = useState("");
  const [answer, setAnswer] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  // 本组件常驻挂载（关窗只是 setupOpen=false），局部态不随卸载重置：每次重新打开都清
  // 一遍，否则上一次成功提交遗留的 busy=true 会让「创建并锁屏」按钮一直灰着点不动。
  useEffect(() => {
    if (setupOpen) {
      setPin("");
      setPin2("");
      setQuestion("");
      setAnswer("");
      setErr("");
      setBusy(false);
    }
  }, [setupOpen]);

  if (!setupOpen || locked) return null;

  const close = () => {
    setPin("");
    setPin2("");
    setQuestion("");
    setAnswer("");
    setErr("");
    setBusy(false);
    lockStore.set({ setupOpen: false });
  };

  /** 只收数字、限 6 位（后端 validate_pin 另有 fail-safe 兜底）。 */
  const digits = (v: string) => v.replace(/\D/g, "").slice(0, 6);

  async function submit() {
    if (!/^\d{4,6}$/.test(pin)) {
      setErr(tr("lock.pinErrLen"));
      return;
    }
    if (pin !== pin2) {
      setErr(tr("lock.pinErrMatch"));
      return;
    }
    if (!question.trim()) {
      setErr(tr("lock.secqErrQuestion"));
      return;
    }
    if (!answer.trim()) {
      setErr(tr("lock.secqErrAnswer"));
      return;
    }
    setBusy(true);
    setErr("");
    try {
      await lockActions.setPin(pin, null, question.trim(), answer);
      close();
      // 用户是从「锁屏」动作进来的：建成即锁，别让人再点一次。
      void lockActions.lockNow().catch((e) => toast.error(errMsg(e)));
    } catch (e) {
      setErr(errMsg(e));
      setBusy(false);
    }
  }

  return (
    <FormDialog
      title={tr("lock.setPin")}
      onClose={close}
      onSubmit={() => void submit()}
      submitting={busy}
      okText={tr("lock.setupLock")}
    >
      <p className="form-hint">{tr("lock.setupDesc")}</p>
      <label>
        {tr("lock.pinLabel")}
        <input
          type="password"
          inputMode="numeric"
          autoComplete="off"
          maxLength={6}
          value={pin}
          onChange={(e) => setPin(digits(e.target.value))}
          autoFocus
        />
      </label>
      <label>
        {tr("lock.confirmLabel")}
        <input
          type="password"
          inputMode="numeric"
          autoComplete="off"
          maxLength={6}
          value={pin2}
          onChange={(e) => setPin2(digits(e.target.value))}
        />
      </label>
      <p className="form-hint">{tr("lock.secqDesc")}</p>
      <label>
        {tr("lock.secqQuestionLabel")}
        <input
          type="text"
          autoComplete="off"
          maxLength={100}
          value={question}
          onChange={(e) => setQuestion(e.target.value)}
          placeholder={tr("lock.secqQuestionPh")}
        />
      </label>
      <label>
        {tr("lock.secqAnswerLabel")}
        <input
          type="text"
          autoComplete="off"
          maxLength={100}
          value={answer}
          onChange={(e) => setAnswer(e.target.value)}
          placeholder={tr("lock.secqAnswerPh")}
        />
      </label>
      {err && <div className="form-error">{err}</div>}
    </FormDialog>
  );
}
