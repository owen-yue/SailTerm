// Agent → 终端本地渲染：提问回显、● 说明、待授权灰字提示、思考条。
// 只 term.write / decoration，不经过 PTY（避免当命令执行）。

import type { ITheme, Terminal } from "@xterm/xterm";
import { t as tr } from "./i18n";
import { mdToAnsi } from "./mdToAnsi";

export interface CmdCardHandlers {
  exec: () => void;
  reject: () => void;
}

/** 会话 id → xterm 实例（Agent 事件按会话路由）。 */
const terms = new Map<string, Terminal>();
/** 会话 id → 写 PTY 通道（AI 回复后补空回车用）。 */
const writers = new Map<string, (data: string) => Promise<void>>();
/** 用户是否正在提示符行输入（AI 补回车前要避让，否则会把用户输入执行掉）。 */
const typing = new Map<string, boolean>();
/** 最近一次用户按键时刻（AI 文本写入要避开用户敲键引发的 PSReadLine 逐字重绘）。 */
const lastKeyAt = new Map<string, number>();

/** 会话是否处于「AI 回复窗口」：从 send 开始到回复写完+提示符就绪。期间用户输入排队。 */
const busy = new Map<string, boolean>();
/** 会话是否在「AI 推理中」（send 到 writeReply 之间）：期间字符可直通，Enter 需排队。 */
const sendingSet = new Map<string, boolean>();
/** 是否已补空回车、正在等 shell 重打提示符（OSC 133 'B' 或超时兜底）。 */
const awaitingPrompt = new Map<string, boolean>();
/** 排队中的用户输入（回复窗口结束后按序回放进 PTY）。 */
const pendingInputs = new Map<string, PendingInput[]>();
/** 回放回调（Terminal.tsx 注册：把排队输入按序写进 PTY / 重新分发问句）。 */
const replayCbs = new Map<string, (inputs: PendingInput[]) => void>();
/** 回复轮次计数（区分「兜底超时」与「下一轮 send」的竞态）。 */
const replySeqs = new Map<string, number>();

/** 排队输入：终端数据（字符/控制键）或一次 AI 问句提交。 */
export type PendingInput =
  | { kind: "data"; data: string }
  | { kind: "enter"; q: string };

/** 结束回复窗口：解除 busy，按序回放排队输入（幂等，OSC 133 与兜底超时先到先得）。
 * 注意不清 sendingSet：画绿色问句的短锁结束时会回放（此刻 send 刚把推理窗口开启，
 * 清掉会让推理期的 Enter 排队失效）；sendingSet 统一由 writeReply 开头收尾。 */
function finishReply(sessionId: string): void {
  if (!busy.get(sessionId)) return;
  busy.set(sessionId, false);
  awaitingPrompt.set(sessionId, false);
  const inputs = pendingInputs.get(sessionId) ?? [];
  pendingInputs.set(sessionId, []);
  replayCbs.get(sessionId)?.(inputs);
}

export function bindTerm(
  sessionId: string,
  term: Terminal,
  write?: (data: string) => Promise<void>,
): () => void {
  terms.set(sessionId, term);
  if (write) writers.set(sessionId, write);
  return () => {
    if (terms.get(sessionId) === term) terms.delete(sessionId);
    writers.delete(sessionId);
    typing.delete(sessionId);
    busy.delete(sessionId);
    awaitingPrompt.delete(sessionId);
    pendingInputs.delete(sessionId);
    replayCbs.delete(sessionId);
    replySeqs.delete(sessionId);
    sendingSet.delete(sessionId);
  };
}

/** 标记用户是否正在当前提示符行输入（由 Terminal.tsx 的行跟踪同步）。 */
export function setTyping(sessionId: string, active: boolean): void {
  typing.set(sessionId, active);
  lastKeyAt.set(sessionId, Date.now());
}

