// 文件浏览器/侧栏的扁平线稿图标（16 viewBox，stroke=currentColor）。
//
// 为什么不用 emoji/文本符号：macOS 会把 💻 🖥  📄  渲染成 Apple Color Emoji（彩色、圆角、
// 带高光），与本项目统一的细线图标语言（见 dockRegistry 的 DOCK_ICONS）格格不入；↑ ⌂ ⟳ ＋ × 等
// 文本符号又随字体回退而粗细不一。这里统一用 currentColor 描边，颜色跟随文字、尺寸交给外层 CSS。
// 约定：这些组件不带 width/height，由使用处（.icon-btn svg / .fp-nm i svg 等）统一设定尺寸。
import type { SVGProps } from "react";

type P = SVGProps<SVGSVGElement>;

/** 通用外壳：16 网格、无填充、currentColor 描边、圆角线帽。 */
function Svg({ children, ...rest }: P) {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.3"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...rest}
    >
      {children}
    </svg>
  );
}

/** 本地（💻）：笔记本。 */
export const IconLaptop = (p: P) => (
  <Svg {...p}>
    <rect x="3.5" y="4" width="9" height="6" rx="1" />
    <path d="M2 12.5h12" />
  </Svg>
);

/** 远端 SFTP（🖥）：显示器。 */
export const IconMonitor = (p: P) => (
  <Svg {...p}>
    <rect x="2.5" y="3.5" width="11" height="7" rx="1" />
    <path d="M6.5 13.5h3M8 10.5v3" />
  </Svg>
);

/** 远端 FTP（🌐）：地球。 */
export const IconGlobe = (p: P) => (
  <Svg {...p}>
    <circle cx="8" cy="8" r="5.5" />
    <path d="M2.5 8h11M8 2.5c1.7 1.7 1.7 9.3 0 11M8 2.5c-1.7 1.7-1.7 9.3 0 11" />
  </Svg>
);

/** 上级目录 / 上传（↑）。 */
export const IconArrowUp = (p: P) => (
  <Svg {...p}>
    <path d="M8 12.5v-9M4.5 7 8 3.5 11.5 7" />
  </Svg>
);

/** 下载（↓）。 */
export const IconArrowDown = (p: P) => (
  <Svg {...p}>
    <path d="M8 3.5v9M4.5 9 8 12.5 11.5 9" />
  </Svg>
);

/** 根目录（⌂）：房子。 */
export const IconHome = (p: P) => (
  <Svg {...p}>
    <path d="M3 7.5 8 3l5 4.5" />
    <path d="M4.5 8.8V13h7V8.8" />
  </Svg>
);

/** 刷新（⟳）：环形箭头。 */
export const IconRefresh = (p: P) => (
  <Svg {...p}>
    <path d="M13 8a5 5 0 1 1-1.6-3.7" />
    <path d="M13.2 3v2.4h-2.4" />
  </Svg>
);

/** 新建目录（＋）。 */
export const IconPlus = (p: P) => (
  <Svg {...p}>
    <path d="M8 3.5v9M3.5 8h9" />
  </Svg>
);

/** 跟随终端当前目录（◎）：靶心，读作「定位到终端所在处」。 */
export const IconTarget = (p: P) => (
  <Svg {...p}>
    <circle cx="8" cy="8" r="4.6" />
    <circle cx="8" cy="8" r="1.4" />
    <path d="M8 1.4v2M8 12.6v2M1.4 8h2M12.6 8h2" />
  </Svg>
);

/** 目录（📁）：文件夹。 */
export const IconFolder = (p: P) => (
  <Svg {...p}>
    <path d="M2.5 4.5h3.4l1.3 1.8H13.5v7.2H2.5z" />
  </Svg>
);

/** 文件（📄）：文档。 */
export const IconFile = (p: P) => (
  <Svg {...p}>
    <path d="M4 2.8h5l3 3v7.4H4z" />
    <path d="M9 2.8V6h3" />
  </Svg>
);

/** 终端选区：命令行窗口（提示符 > + 光标行）。 */
export const IconTerminalSel = (p: P) => (
  <Svg {...p}>
    <rect x="2" y="3" width="12" height="10" rx="1.5" />
    <path d="M4.6 6.6 6.4 8l-1.8 1.4" />
    <path d="M8 9.6h3" />
  </Svg>
);

/** 回形针（📎）：添加图片附件。 */
export const IconPaperclip = (p: P) => (
  <Svg {...p}>
    <path d="M10.6 5.2 6.4 9.4a1.9 1.9 0 0 0 2.7 2.7l4.5-4.5a3.4 3.4 0 0 0-4.8-4.8L3.9 7.3" />
  </Svg>
);

/** 敏感文件（🔒）：挂锁。 */
export const IconLock = (p: P) => (
  <Svg {...p}>
    <rect x="3.8" y="7" width="8.4" height="6.2" rx="1.2" />
    <path d="M5.8 7V5.4a2.2 2.2 0 0 1 4.4 0V7" />
  </Svg>
);

/** 清空 / 关闭（×）：细叉。 */
export const IconClose = (p: P) => (
  <Svg {...p}>
    <path d="M4.5 4.5l7 7M11.5 4.5l-7 7" />
  </Svg>
);

/** 清空 / 删除（🗑）：垃圾桶。 */
export const IconTrash = (p: P) => (
  <Svg {...p}>
    <path d="M3.4 4.4h9.2" />
    <path d="M6.2 4.4V3.1a.6.6 0 0 1 .6-.6h2.4a.6.6 0 0 1 .6.6v1.3" />
    <path d="M4.7 4.4l.6 8.1a1 1 0 0 0 1 .9h3.4a1 1 0 0 0 1-.9l.6-8.1" />
    <path d="M6.7 6.6v4.6M9.3 6.6v4.6" />
  </Svg>
);

