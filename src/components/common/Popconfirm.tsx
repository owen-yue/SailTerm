import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../../lib/i18n";

interface Props {
  /** 气泡左上角的视口坐标（取右键菜单位置或触发元素底边）。 */
  x: number;
  y: number;
  /** 主提示文案，如「删除 123 ？」。 */
  title: string;
  /** 次级说明 / 警告（可选，如「远端目录非空会被拒绝」）。 */
  description?: string;
  /** 确认按钮文案，默认取 `common.delete`。 */
  okText?: string;
  /** 取消按钮文案，默认取 `common.cancel`。 */
  cancelText?: string;
  /** 确认按钮是否为危险操作（红色）。默认 true。 */
  danger?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}

/**
 * 轻量气泡确认（Element Plus Popconfirm 风格）：在触发处弹出小卡片，
 * 警告图标 + 提示文案 + 「取消 / 确定」两颗按钮；点外部 / Esc / 窗口变化即关闭。
 *
 * 定位沿用 ContextMenu 思路：绘制前按实际尺寸做边缘翻转，下方放不下则翻到上方。
 * 用于删除等破坏性动作的**就近确认**，比居中模态更轻、不打断上下文。
 */
export default function Popconfirm({
  x,
  y,
  title,
  description,
  okText,
  cancelText,
  danger = true,
  onConfirm,
  onClose,
}: Props) {
  const t = useI18n();
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: x, top: y });

  // layout effect：绘制前完成翻转，用户看不到气泡跳动
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const left = x + width > window.innerWidth ? Math.max(4, window.innerWidth - width - 4) : x;
    // 下方越界就翻到锚点上方；上方也不够则贴顶
    let top = y;
    if (y + height > window.innerHeight) top = Math.max(4, y - height - 10);
    if (left !== x || top !== y) setPos({ left, top });
  }, [x, y, title, description]);

  useEffect(() => {
    const down = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", down);
    window.addEventListener("keydown", key);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("mousedown", down);
      window.removeEventListener("keydown", key);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose]);

  // 用 createPortal 挂到 document.body：脱离模态/面板等祖先的 overflow 裁切与层叠上下文，
  // 确保在 Sheet 模态（.modal-mask z-index:50）内也能浮于遮罩之上、正常点击（同 HoverPopover/Select 约定）。
  return createPortal(
    <div className="popconfirm" ref={ref} style={pos} onClick={(e) => e.stopPropagation()}>
      <div className="pc-body">
        <span className="pc-icon" aria-hidden>
          ?
        </span>
        <div className="pc-text">
          <div className="pc-title">{title}</div>
          {description && <div className="pc-desc">{description}</div>}
        </div>
      </div>
      <div className="pc-actions">
        <button type="button" className="pc-cancel" onClick={onClose}>
          {cancelText ?? t("common.cancel")}
        </button>
        <button
          type="button"
          className={`pc-ok${danger ? " danger" : ""}`}
          onClick={onConfirm}
        >
          {okText ?? t("common.delete")}
        </button>
      </div>
    </div>,
    document.body,
  );
}
