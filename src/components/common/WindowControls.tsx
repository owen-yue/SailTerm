import { useEffect, useState } from "react";
import { selfWindow } from "../../lib/tauri/window";
import { useI18n } from "../../lib/i18n";
import { isMacOS } from "../../lib/platform";

/** Windows 11 风格细线图标（与系统标题栏观感一致）。 */
function WinGlyph({ kind }: { kind: "min" | "max" | "restore" | "close" }) {
  if (kind === "min") {
    return (
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
        <path d="M1 5h8" stroke="currentColor" strokeWidth="1" />
      </svg>
    );
  }
  if (kind === "max") {
    return (
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
        <rect x="1.5" y="1.5" width="7" height="7" fill="none" stroke="currentColor" strokeWidth="1" />
      </svg>
    );
  }
  if (kind === "restore") {
    return (
      <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
        <rect x="1.5" y="2.5" width="5" height="5" fill="none" stroke="currentColor" strokeWidth="1" />
        <path d="M3.5 2.5V1.5h5v5H7.5" fill="none" stroke="currentColor" strokeWidth="1" />
      </svg>
    );
  }
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
      <path d="M2 2l6 6M8 2L2 8" stroke="currentColor" strokeWidth="1" />
    </svg>
  );
}

/**
 * 自绘窗口控制钮（仅 Windows / Linux）：右上角线框钮。
 * macOS 走系统原生红绿灯，本组件在 mac 下返回 null。
 * 拖拽区在顶栏，按钮自身不带 drag-region，点击可正常触发。
 */
export default function WindowControls() {
  const t = useI18n();
  const mac = isMacOS();
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    void selfWindow.isMaximized().then(setMaximized).catch(() => {});
    const un = selfWindow.onResized(() => {
      void selfWindow.isMaximized().then(setMaximized).catch(() => {});
    });
    return () => {
      void un.then((f) => f());
    };
  }, []);

  const onMin = () => selfWindow.minimize().catch(console.error);
  const onMax = () => selfWindow.toggleMaximize().catch(console.error);
  const onClose = () => selfWindow.close().catch(console.error);

  // macOS 用系统原生红绿灯（tauri.macos.conf.json 开 Overlay 标题栏），不再自绘。
  if (mac) return null;

  return (
    <div className="win-controls" data-tauri-drag-region="false">
      <button type="button" className="win-btn" title={t("win.min")} onClick={onMin}>
        <WinGlyph kind="min" />
      </button>
      <button type="button" className="win-btn" title={maximized ? t("win.restore") : t("win.max")} onClick={onMax}>
        <WinGlyph kind={maximized ? "restore" : "max"} />
      </button>
      <button type="button" className="win-btn win-close" title={t("win.close")} onClick={onClose}>
        <WinGlyph kind="close" />
      </button>
    </div>
  );
}