/** 进入 AI 推理窗口（aiStore.send 开头调用）：期间该会话的 Enter 排队防丢失；
 * 普通字符仍实时进 PTY（屏幕静止，回显安全）。回复写入时才进入 busy 全排队窗口。 */
export function beginReply(sessionId: string): void {
  sendingSet.set(sessionId, true);
  awaitingPrompt.set(sessionId, false);
}

/** 是否处于 AI 推理窗口（Terminal.tsx 据此排队 Enter）。 */
export function isSending(sessionId: string): boolean {
  return sendingSet.get(sessionId) === true;
}

/** 是否处于 AI 回复窗口（Terminal.tsx 据此排队用户输入）。 */
export function isBusy(sessionId: string): boolean {
  return busy.get(sessionId) === true;
}

/** 锁定输入（退格擦回显→画绿色问句的短窗口）：期间用户输入排队，
 * 防止敲字与绿色问句重叠；画完调 unlockInput 回放。 */
export function lockInput(sessionId: string): void {
  busy.set(sessionId, true);
  armLockWatchdog(sessionId);
}

/** 解锁并回放锁期间排队输入（幂等）。 */
export function unlockInput(sessionId: string): void {
  finishReply(sessionId);
}

/** 短锁看门狗（仅 lockInput 挂）：正常流程 ~1s 内必 unlockInput，历史 bug 表明任何
 * 一步漏收（如写 PTY 失败不走后续）都会把会话输入永久静默排队——终端表现为
 * 彻底打不了字。每 1.5s 复查、6.5s 兜底强制收窗口；writeReply 的合法 busy
 * 不经 lockInput、不会被它提前拆。 */
function armLockWatchdog(sessionId: string, waited = 0): void {
  window.setTimeout(() => {
    if (!busy.get(sessionId)) return; // 正常路径已 unlockInput，静默退场
    // 未到兜底上限（含推理窗口在途：draw 里 send 先于 unlockInput 开启 sendingSet）：再等一轮
    if (waited < 5000) {
      armLockWatchdog(sessionId, waited + 1500);
      return;
    }
    // 漏收兜底：强制结束窗口并回放排队输入（finishReply 幂等）
    finishReply(sessionId);
  }, 1500);
}

/** 排队一次用户输入（上限保护，防止无界缓冲）。 */
export function queueInput(sessionId: string, input: PendingInput): void {
  const arr = pendingInputs.get(sessionId) ?? [];
  if (arr.length >= 256) return;
  arr.push(input);
  pendingInputs.set(sessionId, arr);
}

/** 注册排队回放回调（Terminal.tsx 绑定终端时注册）；返回解除函数。 */
export function setReplayHandler(
  sessionId: string,
  cb: (inputs: PendingInput[]) => void,
): () => void {
  replayCbs.set(sessionId, cb);
  return () => {
    if (replayCbs.get(sessionId) === cb) replayCbs.delete(sessionId);
  };
}

/** 提示符就绪（Terminal.tsx 的 OSC 133 'B' 事件）：补回车后的新提示符已打印，
 * 屏幕与 shell 认知同步，回放排队输入才安全。 */
export function setPromptReady(sessionId: string): void {
  if (busy.get(sessionId) && awaitingPrompt.get(sessionId)) finishReply(sessionId);
}

const C = {
  cyan: (s: string) => `\x1b[36m${s}\x1b[0m`,
  dim: (s: string) => `\x1b[2m${s}\x1b[0m`,
  green: (s: string) => `\x1b[32m${s}\x1b[0m`,
  bold: (s: string) => `\x1b[1m${s}\x1b[0m`,
};

/** 提问回显（绿色，接在提示符后）；由终端 Enter 流程调用，不在 send 里重复画。 */
export function writeQuestion(sessionId: string, text: string): void {
  const t = terms.get(sessionId);
  if (!t) return;
  t.write(`\x1b[32m${text}\x1b[0m\r\n`);
}

