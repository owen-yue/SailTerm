// 会话（标签）store：只存后端返回的会话元数据与状态，**不含任何凭证**。
import { sessions as api } from "../lib/tauri";
import { errCode, errMsg } from "../lib/tauri/client";
import type {
  DirectConnect,
  SessionDto,
  SessionStageEvent,
  SessionStateEvent,
} from "../types";
import { aiChat } from "./aiStore";
import { forgetRail, forgetRailMode, rememberRail, rememberRailMode, restoreRail, restoreRailMode } from "./collapseStore";
import { connectionStore } from "./connectionStore";
import { forgetPanel, rememberPanel, restorePanel } from "./panelStore";
import { terminalAiActions } from "./terminalAiStore";
import { createStore } from "./store";

/** 建连进度日志的一行（后端 `session-stage` 事件累积，展示在预加载/失败卡的可折叠日志区）。 */
export interface StageLine {
  host: string;
  stage: string;
  status: "start" | "ok" | "fail";
  detail: string;
}

/** 占位标签记住的连接意图，供失败后「重试」原样重跑（凭证仍在后端，这里只有 id / 非敏感参数）。 */
export type ConnectIntent =
  | { kind: "profile"; profileId: string }
  | { kind: "direct"; params: DirectConnect };

/** 标签视图：在后端 DTO 上附断线原因（展示用）与预加载占位态。 */
export interface Tab extends SessionDto {
  reason?: string;
  /** 前端占位标签阶段：pending=预加载中，failed=失败卡；成功替换为真实会话后为 undefined。 */
  phase?: "pending" | "failed";
  /** 占位标签的关联 id（后端 session-stage 按它归集；重试复用同一条）。 */
  connectId?: string;
  /** 预加载/失败日志（session-stage 累积）。 */
  stages?: StageLine[];
  /** 失败时的最终错误文案。 */
  failMessage?: string;
  /** 占位标签的连接意图（供重试）。 */
  intent?: ConnectIntent;
}

interface SessionStoreState {
  tabs: Tab[];
  activeId: string | null;
  /** 正在建连/重连的会话 id（或 `connecting`），用于按钮 loading。 */
  busy: string;
  error: string;
  /** 已弹出到独立窗口的会话（主窗口不渲染其终端，标签点击改为聚焦那个窗口）。 */
  popped: Record<string, true>;
}

export const sessionStore = createStore<SessionStoreState>({
  tabs: [],
  activeId: null,
  busy: "",
  error: "",
  popped: {},
});

/** 首次建连用的默认终端尺寸；随后由 xterm fit 校正。 */
export const DEFAULT_SIZE = { cols: 80, rows: 24 };

/** 各会话最近的终端尺寸（重连时沿用，避免重连后画面错位）。非响应式。 */
const sizes = new Map<string, { cols: number; rows: number }>();

export function rememberSize(id: string, cols: number, rows: number): void {
  sizes.set(id, { cols, rows });
}

function sizeOf(id: string): { cols: number; rows: number } {
  return sizes.get(id) ?? DEFAULT_SIZE;
}

