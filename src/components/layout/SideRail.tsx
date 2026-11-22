import type { ReactNode } from "react";
import type { DockPanelId } from "../../types";
import { beginDockTabDrag } from "../../lib/dragDockTab";
import Chevron from "../common/Chevron";
import { useI18n } from "../../lib/i18n";

export interface RailTab {
  id: string;
  label: string;
  icon: ReactNode;
  /** 悬浮提示（可选）：比 label 更详细的一句话 */
  hint?: string;
  /** 该面板里有等你回执的内容（AI 反问 / 命令审批挂起）：页签轻微明灭，栏收起时也能余光扫到 */
  awaiting?: boolean;
}

interface Props {
  /** 贴边方向：left = 左侧连接栏；right = 右侧停靠栏（镜像分隔线） */
  side: "left" | "right";
  /** 页签列表：图标 + 文字纵向排列，常驻显示（与下边栏同一交互模型） */
  tabs: RailTab[];
  /** 当前选中的页签 id */
  activeId: string;
  /** 边栏是否展开：收起时页签仍常驻，只是不再高亮内容 */
  open: boolean;
  /** 点击页签：由调用方决定「切页 / 展开 / 点当前页收起」语义 */
  onSelect: (id: string) => void;
  /** 收起/展开开关：置顶显示（先于页签） */
  onToggle: () => void;
  /** 传入即启用停靠拖放：页签可拖出本轨，本轨也是投放热区（右侧栏专用）。 */
  onPanelDrop?: (id: string) => void;
}

/**
 * 侧边栏常驻页签轨（对齐下边栏 .panel-tabs）：图标 + 文字的页签条**始终显示**，
 * 展开/收起只切换旁边的内容区，页签轨本身不消失——与下边栏「收起只留一条薄标签栏」一致。
 *
 * 只有一个页签时（左侧「连接」）也照常显示，保持工具栏观感。
 * 用法：App.tsx（左右两侧）、PopWorkspace.tsx（右侧）。右侧轨接入停靠拖放
 * （传 `onPanelDrop`），左侧连接栏固定不参与。
 */
export default function SideRail({ side, tabs, activeId, open, onSelect, onToggle, onPanelDrop }: Props) {
  const tr = useI18n();
  // 收起/展开箭头指向「动作方向」，与下边栏 panel-x 同语义（开→朝收起方向，关→朝展开方向）
  const chevron = side === "left" ? (open ? "left" : "right") : open ? "right" : "left";
  return (
    <aside
      className={`side-rail ${side}`}
      data-tauri-drag-region="false"
      data-drop-zone={side === "right" ? "right" : undefined}
    >
      {/* 收起/展开箭头置顶（用户指定）：先于页签渲染 */}
      <button
        type="button"
        className="icon-btn side-rail-x"
        title={open ? tr("rail.collapse") : tr("rail.expand")}
        onClick={onToggle}
      >
        <Chevron dir={chevron} />
      </button>
      {tabs.map((t) => (
        <div
          key={t.id}
          role="button"
          tabIndex={0}
          className={`side-rail-btn${t.awaiting ? " awaiting" : ""}`}
          data-on={open && t.id === activeId ? "true" : undefined}
          data-draggable={onPanelDrop ? "1" : undefined}
          data-dock-id={t.id}
          data-dock-label={tr(t.label)}
          title={tr(t.hint || t.label) + (t.awaiting ? ` · ${tr("ai.awaitTip")}` : "")}
          onPointerDown={(e) => {
            if (onPanelDrop) beginDockTabDrag(t.id as DockPanelId, e);
          }}
          onClick={() => onSelect(t.id)}
        >
          <span className="side-rail-ico">{t.icon}</span>
          <span className="side-rail-txt">{tr(t.label)}</span>
        </div>
      ))}
    </aside>
  );
}

/** 侧栏页签线稿图标（16px，与下栏页签图标同一视觉语言；停靠面板图标见 dockRegistry）。 */
export const RAIL_ICONS: Record<string, ReactNode> = {
  connections: (
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round">
      <rect x="2.5" y="2.5" width="11" height="4.5" rx="1" />
      <rect x="2.5" y="9" width="11" height="4.5" rx="1" />
      <path d="M4.5 4.75h.01M4.5 11.25h.01" strokeWidth="1.6" />
    </svg>
  ),
};
