import { useEffect, useRef, useState } from "react";
import { useI18n } from "../../lib/i18n";
import { errMsg } from "../../lib/tauri/client";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";
import PasswordField from "../common/PasswordField";

interface Props {
  title: string;
  /** 输入框上方的字段名。 */
  label?: string;
  initial?: string;
  /** 输入框下方的说明文案（如权限位格式提示）。 */
  hint?: string;
  /** 密码框：掩码显示、不 trim、关自动填充（用于备份主密码）。 */
  secret?: boolean;
  /** 需二次确认（如设置主密码）：出现第二个框，两次一致才放行。 */
  confirm?: boolean;
  /** 确认框的字段名（confirm=true 时生效）。 */
  confirmLabel?: string;
  /** 校验：返回错误文案，空串表示通过。缺省只校验「非空」。 */
  validate?: (value: string) => string;
  okText?: string;
  onSubmit: (value: string) => void | Promise<void>;
  onClose: () => void;
}

/** 单行输入弹窗：新建目录 / 重命名 / 改权限 / 备份主密码共用一套外壳。 */
export default function PromptDialog({
  title,
  label,
  initial = "",
  hint,
  secret = false,
  confirm = false,
  confirmLabel,
  validate,
  okText,
  onSubmit,
  onClose,
}: Props) {
  const t = useI18n();
  // 默认确认钮走全局口径「确定/OK」（早前用 common.ok 译成「好」，与其他弹窗不统一）
  const ok = okText ?? t("common.confirm");
  const [value, setValue] = useState(initial);
  const [value2, setValue2] = useState("");
  const [err, setErr] = useState("");
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    input.current?.focus();
    // 重命名场景整名选中，用户直接敲新名字即可（密码框不选中）
    if (!secret) input.current?.select();
  }, []);
  // Esc 走弹层栈统一仲裁（窗内 .connect-form 的 onKeyDown 仍保留：焦点在输入框时先接 Enter 提交）。
  useEscClose(onClose);

  const submit = async () => {
    // 密码框不 trim（首尾空格可能是主密码的一部分）；普通框 trim。
    const v = secret ? value : value.trim();
    const bad = validate ? validate(v) : v ? "" : t("dlg.notEmpty");
    if (bad) {
      setErr(bad);
      return;
    }
    if (confirm && v !== value2) {
      setErr(t("dlg.pwdMismatch"));
      return;
    }
    setBusy(true);
    try {
      await onSubmit(v);
      onClose();
    } catch (e) {
      // errMsg：后端 AppError 不是 Error 实例，String(e) 只会给出一块 [object Object]
      setErr(errMsg(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-mask" {...useMaskDismiss(onClose)}>
      <div className="modal prompt">
        <div className="modal-head">
          <h2>{title}</h2>
          <button type="button" className="modal-x" title={t("common.close")} onClick={onClose}>
            ×
          </button>
        </div>

        <div
          className="connect-form"
          onKeyDown={(e) => {
            if (e.key === "Enter") void submit();
          }}
        >
          <label>
            {label}
            {secret ? (
              <PasswordField
                ref={input}
                value={value}
                autoComplete="new-password"
                onChange={(v) => {
                  setValue(v);
                  setErr("");
                }}
              />
            ) : (
              <input
                ref={input}
                type="text"
                value={value}
                spellCheck={false}
                onChange={(e) => {
                  setValue(e.target.value);
                  setErr("");
                }}
              />
            )}
          </label>
          {confirm && (
            <label>
              {confirmLabel}
              {secret ? (
                <PasswordField
                  value={value2}
                  autoComplete="new-password"
                  onChange={(v) => {
                    setValue2(v);
                    setErr("");
                  }}
                />
              ) : (
                <input
                  type="text"
                  value={value2}
                  spellCheck={false}
                  onChange={(e) => {
                    setValue2(e.target.value);
                    setErr("");
                  }}
                />
              )}
            </label>
          )}
          {hint && <div className="p-hint">{hint}</div>}
          {err && <div className="form-error">{err}</div>}
        </div>

        <div className="form-actions">
          <button type="button" className="ghost" onClick={onClose}>
            {t("common.cancel")}
          </button>
          <button type="button" disabled={busy} onClick={() => void submit()}>
            {ok}
          </button>
        </div>
      </div>
    </div>
  );
}
