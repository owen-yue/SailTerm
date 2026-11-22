// 弹层（弹窗 / 遮罩 / 浮层）的两条统一交互定式。
//
// 1) `useEscClose`：Esc 关**最顶层**弹层。此前全仓是 15 处各写一遍 window keydown，
//    带来两个后果：① 嵌套弹层（如删除确认盖在连接弹窗上）一次 Esc 全关；
//    ② 有一批弹层压根没登记 Esc（设置中心 / 连接弹窗 / 文件编辑器），还有一批把 Esc
//    挂在**不可聚焦的 div** 上永远接不到（ChmodDialog——ConfirmDialog.tsx 的注释里
//    已记录过同一个坑）。改为单一仲裁点：谁最后挂载谁在最上层，Esc 只关它一个。
//
// 2) `useMaskDismiss`：遮罩关闭。旧写法 `onClick={onClose}` 会在「弹窗内拖选文字、
//    松手时指针落在遮罩上」时误关整个弹窗（连接表单等于整表丢弃已填密码）；
//    正确判据是 mousedown 也必须落在遮罩自身。

import { useEffect, useRef, type MouseEvent } from "react";

type EscHandler = (e: KeyboardEvent) => void;

/** 弹层栈：后挂载者在顶层。模块级单例，与 React 树无关（弹层本就渲染在根上）。 */
const stack: EscHandler[] = [];
let bound = false;

/** capture 阶段拦在最前，配合 stopPropagation 阻断下层弹层/页面的自有 Esc 处理。 */
function onGlobalKey(e: KeyboardEvent) {
  if (e.key !== "Escape" || stack.length === 0) return;
  // 已被更上层（如同样注册了 capture 监听的组件）消费过就不再重复关闭，避免一次 Esc 关两层
  if (e.defaultPrevented) return;
  const top = stack[stack.length - 1];
  e.preventDefault();
  e.stopPropagation();
  top(e);
}

function pushEsc(handler: EscHandler): () => void {
  if (!bound && typeof window !== "undefined") {
    bound = true;
    window.addEventListener("keydown", onGlobalKey, true);
  }
  stack.push(handler);
  return () => {
    const i = stack.lastIndexOf(handler);
    if (i >= 0) stack.splice(i, 1);
  };
}

/**
 * 把 Esc 关窗登记进弹层栈。`active=false` 时不登记（用于「同一位置同时只应有一个
 * 可 Esc 的浮层」或禁用退出的场景，如提交中）。
 * 注意 onClose 进依赖：调用方传内联函数时每帧重挂会把自身挪到栈顶，故此处用 ref 固定身份。
 */
export function useEscClose(onClose: () => void, active = true): void {
  const ref = useRef(onClose);
  ref.current = onClose;
  useEffect(() => {
    if (!active) return;
    const handler: EscHandler = () => ref.current();
    return pushEsc(handler);
  }, [active]);
}

/**
 * 遮罩关闭属性集：只在「按下与松开都落在遮罩自身」时关闭。
 * 用法：`<div className="modal-mask" {...useMaskDismiss(onClose)}>`；
 * 决策类弹窗（主机密钥 / 证书信任 / 同名冲突）传 `enabled=false`，或不挂本 hook。
 */
export function useMaskDismiss(
  onClose: () => void,
  enabled = true,
): { onMouseDown: (e: MouseEvent) => void } {
  const ref = useRef(onClose);
  ref.current = onClose;
  return {
    onMouseDown: (e) => {
      if (!enabled) return;
      if (e.target === e.currentTarget) ref.current();
    },
  };
}
