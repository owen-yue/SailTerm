// AI 对话 store：对话列表 + 当前对话时间线 + 编排步骤流 + 命令审批队列。
//
// 一轮对话的时间线由三部分拼成：① `send` 时乐观压入的用户气泡；② 编排期间 `ai-step` 事件压入的
// 工具步骤（调用/结果/失败）；③ `send` resolve 后压入的 assistant 气泡。审批走独立队列，由
// 模态卡片逐个呈现（复用主机密钥 TOFU 的 push/respond 模式）。
//
// 安全红线：时间线里的文本都是后端**脱敏后**产物；store 不含任何凭证/API Key。
import { ai as api } from "../lib/tauri";
import { errMsg } from "../lib/tauri/client";
import type {
  AiApprovalPrompt,
  AiAttachKind,
  AiAttachment,
  AiConversation,
  AiQuestionPrompt,
  AiStep,
  AiStepEvent,
  AiTurnFile,
  AiTurnImage,
  TurnUsage,
} from "../types";
import { REPLY_TERM_LINES, APP_CHAT_SESSION } from "../types";
import { createStore } from "./store";
import { defaultProviderId, providerStore } from "./providerStore";
import { settingsStore } from "./settingsStore";
import { sessionStore } from "./sessionStore";
import { canAgentRail } from "../lib/capabilities";
import { effectiveTerminalAi } from "./terminalAiStore";
import { toast, notifyOsReply, notifyOsAwait } from "./toastStore";
import * as termAgent from "../lib/termAgent";
import { t, getUiLangName } from "../lib/i18n";

/**
 * 用户气泡上的一张图。两种来源，取其一：
 * - 本轮刚贴/选的：`dataUrl` 已在本地（同时作为发送内容，不再另存一份 base64）；
 * - 从归档恢复的：只有落盘后的 `id`，缩略图按需经 `ai_get_attachment` 懒取（见 AiTimeline 的 ChatThumb）。
 */
export interface ChatImage {
  name: string;
  /** 已归一的 mime（只 image/png 与 image/jpeg）。 */
  mime?: string;
  /** 本地已有的 `data:...;base64,...`；历史恢复为空，靠 `id` 拉。 */
  dataUrl?: string;
  /** 归档附件 id；本轮新图为空（落库时由后端生成）。 */
  id?: string;
}

/** 本轮随文的一份本地文档（待发区 chip 与历史气泡共用一个形状）。
 *
 * 与 [`ChatImage`] 分道的理由：文档本就在磁盘上，只递路径、字节不进前端（读盘与抽取都在
 * 后端本轮开始时做），因此这里**没有 dataUrl**，也不参与「重试重发 base64」那套回收。 */
export interface ChatFile {
  /** 本机绝对路径（系统文件对话框给出，本就是用户显式选择）。 */
  path: string;
  name: string;
  bytes?: number;
  mime?: string;
  kind?: AiAttachKind;
  /** 就地预探失败的原因：有值即代这份发不出去，chip 直接标错（失败内联，不静默丢）。 */
  error?: string;
  /** 以下三项只从历史归档恢复（本轮待发时还没抽取，后端跑完才有）。 */
  chars?: number | null;
  degraded?: string | null;
  /** 已落盘的抽取文本 id（`<id>.md`）；本轮新件为空。 */
  id?: string;
}

/** 时间线里的一格：用户气泡 / 助手气泡 / 一个编排步骤。
 * （助手气泡不带 token 脚注字段：逐条报数已去掉，用量看 lastUsage/归档。） */
export type ChatItem =
  | { kind: "user"; id: string; text: string; images?: ChatImage[]; files?: ChatFile[] }
  | { kind: "assistant"; id: string; text: string }
  | { kind: "step"; id: string; step: AiStep };

/** 本轮结束后留给「继续 / 重试」的挂起态（达上限或真错误才非空）。track=发起轨；retry 存原句文本。 */
type ResumeState = {
  track: "agent" | "chat";
  kind: "continue" | "retry";
  text?: string;
  images?: ChatImage[];
  files?: ChatFile[];
};

/**
 * 本轮模型侧「正在产出什么」的相位（由 `ai-stream` 的 kind 驱动）：
 * 把静态的「思考中」细分成推理 / 生成命令 / 起草答复，并决定推理链是否单独走字。
 */
export type TurnPhase = "idle" | "reasoning" | "text" | "tool";

/**
 * 每个会话一轮的在途态（取代原全局 turnTrack/sending/sendingSession/streamingText/resume）。
 * `turnId` 每会话独立自增：cancel/abort 靠删除条目或改序号来作废在途回调（陈旧结果静默丢弃）。
 */
interface TurnState {
  turnId: number;
  track: "agent" | "chat";
  sending: boolean;
  /** 本轮发起时刻（ms）：「思考中」指示据此每秒走字展示已耗时，区分「在思考」与「真卡住」。 */
  startedAt: number;
  streamingText: string;
  /** 推理/思考链增量（仅推理型模型有）：与正文分开累加，前端单独折叠呈现。 */
  reasoningText: string;
  /** 当前相位：驱动「思考中」提示细分（见 `TurnPhase`）。 */
  phase: TurnPhase;
  resume: ResumeState | null;
  error: string;
  /**
   * 本轨末轮的上下文用量（驱动聊天/Agent 两页的 ctx 圆环）。
   * 聊天轨另有 `chatViews[sid].lastUsage`（历史回看也会回填），这里保留是为了
   * Agent 轨（timelines 不带用量）与新一轮发起/错误收尾时不丢旧值。
   */
  lastUsage: TurnUsage | null;
}

/** 聊天轨按会话隔离的当前对话视图（取代原全局 activeId/items/lastUsage）。 */
interface ChatView {
  activeId: string | null;
  items: ChatItem[];
  lastUsage: TurnUsage | null;
}

