import { useEffect, useState } from "react";
import {
  aiStore,
  AI_QUEUE_KEYS,
  awaitingTracks,
  dockActions,
  dockStore,
  panel,
  recordActions,
  recordStore,
  sessionActions,
  sessionStore,
  connectionStore,
  useStore,
  useStorePick,
} from "../../stores";
import { focusSessionWindow, popTabWindow } from "../../lib/windows";
import { DOCK_ALL, KIND_LABEL } from "../../types";
import { useSessionName } from "../../lib/sessionName";
import { kindHasTerminal } from "../../lib/capabilities";
import { DOCK_ICONS, DOCK_META, visiblePanels } from "./dockRegistry";
import { useI18n } from "../../lib/i18n";
import { useEscClose, useMaskDismiss } from "../../lib/useOverlay";
import ContextMenu from "../common/ContextMenu";
import StatusDot, { statusLabel } from "../common/StatusDot";
import type { Tab } from "../../stores";

/** 页签状态点取哪个态：phase 比 state 更准（连接中 / 刚失败）。 */
const dotState = (t: Tab): string =>
  t.phase === "pending" ? "connecting" : t.phase === "failed" ? "disconnected" : t.state;

/** 首页页签的房子线稿图标（与下栏页签图标同一视觉语言）。 */
const HOME_ICO = (
  <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" width="13" height="13">
    <path d="M2.5 7.5 8 2.5l5.5 5M4 6.8V13h8V6.8M6.5 13v-3.5h3V13" />
  </svg>
);

/** 「停靠面板」入口图标：一个窗口 + 底部停靠带，代表底栏。 */
const MANAGE_ICO = (
  <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" width="14" height="14">
    <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
    <path d="M2 10h12" />
  </svg>
);

/** 专注（放大）模式图标：四角向外箭头 = 进入；四角向内箭头 = 退出还原。 */
const FOCUS_ICO = (
  <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" width="14" height="14">
    <path d="M2 6V2h4" />
    <path d="M14 6V2h-4" />
    <path d="M14 10v4h-4" />
    <path d="M2 10v4h4" />
  </svg>
);
const FOCUS_EXIT_ICO = (
  <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round" width="14" height="14">
    <path d="M6 2v4H2" />
    <path d="M10 2v4h4" />
    <path d="M10 14v-4h4" />
    <path d="M6 14v-4H2" />
  </svg>
);

interface Props {
  /** 首页是否正在展示（高亮首页页签） */
  homeActive?: boolean;
  /** 点首页页签 */
  onHome?: () => void;
  /** 点普通会话页签（用于退出首页态） */
  onNavigate?: () => void;
  /** 终端专注模式是否启用（行尾按钮高亮并切成退出图标） */
  focusMode?: boolean;
  /** 切换专注模式 */
  onToggleFocus?: () => void;
}

/** 主区顶部会话标签页：切换 / 断线重连 / 关闭；拖到 pane 四边即分屏（无分屏按钮）。
 * 右键弹出菜单（独立窗口 / 关闭）；已弹出的标签带 ⧉ 角标，点击改为聚焦那个窗口。
 * 行首固定「首页」页签：随时可回到 Welcome 引导页（终端不销毁）。 */