export const sessionActions = {
  /** 刷新各会话 TCP RTT（侧栏/标签角标）。 */
  async refreshRtt(): Promise<void> {
    const { tabs } = sessionStore.get();
    await Promise.all(
      tabs.map(async (t) => {
        if (t.kind === "local" || t.kind === "serial") return;
        try {
          const ms = await api.rtt(t.id);
          sessionStore.set((prev) => ({
            tabs: prev.tabs.map((x) =>
              x.id === t.id ? { ...x, latencyMs: ms } : x,
            ),
          }));
        } catch {
          /* 探测失败保持原值 */
        }
      }),
    );
  },
  /** 点侧栏档案 / 保存并连接：已有标签则切过去；命中的是失败占位标签时按最新档案重跑。 */
  async openProfile(profileId: string): Promise<string | null> {
    const live = sessionStore.get().tabs.find((t) => t.profileId === profileId);
    if (live) {
      sessionStore.set({ activeId: live.id });
      // 失败卡上「编辑主机 → 保存并连接」：同 profileId 的失败占位标签仍在，
      // 光切过去不会重连。retry 会按 profileId 重新拉取已落库的最新档案（含新凭证）重跑。
      if (live.phase === "failed") void sessionActions.retry(live.id);
      return live.id;
    }
    return startPending({ kind: "profile", profileId });
  },

  /** 双击侧栏档案 / 右键菜单「连接」：无视已有标签，强制新开一条连接（同一服务器可多窗口）。 */
  async openProfileNew(profileId: string): Promise<string | null> {
    return startPending({ kind: "profile", profileId });
  },

  /** 快速连接（不保存档案）。 */
  async openDirect(params: DirectConnect): Promise<string | null> {
    return startPending({ kind: "direct", params });
  },

  /** 打开本地终端（本机 shell，无凭证）；本机秒连，无需预加载页。*/
  async openLocal(shell?: string | null): Promise<string | null> {
    return open(() => api.connectLocal(DEFAULT_SIZE.cols, DEFAULT_SIZE.rows, shell ?? null));
  },

  /** 处理后端 `session-stage` 事件：把一行阶段日志追加到对应的预加载占位标签。 */
  applyStage(evt: SessionStageEvent): void {
    const { tabs } = sessionStore.get();
    if (!tabs.some((t) => t.connectId === evt.connectId && t.phase)) return;
    sessionStore.set({
      tabs: tabs.map((t) =>
        t.connectId === evt.connectId && t.phase
          ? {
              ...t,
              stages: [
                ...(t.stages ?? []),
                { host: evt.host, stage: evt.stage, status: evt.status, detail: evt.detail },
              ],
            }
          : t,
      ),
    });
  },

  /** 失败卡「重试」：同一占位标签、同一 connectId 原样重跑，清空旧日志回到预加载态。 */
  async retry(pendingId: string): Promise<void> {
    const t = sessionStore.get().tabs.find((x) => x.id === pendingId);
    if (!t || !t.intent || !t.connectId) return;
    sessionStore.set({
      tabs: sessionStore.get().tabs.map((x) =>
        x.id === pendingId
          ? { ...x, phase: "pending", stages: [], failMessage: undefined }
          : x,
      ),
      busy: pendingId,
    });
    await runConnect(pendingId, t.connectId, t.intent);
  },

  /** 失败卡/预加载页「关闭标签」：占位标签后端根本没有会话，只摘本地视图。 */
  closePending(pendingId: string): void {
    dropTab(pendingId);
  },

  /** 断线重连：沿用同一 session_id，终端内容保留。 */
  async reconnect(id: string): Promise<boolean> {
    const { cols, rows } = sizeOf(id);
    sessionStore.set({ busy: id, error: "" });
    try {
      apply(await api.reconnect(id, cols, rows));
      return true;
    } catch (e) {
      sessionStore.set({ error: errMsg(e) });
      return false;
    } finally {
      sessionStore.set({ busy: "" });
    }
  },

  /** 关闭标签：先摘本地视图，再通知后端释放会话。占位标签（预加载/失败）后端无会话，只摘视图。 */
  async close(id: string): Promise<void> {
    const t = sessionStore.get().tabs.find((x) => x.id === id);
    dropTab(id);
    if (t?.phase) return; // 预加载占位：没有后端会话可关
    try {
      await api.close(id);
    } catch (e) {
      // 会话已自行退出属正常情形，不打扰用户
      if (errCode(e) !== "E_NO_SESSION") sessionStore.set({ error: errMsg(e) });
    }
  },

  /** 关闭本窗口标签栏里的全部会话（不含已弹出到独立窗口的；那些由那个窗口自行管理）。 */
  async closeAll(): Promise<void> {
    const { tabs, popped } = sessionStore.get();
    const ids = tabs.filter((t) => !popped[t.id]).map((t) => t.id);
    for (const id of ids) await sessionActions.close(id);
  },

  /** 关闭除指定会话外的其他会话（不含已弹出到独立窗口的）。 */
  async closeOthers(keepId: string): Promise<void> {
    const { tabs, popped } = sessionStore.get();
    const ids = tabs.filter((t) => t.id !== keepId && !popped[t.id]).map((t) => t.id);
    for (const id of ids) await sessionActions.close(id);
  },

  select(id: string): void {
    sessionStore.set({ activeId: id });
  },

  /** 弹出到独立窗口：主窗口摘掉该 pane，焦点交给其余未弹出的标签。 */
  pop(id: string): void {
    sessionStore.set((prev) => {
      const home = prev.tabs.filter((t) => !prev.popped[t.id] && t.id !== id);
      return {
        popped: { ...prev.popped, [id]: true },
        activeId: prev.activeId === id ? (home[home.length - 1]?.id ?? null) : prev.activeId,
      };
    });
  },

  /** 独立窗口关闭：会话归还主窗口（终端重挂，靠 ring 缓冲恢复近期屏幕）。 */
  unpop(id: string): void {
    sessionStore.set((prev) => {
      if (!prev.popped[id]) return prev;
      const popped = { ...prev.popped };
      delete popped[id];
      return { popped, activeId: prev.activeId ?? id };
    });
  },

  /** 处理后端 `session-state` 事件。 */
  applyState(evt: SessionStateEvent): void {
    if (evt.state === "closed") {
      dropTab(evt.sessionId);
      return;
    }
    const { tabs } = sessionStore.get();
    sessionStore.set({
      tabs: tabs.map((t) => (t.id === evt.sessionId ? { ...t, state: evt.state, reason: evt.reason } : t)),
    });
  },

  /** 以后端注册表为准同步标签（启动/整页刷新后用）。 */
  async sync(): Promise<void> {
    try {
      const tabs = await api.list();
      // 刷新后 per-session 档快照已清空：为每条现存标签补快照当前默认档（幂等，已快照的不覆盖）
      tabs.forEach((t) => terminalAiActions.ensure(t.id, t.kind));
      sessionStore.set((prev) => ({
        tabs,
        // 刷新后 activeId 丢了：默认落在最后一个标签，否则一个终端都不会显示
        activeId:
          prev.activeId && tabs.some((t) => t.id === prev.activeId)
            ? prev.activeId
            : (tabs[tabs.length - 1]?.id ?? null),
      }));
    } catch (e) {
      sessionStore.set({ error: errMsg(e) });
    }
  },

  clearError(): void {
    sessionStore.set({ error: "" });
  },
};

