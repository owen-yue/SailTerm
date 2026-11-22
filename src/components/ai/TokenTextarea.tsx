import {
  forwardRef,
  useCallback,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import type { CSSProperties, KeyboardEvent } from "react";

/**
 * 通用「令牌补全」多行输入框（可复用）。
 *
 * 在任意 `<textarea>` 场景里，当用户敲开触发串（默认 `{{`）时，弹出一个跟随光标的
 * 候选下拉，支持 ↑/↓ 选择、Enter/Tab 补全、Esc 关闭、鼠标点击；选中后把
 * `触发串 + 前缀` 补成 `触发串 + key + 结束串`（默认 `{{role}}`）。
 *
 * 与业务解耦：候选项（含说明文案）由调用方以 `tokens` 传入，组件本身不含任何 i18n，
 * 因此提示词模板编辑、命令/提及补全等任何地方都能直接复用。
 * 另通过 ref 暴露 `insertToken(key)`，让「点击芯片在光标处插入」与内联补全共用同一套逻辑。
 */
export interface TokenDef {
  /** 补全标识：用户敲 `{{` 后按此前缀过滤，选中后补成 `{{key}}`（不含触发/结束串）。 */
  key: string;
  /** 候选项右侧说明，可空。 */
  desc?: string;
}

export interface TokenTextareaHandle {
  /** 在当前光标处插入一整个令牌（`open + key + close`），供宏芯片点击复用。 */
  insertToken: (key: string) => void;
  focus: () => void;
}

interface Props {
  value: string;
  onChange: (next: string) => void;
  tokens: TokenDef[];
  /** 触发串，默认 `{{`。 */
  open?: string;
  /** 结束串，默认 `}}`；置空则只插入 key。 */
  close?: string;
  placeholder?: string;
  className?: string;
  style?: CSSProperties;
  disabled?: boolean;
  rows?: number;
  spellCheck?: boolean;
}

interface Menu {
  items: TokenDef[];
  /** 待替换区间 [from, to)：from 是触发串起点，to 是当前光标。 */
  from: number;
  to: number;
  top: number;
  left: number;
  up: boolean;
}

// 把 mirror 计算所需样式从 textarea 拷到隐藏 div，量出光标在内容坐标系里的位置。
const MIRROR_PROPS = [
  "fontFamily", "fontStyle", "fontWeight", "fontSize", "lineHeight", "letterSpacing",
  "textTransform", "wordSpacing", "paddingTop", "paddingRight", "paddingBottom",
  "paddingLeft", "borderTopWidth", "borderRightWidth", "borderBottomWidth",
  "borderLeftWidth", "boxSizing", "tabSize",
] as const;

function measureCaret(ta: HTMLTextAreaElement, pos: number) {
  const cs = getComputedStyle(ta);
  const mirror = document.createElement("div");
  for (const p of MIRROR_PROPS) mirror.style[p] = cs[p as any];
  Object.assign(mirror.style, {
    position: "absolute",
    top: "0",
    left: "-99999px",
    whiteSpace: "pre-wrap",
    wordWrap: "break-word",
    overflowWrap: "break-word",
    width: `${ta.clientWidth}px`,
  } as CSSProperties);
  mirror.textContent = ta.value.slice(0, pos);
  const marker = document.createElement("span");
  marker.textContent = "\u200b"; // 零宽占位，量它的偏移即光标位置
  mirror.appendChild(marker);
  mirror.appendChild(document.createTextNode(ta.value.slice(pos) || "\u200b"));
  document.body.appendChild(mirror);
  const top = marker.offsetTop - ta.scrollTop;
  const left = marker.offsetLeft - ta.scrollLeft;
  const lh = parseFloat(cs.lineHeight) || parseFloat(cs.fontSize) || 14;
  document.body.removeChild(mirror);
  return { top, left, lh };
}

const TokenTextarea = forwardRef<TokenTextareaHandle, Props>(function TokenTextarea(
  { value, onChange, tokens, open = "{{", close = "}}", placeholder, className, style, disabled, rows, spellCheck },
  ref,
) {
  const taRef = useRef<HTMLTextAreaElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const pendingCaret = useRef<number | null>(null);
  const [menu, setMenu] = useState<Menu | null>(null);
  const [active, setActive] = useState(0);

  // 受控 value 更新后把光标恢复到 we 设定的位置（补全/插入都需要）。
  useLayoutEffect(() => {
    if (pendingCaret.current != null && taRef.current) {
      const p = pendingCaret.current;
      pendingCaret.current = null;
      const ta = taRef.current;
      ta.selectionStart = ta.selectionEnd = p;
    }
  }, [value]);

  const recompute = useCallback(() => {
    const ta = taRef.current;
    if (!ta || disabled) return setMenu(null);
    const caret = ta.selectionStart;
    const before = ta.value.slice(0, caret);
    const idx = before.lastIndexOf(open);
    if (idx < 0) return setMenu(null);
    const partial = before.slice(idx + open.length);
    // 已含结束串/换行/空白 → 视为令牌已收尾或用户已移开，收起菜单
    if (partial.includes(close) || /[\s{}]/.test(partial)) return setMenu(null);
    const prefix = partial.toLowerCase();
    const items = tokens.filter((t) => t.key.toLowerCase().startsWith(prefix));
    if (items.length === 0) return setMenu(null);
    const { top, left, lh } = measureCaret(ta, caret);
    const wrapH = wrapRef.current?.clientHeight ?? ta.clientHeight;
    const estH = Math.min(items.length, 8) * 30 + 8;
    const up = top + lh + estH > wrapH && top > estH; // 底部放不下且上方够空间则向上翻
    setMenu({ items, from: idx, to: caret, top: up ? top : top + lh + 2, left, up });
    setActive(0);
  }, [close, disabled, open, tokens]);

  const commit = useCallback(
    (item: TokenDef) => {
      const ta = taRef.current;
      if (!ta || !menu) return;
      const insert = open + item.key + close;
      const next = ta.value.slice(0, menu.from) + insert + ta.value.slice(menu.to);
      pendingCaret.current = menu.from + insert.length;
      onChange(next);
      setMenu(null);
    },
    [close, menu, onChange, open],
  );

  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (!menu) return;
    const n = menu.items.length;
    if (e.key === "ArrowDown") { e.preventDefault(); setActive((a) => (a + 1) % n); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setActive((a) => (a - 1 + n) % n); }
    else if (e.key === "Enter" || e.key === "Tab") { e.preventDefault(); commit(menu.items[active]); }
    else if (e.key === "Escape") { e.preventDefault(); setMenu(null); }
  };

  useImperativeHandle(ref, () => ({
    focus: () => taRef.current?.focus(),
    insertToken: (key: string) => {
      const ta = taRef.current;
      if (!ta) return;
      const caret = ta.selectionStart;
      const insert = open + key + close;
      const next = ta.value.slice(0, caret) + insert + ta.value.slice(caret);
      pendingCaret.current = caret + insert.length;
      onChange(next);
      requestAnimationFrame(() => taRef.current?.focus());
    },
  }));

  return (
    <div className="tt-wrap" ref={wrapRef} style={{ position: "relative" }}>
      <textarea
        ref={taRef}
        className={className}
        style={style}
        value={value}
        placeholder={placeholder}
        disabled={disabled}
        rows={rows}
        spellCheck={spellCheck}
        onChange={(e) => { onChange(e.target.value); recompute(); }}
        onKeyDown={onKeyDown}
        onClick={() => setMenu(null)}
        onBlur={() => setMenu(null)}
        onScroll={() => setMenu(null)}
      />
      {menu && (
        <div
          className="tt-menu"
          style={{
            top: menu.up ? undefined : menu.top,
            bottom: menu.up ? `calc(100% - ${menu.top}px)` : undefined,
            left: menu.left,
          }}
          onMouseDown={(e) => e.preventDefault() /* 保持 textarea 焦点 */}
        >
          {menu.items.map((it, i) => (
            <button
              type="button"
              key={it.key}
              className="tt-item"
              data-on={i === active ? "true" : "false"}
              onMouseEnter={() => setActive(i)}
              onClick={() => commit(it)}
            >
              <span className="tt-item-key">{open + it.key + close}</span>
              {it.desc && <span className="tt-item-desc">{it.desc}</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
});

export default TokenTextarea;