interface AiStoreState {
  conversations: AiConversation[];
  /**
   * 历史列表的作用域（跟服务器走，别把所有服务器的历史堆在一起）：
   * - `undefined` = 不过滤（当前没有会话上下文）；
   * - `string`    = 只看这个连接档案（profile_id）下的对话；
   * - `null`      = 直连/临时会话（无 profile）：只看未归属服务器的那批。
   */
  conversationScope: string | null | undefined;
  /** 聊天轨按会话隔离的当前对话视图（`sessionId -> ChatView`；取代原全局 activeId/items/lastUsage）。 */
  chatViews: Record<string, ChatView>;
  /**
   * 按连接（sessionId）隔离的完整时间线：问题/步骤/回复全部累积，
   * 直到该连接关闭（标签移除）才清空——Agent 侧边栏据此展示全程记录。
   * 与 `chatViews[sid].items` 独立：后者是 AiDrawer 的“当前对话”视图。
   */
  timelines: Record<string, ChatItem[]>;
  /**
   * Agent 轨（控制台 / Agent 页签）按连接钉死的对话 id：`sessionId -> conversationId`。
   * 与聊天页的 `chatViews[sid].activeId` 彻底分开——聊天页「新增聊天」/切历史只动它，
   * 永不碰这里，故控制台一路聊不会因聊天页操作而失忆（方案1：连接即一段连续对话）。
   * 与 `timelines[sessionId]` 展示的是同一条对话 → 看到的=记得的。
   */
  agentConvIds: Record<string, string>;
  /**
   * 聊天页（对话轨）按终端会话记住的当前对话 id：`sessionId -> conversationId`。
   * 修“新开一个终端、聊天页却还显上一个 session 的对话”的 bug：切标签时 `focusSession`
   * 据此把显示重绑到该终端自己的对话（有则恢复、无则空新），不再串台。
   */
  chatConvIds: Record<string, string>;
  /**
   * 每轨（chat / agent）最多一轮在途的编排轮次，键为 `turnKey(sessionId, track)`。
   * 同一条连接上 chat 与 Agent 各占一个独立槽，互不 busy 拦截；跨会话天然并发——各写各的 timelines/chatViews。
   */
  turns: Record<string, TurnState>;
  /** 每会话「下一次 send 使用的 provider」（`sessionId -> providerId`；null/缺省 = 后端默认，取代原全局 providerId）。 */
  providerBy: Record<string, string | null>;
  /** 待用户裁决的命令审批（队列）。 */
  approvals: AiApprovalPrompt[];
  /** 待用户作答的澄清反问（队列）：与审批同构，但编排循环串行→同会话最多一个 pending，不撞车。 */
  questions: AiQuestionPrompt[];
  /** 仅承载「非发送类」操作错误（拉历史/切会话/审批应答失败等）；发送类错误改落 `turns[sid].error`。 */
  error: string;
  /** 就地卡片点「ⓘ详情」时打开的五要素审批弹窗对应的 requestId；null = 未打开。 */
  detailApprovalId: string | null;
  // streamingText / resume 已并入 `turns[sid]`（见 TurnState）。
  /**
   * 待预填进聊天输入框的草稿（「文件 → 发送到 AI 对话」用）：`seq` 自增以让相同文本也能重复触发。
   * AiDrawer 应用后立即 `consumeDraft` 置空，避免重挂载时重复预填。
   */
  draft: { text: string; seq: number } | null;
  /**
   * 待插入聊天编辑区的文件 chip 请求（「文件 → 发送到 AI 对话」）：`seq` 自增以让同文件也能重复触发。
   * `sessionId` 是点选时文件栏所属的会话：chip 只许落在同一个会话的输入框——否则在 A 机的
   * 文件栏点「发送到 AI 对话」、右栏 AI 偏偏停在 B 机时，引用会插进 B 机的对话里。
   * AiDrawer 把 chip 插进 contenteditable 后立即 `consumeChip` 置空，避免重挂载时重复插入。
   */
  chipReq: { sessionId: string; path: string; name: string; remote: boolean; isDir: boolean; seq: number } | null;
  /**
   * 待插入聊天编辑区的终端选区 chip 请求（终端右键「问 AI」）：`seq` 自增以让同一段选区也能重复触发。
   * `sessionId` 是框选时所属的会话，口径与 `chipReq` 一致：A 终端的选区不该插进 B 会话的输入框。
   * AiDrawer 把 chip 插进 contenteditable 后立即 `consumeTermSel` 置空，避免重挂载时重复插入。
   */
  termReq: { sessionId: string; text: string; seq: number } | null;
  // lastUsage 已并入 `chatViews[sid]`（见 ChatView）。
}

export const aiStore = createStore<AiStoreState>({
  conversations: [],
  conversationScope: undefined,
  chatViews: {},
  timelines: {},
  agentConvIds: {},
  chatConvIds: {},
  turns: {},
  providerBy: {},
  approvals: [],
  questions: [],
  error: "",
  detailApprovalId: null,
  draft: null,
  chipReq: null,
  termReq: null,
});

let seq = 0;
const uid = (): string => `ai-${++seq}`;
// 轮次序号采用「模块级每会话单调水位」`turnGen`（而非从 turns[sid] 条目派生）：条目会随
// cancel/abort 被 delete，若序号基线跟着条目归零，重发的下一轮会与被取消轮重号 → 陈旧
// 回调（late resolve 或迟到事件）反而通过 turnId 比对、灌进新视图。水位只增不归零，仅在会话
// 彻底清理（clearSessionTimeline）时回收，保证同一会话内 turnId 严格递增、陈旧必失配。
const turnGen = new Map<string, number>();

/** 复合轮次键：同一条连接（sessionId）上 chat 与 Agent 两轨各自独立的在途槽位。
 *  与后端 `{track}\u{1}{session_id}` 语义对齐（前端仅内存态，用可读的 `::` 分隔即可）。 */
export const turnKey = (sessionId: string, track: "agent" | "chat"): string =>
  `${track}::${sessionId}`;

/** 在 (agent, chat) 两槽里定位「归属该 turn 且仍在途」的那一槽（turn 跨轨全局唯一 → 至多一个命中）。
 *  事件（ai-step / ai-stream）只带 (sessionId, turn)、不带轨，故据此把实时增量路由到正确的轨。 */
function locateTurn(
  sessionId: string,
  turn: number,
): { key: string; tn: TurnState } | null {
  const { turns } = aiStore.get();
  for (const track of ["agent", "chat"] as const) {
    const key = turnKey(sessionId, track);
    const tn = turns[key];
    if (tn?.sending && tn.turnId === turn) return { key, tn };
  }
  return null;
}

