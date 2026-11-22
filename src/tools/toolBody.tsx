// 工具 id → 组件主体映射（工具独立窗口 ToolWindow 与全局工作区面板 GlobalPanel 共用同一份）。
// 连接类内嵌主代码既有管理组件（embedded 态，去掉其自带模态壳）；网络 / 开发者类渲染本模块自带视图。
// 新增工具在此加一支即可（registry.tsx 里加元数据 + 这里接视图）。
import KeyGenerator from "../components/connect/KeyGenerator";
import TunnelManager from "../components/connect/TunnelManager";
import RecordingManager from "../components/terminal/RecordingManager";
import PortListView from "./views/PortListView";
import PortScannerView from "./views/PortScannerView";
import PingView from "./views/PingView";
import NetDiagView from "./views/NetDiagView";
import OpsKitView from "./views/OpsKitView";
import SendToolView from "./views/SendToolView";
import DevToolsView from "./views/DevToolsView";

const noop = () => {};

/** id → 工具主体（返回 null 表示未注册）。 */
export function toolBodyById(id: string) {
  switch (id) {
    case "keygen":
      // 纯生成器（与密钥库解耦）：只生成 + 输出公钥 + 存盘，不入库。
      return <KeyGenerator />;
    case "tunnel":
      return <TunnelManager onClose={noop} embedded />;
    case "record":
      return <RecordingManager onClose={noop} embedded />;
    case "send":
      // 本模块自带视图（不套主代码组件）：驱动在 sendStore，窗口关掉即在途发送停止。
      return <SendToolView />;
    case "ports":
      return <PortListView />;
    case "scan":
      return <PortScannerView />;
    case "ping":
      return <PingView />;
    case "netdiag":
      return <NetDiagView />;
    case "devtools":
      return <DevToolsView />;
    case "opskit":
      return <OpsKitView />;
    default:
      return null;
  }
}
