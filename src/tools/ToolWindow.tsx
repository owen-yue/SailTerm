// 工具独立窗口：一个工具 = 一个 OS 窗口，复用「会话弹出窗口」同款薄顶栏（WindowControls + 拖拽区）。
// 主体按 id 渲染——连接类内嵌主代码既有管理组件（embedded 态，去掉其自带模态壳），
// 网络类渲染本模块自带视图。由 main.tsx 依 `?tool=<id>` 路由到本组件。
// 独立 JS 上下文：需在此同步外观主题；record 依赖会话列表，故一并 sync 存活会话。
import { useEffect } from "react";
import WindowControls from "../components/common/WindowControls";
import Toasts from "../components/common/Toasts";
import { useBrowserGuards } from "../lib/shellGuards";
import { revealWindow } from "../lib/tauri";
import { useI18n } from "../lib/i18n";
import { appearanceActions, connections, sessionActions } from "../stores";
import { toolBodyById } from "./toolBody";
import { toolById } from "./registry";
// 副作用导入：注册工具自带文案包与样式（本窗口是独立上下文，需各自加载一次）。
import "./locales";
import "./tools.css";

export default function ToolWindow({ id }: { id: string }) {
  const t = useI18n();
  const def = toolById(id);

  // 桌面壳防浏览器化：拦截 F5/F12/Ctrl+R 等（与主窗口/弹出窗口一致）。
  useBrowserGuards();

  // 本窗口独立上下文：应用外观主题（后台拉权威值校正）；record 需认领后端存活会话及档案配置。
  // 主题已由 main.tsx 同步预着色，直接亮窗不等 refresh（mac 建窗隐藏，避免 WKWebView 白/错色闪）。
  useEffect(() => {
    void appearanceActions.refresh();
    void sessionActions.sync();
    void connections.refresh();
    revealWindow();
  }, []);

  if (!def) return null;

  return (
    <div className="app tool-win">
      <header className="topbar" data-tauri-drag-region="deep">
        <WindowControls />
        <div className="topbar-main">
          <span className="tag">
            <span className="tool-win-ico">{def.icon}</span> {t(def.name)}
          </span>
        </div>
      </header>
      <div className="body">
        <main className="tool-win-body">{toolBodyById(id)}</main>
      </div>
      <Toasts />
    </div>
  );
}
