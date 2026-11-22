import { useEffect, useState } from "react";
import { toast, toastStore, useStore, type ToastItem, type ToastKind } from "../../stores";
import { useI18n } from "../../lib/i18n";

interface Props {
  /** 需常驻、手动关闭的错误（会话/连接/传输错误合并而来）；无则 null。 */
  error?: string | null;
  /** 关闭常驻错误时回调（清 session/connection/transfer 三处 error）。 */
  onDismissError?: () => void;
}

/**
 * 全局通知层：右上角逐条滑入的卡片，可堆叠。
 * - `error`（会话/连接/传输错误）与 kind==="error" 的通知常驻，仅 × 可关；
 * - warn/info/success 到点自动淡出（warn 略久）。
 * 主窗口与弹出窗口共用本组件。
 */
export default function Toasts({ error, onDismissError }: Props) {
  const { items } = useStore(toastStore);
  const errs = items.filter((i) => i.kind === "error");
  const rest = items.filter((i) => i.kind !== "error");
  return (
    <div className="toasts">
      {/* 按严重级分两个 live region：一个容器只能有一种 politeness，混在一起会把报错降级成
          「稍后再念」。空区也常驻 DOM——连容器一起插入的 live region 读屏经常不播报。 */}
      <div className="toast-zone" role="alert" aria-live="assertive">
        {error && (
          <div className="toast-card error">
            <KindIcon kind="error" />
            <span className="tc-msg">{error}</span>
            <CloseButton onClick={onDismissError} />
          </div>
        )}
        {errs.map((item) => (
          <ToastCard key={item.id} item={item} />
        ))}
      </div>
      <div className="toast-zone" role="status" aria-live="polite">
        {rest.map((item) => (
          <ToastCard key={item.id} item={item} />
        ))}
      </div>
    </div>
  );
}

/** 单条通知卡片：负责自身定时淡出与退场动画。 */
function ToastCard({ item }: { item: ToastItem }) {
  const [leaving, setLeaving] = useState(false);
  /** 鼠标悬停或键盘聚焦时暂停倒计时：长一点的提示常在读完之前就被抹掉。 */
  const [paused, setPaused] = useState(false);
  const close = () => setLeaving(true);

  // 非 error 类型定时自动淡出；error 常驻。
  useEffect(() => {
    if (item.kind === "error" || leaving || paused) return;
    const ms = item.kind === "warn" ? 6000 : 4000;
    const t = setTimeout(close, ms);
    return () => clearTimeout(t);
  }, [item.id, item.kind, paused, leaving]);

  // 淡出动画结束后再从 store 移除。
  useEffect(() => {
    if (!leaving) return;
    const t = setTimeout(() => toast.dismiss(item.id), 220);
    return () => clearTimeout(t);
  }, [leaving, item.id]);

  return (
    <div
      className={`toast-card ${item.kind}${leaving ? " leaving" : ""}`}
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
      onFocusCapture={() => setPaused(true)}
      onBlurCapture={() => setPaused(false)}
    >
      <KindIcon kind={item.kind} />
      {/* 来源在上、正文在下：长报错不被来源标签挤成窄条（同历史面板口径） */}
      <div className="tc-body">
        {item.source && <span className="tc-src">{item.source}</span>}
        <span className="tc-msg">{item.message}</span>
      </div>
      <CloseButton onClick={close} />
    </div>
  );
}

function CloseButton({ onClick }: { onClick?: () => void }) {
  const tr = useI18n();
  return (
    <button
      type="button"
      className="tc-x"
      title={tr("common.close")}
      aria-label={tr("common.close")}
      onClick={onClick}
    >
      ×
    </button>
  );
}

/** 各类型线稿图标（16px，随色条同色）；通知历史面板（NotificationsMenu）共用同一套。 */
export function KindIcon({ kind }: { kind: ToastKind }) {
  const common = {
    className: "tc-ico",
    width: 16,
    height: 16,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 2,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
  };
  if (kind === "success") {
    return (
      <svg {...common}>
        <circle cx="12" cy="12" r="9" />
        <path d="m8.5 12 2.5 2.5 4.5-5" />
      </svg>
    );
  }
  if (kind === "error") {
    return (
      <svg {...common}>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7.5v5.5" />
        <path d="M12 16.5h.01" />
      </svg>
    );
  }
  if (kind === "warn") {
    return (
      <svg {...common}>
        <path d="M10.3 4.3 2.5 18a1.8 1.8 0 0 0 1.6 2.7h15.8A1.8 1.8 0 0 0 21.5 18L13.7 4.3a1.8 1.8 0 0 0-3.4 0Z" />
        <path d="M12 9.5v4" />
        <path d="M12 17h.01" />
      </svg>
    );
  }
  return (
    <svg {...common}>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v5" />
      <path d="M12 8h.01" />
    </svg>
  );
}
