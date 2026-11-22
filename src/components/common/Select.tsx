import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

export interface SelectOption {
  value: string;
  label: string;
}

interface Props {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  /** 触发器额外类名（用于宽度/外观微调，如 ai-mode-pick / ai-model-pick）。 */
  className?: string;
  /** 前导图标（如模式切换的 ∞ / 💬）。 */
  icon?: ReactNode;
  disabled?: boolean;
  title?: string;
  /** 无选中项时的占位文字。 */
  placeholder?: string;
  /**
   * 行内动作（如模型行的「编辑」铅笔）：提供后每个选项悬停时在右侧淡入一枚图标按钮。
   * 点击只回调该行的 value、不改变当前选中（也不触发 onChange）。
   */
  optionAction?: {
    icon: ReactNode;
    title: string;
    onSelect: (value: string) => void;
  };
  /** 弹层底部的分隔动作项（如「添加模型」）：点击回调、不改变选中值。 */
  footerAction?: {
    label: ReactNode;
    onSelect: () => void;
  };
}

/**
 * 统一的品牌化下拉框（取代原生 `<select>`）。
 *
 * 原生 select 的弹出列表由系统绘制、CSS 够不着，暗色主题下会弹出与整体割裂的灰白菜单
 *这里用 portal 把弹层挂到 body、按触发器矩形 fixed 定位：既不受任何祖先
 * overflow 裁切，又能完全走主题变量（圆角 / --bg-2 / 品牌高亮）。空间不足时自动向上弹。
 */
export default function Select({
  value,
  options,
  onChange,
  className = "",
  icon,
  disabled,
  title,
  placeholder,
  optionAction,
  footerAction,
}: Props) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const [rect, setRect] = useState<DOMRect | null>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const current = options.find((o) => o.value === value);

  const place = () => {
    if (btnRef.current) setRect(btnRef.current.getBoundingClientRect());
  };

  // 打开时定位 + 把高亮项对齐到当前值
  useLayoutEffect(() => {
    if (!open) return;
    place();
    const idx = options.findIndex((o) => o.value === value);
    setActive(idx < 0 ? 0 : idx);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // 外点关闭 + 滚动/缩放时重定位
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (btnRef.current?.contains(t) || listRef.current?.contains(t)) return;
      setOpen(false);
    };
    const reflow = () => place();
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("resize", reflow);
    window.addEventListener("scroll", reflow, true);
    return () => {
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("resize", reflow);
      window.removeEventListener("scroll", reflow, true);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const pick = (v: string) => {
    onChange(v);
    setOpen(false);
  };

  const onKeyDown = (e: ReactKeyboardEvent) => {
    if (disabled) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (!open) setOpen(true);
      else if (options[active]) pick(options[active].value);
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!open) setOpen(true);
      else setActive((a) => Math.min(options.length - 1, a + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => Math.max(0, a - 1));
    } else if (e.key === "Escape") {
      setOpen(false);
    }
  };

  // 弹层几何：下方空间不足且上方更宽裕时向上弹（底部分隔动作项额外占一行）
  const menuH = Math.min((options.length + (footerAction ? 1 : 0)) * 30 + 12, 260);
  const spaceBelow = rect ? window.innerHeight - rect.bottom : 0;
  const openUp = !!rect && spaceBelow < menuH + 8 && rect.top > spaceBelow;
  const style: CSSProperties = rect
    ? {
        position: "fixed",
        left: rect.left,
        minWidth: rect.width,
        top: openUp ? undefined : rect.bottom + 4,
        bottom: openUp ? window.innerHeight - rect.top + 4 : undefined,
      }
    : {};

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        className={`ui-select ${open ? "open" : ""} ${className}`}
        disabled={disabled}
        title={title}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => !disabled && setOpen((v) => !v)}
        onKeyDown={onKeyDown}
      >
        {icon && <span className="ui-select-ico" aria-hidden="true">{icon}</span>}
        <span className="ui-select-label">{current ? current.label : placeholder}</span>
        <span className="ui-select-caret" aria-hidden="true">▾</span>
      </button>
      {open &&
        rect &&
        createPortal(
          <div ref={listRef} className="ui-select-menu" role="listbox" style={style}>
            {options.map((o, i) => (
              <div
                key={o.value}
                role="option"
                aria-selected={o.value === value}
                className={`ui-select-opt ${o.value === value ? "sel" : ""} ${i === active ? "act" : ""}`}
                onMouseEnter={() => setActive(i)}
                onClick={() => pick(o.value)}
              >
                <span className="ui-select-opt-label">{o.label}</span>
                {optionAction && (
                  <button
                    type="button"
                    className="ui-select-opt-act"
                    title={optionAction.title}
                    aria-label={optionAction.title}
                    onClick={(e) => {
                      // 行内动作不冒泡到整行的「选中」：只对该项发起编辑，不改当前选择。
                      e.stopPropagation();
                      optionAction.onSelect(o.value);
                      setOpen(false);
                    }}
                  >
                    {optionAction.icon}
                  </button>
                )}
              </div>
            ))}
            {footerAction && (
              <div
                role="button"
                className="ui-select-foot"
                onClick={() => {
                  footerAction.onSelect();
                  setOpen(false);
                }}
              >
                {footerAction.label}
              </div>
            )}
          </div>,
          document.body,
        )}
    </>
  );
}
