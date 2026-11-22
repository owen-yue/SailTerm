// 弹出会话的独立窗口工作区（多窗口 EXT）：薄顶栏（拖拽区 + WindowControls + 会话名）
// + term-stack（本会话主区：终端，FTP 系则为文件浏览器 + AI 侧栏）+ 本窗口独立的 BottomPanel（传输/监控/审计）。
//
// 每个 WebView 有独立 JS 上下文，panelStore/fileStore 等天然一份 → 下栏跟连接走且互不串台。
// 主区归属与主窗口 Pane 同一套能力判断（有 PTY 就显终端；无 PTY 但有文件后端则文件即主区），
// 只是这里恒只有一个会话，不必再按会话分片调度。
// 这里只挂载**本会话**需要的后端事件监听（ownsSession 过滤），会话关闭/归还由主窗口负责。
import { useEffect, useState } from "react";
import BottomPanel from "./BottomPanel";
import FileBrowser from "../file/FileBrowser";
import SessionRail from "./SessionRail";
import SideRail from "./SideRail";
import { DOCK_ICONS, DOCK_META } from "./dockRegistry";
import Splitter from "../common/Splitter";
import StatusDot from "../common/StatusDot";
import Terminal from "../terminal/Terminal";
import Toasts from "../common/Toasts";
import WindowControls from "../common/WindowControls";
import { ownsSession } from "../../lib/windows";
import { allowNativeContextMenu, useBrowserGuards } from "../../lib/shellGuards";
import { useI18n } from "../../lib/i18n";
import { useSessionName } from "../../lib/sessionName";
import {
  ai as aiApi,
  revealWindow,
  sessions as sshApi,
  transfers as transferApi,
  zmodem as zmodemApi,
} from "../../lib/tauri";
import {
  aiChat,
  aiStore,
  AI_QUEUE_KEYS,
  appearanceActions,
  awaitingTracks,
  collapseActions,
  collapseStore,
  fileStore,
  files,
  sessionActions,
  sessionStore,
  toast,
  transferActions,
  transferStore,
  useStore,
  useStorePick,
  type Side,
} from "../../stores";
import type { TransferStateEvent } from "../../types";
import { kindHasFiles, kindHasTerminal } from "../../lib/capabilities";
import { kindIsFtp, sessionLabel } from "../../types";

/** 传输终态：改任务行 + 成功落盘时防抖刷目标栏（与主窗口同逻辑，作用域为本窗口 store）。 */
const fileReload: Record<string, ReturnType<typeof setTimeout> | undefined> = {};
function reloadForTask(task: { direction: "upload" | "download"; sessionId: string }): void {
  if (!ownsSession(task.sessionId)) return;
  const side: Side = task.direction === "upload" ? "remote" : "local";
  // 上传完时连接可能已经断了，此时该会话的远端分片已丢弃，别去白跑一趟
  if (side === "remote" && !(task.sessionId in fileStore.get().remotes)) return;
  clearTimeout(fileReload[side]);
  fileReload[side] = setTimeout(
    () => void files.load({ side, session: task.sessionId }, null, side === "remote"),
    300,
  );
}
/** 按 taskId 轮询复查传输行（与主窗口同款：done 早于建行时补刷，至多 ~5s）。 */
function waitRowThenReload(taskId: string, tries: number): void {
  if (tries <= 0) return;
  window.setTimeout(() => {
    const t = transferStore.get().tasks.find((x) => x.id === taskId);
    if (t) reloadForTask(t);
    else waitRowThenReload(taskId, tries - 1);
  }, 500);
}
function onTransferState(evt: TransferStateEvent): void {
  transferActions.applyState(evt);
  if (evt.status !== "done") return;
  const task = transferStore.get().tasks.find((t) => t.id === evt.taskId);
  if (task) reloadForTask(task);
  // done 早于建行（小文件竞态）：applyState 已暂存，行落地回放后再复查补刷
  else waitRowThenReload(evt.taskId, 10);
}

