// 工具模块自带图标（Lucide 风格 24×24 线稿，stroke=currentColor，统一 1.8 线宽；
// 独立成文件是为「摘走工具不拖走主图标库」。渲染尺寸由使用处 CSS 控制，故此处只设 viewBox）。
import type { SVGProps } from "react";

type P = SVGProps<SVGSVGElement>;
const base = (p: P) => ({
  width: 16,
  height: 16,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.8,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  "aria-hidden": true,
  ...p,
});

/** 左侧栏「工具」页签：扳手。 */
export const IconWrench = (p: P) => (
  <svg {...base(p)}>
    <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
  </svg>
);

/** SSH 密钥。 */
export const IconKey = (p: P) => (
  <svg {...base(p)}>
    <circle cx="16.5" cy="7.5" r="5.5" />
    <path d="M12.6 11.4 3 21v0" />
    <path d="M6 18l2 2M9 15l2 2" />
  </svg>
);

/** 端口转发 / 隧道（拱形通道）。 */
export const IconTunnel = (p: P) => (
  <svg {...base(p)}>
    <path d="M4 21v-8a8 8 0 0 1 16 0v8" />
    <path d="M2 21h20" />
    <path d="M9 21v-8a3 3 0 0 1 6 0v8" />
  </svg>
);

/** 会话录制。 */
export const IconRecord = (p: P) => (
  <svg {...base(p)}>
    <circle cx="12" cy="12" r="9" />
    <circle cx="12" cy="12" r="3.5" fill="currentColor" stroke="none" />
  </svg>
);

/** 开放端口列表（服务器 / 服务堆叠）。 */
export const IconPorts = (p: P) => (
  <svg {...base(p)}>
    <rect x="3" y="4" width="18" height="7" rx="2" />
    <rect x="3" y="13" width="18" height="7" rx="2" />
    <path d="M7 7.5h.01M7 16.5h.01" />
  </svg>
);

/** 端口扫描（扫描框 + 扫描线）。 */
export const IconScan = (p: P) => (
  <svg {...base(p)}>
    <path d="M3 7V5a2 2 0 0 1 2-2h2M17 3h2a2 2 0 0 1 2 2v2M21 17v2a2 2 0 0 1-2 2h-2M7 21H5a2 2 0 0 1-2-2v-2" />
    <path d="M3 12h18" />
  </svg>
);

/** Ping（脉冲 / 时延活动）。 */
export const IconPing = (p: P) => (
  <svg {...base(p)}>
    <path d="M22 12h-4l-3 8L9 4l-3 8H2" />
  </svg>
);

/** 开发者工具箱（尖括号 + 斜杠 = 代码）。 */
export const IconDevTools = (p: P) => (
  <svg {...base(p)}>
    <path d="M8 8l-4 4 4 4" />
    <path d="M16 8l4 4-4 4" />
    <path d="M13.5 6l-3 12" />
  </svg>
);

/** 网络诊断（路由跳转：节点连线 + 目标）。 */
export const IconNetDiag = (p: P) => (
  <svg {...base(p)}>
    <circle cx="5" cy="18" r="2.2" />
    <circle cx="12" cy="6" r="2.2" />
    <circle cx="19" cy="15" r="2.2" />
    <path d="M6.7 16.4 10.6 7.9M13.7 7.3l4 6" />
  </svg>
);

/** 运维工具箱（仪表盘：半圆弧 + 指针）。 */
export const IconOpsKit = (p: P) => (
  <svg {...base(p)}>
    <path d="M4 16a8 8 0 1 1 16 0" />
    <path d="M12 16l4-4" />
    <path d="M4 16h2M18 16h2M12 8V6" />
  </svg>
);

/** 发送（纸飞机：把一份脚本发到多台）。 */
export const IconSend = (p: P) => (
  <svg {...base(p)}>
    <path d="M21 3 3 10.5l6.4 2.6L12 21l3.1-6.6z" />
    <path d="M9.4 13.1 21 3" />
  </svg>
);
