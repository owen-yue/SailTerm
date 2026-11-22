import { useCallback, useRef } from "react";

interface Props {
  /** `x` = 水平拖宽（左右），`y` = 垂直拖高（上下）。 */
  axis: "x" | "y";
  /** 拖动方向：x 默认向左拖变宽（handle 在右）；y 向上拖变高（handle 在上）。 */
  onDelta: (delta: number) => void;
  className?: string;
  title?: string;
}

/**
 * 统一拖拽条：侧栏 / AI 面板 / 底部面板共用。
 * 线宽 3px，hover 高亮；z-index 保证压在终端画布之上。
 */
export default function Splitter({ axis, onDelta, className = "", title }: Props) {
  const dragging = useRef(false);

  const down = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      dragging.current = true;
      const start = axis === "x" ? e.clientX : e.clientY;
      let last = start;
      const move = (ev: MouseEvent) => {
        if (!dragging.current) return;
        const cur = axis === "x" ? ev.clientX : ev.clientY;
        onDelta(cur - last);
        last = cur;
      };
      const up = () => {
        dragging.current = false;
        window.removeEventListener("mousemove", move);
        window.removeEventListener("mouseup", up);
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      };
      window.addEventListener("mousemove", move);
      window.addEventListener("mouseup", up);
      document.body.style.cursor = axis === "x" ? "col-resize" : "row-resize";
      document.body.style.userSelect = "none";
    },
    [axis, onDelta],
  );

  return (
    <div
      className={`splitter splitter-${axis} ${className}`}
      title={title}
      onMouseDown={down}
      role="separator"
      aria-orientation={axis === "x" ? "vertical" : "horizontal"}
    />
  );
}