async function open(fn: () => Promise<SessionDto>): Promise<string | null> {
  sessionStore.set({ busy: "connecting", error: "" });
  try {
    const dto = await fn();
    apply(dto);
    return dto.id;
  } catch (e) {
    sessionStore.set({ error: errMsg(e) });
    return null;
  } finally {
    sessionStore.set({ busy: "" });
  }
}

// —— 预加载占位标签：点击即建标签、后端逐阶段回推日志、成功原位换成真实会话、失败原地停失败卡 ——

/** 失败卡的最短加载展示时长（毫秒）：瞬间失败也让进度条先转满这么久再落终态，避免生硬一闪。 */
const MIN_LOADING_MS = 1400;
/** 成功进入终端前的最短展示时长（毫秒）：握手本身已让进度条转了一段，这里只留极短缓冲即切终端，
 * 不再像失败那样补足到 1400ms——否则「连上后还要干等」体验很差。 */
const MIN_SUCCESS_MS = 350;

/** 生成一个前端侧唯一 connectId（仅用于归集 session-stage，与后端 sessionId 无关）。 */
function genConnectId(): string {
  return `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
}

/** 由连接意图拼一个占位标签：显示名/主机/协议尽量从档案或直连参数即时填，缺待首个 stage 事件补。 */
function placeholderFrom(intent: ConnectIntent, connectId: string): Tab {
  const id = `pending:${connectId}`;
  const base = {
    id,
    state: "disconnected" as const,
    phase: "pending" as const,
    connectId,
    stages: [] as StageLine[],
    intent,
  };
  if (intent.kind === "profile") {
    const p = connectionStore.get().profiles.find((x) => x.id === intent.profileId);
    return { ...base, label: p?.name ?? "", host: p?.host ?? "", profileId: intent.profileId, kind: p?.kind ?? "ssh" };
  }
  const { params } = intent;
  return { ...base, label: params.host, host: params.host, profileId: null, kind: params.kind ?? "ssh" };
}

/** 立即上屏一个预加载占位标签，后台发起建连（不阻塞调用方），返回占位标签 id。 */
function startPending(intent: ConnectIntent): string {
  const connectId = genConnectId();
  const tab = placeholderFrom(intent, connectId);
  sessionStore.set({ tabs: [...sessionStore.get().tabs, tab], activeId: tab.id, error: "" });
  void runConnect(tab.id, connectId, intent);
  return tab.id;
}

/** 发起真正的建连：成功把占位标签原位换成真实会话，失败原地转失败卡（错误不再走右上角 toast）。 */
async function runConnect(pendingId: string, connectId: string, intent: ConnectIntent): Promise<void> {
  sessionStore.set({ busy: pendingId });
  const startedAt = Date.now();
  // 终态收场缓冲：失败补足到 MIN_LOADING_MS（转满动画再报错），成功只补足到 MIN_SUCCESS_MS（尽快进终端）。
  // 均从发起建连起算，不额外拖慢真实建连。
  const settle = (minMs: number, fn: () => void): void => {
    const wait = Math.max(0, minMs - (Date.now() - startedAt));
    if (wait > 0) window.setTimeout(fn, wait);
    else fn();
  };
  try {
    const dto =
      intent.kind === "profile"
        ? await api.connect(intent.profileId, DEFAULT_SIZE.cols, DEFAULT_SIZE.rows, connectId)
        : await api.connectDirect(
            { ...intent.params, cols: DEFAULT_SIZE.cols, rows: DEFAULT_SIZE.rows },
            connectId,
          );
    settle(MIN_SUCCESS_MS, () => resolvePending(pendingId, dto));
  } catch (e) {
    const msg = errMsg(e);
    settle(MIN_LOADING_MS, () => markFailed(pendingId, msg));
  } finally {
    if (sessionStore.get().busy === pendingId) sessionStore.set({ busy: "" });
  }
}

/** 建连成功：把占位标签在原位置换成后端真实会话（保留标签栏顺序），并激活真实 id。 */
function resolvePending(pendingId: string, dto: SessionDto): void {
  const { tabs, activeId } = sessionStore.get();
  const idx = tabs.findIndex((t) => t.id === pendingId);
  if (idx < 0) {
    // 占位标签已被用户关掉，但后台建连仍成功：不复活标签，直接释放这条孤儿会话。
    terminalAiActions.ensure(dto.id, dto.kind);
    void api.close(dto.id).catch(() => {});
    return;
  }
  terminalAiActions.ensure(dto.id, dto.kind);
  const next = [...tabs];
  next[idx] = { ...dto };
  sessionStore.set({ tabs: next, activeId: activeId === pendingId ? dto.id : activeId });
  // 占位 id 可能在 activeId 切换时被写进下栏/右栏/AI页签记忆表，换真实 id 后清掉残留
  forgetPanel(pendingId);
  forgetRail(pendingId);
  forgetRailMode(pendingId);
}

/** 建连失败：占位标签停在失败态，携最终错误文案（阶段日志已在 applyStage 里累积）。 */
function markFailed(pendingId: string, msg: string): void {
  sessionStore.set({
    tabs: sessionStore.get().tabs.map((t) =>
      t.id === pendingId ? { ...t, phase: "failed", failMessage: msg } : t,
    ),
  });
}

/** 新增或就地更新一个标签，并激活它。 */
function apply(dto: SessionDto): void {
  const { tabs } = sessionStore.get();
  const isNew = !tabs.some((t) => t.id === dto.id);
  const next = isNew
    ? [...tabs, { ...dto }]
    : tabs.map((t) => (t.id === dto.id ? { ...t, ...dto, reason: undefined } : t));
  // 新终端建立时快照当前全局默认档，使其此后独立于其他终端的切换（幂等，重连不覆盖已有选择）
  terminalAiActions.ensure(dto.id, dto.kind);
  sessionStore.set({ tabs: next, activeId: dto.id });
  // 下栏的「新连接默认收起 / 切标签恢复各自记忆」不在此处显式操作：activeId 变化
  // 由文件末尾的统一订阅驱动——新标签无记忆即回落收起默认，老标签保留各自的下栏态。
}

function dropTab(id: string): void {
  const { tabs, activeId, popped, busy } = sessionStore.get();
  // 先收属于本会话的 AI 在途态与按终端记的对话/时间线（都幂等，故放在“标签已不在”的
  // 早退之前）：否则一轮挂在已死连接上的编排轮次会把全局发送态撑住，让之后所有会话的
  // AI 永远“思考中”；重复 closed 事件下晚到的清理也会漏掉 timelines/convIds 残留。
  aiChat.abortSession(id);
  aiChat.clearSessionTimeline(id);
  const next = tabs.filter((t) => t.id !== id);
  if (next.length === tabs.length) return;
  sizes.delete(id);
  // 会话真正结束：清掉该终端的 shell/agent 档快照（弹出/归还窗口不走这里，记录保留）
  terminalAiActions.drop(id);
  const nextPopped = popped[id] ? Object.fromEntries(Object.entries(popped).filter(([k]) => k !== id)) : popped;
  sessionStore.set({
    tabs: next,
    popped: nextPopped,
    activeId: activeId === id ? (next[next.length - 1]?.id ?? null) : activeId,
    // 关掉的正是那条在建连的会话时，同步把 busy 交还：后台握手若永不返回（对端不发 SSH
    // banner），runConnect 的 finally 就永不执行，关标签是用户唯一的脱身口 —— busy 留着
    // 会让之后每一个连接表单都永久停在「处理中」，且无法取消。
    ...(busy === id ? { busy: "" } : {}),
  });
  // 订阅可能刚把「被关闭的旧活动标签」快照进了记忆表；此处显式清掉，保证关闭的会话不留残档
  // （非活动标签被关则直接清它的旧记忆）。放在 set 之后，才能覆盖订阅内 remember 的回写。
  forgetPanel(id);
  forgetRail(id);
  forgetRailMode(id);
}

// 下栏/右栏/AI页签按会话标签独立记忆的驱动：任何改 activeId 的路径（select/apply/pop/unpop/dropTab/sync）
// 都经由这一个订阅收敛——切走时快照旧标签的态，切入时恢复该标签记忆的（新标签各回落默认：下栏/右栏收起、AI页签 Agent）。
// 用 lastActiveId 记录上一个活动标签，因为 store.set 会先落地 activeId 再通知，回调里已取不到旧值。
let lastActiveId: string | null = null;
sessionStore.subscribe(() => {
  const cur = sessionStore.get().activeId;
  if (cur === lastActiveId) return;
  if (lastActiveId) {
    rememberPanel(lastActiveId);
    rememberRail(lastActiveId);
    rememberRailMode(lastActiveId);
  }
  restorePanel(cur);
  restoreRail(cur);
  restoreRailMode(cur, sessionStore.get().tabs.find((t) => t.id === cur)?.kind);
  lastActiveId = cur;
});