export default function PopWorkspace({ sessionId }: { sessionId: string }) {
  const tr = useI18n();
  const { tabs } = useStore(sessionStore);
  // 本会话待你回执的 AI 卡（反问 / 审批）：右栏 AI 页签角标据此点亮（与主窗口同一口径）。
  // 只选两字段：流式输出逐 token 写 turns，不该带着整个弹出窗重渲染。
  const { approvals, questions } = useStorePick(aiStore, AI_QUEUE_KEYS);
  // 顶栏那个名字要跟主窗口页签对上：弹出窗口基数同样是全量真实会话（本窗口 sync 过会话表）。
  const name = useSessionName();
  const tab = tabs.find((t) => t.id === sessionId) ?? null;
  const hasTerm = !!tab && kindHasTerminal(tab.kind);
  /** 无终端却有文件后端 = FTP 系：它没地方放终端，文件浏览器就是主区（与主窗口 Pane 同规则）。 */
  const filesInMain = !!tab && !hasTerm && kindHasFiles(tab.kind);
  const [railW, setRailW] = useState(() => Number(localStorage.getItem("sailterm.railW") || 360));
  // AI 右栏收起：与主窗口同一套 collapseStore（每窗口独立 JS 上下文，状态初始值同源 localStorage）
  const { rail: railCollapsed } = useStore(collapseStore);

  // 桌面壳防浏览器化：拦截 F5/F12/Ctrl+R 等，并依开发者模式同步本窗口 devtools
  useBrowserGuards();

  // 屏蔽浏览器默认右键（终端自行处理）；非开发者模式下连输入框也屏蔽以杜绝原生「检查」
  useEffect(() => {
    const onCtx = (e: MouseEvent) => {
      if (allowNativeContextMenu(e.target as HTMLElement | null)) return;
      e.preventDefault();
    };
    document.addEventListener("contextmenu", onCtx);
    return () => document.removeEventListener("contextmenu", onCtx);
  }, []);

  // 启动：同步后端存活会话（本窗口的 store 是空的）、恢复外观偏好、认领本会话
  // 主题已由 main.tsx 同步预着色，直接亮窗不等 refresh（mac 建窗隐藏，避免 WKWebView 白/错色闪）。
  useEffect(() => {
    void sessionActions.sync();
    void appearanceActions.refresh();
    void transferActions.refresh();
    sessionActions.select(sessionId);
    revealWindow();
  }, [sessionId]);

  // 文件分片生命周期：弹窗恒只有一个会话，连上即拉（FTP 系文件即主区，SSH 的远端栏在
  // 下栏文件面板里），不必像主窗口那样区分「聚焦才 open」。断线即丢弃分片（控制连接已失效）。
  useEffect(() => {
    if (!tab) return;
    if (tab.state === "connected" && kindHasFiles(tab.kind)) void files.open(tab.id);
    else if (tab.state !== "connected") files.drop(tab.id);
  }, [tab]);

  // 本窗口的后端事件监听：会话状态 / AI 步骤·审批·流 / 传输进度·终态 / zmodem，
  // 全部按 ownsSession 过滤，避免与主窗口重复弹审批、文件框
  useEffect(() => {
    let cancelled = false;
    const offs: Array<() => void> = [];
    const keep = (u: () => void) => (cancelled ? u() : offs.push(u));
    sshApi.onState(sessionActions.applyState).then(keep);
    aiApi.onStep((evt) => {
      if (ownsSession(evt.sessionId)) aiChat.applyStep(evt);
    }).then(keep);
    aiApi.onApproval((p) => {
      if (ownsSession(p.sessionId)) aiChat.pushApproval(p);
    }).then(keep);
    // 澄清反问与审批同族：后端把 `run_turn` 挂在回执上等（反问不设超时）。本窗口必须接
    // 与主窗口同一个监听，否则在独立窗口里发起的轮次遇到 ask_user 就无卡可点、整轮永久挂住。
    aiApi.onQuestion((p) => {
      if (ownsSession(p.sessionId)) aiChat.pushQuestion(p);
    }).then(keep);
    aiApi.onStream((p) => {
      if (ownsSession(p.sessionId)) aiChat.appendStream(p.sessionId, p.delta, p.turn);
    }).then(keep);
    // 审批重同步：本窗 store 是空的，建窗**之前**已挂起的审批只能从后端快照拉起
    //（事件是瞬时的，错过即无卡可点，而后端 run_turn 正挂在回执上等裁决）。
    aiApi
      .listPendingApprovals()
      .then((ps) => {
        if (cancelled) return;
        for (const p of ps) if (ownsSession(p.sessionId)) aiChat.pushApproval(p);
      })
      .catch(() => {});
    // 反问同口径重同步（与审批双保险）：会话在反问挂起期间被弹到新窗，实时事件已错过、
    // 主窗又按 ownsSession 滤掉该会话，不拉快照就两窗都无卡可答、整轮挂死。
    aiApi
      .listPendingQuestions()
      .then((ps) => {
        if (cancelled) return;
        for (const p of ps) if (ownsSession(p.sessionId)) aiChat.pushQuestion(p);
      })
      .catch(() => {});
    transferApi.onProgress((p) => {
      const task = transferStore.get().tasks.find((t) => t.id === p.taskId);
      if (!task || ownsSession(task.sessionId)) transferActions.applyProgress(p);
    }).then(keep);
    transferApi.onState(onTransferState).then(keep);
    transferApi.onPhase((p) => {
      const task = transferStore.get().tasks.find((t) => t.id === p.taskId);
      if (!task || ownsSession(task.sessionId)) transferActions.applyPhase(p);
    }).then(keep);
    // 打包失败回退：只认本会话——摘行 + 刷新 + 提示。
    transferApi
      .onFallback((p) => {
        const task = transferStore.get().tasks.find((t) => t.id === p.taskId);
        if (task && !ownsSession(task.sessionId)) return;
        transferActions.drop(p.taskId);
        void transferActions.refresh(sessionId);
        toast.show(tr("bundle.fallback").replace("{reason}", p.reason));
      })
      .then(keep);

    // 终端内 ZMODEM：与主窗口同款处理（非模态系统框 + 本地传输行），只认本会话
    import("../../lib/tauri").then(async ({ on }) => {
      keep(
        await on<{ sessionId: string; direction: "upload" | "download" }>(
          "zmodem-request",
          async ({ sessionId: sid, direction }) => {
            if (!ownsSession(sid)) return;
            document.body.classList.add("zm-dialog-open");
            try {
              const sel = await zmodemApi.pick(
                direction,
                tr(direction === "upload" ? "dlg.zmodemUpload" : "dlg.zmodemDownload"),
              );
              if (sel) {
                toast.show(direction === "upload" ? tr("pw.uploading") : tr("pw.downloading"));
                const now = Date.now();
                transferActions.addLocal({
                  id: `zm-${sid}-${now}`,
                  sessionId: sid,
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
                await zmodemApi.start(sid, sel);
              } else {
                await zmodemApi.cancel(sid);
              }
            } catch {
              zmodemApi.cancel(sid).catch(() => {});
            } finally {
              document.body.classList.remove("zm-dialog-open");
            }
          },
        ),
      );
      keep(
        await on<{ sessionId: string; name: string; size: number }>(
          "zmodem-file",
          ({ sessionId: sid, name, size }) => {
            if (!ownsSession(sid)) return;
            transferActions.updateLocal(sid, (t) => ({ ...t, remotePath: name, totalBytes: size }));
          },
        ),
      );
      keep(
        await on<{ sessionId: string; ok: boolean; message: string }>(
          "zmodem-done",
          ({ sessionId: sid, ok, message }) => {
            if (!ownsSession(sid)) return;
            transferActions.settleLocal(sid, ok ? "done" : "failed", message || null);
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
      offs.forEach((u) => u());
    };
  }, []);

  return (
    <div className="app">
      <header
        className="topbar"
        data-tauri-drag-region="deep"
        // 双击放大/还原由 Tauri 核心 drag.js 统一处理；再挂 onDoubleClick→toggleMaximize 会二次触发
      >
        <WindowControls />
        <div className="topbar-main">
          {tab && (
            <span className="tag" data-tauri-drag-region="false">
              <StatusDot state={tab.state} /> {name(tab)}
            </span>
          )}
          <span className="tag">{tr("pw.subtitle")}</span>
        </div>
      </header>

      <div className="body">
        <main className="main">
          <div className="term-host">
            <div className="term-pane" data-active data-focus>
              <div className="term-stack">
                {hasTerm && (
                  /* term-slot：本窗口的 .term-stack 是 row（终端与 AI 栏同排），
                     这一层壳提供 flex:1/min-width:0，让终端与文件主区各自占一份弹性宽。 */
                  <div className="term-slot">
                    <Terminal sessionId={sessionId} active />
                  </div>
                )}
                {tab && filesInMain && (
                  <div className="term-slot">
                    <FileBrowser
                      session={sessionId}
                      connected={tab.state === "connected"}
                      title={sessionLabel(tab, tr)}
                      ftp={kindIsFtp(tab.kind)}
                      zone="main"
                    />
                  </div>
                )}
                {/* 右侧常驻页签轨（单「AI」页签，面板内横向切 Agent/聊天）始终显示；
                    展开/收起只切换左邻内容区。弹出窗口不参与拖拽停靠：栏固定右侧 */}
                {!railCollapsed && (
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
                      style={{ width: railW, flex: "none", minWidth: 0, display: "flex", overflow: "hidden" }}
                    >
                      <SessionRail sessionId={sessionId} />
                    </div>
                  </>
                )}
                <SideRail
                  side="right"
                  tabs={[
                    {
                      id: "ai",
                      label: DOCK_META.ai.label,
                      icon: DOCK_ICONS.ai,
                      hint: DOCK_META.ai.hint,
                      awaiting: awaitingTracks(approvals, questions, sessionId).size > 0,
                    },
                  ]}
                  activeId="ai"
                  open={!railCollapsed}
                  onSelect={() => collapseActions.toggle("rail")}
                  onToggle={() => collapseActions.toggle("rail")}
                />
              </div>
            </div>
          </div>
          {/* 本窗固定列表（dockStore 仅主窗口消费，弹窗不参与拖拽停靠）：AI 栏固定右侧不进下栏，
              其余面板走下栏。files 对 SSH 有效（文件在侧栏）；FTP 系已在主区，
              由 DOCK_META.files.visible 自动滤掉，不会显两份。 */}
          <BottomPanel pinned={["files", "transfers", "monitor", "audit"]} />
        </main>
      </div>

      <Toasts />
    </div>
  );
}
