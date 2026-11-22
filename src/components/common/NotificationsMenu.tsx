// 顶栏通知历史（铃铛）：toast 右上角瞬时消失，报错却需要留痕回看——这里列出内存里的历史
// （上限 100 条，应用重启即清；不落盘，报错文本可能含主机名/路径等敏感信息）。
// 下拉面板沿用 Popconfirm/EditableSelect 的约定：portal 挂 body、z:1000 浮于弹层之上、
// 点外部/Esc 关闭、绘制前按视口边缘夹位置。
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { toast, toastStore, useStore } from "../../stores";
import { useI18n } from "../../lib/i18n";
import { KindIcon } from "./Toasts";

/** 面板固定宽（px）；窄屏由定位逻辑夹进视口。 */
const PANEL_W = 340;

export default function NotificationsMenu() {
  const t = useI18n();
  const { history, readAt } = useStore(toastStore);
  const [open, setOpen] = useState(false);
  const btnRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: 0, top: 0 });

  const unread = history.reduce((n, h) => (h.at > readAt ? n + 1 : n), 0);

  function toggle() {
    setOpen((o) => {
      const next = !o;
      // 打开即清未读：面板里全是「已看过」的
      if (next) toast.markRead();
      return next;
    });
  }

  // 锚在按钮下方右对齐；下方放不下贴底（首帧未量到高度按 240 兜底，翻不了大车）
  useLayoutEffect(() => {
    if (!open) return;
    const r = btnRef.current?.getBoundingClientRect();
    if (!r) return;
    const h = panelRef.current?.getBoundingClientRect().height || 240;
    const left = Math.max(8, Math.min(r.right - PANEL_W, window.innerWidth - PANEL_W - 8));
    let top = r.bottom + 4;
    if (top + h > window.innerHeight - 8) top = Math.max(44, window.innerHeight - h - 8);
    setPos({ left, top });
  }, [open, history.length]);

  useEffect(() => {
    if (!open) return;
    const down = (e: MouseEvent) => {
      const el = e.target as Node;
      // 铃铛自身放行：它的 click 负责收起，别被这里的 mousedown 抢先后又开回去
      if (panelRef.current?.contains(el) || btnRef.current?.contains(el)) return;
      setOpen(false);
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", down);
    window.addEventListener("keydown", key);
    return () => {
      window.removeEventListener("mousedown", down);
      window.removeEventListener("keydown", key);
    };
  }, [open]);

  const fmt = (at: number) =>
    new Date(at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });

  return (
    <>
      <button
        ref={btnRef}
        className="mem-toggle notif-btn"
        data-tauri-drag-region="false"
        data-on={open}
        title={t("notify.title")}
        onClick={toggle}
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
          <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
          <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
        </svg>
        {unread > 0 && <span className="notif-badge">{unread > 99 ? "99+" : unread}</span>}
      </button>
      {open &&
        createPortal(
          <div
            className="notif-panel"
            ref={panelRef}
            style={{ left: pos.left, top: pos.top, width: PANEL_W }}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="notif-head">
              <span>{t("notify.title")}</span>
              <button type="button" className="notif-clear" onClick={() => toast.clearHistory()} disabled={history.length === 0}>
                {t("notify.clear")}
              </button>
            </div>
            <div className="notif-list">
              {history.length === 0 && <div className="notif-empty">{t("notify.empty")}</div>}
              {[...history].reverse().map((h) => (
                <div key={h.id} className={`notif-item ${h.kind}`}>
                  <KindIcon kind={h.kind} />
                  {/* 正文占满整行，来源与时间收成下方小字，避免长报错被挤成窄条 */}
                  <div className="ni-body">
                    <span className="ni-msg">{h.message}</span>
                    <div className="ni-meta">
                      {h.source && <span className="ni-src">{h.source}</span>}
                      <span className="ni-time">{fmt(h.at)}</span>
                    </div>
                  </div>
                </div>
              ))}
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}
