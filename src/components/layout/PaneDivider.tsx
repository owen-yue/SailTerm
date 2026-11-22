import { useRef } from "react";
import Splitter from "../common/Splitter";
import Chevron from "../common/Chevron";

interface Props {
  /** 拖动增量（px）→ 调整两栏占比；totalPx 由本组件按父容器宽度测得后传入。 */
  nudge: (deltaPx: number, totalPx: number) => void;
  /** ◀：把分隔线推到最左（左栏收成 0 宽、右栏铺满）。 */
  dockLeft: () => void;
  /** ▶：把分隔线推到最右（右栏收成 0 宽、左栏铺满）。 */
  dockRight: () => void;
  /** 双击分隔线复位等分。 */
  reset: () => void;
  /** 三处提示文案（拖动 / 靠左吸附 / 靠右吸附），由调用方按各自的 i18n key 传入。 */
  dragTitle: string;
  dockLeftTitle: string;
  dockRightTitle: string;
}

/**
 * 双栏之间的「可拖动分隔线 + 悬停浮现吸附箭头」。
 *
 * 平时就是一条 app 通用细线，鼠标悬停才在两侧浮现 ◀ ▶ 一键吸附到最左/最右；双击复位等分。
 * 与 dockPaneStore 解耦——只吃 nudge/dockLeft/dockRight/reset 四个回调，任何横向双栏布局都能复用
 * （文件栏、其它停靠面板皆可）。两栏各自用 flexGrow=占比 / 1-占比 排布（见 FileBrowser），
 * 本组件只负责中间这根线；拖动基准宽度由组件按 parentElement 自行测量，调用方无需再传容器 ref。
 */
export default function PaneDivider({ nudge, dockLeft, dockRight, reset, dragTitle, dockLeftTitle, dockRightTitle }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const total = () => ref.current?.parentElement?.clientWidth ?? 0;
  return (
    <div className="pane-divider" ref={ref} onDoubleClick={reset}>
      <Splitter axis="x" className="pane-divider-drag" title={dragTitle} onDelta={(d) => nudge(d, total())} />
      {/* 吸附箭头是 .pane-divider 的子元素：悬停箭头仍算悬停分隔线、不会中途跑光；
          ◀ 贴线左（推走左栏）、▶ 贴线右（推走右栏），平时隐藏、hover 才浮现。 */}
      <button type="button" className="pane-dock-btn pane-dock-left" title={dockLeftTitle} onClick={dockLeft}>
        <Chevron dir="left" />
      </button>
      <button type="button" className="pane-dock-btn pane-dock-right" title={dockRightTitle} onClick={dockRight}>
        <Chevron dir="right" />
      </button>
    </div>
  );
}
