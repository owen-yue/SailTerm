import { useEffect, useRef } from "react";
import Chevron from "../common/Chevron";
import { DockPanelBody, DOCK_ICONS, DOCK_META, visiblePanels } from "./dockRegistry";
import { beginDockTabDrag } from "../../lib/dragDockTab";
import { type DockPanelId } from "../../types";
import {
  activeCount,
  aiStore,
  AI_QUEUE_KEYS,
  awaitingTracks,
  dockStore,
  panel,
  panelStore,
  sessionStore,
  transferStore,
  useStore,
  useStorePick,
} from "../../stores";
import { useI18n } from "../../lib/i18n";

interface Props {
  /**
   * 固定面板列表（弹出独立窗口用）：不读 dockStore、不可拖拽换位。
   * 停靠布局只属于主窗口；弹出窗口若不固定列表，主窗口把面板拖去右栏后
   * 这个窗口里该面板就凭空消失了。
   */
  pinned?: DockPanelId[];
  /**
   * 隐去「文件」页签（主窗口专用）：聚焦会话正在主区看「终端 | 文件」大视图时，
   * 同一会话不该再摆一份文件树。只影响展示，dockStore 布局不动，收起大视图即回来。
   */
  hideFiles?: boolean;
}

/**
 * 底部停靠栏：面板列表来自 dockStore.bottom（传输/监控/审计 + 从右栏拖来的 AI）。
 *
 * 页签可拖到右侧栏换位、也接受从右栏拖来（拖拽热区 = 整条页签条）。
 * 收起时只留一条薄标签栏，把纵向空间还给终端（「布局灵活性」）；
 * 面板全部拖去右栏后保留薄条作为投放热区——否则「全拖走了就再也拖不回来」。
 */
export default function BottomPanel({ pinned, hideFiles }: Props = {}) {
  const t = useI18n();
  const { open, tab, height } = useStore(panelStore);
  const { bottom: docked } = useStore(dockStore);
  const { tabs, activeId } = useStore(sessionStore);
  const { tasks } = useStore(transferStore);
  // 待你回执的 AI 卡（澄清反问 / 命令审批）：AI 被拖到下栏时同样要亮角标（与右栏同一判据）。
  // 只选两字段：流式输出逐 token 写 turns，不该带着整条下栏重渲染。
  const { approvals, questions } = useStorePick(aiStore, AI_QUEUE_KEYS);
  const active = activeCount(tasks);
  const stopResize = useRef<(() => void) | null>(null);

  /** 聚焦会话：只用来按能力过滤页签（监控/会话面板对 FTP 系无意义，不该占位）。 */
  const activeTab = tabs.find((x) => x.id === activeId) ?? null;
  // 过滤只影响**展示**：dockStore 里的用户布局不动，切回支持的会话页签就回来
  const bottom = visiblePanels(pinned ?? docked, activeTab?.kind ?? null).filter(
    (id) => !hideFiles || id !== "files",
  );

  // 拖到一半组件被卸载时，别把挂在 window 上的监听泄漏出去
  useEffect(() => () => stopResize.current?.(), []);

  const startResize = (e: React.MouseEvent) => {
    e.preventDefault();
    const startY = e.clientY;
    const startH = panelStore.get().height;
    const move = (ev: MouseEvent) => panel.setHeight(startH + (startY - ev.clientY));
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      stopResize.current = null;
    };
    stopResize.current = up;
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  // 脏态防御：激活页可能已被拖去右栏（或 localStorage 存了旧值），回落到下栏第一页；
  // 正停在被隐藏的面板（hideFiles）上则置空——静默过滤不该把用户素不相识的面板弹开
  const cur: DockPanelId | undefined = bottom.includes(tab)
    ? tab
    : tab === "files" && hideFiles
      ? undefined
      : bottom[0];

  // ① 下栏空态：不再常驻一条尴尬的空栏——平时不占位（display:none），仅当正在拖页签
  //    （body.dock-dragging）时由 CSS 浮出这条投放条，把面板拖回来；平时靠标签栏「停靠面板」入口唤回。
  if (!pinned && bottom.length === 0) {
    return (
      <div className="panel panel-empty-slot">
        <div className="panel-tabs" data-drop-zone="bottom">
          <span className="ptab-empty">{t("panel.dropHere")}</span>
        </div>
      </div>
    );
  }

  return (
    <div className="panel" data-open={open} style={open ? { height } : undefined}>
      {open && <div className="panel-grip" title={t("panel.grip")} onMouseDown={startResize} />}

      <div className="panel-tabs" data-drop-zone={pinned ? undefined : "bottom"}>
        {bottom.length === 0 ? (
          <span className="ptab-empty">{t("panel.dropHere")}</span>
        ) : (
          bottom.map((id) => {
            const meta = DOCK_META[id];
            // AI 面板里有等你回执的卡（反问 / 审批）：页签轻微明灭，面板收起时也能余光扫到
            const awaitAi =
              id === "ai" &&
              !!activeTab &&
              awaitingTracks(approvals, questions, activeTab.id).size > 0;
            return (
              <div
                key={id}
                role="button"
                tabIndex={0}
                className={`ptab${awaitAi ? " awaiting" : ""}`}
                data-on={open && cur === id}
                data-draggable={pinned ? undefined : "1"}
                data-dock-id={id}
                data-dock-label={t(meta.label)}
                title={t(meta.hint ?? meta.label) + (awaitAi ? ` · ${t("ai.awaitTip")}` : "")}
                onPointerDown={(e) => {
                  if (!pinned) beginDockTabDrag(id, e);
                }}
                onClick={() => panel.show(id)}
              >
                <span className="ptab-ico">{DOCK_ICONS[id]}</span>
                {t(meta.label)}
                {id === "transfers" && active > 0 && <b className="ptab-n">{active}</b>}
              </div>
            );
          })
        )}
        <button
          type="button"
          className="icon-btn panel-x"
          title={open ? t("panel.collapse") : t("panel.expand")}
          onClick={() => panel.toggle()}
        >
          <Chevron dir={open ? "down" : "up"} />
        </button>
      </div>

      {open && cur && (
        <div className="panel-body">
          <DockPanelBody id={cur} tab={activeTab} zone="bottom" />
        </div>
      )}
    </div>
  );
}
