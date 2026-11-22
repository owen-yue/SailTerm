import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Terminal as XTerm } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { b64ToBytes, history, file as fileApi, local as localApi, sessions as sshApi, dialog, type UnlistenFn } from "../../lib/tauri";
import {
  isPathToken,
  lastToken,
  lineWithoutLastToken,
  matchNames,
  pathGhostSuffix,
  splitPathToken,
} from "../../lib/complete";
import { highlightColor, inkFor, resolveTheme, SEARCH_HIT, tintFor } from "../../lib/themes";
import { attachTermHighlight, type TermHighlight } from "../../lib/termHighlight";
import { copyText } from "../../lib/clipboard";
import { isShellLike, looksLikeQuestion, withTriggerHint } from "../../lib/agentMode";
import { canAgentRail, hasAnyCap, kindHasTerminal } from "../../lib/capabilities";
import { bindTerm, isBusy, isSending, lockInput, queueInput, setPromptReady, setReplayHandler, setTyping, unlockInput } from "../../lib/termAgent";
import { scanTriggers } from "../../lib/triggers";
import { useElapsedSec, formatElapsed, thinkingKeyForPhase } from "../../lib/useElapsed";
import { useI18n } from "../../lib/i18n";
import { aiChat, aiStore, appearanceStore, connectionStore, dockActions, rememberSize, sessionActions, sessionStore, settingsActions, settingsStore, terminalAiStore, terminalStore, toast, triggerStore, turnKey, uiPrefsStore, useStore } from "../../stores";
import AiToggle from "../ai/AiToggle";
import ApprovalBar from "../ai/ApprovalBar";
import QuestionBar from "../ai/QuestionBar";
import Button from "../common/Button";
import ContextMenu from "../common/ContextMenu";
import QuickBar from "./QuickBar";
import { sessionLabel } from "../../types";

interface Props {
  sessionId: string;
  /** 所在窗格是否显示：由隐藏切到显示时把键盘焦点收回终端。
   *  默认 true（独立弹出窗口等单实例场景）；分屏树外的隐藏 pane 传 false。 */
  active?: boolean;
  /**
   * 本终端是否自带顶部 AI 细条（`.term-strip`）。默认 true。
   * 分屏下由 pane-head 直接承载 AiToggle（那里不额外占行），此时传 false——
   * 细条的价值就是「不叠在画布上、永不遮挡输出」，能搭现成的行就不该再多占一行。
   */
  aiStrip?: boolean;
}

/** 命令行最大记录长度（超长多为粘贴脚本，不入历史）。 */
const MAX_CMD_LEN = 500;

/** ghost 补全：本地历史加载上限 / 触发补全的最小输入长度。 */
const GHOST_HIST_LIMIT = 300;
const GHOST_MIN_LEN = 2;
/** 历史候选弹层最多展示条数（按最近使用取前 12 条，避免列表过长）。 */
const HIST_CYCLE_MAX = 12;
/** 路径列目录防抖（毫秒）。 */
const PATH_DEBOUNCE_MS = 120;

/**
 * 终端「命令 vs 提问」分流策略：
 * - "explicit"（默认）：拿不准一律当 shell，只有明确提问信号才交给 AI——最防劫持真命令。
 * - "auto"：用 isShellLike 从文本猜意图，口语提问更顺手但有误判空间。
 * 想恢复旧行为把值改成 "auto" 即可（isShellLike 逻辑完整保留在 lib/agentMode.ts）。
 */
const QUESTION_ROUTING: "explicit" | "auto" = "explicit";

/** 框选问 AI 的选区字符上限：超了只取前段并提示，别把整段 scrollback 顶进输入框。 */
const MAX_ASK_SEL = 8000;

/** 历史命令去重（保留最近优先顺序，剔除空白项）。 */
function dedupeCmds(list: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of list) {
    const c = raw.trim();
    if (!c || seen.has(c)) continue;
    seen.add(c);
    out.push(c);
  }
  return out;
}

/** 前缀匹配（本地，不出网）：返回所有「以 prefix 开头且更长」的历史命令（最近优先）；
 *  首条作灰字预览，其余靠 Tab 轮换。 */
function historyMatches(prefix: string, cmds: string[]): string[] {
  return cmds
    .filter((c) => c.length > prefix.length && c.startsWith(prefix))
    .slice(0, HIST_CYCLE_MAX);
}

/**
 * 判断一段 onData 数据是否「纯粹」由终端→主机的查询应答序列构成：
 * - DSR/光标点报告 `ESC[<行>;<列>R`、`ESC[<列>R`
 * - DA/DECID 报告 `ESC[?...c` / `ESC[>...c`
 * - OSC 10/11 前景/背景色应答 `ESC]1x;rgb:...` （BEL 或 ST 结尾）
 * vim/less 等全屏程序启动/退出时会发这些查询，xterm.js 自动应答并从 onData 回写 PTY；
 * 若应答晚于程序退出才到达，就被 bash 当输入回显成 `2R11;rgb:…;276;0c` 这类乱码（
 * 切页签后窜出字符）。仅在主屏 + 确认停在干净提示符时当作陈旧应答丢弃（见 onData）。
 */
const TERMINAL_RESPONSE =
  /^(?:\x1b\[[0-9;?<>!]*[Rcn]|\x1b\][0-9]+;[^\x07\x1b]*(?:\x07|\x1b\\))+$/;
function isTerminalResponse(d: string): boolean {
  return d.length > 0 && d.length < 128 && TERMINAL_RESPONSE.test(d);
}

/**
 * xterm.js 终端：键盘 → 后端 PTY；后端输出 → 终端；容器尺寸变化 → fit 并同步远端窗口。
 * 实例只随 `sessionId` 创建一次，断线重连时**保留**已有内容。
 */