export const aiChat = {
  /** 拉历史对话列表（按当前作用域过滤，最近更新在前）。 */
  async refreshConversations(): Promise<void> {
    const scope = aiStore.get().conversationScope;
    try {
      // 后端只支持「按 profile 等值过滤」；直连会话（scope=null）要的是「未归属服务器」那批，
      // 传 null 会被当成「不过滤」，故取回后在前端补齐同一口径。
      const all = await api.listConversations(typeof scope === "string" ? scope : null);
      aiStore.set({ conversations: scope === null ? all.filter((c) => !c.connectionId) : all });
    } catch (e) {
      aiStore.set({ error: errMsg(e) });
    }
  },

  /** 设定历史作用域（打开抽屉 / 切换标签时调用）；没变就不重复拉。 */
  setScope(scope: string | null | undefined): void {
    if (aiStore.get().conversationScope === scope) return;
    aiStore.set({ conversationScope: scope });
    void aiChat.refreshConversations();
  },

  /** 预填一段聊天输入草稿（外部入口如「文件 → 发送到 AI 对话」）；`seq` 自增保证同文本也重触发。 */
  setDraft(text: string): void {
    aiStore.set((prev) => ({ draft: { text, seq: (prev.draft?.seq ?? 0) + 1 } }));
  },

  /** 草稿已被输入框接收，清空以免组件重挂载时重复预填。 */
  consumeDraft(): void {
    if (aiStore.get().draft) aiStore.set({ draft: null });
  },

  /** 请求把一个文件 chip 插入聊天编辑区（同路径去重在编辑区 DOM 侧做）；入口：文件栏右键「发送到 AI 对话」。 */
  addRef(r: { sessionId: string; path: string; name: string; remote: boolean; isDir: boolean }): void {
    aiStore.set((prev) => ({ chipReq: { ...r, seq: (prev.chipReq?.seq ?? 0) + 1 } }));
  },

  /** chip 请求已被编辑区接收，清空以免组件重挂载时重复插入。 */
  consumeChip(): void {
    if (aiStore.get().chipReq) aiStore.set({ chipReq: null });
  },

  /** 请求把一个终端选区 chip 插入聊天编辑区；入口：终端右键「问 AI」（选区随 chip 走，不再贴进正文）。 */
  addTermSel(r: { sessionId: string; text: string }): void {
    aiStore.set((prev) => ({ termReq: { ...r, seq: (prev.termReq?.seq ?? 0) + 1 } }));
  },

  /** 选区 chip 请求已被编辑区接收，清空以免组件重挂载时重复插入。 */
  consumeTermSel(): void {
    if (aiStore.get().termReq) aiStore.set({ termReq: null });
  },

  /**
   * 打开一段既有对话：载入归档消息重建时间线（问答 + 当时的过程轨迹）。
   * `sessionId` 用于把对话认领到当前会话，否则随后的 send 会另起新对话（串不上历史）。
   */
  async open(conversationId: string, sessionId?: string | null): Promise<void> {
    // 视图按会话键（sid）；无会话时落 `""`（首页浏览历史，不参与 send/turns）。
    const sid = sessionId ?? "";
    aiStore.set((prev) => ({
      chatViews: { ...prev.chatViews, [sid]: { activeId: conversationId, items: [], lastUsage: null } },
      error: "",
      // 记住该终端当前看的是哪条对话，切标签回来能恢复（不再串到别的 session）。
      ...(sessionId
        ? { chatConvIds: { ...prev.chatConvIds, [sessionId]: conversationId } }
        : {}),
    }));
    try {
      const msgs = await api.listMessages(conversationId);
      const items: ChatItem[] = [];
      // 末条带上下文真值的 assistant 行：据此回填工具栏 ctx 环，切历史会话不再清空（修 bug）。
      let lastCtx: TurnUsage | null = null;
      for (const m of msgs) {
        if (m.role === "user") {
          const refs = parseRefs(m.attachments);
          const images = refsToImages(refs);
          const files = refsToFiles(refs);
          items.push({
            kind: "user",
            id: m.id,
            text: m.content ?? "",
            ...(images.length ? { images } : {}),
            ...(files.length ? { files } : {}),
          });
        } else if (m.role === "assistant") {
          // 过程轨迹挂在 assistant 消息上：先重现当时的步骤，再接回复气泡（与实时顺序一致）
          parseSteps(m.steps).forEach((step, i) =>
            items.push({ kind: "step", id: `${m.id}-s${i}`, step }),
          );
          // 归档带 `tokens`（本轮合计）与 v13 起的 `context_used/context_window`（旧数据 NULL→0）。
          const contextUsed = m.contextUsed ?? 0;
          const contextWindow = m.contextWindow ?? 0;
          const usage: TurnUsage | undefined =
            m.tokens || contextUsed
              ? { inputTokens: 0, outputTokens: 0, totalTokens: m.tokens ?? 0, contextUsed, contextWindow, estimated: true }
              : undefined;
          if (contextUsed > 0 && contextWindow > 0) lastCtx = usage ?? null;
          items.push({ kind: "assistant", id: m.id, text: m.content ?? "" });
        }
      }
      // 回看历史时同样收口：归档的 steps 里 toolCall 停在发起时的 NEEDS_APPROVAL，
      // 逐条遇到 toolResult 就把前面同工具那张待批准命令卡翻为 AUTO（去黄标）。
      let settled = items;
      for (let i = 0; i < settled.length; i++) {
        const it = settled[i];
        if (it.kind === "step" && it.step.kind === "toolResult") {
          settled = settleResolvedCall(settled, it.step.tool, i);
        }
      }
      // 期间该会话可能又被切走/删对话：仅当视图仍停在该对话时才落地。
      if (aiStore.get().chatViews[sid]?.activeId === conversationId)
        aiStore.set((prev) => ({
          chatViews: {
            ...prev.chatViews,
            [sid]: { activeId: conversationId, items: settled, lastUsage: lastCtx },
          },
        }));
    } catch (e) {
      aiStore.set({ error: errMsg(e) });
    }
  },

  /** 开一段新对话（清空时间线；真正落库推迟到首次 send）。传入 `sessionId` 时同时抹掉该终端的对话记忆，
   *  并把空对话认领到该会话（避免切标签回来又把旧对话自动开回）。 */
  start(sessionId?: string | null): void {
    aiStore.set((prev) => {
      const sid = sessionId ?? "";
      const chatViews = { ...prev.chatViews, [sid]: { activeId: null, items: [], lastUsage: null } };
      // 新建聊天视图已清空：若本会话留有「重试」挂起态（上一轮失败残留的 inert 条目），点重试会以新空
      // 对话重跑旧问句、冒出无用户气泡的孤助手回复，故一并抹掉 resume（在途轮次不动，由 UI chatBusyHere 守卫）。
      const ckey = sessionId ? turnKey(sessionId, "chat") : "";
      const tn = sessionId ? prev.turns[ckey] : undefined;
      const turns =
        sessionId && tn && !tn.sending && tn.resume
          ? { ...prev.turns, [ckey]: { ...tn, resume: null } }
          : prev.turns;
      if (!sessionId || !(sessionId in prev.chatConvIds)) {
        return { chatViews, turns, error: "" };
      }
      const chatConvIds = { ...prev.chatConvIds };
      delete chatConvIds[sessionId];
      return { chatViews, turns, error: "", chatConvIds };
    });
  },

  /**
   * 确保该会话的聊天视图已物化（切标签时调用）：视图按会话常驻，已存在则不动（切回不重载）；
   * 否则有记住的对话就从库载入，没有就建空白新视图。取代原「重绑全局 items/activeId」逻辑。
   */
  focusSession(sessionId: string | null): void {
    if (!sessionId) return; // 无终端标签（首页）：不动，避免误清
    const { chatViews, chatConvIds } = aiStore.get();
    if (chatViews[sessionId]) return; // 该会话视图已在（含在途轮次的实时写入）：不重载、不打断
    const remembered = chatConvIds[sessionId];
    if (remembered) {
      void aiChat.open(remembered, sessionId); // 首次切到该会话：从库恢复它上次看的对话
    } else {
      aiStore.set((prev) => ({
        chatViews: { ...prev.chatViews, [sessionId]: { activeId: null, items: [], lastUsage: null } },
      }));
    }
  },

  /** 选择该会话下一次对话使用的 provider（模型切换，按会话独立）；null = 后端默认。
   *  键与读侧（AiDrawer `providerBy[csid]`，csid=会话 ?? ""）对齐：首页也写 `""`，不再静默丢弃选择。 */
  setProvider(sessionId: string | null | undefined, providerId: string | null): void {
    const sid = sessionId ?? "";
    aiStore.set((prev) => ({ providerBy: { ...prev.providerBy, [sid]: providerId } }));
  },

  /**
   * 发一轮对话。乐观压入用户气泡后置 `sending`，交给后端跑编排循环；期间 `ai-step` 事件
   * 由 [`aiChat.applyStep`] 压入步骤，审批由 [`aiChat.pushApproval`] 入队。resolve 后压入助手气泡。
   */
  async send(sessionId: string, text: string, images?: ChatImage[], files?: ChatFile[]): Promise<void> {
    const trimmed = text.trim();
    // 带图无话是合法输入（贴张报错截图让 AI 看）；带文档同理（丢个日志只问「看一下这份」）。
    // 三者都空才拦。
    if (!trimmed && !images?.length && !files?.length) return;
    // 轨道由当前页签决定：Agent 页=控制台轨（按连接钉死对话、只写 timelines）；
    // 聊天页=对话轨（按会话 chatViews、只写视图）。两轨互不污染，保证 Agent 展示=Agent 记忆。
    // 应用级问答（保留 id）恒走 chat 轨：它没有终端，Agent 轨的「落笔兑现」无从谈起；
    // 且首页聊天读的是 chatViews，若跟着落库 rail 走了 agent 轨会写进 timelines 而屏上看不见。
    const rail = settingsStore.get().rail;
    const track: "agent" | "chat" =
      sessionId !== APP_CHAT_SESSION && rail === "agent" ? "agent" : "chat";
    if (aiStore.get().turns[turnKey(sessionId, track)]?.sending) {
      // 本轨上一轮还在途（可能卡在命令审批等待）：不吞掉输入，明确告知用户。
      // chat 与 Agent 是同一连接上的独立两轨——另一轨在途不拦本轨（真·并发发起）。
      toast.show(t("ast.busy"));
      return;
    }
    // 乐观压入用户气泡（模型调用交给 runTurn；「重试」走 runTurn 复用此气泡，不再新增/删除）
    const userItem: ChatItem = {
      kind: "user",
      id: uid(),
      text: trimmed,
      ...(images?.length ? { images } : {}),
      ...(files?.length ? { files } : {}),
    };
    aiStore.set((prev) =>
      track === "agent"
        ? {
            // 控制台轨：只累积到该连接的时间线（Agent 侧边栏据此展示全程），不碰聊天页视图。
            timelines: {
              ...prev.timelines,
              [sessionId]: [...(prev.timelines[sessionId] ?? []), userItem],
            },
          }
        : {
            // 对话轨：写该会话自己的视图（后台会话也照常写各自的视图，这是并发核心）。
            chatViews: {
              ...prev.chatViews,
              [sessionId]: {
                activeId: prev.chatViews[sessionId]?.activeId ?? null,
                items: [...(prev.chatViews[sessionId]?.items ?? []), userItem],
                lastUsage: prev.chatViews[sessionId]?.lastUsage ?? null,
              },
            },
          },
    );
    await runTurn(sessionId, trimmed, track, images, files);
  },

  /**
   * 终止「该会话·该轨」正在途的 AI 轮次（chat 与 Agent 各一独立槽，互不牵连）。
   * 先删本轨 turns[key]（在途 runTurn 的 turnId 比对随即失配 → 陈旧结果静默丢弃），
   * 再本地收状态（只清本轨挂起审批/反问），最后通知后端按轨丢弃 run_turn future。
   * 语义=只停本会话该轨 AI 编排，不向远端发中断（已下发的命令继续跑，由用户自行处理）。
   */
  cancel(sessionId: string, track: "agent" | "chat"): void {
    const key = turnKey(sessionId, track);
    const tn = aiStore.get().turns[key];
    if (!tn?.sending) return;
    if (termPaint(sessionId, track)) {
      termAgent.writeTerminated(sessionId);
    } else {
      termAgent.endReplyWindow(sessionId);
    }
    aiStore.set((prev) => {
      const turns = { ...prev.turns };
      delete turns[key];
      return {
        turns,
        // run_turn 被 drop，本轨挂起审批的回执已无意义，从队列收起（按 surface 精确定位本轨，另一轨不动）。
        // surface 已经 surfaceOf 归一为恒非空，严格比对即可。
        approvals: prev.approvals.filter((a) => !(a.sessionId === sessionId && a.surface === track)),
        // 挂起的澄清反问同理：回执通道已随本轮作废
        questions: prev.questions.filter((q) => !(q.sessionId === sessionId && q.surface === track)),
      };
    });
    void api.cancel(sessionId, track).catch(() => {});
  },

  /**
   * 达往返上限优雅收尾后「继续」：发一条续跑指令作新一轮。
   * 上一轮已被归档（用户目标 + assistant 收尾说明），模型重建历史时能看到原任务，
   * 再叠加每轮现组的终端快照/cwd，即可从断点接上而不必从头重做。
   */
  continueTurn(sessionId: string, track: "agent" | "chat"): void {
    const tn = aiStore.get().turns[turnKey(sessionId, track)];
    if (!tn?.resume || tn.resume.kind !== "continue") return;
    void aiChat.send(sessionId, t("aw.continuePrompt"));
  },

  /**
   * 真错误后「重试」：本轮未落库，用户气泡留在原地，只重跑助手侧（runTurn）。
   */
  retry(sessionId: string, track: "agent" | "chat"): void {
    const tn = aiStore.get().turns[turnKey(sessionId, track)];
    if (!tn?.resume || tn.resume.kind !== "retry") return;
    // 纯图轮（只贴了图、不写字）正文为空：靠图片非空也能重试，否则失败卡上的按钮按下去没反应。
    // 文档轮同理：只丢个文件问一句也是合法输入，不能因正文空就早退。
    if (!tn.resume.text && !tn.resume.images?.length && !tn.resume.files?.length) return;
    // 用户那句气泡留在原地，只重跑助手侧（runTurn 会清 error/resume、置发送态）：
    // 轨道跟着 resume.track 走（不读残留轮次），重跑回到发起那一轨。
    // 图片/文档同样原样重发（路径与 base64 还在 resume 里），否则重跑出来的答复与屏上的附件对不上。
    void runTurn(sessionId, tn.resume.text ?? "", tn.resume.track, tn.resume.images, tn.resume.files);
  },

  /** 真 token 流：按 `kind` 分流累加。text=正文草稿、reasoning=推理链、tool=只置相位。
   *  非本会话在途、或归属轮次与当前在途 turnId 不符（上一轮被终止后迟到的增量）都丢弃——避免跨轮串台。 */
  appendStream(
    sessionId: string,
    delta: string,
    turn: number,
    kind: "text" | "reasoning" | "tool" = "text",
  ): void {
    const loc = locateTurn(sessionId, turn);
    if (!loc) return;
    const { key, tn } = loc;
    const patch: Partial<TurnState> =
      kind === "reasoning"
        ? { reasoningText: tn.reasoningText + delta, phase: "reasoning" }
        : kind === "tool"
          ? { phase: "tool" }
          : { streamingText: tn.streamingText + delta, phase: "text" };
    aiStore.set((prev) => ({
      turns: { ...prev.turns, [key]: { ...tn, ...patch } },
    }));
  },

  /** 处理 `ai-step`：压入该会话轮次的工具步骤（`final` 与 send 的回复重复，跳过）。 */
  applyStep(evt: AiStepEvent): void {
    // 事件只带 (sessionId, turn)、不带轨：在两轨槽里定位归属该 turn 且在途的那一槽
    //（turn 跨轨全局唯一 → 至多一个命中）。无命中=已结束/被删/迟到，直接丢弃避免跨轮串台。
    const loc = locateTurn(evt.sessionId, evt.turn);
    if (!loc) return;
    const tn = loc.tn;
    if (evt.step.kind === "final") return;
    const stepItem: ChatItem = { kind: "step", id: uid(), step: evt.step };
    // 结果回来即说明该命令已获批准并执行完，但发起审批那一刻压入的 toolCall 卡不会再更新——
    // 这里在数据层把同工具最近一张还挂「待批准」的命令卡收口（→AUTO 去黄标）。
    const settledTool = evt.step.kind === "toolResult" ? evt.step.tool : null;
    // 按本轮轨道只写一侧：Agent 轨→时间线（与展示/记忆同源）；Chat 轨→该会话自己的视图。
    // 两轨都不再比对「当前可见会话」——后台会话也照常写各自的，切回时直接读到。
    const collapse = evt.step.kind === "reasoning";
    aiStore.set((prev) => {
      const patch: Partial<AiStoreState> =
        tn.track === "agent"
          ? {
              timelines: {
                ...prev.timelines,
                [evt.sessionId]: settleResolvedCall(
                  [...(prev.timelines[evt.sessionId] ?? []), stepItem],
                  settledTool,
                  (prev.timelines[evt.sessionId] ?? []).length,
                ),
              },
            }
          : {
              chatViews: {
                ...prev.chatViews,
                [evt.sessionId]: {
                  activeId: prev.chatViews[evt.sessionId]?.activeId ?? null,
                  items: settleResolvedCall(
                    [...(prev.chatViews[evt.sessionId]?.items ?? []), stepItem],
                    settledTool,
                    (prev.chatViews[evt.sessionId]?.items ?? []).length,
                  ),
                  lastUsage: prev.chatViews[evt.sessionId]?.lastUsage ?? null,
                },
              },
            };
      // 推理步落轨迹 = 这段思考已完成：清空实时缓冲，展开的「思考中」行随即折叠成块内的一步（自动折叠）。
      const cur = prev.turns[loc.key];
      if (collapse && cur) {
        patch.turns = { ...prev.turns, [loc.key]: { ...cur, reasoningText: "" } };
      }
      return patch;
    });
  },

  /** 连接结束（关闭标签）：清掉该连接的时间线、钉死的对话、按会话隔离的视图/轮次/模型选择。 */
  clearSessionTimeline(sessionId: string): void {
    // 会话彻底没了：回收模块级轮次水位（同 key 不会被复用为新会话，回收仅为防 Map 无界增长）。
    turnGen.delete(sessionId);
    aiStore.set((prev) => {
      if (
        !(sessionId in prev.timelines) &&
        !(sessionId in prev.agentConvIds) &&
        !(sessionId in prev.chatConvIds) &&
        !(sessionId in prev.chatViews) &&
        !(sessionId in prev.providerBy) &&
        !(turnKey(sessionId, "agent") in prev.turns) &&
        !(turnKey(sessionId, "chat") in prev.turns)
      )
        return {};
      const timelines = { ...prev.timelines };
      const agentConvIds = { ...prev.agentConvIds };
      const chatConvIds = { ...prev.chatConvIds };
      const chatViews = { ...prev.chatViews };
      const providerBy = { ...prev.providerBy };
      const turns = { ...prev.turns };
      delete timelines[sessionId];
      delete agentConvIds[sessionId];
      delete chatConvIds[sessionId];
      delete chatViews[sessionId];
      delete providerBy[sessionId];
      delete turns[turnKey(sessionId, "agent")];
      delete turns[turnKey(sessionId, "chat")];
      return { timelines, agentConvIds, chatConvIds, chatViews, providerBy, turns };
    });
  },

  /**
   * 会话已结束（关标签 / 断线 / 掉线）：收掉属于它的在途痕迹。
   *
   * 后端 `session::teardown` 已同步取消本轮（`ai_send_message` 随即 reject），但这里不依赖那个时序：
   * 删除 turns[sid] 既解锁该会话、又让在途 runTurn 的 turnId 比对失配而丢弃陈旧结果。
   * 与 [`cancel`] 的区别：不给终端写「已终止」（会话连同终端都在销毁中），只收回复窗口并提示一句原因；
   * 不记「重试」挂起态（会话已不在，原地重跑没有意义）。视图/模型残留由随后 clearSessionTimeline 统一清。
   */
  abortSession(sessionId: string): void {
    const { turns, approvals, questions } = aiStore.get();
    const keys = [turnKey(sessionId, "agent"), turnKey(sessionId, "chat")];
    const anySending = keys.some((k) => turns[k]?.sending);
    const hasOurs = approvals.some((a) => a.sessionId === sessionId);
    const dropQ = questions.filter((q) => q.sessionId !== sessionId);
    if (!anySending) {
      // 该会话两轨都无在途轮次：只作废本会话的挂起审批/反问。
      if (hasOurs || dropQ.length !== questions.length) {
        aiStore.set({
          approvals: approvals.filter((a) => a.sessionId !== sessionId),
          questions: dropQ,
        });
      }
      return;
    }
    aiStore.set((prev) => {
      const nextTurns = { ...prev.turns };
      // 会话都没了，两轨在途槽一并删除即令在途 runTurn 的 turnId 比对失配
      for (const k of keys) delete nextTurns[k];
      return {
        turns: nextTurns,
        // 会话都没了，这些待裁决的命令再按下去也无处可执行
        approvals: hasOurs ? approvals.filter((a) => a.sessionId !== sessionId) : approvals,
        questions: dropQ,
      };
    });
    termAgent.endReplyWindow(sessionId);
    toast.show(t("ast.sessionGone"));
    // 会话连同两轨都在销毁中：向后端两轨都发一道终止（幂等，未在途那轨无副作用）。
    void api.cancel(sessionId, "agent").catch(() => {});
    void api.cancel(sessionId, "chat").catch(() => {});
  },

  /** 后端抛来一个待裁决的命令审批（去重入队）：终端里只补一行灰字提醒，
   * 真正的裁决入口与命令全文都在确认条上：终端 AI 开关开着在终端，关上后在右栏（AiDrawer / AgentPanel）。 */
  pushApproval(prompt: AiApprovalPrompt): void {
    const { approvals } = aiStore.get();
    if (approvals.some((a) => a.requestId === prompt.requestId)) return;
    // 归属轨优先用后端权威 `track`；缺失才按当前 rail 兑底猜一轨（见 surfaceOf），保证 surface 恒非空。
    const surface = surfaceOf(prompt.track, prompt.sessionId);
    aiStore.set({
      approvals: [...approvals, { ...prompt, surface }],
      detailApprovalId: null,
    });
    // 不再额外弹 toast：一行灰字 + 确认条已足够显眼，toast 只留给异常。
    // 命令不再在终端里重复落笔（授权一次、裁决一次共两遍）：批准后端会把命令打进 PTY，
    // shell 自己的回显就是控制台里唯一那份记录，与手打无异。
    if (termPaint(prompt.sessionId, surface)) {
      termAgent.writeApprovalHint(prompt.sessionId);
    }
    // 等你回执：窗口失焦时补一条 OS 横幅（聚焦时确认条已在屏上，不打断）。
    notifyOsAwait("approval");
  },

  /** 用户裁决命令审批：先出队（防重复点击），再回后端。请求已失效则静默忽略。
   * `edited` 非空 = 编辑改判（改后命令交后端重新分级）。终端里不再补画「✓已同意 + 命令」：
   * 批准时 shell 回显自会作证，拒绝时什么都没发生，无需在用户控制台上留双重痕迹。*/
  async respondApproval(
    requestId: string,
    accept: boolean,
    edited?: string,
    always?: boolean,
  ): Promise<void> {
    dismissApproval(requestId);
    try {
      await api.respondApproval(requestId, accept, edited, always);
    } catch (e) {
      // 超时或重复应答：后端多已按拒绝处理；真错误仍提示
      const msg = errMsg(e);
      if (msg && !/失效|超时|重复/i.test(msg)) {
        aiStore.set({ error: msg });
      }
    }
  },

  /** 后端抛来一个待作答的澄清反问（去重入队）。纯交互、不往终端落笔（问题卡只活在确认条槽位）。
   * 归属轨优先用后端下发的权威 `prompt.track`（并发时当前页签未必等于发起轨）；缺失才按 rail 兑底猜一轨，
   * 保证 surface 恒非空——既不因缺 track 而两轨都渲染（泄漏），也不因滤空而永久挂死（无卡可点）。 */
  pushQuestion(prompt: AiQuestionPrompt): void {
    const { questions } = aiStore.get();
    if (questions.some((q) => q.requestId === prompt.requestId)) return;
    aiStore.set({ questions: [...questions, { ...prompt, surface: surfaceOf(prompt.track, prompt.sessionId) }] });
    // 等你回执：与审批同策略，失焦时补一条 OS 横幅。
    notifyOsAwait("question");
  },

  /** 用户作答澄清反问：先出队（防重复点击），再回后端。请求已失效则静默忽略。
   * `answer` 非空=选项/自定义文本（多选由调用方拼好）；`aiDecides=true`=让 AI 自行决定。*/
  async respondQuestion(requestId: string, answer?: string, aiDecides?: boolean): Promise<void> {
    aiStore.set((prev) => ({ questions: prev.questions.filter((q) => q.requestId !== requestId) }));
    try {
      await api.respondQuestion(requestId, answer, aiDecides);
    } catch (e) {
      const msg = errMsg(e);
      if (msg && !/失效|超时|重复/i.test(msg)) {
        aiStore.set({ error: msg });
      }
    }
  },

  async rename(conversationId: string, title: string): Promise<void> {
    try {
      await api.renameConversation(conversationId, title);
      await aiChat.refreshConversations();
    } catch (e) {
      aiStore.set({ error: errMsg(e) });
    }
  },

  async remove(conversationId: string): Promise<void> {
    try {
      await api.deleteConversation(conversationId);
      aiStore.set((prev) => {
        // 抹掉任何指向被删对话的会话记忆，避免切标签时 focusSession / Agent 轨去开一条已删对话。
        const drop = (m: Record<string, string>) => {
          const next: Record<string, string> = {};
          for (const [k, v] of Object.entries(m)) if (v !== conversationId) next[k] = v;
          return next;
        };
        // 遍历各会话视图，凡正显示被删对话的 → 重置为空白新对话。
        const chatViews: Record<string, ChatView> = {};
        for (const [k, v] of Object.entries(prev.chatViews)) {
          chatViews[k] = v.activeId === conversationId ? { activeId: null, items: [], lastUsage: null } : v;
        }
        return {
          conversations: prev.conversations.filter((c) => c.id !== conversationId),
          chatViews,
          chatConvIds: drop(prev.chatConvIds),
          agentConvIds: drop(prev.agentConvIds),
        };
      });
    } catch (e) {
      aiStore.set({ error: errMsg(e) });
    }
  },

  clearError(): void {
    aiStore.set({ error: "" });
  },

  /** 就地卡片点「ⓘ详情」：打开该审批的五要素弹窗。*/
  showDetail(requestId: string): void {
    aiStore.set({ detailApprovalId: requestId });
  },

  /** 关闭五要素审批弹窗（不裁决；后端超时按拒绝，fail-closed）。 */
  hideDetail(): void {
    aiStore.set({ detailApprovalId: null });
  },
};

