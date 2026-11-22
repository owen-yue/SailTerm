import type { ReactNode } from "react";
import { useI18n } from "../../lib/i18n";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";

interface Props {
  title: ReactNode;
  onClose: () => void;
  /** 嵌入设置中心时只渲染内容体，不套模态壳（标题栏/遮罩由设置中心提供）。 */
  embedded?: boolean;
  /** 模态附加类，如 ProviderSettings 的 `pv`（默认 `sec-set`）。 */
  variant?: string;
  children: ReactNode;
}

/**
 * 设置类模态统一外壳：独立打开时是 `modal sec-set`（或 variant），嵌入设置中心时退化为纯内容体。
 * 内容（body / 错误条 / 底部操作条）由调用方组装，本组件只负责壳与标题。
 */
export default function Sheet({
  title,
  onClose,
  embedded = false,
  variant = "sec-set",
  children,
}: Props) {
  const t = useI18n();
  // 钩子必须无条件调用（故写在 embedded 早退之前）；嵌入模式由设置中心接管退出。
  useEscClose(onClose, !embedded);
  const maskProps = useMaskDismiss(onClose);
  if (embedded) return <div className="ss-sheet">{children}</div>;
  return (
    <div className="modal-mask" {...maskProps}>
      <div className={`modal ${variant}`} onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h2>{title}</h2>
          <button className="modal-x" onClick={onClose} aria-label={t("common.close")}>
            ×
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}
