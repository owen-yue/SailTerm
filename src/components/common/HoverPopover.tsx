import { useCallback, useRef, useState } from "react";
import type { ReactNode } from "react";
import { createPortal } from "react-dom";

/**
 * 通用悬停浮层（tooltip / popover）。
 *
 * 触发器内联渲染，浮层内容用 `createPortal` 挂到 `document.body` 并以 `position:fixed`
 * 按触发器矩形定位——从而**脱离任何祖先的 overflow 裁切**（设置面板 `.sc-pane` 是
 * `overflow-y:auto`，绝对定位的下拉会被切掉，这正是本组件要解决的问题）。
 *
 * 关闭用「延迟隐藏 + 进入内容即取消」，让鼠标能从触发器平滑移到浮层里点击（如复制/插入），
 * 不会因为跨越触发器与浮层之间的空隙而中途消失。
 */
interface Props {
  /** 触发器内容（如一个 ? 徽标 + 文字）。 */
  children: ReactNode;
  /** 悬停时展示的浮层内容。 */
  content: ReactNode;
  /** 触发器样式类。 */
  className?: string;
  /** 浮层对齐触发器的哪条边：start=左对齐、end=右对齐。 */
  align?: "start" | "end";
  /** 浮层宽度（px）；窄屏下会自动夹到视口内。 */
  width?: number;
}

export default function HoverPopover({ children, content, className, align = "start", width = 380 }: Props) {
  const anchorRef = useRef<HTMLSpanElement>(null);
  const hideTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);

  const show = useCallback(() => {
    if (hideTimer.current) clearTimeout(hideTimer.current);
    const el = anchorRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const w = Math.min(width, window.innerWidth - 16);
    let left = align === "end" ? r.right - w : r.left;
    left = Math.max(8, Math.min(left, window.innerWidth - w - 8));
    // top 紧贴触发器下沿（不留空隙），空隙改由浮层自身 padding-top 桥接，保证 hover 连续
    setPos({ top: r.bottom, left });
  }, [align, width]);

  const scheduleHide = useCallback(() => {
    if (hideTimer.current) clearTimeout(hideTimer.current);
    hideTimer.current = setTimeout(() => setPos(null), 140);
  }, []);

  return (
    <>
      <span ref={anchorRef} className={className} onMouseEnter={show} onMouseLeave={scheduleHide}>
        {children}
      </span>
      {pos &&
        createPortal(
          <div
            className="hover-pop"
            style={{ top: pos.top, left: pos.left, width }}
            onMouseEnter={show}
            onMouseLeave={scheduleHide}
          >
            <div className="hover-pop-inner">{content}</div>
          </div>,
          document.body,
        )}
    </>
  );
}