/**
 * AI 是否允许在终端画面上落笔（回复 / 待授权提示）。
 *
 * 落笔与否由「本轮发起时冻结的轨道 `track`」决定（调用方显式传入），**不读实时 `rail`**（关键：两轨独立、
 * 互不串扰）。轨道在发起那刻定死（send 按当时页签、pushApproval 按后端下发的 prompt.track、retry 按 resume.track），
 * 此后整轮——含流式回复、待授权提示、错误回写——都归这条轨，用户在途切右栏页签也改不了它的归属：
 * - 聊天轨发起（track=chat）：主工作窗口就是右侧聊天窗，**硬禁**一切终端落笔（否则就是「聊天内容漏进控制台」的串轨 bug）。
 * - Agent 轨发起（track=agent）：主工作窗口是控制台，终端即主场，落笔与否再由逐终端 `terminalAi` 开关决定。
 * 注意只管「画到终端」，不管审批本身——确认条与裁决流程照旧。
 * 双槽后不再从 `turns` 反查轨道：调用方各自持有本轮的轨，直接传入即可（也免去须在删改条目前调用的时序约束）。
 */
function termPaint(sessionId: string | null | undefined, track: "agent" | "chat"): boolean {
  if (track === "chat") return false;
  return effectiveTerminalAi(sessionId) === "on";
}

