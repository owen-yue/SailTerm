// AI 对话：编排入口 + 审批应答 + 对话/消息归档，编排步骤与命令审批走事件。
//
// `ai_send_message` 是**长调用**：后端在里面跑「模型 → 工具 → 审批 → 脱敏」的循环，期间通过
// `ai-step`（工具时间线，流式渲染）与 `ai-approval-request`（命令执行前审批）两个事件与前端互动，
// 循环结束才 resolve 出 `ChatReply`。凭证与 API Key 永不经这些通道。
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AiApprovalPrompt,
  AiChatMessage,
  AiConversation,
  AiQuestionPrompt,
  AiSendMessage,
  AiStepEvent,
  AttachmentData,
  AttachmentProbe,
  ChatReply,
  PromptPreview,
  PromptTemplate,
  StagedImage,
  UsageStats,
} from "../../types";
import { call } from "./client";

/** 编排过程事件名（负载 `{ sessionId, step }`）。 */
export const EVENT_AI_STEP = "ai-step";
/** 命令审批请求事件名（负载 `AiApprovalPrompt`）。 */
export const EVENT_AI_APPROVAL = "ai-approval-request";
/** 澄清反问请求事件名（负载 `AiQuestionPrompt`）。 */
export const EVENT_AI_QUESTION = "ai-question-request";
/** 文本 token 流事件：`{sessionId, delta, kind}`（kind=text|reasoning|tool；tool 时 delta 空）。 */
export const EVENT_AI_STREAM = "ai-stream";

