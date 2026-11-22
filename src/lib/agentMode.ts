// Agent 模式：区分「shell 命令」与「自然语言」。
// 两套分流策略（由 Terminal 回车处理里的 QUESTION_ROUTING 选一）：
//   "explicit"（默认）= looksLikeQuestion：拿不准一律当命令，只有明确提问信号才走 AI，最防劫持。
//   "auto"          = isShellLike：从文本猜意图，口语提问更顺手但有误判空间。
// Shift+Enter 始终强制提问（由调用方处理）。

/** 常见命令首词（命中则当 shell，交给提示符执行）。 */
const SHELL_CMDS = new Set([
  "ls","cd","cat","pwd","whoami","uname","df","free","top","htop","ps","grep","tail","head",
  "systemctl","docker","nginx","vim","vi","nano","ssh","scp","curl","wget","ping","ip","ifconfig",
  "less","more","python","python3","node","npm","pnpm","yarn","git","apt","apt-get","yum","dnf",
  "make","sudo","chmod","chown","rm","cp","mv","mkdir","touch","echo","export","env","history",
  "clear","exit","logout","su","passwd","journalctl","ss","netstat","lsof","kill","pkill","find",
  "tar","zip","unzip","rsync","sed","awk","sort","uniq","wc","date","uptime","id","groups","mount",
  "umount","fdisk","lsblk","du","tree","which","whereis","man","info","test","stat","file","xxd",
]);

/** 中文疑问信号：出现这些口语疑问词即视为自然语言提问（哪怕以命令词开头，如「git 怎么回滚」）。 */
const CJK_QUESTION =
  /(怎么|怎样|咋|如何|为什么|为何|是什么|什么意思|哪些|哪个|哪里|哪儿|多少|能否|能不能|可不可以|是否|有没有|是不是)/;

/** 行首显式提问标记表：分流规则与 UI 提示文案的单一事实来源，新增标记只改这里。 */
export const AI_TRIGGER_PREFIXES = ["?", "？", "问:", "问：", "/ask"] as const;

/** 由标记表拼行首正则：逐项转义；字母结尾的项补词界判定（保留原 `/ask\b` 语义）。 */
const TRIGGER_PREFIX_RE = new RegExp(
  "^(?:" +
    AI_TRIGGER_PREFIXES.map((p) =>
      p.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + (/[a-z0-9]$/i.test(p) ? "(?![a-z0-9])" : ""),
    ).join("|") +
    ")",
);

/** 提问信号枚举：每个成员对应一条正向信号，命中结果可作「为什么走了 AI」的调试/埋点标识。 */
export enum QuestionSignal {
  /** 行首显式标记（见 AI_TRIGGER_PREFIXES） */
  TriggerPrefix = "triggerPrefix",
  /** 句中含问号 */
  QuestionMark = "questionMark",
  /** 英文疑问词开头 */
  EnQuestionWord = "enQuestionWord",
  /** 句中含中文口语疑问词 */
  CjkQuestionWord = "cjkQuestionWord",
  /** 中文口语短句且首词不是已知命令（如「看一下nginx日志」） */
  CjkColloquial = "cjkColloquial",
}

/** 一条提问信号规则：对已 trim 的输入做命中判定。 */
interface QuestionRule {
  signal: QuestionSignal;
  hit: (s: string) => boolean;
}

/** 正向提问规则表（按优先级顺序短路匹配）。 */
const QUESTION_RULES: readonly QuestionRule[] = [
  { signal: QuestionSignal.TriggerPrefix, hit: (s) => TRIGGER_PREFIX_RE.test(s) },
  { signal: QuestionSignal.QuestionMark, hit: (s) => /[？?]/.test(s) },
  {
    signal: QuestionSignal.EnQuestionWord,
    // 词边界防 whatsapp/checkinstall 等误拦
    hit: (s) => /^(what|why|how|when|where|who|whom|whose|can|could|would|should|please|explain|tell me|find out)\b/i.test(s),
  },
  { signal: QuestionSignal.CjkQuestionWord, hit: (s) => CJK_QUESTION.test(s) },
  {
    signal: QuestionSignal.CjkColloquial,
    // 命令带中文参数的情况已由首词排除（如 `cd 工作项目/`）
    hit: (s) => /[\u4e00-\u9fff]/.test(s) && !SHELL_CMDS.has(s.split(/\s+/)[0].toLowerCase()),
  },
];