/**
 * 反推/兑底归属轨：一律以**后端权威 `track`** 为准（并发时当前页签未必等于发起轨）。
 * 仅当 `track` 缺失（旧后端/前后端版本错配、事件未带此字段）才按当前 rail + canAgentRail
 * 兑底猜**单一**一轨——保证 surface 恒非空：既不因滤空而「两轨都渲染」（跨轨泄漏），
 * 也不因滤空而「哪都不渲染」（永久挂死、无卡可点）。正常构建 track 恒在，不走猜测分支。
 */
function surfaceOf(
  track: "agent" | "chat" | undefined,
  sessionId: string,
): "agent" | "chat" {
  if (track) return track;
  const rail = settingsStore.get().rail;
  const kind = sessionStore.get().tabs.find((tab) => tab.id === sessionId)?.kind ?? null;
  return rail === "agent" && canAgentRail(kind) ? "agent" : "chat";
}

/**
 * 「等你回执」角标只读的两个字段：布局组件（App / 页签栏 / 下栏 / 侧轨 / 弹出窗）用
 * `useStorePick(aiStore, AI_QUEUE_KEYS)` 订阅，不随 `turns` 的逐 token 流式写入重渲染。
 */
export const AI_QUEUE_KEYS = ["approvals", "questions"] as const;

