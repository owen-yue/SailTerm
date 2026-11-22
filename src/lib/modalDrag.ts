// 弹框拖拽：让所有 `.modal-mask > .modal` 弹窗可用标题栏（`.modal-head`）拖动。
//
// 弹窗数量多但外壳结构统一（Sheet / FormDialog / ConfirmDialog / PromptDialog /
// SettingsCenter / HostKeyDialog / FileEditor / UsageStatsDialog … 都以 `.modal-head`
// 作标题栏），逐个组件挂 hook 太侵入。这里用一个 document 级委托监听：命中 `.modal-head`
// 的空白处即拖动其所属 `.modal`，未来新增弹窗也自动支持，无需改动任何业务组件。
//
// 定位原理：`.modal-mask` 是 `display:grid; place-items:center`，`.modal` 天然居中。
// 拖拽不改布局，只给 `.modal` 叠加 `transform: translate(x, y)` 偏移；偏移量按元素记在
// WeakMap 里，每次打开都是新元素 → 从居中 (0,0) 起。位移按视口做边界钳制，标题栏始终留在
// 可见区内（弹窗体本身有 max-height + 内部滚动，不会因拖拽丢失内容）。

/** 每个 `.modal` 元素当前的位移量（视口像素）。元素随弹窗挂载新建，默认从 0 起。 */
const offsets = new WeakMap<HTMLElement, { x: number; y: number }>();

/** 标题栏里的交互控件：从这些元素按下不触发拖拽（关闭按钮、搜索框、分段按钮等照常工作）。 */
const INTERACTIVE =
  "button, a, input, select, textarea, [role='button'], [role='switch'], [contenteditable='true']";

function onPointerDown(e: PointerEvent): void {
  if (e.button !== 0 || !e.isPrimary) return; // 只响应左键 / 主指针
  const target = e.target as HTMLElement | null;
  if (!target || typeof target.closest !== "function") return;
  const head = target.closest(".modal-head");
  if (!head) return;
  // 已自带拖拽的标题栏（用 useDragModal + .drag-handle 的弹窗）跳过，避免与其
  // 内联 transform 双写冲突。全局监听只接管那些没有专属拖拽实现的弹窗。
  if (head.classList.contains("drag-handle")) return;
  if (target.closest(INTERACTIVE)) return; // 交互控件不劫持
  const modal = head.closest(".modal") as HTMLElement | null;
  if (!modal) return;

  const start = offsets.get(modal) ?? { x: 0, y: 0 };
  const rect = modal.getBoundingClientRect();
  // 未位移时的基准视口坐标 = 当前坐标 - 既有位移
  const baseLeft = rect.left - start.x;
  const baseTop = rect.top - start.y;
  const w = rect.width;
  const h = rect.height;
  const startX = e.clientX;
  const startY = e.clientY;
  const M = 8; // 距视口边缘最小留白

  const clamp = (v: number, min: number, max: number) => (max < min ? min : Math.min(Math.max(v, min), max));

  e.preventDefault(); // 抑制标题文字被选中
  (head as HTMLElement).style.cursor = "grabbing";
  document.body.style.userSelect = "none";

  const onMove = (ev: PointerEvent) => {
    const nx = clamp(start.x + (ev.clientX - startX), M - baseLeft, window.innerWidth - w - M - baseLeft);
    const ny = clamp(start.y + (ev.clientY - startY), M - baseTop, window.innerHeight - h - M - baseTop);
    offsets.set(modal, { x: nx, y: ny });
    modal.style.transform = `translate(${nx}px, ${ny}px)`;
  };
  const onUp = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
    (head as HTMLElement).style.cursor = "";
    document.body.style.userSelect = "";
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}

/**
 * 安装全局弹框拖拽。由入口 `main.tsx` 顶层调用一次：每个 WebView（主窗口 / 会话弹出窗 /
 * 工具窗）都有独立 document，在入口装才三种窗口全覆盖。用捕获阶段监听，避免个别弹窗
 * 内部对 pointerdown 调 stopPropagation 后拖拽失效。返回卸载函数（入口一次性安装，无需清理）。
 */
export function installModalDrag(): () => void {
  document.addEventListener("pointerdown", onPointerDown, true);
  return () => document.removeEventListener("pointerdown", onPointerDown, true);
}