/** 编辑（✎）：斜放铅笔，与 IconTrash 同属卡片行操作按钮。 */
export const IconEdit = (p: P) => (
  <Svg {...p}>
    <path d="M11.2 2.9l1.9 1.9-8 8-2.6.7.7-2.6z" />
    <path d="M9.9 4.2l1.9 1.9" />
  </Svg>
);

/** 命令片段（📋）：写字板。 */
export const IconClipboard = (p: P) => (
  <Svg {...p}>
    <rect x="3.4" y="3.4" width="9.2" height="9.8" rx="1.4" />
    <rect x="6" y="2.2" width="4" height="2" rx="0.6" />
    <path d="M5.8 7.4h4.4M5.8 9.8h3" />
  </Svg>
);

/** 密钥 / 关键词（🔑）：钥匙。 */
export const IconKey = (p: P) => (
  <Svg {...p}>
    <circle cx="5.6" cy="5.6" r="2.7" />
    <path d="M7.5 7.5l5 5" />
    <path d="M10.6 10.6l1.4-1.4M12 12l1.4-1.4" />
  </Svg>
);

/** 触发器（⚡）：闪电。 */
export const IconZap = (p: P) => (
  <Svg {...p}>
    <path d="M9.4 1.7 4.2 9h3.4l-1 5.3L12.4 7H8.6z" />
  </Svg>
);

/** 会话录制（🎥）：摄影机。 */
export const IconRecord = (p: P) => (
  <Svg {...p}>
    <rect x="2.4" y="5.4" width="8" height="6.2" rx="1.2" />
    <path d="M10.4 7.7l3.2-1.9v5.4l-3.2-1.9z" />
  </Svg>
);

/** 隧道 / 端口转发（🔀）：交叉箭头。 */
export const IconTunnel = (p: P) => (
  <Svg {...p}>
    <path d="M2.8 5h2.4l5.6 6h2.4" />
    <path d="M2.8 11h2.4l5.6-6h2.4" />
    <path d="M11.6 3.4 13.6 5l-2 1.6" />
    <path d="M11.6 9.4 13.6 11l-2 1.6" />
  </Svg>
);

/** AI 记忆（🧠）：大脑。 */
export const IconBrain = (p: P) => (
  <Svg {...p}>
    <path d="M8 3.2A2.4 2.4 0 0 0 5.7 4.9 2.2 2.2 0 0 0 4 7.1a2.3 2.3 0 0 0 .6 3.7A2.2 2.2 0 0 0 8 12.8z" />
    <path d="M8 3.2a2.4 2.4 0 0 1 2.3 1.7A2.2 2.2 0 0 1 12 7.1a2.3 2.3 0 0 1-.6 3.7A2.2 2.2 0 0 1 8 12.8z" />
    <path d="M8 3.2v9.6" />
  </Svg>
);

/** 插件（🧩）：拼图片。 */
export const IconPuzzle = (p: P) => (
  <Svg {...p}>
    <path d="M3 3.4h3.2a1.3 1.3 0 1 1 2.6 0H12v3.2a1.3 1.3 0 1 0 0 2.6V12.6H9.4a1.3 1.3 0 1 1-2.6 0H3V9.4a1.3 1.3 0 1 0 0-2.6z" />
  </Svg>
);

/** 设置（⚙）：齿轮（24 网格圆角齿 + 中心孔，避开「中心圆+放射线」像太阳的画法）。 */
export const IconGear = (p: P) => (
  <Svg viewBox="0 0 24 24" strokeWidth={2} {...p}>
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
  </Svg>
);

/** 详情（ⓘ）：圈 i。 */
export const IconInfo = (p: P) => (
  <Svg {...p}>
    <circle cx="8" cy="8" r="6" />
    <path d="M8 7.3v3.6M8 5.1h.01" />
  </Svg>
);

/** 成功 / 已同意（✓）：对号。 */
export const IconCheck = (p: P) => (
  <Svg {...p}>
    <path d="M3.4 8.4l3 3 6.2-6.8" />
  </Svg>
);

/** 显示密码（👁）：睁眼。 */
export const IconEye = (p: P) => (
  <Svg {...p}>
    <path d="M1.6 8S3.8 4 8 4s6.4 4 6.4 4-2.2 4-6.4 4S1.6 8 1.6 8z" />
    <circle cx="8" cy="8" r="2" />
  </Svg>
);

/** 隐藏密码（🙈）：闭眼（睁眼 + 斜线）。 */
export const IconEyeOff = (p: P) => (
  <Svg {...p}>
    <path d="M1.6 8S3.8 4 8 4s6.4 4 6.4 4-2.2 4-6.4 4S1.6 8 1.6 8z" />
    <circle cx="8" cy="8" r="2" />
    <path d="M3 13 13 3" />
  </Svg>
);

/** 打开位置（在文件管理器中显示）：开口文件夹——前页板抬起的形态，
   区别于右侧「选择目录」的实心文件夹（IconFolder），且不再像分享/外链箭头。 */
export const IconFolderOpen = (p: P) => (
  <Svg {...p}>
    <path d="M2.5 4.5h3.4l1.3 1.8h6.3v1.4" />
    <path d="M2.5 7.8h11l-1.7 4.7H2.5z" />
  </Svg>
);
