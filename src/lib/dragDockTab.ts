// 停靠页签拖拽（下栏 ↔ 右栏换位、同区拖拽重排）：自定义 pointer 事件实现。
//
// 为什么不用 HTML5 draggable/drop：macOS WKWebView（Tauri 默认内核）里，为支持从 Finder 拖文件
// 上传而开启的 dragDropEnabled（默认 true，见 lib/tauri/file.ts 的 onDragDropEvent）会**吞掉页面内
// 所有 HTML5 拖放事件**——dragstart/dragover/drop 根本不触发，二者互斥。pointer（鼠标/触控）事件
// 不受此影响，且与原生文件拖放共存，故这里用 pointer 手写拖拽。
//
// 交互：按下页签 → 移动超阈值判定拖拽（pointermove.preventDefault + 清选区，杜绝拖过文字被选中）
// → 跟随指针显示「图标+名称」幽灵胶囊 + 在插入位画一条插入指示杠 → 松手按落点跨区移动或同区重排。
import type { PointerEvent as ReactPointerEvent } from "react";
import type { DockPanelId } from "../types";
import { dockActions, dockStore, type DockZone } from "../stores";

/** 判定「按下即拖」还是「只是点击」的最小位移（px）。 */
const THRESHOLD = 4;
/** 插入指示杠粗细（px）与离页签的间距（px）。 */
const CARET_TH = 2;
const CARET_GAP = 2;
/** 邻近容差（px）：松手点离某条页签条多近仍算投向该区——放宽热区，不必像素级对准。 */
const STRIP_MARGIN = 64;

interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

interface Hit {
  zone: DockZone;
  el: HTMLElement;
  /** 插入位（以「摘除被拖项后」该区**可见**页签数计），缺省=追加末尾。 */
  index: number;
  /** 该插入位参照的可见页签序列（DOM 顺序，不含被拖项）—— store 下标换算要用。 */
  others: DockPanelId[];
  /** 插入指示杠的落位（视口坐标）。 */
  caret: Rect;
}

/**
 * 可见页签序号 → `dockStore` 里的真实下标。
 *
 * 页签条只渲染当前会话用得上的面板（见 `dockRegistry.visiblePanels`），而 store 存的是**完整布局**
 * （能力是当前会话的属性，不该替用户改掉布局）—— 两边下标于是不同。直接拿可见序号去 splice，
 * 落点会偏：FTP 会话下把「会话」拖到「审计」之后，store 里会插到被隐藏的「监控」前面。
 * 所以按「插到哪个可见页签之前」回 store 找锚点；被隐藏的项留在它前面也不占页签位，观感仍正确。
 */
function storeIndex(zone: DockZone, dragged: DockPanelId, others: DockPanelId[], index: number): number {
  const s = dockStore.get();
  const dst = (zone === "right" ? s.right : s.bottom).filter((x) => x !== dragged);
  const anchor = others[index];
  if (anchor === undefined) return dst.length; // 拖到末尾：隐藏项在前也不碍事
  const at = dst.indexOf(anchor);
  return at < 0 ? dst.length : at;
}

/** 由目标区 + 其余页签 + 插入位算出指示杠矩形：下栏竖杠、右栏横杠，贴在前/后一页签的边缘。 */
function caretRect(zone: DockZone, el: HTMLElement, others: HTMLElement[], index: number): Rect {
  const zr = el.getBoundingClientRect();
  const pad = 4;
  if (others.length === 0) {
    return zone === "bottom"
      ? { left: zr.left + pad, top: zr.top + pad, width: CARET_TH, height: Math.max(0, zr.height - pad * 2) }
      : { left: zr.left + pad, top: zr.top + pad, width: Math.max(0, zr.width - pad * 2), height: CARET_TH };
  }
  if (zone === "bottom") {
    const edge =
      index >= others.length
        ? others[others.length - 1].getBoundingClientRect().right
        : others[Math.max(0, index)].getBoundingClientRect().left;
    const left = index >= others.length ? edge + CARET_GAP : edge - CARET_GAP - CARET_TH;
    return { left, top: zr.top + pad, width: CARET_TH, height: Math.max(0, zr.height - pad * 2) };
  }
  const edge =
    index >= others.length
      ? others[others.length - 1].getBoundingClientRect().bottom
      : others[Math.max(0, index)].getBoundingClientRect().top;
  const top = index >= others.length ? edge + CARET_GAP : edge - CARET_GAP - CARET_TH;
  return { left: zr.left + pad, top, width: Math.max(0, zr.width - pad * 2), height: CARET_TH };
}

