import { useEffect, useRef } from "react";
import FilePane from "./FilePane";
import TransferStrip from "./TransferStrip";
import PaneDivider from "../layout/PaneDivider";
import { IconGlobe, IconLaptop, IconMonitor } from "../common/icons";
import {
  files,
  fileStore,
  MAIN_SPLIT_ENABLED,
  splitActions,
  termSplitStore,
  useDockSplit,
  useStore,
  type Lane,
} from "../../stores";
import { useI18n } from "../../lib/i18n";

interface Props {
  /** 本实例服务的会话：远端栏按此分片，与「当前活动标签」无关。 */
  session: string;
  /** 该会话是否已连接（未连接时远端栏整体置灰）。 */
  connected: boolean;
  /** 远端标题显示的会话名。 */
  title: string;
  /** 远端是 FTP/FTPS（无 shell）：换地球图标，并隐藏「权限…」入口。 */
  ftp: boolean;
  /**
   * 本实例渲染在哪块地：`right` / `bottom` = 停靠区，`main` = FTP 主区（文件即全部内容），
   * `split` = 「终端 | 文件」主区分屏的大视图。
   * 两用途：① 选**双栏默认比例**（右栏窄列本地栏该压小，其余平分）；② 决定顶条形态
   * （停靠位显「展开到大视图」，分屏位显标题 +「收起」）。比例键按区分开（见 dockPaneStore）。
   */
  zone: "right" | "bottom" | "main" | "split";
}

/**
 * 双栏文件浏览器：本地 / 远端左右并排，中间是可拖动的分隔条。
 *
 * 三个挂载点：
 *  - **停靠面板**（`files`）：跟聚焦会话，SSH 会话下它是主区终端的配角；
 *  - **FTP 主区**：FTP/FTPS 没有 PTY，文件就是它的全部内容（无需切换条）；
 *  - **主区分屏**（`split`）：「终端 | 文件」大视图，SSH 会话按需与终端并排（见 App 的 Pane）。
 * 各处都按 `session` 取 fileStore 分片，分屏下多会话并存互不串台。
 *
 * 布局策略（始终横向，不随宽度纵向堆叠）：
 *  - 拖动分隔条自由调整两栏宽度（比例按「哪个区」分别记忆，见 dockPaneStore）；
 *  - 两个吸附箭头平时隐藏、鼠标悬停到分隔线上才浮现：◀ 贴线左（推走本地=只看远端）、▶ 贴线右（推走远端=只看本地）。
 *
 * 从操作系统拖进来的文件**不在这里订阅**：全局落点路由收口在 App（onOsFileDrop 只挂一份），
 * 命中某实例远端栏时按该栏的会话与目录上传——一份监听管所有挂载点，多实例并存也不会重复注册。
 */
export default function FileBrowser({ session, connected, title, ftp, zone }: Props) {
  const tr = useI18n();
  const hostRef = useRef<HTMLDivElement>(null);
  // 分屏态：停靠位的「展开到大视图」按钮据此显隐（已展开则不重复给入口）
  const splitOn = useStore(termSplitStore).ids.has(session);

  // 双栏比例按区分开记忆：右栏默认把本地压到三成（那里的主角是远端），下栏/主区/分屏默认平分
  const split = useDockSplit(zone, "files", zone === "right" ? 0.28 : 0.5);

  // 本地栏状态全局共享，但仍带上本视图的会话：上传目标与 zmodem 对端要靠它找到这一屏
  const localLane: Lane = { side: "local", session };

  // 首次挂载时加载本地栏；远端栏由挂载方的会话视图调度负责 open/drop（主窗口 App / 弹窗 PopWorkspace）
  useEffect(() => {
    if (!fileStore.get().local.path) void files.load(localLane);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="fb-host" ref={hostRef}>
      {/* 顶条只给「能换位」的形态：停靠位 =「⇤ 展开到大视图」（把双栏摆到终端右侧半屏）；
          分屏位 = 标题 +「收起」回停靠形态。FTP 主区（main）没有换位概念，保持纯净观感。 */}
      {(zone === "right" || zone === "bottom") && (
        <div className="fb-head">
          {/* 大视图入口总开关（当前关）：关掉后顶条为空，靠 CSS :empty 不占高 */}
          {MAIN_SPLIT_ENABLED && connected && !splitOn && splitActions.isSplittable(session) && (
            <button
              className="fb-head-btn"
              title={tr("fb.expandMainHint")}
              onClick={() => splitActions.toggle(session)}
            >
              ⇤ {tr("fb.expandMain")}
            </button>
          )}
        </div>
      )}
      {zone === "split" && (
        <div className="fb-head">
          <span className="fb-head-title" title={title}>
            <IconMonitor />
            {title}
          </span>
          <button
            className="fb-head-btn"
            title={tr("fb.collapseMain")}
            onClick={() => splitActions.toggle(session)}
          >
            {tr("fb.collapseMain")} ✕
          </button>
        </div>
      )}
      <div className="fb fb-split">
        <div className="fb-pane" style={{ flexGrow: split.ratio, flexBasis: 0, flexShrink: 1 }}>
          <FilePane lane={localLane} icon={<IconLaptop />} title={tr("kind.local")} />
        </div>
        <PaneDivider
          nudge={split.nudge}
          dockLeft={split.dockLeft}
          dockRight={split.dockRight}
          reset={split.reset}
          dragTitle={tr("fb.splitDrag")}
          dockLeftTitle={tr("fb.dockRemote")}
          dockRightTitle={tr("fb.dockLocal")}
        />
        <div className="fb-pane" style={{ flexGrow: 1 - split.ratio, flexBasis: 0, flexShrink: 1 }}>
          <FilePane
            lane={{ side: "remote", session }}
            icon={ftp ? <IconGlobe /> : <IconMonitor />}
            title={title}
            ftp={ftp}
            connected={connected}
          />
        </div>
      </div>
      <TransferStrip session={session} />
    </div>
  );
}