export const ai = {
  /** 跑一轮对话（长调用，期间 emit 步骤/审批事件）。 */
  send: (params: AiSendMessage) => call<ChatReply>("ai_send_message", { params }),

  /**
   * 连接失败诊断：失败的连接没有活动会话，走不了 `send`。后端复用 provider 跑一条
   * 单轮、无工具、不写库的 completion，把前端拼好的连接日志换成失败归因 + 下一步建议文本。
   */
  diagnoseConnect: (log: string, providerId?: string | null) =>
    call<string>("ai_diagnose_connect", { providerId: providerId ?? null, log }),

  /** 终止该会话**该轨**正在进行的 AI 编排轮次（后端丢弃该轨 run_turn future，另一轨不受牵连）。只停 AI，不动远端已跑命令。 */
  cancel: (sessionId: string, track: "agent" | "chat") =>
    call<void>("ai_cancel_turn", { sessionId, track }),

  /** 当前挂起命令审批的快照（后端内存表）：事件是瞬时的，新挂载/重挂载的窗口错过
   * `ai-approval-request` 就没有卡可点，而 `run_turn` 正挂在回执上等裁决——拉起这份快照重入队。
   * 幂等纯读；重复 requestId 由 `pushApproval` 去重。*/
  listPendingApprovals: () => call<AiApprovalPrompt[]>("ai_list_pending_approvals"),

  /** 当前挂起澄清反问的快照（后端内存表）：与审批同口径——新挂载/重挂载的窗口错过
   * `ai-question-request` 就没卡可答，而 `run_turn` 正挂在回执上等作答。
   * 幂等纯读；重复 requestId 由 `pushQuestion` 去重。*/
  listPendingQuestions: () => call<AiQuestionPrompt[]>("ai_list_pending_questions"),

  /** 应答命令审批；返回 `false` 表示请求已失效（超时/重复应答/未知 id）。
   * `edited` 非空 = 编辑改判：后端对改后命令重新分级再决定执行/拦截/再审批。*/
  respondApproval: (requestId: string, accept: boolean, edited?: string, always?: boolean) =>
    call<boolean>("ai_respond_approval", {
      requestId,
      accept,
      edited: edited ?? null,
      always: always ?? false,
    }),

  /** 应答澄清反问；返回 `false` 表示请求已失效。`answer` 非空=用户作答（整卡选项/自填文本，多题/多选拼成一段）；
   * `answer` 空且 `aiDecides=true`=让 AI 自行决定（新 UI 已无此出口，仅保留入参兼容）。*/
  respondQuestion: (requestId: string, answer?: string, aiDecides?: boolean) =>
    call<boolean>("ai_respond_question", {
      requestId,
      answer: answer ?? null,
      aiDecides: aiDecides ?? false,
    }),

  /** 按连接（或全局）列对话，最近更新在前。 */
  listConversations: (connectionId?: string | null) =>
    call<AiConversation[]>("ai_conversation_list", { connectionId: connectionId ?? null }),

  createConversation: (
    connectionId?: string | null,
    title?: string | null,
    providerId?: string | null,
  ) =>
    call<AiConversation>("ai_conversation_create", {
      connectionId: connectionId ?? null,
      title: title ?? null,
      providerId: providerId ?? null,
    }),

  renameConversation: (id: string, title: string) =>
    call<void>("ai_conversation_rename", { id, title }),

  /** 删除对话及其全部消息。 */
  deleteConversation: (id: string) => call<void>("ai_conversation_delete", { id }),

  /** 取一个对话的最近 `limit` 条消息（时间正序，用于渲染与重建上下文）。 */
  listMessages: (conversationId: string, limit?: number | null) =>
    call<AiChatMessage[]>("ai_message_list", { conversationId, limit: limit ?? null }),

  /**
   * 📎 选文件入口专用：把本地路径的图读成 base64 交前端压缩归一（WebView 读不了任意本地字节，
   * 而粘贴/拖拽拿得到 File 对象无需走这里）。不写库、不落盘：真正要发的 base64 回 `send` 的 `images`。
   */
  stageImage: (path: string) => call<StagedImage>("ai_stage_image", { path }),

  /** 📎 选完文件的就地预探（文档类）：**只判不发** —— 只读头部判型、查体积与受保护文件，
   *  不抽取、不落盘。没必要再往前扩：「选个 500MB 日志 / 选了 `vault.key` / 选了目录」都是
   *  选完那一刻就能知道的事，不前移到 chip 上就会变成「点了发送但什么也没发生」。 */
  probeAttachment: (path: string) => call<AttachmentProbe>("ai_probe_attachment", { path }),

  /** 历史气泡缩略图的按需拉取（按附件 id 直取）：清单元数据已随消息回传，字节只在本屏真要看时取。
   *  走这道而非开 asset protocol scope，是为了不新增一个本地可读面（与收紧权限的取向一致）。 */
  getAttachment: (id: string) => call<AttachmentData>("ai_get_attachment", { id }),

  /** 用量统计汇总（「用量统计」弹窗）；时区偏移由前端带入换算本地自然日。 */
  usageStats: () =>
    call<UsageStats>("ai_usage_stats", { tzOffsetSecs: -new Date().getTimezoneOffset() * 60 }),

  // ── 系统提示词模板（仅开发者模式；前端经 devMode 门禁才调）──
  /** 读取当前模板覆盖版 + 内置默认（面板初值）。 */
  promptGet: () => call<PromptTemplate>("ai_prompt_get"),
  /** 写入覆盖模板（空/纯空白 = 清除覆盖，回落内置默认）。 */
  promptSet: (template: string) => call<PromptTemplate>("ai_prompt_set", { template }),
  /** 恢复默认：删覆盖键。 */
  promptReset: () => call<PromptTemplate>("ai_prompt_reset"),
  /** 预览：用传入模板以样例动态块渲染（不写库）；空串 = 预览内置默认。`lang` = 当前界面有效语言。 */
  promptPreview: (template: string, lang: string) =>
    call<PromptPreview>("ai_prompt_preview", { template, lang }),

  /** 订阅编排步骤（工具调用/结果/最终答复/失败）。 */
  onStep: (cb: (p: AiStepEvent) => void): Promise<UnlistenFn> =>
    listen<AiStepEvent>(EVENT_AI_STEP, (e) => cb(e.payload)),

  /** 订阅命令审批请求（前端弹审批卡片）。 */
  onApproval: (cb: (p: AiApprovalPrompt) => void): Promise<UnlistenFn> =>
    listen<AiApprovalPrompt>(EVENT_AI_APPROVAL, (e) => cb(e.payload)),

  /** 订阅澄清反问请求（前端渲染澄清卡）。 */
  onQuestion: (cb: (p: AiQuestionPrompt) => void): Promise<UnlistenFn> =>
    listen<AiQuestionPrompt>(EVENT_AI_QUESTION, (e) => cb(e.payload)),

  /** 真 token 流：每段增量回调一次（`kind` 分正文/推理/工具起手；携 `turn` 归属轮次，前端据此丢弃跨轮迟到增量）。 */
  onStream: (
    cb: (p: { sessionId: string; delta: string; kind: "text" | "reasoning" | "tool"; turn: number }) => void,
  ): Promise<UnlistenFn> =>
    listen<{ sessionId: string; delta: string; kind: "text" | "reasoning" | "tool"; turn: number }>(
      EVENT_AI_STREAM,
      (e) => cb(e.payload),
    ),
};