/** Shell 档专用：AI 不在终端落笔，但推理/回复窗口的状态要照常收掉（否则 busy 与排队输入残留）。 */
export function endReplyWindow(sessionId: string): void {
  sendingSet.set(sessionId, false);
  finishReply(sessionId);
}

/** 终止本轮 AI：在终端写一行 dim「已终止」并收掉回复窗口。
 * 与 endReplyWindow 不同：此处绕过 finishReply 的 busy 守卫，无条件排空并回放排队输入
 * （推理期排队的 Enter 也要在终止后回到 shell），并清 sendingSet/busy/awaitingPrompt。 */
export function writeTerminated(sessionId: string): void {
  const t = terms.get(sessionId);
  if (t) {
    if (t.buffer.active.cursorX > 0) t.write("\r\n");
    t.write(C.dim(tr("ta.terminated")) + "\r\n");
  }
  sendingSet.set(sessionId, false);
  busy.set(sessionId, false);
  awaitingPrompt.set(sessionId, false);
  const inputs = pendingInputs.get(sessionId) ?? [];
  pendingInputs.set(sessionId, []);
  replayCbs.get(sessionId)?.(inputs);
}

/** 终端里一条回复最多打几行：由「回复详略」设置传入（`0` = 不截）。 */
export function writeReply(sessionId: string, text: string, maxLines: number): void {
  // 推理窗口结束：从这里起用户输入全部排队（回复写入会让屏幕与 shell 认知短暂失步）
  sendingSet.set(sessionId, false);
  const t = terms.get(sessionId);
  if (!t) {
    // 终端已卸载：直接结束本轮回复窗口（幂等），不留残留 busy
    finishReply(sessionId);
    return;
  }
  busy.set(sessionId, true);
  // 就地书写不能假定光标在行首：console 模式下命令在终端执行完，PowerShell 会立即
  // 打印新提示符，光标停在提示符后；直接贴会写成 `PS C:\...> ● 回复`，污染提示符行。
  // 不在行首先换行。（回复窗口内用户输入已排队，此刻屏幕静止，写入无竞争。）
  // 拼成单个字符串一次写入（含前置换行）：多次 write 之间事件循环可能插入
  // PSReadLine 重绘或提问绿字，把 AI 文本拆散到错误位置。
  if (t.buffer.active.cursorX > 0) t.write("\r\n");
  // 拼成单个字符串一次写入：多次 write 之间事件循环可能插入 PSReadLine 的重绘输出
  // 终端只打要点：按详略档收口，全文完整留在右侧栏时间线（那里才是读长回复的地方）。
  // 先截断后渲染：clipForTerm 直接按原始 Markdown 行数收口（并尽量落在空行段落边界，
  // 不把代码块切一半），渲染只影响展示、不回头参与截断判定，两者互不干扰。
  const { body, dropped } = clipForTerm(text, maxLines);
  const lines = mdToAnsi(body).split("\n");
  let out = C.cyan("● ") + (lines[0] ?? "") + "\r\n";
  for (const l of lines.slice(1)) out += `  ${l}\r\n`;
  if (dropped > 0) out += C.dim(tr("ta.moreLines").replace("{n}", String(dropped))) + "\r\n";
  t.write(out);
  // 回复写在 shell 提示符之外，shell 并不知情：补一个空回车让 shell 重打提示符，
  // 否则回复后光标悬在文本末尾、没有新命令行。补回车后必须等提示符就绪再回放排队
  // 输入：提示符打印完之前用户输入进 PTY，会被 PSReadLine 按旧光标认知绝对定位
  // 重绘，与 AI 文本互相覆盖——这是错乱的根源。
  const w = writers.get(sessionId);
  if (!w) {
    finishReply(sessionId);
    return;
  }
  const seq = (replySeqs.get(sessionId) ?? 0) + 1;
  replySeqs.set(sessionId, seq);
  awaitingPrompt.set(sessionId, true);
  void w("\r").catch(() => finishReply(sessionId));
  // 等提示符就绪（新提示符打印后光标停在提示符后，cursorX>0）再回放排队输入；
  // 不依赖 OSC 133 集成，本地 PowerShell 5.1 同样可靠，800ms 兜底。
  const deadline = Date.now() + 800;
  const waitPrompt = () => {
    if (replySeqs.get(sessionId) !== seq) return; // 已被下一轮接管
    const tt = terms.get(sessionId);
    if (!tt || tt.buffer.active.cursorX > 0 || Date.now() > deadline) {
      finishReply(sessionId);
      return;
    }
    window.setTimeout(waitPrompt, 50);
  };
  window.setTimeout(waitPrompt, 50);
}

