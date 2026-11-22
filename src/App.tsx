import { useEffect, useMemo, useRef, useState } from "react";
import { IconGear, IconKey } from "./components/common/icons";
import AiApprovalDialog from "./components/ai/AiApprovalDialog";
import AiToggle from "./components/ai/AiToggle";
import BottomPanel from "./components/layout/BottomPanel";
import GlobalPanel from "./components/layout/GlobalPanel";
import { DockPanelBody, DOCK_ICONS, DOCK_META, visiblePanels } from "./components/layout/dockRegistry";
import ConflictDialog from "./components/file/ConflictDialog";
import ConnectPanel from "./components/connect/ConnectPanel";
import FileBrowser from "./components/file/FileBrowser";
import ConnectProgress from "./components/connect/ConnectProgress";
import FtpsCertDialog from "./components/file/FtpsCertDialog";
import HostKeyDialog from "./components/connect/HostKeyDialog";
import KbdAuthDialog from "./components/connect/KbdAuthDialog";
import ImageLightbox from "./components/file/ImageLightbox";
import RestartChip from "./components/terminal/RestartChip";
import SettingsCenter from "./components/settings/SettingsCenter";
import type { CategoryId } from "./components/settings/SettingsCenter";
import UsageStatsDialog from "./components/ai/UsageStatsDialog";
import Sidebar from "./components/layout/Sidebar";
import SideRail, { RAIL_ICONS } from "./components/layout/SideRail";
// —— 工具模块挂载点（可分离：删掉这几行 import + 下方 leftTab 分支即从主代码摘除工具；工具本体以独立窗口打开，见 tools/openWindow.ts）——
import ToolsSidebar from "./tools/ToolsSidebar";
import { IconSend, IconWrench } from "./tools/icons";
import { toolsActions, toolsStore } from "./tools/store";
import { openToolWindow } from "./tools/openWindow";
import Splitter from "./components/common/Splitter";
import Toasts from "./components/common/Toasts";
// 铃铛入口暂隐藏（见顶栏注释），组件保留，放开时接回 import 与挂载两处。
// import NotificationsMenu from "./components/common/NotificationsMenu";
import StatusDot, { statusLabel } from "./components/common/StatusDot";
import WindowControls from "./components/common/WindowControls";
import { plugins as pluginApi } from "./lib/tauri";
import { writePluginOutput } from "./lib/termAgent";
import { registerPluginLocales, useI18n } from "./lib/i18n";
import { connectUi } from "./lib/connectUi";
import { allowNativeContextMenu, useBrowserGuards } from "./lib/shellGuards";
import { ownsSession, popTabWindow } from "./lib/windows";
import { requestLock } from "./lib/lockClient";
import { FEATURES } from "./lib/features";
import { kbdLabel } from "./lib/kbd";
import type { CommandCtx, SettingsCat } from "./lib/commands";
import type { PluginUiButton } from "./types";
import TabBar from "./components/layout/TabBar";
import Terminal from "./components/terminal/Terminal";
import Welcome from "./components/layout/Welcome";
import CommandPalette from "./components/dialog/CommandPalette";
import ShortcutHelp from "./components/dialog/ShortcutHelp";
import { setFaqListener } from "./lib/faqJump";
import { useSessionName } from "./lib/sessionName";
import { ai as aiApi, ftpsCerts, hostKeys, onOsFileDrop, selfWindow, sessions as sshApi, sshKbd, transfers as transferApi, zmodem as zmodemApi, type UnlistenFn } from "./lib/tauri";
import {
  aiChat,
  aiStore,
  AI_QUEUE_KEYS,
  appearanceActions,
  appearanceStore,
  awaitingTracks,
  collapseActions,
  collapseStore,
  connectionStore,
  connections,
  dockActions,
  dockStore,
  dockZoneOf,
  aggregateProgress,
  files,
  fileStore,
  ftpsCertDecisions,
  hostKeyDecisions,
  isFinal,
  kbdDecisions,
  layoutActions,
  layoutStore,
  MAIN_SPLIT_ENABLED,
  nodeLeaves,
  paneOf,
  panel,
  panelStore,
  providerActions,
  sessionActions,
  sessionStore,
  settingsActions,
  settingsStore,
  splitActions,
  termSplitStore,
  terminalAiStore,
  terminalStore,
  toast,
  setWinFocused,
  notifyOsBypass,
  transferActions,
  transferStore,
  triggerActions,
  uploadPaths,
  useDockSplit,
  useStore,
  useStorePick,
} from "./stores";
import type { DropZone, SplitNode, Side } from "./stores";
import { canAgentRail, hasAnyCap, hasCap, kindHasFiles, kindHasTerminal } from "./lib/capabilities";
import { kindIsFtp, sessionLabel } from "./types";
import type { DockPanelId, Profile, TransferStateEvent } from "./types";
import type { Tab } from "./stores";
import "./App.css";

/** 对话框状态：关闭 / 新建 / 编辑某个档案。 */
type Dialog = { mode: "new" } | { mode: "edit"; profile: Profile } | null;

/** 四个根级弹窗的标识：命令面板、快捷键帮助、用量统计、设置中心。 */
type RootEntry = "palette" | "help" | "usage" | "settings";
/**
 * 根级弹窗槽位：同一时刻只允许挂一个，开新的自动顶掉旧的。
 * 此前这四个各存一份 state，叠开时两层遮罩按 DOM 顺序压在一起——下层弹窗既看不见又关不掉
 * （Esc 栈只关最顶层）。用量统计是跨会话累计的全局弹窗、不属某个 AI 抽屉，故也挂在全局。
 * 嵌套的决策类弹窗（确认 / 输入 / 主机密钥 / 图片大图）不占本槽位：它们本就该盖在宿主弹窗之上。
 */
type RootModal =
  | { kind: "none" | Exclude<RootEntry, "settings"> }
  | { kind: "settings"; cat: CategoryId };

/** 拖放分区：pane 四边 28%/35% 条带为分屏投放区，中间为「弹出独立窗口」区（第七节接入）。 */
function dropZoneAt(e: { clientX: number; clientY: number }, el: Element): DropZone | null {
  const r = el.getBoundingClientRect();
  const x = (e.clientX - r.left) / r.width;
  const y = (e.clientY - r.top) / r.height;
  return x < 0.28 ? "left" : x > 0.72 ? "right" : y < 0.35 ? "top" : y > 0.65 ? "bottom" : null;
}

interface PaneProps {
  tab: Tab;
  /** 分屏模式下才渲染 pane-head（标题 / × 移出分屏）。 */
  head: boolean;
  focus: boolean;
  /** 本窗格当前是否显示（单屏=聚焦项；分屏=树内叶子）。 */
  active: boolean;
}

/** 一个会话窗格：四边拖放分屏 + pane-head + 主区内容。
 * 主区归属看能力表：有 PTY 就是终端（文件浏览在右侧栏），无 PTY 但有文件后端（FTP/FTPS）则文件即主区。
 * SSH 会话另可开「终端 | 文件」主区分屏（大视图，per-session 记忆于 termSplitStore），
 * 展开时停靠区的「文件」页签自动隐去（同一会话不摆两份文件树），收起即回纯终端 + 侧栏配角形态。
 * AI 栏已提升为窗口级停靠栏（App 的右停靠列，跟随聚焦会话），窗格内不再各自挂栏。 */
