import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import type { KeyboardEvent as RKeyboardEvent, ReactNode } from "react";
import Chevron from "./Chevron";
import { useEscClose } from "../../lib/useOverlay";

export interface MenuItem {
  /** 菜单项文案；可内嵌线稿图标（ReactNode），故渲染时改用下标作 key。 */
  label: ReactNode;
  /** 危险操作（删除）标红。 */
  danger?: boolean;
  disabled?: boolean;
  /** 在这一项之前画一条分隔线。 */
  sep?: boolean;
  onClick: () => void;
  /**
   * 二级菜单：悬停/→ 键向右飞出（终端右键菜单的「导出」用它收成两项）。
   * 只支持一层——再深的层级在运维工具里属于「该做成面板」的信号。
   */
  children?: MenuItem[];
}

interface Props {
  /** 视口坐标（来自 `onContextMenu` 的 clientX/clientY）。 */
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
  /** 追加类名：终端内层叠上下文特殊（需要高于 xterm 画布与下栏），用它可以微调 z-index。 */
  className?: string;
}

/** 可选项下标（跳过 disabled 与分隔线占位）。 */
function enabledIdx(items: MenuItem[] | undefined): number[] {
  if (!items) return [];
  return items.map((it, i) => (it.disabled ? -1 : i)).filter((i) => i >= 0);
}

/**
 * 轻量右键菜单：点外部 / Esc / 窗口尺寸变化即关闭。
 * 首次渲染后按实际尺寸做一次边缘翻转，避免菜单被窗口裁掉。
 *
 * 键盘可达性（本菜单是连接/文件/页签/终端的主要操作面，此前**只能鼠标点**）：
 * 打开即把焦点放进第一个可选项，↑↓ 移动、→ 展开二级、← 收起、Home/End 跳首尾、
 * Enter/Space 执行、Esc 先收二级再关菜单；并补 `role=menu` / `role=menuitem`
 * 与 `aria-haspopup`，让读屏能念出当前项。
 */
