// 工具启动器：分类小标题 + 图标 + 名称 + 说明的行式列表（对齐 Sidebar 分组头的精致小标签风格）。
// 点任意工具 → 切到工具区并整页展示它（toolsStore.openTool + GlobalPanel）；如需独立窗口，由 GlobalPanel 头部的「弹出」按钮负责。
import { useI18n } from "../lib/i18n";
import { toolsActions } from "./store";
import { groupedTools } from "./registry";

export default function ToolsLauncher() {
  const t = useI18n();
  const groups = groupedTools();

  return (
    <div className="tools-launcher">
      {groups.map((g) => (
        <section key={g.category} className="tools-group">
          <div className="tools-group-head">{t(`tools.cat.${g.category}`)}</div>
          {g.tools.map((tool) => (
            <button
              key={tool.id}
              type="button"
              className="tools-item"
              title={t(tool.desc)}
              onClick={() => toolsActions.openTool(tool.id)}
            >
              <span className="tools-item-ico">{tool.icon}</span>
              <span className="tools-item-text">
                <span className="tools-item-name">{t(tool.name)}</span>
                <span className="tools-item-desc">{t(tool.desc)}</span>
              </span>
            </button>
          ))}
        </section>
      ))}
    </div>
  );
}