function Pane({ tab: t, head, focus, active }: PaneProps) {
  const tr = useI18n();
  // 分屏后几个 pane-head 同屏，名字必须能跟页签对上（重名带同一个 #n）。
  const name = useSessionName();
  const id = t.id;
  const hasTerm = kindHasTerminal(t.kind);
  /** 无终端却有远端文件后端 = FTP 系：它没地方放终端，文件浏览器就是主区，不需要切换条。 */
  const filesInMain = !hasTerm && kindHasFiles(t.kind);
  /** 本会话的主区分屏态：只在显示中的已连 SSH 终端生效（隐藏 pane 不挂 FileBrowser，省目录请求）。
   * 前置 MAIN_SPLIT_ENABLED：入口已关，已持久化的旧分屏态也不再渲染（见 termSplitStore）。 */
  const splitOn =
    MAIN_SPLIT_ENABLED &&
    active &&
    !t.phase &&
    hasTerm &&
    t.state === "connected" &&
    termSplitStore.get().ids.has(id);
  // 文件列宽度：useDockSplit 的 ratio 语义在这里当「右侧列占宽比例」用，键空间与停靠区不撞
  const filesSplit = useDockSplit("tsplit", "files", 0.5);
  const filesFlex = { flexGrow: filesSplit.ratio, flexBasis: "50%", flexShrink: 1, minWidth: 180 };
  return (
    <div
      className="term-pane"
      data-session={id}
      data-active={active}
      data-focus={focus}
      onDragOver={(e) => {
        if (!e.dataTransfer.types.includes("text/session-id")) return;
        e.preventDefault();
        e.dataTransfer.dropEffect = "move";
        const zone = dropZoneAt(e, e.currentTarget);
        e.currentTarget.dataset.dropZone = zone ?? "";
      }}
      onDragLeave={(e) => {
        e.currentTarget.dataset.dropZone = "";
      }}
      onDrop={(e) => {
        e.preventDefault();
        e.currentTarget.dataset.dropZone = "";
        const dragId = e.dataTransfer.getData("text/session-id");
        if (!dragId || dragId === id) return;
        const zone = dropZoneAt(e, e.currentTarget);
        if (!zone) return;
        layoutActions.splitAt(id, zone, dragId);
      }}
    >
      {/* 分屏时每个可见 pane 顶一条细头：点标题聚焦、× 移出分屏（会话标签保留） */}
      {head && (
        <div className="pane-head" data-focus={focus}>
          <button
            className="pane-title"
            title={`${tr("app.paneFocus")} · ${statusLabel(tr, t.state)}`}
            onClick={() => sessionActions.select(id)}
          >
            <StatusDot state={t.state} tooltip={false} />
            <span className="tl">{name(t)}</span>
          </button>
          <button
            className="pane-x"
            title={tr("app.panePop")}
            onClick={() => popTabWindow(t)}
          >
            ⧉
          </button>
          <button
            className="pane-x"
            title={tr("app.paneUnsplit")}
            onClick={() => layoutActions.closeLeaf(id)}
          >
            ×
          </button>
          {/* 分屏下 AI 开关搭 pane-head 现成这一行（终端不再单占一条细条）。
              与 Terminal 的细条同源：不再随页签隐藏（两轨已独立，开关在聊天页签照样有意义）；
              聊天唯一形态的会话（canAgentRail 不过）仍不画；
              放这里也不需要「备用屏收起」判据——它不额外占行，没东西可让。 */}
          {hasTerm && canAgentRail(t.kind) && <AiToggle sessionId={id} />}
        </div>
      )}
      {/* 预加载/失败占位标签：主区渲染进度视图而非终端（或 FTP 文件栏）；
          建连成功后占位标签被换成真实会话（phase 清空），本分支自然让位给下面的终端/文件栏。 */}
      {t.phase ? (
        <ConnectProgress tab={t} />
      ) : (
        <>
          {hasTerm && (
            <div className="term-stack">
              <div className="term-slot">
                <Terminal sessionId={id} active={active} aiStrip={!head} />
                <UploadStrip session={id} />
              </div>
              {/* 「终端 | 文件」大视图：右侧并排双栏文件浏览器，宽度可拖（按会话窗口共享记忆） */}
              {splitOn && (
                <>
                  <Splitter
                    axis="x"
                    title={tr("ts.resize")}
                    onDelta={(d) => {
                      const host = document.getElementById(`tsplit-${id}`);
                      filesSplit.nudge(d, host ? host.clientWidth : 0);
                    }}
                  />
                  <div className="term-files" id={`tsplit-${id}`} style={filesFlex}>
                    <FileBrowser
                      session={id}
                      connected
                      title={sessionLabel(t, tr)}
                      ftp={kindIsFtp(t.kind)}
                      zone="split"
                    />
                  </div>
                </>
              )}
            </div>
          )}
          {filesInMain && (
            <FileBrowser
              session={id}
              connected={t.state === "connected"}
              title={sessionLabel(t, tr)}
              ftp={kindIsFtp(t.kind)}
              zone="main"
            />
          )}
        </>
      )}
    </div>
  );
}

interface SplitViewProps {
  node: SplitNode;
  activeId: string | null;
}

/** 递归渲染嵌套分屏树：分支 = flex 容器 + 分割条（ratio 写回），叶子 = Pane。 */
function SplitView({ node, tabs, activeId }: SplitViewProps & { tabs: Tab[] }) {
  const tr = useI18n();
  if (node.kind === "leaf") {
    const t = tabs.find((x) => x.id === node.session);
    if (!t) return null;
    return <Pane tab={t} head active focus={t.id === activeId} />;
  }
  const row = node.dir === "row";
  return (
    <div className="split-branch" data-dir={node.dir} data-branch={node.id}>
      <div className="split-child" style={{ flexBasis: `calc(${node.ratio * 100}% - 2px)` }}>
        <SplitView node={node.a} tabs={tabs} activeId={activeId} />
      </div>
      <Splitter
        axis={row ? "x" : "y"}
        className="split-sep"
        title={tr("app.splitRatio")}
        onDelta={(d) => {
          const el = document.querySelector<HTMLElement>(`[data-branch="${node.id}"]`);
          const total = el ? (row ? el.clientWidth : el.clientHeight) : 0;
          layoutActions.nudgeRatio(node.id, d, total);
        }}
      />
      <div className="split-child" style={{ flexBasis: `calc(${(1 - node.ratio) * 100}% - 2px)` }}>
        <SplitView node={node.b} tabs={tabs} activeId={activeId} />
      </div>
    </div>
  );
}

/**
 * 终端底部的「拖到终端即上传」进度条：本会话有在传的上传时浮现，全部落定后淡出。
 * 快速上传时文件面板可能整块收着（TransferStrip 看不见），这条就是唯一常驻反馈；
 * 按用户既定偏好不弹右上角 toast，preparing 占位期先显字（submitBatch 入口秒现）。
 */
function UploadStrip({ session }: { session: string }) {
  const tr = useI18n();
  const { tasks, preparing } = useStore(transferStore);
  const mine = useMemo(() => tasks.filter((x) => x.sessionId === session && x.direction === "upload"), [tasks, session]);
  const live = useMemo(() => mine.filter((x) => !isFinal(x.status)), [mine]);
  if (live.length === 0 && preparing === 0) return null;
  // 本子集只含 upload 方向，传独立 scopeKey 与全会话锚点隔离，避免冻结起点被互相覆盖。
  const { pct } = aggregateProgress(mine, `${session}:up`);
  return (
    <div className="up-strip">
      ↑ {live.length === 0 ? tr("ts.preparing") : `${tr("ts.uploading")} ${pct}%`}
    </div>
  );
}

/**
 * 传输终态事件：先交给 store 改任务行，再在**成功落盘**时刷一下目标栏，
 * 让用户立刻看到新文件（失败/取消时刷也没变化，不刷）。
 * 批量任务逐个完成时会连续来一串 done：按栏合并成一次重列，避免几十个文件刷几十次目录。
 */
const fileReload: Record<string, ReturnType<typeof setTimeout> | undefined> = {};
/** 按传输方向重列目标栏（上传→远端，下载→本地）；300ms 去抖合并一串 done。 */
function scheduleFileReload(direction: "upload" | "download", sessionId: string): void {
  const side: Side = direction === "upload" ? "remote" : "local";
  // 上传完时连接可能已经断了，此时该会话的远端分片已丢弃，别去白跑一趟
  if (side === "remote" && !(sessionId in fileStore.get().remotes)) return;
  // 去抖键按会话分开：同时传两个会话时不该互相抹掉对方的刷新
  const k = `${side}::${sessionId}`;
  clearTimeout(fileReload[k]);
  fileReload[k] = setTimeout(
    () => void files.load({ side, session: sessionId }, null, side === "remote"),
    300,
  );
}
function onTransferState(evt: TransferStateEvent): void {
  transferActions.applyState(evt);
  if (evt.status !== "done") return;
  const task = transferStore.get().tasks.find((t) => t.id === evt.taskId);
  if (task) {
    scheduleFileReload(task.direction, task.sessionId);
    return;
  }
  // done 早于建行（小文件竞态：worker 在 submit 回执 upsert 之前就广播了 done）：
  // applyState 已把它暂存、upsert 落地时会回放，但这里查不到方向就会漏掉刷新（上传完远端不刷新）。
  // 建行本身可能晚于 500ms（回执排队），故轮询复查至多 ~5s。
  waitRowThenReload(evt.taskId, 10);
}

