import { useEffect, useMemo, useRef, useState, type MouseEvent, type KeyboardEvent, type ClipboardEvent, type DragEvent } from "react";
import ProviderFormDialog from "../settings/ProviderFormDialog";
import ApprovalBar from "./ApprovalBar";
import QuestionBar from "./QuestionBar";
import EditableSelect from "../common/EditableSelect";
import Popconfirm from "../common/Popconfirm";
import { showImage } from "../file/ImageLightbox";
import { StepGroup, TimelineItem, groupTimeline } from "./AiTimeline";
import ContextRing from "./ContextRing";
import { focusTerm } from "../../lib/termAgent";
import { canAgentRail, hasAnyCap } from "../../lib/capabilities";
import { IconClose, IconPaperclip } from "../common/icons";
import { normalizeImage, stagedToFile, IMG_MAX_COUNT } from "../../lib/chatImage";
import type { ImageReject } from "../../lib/chatImage";
import { FILE_MAX_COUNT, kindLabel } from "../../lib/chatFile";
import { formatBytes } from "../../lib/format";
import { useElapsedSec, formatElapsed, thinkingKeyForPhase } from "../../lib/useElapsed";
import { onOsFileDrop } from "../../lib/tauri/file";
import { ai as aiApi, dialog, type UnlistenFn } from "../../lib/tauri";
import { errMsg } from "../../lib/tauri/client";
import { useI18n } from "../../lib/i18n";
import type { ChatFile, ChatImage, ChatItem } from "../../stores";
import type { AiProvider, AttachmentProbe } from "../../types";
import { APP_CHAT_SESSION } from "../../types";
import {
  aiChat,
  aiStore,
  defaultProviderId,
  providerActions,
  providerStore,
  sessionStore,
  settingsActions,
  settingsStore,
  toast,
  turnKey,
  useStore,
} from "../../stores";

interface Props {
  open: boolean;
  onClose?: () => void;
  /** 嵌入会话侧栏：无遮罩/浮层，作为聊天页内容。 */
  embedded?: boolean;
  sessionId?: string;
}

/**
 * 各会话未发送的输入草稿（sessionId → contenteditable 的 innerHTML），放在模块级：
 * 切「Agent | 聊天」子页签、收起/展开右栏面板都会卸载 AiDrawer，实例内的 Map 会连草稿一起没了。
 * 存 HTML 而非纯文本是为了保住内联文件 chip（顺序与 `data-*` 都在 DOM 里）；
 * 内容只可能来自纯文本键入/粘贴（粘贴已强制转纯文本）与自家 chip，回填 innerHTML 无注入面。
 */
const sessionDrafts = new Map<string, string>();

/**
 * 各会话待发送的图片（sessionId → 已归一的图），同样放在模块级：它们与草稿是同一条输入
 * 的两个部分，收起/展开右栏、切页签卸载 AiDrawer 时不能一个记得一个忘了。
 * 存的是归一后的 dataUrl（既用于缩略图，也是发送时抽取 base64 的唯一留存）。
 */
const sessionImages = new Map<string, ChatImage[]>();

/**
 * 各会话待发送的本地文档（sessionId → 已就地预探的件），同样放在模块级（与草稿/图片同口径）。
 * 存的是**路径与元信息**，不是字节：读盘与抽取都在后端本轮开始时做，前端只负责把预探结果摆上屏。
 */
const sessionFiles = new Map<string, ChatFile[]>();

/** 会话无视图时的稳定空引用（保持 items 依赖身份不变，避免每帧重算 groupTimeline）。 */
const EMPTY_ITEMS: ChatItem[] = [];

// 终端选区 chip 的正文载体：选区原文随 chip 走，发送时才展开成 ```text 围栏。
// 存进 dataset 前过一遍 UTF-8 字节 → base64：草稿以 innerHTML 落盘/回填，裸文本里的
// 引号、尖括号、换行会在序列化时损坏，编码后只剩安全字符集（与 btoa/atob 的 latin1 陷阱无关）。
const encTermText = (s: string): string => btoa(String.fromCharCode(...new TextEncoder().encode(s)));
const decTermText = (s: string): string =>
  new TextDecoder().decode(Uint8Array.from(atob(s), (c) => c.charCodeAt(0)));

/**
 * 最近会话页签条总开关：暂时隐藏（用户当前不需要）。
 * 数据链路（recentConvs / open / chatBusyHere 拦截）均保留，后期做「多个 AI 标签并发」
 * 时把此值改回 true 即恢复显示。
 */
const SHOW_RECENT_TABS = false;

/**
 * AI 助手（聊天页）：问答时间线 + 输入。
 * `embedded` 时作为 SessionRail 的「聊天」页，不再浮层。
 */
