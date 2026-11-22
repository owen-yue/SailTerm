import { useI18n } from "../../lib/i18n";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";

interface Props {
  title: string;
  /** 正文（可含路径等具体信息）。 */
  message: string;
  /** 追加的风险提示，标红显示（如「本地目录会连同内容一起删除」）。 */
  warning?: string;
  okText?: string;
  onOk: () => void | Promise<void>;
  onClose: () => void;
}

/** 危险操作确认（「危险操作强提示」）：删除等不可逆动作必须先过这一道。*/
export default function ConfirmDialog({
  title,
  message,
  warning,
  okText,
  onOk,
  onClose,
}: Props) {
  const tr = useI18n();
  const ok = okText ?? tr("common.delete");
  // Esc 关窗走弹层栈（window 级统一仲裁：只关最顶层，不会把下层的表单窗一起关掉）。
  useEscClose(onClose);

  return (
    <div className="modal-mask" {...useMaskDismiss(onClose)}>
      <div className="modal prompt">
        <div className="modal-head">
          <h2>{title}</h2>
          <button type="button" className="modal-x" title={tr("common.close")} onClick={onClose}>
            ×
          </button>
        </div>

        <div className="hk-body">
          <p className="cf-msg">{message}</p>
          {warning && <p className="hk-warn">{warning}</p>}
        </div>

        <div className="form-actions">
          <button type="button" className="ghost" onClick={onClose}>
            {tr("common.cancel")}
          </button>
          <button type="button" className="danger" onClick={() => void onOk()}>
            {ok}
          </button>
        </div>
      </div>
    </div>
  );
}
