// 让模态框可通过标题栏（或任意手柄）在窗口内拖动。
// 用法：const { offset, handleProps, reset } = useDragModal();
//   弹窗根节点 style={{ transform: `translate(${offset.x}px, ${offset.y}px)` }}，
//   手柄节点 {...handleProps} className="drag-handle"。
// 说明：偏移是相对遮罩居中位置的增量，弹窗关闭重开即随组件卸载复位；
//   如需同一实例内主动复位（例如切换内容时），调用 reset()。
import { useCallback, useRef, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";

export function useDragModal() {
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  // 起点：指针按下时的屏幕坐标 + 当时的偏移，move 时按增量平移。
  const start = useRef({ px: 0, py: 0, ox: 0, oy: 0 });
  const dragging = useRef(false);

  const onPointerDown = useCallback(
    (e: ReactPointerEvent<HTMLElement>) => {
      // 手柄内的交互控件（关闭按钮等）不触发拖动，交给它自己的 click。
      if ((e.target as HTMLElement).closest("button")) return;
      dragging.current = true;
      start.current = { px: e.clientX, py: e.clientY, ox: offset.x, oy: offset.y };
      // 捕获指针：即使移出窗口范围也能持续收到 move/up。
      e.currentTarget.setPointerCapture(e.pointerId);
    },
    [offset],
  );

  const onPointerMove = useCallback((e: ReactPointerEvent<HTMLElement>) => {
    if (!dragging.current) return;
    const { px, py, ox, oy } = start.current;
    setOffset({ x: ox + (e.clientX - px), y: oy + (e.clientY - py) });
  }, []);

  const stop = useCallback((e: ReactPointerEvent<HTMLElement>) => {
    dragging.current = false;
    const el = e.currentTarget;
    if (el.hasPointerCapture?.(e.pointerId)) el.releasePointerCapture(e.pointerId);
  }, []);

  return {
    offset,
    reset: () => setOffset({ x: 0, y: 0 }),
    handleProps: {
      onPointerDown,
      onPointerMove,
      onPointerUp: stop,
      onPointerCancel: stop,
    },
  };
}