/** 思考指示已改为终端底部状态条（见 Terminal.tsx `term-think`），不再写入 xterm。 */

/** 待授权提示：只在终端补一行灰字，命令本身不再落笔重复（确认条已展示全文与目的）。
 * 同意后的留痕也不画：命令会由后端打进 PTY 执行，shell 自己的回显就是最真实的记录，
 * 控制台里看起来与手打无异。 */
export function writeApprovalHint(sessionId: string): void {
  const t = terms.get(sessionId);
  if (!t) return;
  if (t.buffer.active.cursorX > 0) t.write("\r\n");
  t.write(C.dim(`  ${tr("ta.approveHint")} ${tr("ta.confirmBelow")}\r\n`));
}

/** 终端摘要：超过 maxLines（`0`/不足一行 = 不截）只留前若干行，且尽量落在空行（段落边界）——
 * 把表格 / 代码块切一半比少说几句更糟。至少留一段，所以「开头就是长代码块」时会整块打出（那正是用户要的）。 */
function clipForTerm(text: string, maxLines: number): { body: string; dropped: number } {
  const lines = text.split("\n");
  if (maxLines <= 0 || lines.length <= maxLines) return { body: text, dropped: 0 };
  let cut = maxLines;
  for (let i = maxLines; i > 1; i--) {
    if (lines[i - 1].trim() === "") {
      cut = i - 1;
      break;
    }
  }
  const body = lines.slice(0, cut).join("\n").trimEnd();
  return { body, dropped: lines.length - cut };
}

/** 把焦点交还给指定会话的终端（侧栏输入框按 Esc 回终端时用）。 */
export function focusTerm(sessionId: string | null): void {
  if (!sessionId) return;
  terms.get(sessionId)?.focus();
}

/** 插件命令包（只读）执行结果回显：命令 + 捕获到的输出打进会话终端。
 * 全部前端本地写、不经 PTY（输出已由后端 exec 独立通道捕获，这里只是别把它丢掉）。
 * 终端未挂载返回 false，调用方降级到其他提示渠道。 */
export function writePluginOutput(
  sessionId: string,
  command: string,
  stdout: string,
  stderr: string,
  exitCode: number | null,
): boolean {
  const t = terms.get(sessionId);
  if (!t) return false;
  if (t.buffer.active.cursorX > 0) t.write("\r\n");
  let out = `  ${C.bold(command)}\r\n`;
  const body = stdout.trimEnd() || stderr.trimEnd();
  if (body) {
    for (const l of body.split("\n")) out += `  ${l}\r\n`;
  } else {
    out += C.dim(`  ${tr("ta.pluginNoOutput")}\r\n`);
  }
  if (exitCode !== null && exitCode !== 0) out += C.dim(`  exit ${exitCode}\r\n`);
  t.write(out);
  return true;
}

/** 提示符旁灰字引导（仅一次可写，避免刷屏）。 */
export function writePromptHint(sessionId: string): void {
  const t = terms.get(sessionId);
  if (!t) return;
  t.write(C.dim(tr("ta.promptHint") + "\r\n"));
}

/** 调试/测试：解析是否像 shell 命令（与 lib/agentMode 保持一致语义）。 */
export function _unusedTheme(_t: ITheme | undefined): void {}