/**
 * 「等你回执」的统一判据：挂起中的澄清反问 + 命令审批所在轨的集合（空集 = 没人在等你）。
 * 两类卡都必须等前端应答后端 `run_turn` 才继续（反问不设超时，审批超时按拒绝兜底 fail-closed），
 * 用户切到别的会话 / 把 AI 栏收起 / 在 Agent 与聊天子页签间切错，就等于整轮挂住——
 * 会话标签、AI 子页签、停靠页签的角标全部取这一个数，不各算各的口径。
 * `surface` 已在入队时经 [`surfaceOf`] 归一为恒非空，故可直接按轨比对。
 */
export function awaitingTracks(
  approvals: AiApprovalPrompt[],
  questions: AiQuestionPrompt[],
  sessionId?: string | null,
): Set<"agent" | "chat"> {
  const s = new Set<"agent" | "chat">();
  for (const it of approvals) {
    if (sessionId != null && it.sessionId !== sessionId) continue;
    if (it.surface) s.add(it.surface);
  }
  for (const it of questions) {
    if (sessionId != null && it.sessionId !== sessionId) continue;
    if (it.surface) s.add(it.surface);
  }
  return s;
}

/**
 * 归档轨迹反序列化：坏数据（本列上线前的老对话、JSON 结构变更）一律当「没有过程」，
 * 不能让一条脏数据毁掉整段问答的回看。
 */