/** 按 taskId 轮询复查传输行（500ms × tries 次），行一落地就补刷目标栏。 */
function waitRowThenReload(taskId: string, tries: number): void {
  if (tries <= 0) return;
  window.setTimeout(() => {
    const t = transferStore.get().tasks.find((x) => x.id === taskId);
    if (t) scheduleFileReload(t.direction, t.sessionId);
    else waitRowThenReload(taskId, tries - 1);
  }, 500);
}

export default function App() {
  const tr = useI18n();
  const { tabs, activeId, popped, error: sessErr } = useStore(sessionStore);
  // 待你回执的 AI 卡（澄清反问 / 命令审批）：按当前会话聚合，给右栏 AI 页签挂角标。
  // 只选 approvals/questions 两字段：AI 流式输出逐 token 写 turns，整 store 订阅会把整棵根树重渲染。
  const { approvals, questions } = useStorePick(aiStore, AI_QUEUE_KEYS);
  const { error: connErr } = useStore(connectionStore);
  const { error: txErr } = useStore(transferStore);
  const { root } = useStore(layoutStore);
  // 每条终端自己的 shell/agent 档（前端视图态）；用于把全局 terminal_ai 同步成当前聚焦终端的档
  const { bySession } = useStore(terminalAiStore);
  // 嵌套分屏树：树叶子 ≥2 且含 activeId 才分屏，否则单屏跟随 activeId；
  // 弹出独立窗口的会话不在主窗口渲染（避免两窗口同时挂同一会话的 Terminal）
  const splitTree = layoutActions.splitTree(activeId);
  const splitting = !!splitTree;
  const shownIds = splitTree ? nodeLeaves(splitTree) : activeId ? [activeId] : [];
  const visibleTabs = tabs.filter((t) => !popped[t.id]);
  const [dialog, setDialog] = useState<Dialog>(null);
  // 对话框内自行展示错误，避免与表单里的提示重复；toast 通知已改由 <Toasts/> 通知层独立渲染。
  const error = dialog ? "" : sessErr || connErr || txErr;
  // 根级弹窗单槽位（命令面板 / 快捷键帮助 / 用量统计 / 设置中心），见 RootModal 注释。
  const [rootModal, setRootModal] = useState<RootModal>({ kind: "none" });
  /** 收起当前根级弹窗。 */
  const closeRootModal = () => setRootModal({ kind: "none" });
  /** 直接切到某个根级弹窗（其余自动关）——从其它弹窗里跳转过去时用。 */
  const openRootModal = (kind: RootEntry, cat: CategoryId = "general") =>
    setRootModal(kind === "settings" ? { kind, cat } : { kind });
  /** 开关语义：同一个再触发一次就收起（顶栏按钮与快捷键同一口径）。
   *  只依赖 setRootModal，可安全被 [] 依赖的 capture 键盘监听闭包持有。 */
  const toggleRootModal = (kind: RootEntry, cat: CategoryId = "general") =>
    setRootModal((cur) =>
      cur.kind === kind ? { kind: "none" } : kind === "settings" ? { kind, cat } : { kind },
    );
  // 终端专注模式：临时隐藏左连接列 / 右停靠栏 / 底栏，让控制台铺满工作区（F11 或标签栏按钮切）。
  // 只做渲染层覆盖，不改动 collapseStore/panelStore 的持久值——退出即还原进入前的栏位状态。
  const [focusMode, setFocusMode] = useState(false);
  // 答疑直达（lib/faqJump）：任意界面问号调 jumpToFaq(编号) → 开面板并定位到那条
  const [faqFocus, setFaqFocus] = useState<number | null>(null);
  // 窗口焦点同步给 toastStore：失焦时连接失败/服务报错等事件旁路发 OS 横幅（离屏也能看到）。
  useEffect(() => {
    let un: UnlistenFn | null = null;
    selfWindow
      .onFocusChanged((focused) => setWinFocused(focused))
      .then((u) => {
        un = u;
      })
      .catch(() => {});
    return () => {
      un?.();
    };
  }, []);

  // 连接/会话/传输错误不走 toast 队列（只经 Toasts 的 error 内联常驻条展示），
  // 这里补 OS 旁路（通用文案，不含报错原文；聚焦时挂起、失焦补发），同一条只发一次。
  const osErrNotified = useRef("");
  useEffect(() => {
    if (!error || osErrNotified.current === error) return;
    osErrNotified.current = error;
    notifyOsBypass();
  }, [error]);

  useEffect(() => {
    setFaqListener((no) => {
      setFaqFocus(no);
      openRootModal("palette");
    });
    return () => setFaqListener(null);
  }, []);
  const [pluginButtons, setPluginButtons] = useState<(PluginUiButton & { pluginId: string })[]>([]);
  const [railW, setRailW] = useState(() => Number(localStorage.getItem("sailterm.railW") || 360));
  const [sideW, setSideW] = useState(() => Number(localStorage.getItem("sailterm.sideW") || 232));
  // 停靠布局：右侧栏面板列表/激活页（下栏列表由 BottomPanel 自读 dockStore）；分屏态也订阅一份：
  // Pane 渲染期直读 termSplitStore，但 App 需随其变化重渲染才能把 toggle 传到窗格。
  const { right, activeRight } = useStore(dockStore);
  useStore(termSplitStore);
  /** 聚焦会话：停靠面板按它取数，也按它的**能力**过滤页签（做不了的不占位，而不是点进去摆一句空文案）。 */
  const activeTab = tabs.find((x) => x.id === activeId) ?? null;
  // 只算当前会话：角标意味着“点开这个页签就能看到卡”，跨会话聚合会点了页签仍旧落空
  //（那是会话标签角标负责的：它标的是“哪条会话在等你”）。
  const awaitAi = !!activeTab && awaitingTracks(approvals, questions, activeTab.id).size > 0;
  /** 大视图展开时隐去停靠「文件」页签（同一会话不摆两份文件树，收起即回来；
   * dockStore 布局不动，纯展示层过滤）。条件与 Pane 的 splitOn 等价：主窗格分屏态下
   * 每个叶子窗格都算显示中，故按 shownIds 判；弹出窗口的会话主区不渲染，页签照常留在。 */
  const dockFilesHidden =
    MAIN_SPLIT_ENABLED &&
    !!activeTab &&
    termSplitStore.get().ids.has(activeTab.id) &&
    shownIds.includes(activeTab.id) &&
    !popped[activeTab.id];
  const rightPanels = visiblePanels(right, activeTab?.kind ?? null).filter(
    (id) => !dockFilesHidden || id !== "files",
  );
  // 边栏收起状态统一由 collapseStore 管理（左侧栏 / 右停靠栏同一套交互）——
  // 须先于下方 activeRightPanel 计算：那里要读 railCollapsed
  const { side: sideCollapsed, rail: railCollapsed } = useStore(collapseStore);
  /** 隐藏发生前 AI 页是否真的可见（一次大视图 toggle 内定稿）。看 AI 的用户不该被分屏
   * 扰动；activeRight 残留 "ai" 而用户其实没在看 AI 时，摆空栏也不能算"展开 AI 栏"——
   * 单靠 activeRight/railCollapsed 的此刻组合区分不了这两种，只能在动作前取事实。 */
  const aiVisibleBeforeHide = useRef<boolean | null>(null);
  if (!dockFilesHidden) aiVisibleBeforeHide.current = null;
  else if (aiVisibleBeforeHide.current === null)
    aiVisibleBeforeHide.current = activeRight === "ai" && !railCollapsed;
  // 激活页回落分两种：因隐藏而失焦（正停在文件页，或 activeRight 只是初始布局
  // right=["ai","files"] 的被动默认页）→置空，不能让「展开大视图」看起来像顺手
  // 展开了 AI 栏；只有一直在看 AI 的用户保持原样。页签真的不在本区了
  // （拖去另一区/脏键）→照旧回落第一个
  const activeRightPanel: DockPanelId | undefined = rightPanels.includes(activeRight)
    ? activeRight === "ai" && dockFilesHidden && !aiVisibleBeforeHide.current
      ? undefined
      : activeRight
    : activeRight === "files" && dockFilesHidden
      ? undefined
      : rightPanels[0];
  // 当前内容区（唯一真源，工具模块持有）：connections = 会话工作区，tools = 工具区，keys = 密钥库。
  // 三区完全独立，由左侧图标栏单点切换；areaActive = 非会话区（主区整页接管，终端隐藏保活、标签栏/右栏/底栏让位）。
  const { leftTab, activeTool } = useStore(toolsStore);
  const areaActive = leftTab !== "connections";
  const panelId = leftTab === "keys" ? "keys" : activeTool;
  /** 拖标签贴近窗口四边时的「松手弹出独立窗口」提示。 */
  const [edgePop, setEdgePop] = useState(false);
  /** 首页页签态：true 时 Welcome 覆盖工作区（pane 隐藏但保活，终端不销毁）。 */
  const [homeOpen, setHomeOpen] = useState(false);
  // 应用级问答（无连接 AI 弹框 AppChatDialog）暂未开放：入口已全部撤下，
  // 组件与后端 APP_CHAT_SESSION 保留，放开时接回顶栏按钮与本文件三处即可。
  // 从侧栏等其他入口切换会话时退出首页态
  useEffect(() => {
    setHomeOpen(false);
  }, [activeId]);
  // 后端 run_command 用全局 ai.terminal_ai 决定命令打进 PTY 还是独立 exec（它不分会话）。
  // per-session 开关只是前端视图态，这里把全局同步成「当前聚焦终端」的档，保证你正在用的
  // 那条终端的 AI 命令按它自己的模式路由；新终端默认 agent（见 terminalAiStore.ensure）。
  // 聊天唯一形态的会话（canAgentRail 不过）恒 off——不拿快照时序赌，判据在能力表现算。
  useEffect(() => {
    if (!activeId) return;
    const want =
      activeTab && !canAgentRail(activeTab.kind)
        ? "off"
        : bySession[activeId] ?? settingsStore.get().terminalAi;
    if (settingsStore.get().terminalAi !== want) void settingsActions.update({ terminalAi: want });
  }, [activeId, bySession, activeTab]);
  // 订阅 terminalStore（本地终端入口现统一走 openLocalTerminal 按时刻快照读取，不再需要 reactive localShell）。
  useStore(terminalStore);

  // 桌面壳防浏览器化：拦截 F5/F12/Ctrl+R 等，并依开发者模式同步 devtools
  useBrowserGuards();

  // 客户端壳：屏蔽浏览器默认右键（终端/输入框自行处理）；非开发者模式下连输入框也屏蔽以杜绝原生「检查」
  useEffect(() => {
    const onCtx = (e: MouseEvent) => {
      if (allowNativeContextMenu(e.target as HTMLElement | null)) return;
      e.preventDefault();
    };
    document.addEventListener("contextmenu", onCtx);
    return () => document.removeEventListener("contextmenu", onCtx);
  }, []);

  // 启动：读取本地连接与应用信息，并挂上后端事件（会话状态 / 主机密钥确认 / 传输进度与终态）
  useEffect(() => {
    void connections.refresh();
    void appearanceActions.refresh();
    void providerActions.refresh();
    // 先注册插件语料（manifest.locales），再拉设置应用语言偏好：
    // 顺序不能反——resolveLang 需要插件 code 已在注册表里，否则「跟随系统」会误回落内置语言。
    void registerPluginLocales().then(() => settingsActions.refresh());
    // WebView 整页刷新（开发时 HMR、运行时 Ctrl+R）会把标签列表清成空，
    // 但后端会话还活着；不同步一次就会留下一批无人认领的孤儿连接。
    void sessionActions.sync();
    void sessionActions.refreshRtt();
    // RTT 只在窗口可见时轮询：最小化/后台时省掉无意义的 SSH 往返；回到前台立即补一次。
    const refreshRtt = () => {
      if (!document.hidden) void sessionActions.refreshRtt();
    };
    const rttTimer = window.setInterval(refreshRtt, 15000);
    document.addEventListener("visibilitychange", refreshRtt);

    void transferActions.refresh();
    void triggerActions.refresh();
    void pluginApi
      .list()
      .then((list) => {
        const bs: (PluginUiButton & { pluginId: string })[] = [];
        for (const pl of list) {
          for (const b of pl.manifest.buttons ?? []) {
            if ((b.target ?? "topbar") === "topbar") bs.push({ ...b, pluginId: pl.manifest.id });
          }
        }
        setPluginButtons(bs);
      })
      .catch(() => {});

    let cancelled = false;
    const offs: Array<() => void> = [];
    const keep = (u: () => void) => (cancelled ? u() : offs.push(u));
    sshApi.onState(sessionActions.applyState).then(keep);
    // 建连阶段事件：追加到对应预加载占位标签的进度日志（按 connectId 归集）。
    sshApi.onStage(sessionActions.applyStage).then(keep);
    hostKeys.onPrompt(hostKeyDecisions.push).then(keep);
    sshKbd.onPrompt(kbdDecisions.push).then(keep);
    ftpsCerts.onPrompt(ftpsCertDecisions.push).then(keep);
    transferApi.onProgress(transferActions.applyProgress).then(keep);
    transferApi.onState(onTransferState).then(keep);
    transferApi.onPhase(transferActions.applyPhase).then(keep);
    // 打包失败回退：摘掉已删的 bundle 行 + 刷回逐文件新行 + 提示（不阻断，逐文件照常推进）。
    transferApi
      .onFallback((p) => {
        const task = transferStore.get().tasks.find((t) => t.id === p.taskId);
        transferActions.drop(p.taskId);
        void transferActions.refresh(task?.sessionId ?? null);
        toast.show(tr("bundle.fallback").replace("{reason}", p.reason));
      })
      .then(keep);
    // AI 编排步骤（流式时间线）与命令审批请求（弹审批卡片）；
    // 已弹出独立窗口的会话归那个窗口管（ownsSession），避免两窗同时弹同一审批
    aiApi.onStep((evt) => {
      if (ownsSession(evt.sessionId)) aiChat.applyStep(evt);
    }).then(keep);
    aiApi.onApproval((p) => {
      if (ownsSession(p.sessionId)) aiChat.pushApproval(p);
    }).then(keep);
    aiApi.onQuestion((p) => {
      if (ownsSession(p.sessionId)) aiChat.pushQuestion(p);
    }).then(keep);
    aiApi
      .onStream((p) => {
        if (ownsSession(p.sessionId)) aiChat.appendStream(p.sessionId, p.delta, p.turn, p.kind);
      })
      .then(keep);
    // 审批重同步：监听挂上**之前**已挂起的审批（如弹出窗口接管了会话后本窗事件断档）错过即无卡可点，
    // 而后端 run_turn 正挂在回执上等裁决——拉一次后端快照重入队（pushApproval 自带去重，与实时事件竞态无害）。
    aiApi
      .listPendingApprovals()
      .then((ps) => {
        if (cancelled) return;
        for (const p of ps) if (ownsSession(p.sessionId)) aiChat.pushApproval(p);
      })
      .catch(() => {});
    // 反问同口径重同步：挂起的反问同样只能从后端内存快照拉起（pushQuestion 自带去重）。
    aiApi
      .listPendingQuestions()
      .then((ps) => {
        if (cancelled) return;
        for (const p of ps) if (ownsSession(p.sessionId)) aiChat.pushQuestion(p);
      })
      .catch(() => {});

    // 终端内 ZMODEM：远端敲 rz/sz 时弹原生文件框，选完交给后端接管
    import("./lib/tauri").then(async ({ on }) => {
      keep(
        await on<{ sessionId: string; direction: "upload" | "download" }>(
          "zmodem-request",
          async ({ sessionId, direction }) => {
            if (!ownsSession(sessionId)) return; // 弹出窗口的会话由那个窗口弹文件框
            // 后端自建非模态系统框（不设父窗口）：主窗口不被禁用，
            // 远程桌面下光标也不会“消失”；取消返回 null → 发 CAN 恢复终端
            document.body.classList.add("zm-dialog-open");
            try {
              const sel = await zmodemApi.pick(
                direction,
                tr(direction === "upload" ? "dlg.zmodemUpload" : "dlg.zmodemDownload"),
              );
              if (sel) {
                // 选完立即给反馈，避免用户以为没反应；同时挂一条本地传输行
                // （终端内 ZMODEM 不走后端传输队列，列表行由前端维护）
                toast.show(
                  direction === "upload" ? tr("pw.uploading") : tr("pw.downloading"),
                );
                const now = Date.now();
                transferActions.addLocal({
                  id: `zm-${sessionId}-${now}`,
                  sessionId,
                  direction,
                  remotePath: "",
                  localPath: sel,
                  totalBytes: 0,
                  doneBytes: 0,
                  status: "running",
                  error: null,
                  createdAt: now,
                  updatedAt: now,
                });
                await zmodemApi.start(sessionId, sel);
              } else {
                await zmodemApi.cancel(sessionId);
              }
            } catch {
              zmodemApi.cancel(sessionId).catch(() => {});
            } finally {
              document.body.classList.remove("zm-dialog-open");
            }
          },
        ),
      );
      // 传输开始后后端广播文件名/大小，补全本地传输行
      keep(
        await on<{ sessionId: string; name: string; size: number }>(
          "zmodem-file",
          ({ sessionId, name, size }) => {
            transferActions.updateLocal(sessionId, (t) => ({
              ...t,
              remotePath: name,
              totalBytes: size,
            }));
          },
        ),
      );
      keep(
        await on<{ sessionId: string; ok: boolean; message: string }>(
          "zmodem-done",
          ({ sessionId, ok, message }) => {
            transferActions.settleLocal(sessionId, ok ? "done" : "failed", message || null);
            toast.show(
              ok ? tr("pw.zmDone") : tr("pw.zmFail").replace("{m}", message),
              ok ? "success" : "error",
            );
          },
        ),
      );
    });

    return () => {
      cancelled = true;
      clearInterval(rttTimer);
      document.removeEventListener("visibilitychange", refreshRtt);
      offs.forEach((u) => u());
    };
  }, []);

  // 拖标签到窗口边缘（≤24px）弹出独立窗口：WebView 收不到窗口外的 drop，
  // 故用 body 级 dragover 检测贴边 + 全屏 overlay 提示，drop 在边缘区即弹出
  useEffect(() => {
    const EDGE = 24;
    const atEdge = (e: DragEvent) =>
      e.clientX <= EDGE ||
      e.clientY <= EDGE ||
      e.clientX >= window.innerWidth - EDGE ||
      e.clientY >= window.innerHeight - EDGE;
    const onOver = (e: DragEvent) => {
      if (!e.dataTransfer?.types.includes("text/session-id")) return;
      setEdgePop(atEdge(e));
    };
    const onLeave = (e: DragEvent) => {
      if (e.relatedTarget === null) setEdgePop(false); // 真正离开窗口
    };
    const onDrop = (e: DragEvent) => {
      if (!edgePop || !e.dataTransfer) return;
      const id = e.dataTransfer.getData("text/session-id");
      if (!id || !atEdge(e)) return;
      e.preventDefault();
      const tab = sessionStore.get().tabs.find((t) => t.id === id);
      if (tab) popTabWindow(tab);
    };
    const onEnd = () => setEdgePop(false);
    document.body.addEventListener("dragover", onOver);
    document.body.addEventListener("dragleave", onLeave);
    document.body.addEventListener("drop", onEnd);
    document.body.addEventListener("dragend", onEnd);
    window.addEventListener("drop", onDrop);
    return () => {
      document.body.removeEventListener("dragover", onOver);
      document.body.removeEventListener("dragleave", onLeave);
      document.body.removeEventListener("drop", onEnd);
      document.body.removeEventListener("dragend", onEnd);
      window.removeEventListener("drop", onDrop);
    };
  }, [edgePop]);

  // 展开/收起 AI 聊天面板（Ctrl+Shift+A 与命令面板「AI 聊天」命令共用同一实现）。
  // 只读各 store 的当前快照（均经 .get()），故可安全地在空依赖的快捷键 effect 里捕获。
  const toggleAiChat = () => {
    // 无可用会话（没标签 / 未连接 / 协议无 AI 通路）：不响应——应用级问答暂未开放。
    const ss = sessionStore.get();
    const tab = ss.tabs.find((t) => t.id === ss.activeId);
    const sessionAiOk =
      !!tab && !tab.phase && tab.state === "connected" && hasAnyCap(tab.kind, ["exec", "fs", "pty"]);
    if (!sessionAiOk) return;
    const zone = dockZoneOf("ai");
    const aiVisible =
      zone === "right"
        ? !collapseStore.get().rail && dockStore.get().activeRight === "ai"
        : panelStore.get().open && panelStore.get().tab === "ai";
    if (settingsStore.get().rail === "chat" && aiVisible) {
      if (zone === "right") collapseActions.toggle("rail");
      else panel.close();
    } else {
      void settingsActions.update({ rail: "chat" });
      dockActions.reveal("ai");
    }
  };

  // 发送（多机脚本）：跨会话发射器，恢复为弹独立工具窗口（不进工具区，也不列在启动器）。
  const openSendTool = () => {
    void openToolWindow("send", `${tr("tool.send.name")} — SailTerm`);
  };

  // 打开本地终端：无论从哪个区触发都先跳回会话区，免得终端在背后被隐藏、看着像没反应（三区自动跳区）。
  const openLocalTerminal = () => {
    toolsActions.setLeftTab("connections");
    void sessionActions.openLocal(terminalStore.get().localShell || null);
  };

  // 全局快捷键（专业工具习惯）：Ctrl+Shift+A 抽屉 · Ctrl+Shift+, 设置 · Ctrl+Shift+B 发送
  // Ctrl+Shift+R 录制 · Ctrl+Shift+L 本地终端 · Ctrl+Shift+M / F11 终端专注（放大）· Ctrl+1..9 切标签
  // Ctrl/Cmd+T 新开终端 · Ctrl/Cmd+W 关标签 · Ctrl/Cmd+N 新建连接（均在下方单独处理）
  // Esc 关顶层弹层不再靠本文件：由各弹层调 useEscClose 入栈（lib/useOverlay），单一仲裁点、只关最上一层。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (!mod || !e.shiftKey) return;
      const k = e.key.toLowerCase();
      const stop = () => {
        e.preventDefault();
        e.stopPropagation();
      };
      if (k === "a") {
        stop();
        toggleAiChat();
      } else if (k === "," || k === "；") {
        stop();
        toggleRootModal("settings");
      } else if (k === "b") {
        // 无论面板是否可达都吞下 Ctrl+Shift+B，免得落到 WebView 的默认行为。
        // 键位沿用自旧「多机群发」：列表头的「全选」就是群发的后继。
        stop();
        openSendTool();
      } else if (k === "r") {
        // 无论录制开关与否都先吞下 Ctrl+Shift+R，避免落到 WebView 的硬刷新；录制关时不再开面板。
        stop();
        if (FEATURES.recording) toolsActions.openTool("record");
      } else if (k === "l") {
        stop();
        openLocalTerminal();
      } else if (k === "m") {
        stop();
        // Ctrl/Cmd+Shift+M：终端专注模式开关（隐藏左列/右栏/底栏，控制台铺满）。
        setFocusMode((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // F11：终端专注（放大/还原）。capture 阶段先于 WebView 处理，preventDefault 取消
  // WebView2 可能的原生全屏，stopPropagation 避免 F11 下发给终端/输入区。全局生效（含焦点在终端）。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "F11" || e.ctrlKey || e.metaKey || e.altKey) return;
      e.preventDefault();
      e.stopPropagation();
      setFocusMode((v) => !v);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  // 命令面板（Ctrl+Shift+P / F1）与快捷键帮助（Ctrl+/）：capture 阶段抢在 xterm 之前，
  // preventDefault + stopPropagation 既挡 WebView 默认（F1 原生帮助），又避免键位下发终端。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (e.key === "F1" && !e.ctrlKey && !e.metaKey && !e.altKey) {
        e.preventDefault();
        e.stopPropagation();
        toggleRootModal("palette");
        return;
      }
      if (mod && e.shiftKey && !e.altKey && e.key.toLowerCase() === "p") {
        e.preventDefault();
        e.stopPropagation();
        toggleRootModal("palette");
        return;
      }
      if (mod && !e.shiftKey && !e.altKey && (e.key === "/" || e.code === "Slash")) {
        e.preventDefault();
        e.stopPropagation();
        toggleRootModal("help");
        return;
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  // Ctrl+1..9 切标签；Ctrl+= / - / 0 终端字号
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.shiftKey || e.altKey) return;
      const n = Number(e.key);
      if (n >= 1 && n <= 9) {
        const t = sessionStore.get().tabs[n - 1];
        if (t) {
          e.preventDefault();
          sessionActions.select(t.id);
        }
        return;
      }
      // 字号缩放（与 VS Code 习惯一致）
      const zoom = (delta: number | "reset") => {
        e.preventDefault();
        const cur = appearanceStore.get().fontSize;
        const next =
          delta === "reset" ? 13 : Math.min(32, Math.max(8, Math.round(cur + delta)));
        void appearanceActions.update({ fontSize: next });
      };
      if (e.key === "=" || e.key === "+") zoom(1);
      else if (e.key === "-") zoom(-1);
      else if (e.key === "0") zoom("reset");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // 新开终端 / 新建连接 / 关标签（Ctrl+T / Ctrl+N / Ctrl+W，Mac 上为 ⌘）：终端软件的通行键位，
  // 此前完全没接——按下去毫无反应。
  // **关键门控**：这三个组合在 readline 里都有本义（Ctrl+T transpose-chars、Ctrl+N 下条历史、
  // Ctrl+W 删前一个词），焦点在终端内时一律放行给 shell，只在终端之外才当 app 级键位接管。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.shiftKey || e.altKey) return;
      if ((document.activeElement as HTMLElement | null)?.closest(".xterm")) return;
      const k = e.key.toLowerCase();
      if (k === "t") {
        e.preventDefault();
        openLocalTerminal();
      } else if (k === "n") {
        e.preventDefault();
        openNew();
      } else if (k === "w") {
        // 没得关时不吞键（避免 ⌘W 在空状态把 WebView 的默认行为拦下来）。
        const id = sessionStore.get().activeId;
        if (!id) return;
        e.preventDefault();
        void sessionActions.close(id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // openNew 只读 store 快照 + 稳定 setter，无需进依赖。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 系统文件拖入的**全局落点路由**（整窗只挂这一份；FileBrowser 不再各自订阅）：
  //  - 落在看得见的远端文件栏 → 按「那块栏自己的会话」传到其当前目录（分屏/双会话不错台）；
  //  - 落在终端画布（含 pane-head）→ 快速上传到该窗格会话的远端当前目录，不必先开文件面板；
  //  - 其余落点（页签轨/空白侧栏区等）不响应——保守版，误拖不会默默传文件。
  // 反馈：submitBatch 的 preparing 占位 → 终端底 UploadStrip / 面板内 TransferStrip，不弹 toast。
  useEffect(() => {
    let cancelled = false;
    let un: UnlistenFn | undefined;
    onOsFileDrop((paths, at) => {
      if (paths.length === 0) return;
      const el = document.elementFromPoint(at.x, at.y);
      if (!el) return;
      const pane = el.closest<HTMLElement>('[data-side="remote"]');
      if (pane) {
        const sid = pane.dataset.session || activeId;
        if (sid) void uploadPaths(sid, paths);
        return;
      }
      if (!el.closest(".term-stack, .pane-head")) return;
      const sid = el.closest<HTMLElement>(".term-pane")?.dataset.session || activeId;
      const tab = sid ? sessionStore.get().tabs.find((x) => x.id === sid) : null;
      if (!tab || tab.state !== "connected" || !hasCap(tab.kind, "fs")) return;
      const dst = paneOf(fileStore.get(), { side: "remote", session: sid! }).path;
      void uploadPaths(sid!, paths);
      toast.show(tr("ts.uploadedTo").replace("{p}", dst || "/"));
    })
      .then((u) => {
        if (cancelled) u();
        else un = u;
      })
      // 纯浏览器预览（非 Tauri 环境）拿不到 webview，静默降级为不支持系统拖放
      .catch(() => {});
    return () => {
      cancelled = true;
      un?.();
    };
  }, [activeId, tr]);

  // 分屏记忆自愈：关闭的会话从 termSplitStore 摘除（持久化键不留死 id）。
  useEffect(() => {
    const alive = new Set(tabs.map((x) => x.id));
    for (const id of termSplitStore.get().ids) if (!alive.has(id)) splitActions.forget(id);
  }, [tabs]);

  // 远端分片按需加载：FTP 系连上即拉（文件就是它的主视图）；SSH 只拉聚焦那一个——
  // 它的文件在右侧栏，犯不着给每个后台会话都先列一次目录。切走的会话分片**保留**
  // （下次切回来还在原来那个目录），只有断线或关闭才丢弃（SFTP 通道与 FTP 控制连接都随会话失效）。
  useEffect(() => {
    const alive = new Set(tabs.map((x) => x.id));
    for (const t of tabs) {
      const mainFiles = !kindHasTerminal(t.kind); // 无终端：文件即主区，必须可见
      // 开了「终端 | 文件」分屏的会话也要即拉：大视图就是给它看目录的，不能等聚焦调度那个 effect 再补
      if (
        t.state === "connected" &&
        kindHasFiles(t.kind) &&
        (mainFiles || (MAIN_SPLIT_ENABLED && termSplitStore.get().ids.has(t.id)) || t.id === activeId)
      ) {
        void files.open(t.id);
      } else if (t.state !== "connected") {
        files.drop(t.id);
      }
    }
    // 会话已关闭：分片一并回收（不挂在关闭路径上，靠这里自愈）
    for (const id of Object.keys(fileStore.get().remotes)) {
      if (!alive.has(id)) files.drop(id);
    }
  }, [tabs, activeId]);

  // 分屏树自愈：关闭 / 弹出的会话从树中剪除（分支坍缩为兄弟），无需手动干预
  useEffect(() => {
    layoutActions.prune();
  }, [tabs, popped]);

  // 分屏自愈：聚焦标签被切到分屏树之外时退出分屏（回单屏），避免「聚焦项不在可见组」。
  useEffect(() => {
    if (root && activeId && !nodeLeaves(root).includes(activeId)) layoutActions.reset();
  }, [activeId, root]);

  const dismissError = () => {
    sessionActions.clearError();
    connections.clearError();
    transferActions.clearError();
    toast.clear();
  };
  const openNew = () => {
    dismissError();
    // 连接表单属会话区：无论从哪个区触发（顶栏 / Ctrl+N / 命令面板）都先跳回会话区。
    toolsActions.setLeftTab("connections");
    setDialog({ mode: "new" });
  };
  const openEdit = (profile: Profile) => {
    dismissError();
    toolsActions.setLeftTab("connections");
    setDialog({ mode: "edit", profile });
  };

  // 预加载失败卡的「编辑主机」出口：注册真实编辑动作（按 profileId 找档案并打开编辑表单），
  // 深层的 ConnectProgress 经 connectUi 单例调用，无需层层透传 onEdit。
  useEffect(() => {
    connectUi.setEditProfile((profileId) => {
      const p = connectionStore.get().profiles.find((x) => x.id === profileId);
      if (p) openEdit(p);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 命令面板 / 快捷键帮助共用的执行上下文：把注册表里的抽象命令接到 App 的真实动作上。
  // 非响应式：openNew/toggleAiChat 只读 store 快照，setter 均为稳定函数，每次渲染重建无副作用。
  const commandCtx: CommandCtx = {
    openConnectNew: () => openNew(),
    newLocalTerminal: openLocalTerminal,
    closeActiveTab: () => {
      const id = sessionStore.get().activeId;
      if (id) void sessionActions.close(id);
    },
    toggleAiChat,
    openSendTool,
    toggleFocus: () => setFocusMode((v) => !v),
    // 锁屏：统一走 requestLock——未设 PIN 它会自动拉起「设置 PIN」弹窗（PinSetupDialog），
    // 已设则直接锁屏；多窗口同步靠后端事件，这里不另翻本地态。
    lockNow: () => void requestLock(),
    openUsageStats: () => openRootModal("usage"),
    openSettings: (cat: SettingsCat) => openRootModal("settings", cat),
    openCommandPalette: () => openRootModal("palette"),
    toggleShortcutHelp: () => toggleRootModal("help"),
  };

  return (
    <div className="app">
      <header
        className="topbar"
        data-tauri-drag-region="deep"
        // 双击放大/还原由 Tauri 核心 drag.js 统一处理（macOS 在 mouseup 触发、移动即取消）；
        // 这里再挂 onDoubleClick→toggleMaximize 会二次触发，导致放大后瞬间还原
      >
        <WindowControls />
        {/* 顶栏空白区作为拖拽区（继承 topbar 的 deep 拖拽），仅交互控件显式排除 */}
        <div className="topbar-main">
          {/* 凭证后端、安全状态灯属非实时信息，已收进设置中心（关于页 / 安全页），顶栏不再常驻 */}
          {/* 命令与答疑入口（与 Ctrl+Shift+P / F1 同效）：问号图标，区别于旁边的终端按钮 */}
          <button
            className="mem-toggle" data-tauri-drag-region="false"
            title={kbdLabel(tr("cmd.paletteTip"))}
            onClick={() => toggleRootModal("palette")}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <circle cx="12" cy="12" r="10" />
              <path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3" />
              <path d="M12 17h.01" />
            </svg>
          </button>
          <button
            className="mem-toggle" data-tauri-drag-region="false"
            title={tr("topbar.local.title")}
            onClick={openLocalTerminal}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <polyline points="4 17 10 11 4 5" />
              <line x1="12" y1="19" x2="20" y2="19" />
            </svg>
          </button>
          {/* 发送：跨会话的全局工具，弹独立工具窗口（不进下栏——那里按会话记开合） */}
          <button
            className="mem-toggle" data-tauri-drag-region="false"
            title={`${tr("tool.send.name")} · ${kbdLabel("Ctrl+Shift+B")}`}
            onClick={openSendTool}
          >
            <IconSend width={14} height={14} />
          </button>
          {/* 锁屏：手动锁入口只留两个——顶栏按钮与 Ctrl+Alt+L（设置页只管 PIN/档位，
              不放立即锁屏按钮）；未设 PIN 时走 lockNow 引导跳安全设置。 */}
          <button
            className="mem-toggle" data-tauri-drag-region="false"
            title={`${tr("cmd.lock")} · ${kbdLabel("Ctrl+Alt+L")}`}
            onClick={() => commandCtx.lockNow()}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
              <rect x="3" y="11" width="18" height="11" rx="2" />
              <path d="M7 11V7a5 5 0 0 1 10 0v4" />
            </svg>
          </button>
          {FEATURES.recording && (
            <button className="mem-toggle" data-tauri-drag-region="false" title={tr("topbar.record.title")} onClick={() => toolsActions.openTool("record")}>
              {tr("topbar.record")}
            </button>
          )}
          {FEATURES.plugins &&
            pluginButtons.map((b) => (
              <button
                key={`${b.commandId}-${b.id}`}
                className="mem-toggle"
                data-tauri-drag-region="false"
                title={b.title}
                onClick={() => {
                  // 插件结果回显到当前会话终端：先跳回会话区，免得在工具/密钥库区背后静默写入看不见。
                  toolsActions.setLeftTab("connections");
                  const sid = activeId ?? "";
                  void pluginApi
                    .run(b.pluginId, b.commandId, sid, "")
                    .then((r) => {
                      // 与文件栏 ⚡ 菜单同一口径：结果回显到当前会话终端，不再静默丢弃
                      if (!writePluginOutput(sid, r.command, r.stdout, r.stderr, r.exitCode))
                        toast.show(r.command);
                    })
                    .catch((e) => toast.error(String((e as Error)?.message ?? e)));
                }}
              >
                {b.icon || b.title.slice(0, 2)}
              </button>
            ))}
          <button
            className="mem-toggle" data-tauri-drag-region="false"
            title={tr("aw.statsTip")}
            onClick={() => toggleRootModal("usage")}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
              <path d="M5 20V10" />
              <path d="M12 20V4" />
              <path d="M19 20v-7" />
            </svg>
          </button>
          {/* 通知历史（铃铛）：入口暂隐藏——重大事件已改走 OS 横幅，组件与 toast 历史链路保留，
              放开时接回本行即可。 */}
          {/* <NotificationsMenu /> */}
          <button
            className="mem-toggle" data-tauri-drag-region="false"
            title={tr("settings.title")}
            onClick={() => toggleRootModal("settings")}
          >
            <IconGear width={14} height={14} />
          </button>
        </div>
      </header>

      <div className="body">
        {/* 左侧常驻页签轨（对齐下边栏）：页签条始终显示，展开/收起只切换旁边的连接列表。
            专注模式下连窄轨一起隐藏，让终端真正铺满（退出靠标签栏按钮 / Ctrl+Shift+M）。 */}
        {!focusMode && (
          <SideRail
            side="left"
            tabs={[
              { id: "connections", label: "rail.connections", icon: RAIL_ICONS.connections, hint: "rail.connections.hint" },
              { id: "keys", label: "rail.keys", icon: <IconKey />, hint: "rail.keys.hint" },
              { id: "tools", label: "rail.tools", icon: <IconWrench />, hint: "rail.tools.hint" },
            ]}
            activeId={leftTab}
            open={!sideCollapsed}
            onSelect={(id) => {
              // 三区单点切换（唯一真源 leftTab）：点当前页 = 收起/展开；点另一页 = 切区并在收起态下展开。
              if (id === leftTab) collapseActions.toggle("side");
              else {
                toolsActions.setLeftTab(id as "connections" | "tools" | "keys");
                if (sideCollapsed) collapseActions.set("side", false);
              }
            }}
            onToggle={() => collapseActions.toggle("side")}
          />
        )}
        {/* 左侧栏内容（连接列表 / 工具启动器二选一）通底占满整个页面高度；底栏只在它们右侧铺开。
            密钥库是自成一页的整页视图，不需要左侧栏：keys 区整块隐去，主区从图标栏一直铺到右边缘。 */}
        {!focusMode && !sideCollapsed && leftTab !== "keys" && (
          <>
            <div
              className="sidebar-wrap"
              style={{ width: sideW, overflow: "hidden", flex: "none", height: "100%", alignSelf: "stretch", display: "flex", flexDirection: "column" }}
            >
              {leftTab === "tools" ? (
                <ToolsSidebar />
              ) : (
                <Sidebar
                  onNew={openNew}
                  onEdit={openEdit}
                  selectedId={dialog?.mode === "edit" ? dialog.profile.id : null}
                />
              )}
            </div>
            <Splitter
              axis="x"
              title={tr("app.sideResize")}
              onDelta={(d) =>
                setSideW((w) => {
                  const n = Math.min(420, Math.max(160, w + d));
                  localStorage.setItem("sailterm.sideW", String(n));
                  return n;
                })
              }
            />
          </>
        )}
        {/* 连接编辑侧面板：master-detail，作为浮层贴在左侧栏右边缘（不挤动控制台）。
            left = rail(40) + 侧栏(sideW) + 分隔条(3)；收起时只剩 rail，专注模式两者皆隐。
            连接表单只属于会话区：切到工具 / 密钥库区时隐去（display:none 保持挂载、不丢已填内容），回到会话区重现。 */}
        {dialog && (
          <div style={{ display: leftTab === "connections" ? "contents" : "none" }}>
            <ConnectPanel
              profile={dialog.mode === "edit" ? dialog.profile : null}
              left={focusMode ? 0 : sideCollapsed ? 40 : 40 + sideW + 3}
              visible={leftTab === "connections"}
              onClose={() => setDialog(null)}
            />
          </div>
        )}
        {/* 连接列右侧是一列：上=会话标签栏（横跨终端+右停靠整幅），中=工作区行，下=底栏 */}
        <div className="body-col">
        {/* 会话标签页置顶横跨「终端 + 右停靠栏」整幅：右栏改为从标签栏下方起，
            不再与顶部连接切换栏同高，连接多时标签不被右栏挤占。
            全局面板（工具 / 密钥库）整页接管时隐去标签栏，使其成为独立一屏。 */}
        {!areaActive && (
          <TabBar
            homeActive={homeOpen || tabs.length === 0}
            onHome={() => setHomeOpen(true)}
            onNavigate={() => setHomeOpen(false)}
            focusMode={focusMode}
            onToggleFocus={() => setFocusMode((v) => !v)}
          />
        )}
        <div className="body-row">

        <main className="main">
          {/* 全局面板（工具 / 密钥库）整页接管主区；term-host 只 display:none 隐藏，pane 保持挂载不重建 xterm。 */}
          {areaActive && <GlobalPanel id={panelId} />}
          <div className="term-host" data-split={splitting} style={areaActive ? { display: "none" } : undefined}>
            {tabs.length === 0 ? (
              <Welcome />
            ) : (
              <>
                {homeOpen && <Welcome />}
                {/* 首页态只隐藏不卸载：xterm 实例与滚动历史保留；display:contents 不破坏 flex 布局 */}
                <div style={{ display: homeOpen ? "none" : "contents" }}>
                  {splitting && splitTree ? (
                    <>
                      {/* 分屏树之外的标签仍隐藏挂载（保住 xterm 实例与滚动历史），只留 Terminal */}
                      {visibleTabs.map((t) =>
                        shownIds.includes(t.id) ? null : (
                          <div key={t.id} className="term-pane" data-active={false}>
                            {kindHasTerminal(t.kind) && !t.phase && (
                              <Terminal sessionId={t.id} active={false} />
                            )}
                          </div>
                        ),
                      )}
                      <SplitView
                        node={splitTree}
                        tabs={tabs}
                        activeId={activeId}
                      />
                    </>
                  ) : (
                    /* 单屏：所有标签固定同处一个 keyed map，切标签只翻 data-active。
                       每个会话的 Terminal 树位置恒定，不再因跨「隐藏 bare pane / 活动 Pane」
                       两棵子树移动而被 React 卸载重建——重建后空 xterm 会回退去重放后端
                       16KB 原始 PTY 字节，夹了 vim 全屏程序就糊屏（切标签乱码根因）。 */
                    visibleTabs.map((t) => (
                      <Pane
                        key={t.id}
                        tab={t}
                        head={false}
                        active={t.id === activeId}
                        focus={t.id === activeId}
                      />
                    ))
                  )}
                </div>
              </>
            )}
          </div>
        </main>

        {/* 右侧停靠栏（全高）：AI 面板 + 从下栏拖来的面板；内容跟随聚焦会话。
            与底栏对称——只有「正在看某个会话」时才出现：无会话(tabs=0)或回到首页(homeOpen)
            时两者一起隐去（Welcome 保持清爽）；专注模式与全局面板接管时也隐去。收起/展开走 collapseStore.rail */}
        {!focusMode && !areaActive && !homeOpen && tabs.length > 0 && (rightPanels.length > 0 ? (
          <>
            {/* 无激活页（因隐藏文件页签而失焦）就不渲染内容区：只留 SideRail 窄轨。
                否则收起态下点展开大视图，会凭空多出一块空白宽栏，看上去就像"AI 栏被顺手展开了"。 */}
            {!railCollapsed && activeRightPanel && (
              <>
                <Splitter
                  axis="x"
                  title={tr("pw.resizeTip")}
                  onDelta={(d) =>
                    setRailW((w) => {
                      const n = Math.min(640, Math.max(260, w - d));
                      localStorage.setItem("sailterm.railW", String(n));
                      return n;
                    })
                  }
                />
                <div
                  className="rail-content"
                  style={{
                    width: railW,
                    flex: "none",
                    minWidth: 0,
                    display: "flex",
                    overflow: "hidden",
                  }}
                >
                  {activeRightPanel === "ai" ? (
                    <DockPanelBody id="ai" tab={activeTab} zone="right" />
                  ) : (
                    <div className="panel-body">
                      <DockPanelBody id={activeRightPanel} tab={activeTab} zone="right" />
                    </div>
                  )}
                </div>
              </>
            )}
            <SideRail
              side="right"
              tabs={rightPanels.map((id) => ({
                id,
                label: DOCK_META[id].label,
                icon: DOCK_ICONS[id],
                hint: DOCK_META[id].hint,
                // AI 栏被收起时反问 / 审批卡完全不可见，那一轮就一直挂着等回执
                awaiting: id === "ai" && awaitAi,
              }))}
              activeId={activeRightPanel ?? ""}
              open={!railCollapsed}
              onSelect={(id) => dockActions.showRight(id as DockPanelId)}
              onToggle={() => collapseActions.toggle("rail")}
              onPanelDrop={(id) => dockActions.move(id as DockPanelId, "right")}
            />
          </>
        ) : (
          /* 右栏被拖空：平时不占位，仅拖页签时（body.dock-dragging）浮出这条虚线投放轨，把面板拖回右栏 */
          <aside className="side-rail right rail-empty-slot" data-drop-zone="right" />
        ))}
        </div>
        {/* /body-row ↑ · 底栏 ↓：横跨「终端 + 右停靠」的宽度（左侧连接列通底，不被底栏压到）；
            与右栏同门控：回首页/无会话时一并隐去；专注模式也隐去；
            聚焦会话在看大视图时下栏「文件」页签也隐去（与右栏同源，dockFilesHidden） */}
        {!focusMode && !areaActive && !homeOpen && tabs.length > 0 && (
          <BottomPanel hideFiles={dockFilesHidden} />
        )}
        </div>
      </div>

      <Toasts error={error} onDismissError={dismissError} />

      {/* 有「重启后生效」的设置被改动时，右下角常驻一键重启徽标 */}
      <RestartChip />

      {/* 拖标签贴窗口边缘：松手弹出为独立窗口的全屏提示 */}
      {edgePop && (
        <div className="edge-pop-overlay">
          <span>{tr("app.edgePop")}</span>
        </div>
      )}

      <HostKeyDialog />
      <KbdAuthDialog />
      <FtpsCertDialog />
      <ConflictDialog />
      <AiApprovalDialog />
      {/* 聊天缩略图点开的全屏大图层：全局单例（与 Toasts 同模式），随时只有零或一张 */}
      <ImageLightbox />
      {rootModal.kind === "settings" && (
        <SettingsCenter initial={rootModal.cat} onClose={closeRootModal} />
      )}
      <CommandPalette
        open={rootModal.kind === "palette"}
        onClose={() => {
          closeRootModal();
          setFaqFocus(null);
        }}
        ctx={commandCtx}
        focusFaqNo={faqFocus}
      />
      <ShortcutHelp open={rootModal.kind === "help"} onClose={closeRootModal} />
      {rootModal.kind === "usage" && <UsageStatsDialog onClose={closeRootModal} />}
    </div>
  );
}
