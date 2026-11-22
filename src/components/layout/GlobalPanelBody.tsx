// 全局工作区面板主体：id → 组件。密钥库走主代码 KeyVaultPage，其余工具复用 toolBodyById 映射。
// id 为 null（工具区尚未选中任何工具）时显示选择提示。与工具独立窗口（ToolWindow）共享同一份工具视图。
import KeyVaultPage from "../connect/KeyVaultPage";
import { toolBodyById } from "../../tools/toolBody";
import { useI18n } from "../../lib/i18n";

export default function GlobalPanelBody({ id }: { id: string | null }) {
  const tr = useI18n();
  // 密钥库自带 .key-vault（height:100% + 内边距 + 滚动），直接铺满栏高。
  if (id === "keys") return <KeyVaultPage />;
  // 工具区未选中工具：空态提示（左侧启动器仍在，选一个即整页展示）。
  if (!id) return <div className="global-panel-empty">{tr("tools.pickHint")}</div>;
  // 工具视图在独立窗口里靠 .tool-win-body 补内边距/滚动；全局面板无那层壳，这里补一个同语义容器。
  return <div className="global-panel-tool">{toolBodyById(id)}</div>;
}