export default function AiDrawer({ open, onClose, embedded = false, sessionId }: Props) {
  const tr = useI18n();
  const { conversations, turns, chatViews, providerBy, error, draft, chipReq, termReq } = useStore(aiStore);
  const { rail, chatMode } = useStore(settingsStore);
  const { items: providers } = useStore(providerStore);
  const { tabs, activeId: sessId } = useStore(sessionStore);

  // 模型新增/编辑弹框（共用 ProviderFormDialog）：null=关；provider=null 新建、非空编辑该条。
  const [pv, setPv] = useState<{ provider: AiProvider | null } | null>(null);
  const [histOpen, setHistOpen] = useState(false);
  /** 待删除的对话 + 气泡锚点（取 × 按钮位置）：整段对话不可恢复，需就地确认。 */
  const [delConv, setDelConv] = useState<{ id: string; title: string; x: number; y: number } | null>(null);
  const [hasInput, setHasInput] = useState(false);
  const scroller = useRef<HTMLDivElement>(null);
  const edRef = useRef<HTMLDivElement>(null);
  /**
   * 未发送的输入草稿，按会话分档（sessionId → contenteditable 的 innerHTML）。
   * 编辑区是非受控 DOM，切会话时本组件被复用、DOM 不重建：不做隔离的话，
   * 给 A 服务器写的命令会原样出现在 B 会话的输入框里，回车即误发到另一台机器。
   * 存在组件外：切「Agent | 聊天」子页签、收起/展开右栏面板都会卸载本组件，
   * 放实例内会把一条还没发出去的命令连同草稿一起丢掉；模块级则跟着会话走。
   */
  const drafts = useRef(sessionDrafts);
  /** 编辑区当前展示的是哪个会话的草稿；DOM 卸载（关闭面板）后置空，下次打开据此重放。 */
  const shownKey = useRef<string | null>(null);

  const talkSession = sessionId ?? sessId;
  // 应用级问答（首页 AI 聊天）：保留 id 不是真实会话，没有终端/执行通路，
  // 但问答本身合法——门控/历史作用域/草稿回收都要为它单独放行（见各处 appChat 分支）。
  const appChat = talkSession === APP_CHAT_SESSION;
  const draftKey = talkSession ?? "";
  // —— 并发化：轮次与聊天视图都按会话（talkSession）取，切标签/后台轮次互不串台 ——
  const csid = talkSession ?? "";
  const view = chatViews[csid];
  const items = view?.items ?? EMPTY_ITEMS;
  const activeId = view?.activeId ?? null;
  const lastUsage = view?.lastUsage ?? null;
  const tn = turns[turnKey(csid, "chat")];
  const sending = tn?.sending === true;
  // 思考占位走字：本轮已耗时每秒更新（模型静默思考段也能看出没卡住）
  const elapsed = useElapsedSec(tn?.startedAt, sending);
  // 分相位提示（推理中 / 生成命令中 / 思考中）；聊天抽屉不渲推理链正文，只细分标签。
  const phase = tn?.phase ?? "idle";
  const resume = tn?.resume ?? null;
  const sendError = tn?.error ?? "";
  const providerId = providerBy[csid] ?? null;
  /** 本会话待发送的图片：模块级 Map 是唯一真相（同 sessionDrafts），这里的 state 只镜像给 React 渲染。 */
  const [pending, setPending] = useState<ChatImage[]>(() => sessionImages.get(draftKey) ?? []);
  const commitImages = (next: ChatImage[]): void => {
    if (next.length) sessionImages.set(draftKey, next);
    else sessionImages.delete(draftKey);
    setPending(next);
  };
  /** 本会话待发送的文档：与图片同一个模块级 Map 机制（唯一真相在 Map，state 只供渲染）。 */
  const [pendingFiles, setPendingFiles] = useState<ChatFile[]>(() => sessionFiles.get(draftKey) ?? []);
  const commitFiles = (next: ChatFile[]): void => {
    if (next.length) sessionFiles.set(draftKey, next);
    else sessionFiles.delete(draftKey);
    setPendingFiles(next);
  };
  const activeTab = tabs.find((t) => t.id === talkSession) ?? null;
  // 历史跟服务器走（别把所有连接的历史堆在一个列表里）：
  // 有档案按档案过滤；直连会话无档案，退到「未归属服务器」那批；没有会话时不过滤。
  // 应用级问答的对话不归属任何服务器（后端 connection_id 恒 None），落「未归属」那批。
  const scope = appChat ? null : activeTab ? activeTab.profileId ?? null : undefined;
  // 连续过程步骤归成可折叠块（默认收起）；用 useMemo 保持块 key 稳定，展开状态才不会被刷掉
  const runs = useMemo(() => groupTimeline(items), [items]);

  // 打开时刷新（嵌入模式也刷）；切标签/换服务器后重拉当前作用域的历史
  useEffect(() => {
    if (!open) return;
    aiChat.setScope(scope);
    void providerActions.refresh();
  }, [open, embedded, scope]);

  // 切终端标签时把聊天显示重绑到该 session 自己的对话（修“新开终端还显上一个 session”的串台 bug）。
  useEffect(() => {
    if (open) aiChat.focusSession(talkSession ?? null);
  }, [open, talkSession]);

  // 时间线增长或进入「思考中」时自动滚到底
  useEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [items, sending]);

  // —— contenteditable 编辑区：文字与文件 chip 同处一个流（行内附件形态）——
  // 按 DOM 顺序读出正文文字、内联文件 chip 与终端选区 chip（两者都不进正文，只作附件，发送时各自展开）。
  // 文件 chip 归 `refs`（📎 头），终端选区 chip 归 `terms`（```text 围栏）：两者都挂 data-ref 供
  // 「是否有内容」判定共用，但语义不同，必须按 data-term 分流——否则选区 chip 会被当成空路径的文件引用。
  const readEditor = (): {
    text: string;
    refs: { path: string; remote: boolean; isDir: boolean }[];
    terms: string[];
  } => {
    const el = edRef.current;
    const out: {
      text: string;
      refs: { path: string; remote: boolean; isDir: boolean }[];
      terms: string[];
    } = { text: "", refs: [], terms: [] };
    if (!el) return out;
    const walk = (node: Node): void => {
      node.childNodes.forEach((c) => {
        if (c.nodeType === Node.TEXT_NODE) out.text += c.nodeValue ?? "";
        else if (c.nodeType === Node.ELEMENT_NODE) {
          const e = c as HTMLElement;
          if (e.dataset.term) out.terms.push(e.dataset.text ?? "");
          else if (e.dataset.ref) out.refs.push({ path: e.dataset.path ?? "", remote: e.dataset.remote === "1", isDir: e.dataset.dir === "1" });
          else if (e.tagName === "BR") out.text += "\n";
          else walk(e);
        }
      });
    };
    walk(el);
    return out;
  };
  // contenteditable 非受控，打字不触发 React 重渲染——手动同步「是否有内容」以驱动发送钮/占位符；
  // 顺手把当前内容落册到本会话的草稿槽（切会话/关闭面板后靠它恢复）。
  const syncInput = (): void => {
    const { text, refs, terms } = readEditor();
    setHasInput(!!text.trim() || refs.length > 0 || terms.length > 0);
    const el = edRef.current;
    if (el) drafts.current.set(draftKey, el.innerHTML);
  };
  const makeChip = (path: string, name: string, remote: boolean, isDir: boolean): HTMLElement => {
    const span = document.createElement("span");
    span.className = "ai-ref";
    span.contentEditable = "false";
    span.dataset.ref = "1";
    span.dataset.path = path;
    span.dataset.remote = remote ? "1" : "0";
    span.dataset.dir = isDir ? "1" : "0";
    span.title = path;
    // 目录用文件夹线稿、文件用文档线稿（与 IconFolder / IconFile 同款）
    const glyph = isDir
      ? '<path d="M2.5 4.5h3.4l1.3 1.8H13.5v7.2H2.5z"/>'
      : '<path d="M4 2.8h5l3 3v7.4H4z"/><path d="M9 2.8V6h3"/>';
    span.innerHTML =
      `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${glyph}</svg>` +
      "<b></b>" +
      `<button type="button" class="ai-ref-x" aria-label="${tr("aw.refRemove")}">×</button>`;
    (span.querySelector("b") as HTMLElement).textContent = name;
    return span;
  };
  // 把 chip 插到当前光标处（光标不在编辑区内则追加到末尾并留空格便于续打）；同路径去重
  const insertChip = (path: string, name: string, remote: boolean, isDir: boolean): void => {
    const el = edRef.current;
    if (!el) return;
    const flag = remote ? "1" : "0";
    if (Array.from(el.querySelectorAll<HTMLElement>("[data-ref]")).some((c) => c.dataset.path === path && c.dataset.remote === flag)) return;
    el.focus();
    const chip = makeChip(path, name, remote, isDir);
    const sel = window.getSelection();
    if (sel && sel.rangeCount && el.contains(sel.anchorNode)) {
      const r = sel.getRangeAt(0);
      r.deleteContents();
      r.insertNode(chip);
      const after = document.createRange();
      after.setStartAfter(chip);
      after.collapse(true);
      sel.removeAllRanges();
      sel.addRange(after);
    } else {
      el.appendChild(chip);
      const tail = document.createTextNode(" ");
      el.appendChild(tail);
      const r = document.createRange();
      r.setStartAfter(tail);
      r.collapse(true);
      sel?.removeAllRanges();
      sel?.addRange(r);
    }
    syncInput();
  };

  // 终端选区 chip：与文件 chip 同挂 `data-ref`（readEditor 不读它的文本），另标 `data-term`
  // 以便发送时单独展开；选区原文 base64 存进 dataset，切会话/收面板随草稿 HTML 一起存活。
  const makeTermChip = (text: string): HTMLElement => {
    const span = document.createElement("span");
    span.className = "ai-ref";
    span.contentEditable = "false";
    span.dataset.ref = "1";
    span.dataset.term = "1";
    span.dataset.text = encTermText(text);
    const lines = text.split("\n").filter((l) => l.trim() !== "").length;
    span.title = `${text.slice(0, 400)}${text.length > 400 ? "…" : ""}`;
    span.innerHTML =
      '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M4.6 6.6 6.4 8l-1.8 1.4"/><path d="M8 9.6h3"/></svg>' +
      "<b></b>" +
      `<button type="button" class="ai-ref-x" aria-label="${tr("aw.refRemove")}">×</button>`;
    (span.querySelector("b") as HTMLElement).textContent = tr("tx.askAiChip").replace("{n}", String(lines));
    return span;
  };
  // 把终端选区 chip 插到编辑区末尾（与文件 chip 的追加支同构）；选区总是要包整段输出，
  // 不跟用户正在打的问句抢光标位置，固定追加最稳。同文本去重（与文件 chip 同路径去重同口径）：
  // 吃掉 StrictMode/多实例下 effect 重跑导致的重复插入。
  const insertTermChip = (text: string): void => {
    const el = edRef.current;
    if (!el) return;
    const enc = encTermText(text);
    if (Array.from(el.querySelectorAll<HTMLElement>("[data-term]")).some((c) => c.dataset.text === enc)) return;
    el.appendChild(makeTermChip(text));
    const tail = document.createTextNode(" ");
    el.appendChild(tail);
    const r = document.createRange();
    r.setStartAfter(tail);
    r.collapse(true);
    window.getSelection()?.removeAllRanges();
    window.getSelection()?.addRange(r);
    syncInput();
  };

  // 会话切换：先把旧会话的现行内容存档，再把目标会话自己的草稿放回编辑区（没有则清空）。
  // 必须排在下面「草稿预填」的 effect 之前：同一次切会话若还带外部预填，让预填后写、覆盖这里的恢复。
  useEffect(() => {
    if (!open) {
      // 面板关闭时编辑区已随 `return null` 卸载，下次打开必须重新按会话重放
      shownKey.current = null;
      return;
    }
    const el = edRef.current;
    if (!el || shownKey.current === draftKey) return;
    if (shownKey.current !== null) drafts.current.set(shownKey.current, el.innerHTML);
    shownKey.current = draftKey;
    el.innerHTML = drafts.current.get(draftKey) ?? "";
    // 图片跟着草稿一起换档：给 A 机贴的图不该出现在 B 会话的待发区（与草稿同口径隔离）
    setPending(sessionImages.get(draftKey) ?? []);
    setPendingFiles(sessionFiles.get(draftKey) ?? []);
    const { text, refs, terms } = readEditor();
    setHasInput(!!text.trim() || refs.length > 0 || terms.length > 0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, draftKey]);

  // 会话关掉后其草稿槽一并回收；空 key（无活动会话）不在册，避免关标签时被误存。
  // 保留 id（应用级问答）不是真实会话、永不在 tabs 里，同样豁免——否则每次开关标签
  // 都会把首页聊天还没发出去的草稿/图/文档误清。
  useEffect(() => {
    const alive = new Set(tabs.map((t) => t.id));
    for (const key of Array.from(drafts.current.keys())) {
      if (key && key !== APP_CHAT_SESSION && !alive.has(key)) drafts.current.delete(key);
    }
    for (const key of Array.from(sessionImages.keys())) {
      if (key && key !== APP_CHAT_SESSION && !alive.has(key)) sessionImages.delete(key);
    }
    for (const key of Array.from(sessionFiles.keys())) {
      if (key && key !== APP_CHAT_SESSION && !alive.has(key)) sessionFiles.delete(key);
    }
    // 目标会话已被关掉：chip 请求永远等不到落点，丢弃比落到别的会话安全
    const chipPending = aiStore.get().chipReq;
    if (chipPending && chipPending.sessionId && !alive.has(chipPending.sessionId)) aiChat.consumeChip();
    const termPending = aiStore.get().termReq;
    if (termPending && termPending.sessionId && !alive.has(termPending.sessionId)) aiChat.consumeTermSel();
  }, [tabs]);

  // 外部预填草稿（历史「修改并重发」等）：写入编辑区纯文本、光标置末，随即消费防重复
  useEffect(() => {
    if (!draft) return;
    const el = edRef.current;
    if (el) {
      el.textContent = draft.text;
      el.focus();
      const r = document.createRange();
      r.selectNodeContents(el);
      r.collapse(false);
      const sel = window.getSelection();
      sel?.removeAllRanges();
      sel?.addRange(r);
    }
    aiChat.consumeDraft();
    syncInput();
  }, [draft]);

  // 「发送到 AI 对话」：把请求的 chip 插入编辑区，随即消费。
  // 只认与当前会话相符的请求：文件栏在 A 机点的引用不能落进 B 机正在看的对话；
  // 不匹配就先留着，等用户切到 A（draftKey 变化会重跑本 effect）再插。
  useEffect(() => {
    if (!chipReq || chipReq.sessionId !== draftKey) return;
    insertChip(chipReq.path, chipReq.name, chipReq.remote, chipReq.isDir);
    aiChat.consumeChip();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [chipReq?.seq, draftKey]);

  // 终端右键「问 AI」：把选区做成 chip 插入编辑区，随即消费；会话门控与文件 chip 同口径。
  useEffect(() => {
    if (!termReq || termReq.sessionId !== draftKey) return;
    insertTermChip(termReq.text);
    aiChat.consumeTermSel();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [termReq?.seq, draftKey]);

  if (!open) return null;

  // AI 可用与否查能力表：**有命令执行通道（exec）、远端文件后端（fs）或终端（pty）之一就值得开**。
  // 原先写死 `(ssh || local)`，FTP/FTPS 被整个挡在外面——可后端四个文件类工具（list_dir/
  // read_file/search_files/transfer_file）走的 `files::backend()` 对 FTP 完全可用，挡掉的是真能用
  // 的能力（工具集现由后端 `ToolRegistry::for_caps` 按同一张表下发，两端不会错位）。
  // pty 档兼容 Telnet/串口/mosh：无 exec/fs 但聊天 + 终端快照分析可用（与 dockRegistry 的 AI 面板
  // 可见性、railTabsFor 的「chat 恒注册」同源，不能这里放行那里又藏栏）。
  // 本会话是否能开「Agent 档」：Agent = AI 把话写进终端且能兑现执行，判据统一走能力表
  // canAgentRail（有 PTY 且非聊天唯一形态）。FTP/FTPS 没有终端、本地 Windows（ConPTY 重绘冲突）、
  // Telnet/串口/mosh（无 exec、拿不到执行工具）都不注册 Agent——此时聊天页是**唯一形态**
  // （与 SessionRail 的隐藏页签同源）：否则落库的 rail="agent" 会让审批/澄清条按 agent 档不渲染。
  const chatRail = rail === "chat" || !canAgentRail(activeTab?.kind);
  // 应用级问答：保留 id 直接放行（后端钉死空能力集，只问答不执行）；
  // 真实会话仍按能力表 + 已连接门控。
  const aiSessionId = appChat
    ? APP_CHAT_SESSION
    : activeTab && hasAnyCap(activeTab.kind, ["exec", "fs", "pty"]) && activeTab.state === "connected"
      ? activeTab.id
      : null;
  const effective = providerId ?? defaultProviderId(providers);
  const noProvider = providers.length === 0;
  const canSend = !!aiSessionId && !sending && !noProvider;
  // 待发区非空也算「有内容」：纯贴一张图不发一句话是合法输入（丢个日志只问一句同理）
  const canSubmit = hasInput || pending.length > 0 || pendingFiles.length > 0;
  // 本对话正在生成：发送钮变停止（观感见 .ai-send[data-stop]）
  const isStop = sending;
  // 本会话有在途的「聊天轨」轮次：此时新建/切换本会话对话会把旧轮次的步骤与回复灌进新视图（串台）。
  // 跨会话切换已结构性免疫（各写各自的 chatViews），此守卫只防「本会话在途时新建/切换本会话」。
  const chatBusyHere = sending && tn?.track === "chat";
  // 最近会话页签：conversations 后端已按「最近更新在前」返回、按当前服务器作用域过滤，取前 5。
  const recentConvs = conversations.slice(0, 5);

  const submit = () => {
    if (!canSend || !canSubmit) return;
    const { text, refs, terms } = readEditor();
    // chip 展开成消息头部协议文本（AI 侧约定不变：📎 远程文件：/path），后端/历史视图零改动
    const head = refs
      .map((r) => {
        const label = r.isDir
          ? r.remote
            ? tr("fp.aiRefRemoteDir")
            : tr("fp.aiRefLocalDir")
          : r.remote
            ? tr("fp.aiRefRemote")
            : tr("fp.aiRefLocal");
        // 目录用 📁、文件用 📎：稳定文本标记，气泡重开后据此还原图标
        return `${r.isDir ? "📁" : "📎"} ${label}${r.path}\n`;
      })
      .join("");
    // 终端选区 chip → `🖥 ` 标记行 + ```text 围栏：围栏保持既有约定（不触发「插入终端」按钮、
    // 多行缩进不与问句混句）；标记行供气泡侧 splitUserTerm 还原成 chip，AI 仍看到全文。
    // terms 已由 readEditor 按 data-term 分流取出（base64），不再二次查 DOM。
    const termHead = terms
      .map((enc) => `🖥\n\`\`\`text\n${decTermText(enc)}\n\`\`\`\n`)
      .join("");
    const body = text.trim();
    // 预探失败的 chip 不参与「有没有内容」判定，也不下发（它本就被拒在读盘之前）。
    const sendFiles = pendingFiles.filter((f) => !f.error);
    if (!head && !termHead && !body && pending.length === 0 && sendFiles.length === 0) return;
    // 带图但本轮用的模型未确认支持视觉 → 前端就拦（后端还有一道）：覆盖「贴完图又切模型」的时序。
    // 不做任何降级（不转文字、不 OCR）：降级会把「模型能不能看图」这个事实悄悄改掉，答复质量不可预期。
    // 文档不受此闸：抽的是文字，任何文本模型都吃得上。
    if (pending.length && providers.find((p) => p.id === effective)?.supportsVision !== true) {
      toast.show(tr("aw.imgBlocked"));
      return;
    }
    void aiChat.send(
      aiSessionId as string,
      head + termHead + body,
      pending.length ? pending : undefined,
      sendFiles.length ? sendFiles : undefined,
    );
    const el = edRef.current;
    if (el) el.innerHTML = "";
    // 已发出就不该再挂在本会话的草稿槽里，否则切走再回来又看到刚发过的内容
    drafts.current.delete(draftKey);
    setHasInput(false);
    commitImages([]);
    commitFiles([]);
  };

  // 编辑区事件：× 委托移除 chip；Enter 发送 / Shift+Enter 换行 / Esc 交还焦点；粘贴转纯文本防注入
  const onEditorClick = (e: MouseEvent) => {
    const x = (e.target as HTMLElement).closest(".ai-ref-x");
    if (!x) return;
    x.closest(".ai-ref")?.remove();
    syncInput();
  };
  const onEditorKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    } else if (e.key === "Enter") {
      e.preventDefault();
      document.execCommand("insertLineBreak");
    } else if (e.key === "Escape") {
      // 终端内的就地 AI 输入浮层已撤除（AI 输入只此一处），Esc 负责把焦点还给终端
      e.preventDefault();
      focusTerm(talkSession);
    }
  };
  // —— 三入口（粘贴 / 拖拽 / 📎 选文件）汇到同一个 addImage：归一后进本会话的待发区 ——
  const rejectText = (r: ImageReject): string =>
    r === "tooMany" ? tr("aw.imgTooMany") : r === "tooBig" ? tr("aw.imgTooBig") : tr("aw.imgUnreadable");
  const addImage = async (file: File): Promise<void> => {
    const cur = sessionImages.get(draftKey) ?? [];
    let img: ChatImage | ImageReject;
    try {
      img = await normalizeImage(file, cur.length);
    } catch {
      toast.show(tr("aw.imgUnreadable"));
      return;
    }
    if (typeof img === "string") {
      toast.show(rejectText(img));
      return;
    }
    // 编码期间可能又贴进来一张（连续粘贴/拖多个）：入列前再按此刻的真实张数拦一次
    const now = sessionImages.get(draftKey) ?? [];
    if (now.length >= IMG_MAX_COUNT) {
      toast.show(tr("aw.imgTooMany"));
      return;
    }
    commitImages([...now, img]);
  };
  const removeImage = (i: number): void => {
    commitImages((sessionImages.get(draftKey) ?? []).filter((_, n) => n !== i));
  };
  const removeFile = (i: number): void => {
    commitFiles((sessionFiles.get(draftKey) ?? []).filter((_, n) => n !== i));
  };
  /** 预探成功的文档 → 待发 chip（去重 + 个数上限）。上限取此刻 Map 的真实值，不取快照。 */
  const applyProbe = (p: AttachmentProbe): void => {
    const now = (sessionFiles.get(draftKey) ?? []).filter((f) => f.path !== p.path);
    if (now.length >= FILE_MAX_COUNT) {
      toast.show(tr("aw.fileTooMany"));
      return;
    }
    commitFiles([
      ...now,
      { path: p.path, name: p.name, bytes: p.bytes, mime: p.mime, kind: p.kind },
    ]);
  };
  /** 预探失败 → 带 `error` 的 chip（失败内联在 chip 上，不只看一眼就消失的 toast）。 */
  const rejectPath = (path: string, e: unknown): void => {
    const cur = sessionFiles.get(draftKey) ?? [];
    const name = path.split(/[\\/]/).pop() || path;
    commitFiles([...cur, { path, name, error: errMsg(e) }]);
  };
  /**
   * 本地路径 → 待发区（📎 选择器入口，与图片那三个入口同构）。
   *
   * 先就地预探再定去向：后端只读头部就能判出 `kind`，图仍走原来的 `stageImage` + canvas 归一
   * （字节只有浏览器侧能读），文档则只存一个 chip——不为它读一次字节，因为抽取在后端按路径做。
   */
  const addFilePath = async (path: string): Promise<void> => {
    try {
      const p = await aiApi.probeAttachment(path);
      if (p.kind === "image") {
        void addImage(stagedToFile(await aiApi.stageImage(p.path)));
        return;
      }
      applyProbe(p);
    } catch (e) {
      rejectPath(path, e);
    }
  };
  /**
   * 系统级拖拽入口：只接文档。**图片一概不接** —— 它仍由 DOM `drop` 那条读字节的通路负责
   * （那里拿得到 `File`）；两条都收，同一张图会被贴两次。至于靠 `kind` 而不是后缀名筛：
   * 判型的唯一权威在后端，前端再维一份图片后缀名单就是第二套口径。
   */
  const addDroppedPath = async (path: string): Promise<void> => {
    try {
      const p = await aiApi.probeAttachment(path);
      if (p.kind !== "image") applyProbe(p);
    } catch (e) {
      rejectPath(path, e);
    }
  };
  /** 📎 选文件：不再只收图——WebView 读不了任意本地字节，图走后端读成 base64，文档只拿路径。 */
  const pickAttachment = async (): Promise<void> => {
    const path = await dialog.pickFile(tr("aw.attach"));
    if (!path) return;
    await addFilePath(path);
  };

  // 从资源管理器拖进窗口的文件：Tauri 的 `dragDropEnabled` 会吞掉页面内 HTML5 拖放（见
  // lib/tauri/file.ts 的 onOsFileDrop），故与文件面板同一套：拿原生路径 + 命中测试。
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    let un: UnlistenFn | undefined;
    onOsFileDrop((paths, at) => {
      if (!canSend || paths.length === 0) return;
      const hit = document.elementFromPoint(at.x, at.y)?.closest(".ai-box");
      if (!hit || !hit.contains(edRef.current)) return;
      paths.forEach((p) => void addDroppedPath(p));
    })
      .then((u) => {
        if (cancelled) u();
        else un = u;
      })
      // 纯浏览器预览（非 Tauri 环境）拿不到 webview，静默降级为只支持面板内拖拽
      .catch(() => {});
    return () => {
      cancelled = true;
      un?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, canSend, draftKey]);
  const onEditorPaste = (e: ClipboardEvent) => {
    e.preventDefault();
    // 先认剪贴板里的图（Win+Shift+S 截图 / 从图片应用 Ctrl+V）：有图就只收图；
    // 没有图片项才回落原有的「转纯文本插入」分支（保持既有行为，也防带样式的富文本注入）。
    const imgs: File[] = [];
    const items = e.clipboardData.items;
    for (let i = 0; i < items.length; i++) {
      if (!items[i].type.startsWith("image/")) continue;
      const f = items[i].getAsFile();
      if (f && f.size > 0) imgs.push(f);
    }
    if (imgs.length) {
      imgs.forEach((f) => void addImage(f));
      return;
    }
    document.execCommand("insertText", false, e.clipboardData.getData("text/plain"));
  };
  // 拖放：只对携文件且允许发送时抢下默认行为（文本选区拖动等一概不动，保持 contenteditable 原生表现）
  const onEditorDragOver = (e: DragEvent) => {
    if (canSend && e.dataTransfer.types.includes("Files")) e.preventDefault();
  };
  const onEditorDrop = (e: DragEvent) => {
    const files = Array.from(e.dataTransfer.files);
    if (!files.length) return;
    e.preventDefault();
    const imgs = files.filter((f) => f.type.startsWith("image/"));
    if (!imgs.length) {
      // 走到这里的是「页内 HTML5 拖放且不是图」：`File` 对象本身不携本机绝对路径，
      // 而文档抽取在后端按路径做——从资源管理器拖进来的已由 onOsFileDrop 那条接住。
      toast.show(tr("aw.imgUnreadable"));
      return;
    }
    imgs.forEach((f) => void addImage(f));
  };

  return (
    <>
      <aside className={embedded ? "ai-drawer embedded" : "ai-drawer"}>
        <div className="ai-head">
          <span className="ai-title">{tr("aw.title")}</span>
          {/* 模型管理入口已收进底部模型下拉（行悬停「编辑」+ 「添加模型」），与 Agent 页对齐，顶部不再设齿轮。 */}
          {!embedded && (
            <button className="icon-btn" title={tr("common.close")} onClick={onClose}>
              ×
            </button>
          )}
          {/* 最近会话页签：与「＋新建 / 时钟历史」同处头部一行，占左侧可滚区。
              按当前服务器作用域取最近 5 段；更早的从「时钟」历史列表找回。
              运行中（chatBusyHere）切换会串台，故拦截并提示。 */}
          {SHOW_RECENT_TABS && recentConvs.length > 0 && (
            <div className="ai-conv-tabs" role="tablist">
              {recentConvs.map((c) => {
                const on = c.id === activeId;
                const label = c.title || tr("aw.newChat");
                return (
                  <button
                    key={c.id}
                    type="button"
                    role="tab"
                    className="ai-conv-tab"
                    aria-selected={on}
                    data-on={on || undefined}
                    data-busy={chatBusyHere && !on ? true : undefined}
                    title={label}
                    onClick={() => {
                      if (on) return;
                      if (chatBusyHere) {
                        toast.show(tr("aw.busySwitch"));
                        return;
                      }
                      void aiChat.open(c.id, talkSession);
                    }}
                  >
                    {label}
                  </button>
                );
              })}
            </div>
          )}
          {/* 右上动作组：新建会话「＋」+ 历史，图标按钮并排（用量统计已提到全局顶栏） */}
          <div className="ai-head-actions">
            <button
              type="button"
              className="ai-hist-btn"
              data-busy={chatBusyHere || undefined}
              title={chatBusyHere ? tr("aw.busySwitch") : tr("aw.newChatTip")}
              aria-label={tr("aw.newChat")}
              onClick={() => {
                if (chatBusyHere) {
                  toast.show(tr("aw.busySwitch"));
                  return;
                }
                aiChat.start(talkSession);
              }}
            >
              <svg
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden="true"
              >
                <path d="M12 5v14" />
                <path d="M5 12h14" />
              </svg>
            </button>
            <button
              type="button"
              className="ai-hist-btn"
              data-on={histOpen}
              title={tr("aw.history")}
              aria-label={tr("aw.history")}
              onClick={() => setHistOpen((v) => !v)}
            >
              <svg
                width="16"
                height="16"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden="true"
              >
                <path d="M3 12a9 9 0 1 0 3-6.7L3 8" />
                <path d="M3 3v5h5" />
                <path d="M12 7v5l3 2" />
              </svg>
            </button>
          </div>
        </div>

        {histOpen && (
          <div className="ai-convs">
            {conversations.length === 0 && (
              <div className="ai-conv-empty">
                {scope === undefined ? tr("aw.noHistory") : tr("aw.noHistoryServer")}
              </div>
            )}
            {conversations.map((c) => (
              <div
                key={c.id}
                className="ai-conv"
                data-on={c.id === activeId}
                onClick={() => {
                  if (chatBusyHere) {
                    toast.show(tr("aw.busySwitch"));
                    return;
                  }
                  void aiChat.open(c.id, talkSession);
                  setHistOpen(false);
                }}
              >
                <span className="ai-conv-title">{c.title || tr("aw.newChat")}</span>
                <button
                  className="icon-btn danger ai-conv-x"
                  title={tr("aw.delChatTip")}
                  onClick={(e) => {
                    e.stopPropagation();
                    const rect = e.currentTarget.getBoundingClientRect();
                    setDelConv({ id: c.id, title: c.title || tr("aw.newChat"), x: rect.left, y: rect.bottom + 6 });
                  }}
                >
                  ×
                </button>
              </div>
            ))}
          </div>
        )}

        <div className="ai-timeline" ref={scroller}>
          {items.length === 0 && !sending && (
            <div className="ai-hint">
              {noProvider ? (
                tr("aw.hintNoProvider")
              ) : appChat ? (
                tr("aw.hintAppChat")
              ) : aiSessionId ? (
                // 提示语不分协议：各档能做/不能做什么已由系统提示词逐条告知，界面不再叠一层说明。
                tr("aw.hintIdle")
              ) : (
                tr("aw.hintNoSession")
              )}
            </div>
          )}
          {runs.map((run, i) =>
            run.kind === "single" ? (
              /* 应用级问答没有终端可插：传 null 隐去代码块「插入终端」钮（保留复制） */
              <TimelineItem key={run.id} item={run.item} sessionId={appChat ? null : aiSessionId} />
            ) : (
              <StepGroup
                key={run.id}
                steps={run.steps}
                live={sending && i === runs.length - 1}
                render={(it) => <TimelineItem item={it} />}
              />
            ),
          )}
          {/* 真错误：失败卡内联在最后一条用户气泡下方的「助手位」（取代底部「重试」条）。
              只认 chat 轨的挂起态：Agent 轨（控制台发起）的失败/续跑不在聊天窗里接管，防跨轨重试。 */}
          {!sending && resume && resume.track === "chat" && resume.kind === "retry" && (
            <div className="ai-msg ai">
              <div className="ai-fail">
                <div className="ai-fail-head">
                  <span className="ai-fail-ico"><IconClose /></span>
                  <span className="ai-fail-text">{tr("aw.failTitle")}</span>
                  <button type="button" className="ai-fail-retry" onClick={() => aiChat.retry(csid, "chat")}>
                    {tr("aw.retry")}
                  </button>
                </div>
                {sendError && <div className="ai-fail-detail">{sendError}</div>}
              </div>
            </div>
          )}
          {sending && (
            <div className="ai-thinking">
              <span className="ai-dots">
                <i />
                <i />
                <i />
              </span>
              {tr(`aw.${thinkingKeyForPhase(phase)}`).replace("{e}", formatElapsed(elapsed))}
            </div>
          )}
        </div>

        {/* 非聊天发送类错误（拉历史/切会话等）仍走底部横幅；聊天发送失败已内联成上方失败卡 */}
        {error && !(resume && resume.track === "chat" && resume.kind === "retry") && (
          <div className="ai-error">
            <span>{error}</span>
            <button className="icon-btn" title={tr("common.close")} onClick={() => aiChat.clearError()}>
              ×
            </button>
          </div>
        )}

        {/* 优雅收尾（达往返上限）→「继续」（底部细条）；真错误的「重试」已移到时间线内联失败卡。 */}
        {!sending && resume && resume.track === "chat" && resume.kind === "continue" && (
          <div className="ai-resume">
            <button
              type="button"
              className="ai-resume-btn"
              onClick={() => aiChat.continueTurn(csid, "chat")}
            >
              {tr("aw.continue")}
            </button>
          </div>
        )}

        {/* 聊天页只要有待裁决审批就挂确认条（输入框上方），不再看终端 AI 开关脸色。
            原因：新开终端默认 agent(开)，此时确认条原本只浮在终端里；一旦该终端不是当前可见标签
            （单屏切换后隐藏、或用户正盯着抽屉），就会出现「时间线显示待批准、却没有任何可点按钮」的死局
AgentPanel(agent 页) 本就无条件渲染可裁决卡，这里让 chat 页与之对齐。*/}
        {chatRail && talkSession && <ApprovalBar sessionId={talkSession} inline surface="chat" />}
        {chatRail && talkSession && <QuestionBar sessionId={talkSession} inline surface="chat" />}

        <div className="ai-input">
          {/* 输入盒：contenteditable 编辑区，文字与文件 chip 同行混排 */}
          <div
            className="ai-box"
            onClick={onEditorClick}
            onDragOver={onEditorDragOver}
            onDrop={onEditorDrop}
          >
            {/* 待发图缩略图条（编辑区上方一行）：× 移除与文件 chip 同一套交互，位置固定在文字之上。
                40px 方框看不清内容，点图开大图核对（发之前发现贴错图代价最小）；× 自己 stopPropagation。 */}
            {pending.length > 0 && (
              <div className="ai-imgs">
                {pending.map((img, i) => (
                  <span key={i} className="ai-img" title={img.name}>
                    <img
                      src={img.dataUrl}
                      alt={img.name}
                      onClick={() => img.dataUrl && showImage(img.dataUrl, img.name ?? "")}
                    />
                    <button
                      type="button"
                      className="ai-img-x"
                      aria-label={tr("aw.refRemove")}
                      title={tr("aw.refRemove")}
                      onClick={(e) => {
                        e.stopPropagation();
                        removeImage(i);
                      }}
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
            )}
            {/* 待发文档 chip 条（紧接图片条之下、编辑区之上）：类型徽标 + 名字 + 大小；
                预探失败的 chip 就地标错（不静默丢），hover title 给完整路径与失败原因。 */}
            {pendingFiles.length > 0 && (
              <div className="ai-files">
                {pendingFiles.map((f, i) => (
                  <span
                    key={`${f.path}-${i}`}
                    className={f.error ? "ai-file err" : "ai-file"}
                    title={f.error ? `${f.path}\n${f.error}` : f.path}
                  >
                    <span className="ai-file-kind">{f.error ? "!" : kindLabel(tr, f.kind)}</span>
                    <span className="ai-file-name">{f.name}</span>
                    {!f.error && <span className="ai-file-size">{formatBytes(f.bytes ?? 0)}</span>}
                    <button
                      type="button"
                      className="ai-file-x"
                      aria-label={tr("aw.refRemove")}
                      title={tr("aw.refRemove")}
                      onClick={(e) => {
                        e.stopPropagation();
                        removeFile(i);
                      }}
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
            )}
            <div
              ref={edRef}
              className="ai-editor"
              contentEditable={canSend}
              suppressContentEditableWarning
              role="textbox"
              aria-multiline="true"
              data-placeholder={
                noProvider ? tr("aw.phNoProvider") : aiSessionId ? tr("aw.phReady") : tr("aw.phNoSession")
              }
              onInput={syncInput}
              onKeyDown={onEditorKeyDown}
              onPaste={onEditorPaste}
            />
          </div>
          {/* 输入框下方工具栏：只读/读写滑动开关 + 模型小下拉在左，发送压右（新建/历史均在右上图标） */}
          <div className="ai-input-bar">
            {/* 聊天页签内的「只读 / 读写」能力开关（写回 chatMode，滑动开关复用终端开关样式）：
                只读=仅查询命令、写命令直接拒；读写=可执行写/高危命令，但每条都弹授权框（与 Agent 页签一致）。
                命令在本页签恒静默执行（后端按 rail 强制）。
                应用级问答不画：没有任何执行通路，两档都无从兑现，摆出来只是误导。 */}
            {!appChat && (
            <button
              type="button"
              className="term-ai-switch ai-mode-switch"
              role="switch"
              aria-checked={chatMode === "agent"}
              aria-label={chatMode === "agent" ? tr("aw.modeAgent") : tr("aw.modeChat")}
              title={chatMode === "agent" ? tr("aw.modeAgentTip") : tr("aw.modeChatTip")}
              onClick={() => void settingsActions.update({ chatMode: chatMode === "agent" ? "chat" : "agent" })}
            >
              <span className="term-ai-word" data-on={chatMode !== "agent"}>{tr("aw.modeChat")}</span>
              <span className="term-ai-knob" aria-hidden="true"><i /></span>
              <span className="term-ai-word" data-on={chatMode === "agent"}>{tr("aw.modeAgent")}</span>
            </button>
            )}
            {/* 模型下拉用公共组件 EditableSelect（行悬停「编辑」+ 底部「添加模型」），与 Agent 页一致；
                故下拉常开（即便未配模型也要能点开走「添加模型」加首个）。 */}
            <EditableSelect
              className="ai-model-pick"
              value={effective ?? ""}
              title={noProvider ? tr("aw.noProviderTip") : tr("aw.pickModelTip")}
              placeholder={tr("aw.noProvider")}
              options={providers.map((p) => ({ value: p.id, label: `${p.model} · ${p.name}` }))}
              onChange={(v) => aiChat.setProvider(talkSession, v || null)}
              addLabel={tr("aw.addModels")}
              editTitle={tr("common.edit")}
              onAdd={() => setPv({ provider: null })}
              onEdit={(id) => setPv({ provider: providers.find((p) => p.id === id) ?? null })}
            />
            {/* 上下文占用（常驻）：空心圆环进度，末轮实际发送量/窗口总量；hover 看百分比。
                与 Agent 页签底部工具栏共用 ContextRing，两页体验一致。 */}
            <ContextRing usage={lastUsage} />
            <span className="ai-tool-spacer" />
            {/* 📎 选图（三入口之一）：粘贴与拖拽不占按钮，选文件只能走原生对话框 */}
            <button
              type="button"
              className="icon-btn"
              disabled={!canSend || (pending.length >= IMG_MAX_COUNT && pendingFiles.length >= FILE_MAX_COUNT)}
              title={tr("aw.attach")}
              aria-label={tr("aw.attach")}
              onClick={() => void pickAttachment()}
            >
              <IconPaperclip />
            </button>
            <button
              type="button"
              className="ai-send"
              data-stop={isStop}
              onClick={isStop ? () => aiChat.cancel(csid, "chat") : submit}
              disabled={isStop ? false : !canSend || !canSubmit}
              title={isStop ? tr("aw.stopTip") : tr("aw.send")}
              aria-label={isStop ? tr("aw.stop") : tr("aw.send")}
            >
              {isStop ? (
                /* 停止：圆钮内的呼吸方块（方块占钮面约 1/3，太小显孱弱） */
                <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                  <rect x="4.5" y="4.5" width="15" height="15" rx="3.5" />
                </svg>
              ) : (
                /* 发送：向上箭头 */
                <svg
                  width="16"
                  height="16"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2.2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden="true"
                >
                  <path d="M12 19V5" />
                  <path d="M5 12l7-7 7 7" />
                </svg>
              )}
            </button>
          </div>
        </div>
      </aside>

      {pv && <ProviderFormDialog initial={pv.provider} onClose={() => setPv(null)} />}

      {delConv && (
        <Popconfirm
          x={delConv.x}
          y={delConv.y}
          title={tr("aw.delChatMsg").replace("{n}", delConv.title)}
          description={tr("aw.delChatWarn")}
          onConfirm={() => {
            const id = delConv.id;
            setDelConv(null);
            void aiChat.remove(id);
          }}
          onClose={() => setDelConv(null)}
        />
      )}
    </>
  );
}