function parseSteps(raw: string | null): AiStep[] {
  if (!raw) return [];
  try {
    const v: unknown = JSON.parse(raw);
    return Array.isArray(v) ? (v as AiStep[]) : [];
  } catch {
    return [];
  }
}

/**
 * 把时间线里从 `from` 往前数最近一张同工具、还挂着「待批准」的 toolCall 命令卡收口为 AUTO
 * （去掉黄标）。命令卡是发起审批那一刻压入的，之后不会自动更新，toolResult 到达即说明已
 * 批准并执行完，靠这个在数据层补一刀。找不到/已收口则原样返回。
 */
function settleResolvedCall(items: ChatItem[], tool: string | null, from: number): ChatItem[] {
  if (!tool) return items;
  for (let i = Math.min(from, items.length - 1); i >= 0; i--) {
    const it = items[i];
    if (it.kind === "step" && it.step.kind === "toolCall" && it.step.tool === tool) {
      if (it.step.decision !== "NEEDS_APPROVAL") return items;
      const next = items.slice();
      next[i] = { ...it, step: { ...it.step, decision: "AUTO" } };
      return next;
    }
  }
  return items;
}

function dismissApproval(requestId: string): void {
  const { approvals, detailApprovalId } = aiStore.get();
  aiStore.set({
    approvals: approvals.filter((a) => a.requestId !== requestId),
    // 裁决后若详情弹窗正开着这条，一并收起
    detailApprovalId: detailApprovalId === requestId ? null : detailApprovalId,
  });
}

/**
 * 归档附件清单反序列化：坏数据（本列上线前的老对话、JSON 结构变更）一律当「没有附件」，
 * 与 [`parseSteps`] 同口径：不能让一条脏数据毁掉整段问答的回看。
 *
 * 只解一次：图片与文档两个视图都从同一份 refs 派生（下面两个 `refsTo*`）——
 * 各自重解一遍 JSON 就是两条会慢慢漂移的通路。
 */
function parseRefs(raw: string | null): AiAttachment[] {
  if (!raw) return [];
  try {
    const v: unknown = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return (v as AiAttachment[]).filter((a) => typeof a?.id === "string");
  } catch {
    return [];
  }
}

/** 清单里的图片行。旧行不带 `kind`，前端也要给出同一缺省（后端 `#[default] Image`）。
 *  不能写成 `a.kind === "image"`：那是把历史数据误判成「没图」，缩略图整排消失。 */
function refsToImages(refs: AiAttachment[]): ChatImage[] {
  return refs
    .filter((a) => (a.kind ?? "image") === "image")
    .map((a) => ({ id: a.id, name: a.name || "image" }));
}

/** 清单里的文档行：不靠路径取字节，只拿元数据渲染 chip（抽取文本已在后端投送过）。 */
function refsToFiles(refs: AiAttachment[]): ChatFile[] {
  return refs
    .filter((a) => (a.kind ?? "image") !== "image")
    .map((a) => ({
      id: a.id,
      path: a.origin?.path ?? "",
      name: a.name || a.id,
      bytes: a.bytes,
      mime: a.mime,
      kind: a.kind,
      chars: a.chars,
      degraded: a.degraded,
    }));
}

/**
 * 时间线上的图 → 后端 `TurnImage`：只发 `dataUrl` 逗号后的 base64（不在 store 里另存一份）。
 * 只有 `id`、本地无字节的（历史恢复）直接丢弃：重试只会发生在「本轮未落库」的真错误上，
 * 那种轮次的图必定还在内存里；真把空串发过去会被后端当非法图片拒发。
 */
function toTurnImages(images?: ChatImage[]): AiTurnImage[] {
  return (images ?? [])
    .filter((img) => !!img.dataUrl)
    .map((img) => ({
      mime: img.mime ?? "image/png",
      name: img.name,
      data: (img.dataUrl as string).slice((img.dataUrl as string).indexOf(",") + 1),
    }));
}

/** 待发文档 → 后端 `TurnFile`：只递路径。预探失败的 chip（`error` 非空）不发：
 *  它本就被拒在读盘之前，发过去只会让后端再错一次、把整轮拖脏。 */
function toTurnFiles(files?: ChatFile[]): AiTurnFile[] {
  return (files ?? [])
    .filter((f) => !f.error && !!f.path)
    .map((f) => ({ path: f.path, name: f.name }));
}

/**
 * 跑一轮模型调用：置发送态 → await 后端编排 → 成功压助手气泡 / 真错误记「重试」挂起态。
 * 不碰用户气泡：首次由 send 压入，「重试」复用已在场的那条（对齐主流「原地重新生成」，无重发闪动）。
 */