/**
 * 是否更像 shell 命令（应真实执行）。
 * - `!` 前缀强制 shell
 * - 带 `?`/`！` 或以疑问词（what/why/how…）开头 → 自然语言
 * - **含中文**：仅当「首词是已知命令 且 句中无中文疑问词」才当命令——这样 `cd 工作项目/`、
 *   `git commit -m "修复登录"` 能正常执行，而「看一下nginx」「git 怎么回滚」仍走提问。
 *   （旧规则「含中文一律当问句」会把中文目录/文件名误判成提问。）
 * - 首词在常见命令表 → shell
 * - 含管道/重定向/变量展开 → shell
 * - **含 CLI 选项（词首 `-x` / `--xxx`，常带 `=值`）→ shell**：真实命令最稳的结构信号，
 *   免去了「未列入白名单的长命令（如 mysql_config_editor … --host=…）被长度兜底误判为问句」
 * - 其余短英文 token → shell
 */
export function isShellLike(input: string): boolean {
  const s = input.trim();
  if (!s) return true;
  if (s.startsWith("!")) return true;
  if (/[？?！!]/.test(s)) return false;
  // 疑问句开头（词边界防 whatsapp/checkinstall 等误拦）→ 自然语言；先于选项判定，
  // 以免「what does --password do」这类反问因带 -- 而被当命令。
  if (/^(what|why|how|can|could|please|help|check|find out|tell me)\b/i.test(s)) return false;
  const first = s.split(/\s+/)[0];
  // 含中文：命令词 + 无疑问信号 → 命令（中文只是路径/文件名/提交信息）；否则当问句。
  if (/[\u4e00-\u9fff]/.test(s)) {
    return SHELL_CMDS.has(first.toLowerCase()) && !CJK_QUESTION.test(s);
  }
  if (SHELL_CMDS.has(first.toLowerCase())) return true;
  if (/[|;&<>$`]/.test(s)) return true;
  // 词首的 `-x` / `--xxx`（后接 = 或空格或结尾）= CLI 选项 → 判定为命令。
  if (/(^|\s)--?[A-Za-z][\w-]*(?:=|\s|$)/.test(s)) return true;
  if (/^[a-zA-Z0-9_\-./=:~ ]+$/.test(s) && s.length < 80 && s.split(/\s+/).length <= 12) return true;
  return false;
}

/**
 * 显式提问分流（默认策略）：返回输入命中的提问信号（规则表 QUESTION_RULES 首中即止；
 * 未命中任何正向信号返回 null。
 * 与 isShellLike 的根本区别：**拿不准时默认是命令**（返回 null），因此任何合法/陌生的
 * 英文命令都不可能被劫持。同时保留中文口语提问的识别，不牺牲中文直接提问的顺手度。
 * 注：Shift+Enter 强制提问由调用方（Terminal 回车处理）另行处理。
 */
export function matchQuestionSignal(input: string): QuestionSignal | null {
  const s = input.trim();
  if (!s) return null;
  return QUESTION_RULES.find((r) => r.hit(s))?.signal ?? null;
}

/** 显式分流判定：委托规则管道，保持调用方（Terminal 回车处理）所需的布尔语义。 */
export function looksLikeQuestion(input: string): boolean {
  return matchQuestionSignal(input) !== null;
}

/** UI 提示文案里展示的主标记（表首项）：改标记表后提示语自动跟随。 */
export const AI_TRIGGER_HINT: string = AI_TRIGGER_PREFIXES[0];

/** 把文案中的 `{trigger}` 占位替换为主标记（与 i18n 的 `{n}` 插值惯例一致）。 */
export function withTriggerHint(text: string): string {
  return text.replace("{trigger}", AI_TRIGGER_HINT);
}
