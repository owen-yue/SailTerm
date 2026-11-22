// 全局工作区面板：整页接管主区（占满 .main），承载「工具 / 密钥库」两个独立内容区。
// 由 App 的 leftTab 单点驱动：keys → 密钥库整页；tools → 当前选中工具（activeTool，未选时显示选择提示）。
// 与会话区互斥：接管时终端隐藏保活、会话标签栏 / 右栏 / 底栏让位（门控在 App）。
// 头部动作：密钥库不提供关闭（本身就是侧栏功能切换出的整页，离开即点侧栏其它区）；
// 工具提供「弹出为独立窗口」与「关闭」——关闭只清空当前工具、留在工具区选工具提示，不切回连接区。
import GlobalPanelBody from "./GlobalPanelBody";
import { IconKey } from "../common/icons";
import { IconWrench } from "../../tools/icons";
import { toolById } from "../../tools/registry";
import { openToolWindow } from "../../tools/openWindow";
import { useI18n } from "../../lib/i18n";
import { toolsActions } from "../../tools/store";

/** 弹出为独立窗口的线稿图标（16px，与侧栏图标同一视觉语言）。 */
function IconPopout() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d="M14 4h6v6" />
      <path d="M20 4l-9 9" />
      <path d="M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5" />
    </svg>
  );
}

export default function GlobalPanel({ id }: { id: string | null }) {
  const tr = useI18n();
  const isKeys = id === "keys";
  const def = id && !isKeys ? toolById(id) : null;
  const title = isKeys ? tr("rail.keys") : def ? tr(def.name) : tr("tools.pickHint");
  const icon = isKeys ? <IconKey /> : (def?.icon ?? <IconWrench />);

  return (
    <div className="global-panel">
      <div className="global-panel-head" data-tauri-drag-region="deep">
        <span className="global-panel-ico">{icon}</span>
        <span className="global-panel-title">{title}</span>
        {def && (
          <span className="global-panel-actions" data-tauri-drag-region="false">
            <button
              type="button"
              className="icon-btn"
              title={tr("globalRail.popout")}
              aria-label={tr("globalRail.popout")}
              onClick={() => void openToolWindow(id as string, `${title} — SailTerm`)}
            >
              <IconPopout />
            </button>
            <button
              type="button"
              className="icon-btn"
              title={tr("common.close")}
              aria-label={tr("common.close")}
              onClick={() => toolsActions.clearTool()}
            >
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true">
                <path d="M6 6l12 12M18 6L6 18" />
              </svg>
            </button>
          </span>
        )}
      </div>
      <div className="global-panel-body">
        <GlobalPanelBody id={id} />
      </div>
    </div>
  );
}