/** 在页签 pointerdown 时调用，接管后续 move/up。目标区与插入位都由松手落点决定。 */
export function beginDockTabDrag(id: DockPanelId, e: ReactPointerEvent<HTMLElement>): void {
  if (e.button !== 0) return; // 仅左键/主触发起拖
  // 注：不在 pointerdown 上 preventDefault——WKWebView 里那会吞掉后续 click，破坏单击切页。
  // 防选文字改由拖拽中的 pointermove.preventDefault + dock-dragging 的 user-select:none 兜底。
  const src = e.currentTarget;
  const label = src.dataset.dockLabel ?? src.textContent ?? "";
  const startX = e.clientX;
  const startY = e.clientY;
  let dragging = false;
  let hintEl: HTMLElement | null = null;
  let ghost: HTMLElement | null = null;
  let caretEl: HTMLElement | null = null;

  // 命中测试：指针下是否落在某停靠热区（页签条）；顺带按指针在该区内某页签的前/后半算插入位与指示杠落位。
  const hitTest = (x: number, y: number): Hit | null => {
    const point = document.elementFromPoint(x, y);
    // ① 直接命中：指针正落在某条页签条上。
    let el =
      point?.closest<HTMLElement>('[data-drop-zone="bottom"]') ??
      point?.closest<HTMLElement>('[data-drop-zone="right"]') ??
      null;
    let zone: DockZone | null = el
      ? el.getAttribute("data-drop-zone") === "bottom"
        ? "bottom"
        : "right"
      : null;
    // ② 邻近容差：没正落在页签条上时，取「离指针最近且在容差内」的那条——下栏条仅 28px 高、
    //    右栏条仅 54px 宽，像素级对准太苛刻，故在附近松手也算投向该栏（落点仍按轴向上最近页签插）。
    if (!el) {
      let bestD = Infinity;
      for (const z of ["bottom", "right"] as DockZone[]) {
        const cand = document.querySelector<HTMLElement>(`[data-drop-zone="${z}"]`);
        if (!cand) continue;
        const r = cand.getBoundingClientRect();
        const dx = Math.max(r.left - x, 0, x - r.right);
        const dy = Math.max(r.top - y, 0, y - r.bottom);
        const d = Math.hypot(dx, dy);
        if (d <= STRIP_MARGIN && d < bestD) {
          bestD = d;
          el = cand;
          zone = z;
        }
      }
    }
    if (!el || !zone) return null;
    // 该区内其余页签（DOM 顺序=数组顺序），排除被拖项，index 以其为准
    const others = Array.from(el.querySelectorAll<HTMLElement>("[data-dock-id]")).filter(
      (t) => t.dataset.dockId !== id,
    );
    const othersIds = others.map((t) => t.dataset.dockId as DockPanelId);
    // 插入位：优先指针正压的页签；邻近松手时取沿轴最近的一枚，按其前/后半决定插到它前还是后
    let over = point?.closest<HTMLElement>("[data-dock-id]") ?? null;
    if (over && over.dataset.dockId === id) over = null;
    if (!over && others.length) {
      let bd = Infinity;
      for (const tb of others) {
        const r = tb.getBoundingClientRect();
        const c = zone === "bottom" ? r.left + r.width / 2 : r.top + r.height / 2;
        const d = Math.abs((zone === "bottom" ? x : y) - c);
        if (d < bd) {
          bd = d;
          over = tb;
        }
      }
    }
    let index = others.length;
    if (over) {
      const r = over.getBoundingClientRect();
      const after = zone === "bottom" ? x > r.left + r.width / 2 : y > r.top + r.height / 2;
      index = Math.max(0, others.indexOf(over)) + (after ? 1 : 0);
    }
    return { zone, el, index, others: othersIds, caret: caretRect(zone, el, others, index) };
  };

  const setHint = (el: HTMLElement | null) => {
    if (hintEl === el) return;
    hintEl?.removeAttribute("data-drop-hint");
    hintEl = el;
    hintEl?.setAttribute("data-drop-hint", "1");
  };

  const placeGhost = (x: number, y: number) => {
    if (ghost) ghost.style.transform = `translate(${x + 14}px, ${y + 16}px)`;
  };

  // 造一个仿系统的拖拽胶囊：图标 + 名称，脱离文档流跟随指针
  const makeGhost = () => {
    ghost = document.createElement("div");
    ghost.className = "dock-ghost";
    const svg = src.querySelector("svg");
    if (svg) {
      const ic = svg.cloneNode(true) as SVGElement;
      ic.removeAttribute("class");
      ic.style.width = "14px";
      ic.style.height = "14px";
      ic.style.display = "block";
      ghost.appendChild(ic);
    }
    const txt = document.createElement("span");
    txt.textContent = label;
    ghost.appendChild(txt);
    document.body.appendChild(ghost);
    placeGhost(e.clientX, e.clientY);
  };

  const showCaret = (r: Rect) => {
    if (!caretEl) {
      caretEl = document.createElement("div");
      caretEl.className = "dock-caret";
      document.body.appendChild(caretEl);
    }
    caretEl.style.left = `${r.left}px`;
    caretEl.style.top = `${r.top}px`;
    caretEl.style.width = `${r.width}px`;
    caretEl.style.height = `${r.height}px`;
  };

  const onMove = (ev: PointerEvent) => {
    if (!dragging) {
      if (Math.hypot(ev.clientX - startX, ev.clientY - startY) < THRESHOLD) return;
      dragging = true;
      document.getSelection()?.removeAllRanges(); // 清掉可能已产生的选区
      document.body.classList.add("dock-dragging");
      makeGhost();
    }
    ev.preventDefault(); // 拖拽中阻止浏览器原生选文字（不影响 click）
    placeGhost(ev.clientX, ev.clientY);
    const hit = hitTest(ev.clientX, ev.clientY);
    setHint(hit ? hit.el : null); // 同区也是有效重排目标，故一律高亮命中的页签条
    if (hit) showCaret(hit.caret);
    else {
      caretEl?.remove();
      caretEl = null;
    }
  };

  const teardown = () => {
    document.removeEventListener("pointermove", onMove, true);
    document.removeEventListener("pointerup", onUp, true);
    document.removeEventListener("pointercancel", onUp, true);
    document.body.classList.remove("dock-dragging");
    setHint(null);
    ghost?.remove();
    ghost = null;
    caretEl?.remove();
    caretEl = null;
  };

  const onUp = (ev: PointerEvent) => {
    const wasDragging = dragging;
    const hit = wasDragging ? hitTest(ev.clientX, ev.clientY) : null;
    teardown();
    if (!wasDragging) return; // 视为点击：不吞事件，onClick/onDoubleClick 照常
    // 拖拽后浏览器仍会补发一次 click → 捕获阶段吞掉，避免误触发切页/收起
    const swallow = (ce: MouseEvent) => {
      ce.stopPropagation();
      ce.preventDefault();
      window.removeEventListener("click", swallow, true);
    };
    window.addEventListener("click", swallow, true);
    if (hit) dockActions.move(id, hit.zone, storeIndex(hit.zone, id, hit.others, hit.index));
  };

  document.addEventListener("pointermove", onMove, true);
  document.addEventListener("pointerup", onUp, true);
  document.addEventListener("pointercancel", onUp, true);
}
