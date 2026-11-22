// 录制回放视图：用只读 xterm.js 终端渲染（与控制台同款控件），字体/主题/选区/滚动观感一致。
// 面板与工具窗口两处回放共用，避免各自漏接右键守卫。落盘是 LineScreen 还原后的纯文本（不含
// ANSI 颜色），故此处只做「控件级」还原：终端观感统一，但不复现颜色。
import { useEffect, useRef, useState } from "react";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";
import ContextMenu from "./ContextMenu";
import { useI18n } from "../../lib/i18n";
import { appearanceStore, terminalStore, uiPrefsStore } from "../../stores";
import { resolveTheme } from "../../lib/themes";

interface Props {
  /** 真实回放文本（复制全部取它，不含占位符）。 */
  text: string;
  /** text 为空时展示的占位文案。 */
  empty?: string;
  /** 追加到容器的类名（rp-play-body / rec-play）。 */
  className?: string;
}

export default function RecBody({ text, empty, className }: Props) {
  const tr = useI18n();
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const [ctx, setCtx] = useState<{ x: number; y: number; sel: string } | null>(null);

  useEffect(() => {
    const el = hostRef.current;
    if (!el || !text) return;

    // 创建时读一次当前外观/终端偏好：回放视图随 text 变化重建（仅切文件时触发，代价可接受）。
    const appearance = appearanceStore.get();
    const termPrefs = terminalStore.get();
    const prefs = uiPrefsStore.get();
    const term = new XTerm({
      // 只读回放：屏蔽键盘输入，避免误操作被当成命令；下划线光标不遮挡末字符。
      disableStdin: true,
      cursorBlink: false,
      cursorStyle: "underline",
      fontSize: appearance.fontSize,
      fontFamily: appearance.fontFamily,
      lineHeight: prefs.lineHeight,
      letterSpacing: prefs.letterSpacing,
      theme: resolveTheme(appearance.theme, appearance.customTheme),
      // 一次性写入的静态文本：scrollback 取「设置值」与「文本行数」的较大者，保证全文可回滚查看。
      scrollback: Math.max(termPrefs.scrollback, text.split("\n").length + 16),
      // 纯文本按 \n 换行即回到行首。
      convertEol: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();
    term.write(text);
    termRef.current = term;

    // 容器尺寸变化（拖宽窄栏、窗口缩放）时重算行列。
    const ro = new ResizeObserver(() => fit.fit());
    ro.observe(el);
    return () => {
      ro.disconnect();
      term.dispose();
      termRef.current = null;
    };
  }, [text]);

  if (!text) {
    return <div className={`rec-body${className ? ` ${className}` : ""}`}>{empty}</div>;
  }

  return (
    <>
      <div
        ref={hostRef}
        className={`rec-body rec-term${className ? ` ${className}` : ""}`}
        onContextMenu={(e) => {
          // 右键：快照当前终端选区（菜单遮罩会清选区，故此刻一次性取）。
          e.preventDefault();
          setCtx({ x: e.clientX, y: e.clientY, sel: termRef.current?.getSelection() ?? "" });
        }}
      />
      {ctx && (
        <ContextMenu
          x={ctx.x}
          y={ctx.y}
          onClose={() => setCtx(null)}
          items={[
            {
              label: tr("common.copy"),
              // 无选区时置灰而非消失（与终端右键菜单同约定）。
              disabled: ctx.sel.trim() === "",
              onClick: () => void navigator.clipboard.writeText(ctx.sel),
            },
            {
              label: tr("rec.copyAll"),
              onClick: () => void navigator.clipboard.writeText(text),
            },
          ]}
        />
      )}
    </>
  );
}