async function runTurn(
  sessionId: string,
  trimmed: string,
  track: "agent" | "chat",
  images?: ChatImage[],
  files?: ChatFile[],
): Promise<void> {
  const { chatViews, providerBy, agentConvIds } = aiStore.get();
  // 轮次序号取自模块级每会话单调水位（不随 cancel/abort 删条目归零）：自增后写回水位并快照进本条目。
  const myTurn = (turnGen.get(sessionId) ?? 0) + 1;
  turnGen.set(sessionId, myTurn);
  // 本轨的在途槽键（同会话 chat/agent 各一独立槽）；turnGen 仍按 sessionId 共享水位 → turn 跨轨全局唯一。
  const key = turnKey(sessionId, track);
  // 轨道由调用方显式传入（send 按当时页签捕获；retry 按 resume.track），存进本会话轮次条目：
  // Agent 轨用按连接钉死的 agentConvIds[sessionId]；Chat 轨用该会话自己的视图 chatViews[sessionId].activeId。
  // 只有 Agent 轨才占用终端推理窗口（期间该会话的 Enter 由 Terminal.tsx 排队防丢失）；
  // Chat 轨的终端是纯 shell，不该因为在面板里聊一句就把用户终端上的回车静默排队。
  if (track === "agent") termAgent.beginReply(sessionId);
  const conversationId =
    track === "agent" ? agentConvIds[sessionId] ?? null : chatViews[sessionId]?.activeId ?? null;
  const fallback = providerBy[sessionId] ?? defaultProviderId(providerStore.get().items) ?? null;
  aiStore.set((prev) => ({
    turns: {
      ...prev.turns,
      [key]: { turnId: myTurn, track, sending: true, startedAt: Date.now(), streamingText: "", reasoningText: "", phase: "idle", resume: null, error: "", lastUsage: prev.turns[key]?.lastUsage ?? null },
    },
  }));
  // 思考指示走终端底部状态条（Terminal.tsx），不往 xterm 缓冲写行
  const sendImages = toTurnImages(images);
  const sendFiles = toTurnFiles(files);
  try {
    const reply = await api.send({
      sessionId,
      text: trimmed,
      conversationId,
      providerId: fallback,
      // 本轮序号：后端经 ai_turn 水位在每条 ai-step/ai-stream 上原样回带，供跨轮迟到事件过滤。
      turn: myTurn,
      // 能力由轨道决定：Agent 轨=全能力（读写）；Chat 轨=按「只读/读写」开关（chatMode）。
      mode: track === "agent" ? "agent" : settingsStore.get().chatMode,
      // 执行通道同样由发起那刻的轨道决定（不依赖后端库里的页签值，那只是按会话视图态的残留）：
      // Agent 轨=命令打进本终端（像手打），Chat 轨=后台静默 exec。
      rail: track,
      // 界面有效语言（前端解析后的实际生效语言名）：驱动后端 {{language}} 宏，让 AI 跟随界面语言。
      lang: getUiLangName(),
      // AI 发起下载时的原生保存目录框标题：前端本地化后随轮下发，后端不写死展示串。
      downloadPickTitle: t("dlg.aiDownloadPick"),
      // 本轮附件：无图/无件时整个字段不下发（后端 `#[serde(default)]` → 空 Vec → 纯文本轮）。
      ...(sendImages.length ? { images: sendImages } : {}),
      ...(sendFiles.length ? { files: sendFiles } : {}),
    });
    if (aiStore.get().turns[key]?.turnId !== myTurn) return; // 已被终止/新一轮接管：丢弃这次结果，不压助手气泡
    const assistantItem: ChatItem = { kind: "assistant", id: uid(), text: reply.text };
    // 终端落笔判定按本轮的轨（track 在闭包内直接可得）——不再从 turns 反查。
    const paint = termPaint(sessionId, track);
    aiStore.set((prev) => {
      const cur = prev.chatViews[sessionId];
      // 轮次收尾：保留条目（inert：sending=false）以便 retry/continue 读 resume；
      // 下一次 runTurn 会把 turnId 自增，陈旧回调随即失配。自然答复不带 resume。
      const done: TurnState = {
        turnId: myTurn,
        track,
        sending: false,
        startedAt: prev.turns[key]?.startedAt ?? 0,
        streamingText: "",
        reasoningText: "",
        phase: "idle",
        resume: reply.resumable ? { track, kind: "continue" as const } : null,
        error: "",
        lastUsage: reply.usage ?? prev.turns[key]?.lastUsage ?? null,
      };
      if (track === "agent") {
        return {
          turns: { ...prev.turns, [key]: done },
          agentConvIds: { ...prev.agentConvIds, [sessionId]: reply.conversationId },
          timelines: {
            ...prev.timelines,
            [sessionId]: [...(prev.timelines[sessionId] ?? []), assistantItem],
          },
        };
      }
      // Chat 轨：写该会话自己的视图（后台会话也照常写各自的），记 chatConvIds/activeId/lastUsage。
      return {
        turns: { ...prev.turns, [key]: done },
        chatViews: {
          ...prev.chatViews,
          [sessionId]: {
            activeId: reply.conversationId,
            items: [...(cur?.items ?? []), assistantItem],
            lastUsage: reply.usage ?? cur?.lastUsage ?? null,
          },
        },
        chatConvIds: { ...prev.chatConvIds, [sessionId]: reply.conversationId },
      };
    });
    if (reply.memoriesSaved > 0) {
      toast.show(t("ast.memSaved").replace("{n}", String(reply.memoriesSaved)));
    }
    // 系统提示词体检告警（开发者改坏模板时后端随本轮回推）：非阻断，只提醒去检查或恢复默认。
    if (reply.promptWarning) {
      toast.warn(reply.promptWarning);
    }
    // 图片归档失败（写盘报错）：本轮答复不受影响，只告知“这张图下次看不到了”，不阻断对话。
    if (reply.attachWarning) {
      toast.warn(reply.attachWarning);
    }
    // 终端里按「回复详略」档收口行数（详细档 = 0 不截）；侧栏与历史始终是全文。
    if (paint) {
      termAgent.writeReply(sessionId, reply.text, REPLY_TERM_LINES[settingsStore.get().replyVerbosity]);
    } else {
      termAgent.endReplyWindow(sessionId);
    }
    void aiChat.refreshConversations();
    notifyOsReply();
  } catch (e) {
    if (aiStore.get().turns[key]?.turnId !== myTurn) return; // 终止导致的 reject：静默收场，不报错、不在终端落笔
    const paint = termPaint(sessionId, track);
    aiStore.set((prev) => ({
      turns: {
        ...prev.turns,
        [key]: {
          turnId: myTurn,
          track,
          sending: false,
          startedAt: prev.turns[key]?.startedAt ?? 0,
          streamingText: "",
          reasoningText: "",
          phase: "idle",
          error: errMsg(e),
          lastUsage: prev.turns[key]?.lastUsage ?? null,
          // 真错误（本轮未落库）→ 记「重试」挂起态（带轨道），保存那句原样用户文本与本轮附件以便原地重跑。
          resume: { track, kind: "retry", text: trimmed, images, files },
        },
      },
    }));
    if (paint) {
      termAgent.writeReply(sessionId, `${t("ast.errPrefix")}${errMsg(e)}`, REPLY_TERM_LINES[settingsStore.get().replyVerbosity]);
    } else {
      termAgent.endReplyWindow(sessionId);
    }
  }
}