export default function TabBar({ homeActive = false, onHome, onNavigate, focusMode = false, onToggleFocus }: Props) {
  const tr = useI18n();
  // 页签名走统一入口：同主机多条会话时与发送工具给出同一个 `#n`（否则两处名字对不上，用户无法确定是哪一页）。
  const name = useSessionName();
  const { tabs, activeId, busy, popped } = useStore(sessionStore);
  const { recordingIds } = useStore(recordStore);
  const { profiles } = useStore(connectionStore);
  // 待你回执（AI 澄清反问 / 命令审批）的会话点亮角标：卡片只渲在发起它的那个 surface，
  // 切到别的标签时没有任何提示就等于整轮挂住，这里标出“哪条会话在等你”。
  // 只选两字段（不订阅整个 aiStore）：流式输出逐 token 写 turns，不该带着页签栏重渲染。
  const { approvals, questions } = useStorePick(aiStore, AI_QUEUE_KEYS);
  const dock = useStore(dockStore);
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  const [manage, setManage] = useState<{ x: number; y: number } | null>(null);
  // 窗口失焦收起菜单（点他处由 ContextMenu 自己处理）
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("blur", close);
    };
  }, [menu]);
  // 「停靠面板」下拉不是右键菜单（它由按钮开合、带图标与分区标签），但它此前压根没有 Esc：
  // 打开后只能再点一次按钮或去点遮罩，键盘用户没有退路。
  useEscClose(() => setManage(null), manage !== null);
  // 首页签常驻：即使 0 会话也保留「首页」页签，全部关闭后不会连标签栏带首页一起消失
  const menuTab = menu ? tabs.find((t) => t.id === menu.id) : null;
  // 本会话有没有等你回执的卡（AI 澄清反问 / 命令审批）。口径统一走 aiStore.awaitingTracks，
  // 这里只问「有没有」、不关心在哪一轨（那是子页签负责的），页签明灭与悬浮提示两处共用。
  const isAwaiting = (id: string) => awaitingTracks(approvals, questions, id).size > 0;

  return (
    <div className="tabbar">
      {onHome && (
        <div
          className={`tab tab-home${homeActive ? " active" : ""}`}
          title={tr("tabs.home")}
          onClick={onHome}
        >
          <span className="tab-home-ico">{HOME_ICO}</span>
          <span className="tl">{tr("tabs.home")}</span>
        </div>
      )}
      {tabs.map((t) => (
        <div
          key={t.id}
          className={`tab${!homeActive && t.id === activeId ? " active" : ""}${
            isAwaiting(t.id) ? " awaiting" : ""
          }`}
          data-state={t.state}
          data-phase={t.phase}
          data-popped={!!popped[t.id]}
          title={
            // 状态开在首位：圆点自身不再挂 tooltip（否则会盖掉下面的失败原因/备注）。
            `${statusLabel(tr, dotState(t))} · ` +
            (popped[t.id]
              ? `${name(t)}${tr("tabs.poppedSuffix")}`
              : t.phase === "failed"
                ? t.failMessage || name(t)
                : t.reason || name(t)) +
            // 页签在轻微明灭时，悬浮要说清为什么闪（不然会被当成连接抖动）
            (isAwaiting(t.id) ? ` · ${tr("ai.awaitTip")}` : "")
          }
          draggable
          onDragStart={(e) => {
            e.dataTransfer.setData("text/session-id", t.id);
            e.dataTransfer.effectAllowed = "move";
          }}
          onClick={() => {
            onNavigate?.();
            if (popped[t.id]) void focusSessionWindow(t.id);
            else sessionActions.select(t.id);
          }}
          onContextMenu={(e) => {
            e.preventDefault();
            setMenu({ id: t.id, x: e.clientX, y: e.clientY });
          }}
        >
          <StatusDot state={dotState(t)} tooltip={false} />
          {/* 录制中：红色小圆点闪烁 */}
          {recordingIds.includes(t.id) && (
            <span className="tab-rec-dot" title={tr("tx.rec.stop")} />
          )}
          {/* 弹出独立窗口的会话：⧉ 角标，点击 = 聚焦那个窗口（不 select） */}
          {popped[t.id] && (
            <em className="kind" title={tr("tabs.poppedTitle")}>
              ⧉
            </em>
          )}
          {/* 无终端的会话（FTP/FTPS）给协议角标 */}
          {!kindHasTerminal(t.kind) && (
            <em className="kind" title={tr(KIND_LABEL[t.kind])}>
              {tr(KIND_LABEL[t.kind])}
            </em>
          )}
          <span className="tl">{name(t)}</span>
          {typeof t.latencyMs === "number" && (
            <em
              className="rtt"
              title="TCP RTT"
              data-slow={t.latencyMs > 200 ? "true" : "false"}
            >
              {t.latencyMs}ms
            </em>
          )}
          {t.state === "disconnected" && !t.phase && (
            <button
              className="tab-act"
              disabled={busy === t.id}
              onClick={(e) => {
                e.stopPropagation();
                void sessionActions.reconnect(t.id);
              }}
            >
              {busy === t.id ? "…" : tr("tabs.reconnect")}
            </button>
          )}
          <span
            className="x"
            title={tr("tabs.close")}
            onClick={(e) => {
              e.stopPropagation();
              void sessionActions.close(t.id);
            }}
          >
            ×
          </span>
        </div>
      ))}
      {/* 行尾控制区：弹性占位把后面的按钮顶到右缘，标签再多也不被横向滚动挤走 */}
      <span className="tabbar-spacer" aria-hidden="true" />
      {onToggleFocus && (
        <button
          type="button"
          className="icon-btn tab-focus-btn"
          data-on={focusMode}
          title={tr(focusMode ? "app.focusExit" : "app.focus")}
          onClick={onToggleFocus}
        >
          {focusMode ? FOCUS_EXIT_ICO : FOCUS_ICO}
        </button>
      )}
      {/* 行尾「停靠面板」入口：仅当下栏被拖空时才出现（此时它是把面板唤回下栏的唯一找回入口）；
          下栏有面板时隐藏，避免平时一直杵在标签栏尾部占位。挪面板走拖页签。 */}
      {dock.bottom.length === 0 && (
        <>
          <button
            type="button"
            className="icon-btn tab-manage-btn"
            title={tr("panel.manage")}
            onClick={(e) => {
              const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
              setManage((m) => (m ? null : { x: r.right, y: r.bottom + 4 }));
            }}
          >
            {MANAGE_ICO}
          </button>
          {manage && (
            <>
              <div className="term-ctx-mask" {...useMaskDismiss(() => setManage(null))} />
              <div
                className="term-ctx tab-ctx tab-manage-pop"
                style={{ left: manage.x, top: manage.y }}
                onMouseDown={(e) => e.stopPropagation()}
              >
                {/* 按聚焦会话的能力过滤：FTP 会话不该在管理菜单里看到「文件」（已在主区）
                    与「监控」（无命令通道，采不到）——唤回一个用不上的面板只是噪音。 */}
                {visiblePanels(
                  DOCK_ALL,
                  tabs.find((x) => x.id === activeId)?.kind ?? null,
                ).map((id) => {
                  const zone = dock.right.includes(id) ? "right" : "bottom";
                  return (
                    <button
                      key={id}
                      type="button"
                      className="tab-manage-item"
                      data-in={zone}
                      onClick={() => {
                        if (zone === "right") dockActions.move(id, "bottom");
                        else panel.reveal(id);
                        setManage(null);
                      }}
                    >
                      <span className="tm-ico">{DOCK_ICONS[id]}</span>
                      <span className="tm-label">{tr(DOCK_META[id].label)}</span>
                      <em className="tm-zone">{tr(zone === "right" ? "panel.inRight" : "panel.inBottom")}</em>
                    </button>
                  );
                })}
              </div>
            </>
          )}
        </>
      )}
      {menu && menuTab && (
        <ContextMenu
          className="tab-ctx"
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={[
            {
              label: popped[menuTab.id] ? tr("tabs.focus") : tr("tabs.popout"),
              onClick: () => {
                if (popped[menuTab.id]) void focusSessionWindow(menuTab.id);
                else popTabWindow(menuTab);
              },
            },
            ...(kindHasTerminal(menuTab.kind) && menuTab.state === "connected"
              ? [{
                  label: recordingIds.includes(menuTab.id) ? tr("tx.rec.stop") : tr("tx.rec.start"),
                  onClick: () => {
                    const pid = menuTab.profileId;
                    const dir = pid ? profiles.find((p) => p.id === pid)?.recordDir || null : null;
                    void recordActions.toggle(menuTab.id, dir);
                  },
                }]
              : []),
            {
              label: tr("tabs.close"),
              onClick: () => void sessionActions.close(menuTab.id),
            },
            ...(tabs.filter((t) => !popped[t.id]).length > 1
              ? [
                  {
                    label: tr("tabs.closeOthers"),
                    onClick: () => void sessionActions.closeOthers(menuTab.id),
                  },
                  {
                    label: tr("tabs.closeAll"),
                    onClick: () => void sessionActions.closeAll(),
                  },
                ]
              : []),
          ]}
        />
      )}
    </div>
  );
}
