// 工具注册表（本模块唯一扩展点：新增工具 = 在此加一条 ToolDef + 在 ToolWindow 里接一个视图）。
// 纯元数据，不引组件（图标除外），保持可序列化与低耦合；渲染映射在 ToolWindow.tsx。
import type { ToolCategory, ToolDef } from "./types";
// 恢复运维工具箱时：把下行改回 `...IconNetDiag, IconOpsKit, IconPing...` 并取消下方 TOOL_DEFS 里 opskit 行的注释。
import { IconDevTools, IconKey, IconNetDiag, IconPing, IconPorts, IconRecord, IconScan, IconSend, IconTunnel } from "./icons";

export const TOOL_DEFS: ToolDef[] = [
  // —— 连接类：复用主代码既有模态组件（引用，不重写）——
  { id: "keygen", category: "connection", name: "tool.keygen.name", desc: "tool.keygen.desc", icon: <IconKey /> },
  { id: "tunnel", category: "connection", name: "tool.tunnel.name", desc: "tool.tunnel.desc", icon: <IconTunnel /> },
  { id: "record", category: "connection", name: "tool.record.name", desc: "tool.record.desc", icon: <IconRecord />, winSize: { width: 1180, height: 760 } },
  // 发送（脚本发到多台）：跨会话的发射器，只走独立弹窗（顶栏 / Ctrl+Shift+B），不进工具区也不列在启动器。
  { id: "send", category: "connection", name: "tool.send.name", desc: "tool.send.desc", icon: <IconSend />, launcherHidden: true },

  // —— 网络类：本模块自带视图，后端 commands/net.rs ——
  { id: "ports", category: "network", name: "tool.ports.name", desc: "tool.ports.desc", icon: <IconPorts /> },
  { id: "scan", category: "network", name: "tool.scan.name", desc: "tool.scan.desc", icon: <IconScan /> },
  { id: "ping", category: "network", name: "tool.ping.name", desc: "tool.ping.desc", icon: <IconPing /> },
  { id: "netdiag", category: "network", name: "tool.netdiag.name", desc: "tool.netdiag.desc", icon: <IconNetDiag /> },

  // —— 开发者类：本模块自带视图，纯前端零后端 ——
  { id: "devtools", category: "dev", name: "tool.devtools.name", desc: "tool.devtools.desc", icon: <IconDevTools /> },

  // —— 运维类（暂隐藏：需要时取消下行注释即在启动器重现，视图/图标/文案均保留）——
  // { id: "opskit", category: "ops", name: "tool.opskit.name", desc: "tool.opskit.desc", icon: <IconOpsKit /> },
];

/** 分类展示顺序（启动器分组）。 */
export const CATEGORY_ORDER: ToolCategory[] = ["connection", "network", "ops", "dev"];

/** 按分类分组（保持 CATEGORY_ORDER 次序，空组省略）。 */
export function groupedTools(): { category: ToolCategory; tools: ToolDef[] }[] {
  return CATEGORY_ORDER.map((category) => ({
    category,
    tools: TOOL_DEFS.filter((t) => t.category === category && !t.launcherHidden),
  })).filter((g) => g.tools.length > 0);
}

export function toolById(id: string | null): ToolDef | undefined {
  return id ? TOOL_DEFS.find((t) => t.id === id) : undefined;
}