export default function ContextMenu({ x, y, items, onClose, className }: Props) {
  const uid = useId();
  const ref = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const subRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const [pos, setPos] = useState({ left: x, top: y });
  const usable = enabledIdx(items);
  const [active, setActive] = useState(() => (usable.length ? usable[0] : -1));
  /** 当前展开的二级菜单父项下标（同时只开一个）。 */
  const [sub, setSub] = useState<number | null>(null);
  const [subActive, setSubActive] = useState(-1);
  /**
   * 用户是否在本菜单里动过键盘。焦点环只在动过键盘后画（见 CSS `.ctx[data-kbd] .ctx-i:focus-visible`）：
   * 本组件打开即把焦点**程序化**送进第一个可选项，而 Chromium 对脚本抢焦点沿用上一个焦点元素的
   * `:focus-visible` 判定——从终端右键时上一个焦点元素是 xterm 的输入框（天然命中），于是鼠标
   * 右键一开菜单，第一项就凭空出现一圈绿边，看着像被选中了。
   */
  const [kbd, setKbd] = useState(false);

  const idOf = (tag: string | number) => `${uid}-${tag}`;

  // 用 layout effect：在浏览器绘制前完成翻转，用户看不到菜单跳动
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const left = x + width > window.innerWidth ? Math.max(4, window.innerWidth - width - 4) : x;
    const top = y + height > window.innerHeight ? Math.max(4, window.innerHeight - height - 4) : y;
    if (left !== x || top !== y) setPos({ left, top });
  }, [x, y]);

  // 打开即把焦点放进第一个可选项（否则键盘用户只能 Tab 跳出到背后的 DOM）
  useLayoutEffect(() => {
    const first = usable[0];
    if (first !== undefined) itemRefs.current[first]?.focus();
    else ref.current?.focus();
  }, []);

  useEffect(() => {
    const down = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    // 打开菜单的那次 mousedown 发生在监听挂上之前，因此不会立刻把自己关掉
    window.addEventListener("mousedown", down);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("mousedown", down);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose]);

  // Esc 走弹层栈：菜单总在对话框之后挂载，故位于栈顶——先关菜单、再关背后的窗，
  // 不再是旧写法「一次 Esc 把菜单和背后对话框一起关掉」。二级菜单展开时先收它。
  useEscClose(() => {
    if (sub === null) {
      onClose();
      return;
    }
    const at = sub;
    setSub(null);
    itemRefs.current[at]?.focus();
  });

  // 飞出后把焦点送进第一个子项（键盘 → 展开与鼠标悬停展开都走这里）
  useEffect(() => {
    if (sub === null) return;
    const first = enabledIdx(items[sub]?.children)[0];
    setSubActive(first ?? -1);
    if (first !== undefined) subRefs.current[first]?.focus();
  }, [sub, items]);

  const focusTop = (i: number) => {
    setActive(i);
    setSub(null);
    itemRefs.current[i]?.focus();
  };

  const moveTop = (delta: number) => {
    if (!usable.length) return;
    const at = usable.indexOf(active);
    focusTop(at < 0 ? usable[0] : usable[(at + delta + usable.length) % usable.length]);
  };

  const moveSub = (delta: number) => {
    const kids = enabledIdx(sub === null ? undefined : items[sub]?.children);
    if (!kids.length) return;
    const at = kids.indexOf(subActive);
    const next = at < 0 ? kids[0] : kids[(at + delta + kids.length) % kids.length];
    setSubActive(next);
    subRefs.current[next]?.focus();
  };

  const jumpSub = (toFirst: boolean) => {
    const kids = enabledIdx(sub === null ? undefined : items[sub]?.children);
    const target = toFirst ? kids[0] : kids[kids.length - 1];
    if (target === undefined) return;
    setSubActive(target);
    subRefs.current[target]?.focus();
  };

  const onKeyDown = (e: RKeyboardEvent) => {
    // 漫游类按键才点亮焦点环：鼠标打开的那一次不算（见 `kbd` 注释）
    if (e.key.startsWith("Arrow") || e.key === "Home" || e.key === "End") setKbd(true);
    const inSub = sub !== null && enabledIdx(items[sub]?.children).length > 0;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const delta = e.key === "ArrowDown" ? 1 : -1;
      if (inSub) moveSub(delta);
      else moveTop(delta);
      return;
    }
    if (e.key === "Home" || e.key === "End") {
      e.preventDefault();
      const toFirst = e.key === "Home";
      if (inSub) jumpSub(toFirst);
      else {
        const target = toFirst ? usable[0] : usable[usable.length - 1];
        if (target !== undefined) focusTop(target);
      }
      return;
    }
    if (e.key === "ArrowRight" && sub !== null) {
      e.preventDefault();
      return; // 已展开：子项内没有更深一层，吞掉避免滚出菜单
    }
    if (e.key === "ArrowLeft") {
      if (sub !== null) {
        e.preventDefault();
        const at = sub;
        setSub(null);
        itemRefs.current[at]?.focus();
      }
      return;
    }
    // Tab 不用于在菜单里漫游（原生菜单一致）：离开即收起，免得焦点掉进背后的页面
    if (e.key === "Tab") onClose();
  };

  return (
    <div
      className={`ctx${className ? ` ${className}` : ""}`}
      data-kbd={kbd || undefined}
      ref={ref}
      style={pos}
      role="menu"
      tabIndex={-1}
      onKeyDown={onKeyDown}
    >
      {items.map((it, i) => {
        const kids = it.children;
        const hasKids = !!kids?.length;
        return (
          <div key={i} className={hasKids ? "ctx-sub" : undefined} onMouseLeave={hasKids ? () => setSub(null) : undefined}>
            {it.sep && <div className="ctx-sep" />}
            <button
              type="button"
              id={idOf(i)}
              role="menuitem"
              ref={(el) => {
                itemRefs.current[i] = el;
              }}
              className={`ctx-i${it.danger ? " danger" : ""}${hasKids ? " ctx-parent" : ""}`}
              disabled={it.disabled}
              aria-haspopup={hasKids ? true : undefined}
              aria-expanded={hasKids ? sub === i : undefined}
              onMouseEnter={() => {
                if (it.disabled) return;
                setActive(i);
                if (hasKids) setSub(i);
              }}
              onFocus={() => setActive(i)}
              onClick={() => {
                if (hasKids) {
                  setSub(sub === i ? null : i);
                  return;
                }
                onClose();
                it.onClick();
              }}
            >
              <span className="ctx-t">{it.label}</span>
              {hasKids && (
                <span className="ctx-caret">
                  <Chevron dir="right" />
                </span>
              )}
            </button>
            {hasKids && sub === i && (
              <div className="ctx-flyout" role="menu" aria-label={typeof it.label === "string" ? it.label : undefined}>
                {kids!.map((c, j) => (
                  <div key={j}>
                    {c.sep && <div className="ctx-sep" />}
                    <button
                      type="button"
                      id={idOf(`s${i}-${j}`)}
                      role="menuitem"
                      ref={(el) => {
                        subRefs.current[j] = el;
                      }}
                      className={`ctx-i${c.danger ? " danger" : ""}`}
                      disabled={c.disabled}
                      onMouseEnter={() => !c.disabled && setSubActive(j)}
                      onFocus={() => setSubActive(j)}
                      onClick={() => {
                        onClose();
                        c.onClick();
                      }}
                    >
                      <span className="ctx-t">{c.label}</span>
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