export default function Terminal({ sessionId, active = true, aiStrip = true }: Props) {
  const tr = useI18n();
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  /** 本会话的关键字着色引擎（随 xterm 实例同生共死，供规则/主题变化时重扫）。 */
  const highlightRef = useRef<TermHighlight | null>(null);
  const { tabs, busy } = useStore(sessionStore);
  const tab = tabs.find((t) => t.id === sessionId);
  // 通知来源标签：本终端所属会话的显示名（顶栏铃铛历史里区分是哪个连接报的）。
  // 在回调里现查 store，免闭包捕获旧 tabs。
  const notifySrc = () => {
    const { tabs: cur } = sessionStore.get();
    const t = cur.find((x) => x.id === sessionId);
    return t ? sessionLabel(t, tr) : undefined;
  };
  // 外观订阅：主题/字号/字体变化时热切换运行中的终端，不重建实例
  const { theme: themeId, fontSize, fontFamily, customTheme } = useStore(appearanceStore);
  // 终端行为偏好：滚动缓冲 / 光标闪烁 / 交互行为（设置中心）
  const { scrollback, cursorBlink, cursorStyle, keywordHighlight } = useStore(terminalStore);
  // 触发规则：关键字着色引擎读它（colorize 类），改一条即重扫已有行
  const { items: triggerItems } = useStore(triggerStore);
  // 前端偏好（外观分类）：行高 / 字距，热切换运行中终端
  const { lineHeight, letterSpacing } = useStore(uiPrefsStore);
  const { terminalAi: defaultAi } = useStore(settingsStore);
  // 终端 shell/agent 档：每条终端各自独立（本会话快照优先，未快照回落全局默认档）。
  // 不再因页签压住这把开关：Agent/聊天两轨早已独立（轨道在 send 那刻冻结、termPaint 按轨硬禁落笔），
  // 聊天页签下终端照样可开 agent——控制台敲的问句会经 dispatchQuestion 自动切回 Agent 页签展示思考。
  // 聊天唯一形态的会话（本地 Windows / Telnet 系，能力表 canAgentRail）同源恒 off。
  const { bySession } = useStore(terminalAiStore);
  const terminalAi = canAgentRail(tab?.kind) ? bySession[sessionId] ?? defaultAi : "off";
  // 键盘/快捷键 handler 只挂一次，用 ref 读最新的终端 AI 开关，避免闭包拿到旧值
  const aiOnRef = useRef(terminalAi);
  aiOnRef.current = terminalAi;
  const { approvals, turns } = useStore(aiStore);
  // 并发化：本会话是否有一轮在途（思考中），按 sessionId 取，不受其他标签影响。
  const tn = turns[turnKey(sessionId, "agent")];
  const sending = tn?.sending === true;
  // 思考条走字：本轮已耗时每秒更新（模型静默思考段也能看出没卡住）
  const elapsed = useElapsedSec(tn?.startedAt, sending);
  const pending = approvals.filter((a) => a.sessionId === sessionId && a.surface === "agent");

  // AI 回复窗口内暂存的用户输入（显示在思考条里，窗口结束后自动回放进终端）
  const [queued, setQueued] = useState("");
  // 终端回滚搜索（Ctrl+F）
  const [searchOpen, setSearchOpen] = useState(false);
  // 备用屏（vim/less/top 等全屏程序）：收起顶部的 AI 开关细条，把那一行还给程序画面
  const [altScreen, setAltScreen] = useState(false);
  // 光标行列位置（顶部细条左侧指示「行 N 字符 M」）：buf 坐标 0 基，展示时 +1。
  const [cursorPos, setCursorPos] = useState({ row: 0, col: 0 });
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; sel: string } | null>(null);
  // 右键菜单实际落点：初始 = 点击坐标，渲染后按视口边界收敛（靠边/靠下时防止菜单被截断）。
  // 单独存 state 而非直接改 style，避免菜单打开期间其它状态刷新把坐标重刷回原始值再次截断。
  const [searchQ, setSearchQ] = useState("");
  const searchRef = useRef<SearchAddon | null>(null);
  // 命中计数（当前第几个 / 共几个）：来自 addon 的 onDidChangeResults。搜索范围是整个缓冲区
  // （含已滚上去的回滚历史），屏上看到的标记数往往少于总数，不报数就说不清「到底命中几处」。
  const [searchHits, setSearchHits] = useState<{ idx: number; count: number } | null>(null);
  // 上一次真正搜过的查询词：区分「词变了（该从头来）」与「同词再搜（该往前进）」。
  const lastQueryRef = useRef("");
  // 上一条命令失败信息（OSC133 'D;<exit>' 非零退出码）：驱动终端右下角「让 AI 分析」浮标。
  const [lastErr, setLastErr] = useState<{ cmd: string; code: number } | null>(null);
  // 断连状态条可被用户关掉（不挡视线读回滚）；页签上的「重连」仍在，故关掉不丢入口。
  // 切会话或状态离开 disconnected（重连成功）时复位，下次断连会重新出现。
  const [discDismissed, setDiscDismissed] = useState(false);
  useEffect(() => {
    setDiscDismissed(false);
  }, [sessionId, tab?.state]);
  // OSC 133 提示符状态：seenB=shell 集成确实吐出 B 标记（精确门控可用）；
  // atMainPrompt=当前停在干净主提示符（PS1 输入起点），PS2 续行/命令执行中为 false。
  const promptRef = useRef({ seenB: false, atMainPrompt: false });

  // 取当前终端缓冲渲染后的纯文本（所见即所得）：xterm 已解析 ANSI，translateToString 出的是
  // 屏幕 + scrollback 的真实画面，其中也含终端 AI 画进缓冲的问句/回复/待授权提示（那些是前端
  // term.write 本地落的）。无内容返回 null。
  const bufferText = (): { text: string; count: number } | null => {
    const term = termRef.current;
    if (!term) return null;
    const buf = term.buffer.active;
    const lines: string[] = [];
    for (let i = 0; i < buf.length; i++) {
      const line = buf.getLine(i);
      lines.push(line ? line.translateToString(true) : "");
    }
    // 去掉尾部连续空行（视口未满时 getLine 会返回大量空行）
    while (lines.length > 0 && lines[lines.length - 1] === "") lines.pop();
    if (lines.length === 0) return null;
    return { text: lines.join("\r\n") + "\r\n", count: lines.length };
  };

  // 导出落盘（右键「导出输出…」→ 二级菜单选是否含 AI）：走系统保存框，不经剪贴板（大内容会卡）。
  // withAi=true 用前端缓冲（所见即所得，含终端里 AI 画的文字）；false 走后端 TerminalRing 原始流
  // ——AI 文字从不经过 PTY，故 snapshot 天然是「无 AI」的纯 shell 输出。
  const runExport = async (withAi: boolean) => {
    let text: string;
    let count: number;
    if (withAi) {
      const b = bufferText();
      if (!b) {
        toast.show(tr("tx.exportEmpty"), "warn", notifySrc());
        return;
      }
      text = b.text;
      count = b.count;
    } else {
      // session_snapshot 已在后端 strip_ansi + trim，前端拿到的就是干净文本；上限 64KB（后端夹取）
      const snap = (await sshApi.snapshot(sessionId, 64 * 1024)).trim();
      if (!snap) {
        toast.show(tr("tx.exportEmpty"), "warn", notifySrc());
        return;
      }
      text = snap.replace(/\r?\n/g, "\r\n") + "\r\n";
      count = snap.split(/\r?\n/).length;
    }
    const ts = new Date().toISOString().slice(0, 19).replace(/[:T]/g, "-");
    const raw = (tab?.label ?? "terminal").replace(/[^\w.-]+/g, "_").slice(0, 40);
    const path = await dialog.saveFile({
      title: tr("tx.exportTitle"),
      defaultName: `sailterm-${raw || "terminal"}-${ts}.log`,
      filterName: "Text",
      extensions: ["log", "txt"],
    });
    if (!path) return; // 用户取消保存框
    try {
      await localApi.saveText(path, text);
      toast.show(tr("tx.exportDone").replace("{n}", String(count)), "success", notifySrc());
    } catch (e) {
      toast.show(`${tr("tx.exportFail")}：${String((e as Error)?.message ?? e)}`, "error", notifySrc());
    }
  };

  // 清屏/重置只动本地 xterm 缓冲，远端 shell 不知情、不会自己重画提示符（擦完一片空白只剩光标）。
  // 补发一个 Ctrl+L（\x0c）让 readline 原地重绘提示符且不执行命令，避免“重置后屏是空的”的困惑。
  const nudgeRedraw = () => {
    void sshApi.write(sessionId, "\x0c");
  };

  // 菜单项跑完把键盘焦点送回终端：ContextMenu 是先 onClose 再 onClick，那个正被聚焦的菜单按钮
  // 随即卸载，焦点掉回 body——粘完不点一下鼠标就敲不进字。rAF 等卸载绘完再抢。
  // 只给「留在终端里继续干活」的项用：查找（输入框自己 autoFocus）、导出/问 AI（焦点该去侧栏）不裹。
  const refocusTerm = () => requestAnimationFrame(() => termRef.current?.focus());

  /* 搜索命中着色。两个硬约束决定了方案，都不是拍脑袋：
     ① 终端底色随主题变（可能是白、米、深蓝…），而且 decoration 的 backgroundColor 是**替换**格子
        底色、不是叠加，所以必须从主题自己的底色出发染色。
     ② WebGL 渲染器只认 #rrggbb（无 alpha），拿不到 VS Code 那种半透明叠底。
     前两版都在“不敢亮”上栽了：混到 90%（#e7c25e）把浅字压到 1.3:1 是糊，把对比度保底到原生 72%
     又使得两档只差 1.0–1.1（根本分不出哪个是当前）、dracula 上还直接反转。
     思路换成 VS Code 1.68 的「最小对比度」：块可以亮，但亮就去改**文字色**。
     xterm 在选中格子上会应用 theme.selectionForeground（CellColorResolver L124），
     而 addon 跳位必选当前命中——正好拿来当“亮块 + 墨字”的通道，不用接管 addon 内部。
     `matchBorder`/`activeMatchBorder` 依旧指望不上：它们靠 decoration.onRender 往 DOM 写 outline，
     而本项目默认 WebGL 渲染器下该回调不触发（同 ghost 叠层绕开 decoration 管线的原因）。 */

  // 单一取色入口：标记色与选区色必须共用 hard/ink，否则两处各自漂移。
  const searchTints = () => {
    const a = appearanceStore.get();
    const th = resolveTheme(a.theme, a.customTheme);
    const bg = th.background ?? "#0a0e13";
    const fg = th.foreground ?? "#d7e2ec";
    // 这里用基色 yellow 而不是 highlightColor 的 bright 版：亮色版本本身就是为「当文字色」设计的，
    // 彩度低、发白，拿来做底色会染成一片灰。
    const accent = th.yellow ?? highlightColor(th, "yellow");
    const hard = tintFor(bg, accent, fg, SEARCH_HIT.hard.sat, SEARCH_HIT.hard.lift, SEARCH_HIT.hard.keep);
    return {
      hard,
      ink: inkFor(hard, fg),
      soft: tintFor(bg, accent, fg, SEARCH_HIT.soft.sat, SEARCH_HIT.soft.lift, SEARCH_HIT.soft.keep),
      accent,
    };
  };

  const searchDecorations = () => {
    const { hard, soft, accent } = searchTints();
    return {
      matchBackground: soft,
      matchOverviewRuler: hard,
      activeMatchBackground: hard,
      activeMatchColorOverviewRuler: accent,
    };
  };

  /* 搜索条开着时把选区这条管线借给「当前命中」，两件事一起做：
     • selectionBackground = hard。addon-webgl 的 CellColorResolver 对普通格子（无 ANSI 底色）
       被选中时是直接拿 selectionBackground **替换**掉 decoration 底色（L118-120，不是 50% 混；
       50% 混只发生在格子自带底色/反显时），而 ThemeService 算的 selectionBackgroundOpaque
       在强制降透明（L100）之前就已 blend 完成，所以我们给的 hex 原样上屏。
     • selectionForeground = ink。强底上不把字改成墨色就是糊，这一步就是 VS Code 的
       「最小对比度」；inkFor 认为对比度已经够（亮色主题常见）则返回 undefined，不干扰原色。
     副作用：① 带 ANSI 底色的命中块会被对半混，颜色与其他命中不同（渲染器写死的，无法避免）；
     ② 搜索态下手动选择也走同一套亮块墨色。关掉搜索即还原。 */
  const withSearchTint = (th: ReturnType<typeof resolveTheme>): ReturnType<typeof resolveTheme> => {
    const { hard, ink } = searchTints();
    return { ...th, selectionBackground: hard, selectionInactiveBackground: hard, selectionForeground: ink };
  };

  // 搜索跳位期间置位：addon 靠 term.select() 实现定位，会触发 onSelectionChange，
  // 而「选中即复制」会把匹配词写进剪贴板——搜一次覆盖一次，用户刚复制的东西就没了。
  const searchBusyRef = useRef(false);

  /* 统一搜索入口（输入即搜 / Enter / Shift+Enter / 上一个 / 下一个 都走这里）。两条硬规则：
     1) 查询词一变就「从头再来」：addon 的 findNext 以当前选区为起点，而搜索框每敲一个字符都搜
        一次，不清选区的话命中点会跟着上一次结果往前漂（实测同词连搜在 0→1→0→1 来回绕），
        看起来就既不是「只标一个」也不是「全标了」。清掉选区后起点回到缓冲区开头，恒定命中第一个。
     2) 空串要真清空，不能直接 return：旧写法删光关键字后上一轮的标记还糊在屏上。 */
  const runSearch = (q: string, back = false): void => {
    const term = termRef.current;
    if (q === "") {
      lastQueryRef.current = "";
      searchRef.current?.clearDecorations();
      term?.clearSelection();
      setSearchHits(null);
      return;
    }
    if (q !== lastQueryRef.current) {
      lastQueryRef.current = q;
      searchRef.current?.clearDecorations();
      term?.clearSelection();
    }
    const opts = { decorations: searchDecorations() };
    searchBusyRef.current = true;
    try {
      if (back) searchRef.current?.findPrevious(q, opts);
      else searchRef.current?.findNext(q, opts);
    } finally {
      searchBusyRef.current = false;
    }
  };

  // 收起搜索条：标记、计数、上次的词一并清干净，下次 Ctrl+F 是空白起点。
  const closeSearch = (): void => {
    runSearch("");
    setSearchOpen(false);
    setSearchQ("");
    termRef.current?.focus();
  };

  // 点「上一个/下一个」后把焦点送回搜索框：否则焦点落在按钮上，想再按 Enter 得先点一下输入框。
  const searchInputRef = useRef<HTMLInputElement | null>(null);
  const keepSearchFocus = () => requestAnimationFrame(() => searchInputRef.current?.focus());

  // AI 输入只保留 AI 面板「聊天」子页一处：同一个问题不该有两个输入口，终端下面再叠一个聊天框属多余
  //Ctrl+Space 不再进入「就地 AI 模式」，只负责把焦点交给 AI 面板输入框。
  const focusChatInput = () => {
    // 切到聊天子页（乐观写 settingsStore，同帧生效）+ 展开 AI 面板所在停靠区（右栏或下栏）
    void settingsActions.update({ rail: "chat" });
    dockActions.reveal("ai");
    // 展开是异步渲染：等一帧再抢焦点，否则此刻输入区还没挂上
    requestAnimationFrame(() => {
      // 输入区早已是 `div.ai-editor[contenteditable]`（不是 textarea）：选择器写错时此处会
      // 静默回落回终端焦点，预填好的草稿就停在屏上但光标不在，补话得先手动点一下。
      const el = document.querySelector<HTMLDivElement>(".ai-drawer .ai-editor");
      if (el) {
        el.focus();
        // 光标置末：预填文本后面要直接接用户自己那句问话
        const r = document.createRange();
        r.selectNodeContents(el);
        r.collapse(false);
        const sel = window.getSelection();
        sel?.removeAllRanges();
        sel?.addRange(r);
      } else {
        termRef.current?.focus();
      }
    });
  };
  /** 框选问 AI：把选区做成聊天输入框里的「终端选区」chip（**不直接发送**）。
   *  与「报错浮标一键诊断」刻意区分：框选是开放式提问，用户得能先补一句真正想问的话。
   *  选区不再平铺进正文——长输出会把输入框刷屏，chip 只占一小格，发送时才展开成 ```text 围栏。 */
  const askAiSel = (raw: string) => {
    const clipped = raw.length > MAX_ASK_SEL;
    const body = clipped ? raw.slice(0, MAX_ASK_SEL) : raw;
    aiChat.addTermSel({ sessionId, text: body });
    focusChatInput();
    if (clipped) toast.show(tr("tx.askAiTruncated"), "info", notifySrc());
  };
  // 精确门控：shell 集成可用（见过 B）时要求停在干净主提示符；否则退回「非备用屏 + 空行」保守判定
  const atCleanPrompt = () => !promptRef.current.seenB || promptRef.current.atMainPrompt;

  // 报错一键诊断：把失败命令 + 终端最近输出尾部拼成问句，展开右栏 AI 并直接发送。
  // 只在终端 AI 开关开着时才可能触发（浮标本身被 terminalAi==="on" 门控，关上时终端不留 AI 浮层）。
  // 后端 Agent 模式本可用 get_terminal_snapshot 自取输出，但问答模式没有工具，故这里显式附上尾部，两档都能拿到上下文。
  const diagnoseErr = () => {
    const term = termRef.current;
    if (!term || !lastErr) return;
    const buf = term.buffer.active;
    const lines: string[] = [];
    for (let i = buf.length - 1; i >= 0 && lines.length < 40; i--) {
      const raw = buf.getLine(i);
      const s = raw ? raw.translateToString(true) : "";
      if (!s.trim() && lines.length === 0) continue; // 先跳过尾部连续空行
      lines.unshift(s);
    }
    const tail = lines.join("\n").slice(-4000); // 有界：最多 40 行 / 4000 字符，别把整段 scrollback 喂进去
    const q =
      tr("tx.diagnosePrompt")
        .replace("{cmd}", lastErr.cmd || "-")
        .replace("{code}", String(lastErr.code)) +
      "\n\n" +
      tail;
    void settingsActions.update({ rail: "chat" });
    dockActions.reveal("ai");
    void aiChat.send(sessionId, q);
    setLastErr(null);
  };

  useEffect(() => {
    const el = hostRef.current;
    if (!el) return;

    // 创建时读一次当前偏好（外观 + 终端行为）：不放进依赖数组，改设置时靠下面的热切换
    // effect 更新，绝不重建 xterm 实例（重建会丢终端内容与滚动历史）。
    const appearance = appearanceStore.get();
    const termPrefs = terminalStore.get();
    const prefs = uiPrefsStore.get();
    const term = new XTerm({
      cursorBlink: termPrefs.cursorBlink,
      cursorStyle: termPrefs.cursorStyle,
      fontSize: appearance.fontSize,
      fontFamily: appearance.fontFamily,
      lineHeight: prefs.lineHeight,
      letterSpacing: prefs.letterSpacing,
      theme: resolveTheme(appearance.theme, appearance.customTheme),
      scrollback: termPrefs.scrollback,
      // 关键字着色（termHighlight.paint → registerDecoration / translateToString 的列数组出参）
      // 依赖 xterm 的 proposed API；不开则命中着色规则时抛「must set allowProposedApi」并整树卸载（黑屏）。
      allowProposedApi: true,
      // LF 也回到行首（部分远端只发 \n）；CR/LF 原样字节仍优先
      convertEol: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    const search = new SearchAddon();
    term.loadAddon(search);
    searchRef.current = search;
    term.loadAddon(new WebLinksAddon());
    term.open(el);
    fit.fit();
    // GPU 渲染器（WebGL2）：大输出/大回滚滚动性能与 CPU 占用显著优于默认 DOM 渲染。
    // 加载失败（无 WebGL2 上下文/驱动限制）或运行期上下文丢失（远程桌面、休眠唤醒、
    // GPU 切换）时，dispose 回落到 xterm 内置 DOM 渲染器，终端仍可用不白屏。
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      /* WebGL 不可用：静默保持默认 DOM 渲染 */
    }
    term.focus();
    termRef.current = term;

    // 选中即复制（设置中心）：开关读最新值，无需重挂监听
    term.onSelectionChange(() => {
      if (searchBusyRef.current) return; // 搜索跳位不是用户选择，不该动剪贴板
      if (!terminalStore.get().copyOnSelect || !term.hasSelection()) return;
      const sel = term.getSelection();
      if (sel) void copyText(sel);
    });

    // 响铃（xterm 6 无 bellStyle 选项，改用 onBell 事件）：visual=容器闪烁，sound=系统提示音
    let bellCtx: AudioContext | null = null;
    term.onBell(() => {
      // per-session 覆盖：本会话绑定的档案若设了响铃样式优先用它，否则跟随全局终端偏好。
      const pid = sessionStore.get().tabs.find((t) => t.id === sessionId)?.profileId;
      const prof = pid ? connectionStore.get().profiles.find((p) => p.id === pid) : undefined;
      const s = prof?.bellStyle ?? terminalStore.get().bellStyle;
      if (s === "visual") {
        el.classList.remove("bell-flash");
        void el.offsetWidth; // 强制重排以重放动画
        el.classList.add("bell-flash");
        window.setTimeout(() => el.classList.remove("bell-flash"), 400);
      } else if (s === "sound") {
        try {
          bellCtx ??= new AudioContext();
          if (bellCtx.state === "suspended") void bellCtx.resume();
          const osc = bellCtx.createOscillator();
          const gain = bellCtx.createGain();
          osc.type = "sine";
          osc.frequency.value = 880;
          gain.gain.setValueAtTime(0.0001, bellCtx.currentTime);
          gain.gain.exponentialRampToValueAtTime(0.3, bellCtx.currentTime + 0.01);
          gain.gain.exponentialRampToValueAtTime(0.0001, bellCtx.currentTime + 0.25);
          osc.connect(gain).connect(bellCtx.destination);
          osc.start();
          osc.stop(bellCtx.currentTime + 0.3);
        } catch {
          // 音频受限时静默降级
        }
      }
    });

    // 用真实尺寸校正远端 PTY（建连时用的是默认值）
    rememberSize(sessionId, term.cols, term.rows);
    void sshApi.resize(sessionId, term.cols, term.rows).catch(() => {});

    // 累积用户输入行，回车时上报命令历史（全屏程序如 vim/top 中不记）
    let line = "";
    // 最近一次提交的命令：OSC133 报非零退出码时带进「让 AI 分析」的诊断上下文
    let lastCmd = "";

    // ── Next ghost 补全：本地历史前缀匹配 → 光标后灰字提示，全程不出网。
    // 数据源仅「本会话所属连接」的历史 + 本会话敲过的命令；decoration 只渲染提示，绝不写入 PTY 缓冲。
    //  路径补全：历史无匹配且末 token 像路径时，列目录做名称前缀匹配（仍不出网）。
    const profileId = sessionStore.get().tabs.find((t) => t.id === sessionId)?.profileId ?? null;
    const kind = sessionStore.get().tabs.find((t) => t.id === sessionId)?.kind ?? "ssh";
    let historyCmds: string[] = [];
    let ghostEl: HTMLDivElement | null = null; // ghost 灰字 DOM 叠层（自绘，绕开 xterm decoration/WebGL 管线）
    let ghostText = ""; // 当前 ghost 后缀（空=无提示）
    let ghostFull = ""; // 接受后的完整命令
    let ghostX = -1; // ghost 起始列（=创建时光标列），变化即重建
    // 历史候选（最近优先）：≥2 条时在光标下方弹候选列表供 ↑↓/鼠标选择。
    let histCands: string[] = [];
    // 历史候选弹层 DOM（自绘叠层，与灰字 ghost 分离；开 pointer-events 支持鼠标点击）
    let histPopupEl: HTMLDivElement | null = null;
    let histPopupCands: string[] = [];
    let histPopupBase = ""; // 弹出时的输入前缀（接受时据此算要补的后缀）
    let histPopupSel = -1; // 当前高亮下标（-1=未选中：弹层默认不高亮任何条，避免“看着绿却未选”）
    let histNavigated = false; // 本次弹层内用户是否已用 ↑↓ 主动移动高亮：决定回车是「采纳高亮」还是「发送原文」
    // 路径补全状态
    let pathMatches: { name: string; isDir: boolean }[] = [];
    let pathPrefix = "";
    let pathBase = ""; // 去掉末 token 后的 line 前缀
    let pathDir = "";
    let pathTimer: ReturnType<typeof setTimeout> | null = null;
    const pathCache = new Map<string, string[]>(); // dir → 名字列表（会话内短缓存）

    // ghost 历史数据源：固定「本会话所属连接」（不跨主机串台），设置 UI 不再暴露此选项；
    // uiPrefs.completionHistoryScope 字段保留，恢复全局选项时改回读它即可。
    // 总开关（uiPrefs.completionEnabled）默认关：关时不拉历史、不渲染 ghost，键位不受拦截。
    const completionOn = uiPrefsStore.get().completionEnabled;
    if (completionOn) {
      history
        .list(profileId, GHOST_HIST_LIMIT)
        .then((rows) => {
          historyCmds = dedupeCmds(rows.map((r) => r.command));
        })
        .catch(() => {});
    }

    const clearGhost = () => {
      ghostEl?.remove();
      ghostEl = null;
      ghostText = "";
      ghostFull = "";
      ghostX = -1;
    };
    /** 取（必要时创建）ghost 叠层元素与格子尺寸。自绘 DOM 叠层：用终端自身格子尺寸把灰字
     *  定位到光标后。不再走 term.registerDecoration/onRender——实测在 WebGL 渲染器下该管线
     *  不回调（灰字画不出）。 */
    const getGhostEl = (): { el: HTMLDivElement; cw: number; ch: number } | null => {
      const screen = term.element?.querySelector(".xterm-screen") as HTMLElement | null;
      if (!screen) return null;
      const cw = screen.clientWidth / term.cols;
      const ch = screen.clientHeight / term.rows;
      if (!ghostEl) {
        const d = document.createElement("div");
        d.className = "term-ghost term-ghost-overlay";
        d.style.cssText =
          "position:absolute;white-space:pre;pointer-events:none;z-index:5;color:#9aa7b8;";
        d.style.fontFamily = term.options.fontFamily ?? "monospace";
        d.style.fontSize = `${term.options.fontSize ?? 14}px`;
        screen.appendChild(d);
        ghostEl = d;
      }
      return { el: ghostEl, cw, ch };
    };
    // ── 历史候选弹层：≥2 条时在光标下方列出。未主动导航时回车=发送已输入命令；
    // 按过 ↑↓ 后回车=采纳高亮项（只填入不执行）；鼠标点击直接采纳。Esc 关闭。
    // 自绘 DOM 叠层（同灰字 ghost，绕开 WebGL decoration 不回调的坑），开 pointer-events 供点击。
    const histPopupCell = (): { screen: HTMLElement; cw: number; ch: number } | null => {
      const screen = term.element?.querySelector(".xterm-screen") as HTMLElement | null;
      if (!screen) return null;
      return { screen, cw: screen.clientWidth / term.cols, ch: screen.clientHeight / term.rows };
    };
    const renderHistPopup = () => {
      if (!histPopupEl) return;
      histPopupEl.textContent = "";
      histPopupCands.forEach((c, i) => {
        const row = document.createElement("div");
        row.className = i === histPopupSel ? "term-hist-item term-hist-sel" : "term-hist-item";
        row.textContent = c;
        // 鼠标悬停高亮该行（与 ↑↓ 共用同一条高亮，杜绝两行同时着色）；悬停即视为「选中」，
        // 使绿色恒等于「回车/点击会作用于这条」。移出列表由容器 mouseleave 复位为无高亮。
        row.addEventListener("mouseenter", () => {
          if (histPopupSel !== i) {
            histPopupSel = i;
            histNavigated = true;
            renderHistPopup();
          }
        });
        // mousedown + preventDefault：抢在 xterm 的鼠标处理前响应，避免点击被当选择/粘贴
        row.addEventListener("mousedown", (ev) => {
          ev.preventDefault();
          ev.stopPropagation();
          acceptHist(i);
        });
        histPopupEl!.appendChild(row);
      });
    };
    const showHistPopup = (cands: string[]) => {
      const cell = histPopupCell();
      const buf = term.buffer.active;
      if (!cell || cands.length < 2) {
        hideHistPopup();
        return;
      }
      histPopupCands = cands;
      histPopupBase = line;
      histPopupSel = -1; // 默认不预选：只有悬停/↑↓ 才出现高亮
      histNavigated = false; // 因输入而弹层/刷新：回到「未导航」，回车默认发送原文
      if (!histPopupEl) {
        const d = document.createElement("div");
        d.className = "term-hist-popup";
        // 鼠标移出列表：取消悬停高亮，回到「未选中」——避免「看着绿但其实没选」的歧义。
        d.addEventListener("mouseleave", () => {
          if (histNavigated && histPopupSel >= 0) {
            histPopupSel = -1;
            histNavigated = false;
            renderHistPopup();
          }
        });
        cell.screen.appendChild(d);
        histPopupEl = d;
      }
      renderHistPopup();
      // 定位到光标下一行、左对齐光标列；横向夹进视口，纵向靠底则翻到光标上方
      const popW = histPopupEl.offsetWidth;
      const popH = histPopupEl.offsetHeight;
      const left = Math.max(0, Math.min(buf.cursorX * cell.cw, cell.screen.clientWidth - popW));
      const below = (buf.cursorY + 1) * cell.ch;
      const top = below + popH > cell.screen.clientHeight ? Math.max(0, buf.cursorY * cell.ch - popH) : below;
      histPopupEl.style.left = `${Math.round(left)}px`;
      histPopupEl.style.top = `${Math.round(top)}px`;
    };
    const hideHistPopup = () => {
      histPopupEl?.remove();
      histPopupEl = null;
      histPopupCands = [];
      histPopupSel = -1;
      histNavigated = false;
    };
    const moveHistSel = (delta: number) => {
      const n = histPopupCands.length;
      if (n === 0) return;
      // 从「未选中(-1)」起：↓ 进第一条、↑ 进最后一条；已选中则环形移动。
      histPopupSel = histPopupSel < 0 ? (delta > 0 ? 0 : n - 1) : (histPopupSel + delta + n) % n;
      histNavigated = true; // 键盘主动导航：此后回车 = 采纳高亮项
      renderHistPopup();
    };
    /** 接受第 idx 条候选：只把「相对当前输入多出的后缀」写进 PTY（shell 回显补全），
     *  绝不追加回车——执行与否交给用户（安全红线，与群发「只插入」同语义）。 */
    const acceptHist = (idx: number) => {
      const cand = histPopupCands[idx];
      if (cand == null) {
        hideHistPopup();
        return;
      }
      const rest = cand.slice(histPopupBase.length);
      if (rest) void sshApi.write(sessionId, rest).catch(() => {});
      line = cand;
      hideHistPopup();
      term.focus();
    };
    // 接受：把后缀写入 PTY（shell 回显后光标自然移到行尾），本地 line 同步为完整命令
    const acceptGhost = () => {
      const suffix = ghostText;
      if (!suffix) return;
      void sshApi.write(sessionId, suffix).catch(() => {});
      line = ghostFull;
      clearGhost();
    };
    /** 路径候选加载（防抖 + 会话内缓存）；就绪后重算 ghost。 */
    const schedulePathLookup = (dir: string) => {
      if (pathTimer) clearTimeout(pathTimer);
      pathTimer = setTimeout(() => {
        pathTimer = null;
        const cached = pathCache.get(dir);
        const apply = (names: string[]) => {
          pathCache.set(dir, names);
          const tok = lastToken(line);
          if (!tok || !isPathToken(tok)) return;
          const { prefix, dir: d } = splitPathToken(tok);
          if (d !== dir) return; // 输入已变
          pathPrefix = prefix;
          pathBase = lineWithoutLastToken(line);
          pathDir = dir;
          pathMatches = matchNames(
            prefix,
            names.map((n) => ({ name: n, isDir: n.endsWith("/") })),
          ).map((m) => ({ name: m.name.replace(/\/$/, ""), isDir: m.isDir }));
          refreshGhost();
        };
        if (cached) {
          apply(cached);
          return;
        }
        const loader =
          kind === "local"
            ? localApi.list(dir.replace(/\/$/, "") || null)
            : fileApi.list(sessionId, dir.replace(/\/$/, "") || ".");
        loader
          .then((listing) => apply(listing.entries.map((e) => e.name + (e.isDir ? "/" : ""))))
          .catch(() => apply([]));
      }, PATH_DEBOUNCE_MS);
    };
    // 依当前 line 重算 ghost：普通屏 + 输入≥2 字 + 停在「可输入的提示符行」+ 整段能落在本行内才显示。
    // 提示符判定做双保险：shell 集成可靠时优先用精确的 atMainPrompt（atCleanPrompt）；但远端标记
    // 并不可靠——有的只发 A 不发 B、或换行后不再补 B，会把 atMainPrompt 永久卡在 false，导致 ghost
    // 再也不出现（AI 触发因有保守回退不受影响，故只表现为「补全没了」）。故再兜一条启发式：
    // 光标停在视口最后一行 = 正处在实时输入行。命令执行中因回车已清空 line（<2 字）不会误提示，
    // 全屏程序（vim/less/top）由 normal 屏判定排除。
    const refreshGhost = () => {
      if (!completionOn) {
        clearGhost();
        hideHistPopup();
        return;
      }
      const buf = term.buffer.active;
      const atPrompt = atCleanPrompt() || buf.cursorY === term.rows - 1;
      if (buf.type !== "normal" || line.length < GHOST_MIN_LEN || !atPrompt) {
        clearGhost();
        hideHistPopup();
        return;
      }
      // 历史候选：≥2 条走光标下方弹层（↑↓/Enter/鼠标选择，只填入不执行）；单条不提示。
      histCands = historyMatches(line, historyCmds);
      // 路径补全：末 token 像路径时接管，仍用行内灰字 + Tab 轮换（与历史互斥，本次不改）。
      let suffix: string | null = null;
      const tok = lastToken(line);
      if (isPathToken(tok) && tok.includes("/")) {
        const { dir, prefix } = splitPathToken(tok);
        if (dir !== pathDir || prefix !== pathPrefix || pathMatches.length === 0) {
          // 缓存命中则同步算，否则异步拉
          const cached = pathCache.get(dir);
          if (cached) {
            pathPrefix = prefix;
            pathBase = lineWithoutLastToken(line);
            pathDir = dir;
            pathMatches = matchNames(
              prefix,
              cached.map((n) => ({ name: n, isDir: n.endsWith("/") })),
            ).map((m) => ({ name: m.name.replace(/\/$/, ""), isDir: m.isDir }));
            pathCycle = -1;
            suffix = pathGhostSuffix(prefix, pathMatches) || null;
          } else {
            clearGhost();
            hideHistPopup();
            schedulePathLookup(dir);
            return;
          }
        } else {
          suffix = pathGhostSuffix(prefix, pathMatches) || null;
        }
      }
      if (suffix) {
        // 路径灰字生效：历史弹层让位
        hideHistPopup();
        const x = buf.cursorX;
        if (x + suffix.length > term.cols) {
          clearGhost();
          return;
        }
        if (ghostText === suffix && ghostX === x && ghostEl) return; // 未变，免重建闪烁
        ghostText = suffix;
        ghostFull = line + suffix;
        ghostX = x;
        const g = getGhostEl();
        if (!g) {
          clearGhost();
          return;
        }
        g.el.textContent = suffix;
        g.el.style.left = `${Math.round(x * g.cw)}px`;
        g.el.style.top = `${Math.round(buf.cursorY * g.ch)}px`;
        g.el.style.width = `${Math.round(suffix.length * g.cw)}px`;
        g.el.style.height = `${Math.round(g.ch)}px`;
        g.el.style.lineHeight = `${Math.round(g.ch)}px`;
        return;
      }
      // 无路径补全：清掉灰字，按历史候选数决定弹层
      clearGhost();
      if (histCands.length >= 2) showHistPopup(histCands);
      else hideHistPopup();
    };

    /** 路径多候选 Tab 轮换：擦掉末 token，写入第 `idx` 个候选。 */
    const cyclePathMatch = (idx: number) => {
      if (pathMatches.length === 0) return;
      const m = pathMatches[idx % pathMatches.length];
      const tok = lastToken(line);
      const replacement = pathDir + m.name + (m.isDir ? "/" : "");
      if (tok.length > 0) void sshApi.write(sessionId, "\x7f".repeat(tok.length)).catch(() => {});
      void sshApi.write(sessionId, replacement).catch(() => {});
      line = pathBase + replacement;
      clearGhost();
    };

    // 路径轮换游标：-1 = 尚未轮换过（首次 Tab 落到 0）
    let pathCycle = -1;

    /** 行跟踪变化时同步「用户是否正在输入」给 termAgent（AI 回复补回车前要避让）。 */
    const syncTyping = () => setTyping(sessionId, line.trim().length > 0);
    const track = (d: string) => {
      if (term.buffer.active.type !== "normal") {
        line = "";
        clearGhost();
        syncTyping();
        return;
      }
      if (d === "\r") {
        const cmd = line.trim();
        line = "";
        clearGhost();
        promptRef.current.atMainPrompt = false; // 回车即离开输入起点，随后 C/D 标记再校正
        if (cmd) {
          lastCmd = cmd; // 记住这条，供随后 'D;<非零>' 退出码组装诊断上下文
          // 本会话敲过的命令即时入表（最近优先、去重），下一条就能补全
          historyCmds = [cmd, ...historyCmds.filter((c) => c !== cmd)].slice(0, GHOST_HIST_LIMIT);
          void history.record(sessionId, cmd).catch(() => {});
        }
        syncTyping();
        return;
      }
      if (d === "\u007f") {
        line = line.slice(0, -1); // Backspace
        pathMatches = []; // 前缀变了：丢弃旧候选
        syncTyping();
        return;
      }
      if (d === "\u0003" || d === "\u001b") {
        line = ""; // Ctrl+C / Esc
        clearGhost();
        syncTyping();
        return;
      }
      if (line.length < MAX_CMD_LEN && /^[\x20-\x7e\u0080-\uffff]+$/.test(d)) {
        line += d;
        syncTyping();
      }
    };

    // AI 触发门控：仅 Ctrl+Space 双向切换唤起 AI（# / @ 首字符入口均已移除）。
    // 备用屏（vim/less/top）里这些键照常送 PTY，绝不拦截。
    // OSC133 精确判定「全新主提示符 PS1」留 c9-2，此处先用「非备用屏 + 空行」保守回退。
    /** 提交 AI 问句：先把主画布切回 Agent 页签（思考链与工具步骤只在那边渲）→
     * 锁输入、Ctrl+U 整行清除 shell 回显 → 光标静止后在当前光标画绿色问句 →
     * 补一个空回车让 shell 重打提示符（恢复屏幕与 PSReadLine 认知同步）→ 发送。
     * 清除→画问句的短窗口锁输入：期间敲的字排队，画完后回放，不与绿色问句重叠。
     * 注：曾试过「先盖画绿色再发 Ctrl+U」压掉擦除→重画的闪窗，已回退：擦除是 shell 侧
     * 异步行为，无法与本地 term.write 原子成对——预画的绿色会被擦除/重绘连字抹掉或
     * 与 shell 回显叠成双份；且「光标在绿色之后」的校验在擦除落地前会被白字回显
     * 误判为已对齐而跳过重画。代价是保留短暂闪窗，时序权威不能省。 */
    const dispatchQuestion = (q: string) => {
      // 自动切页签必须在 send 之前：aiChat.send 按当时的 rail 冻结轨道（agent 轨），
      // 于是聊天页签里敲的控制台问句也能立刻看到 Agent 页签的思考内容。
      // 只在真的发生「聊天 → Agent」切换时才顺带展开 AI 栏；本就在 Agent 页
      // （右栏可能被用户故意收起）时不动布局——回答本来就落在终端画布上。
      if (settingsStore.get().rail !== "agent") {
        void settingsActions.update({ rail: "agent" });
        dockActions.reveal("ai");
      }
      lockInput(sessionId);
      // 画问句时机不能写死延时：shell 忙时回显慢，固定 60ms 会让绿色问句落在错位
      // 光标处（重复/漂移的绿色文字）。改为「光标静止 80ms 后画，350ms 兜底」，
      // 与 PSReadLine 的逐字重绘节奏解耦。
      const kick = (cleared = true) => {
        const t0 = Date.now();
        let settled = false;
        let timer: number | undefined;
        const draw = () => {
          if (settled) return;
          const buf = term.buffer.active;
          const lastIdx = buf.baseY + term.rows - 1;
          const lastLine = buf.getLine(lastIdx);
          const rowText = lastLine ? lastLine.translateToString(true) : "";
          if (rowText.endsWith(q) && Date.now() - t0 < 900) {
            reschedule();
            return;
          }
          settled = true;
          if (timer !== undefined) window.clearTimeout(timer);
          offMove.dispose();
          term.write("\x1b[32m" + q + "\x1b[0m");
          void sshApi.write(sessionId, "\r").catch(() => {});
          void aiChat.send(sessionId, q);
          unlockInput(sessionId);
        };
        const reschedule = () => {
          if (settled) return;
          if (timer !== undefined) window.clearTimeout(timer);
          timer = window.setTimeout(draw, 80);
        };
        const offMove = term.onCursorMove(reschedule);
        // 清行没送到（写失败）：shell 里根本没被扰动，不必等光标静止，立即画。
        if (!cleared) {
          draw();
          return;
        }
        reschedule();
        window.setTimeout(draw, 350);
      };
      // Ctrl+U（\x15）：readline/行纪律自己整行清除并自擦回显，与字符数、locale 无关；
      // 按字符数计数的退格在中文 IME / 多字节 locale 下擦不干净，残留前缀会被补的空回车执行。
      // 必须 then/catch 双路都收口：写失败（如 Telnet 瞬断）时若只挂 .then，
      // 下面的 lockInput 永远无人解——该会话所有输入被静默排队，表现为终端彻底打不了字。
      sshApi.write(sessionId, "\x15").then(
        () => kick(),
        () => kick(false),
      );
    };

    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      // IME 组合期间的按键（中文输入法的 Enter 上屏、候选键等）绝不拦截：放行给输入法，
      // 否则上屏键会被误当终端回车触发 AI 问答（isComposing=true 或 keyCode=229 均属组合中）。
      // 但 Enter 例外：返回 true 会让 xterm 自己处理这个键并把 \r 送进 PTY，于是「按回车
      // 上屏候选词」变成「执行命令」——半截问句被 shell 跑掉（command not found + 文字丢失）。
      // 返回 false 只是让 xterm 跳过该键，不调 preventDefault，输入法照常上屏。
      if (e.isComposing || e.keyCode === 229) return e.key !== "Enter";
      // Ctrl+V / Shift+Insert：xterm 默认把 ^V 送 PTY 并 preventDefault 掉浏览器粘贴事件，
      // 粘贴永远到不了 textarea（远端回显 ^V）。这里自读剪贴板走 term.paste：
      // bracketed paste 与 onData 行跟踪都按正常路径走。
      if (
        ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === "v") ||
        (e.shiftKey && e.key === "Insert")
      ) {
        e.preventDefault();
        navigator.clipboard
          .readText()
          .then((t) => {
            if (t) term.paste(t);
          })
          .catch(() => toast.show(tr("tx.clipFail"), "error", notifySrc()));
        return false;
      }
      // Ctrl+F：终端内搜索（回滚区）
      if ((e.ctrlKey || e.metaKey) && e.key === "f") {
        e.preventDefault();
        setSearchOpen(true);
        return false;
      }
      // Ctrl+Space：把焦点交给侧栏 AI 输入框（仅普通屏；备用屏里可能是输入法/程序快捷键，放行）
      if (e.ctrlKey && e.code === "Space" && term.buffer.active.type === "normal") {
        e.preventDefault();
        focusChatInput();
        return false;
      }
      // 历史候选弹层开启时：↑↓ 移动高亮、Esc 关闭；Enter 视是否已导航分流——
      // 未用 ↑↓ 主动选（刚打完就回车）= 发送已输入命令；已 ↑↓ 选中 = 采纳高亮（填入不执行）。
      // 抢在 agent 回车分流与 shell 原生 ↑↓ 历史召回之前处理；Tab 不拦截（留给原生补全）。
      if (histPopupEl) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          moveHistSel(1);
          return false;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          moveHistSel(-1);
          return false;
        }
        if (e.key === "Enter" && !e.ctrlKey && !e.altKey && !e.metaKey) {
          if (histNavigated) {
            // 已用 ↑↓ 主动选中：采纳高亮候选（填入提示符、不执行）。
            e.preventDefault();
            acceptHist(histPopupSel);
            return false;
          }
          // 未导航：回车发送用户真正输入的命令——收起弹层，放行给 xterm（\r 送 PTY 正常执行）。
          hideHistPopup();
          return true;
        }
        if (e.key === "Escape") {
          e.preventDefault();
          hideHistPopup();
          return false;
        }
      }
      // 终端 AI 开关开=接管自然语言：回车送 AI，像 shell 命令的直接放行。
      // 关=纯 shell 整段跳过（用户参考图里的那个开关；聊天页签视同关，见上方 terminalAi 推导）。
      if (aiOnRef.current === "on" && e.key === "Enter" && term.buffer.active.type === "normal") {
        const q = line.trim();
        e.preventDefault();
        line = "";
        clearGhost();
        void setTyping(sessionId, false);
        if (!q) return true; // 空行交给 shell
        // 分流：explicit 只在「不像提问」时走 shell；auto 用 isShellLike 判定
        const asShell = QUESTION_ROUTING === "explicit" ? !looksLikeQuestion(q) : isShellLike(q);
        if (!e.shiftKey && asShell) {
          // agent 模式下回车被此处拦截、onData 不再触发，track 的 "\r" 分支不会执行——
          // 故这里补记命令历史（否则本会话命令永不入表，ghost 补全在 agent 模式永远空白）。
          lastCmd = q;
          historyCmds = [q, ...historyCmds.filter((c) => c !== q)].slice(0, GHOST_HIST_LIMIT);
          void history.record(sessionId, q).catch(() => {});
          // 正常 shell 命令：AI 窗口内同样排队（回放时补回车执行）
          if (isBusy(sessionId) || isSending(sessionId)) {
            queueInput(sessionId, { kind: "data", data: "\r" });
          } else {
            void sshApi.write(sessionId, "\r").catch(() => {});
          }
          return false;
        }
        if (isBusy(sessionId) || isSending(sessionId)) {
          // AI 窗口内：问句排队，窗口结束后回放
          queueInput(sessionId, { kind: "enter", q });
          return false;
        }
        if (!atCleanPrompt()) {
          // 非干净主提示符（PS2 续行 / 命令执行中）：回车属于 shell，不分派问句
          void sshApi.write(sessionId, "\r").catch(() => {});
          return false;
        }
        dispatchQuestion(q);
        return false;
      }

      const normal = term.buffer.active.type === "normal";
      // 路径补全灰字：→/End/Tab 就地接受，Tab 多候选轮换；这些键不送给 PTY。
      // 历史候选已改弹层（见上方 histPopup 分支），不再占用 Tab——原生 shell 补全不受影响。
      const tabKey = e.key === "Tab" && !e.ctrlKey && !e.altKey && !e.metaKey;
      if (normal && (e.key === "ArrowRight" || e.key === "End" || tabKey) && ghostText) {
        e.preventDefault();
        // 路径多候选时 Tab 轮换（而非一次接受）；→/End 仍接受当前灰字
        if (tabKey && pathMatches.length > 1 && lastToken(line).includes("/")) {
          pathCycle = pathCycle < 0 ? 0 : (pathCycle + 1) % pathMatches.length;
          cyclePathMatch(pathCycle);
          return false;
        }
        acceptGhost();
        return false;
      }
      // Tab 一律放行给 shell：空行按 Tab 列出当前目录文件名是终端基本操作；
      // AI 唤起走 # 首字符或 Ctrl+Space，不占用 Tab。

      // 行编辑键会让本地 line 跟踪失效（↑ 召回历史、← 移入中段等）：清空跟踪并撤 ghost，
      // 避免用过时/错位的 line 提示或误记历史；原键仍放行给 shell。
      if (
        e.key === "ArrowUp" ||
        e.key === "ArrowDown" ||
        e.key === "ArrowLeft" ||
        e.key === "Home" ||
        ((e.key === "ArrowRight" || e.key === "End") && !ghostText)
      ) {
        line = "";
        clearGhost();
        hideHistPopup();
      }

      // Agent 模式：在提示符里直接提问。
      // 输入已由 onData 本地回显，不进 shell；Enter 时才分派。

      // #：空行首字符唤起 AI 的入口按需求下线（需要时取消注释即可恢复）
      // if (
      //   e.key === "#" &&
      //   !e.ctrlKey &&
      //   !e.altKey &&
      //   !e.metaKey &&
      //   term.buffer.active.type === "normal" &&
      //   line.trim() === "" &&
      //   atCleanPrompt()
      // ) {
      //   e.preventDefault();
      //   enterAi();
      //   return false;
      // }
      return true;
    });

    const disposers = [
      // 命中计数：addon 每次 find* 后发 onDidChangeResults（仅在我们传了 decorations 时发，
      // runSearch 每次都传）。实测 resultCount 与缓冲区里该词的真实出现次数一致，软换行长行也算对。
      search.onDidChangeResults((e) => setSearchHits({ idx: e.resultIndex, count: e.resultCount })),
      // OSC 133：捕获 shell 集成吐出的提示符/命令边界标记，维护精确门控状态
      term.parser.registerOscHandler(133, (data) => {
        const p = promptRef.current;
        switch (data.charCodeAt(0)) {
          case 65: // 'A' 提示符开始
            p.atMainPrompt = false;
            break;
          case 66: // 'B' 提示符结束 = 输入起点（证明集成可用，启用精确门控）
            p.seenB = true;
            p.atMainPrompt = true;
            line = ""; // 全新提示符：清空输入跟踪，ghost 从头算
            clearGhost();
            // 补回车后的新提示符已打印：屏幕与 shell 认知同步，可回放排队输入
            setPromptReady(sessionId);
            break;
          case 67: // 'C' 命令开始：撤掉上一条失败浮标，避免新命令执行中还挂着旧报错
            p.atMainPrompt = false;
            setLastErr(null);
            break;
          case 68: {
            // 'D;<exit>[;cwd]' 命令结束携带退出码：非零则挂「让 AI 分析」浮标；成功或无码则清除残留
            p.atMainPrompt = false;
            const semi = data.indexOf(";");
            const code = semi >= 0 ? Number.parseInt(data.slice(semi + 1).split(";")[0], 10) : NaN;
            if (Number.isFinite(code) && code !== 0) setLastErr({ cmd: lastCmd, code });
            else setLastErr(null);
            break;
          }
          default: // 其它子标记：不在干净主提示符
            p.atMainPrompt = false;
        }
        return true; // 已消费，避免 xterm 作默认处理
      }),
      // 光标每次移动（多为 PTY 回显到达）后重算 ghost，使其始终贴在行尾光标处
      term.onCursorMove(() => {
        refreshGhost();
        const buf = term.buffer.active;
        setCursorPos((p) =>
          p.row === buf.cursorY && p.col === buf.cursorX ? p : { row: buf.cursorY, col: buf.cursorX },
        );
      }),
      // 滚动时视口行偏移变了，绝对定位的叠层会错位：直接清掉，下次光标移动再重算
      term.onScroll(() => {
        clearGhost();
        hideHistPopup(); // 弹层同为绝对定位叠层，滚动时必须一并收起，否则错位悬空
      }),
      term.onData((d) => {
        // 丢弃「迟到的终端查询应答」泄漏到 shell 命令行：主屏 + shell 集成确认停在干净主提示符
        // （seenB 且 atMainPrompt）时，一段纯应答序列只可能是上一个全屏程序退出后残留的，
        // 直接写进 PTY 会被 bash 回显成乱码。备用屏（全屏程序运行中）不丢，保证 vim 拿得到应答。
        if (
          term.buffer.active.type === "normal" &&
          promptRef.current.seenB &&
          promptRef.current.atMainPrompt &&
          isTerminalResponse(d)
        ) {
          return;
        }
        track(d);
        if (isBusy(sessionId) || isSending(sessionId)) {
          // AI 窗口（推理中/回复写入）内：输入暂存排队，窗口结束后按序回放。
          // 此刻进 PTY 会被 PSReadLine 用旧光标认知绝对定位重绘输入行，与 AI 文本
          // 互相覆盖；且回复写完补空回车时会把未提交的输入执行掉。
          queueInput(sessionId, { kind: "data", data: d });
          const visible = d.replace(/[\x00-\x1f\x7f]/g, "");
          if (visible) setQueued((prev) => (prev + visible).slice(-80));
          return;
        }
        void sshApi.write(sessionId, d).catch(() => {});
      }),
      term.onResize(({ cols, rows }) => {
        rememberSize(sessionId, cols, rows);
        void sshApi.resize(sessionId, cols, rows).catch(() => {});
      }),
    ];

    // 异步注册的监听：用 cancelled 兜住 StrictMode 的挂载-卸载-再挂载，避免泄漏
    let cancelled = false;
    let fitTimer = 0;
    let lastFitW = 0;
    let lastFitH = 0;
    let unlisten: UnlistenFn | undefined;
    /** Shell 集成注入期：吞掉 `stty` / 哨兵 / 多余提示符回显。 */
    const injectUntil = Date.now() + 4000;
    const filterInjectEcho = (bytes: Uint8Array): Uint8Array | null => {
      if (Date.now() > injectUntil) return bytes;
      const s = new TextDecoder().decode(bytes);
      if (!/__ST_OSC133|stty -echo|stty echo|__st_/.test(s)) return bytes;
      const kept = s
        .split("\n")
        .filter((line) => !/__ST_OSC133|stty -echo|stty echo|__st_/.test(line))
        .join("\n");
      return kept ? new TextEncoder().encode(kept) : null;
    };
    // true = 本终端已经有过内容（实时事件或补帧）。补帧只在「一个实时事件都没收到」时兜底：
    // ring 是整段尾部快照，实时事件已写过再补一遍会把启动输出原样重复显示（二次登录假象）。
    let gotOutput = false;
    sshApi
      .onOutput(sessionId, (bytes) => {
        const filtered = filterInjectEcho(bytes);
        if (!filtered) return; // 纯注入回显，整块丢掉
        if (filtered.length === 0) return;
        gotOutput = true;
        term.write(filtered, () => {
          //  输出触发器：对本段输出做本地匹配（告警 toast / 行高亮）
          if (cancelled) return;
          const text = new TextDecoder().decode(filtered);
          for (const hit of scanTriggers(text, triggerStore.get().items)) {
            if (hit.rule.action === "alert") {
              toast.show(
                tr("tx.trigger").replace("{n}", hit.rule.name).replace("{m}", hit.match),
                "warn",
                notifySrc(),
              );
            } else {
              // 高亮：在当前光标行挂一条装饰（半透明底），提示有匹配发生
              const marker = term.registerMarker(0);
              if (marker) {
                const deco = term.registerDecoration({ marker, width: term.cols });
                setTimeout(() => {
                  deco?.dispose();
                  marker.dispose();
                }, 4000);
              }
            }
          }
        });
      })
      .then((u) => {
        if (cancelled) {
          u();
          return;
        }
        unlisten = u;
        // 环缓冲补帧：订阅前漏掉的启动输出（ConPTY 提示符）两次机会
        const replay = async () => {
          if (cancelled || gotOutput) return;
          try {
            // 必须用原始字节：snapshot 会 strip_ansi 丢掉 \r，导致行粘连
            const b64 = await sshApi.ringB64(sessionId, 16384);
            if (cancelled || gotOutput || !b64) return;
            const bytes = b64ToBytes(b64);
            if (bytes.length > 0) {
              term.write(bytes);
              gotOutput = true;
            }
          } catch {
            /* ignore */
          }
        };
        void replay();
        // 前 2s 轮询环缓冲：ConPTY 冷启动输出可能迟到或事件漏一拍
        let n = 0;
        const poll = window.setInterval(() => {
          n += 1;
          void replay();
          if (gotOutput || n >= 8) clearInterval(poll);
        }, 250);
      });

    // 尺寸变化 → fit。用**尾随防抖**而非每帧 rAF：窗口拖拽/最大化期间 WebView 连续派发 resize，
    // 若每帧都 fit()+term.resize()+整屏重绘+远端 PTY resize IPC，会打满主线程，令右栏/底部面板掉帧、
    // 呈现「慢慢滑入」的卡顿观感。改为拖拽中不 fit（画布随 CSS 拉伸），停手 ~90ms 后 fit 一次校正，
    // 主线程全程空闲，右栏跟随窗口即时重排。尺寸为 0（最小化/隐藏）或自上次未变时跳过，杜绝退化与 RO↔fit 自激。
    const doFit = () => {
      const w = el.clientWidth;
      const h = el.clientHeight;
      if (w < 2 || h < 2) return; // 最小化/隐藏：跳过，避免退化尺寸污染 cols/rows
      if (w === lastFitW && h === lastFitH) return; // 尺寸未变：不重复 fit
      lastFitW = w;
      lastFitH = h;
      fit.fit();
    };
    const ro = new ResizeObserver(() => {
      if (fitTimer) clearTimeout(fitTimer);
      fitTimer = window.setTimeout(doFit, 90);
    });
    ro.observe(el);

    // 备用屏（vim/less/top）：滚轮映射为 ↑↓，与常见终端一致；主屏仍滚回滚区。
    // 应用开了鼠标跟踪（如 vim mouse=a）时不劫持，交给 xterm 原样上报。
    const onWheel = (e: WheelEvent) => {
      if (term.buffer.active.type !== "alternate") return;
      const modes = (term as unknown as { modes?: { mouseTracking?: number } }).modes;
      if (modes?.mouseTracking) return;
      e.preventDefault();
      const steps = Math.min(4, Math.max(1, Math.round(Math.abs(e.deltaY) / 40) || 1));
      const seq = e.deltaY < 0 ? "\x1b[A" : "\x1b[B";
      for (let i = 0; i < steps; i++) void sshApi.write(sessionId, seq);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    // 关键字着色（客户端语义着色）：在已解析的缓冲行上按 colorize 规则上色。
    // 主题/开关/规则都惰性从 store 取，避开 [sessionId] 依赖数组带来的旧闭包。
    const highlight = attachTermHighlight(term, {
      rules: () => triggerStore.get().items,
      colorFor: (key) => {
        const a = appearanceStore.get();
        return highlightColor(resolveTheme(a.theme, a.customTheme), key);
      },
      enabled: () => terminalStore.get().keywordHighlight,
    });
    highlightRef.current = highlight;
    disposers.push({ dispose: () => highlight.dispose() });
    // 切屏必然伴随写入，`onWriteParsed` 是 xterm 解析批次后的节流回调，拿它同步备用屏状态
    // 比自己轮询或挂在按键回调上都准（vim 可能是用户按 ↑ 翻历史敲出来的，与输入无关）。
    // 着色也挂这里：此刻整批写入已成行，一行里的关键字无论来自几个分片都能整词上色。
    disposers.push(
      term.onWriteParsed(() => {
        const buf = term.buffer.active;
        setAltScreen(buf.type !== "normal");
        // 光标随 PTY 输出移动时 onCursorMove 不一定触发（如整屏重绘），批次解析完直接取终值兜底
        setCursorPos((p) =>
          p.row === buf.cursorY && p.col === buf.cursorX ? p : { row: buf.cursorY, col: buf.cursorX },
        );
        highlight.onParsed();
      }),
    );
    const unbindTerm = bindTerm(sessionId, term, (data) => sshApi.write(sessionId, data));
    // 回复窗口结束：按序回放排队输入（数据直写 PTY；问句重走退格+绿色绘制+发送流程）
    const unbindReplay = setReplayHandler(sessionId, (inputs) => {
      for (const input of inputs) {
        if (input.kind === "data") {
          void sshApi.write(sessionId, input.data).catch(() => {});
        } else {
          // 排队问句的回显由 Ctrl+U 整行清除后再画绿色（见 dispatchQuestion）
          dispatchQuestion(input.q);
        }
      }
      setQueued("");
    });

    return () => {
      cancelled = true;
      if (fitTimer) clearTimeout(fitTimer);
      ro.disconnect();
      el.removeEventListener("wheel", onWheel);
      clearGhost();
      disposers.forEach((d) => d.dispose());
      unlisten?.();
      unbindTerm();
      unbindReplay();
      highlightRef.current = null;
      // 时间线清理在 sessionStore.dropTab（会话真正结束）做：
      // 弹出/归还独立窗口也会卸载本组件，那里不能清掉另一窗口在用的记录
      termRef.current = null;
      term.dispose();
    };
  }, [sessionId]);

  // 热切换（+ 终端偏好）：外观/滚动缓冲/光标变化 → 就地更新运行中 xterm 的 options
  // （内容/滚动/光标全保留）。xterm 的 options 均可运行期赋值，改字号后自动重排。
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    const th = resolveTheme(themeId, customTheme);
    // 搜索态下选区色让位给命中色，避免选区混色把当前命中冲灰（见 withSearchTint）
    term.options.theme = searchOpen ? withSearchTint(th) : th;
    term.options.fontSize = fontSize;
    term.options.fontFamily = fontFamily;
    term.options.scrollback = scrollback;
    term.options.cursorBlink = cursorBlink;
    term.options.cursorStyle = cursorStyle;
    term.options.lineHeight = lineHeight;
    term.options.letterSpacing = letterSpacing;
    // bellStyle 不是 xterm option：由创建时挂的 onBell 事件读最新值，无需此处处理
  }, [themeId, fontSize, fontFamily, customTheme, scrollback, cursorBlink, cursorStyle, lineHeight, letterSpacing, searchOpen]);

  // 关键字着色重扫：主题（取色变了）/ 总开关 / 规则集变化 → 清掉旧着色并按新配置回看历史行。
  // 不重扫也能对**后续输出**生效，重扫是为了让改动立刻作用于屏上已有内容（所见即所得）。
  useEffect(() => {
    highlightRef.current?.rescan();
  }, [themeId, customTheme, keywordHighlight, triggerItems]);

  // 切回本终端：窗格由隐藏转为显示时收回键盘焦点。
  // （过去靠「切标签重挂载」顺带 focus；根治不再重挂载后，这里显式补一次。）
  useEffect(() => {
    if (active) termRef.current?.focus();
  }, [active]);


  return (
    <div
      className="term-wrap"
      onContextMenu={(e) => {
        e.preventDefault();
        // 选区快照：自定义菜单的遮罩 mousedown 会清掉 xterm 选区，拖到菜单上时才读就读空了，
        // 故在打开菜单那一刻一次性取下来（「问 AI」只吃这份快照）。
        setCtxMenu({ x: e.clientX, y: e.clientY, sel: termRef.current?.getSelection() ?? "" });
      }}
      onMouseDown={(e) => {
        // 「点终端 = 终端聚焦」：点在交互控件/浮层（输入框、按钮、AI 薄条、右键菜单、搜索、
        // 确认条）上时不抢焦点，其余落点（画布、6px padding、空白区——xterm 原生点击未必命中）
        // 一律把焦点收回终端。
        const el = e.target as HTMLElement | null;
        if (
          el?.closest(
            "input,textarea,button,select,a,[contenteditable],.ctx,.term-search,.agent-float,.term-think",
          )
        )
          return;
        termRef.current?.focus();
      }}
    >
      {/* 顶部细条（不属于 xterm 画布，永不遮挡输出）：左端光标位置，右端 AI 开关。
          备用屏（vim/less 等全屏程序）时整条收起，画布占回那一行。 */}
      {aiStrip && !altScreen && tab && kindHasTerminal(tab.kind) && (
        <div className="term-strip">
          <span className="term-strip-pos" title={tr("tx.cursorPosTip")}>
            {tr("tx.cursorRow")} {cursorPos.row + 1}
            <span className="term-strip-pos-gap" />
            {tr("tx.cursorCol")} {cursorPos.col + 1}
          </span>
          {canAgentRail(tab.kind) && (
            <>
              {terminalAi === "on" && (
                <span className="term-strip-hint" title={withTriggerHint(tr("atg.stripHint.title"))}>
                  {tr("atg.stripHint")}
                </span>
              )}
              <AiToggle sessionId={sessionId} />
            </>
          )}
        </div>
      )}
      <div className="terminal" ref={hostRef} />
      {/* 快捷命令栏：固定行高、附在画布下方（term-wrap 已 flex column，.terminal flex:1 吃剩余）。 */}
      {tab && <QuickBar sessionId={sessionId} tab={tab} />}
      {/* 右键菜单走 portal 挂到 body：.term-wrap 被显式给了 z-index:0（管 xterm 图层），
          它因此成为层叠上下文，内部 z-index 也只在它自己那层有效，会被下栏拖动条（z-index:50）盖住。
          以前这里是一套自绘 .term-ctx（无边缘翻转——贴右/贴下边缘时菜单被窗口裁掉、无 Esc、无键盘），
          现已并入通用 ContextMenu（只靠 .ctx-term 抬高 z-index）。 */}
      {ctxMenu &&
        createPortal(
          <ContextMenu
            className="ctx-term"
            x={ctxMenu.x}
            y={ctxMenu.y}
            onClose={() => setCtxMenu(null)}
            items={[
              {
                label: tr("tx.copy"),
                // 无选区时置灰而非消失：菜单项常驻、靠 disabled 表达「此刻做不了」（与文件栏
                // 右键菜单同约定）；条件渲染只用在整项在此刻毫无意义的场景（如「问 AI」要本会话 AI 可用）。
                // 动作与判据同吃 onContextMenu 那一份选区快照，不再一边读快照一边读实时选区。
                disabled: ctxMenu.sel.trim() === "",
                onClick: () => {
                  void navigator.clipboard.writeText(ctxMenu.sel);
                  refocusTerm();
                },
              },
              /* 复制并粘贴：选中一段长路径/主机名后要敲回命令行，旧路子得先点复制再点粘贴两下。
                 不排备用屏——vim/less 里把选中文本再输一遍是合理诉求，不像抓选区问 AI 那样无意义；
                 未连接（断线页）同样置灰：文本送不进已死的 PTY，点了没反应比灰掉更让人迷惑。 */
              {
                label: tr("tx.copyPaste"),
                disabled: ctxMenu.sel.trim() === "" || tab?.state !== "connected",
                onClick: () => {
                  const sel = ctxMenu.sel;
                  void navigator.clipboard.writeText(sel);
                  // 走 term.paste：与键盘粘贴同一条路（bracketed paste + onData 行跟踪）
                  termRef.current?.paste(sel);
                  refocusTerm();
                },
              },
              /* 框选问 AI：只看「有选区 + 非备用屏 + 本会话 AI 可用（与 AiDrawer 入口同判据：exec/fs/pty）」，
                 不看终端 AI 开关。
                 理由：「开关关上后终端不叠 AI 浮层」约束的是浮层，右键菜单项不是浮层；
                 若一并门控，纯 shell 模式下「顺手问一下选中的输出」这条最自然的诉求就不可达。
                 备用屏（vim/less/top）里选区是程序自己的概念，抓出来无意义。 */
              ...(ctxMenu.sel.trim() !== "" &&
              !altScreen &&
              tab &&
              hasAnyCap(tab.kind, ["exec", "fs", "pty"]) &&
              tab.state === "connected"
                ? [
                    {
                      label: tr("tx.askAi"),
                      onClick: () => askAiSel(ctxMenu.sel),
                    },
                  ]
                : []),
              {
                label: tr("tx.paste"),
                // 与「复制并粘贴」同判据：未连接时粘进已死的 PTY 同样没反应。剪贴板内容有异步才能知道，
                // 不值得为它预读一次，故只在会话态上置灰。
                disabled: tab?.state !== "connected",
                onClick: () => {
                  // 走 term.paste：bracketed paste 与 onData 行跟踪与键盘粘贴一致
                  void navigator.clipboard.readText().then((t) => {
                    if (t) termRef.current?.paste(t);
                  });
                  refocusTerm();
                },
              },
              {
                label: tr("tx.selectAll"),
                onClick: () => {
                  termRef.current?.selectAll();
                  refocusTerm();
                },
              },
              {
                label: tr("tx.find"),
                sep: true,
                onClick: () => setSearchOpen(true),
              },
              {
                label: tr("tx.export"),
                // 二级飞出：导出方式两项收在一个父项里，父项本身不可点执行
                children: [
                  {
                    label: tr("tx.exportWithAi"),
                    onClick: () => void runExport(true),
                  },
                  {
                    label: tr("tx.exportShellOnly"),
                    onClick: () => void runExport(false),
                  },
                ],
                onClick: () => {},
              },
              {
                label: tr("tx.clear"),
                sep: true,
                onClick: () => {
                  termRef.current?.clear();
                  nudgeRedraw();
                  refocusTerm();
                },
              },
              {
                label: tr("tx.reset"),
                onClick: () => {
                  termRef.current?.reset();
                  nudgeRedraw();
                  refocusTerm();
                },
              },
            ]}
          />,
          document.body,
        )}
      {/* 整轮思考指示：工具多轮也算「思考中」，结束自动消失。只在终端 AI 开关开时出现——
          关上后终端就是纯 shell，不该叠任何 AI 浮层（别遮挡终端内容）。*/}
      {terminalAi === "on" && sending && (
        <div
          className="term-think"
          role="status"
          data-lift={pending.length > 0 ? "true" : "false"}
        >
          <span className="term-think-dot">●</span>
          <span>
            {(queued
              ? tr("tx.thinkingQueued").replace("{q}", queued)
              : tr(`tx.${thinkingKeyForPhase(tn?.phase ?? "idle")}`)
            ).replace("{e}", formatElapsed(elapsed))}
          </span>
          <button
            type="button"
            className="term-think-cancel"
            onClick={() => aiChat.cancel(sessionId, "agent")}
            title={tr("aw.stopTip")}
          >
            {tr("tx.abort")}
          </button>
        </div>
      )}
      {/* 终端底部叠层：只留回滚搜索 + 统一授权确认条。AI 问答/输入全部在右侧栏，
          原先这里还有一条 InlineAi 薄条（“AI 已完成”），与侧栏时间线重复，已整条移除 */}
      <div className="term-bottom">
        {searchOpen && (
          <div className="term-search">
            <input
              ref={searchInputRef}
              autoFocus
              value={searchQ}
              placeholder={tr("tx.searchPh")}
              spellCheck={false}
              onChange={(e) => {
                const q = e.target.value;
                setSearchQ(q);
                runSearch(q);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  runSearch(searchQ, e.shiftKey);
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  closeSearch();
                }
              }}
            />
            {/* 命中位置/总数：搜索覆盖整个回滚缓冲区，屏上只画得下视口那几处——不报数就永远说不清
                「到底标了几个」。达到 addon 高亮上限（1000）时显 1000+，不假装是精确值。 */}
            {searchQ !== "" && (
              <span className="ts-count" title={tr("tx.hitsTip")}>
                {searchHits === null || searchHits.count === 0
                  ? tr("tx.noMatch")
                  : searchHits.idx < 0
                    ? `${searchHits.count >= 1000 ? "1000+" : searchHits.count}`
                    : `${searchHits.idx + 1}/${searchHits.count >= 1000 ? "1000+" : searchHits.count}`}
              </span>
            )}
            {/* 共享 <Button>：自包含样式，不依赖祖先容器，避免退回浏览器默认灰框。 */}
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                runSearch(searchQ, true);
                keepSearchFocus();
              }}
            >
              {tr("tx.prev")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                runSearch(searchQ);
                keepSearchFocus();
              }}
            >
              {tr("tx.next")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                closeSearch();
              }}
            >
              {tr("common.close")}
            </Button>
          </div>
        )}
      </div>
      {/* 授权确认条：终端 AI 开关开时贴在这里（提示符正下方，离敲命令的地方最近，也是唯一的裁决入口）。
          开关关上后终端不渲染它——那时审批条挂到右栏当前页签里（AgentPanel / AiDrawer），
          同一组件同一套观感，不去打扰纯 shell 的终端画面。 */}
      {/* 报错浮标：上一条命令非零退出时右下角浮现「让 AI 分析」。只在终端 AI 开关开着时出现
          （关上=纯 shell，不在终端叠任何 AI 浮层，与 .term-think 同闸）。点分析→展开右栏并直接发送诊断问句。 */}
      {terminalAi === "on" && lastErr && (
        <div className="term-err" role="alert">
          <span className="term-err-ico" aria-hidden="true">
            ✕
          </span>
          <span className="term-err-text">
            {tr("tx.errExit").replace("{code}", String(lastErr.code))}
            {lastErr.cmd && <code className="term-err-cmd">{lastErr.cmd}</code>}
          </span>
          <button type="button" className="term-err-diag" onClick={diagnoseErr} title={tr("tx.diagnoseTip")}>
            {tr("tx.diagnose")}
          </button>
          <button
            type="button"
            className="term-err-x"
            onClick={() => setLastErr(null)}
            title={tr("common.close")}
            aria-label={tr("common.close")}
          >
            ✕
          </button>
        </div>
      )}
      {/* 终端浮层审批/反问不按 rail 门控：ApprovalBar / QuestionBar 都按 surface 严格过滤，
          AiDrawer 内联条只渲 chat 轨——agent 轨的待裁决项在 UI 上只有终端浮层这一处。
          若按 rail === "agent" 门控，用户切到聊天页签时 agent 轨的审批/反问就毫无可见入口，
          那一轮永久挂住。双框重复已由 surface 分轨杜绝，故这里无条件渲染。
          keysOff：备用屏（vim/less/top）下不接管 Ctrl+Enter 审批键位，全屏程序自己的按键优先。 */}
      {terminalAi === "on" && (
        <ApprovalBar sessionId={sessionId} surface="agent" keysOff={altScreen} />
      )}
      {terminalAi === "on" && <QuestionBar sessionId={sessionId} surface="agent" />}
      {tab?.state === "disconnected" && !discDismissed && (
        <div className="term-disc" role="status">
          <span className="term-disc-dot" aria-hidden="true" />
          <span className="term-disc-tag">DISCONNECTED</span>
          {tab.reason && <span className="term-disc-reason">{tab.reason}</span>}
          <span className="term-disc-spacer" />
          <span className="term-disc-note">{tr("tx.bufferKept")}</span>
          <button
            type="button"
            className="term-disc-btn"
            disabled={busy === sessionId}
            onClick={() => void sessionActions.reconnect(sessionId)}
          >
            {busy === sessionId ? tr("tx.connecting") : tr("tx.reconnect")}
          </button>
          <button
            type="button"
            className="term-disc-x"
            onClick={() => setDiscDismissed(true)}
            title={tr("common.close")}
            aria-label={tr("common.close")}
          >
            ✕
          </button>
        </div>
      )}
    </div>
  );
}
