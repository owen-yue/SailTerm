// 左侧栏「工具」页：只渲染启动器列表。点工具后切到工具区并整页展示（toolsStore.openTool + GlobalPanel，主窗口照常可操作）。
// App.tsx 只需在 leftTab==="tools" 时渲染本组件（挂载点，约一行）。
import ToolsLauncher from "./ToolsLauncher";
// 副作用导入：注册工具自带文案包（extendLocale）与样式（随模块加载即生效；删目录即全消失）。
import "./locales";
import "./tools.css";

export default function ToolsSidebar() {
  return <ToolsLauncher />;
}
