// 前端共享领域类型：与 Rust DTO 严格对齐（serde camelCase）。
// 安全红线：这里所有类型都**不含明文凭证**，凭证只以布尔标记或
// 一次性表单入参出现，提交后立即从组件 state 清空。

/** 认证方式（与后端 `AuthType` 字面量一致）。 */
export type AuthType = "password" | "privateKey" | "keyboardInteractive" | "auto";

/**
 * 会话/档案的协议类型（与后端 `SessionKind` 字面量一致）。
 * `ftp` 是明文协议，UI 上必须给出警告；`ftps` 走显式 TLS；`local` 是本机 shell。
 * `ftp`/`ftps` 没有终端只能用文件面板；`ssh`/`local` 有 PTY 终端。
 */
export type SessionKind = "ssh" | "ftp" | "ftps" | "local" | "telnet" | "serial" | "mosh";

/** 各协议的默认端口（表单切换协议时用来重置端口输入）；本地终端无端口，记 0。 */
export const DEFAULT_PORT: Record<SessionKind, number> = {
  ssh: 22,
  ftp: 21,
  ftps: 21,
  local: 0,
  telnet: 23,
  serial: 0,
  mosh: 0,
};

/** 协议在界面上的短标签（存 i18n 键，渲染处用 `t()` 解析；缺键回退中文）。 */
export const KIND_LABEL: Record<SessionKind, string> = {
  ssh: "kind.ssh",
  ftp: "kind.ftp",
  ftps: "kind.ftps",
  local: "kind.local",
  telnet: "kind.telnet",
  serial: "kind.serial",
  mosh: "kind.mosh",
};

/** 文件面板专属的会话类型（FTP/FTPS 无终端）；这是**协议分类**，不是能力判断。 */
export const kindIsFtp = (k: SessionKind): boolean => k === "ftp" || k === "ftps";

// 「这个会话能做什么」（有没有终端、有没有远端文件后端…）不在此处定义：
// 一律查 src/lib/capabilities.ts 的能力表。原先这里挂着 kindHasTerminal / kindHasFiles
// 两个谓词，能力表建立后已迁过去（避免同一判断两处各写一份而漂移）。

/**
 * 会话标签的显示名。本地终端的名字由后端写死为中文（`local.rs` 里 `label: "本地终端"`），
 * 属数据字段、不经 i18n，切英文时仍显中文；这里按当前语言实时解析。其余协议（SSH/FTP…）
 * 的 label 是主机名 / 档案名等真实数据，原样展示。`t` 传入组件的 `useI18n()` 结果以保证语言切换时重渲染。
 */
export const sessionLabel = (
  tab: { kind: SessionKind; label: string },
  t: (key: string) => string,
): string => (tab.kind === "local" ? t("topbar.local.title") : tab.label);

/**
 * 重名会话的名字消歧：一律带 `#n` 序号（含第一个）。
 *
 * 后端 label 是 `user@host`（`session.rs::label()`），同一台服务器开两个会话时名字**完全相同**；
 * 而选错的代价是「往哪个会话灌命令」，所以下标从 `#1` 起给全，
 * 不做「第一个不标、第二个起标」（那样要让用户自己推断谁是一号）。
 * 序号按传入数组次序（= 会话建立序）给，稳定可复现。
 *
 * 本函数不挑基数，基数选错就会造成同一条会话两处不同号——**调用请走 `lib/sessionName.ts`**
 * （它把口径定死为「全量真实会话」，并排除只存在于主窗口的占位标签）。
 */
export function disambiguateLabels(
  tabs: { id: string; kind: SessionKind; label: string }[],
  t: (key: string) => string,
): Map<string, string> {
  const total = new Map<string, number>();
  for (const x of tabs) {
    const k = sessionLabel(x, t);
    total.set(k, (total.get(k) ?? 0) + 1);
  }
  const nth = new Map<string, number>();
  const out = new Map<string, string>();
  for (const x of tabs) {
    const base = sessionLabel(x, t);
    if ((total.get(base) ?? 0) <= 1) {
      out.set(x.id, base);
      continue;
    }
    const n = (nth.get(base) ?? 0) + 1;
    nth.set(base, n);
    out.set(x.id, `${base} #${n}`);
  }
  return out;
}

/**
 * 目标列表按「侧栏连接分组」聚合：会话 → 档案 → 分组。
 *
 * 口径与侧栏完全一致（`Sidebar` 的 sections）：真实分组按 `groups` 次序（即 `sortOrder`），
 * 归不到组的（直连会话、本地终端、档案已被删）统一落末尾的「未分组」桶。
 * 空组省略：这一列答的是「发给谁」，没有收件人的组头只是噪声。
 * 只返回 id，名字/状态由调用方按 `tabs` 自己取（保持本函数纯粹、可测）。
 */
export function groupTargetsByConnection(
  tabs: { id: string; profileId: string | null }[],
  profiles: { id: string; groupId: string | null }[],
  groups: { id: string; name: string }[],
  ungroupedName: string,
): { id: string; name: string; tabIds: string[] }[] {
  const groupOfProfile = new Map(profiles.map((p) => [p.id, p.groupId]));
  const buckets = new Map<string, string[]>(groups.map((g) => [g.id, []]));
  const ungrouped: string[] = [];
  for (const tab of tabs) {
    const gid = tab.profileId ? (groupOfProfile.get(tab.profileId) ?? null) : null;
    const hit = gid ? buckets.get(gid) : undefined;
    (hit ?? ungrouped).push(tab.id);
  }
  const out = groups
    .map((g) => ({ id: g.id, name: g.name, tabIds: buckets.get(g.id) ?? [] }))
    .filter((b) => b.tabIds.length > 0);
  if (ungrouped.length > 0) out.push({ id: "", name: ungroupedName, tabIds: ungrouped });
  return out;
}

/** 已保存的连接档案。*/
export interface Profile {
  id: string;
  name: string;
  groupId: string | null;
  /** 引用的密钥库私钥（ssh_key.id）；null = 走内联路径/粘贴。握手时据此从库取私钥。 */
  privateKeyId: string | null;
  kind: SessionKind;
  host: string;
  port: number;
  username: string;
  authType: AuthType;
  /** 磁盘私钥路径（非敏感）；私钥内容与口令都在后端保险库。 */
  privateKeyPath: string | null;
  /** 跳板机档案 id（ProxyJump）；null = 直连。 */
  jumpProfileId: string | null;
  /** 串口线参数（仅 `kind=serial`）。 */
  serial?: SerialParams | null;
  startupDir: string | null;
  /** 登录后自动执行的命令（仅 SSH 类）；null = 不执行。 */
  startupCommand: string | null;
  /** 连接备注（自由文本，仅展示用）。 */
  notes: string | null;
  /** Agent 转发（-A）：仅 SSH 类生效，默认关。 */
  agentForward: boolean;
  /** X11 转发：仅 SSH 类生效，默认关（显示端需本机跑 X server，如 VcXsrv）。 */
  x11Forward: boolean;
  /** 心跳间隔（秒）；0 = 用默认。SSH 与 FTP 保活同源。 */
  keepaliveSecs: number;
  /** FTP 主动模式（PORT）：仅 FTP/FTPS 生效，默认关（= 被动）。 */
  ftpActiveMode: boolean;
  /** 建连超时（秒）；0 = 用默认。 */
  connectTimeoutSecs: number;
  /** 压缩优先（zlib）开关；仅 SSH 生效，默认关。 */
  compression: boolean;
  /** 连上即自动录制终端输出（脱敏落盘）。 */
  autoRecord: boolean;
  /** 录制存放子目录名（相对 app data dir）；null = `recordings/`。 */
  recordDir: string | null;
  /** 录制文件名模式（`%n %Y %m %d %H %M %S`）；null = 内置默认。 */
  recordFilePattern: string | null;
  /** 录制覆盖写（true）/ 追加写（false）。 */
  recordOverwrite: boolean;
  /** 录制每行加时间戳前缀。 */
  recordTimestampPrefix: boolean;
  /** 录制文件加密落盘（密钥归保险库）；默认关（明文），勾选后加密、程序内回放自动解密。 */
  recordEncrypt: boolean;
  /** 初始终端列数；0 = 用实时尺寸。 */
  termCols: number;
  /** 初始终端行数；0 = 用实时尺寸。 */
  termRows: number;
  /** 响铃样式覆盖；null = 跟随全局终端偏好。 */
  bellStyle: "none" | "visual" | "sound" | null;
  colorTag: string | null;
  sortOrder: number;
  createdAt: number;
  updatedAt: number;
  /** 保险库里是否已存密码/私钥口令。 */
  hasCredential: boolean;
  /** 保险库里是否已存私钥内容。 */
  hasPrivateKey: boolean;
}

/** 连接分组。*/
export interface Group {
  id: string;
  name: string;
  parentId: string | null;
  sortOrder: number;
  createdAt: number;
}

/** 连接表单入参：凭证字段只在提交瞬间存在。 */
export interface ProfileInput {
  id?: string;
  name: string;
  groupId?: string | null;
  /** 引用的密钥库私钥（ssh_key.id）；null/空 = 走内联路径/粘贴。 */
  privateKeyId?: string | null;
  kind?: SessionKind;
  host: string;
  port?: number;
  username: string;
  authType: AuthType;
  privateKeyPath?: string | null;
  /** 跳板机档案 id（ProxyJump）；null/空 = 直连。 */
  jumpProfileId?: string | null;
  /** 串口线参数（仅 `kind=serial`）。 */
  serial?: SerialParams | null;
  startupDir?: string | null;
  /** 登录后自动执行的命令（仅 SSH 类）。 */
  startupCommand?: string | null;
  /** 连接备注（自由文本）。 */
  notes?: string | null;
  /** Agent 转发（-A）：仅 SSH 类生效，缺省关。 */
  agentForward?: boolean;
  /** X11 转发：仅 SSH 类生效，缺省关。 */
  x11Forward?: boolean;
  /** 心跳间隔（秒）；缺省/0 = 用默认。 */
  keepaliveSecs?: number;
  /** FTP 主动模式（PORT）：仅 FTP/FTPS 生效，缺省关。 */
  ftpActiveMode?: boolean;
  /** 建连超时（秒）；缺省/0 = 用默认。 */
  connectTimeoutSecs?: number;
  /** 压缩优先（zlib）；仅 SSH 生效，缺省关。 */
  compression?: boolean;
  /** 连上即自动录制及其配置。 */
  autoRecord?: boolean;
  recordDir?: string | null;
  recordFilePattern?: string | null;
  recordOverwrite?: boolean;
  recordTimestampPrefix?: boolean;
  recordEncrypt?: boolean;
  /** 初始终端行列；0/缺省 = 用实时尺寸。 */
  termCols?: number;
  termRows?: number;
  /** 响铃样式覆盖；null/缺省 = 跟随全局。 */
  bellStyle?: "none" | "visual" | "sound" | null;
  colorTag?: string | null;
  sortOrder?: number;
  password?: string;
  privateKeyPem?: string;
  keyPassphrase?: string;
  /** true = 清除该连接已保存的凭证。 */
  clearCredential?: boolean;
}

/** 连接备份导出结果摘要（去凭证）。 */
export interface ExportReport {
  path: string;
  groups: number;
  profiles: number;
  /** 随备份带出的 AI 模型配置条数（分组导出恒为 0）。 */
  providers: number;
  /** 本次封装进加密包的凭证条目数；无凭证导出恒为 0。 */
  secrets: number;
}

/** 连接备份导入结果摘要。 */
export interface ImportReport {
  groupsCreated: number;
  profilesCreated: number;
  profilesSkipped: number;
  providersCreated: number;
  providersSkipped: number;
  /** 解密并回写保险库的凭证条目数（连接凭证 + 密钥库密钥 + AI Key 合计）。 */
  credentialsApplied: number;
  errors: string[];
}

/** 备份文件预检元信息（导入前判定是否需要主密码；不含任何凭证）。 */
export interface BackupInfo {
  version: number;
  groups: number;
  profiles: number;
  providers: number;
  hasCredentials: boolean;
}

/** 快速连接（不保存档案）入参。 */
export interface DirectConnect {
  kind?: SessionKind;
  host: string;
  port?: number;
  username: string;
  authType: AuthType;
  password?: string;
  privateKeyPem?: string;
  privateKeyPath?: string;
  /** 引用的密钥库私钥（ssh_key.id）；优先于粘贴/路径。 */
  privateKeyId?: string;
  keyPassphrase?: string;
  startupDir?: string;
  /** 登录后自动执行的命令（仅 SSH 类）。 */
  startupCommand?: string;
  /** Agent 转发（-A）：仅 SSH 类生效，缺省关。 */
  agentForward?: boolean;
  /** X11 转发：仅 SSH 类生效，缺省关（显示端需本机跑 X server）。 */
  x11Forward?: boolean;
  /** FTP 主动模式（PORT）：仅 FTP/FTPS 生效，缺省关（= 被动）。 */
  ftpActiveMode?: boolean;
  cols?: number;
  rows?: number;
  keepaliveSecs?: number;
  /** 串口线参数（仅 `kind=serial`）。 */
  serial?: SerialParams | null;
  /** 建连超时（秒）；0 = 用默认。 */
  connectTimeoutSecs?: number;
  /** 压缩优先（zlib）；仅 SSH 生效。 */
  compression?: boolean;
  /** 连上即自动录制及其配置。 */
  autoRecord?: boolean;
  recordDir?: string | null;
  recordFilePattern?: string | null;
  recordOverwrite?: boolean;
  recordTimestampPrefix?: boolean;
  recordEncrypt?: boolean;
  /** 初始终端行列；0 = 用实时尺寸。 */
  termCols?: number;
  termRows?: number;
}

/** 串口线参数（后端 `SerialParams`）：默认 8N1 @ 115200。*/
export interface SerialParams {
  baud?: number;
  dataBits?: number;
  stopBits?: number;
  parity?: "none" | "even" | "odd";
  flowControl?: "none" | "hardware" | "software";
}

/** 串口默认参数（表单初值）。 */
export const DEFAULT_SERIAL: Required<SerialParams> = {
  baud: 115200,
  dataBits: 8,
  stopBits: 1,
  parity: "none",
  flowControl: "none",
};

/** 会话状态（后端 `SessionState`）。 */
export type SessionState = "connected" | "disconnected" | "closed";

/** 一个已打开的会话标签（后端 `SessionDto`，不含凭证）。 */
export interface SessionDto {
  id: string;
  label: string;
  host: string;
  profileId: string | null;
  state: SessionState;
  /** 决定主区渲染终端还是文件面板占位。 */
  kind: SessionKind;
  /** RTT（毫秒）；无数据为 null。 */
  latencyMs?: number | null;
}

/** 会话协商快照（后端 `Negotiation`，会话信息面板数据源；不含凭证）。 */
export interface Negotiation {
  serverSoftware: string;
  kex: string;
  cipher: string;
  macClient: string;
  macServer: string;
  compressionClient: string;
  compressionServer: string;
  hostKeyType: string;
  hostKeyFingerprint: string;
  agentForward: boolean;
  x11Forward: boolean;
  keepaliveSecs: number;
  /** 跳板链（自近而远）`host:port` 列表；直连为空。 */
  jumps: string[];
}

/** 一条 FEAT 项（后端 `FtpFeat`）：名字 + 可选描述（如 `MLST` 后跟的 `type ...;size ...;`）。 */
export interface FtpFeat {
  name: string;
  param: string | null;
}

/**
 * FTP/FTPS 的协商快照（后端 `FtpInfo`）。
 *
 * 与 `Negotiation` 刻意分列：一边是 kex/算法/主机密钥，一边是 banner/FEAT/证书，
 * 字段语义不重叠。也没有 TLS 版本/套件与「被动模式」：后端拿不到（见 `FtpInfo` 文档注释），
 * 前端也就没有对应字段可填，不要在这里加回假的。
 */
export interface FtpInfo {
  /** 220 welcome 原文（只去了状态码前缀，未做软件名解析）。 */
  serverSoftware: string;
  /** `SYST` 响应体（如 `UNIX Type: L8`）；服务端不应答时为空串。 */
  system: string;
  /** `FEAT` 清单（按名字排序）；服务端不应答时为空数组。 */
  feat: FtpFeat[];
  /** FTPS：本次握手呈现的叶子证书 SHA-256 指纹；明文 FTP 为 null。 */
  certFingerprint: string | null;
}

/** `session_info` 命令负载：会话 DTO + 协商快照 + 活跃隧道。 */
export interface SessionInfo {
  session: SessionDto;
  /** 仅 SSH 连接有值；其余协议为 null。 */
  negotiation: Negotiation | null;
  /** 仅 FTP/FTPS 连接有值（与 `negotiation` 互斥）。 */
  ftpInfo?: FtpInfo | null;
  tunnels: ActiveTunnel[];
}

/** `session-state` 事件负载。 */
export interface SessionStateEvent {
  sessionId: string;
  state: SessionState;
  reason?: string;
}

/**
 * 一次建连的阶段事件（后端 `session-stage`）：预加载页据此画进度线与日志。
 * 以**前端生成的 `connectId`** 归集（握手成功前还没有 sessionId）。
 */
export interface SessionStageEvent {
  connectId: string;
  host: string;
  /** 粗粒度阶段名：resolve/tcp/kex/tls/auth/login/shell/ready/failed；未知值只展 detail。 */
  stage: string;
  status: "start" | "ok" | "fail";
  /** 人类可读明细（进可折叠日志）。 */
  detail: string;
}

/** 单个真实文件系统分区（后端 `DiskPartition`）：磁盘明细列表的一行。 */
export interface DiskPartition {
  mount: string;
  device: string;
  total: number;
  used: number;
}

/** 一次监控采样（后端 `MonitorSample`）：CPU/内存/磁盘/网卡的原始累计值。*/
export interface MonitorSample {
  cpuTotal: number;
  cpuIdle: number;
  memTotal: number;
  memAvailable: number;
  diskTotal: number;
  diskUsed: number;
  /** 各挂载点明细（已排除伪文件系统、按设备去重）；旧样本可能缺省。 */
  diskPartitions?: DiskPartition[];
  netRx: number;
  netTx: number;
  load1?: number;
  procs?: number;
  diskRead?: number;
  diskWrite?: number;
  /** 磁盘 IO 累计计数是否可得：BSD/macOS 探测打 `NA` → false，
   *  前端显「不支持」而非假装 0 B/s；缺省（旧样本/Linux）按 true 处理。 */
  ioSupported?: boolean;
  cpuCores?: [number, number][];
  source?: "local" | "remote" | string;
}

/**
 * 系统信息卡片（后端 `commands::system::SystemInfo`）：会话首次点开 ℹ️ 时采集一次，
 * 同会话内命中缓存；手动刷新传 `refresh=true` 强制重采。字段缺位时 UI 显 "—"，
 * 不猜。`probedAtMs` = 采集时刻 (unix ms)，卡片右上角显「×××前采集」。 */
export interface SystemInfo {
  os?: string;
  kernel?: string;
  arch?: string;
  distro?: string;
  shell?: string;
  cwd?: string;
  hostname?: string;
  uptimeSec?: number;
  cpuModel?: string;
  cpuCores?: number;
  memTotalKb?: number;
  memAvailKb?: number;
  diskTotalKb?: number;
  diskUsedKb?: number;
  ip?: string;
  probedAtMs: number;
}

/** 触发动作：匹配到输出后做什么。*/
export type TriggerAction = "alert" | "highlight" | "colorize";

/**
 * 着色语义色键（= xterm 主题里的 ANSI 槽位名）：落库存的是键而非十六进制，
 * 实际取色由 `themes.ts` 按当前主题解析——换主题时着色跟着换。
 */
export type TriggerColor = "red" | "green" | "yellow" | "blue" | "magenta" | "cyan";

/** 一条输出触发规则（后端 `TriggerRule`）。 */
export interface TriggerRule {
  id: string;
  name: string;
  /** 正则源串（前端 `RegExp` 编译）。 */
  pattern: string;
  action: TriggerAction;
  /** 着色色键，仅 `action === "colorize"` 生效。 */
  color: TriggerColor;
  enabled: boolean;
  sortOrder: number;
  createdAt: number;
  updatedAt: number;
}

/** 触发规则保存入参。 */
export interface TriggerInput {
  id?: string;
  name: string;
  pattern: string;
  action?: TriggerAction;
  color?: TriggerColor;
  enabled?: boolean;
  sortOrder?: number;
}

/** 一条会话录制文件。*/
export interface RecordingInfo {
  name: string;
  /** 文件所在目录的绝对路径（全局库跨目录聚合，回放/删除/打开按此定位）。*/
  dir: string;
  /** 来源主机名（起录时登记）；null = 遗留/外部产生、索引查不到。*/
  host: string | null;
  size: number;
  modifiedMs: number;
  /** 该文件是否正被会话实时写入（录制中）：前端标「录制中」，回放为截至此刻的快照。*/
  active: boolean;
}

/** 插件设置字段声明（manifest.settings）。*/
export interface PluginSettingSpec {
  key: string;
  type: "string" | "number" | "boolean" | "select";
  label: string;
  default: string;
  options?: string[];
}

/** 插件命令包条目。 */
export interface PluginCommand {
  id: string;
  title: string;
  template: string;
  description?: string;
  /** 执行所需会话能力档位（见 lib/capabilities 的 Capability）；缺省 "exec"，"none" = 不依赖会话通路。 */
  requires?: string;
}

/** 插件 manifest。*/
/** 插件工具栏按钮（manifest.buttons）。 */
export interface PluginUiButton {
  id: string;
  title: string;
  icon?: string;
  target?: "topbar" | "files" | "terminal";
  commandId: string;
}

/** 插件语料包声明（manifest.locales）：code=语言码，label=展示名，path=插件目录内相对 JSON 路径。 */
export interface PluginLocale {
  code: string;
  label: string;
  path: string;
}

/** 插件右键菜单项（manifest.menus）。 */
export interface PluginUiMenu {
  id: string;
  title: string;
  when?: "file" | "dir" | "terminal" | "always";
  commandId: string;
}

/** 插件贡献的 AI 技能（manifest.aiTools）：模型在会话里看到的就是一个普通工具，执行落本插件 wasm 导出。 */
export interface PluginAiTool {
  /** 包内唯一 id（启用开关以此为准）。 */
  id: string;
  /** LLM 可见工具名（[a-z0-9_]）。 */
  name: string;
  description?: string;
  /** 所需会话能力档位；缺省 "none"（技能多数不依赖会话通路）。 */
  requires?: string;
  /** 静态风险级：readonly（直通）| confirm（每次过审批卡）。 */
  risk?: "readonly" | "confirm";
  /** wasm 导出名（缺省 ai_tool_<name>）。 */
  export?: string;
  /** 参数 JSON Schema（OpenAI function.parameters 形态）。 */
  parameters?: Record<string, unknown>;
}

export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  description?: string;
  author?: string;
  permissions: string[];
  wasm?: string | null;
  commands: PluginCommand[];
  settings: PluginSettingSpec[];
  hooks: string[];
  timerSecs?: number;
  bridges?: string[];
  buttons?: PluginUiButton[];
  menus?: PluginUiMenu[];
  /** 插件贡献的 AI 技能（需同时声明 ai_tool 权限，安装时可见）。 */
  aiTools?: PluginAiTool[];
  /** 语料包贡献（可选）：启动时前端拉取 JSON 并注册进 i18n，与内置语言同权。 */
  locales?: PluginLocale[];
}

/** 首次连接的主机密钥确认请求（`host-key-unknown` 事件负载）。 */
export interface HostKeyPrompt {
  requestId: string;
  host: string;
  port: number;
  keyType: string;
  fingerprint: string;
}

/** 一条已信任的主机密钥记录。 */
export interface HostKeyRecord {
  host: string;
  port: number;
  keyType: string;
  keyBlob: string;
  fingerprint: string;
  createdAt: number;
}

/** SSH keyboard-interactive 单条提示（服务端下发）。 */
export interface KbdPromptItem {
  prompt: string;
  /** false = 机密输入（OTP/口令），前端按密码框渲染。 */
  echo: boolean;
}

/** keyboard-interactive 待作答请求（`ssh-kbd-request` 事件负载，不含任何答案）。 */
export interface KbdPrompt {
  requestId: string;
  host: string;
  /** 服务端横幅（协议 Name 字段，可能为空）。 */
  banner: string;
  instructions: string;
  /** 空数组 = banner-only，展示后提交即可。 */
  prompts: KbdPromptItem[];
}

/** FTPS 证书未通过系统信任链校验时的裁决请求（`ftps-cert-unknown` 事件负载）。 */
export interface FtpsCertPrompt {
  requestId: string;
  host: string;
  port: number;
  fingerprint: string;
  /** 证书主体 CN，自签设备可能为空串。 */
  subject: string;
  /** 有效期区间，如 `2026-08-20 ~ 2027-08-20`。 */
  validity: string;
  /** 拒因稳定码，前端本地化成文案。 */
  reasons: CertReason[];
}

/** 后端给出的「为什么常规校验不放行」稳定码。 */
export type CertReason =
  | "self_signed"
  | "legacy_version"
  | "expired"
  | "not_yet_valid"
  | "untrusted";

/** 一条已信任的 FTPS 服务端证书记录。 */
export interface FtpsCertRecord {
  host: string;
  port: number;
  fingerprint: string;
  subject: string;
  validity: string;
  createdAt: number;
}

/** 一条命令历史（后端入库前已脱敏）。 */
export interface HistoryEntry {
  id: string;
  sessionId: string | null;
  profileId: string | null;
  command: string;
  source: string;
  createdAt: number;
}

/** 应用信息（关于/安全状态展示）。 */
export interface AppInfo {
  version: string;
  vaultBackend: string;
  schemaVersion: number;
  dataDir: string;
  sessionCount: number;
}

/** 🛡 安全状态灯色（后端 `SecurityLight`，serde lowercase）。*/
export type SecurityLightLevel = "green" | "gray" | "yellow";

/** 默认 provider 概览（安全灯用，**不含任何凭证**）。 */
export interface ProviderBrief {
  name: string;
  model: string;
  kind: ProviderKind;
  /** 是否本地模型（数据不出机）。 */
  isLocal: boolean;
}

/**
 * 🛡 安全状态灯负载（后端 `SecurityStatus`）：AI 数据流向的实时仪表盘。
 * 顶栏 🛡 点开弹窗展示「当前模型 / 出网 / 脱敏 / 凭证 / 本次会话」，只读、不含凭证。
 */
export interface SecurityStatus {
  light: SecurityLightLevel;
  provider: ProviderBrief | null;
  /** AI 出网总开关（关=仅本地模型可用）。 */
  networkEnabled: boolean;
  /** 生效脱敏层级（云端已强制 ≥ strict）。*/
  sanitizeLevel: SanitizeLevel;
  /** 凭证后端：`os-keyring` | `aes-gcm-file`。 */
  vaultBackend: string;
  /** 凭证是否 OS 级隔离（keyring=true；加密文件回退=false=降级）。 */
  vaultIsolated: boolean;
  approvalMode: ApprovalMode;
  scanLevel: ScanLevel;
  sessionCount: number;
  /** 审计派生：累计拦截高危次数。 */
  auditBlocked: number;
  /** 审计派生：AI 命令提议总数。 */
  auditTotal: number;
}

/* ===== 文件浏览 ===== */

/**
 * 一个目录条目。本地与远端**同构**（后端共用 `FileEntry`），渲染逻辑不分叉。
 * 可选属性在不支持的协议/文件系统上为 `null`（如本地 Windows 没有 uid/gid/mode）。
 */
export interface FileEntry {
  name: string;
  path: string;
  isDir: boolean;
  isSymlink: boolean;
  size: number;
  modifiedMs: number | null;
  /** 权限位（八进制数值，如 0o644 = 420）。 */
  mode: number | null;
  /** `ls -l` 风格的 10 字符权限串，如 `-rw-r--r--`。 */
  perms: string | null;
  uid: number | null;
  gid: number | null;
  user: string | null;
  group: string | null;
}

/** 一次列目录的结果。`parent` 为 `null` 表示已在根，前端据此禁用「返回上级」。 */
export interface Listing {
  path: string;
  parent: string | null;
  entries: FileEntry[];
}

/** 文本文件内容（远程编辑器）。*/
export interface FileText {
  content: string;
  truncated: boolean;
  size: number;
}

/* ===== 传输队列 ===== */

/** 传输方向。 */
export type Direction = "upload" | "download";

/** 任务状态：前两个为进行中，后三个为终态。 */
export type TaskStatus = "pending" | "running" | "done" | "failed" | "canceled";

/** 一条传输任务行（不含任何凭证）。*/
export interface TaskRow {
  id: string;
  sessionId: string;
  /** 归属连接配置（profile）id；旧数据/直连会话为 null。 */
  profileId?: string | null;
  direction: Direction;
  remotePath: string;
  localPath: string;
  totalBytes: number;
  /** 已确认落盘字节数，也是断点续传的起点。 */
  doneBytes: number;
  status: TaskStatus;
  error: string | null;
  createdAt: number;
  updatedAt: number;
  /** 打包传输阶段；后端逐文件恒 `"transfer"`。可选：终端内 ZMODEM 本地行不带此字段，缺省按 transfer 处理。 */
  phase?: TransferPhase;
}

/** 目标已存在时的处理策略；`ask` 会让后端不入队、改抛冲突给前端裁决。 */
export type OnConflict = "ask" | "overwrite" | "skip" | "rename";

/** 提交传输的结果（后端 serde `tag = "kind"` 的内部标签联合）。 */
export type SubmitOutcome =
  | { kind: "queued"; task: TaskRow }
  | { kind: "conflict"; existingPath: string; existingBytes: number }
  | { kind: "skipped" };

/** `transfer-progress` 事件负载（后端已按 200ms 节流）。 */
export interface TransferProgressEvent {
  taskId: string;
  transferred: number;
  total: number;
  /** 字节/秒，本次运行的平均值（不含续传前已传部分）。 */
  speed: number;
}

/** `transfer-state` 事件负载。 */
export interface TransferStateEvent {
  taskId: string;
  status: TaskStatus;
  transferred: number;
  total: number;
  error: string | null;
}

/** 打包传输阶段（`compress` 本机打包 / `transfer` 传单包 / `extract` 远端解压）。 */
export type TransferPhase = "compress" | "transfer" | "extract";

/** `transfer-phase` 事件负载（打包阶段跃迁）。 */
export interface TransferPhaseEvent {
  taskId: string;
  phase: TransferPhase;
}

/** `bundle-fallback` 事件负载（打包失败已回退逐文件）。 */
export interface BundleFallbackEvent {
  taskId: string;
  reason: string;
}

/* ===== AI 助手 ===== */

/**
 * Provider 协议类型（后端 `ProviderKind`，serde kebab-case；Anthropic 库内口径为 `claude`）。
 * `ollama` 本地可无 API Key；`openai-compatible` 必须填 baseUrl（通义/DeepSeek/Kimi/智谱等）。
 */
export type ProviderKind = "openai" | "claude" | "ollama" | "openai-compatible";

/** Provider 在界面上的短标签。 */
export const PROVIDER_LABEL: Record<ProviderKind, string> = {
  openai: "OpenAI",
  claude: "Claude",
  ollama: "Ollama",
  "openai-compatible": "provider.compat",
};

/** 是否需要 API Key（Ollama 本地可空）。与后端 `ProviderKind::needs_key` 一致。 */
export const PROVIDER_NEEDS_KEY: Record<ProviderKind, boolean> = {
  openai: true,
  claude: true,
  ollama: false,
  "openai-compatible": true,
};

/**
 * 新建 provider 时预填的上下文窗口默认值（与后端 `ProviderKind::default_context_tokens` 一致）。
 * 取各协议的保守主流下限（宁小勿大：填大了会把超量上下文发给 provider 触发 400），用户可按实际模型上调。
 */
export const PROVIDER_DEFAULT_CONTEXT: Record<ProviderKind, number> = {
  openai: 128_000,
  claude: 200_000,
  ollama: 8_192,
  "openai-compatible": 64_000,
};

/**
 * 内容脱敏层级（后端 `SanitizeLevel`，serde lowercase）。
 * 云 provider 会被强制抬到 ≥ `strict`；`custom` 为偏执级（+高熵串+自定义正则）。
 */
export type SanitizeLevel = "strict" | "standard" | "loose" | "custom";

/** 脱敏层级的界面短标签。 */
export const SANITIZE_LABEL: Record<SanitizeLevel, string> = {
  strict: "sanitize.strict",
  standard: "sanitize.standard",
  loose: "sanitize.loose",
  custom: "sanitize.custom",
};

/**
 * 审批模式（后端 `ApprovalMode`，serde snake_case）。
 * `auto_readonly`=只读命令自动执行、写/高危走审批；`all`=全部命令都要人工审批。
 */
export type ApprovalMode = "auto_readonly" | "all";

export const APPROVAL_MODE_LABEL: Record<ApprovalMode, string> = {
  auto_readonly: "approval.autoReadonly",
  all: "approval.all",
};

/**
 * 命令风险扫描层级（后端 `ScanLevel`，serde lowercase）。
 * 判定命令风险、是否需审批的严格度：`loose` 多放行 / `standard` / `strict` 多审批。
 */
export type ScanLevel = "loose" | "standard" | "strict";

export const SCAN_LEVEL_LABEL: Record<ScanLevel, string> = {
  loose: "scan.loose",
  standard: "scan.standard",
  strict: "scan.strict",
};

/**
 * 终端上的 AI 开关（后端 `TerminalAi`，设置项 `ai.terminal_ai`）——逐终端的 shell/agent 档：
 * - `off`：终端纯 shell，Enter 永远直送 PTY，AI 不在终端落笔。
 * - `on`：命令与对话都能直接敲进终端，AI 回复/确认条都落在终端里。
 *
 * 只在 Agent 页签（rail）下生效：聊天页签是主画布总闸，停在那儿时终端一律视同 `off`
 * （Terminal.tsx 推导 + termPaint 硬禁），切回 Agent 页签原档自动恢复（落库值不变）。
 * 本开关**只管终端呈现**，不决定命令往哪儿执行——那是页签自己定的：Agent 页签下 AI
 * 命令恒打进终端 PTY（像手打一样回显执行），聊天页签恒后台静默 exec。
 * （历史：曾有独立的 `ai.exec_mode` 静默/当众轴，已删除。）
 */
export type TerminalAi = "off" | "on";

/** AI 主画布总开关（后端 `RailTab`，设置项 `ai.rail_tab`）：Agent=控制台主场（命令打进终端）；聊天=聊天窗主场（终端视同纯 shell、命令恒静默）。两档的落库值与终端开关互不牵动。 */
export type RailTab = "agent" | "chat";

/**
 * 可停靠面板 id（dockStore）：下栏与右侧栏两个停靠区之间拖拽换位。
 * 左侧连接栏固定，不参与停靠。
 *
 * `files` = 会话的远端文件浏览器：跟「聚焦会话」走（面板内容按 sessionId 取 fileStore 分片），
 * 不占主区——有终端的协议主区永远是终端，只有 FTP/FTPS（无 PTY）才把文件摆进主区。
 *
 * 面板对哪些会话可见不在这里定，由 `DOCK_META.visible` + 能力表过滤（见 lib/capabilities）。
 *
 * （发送工具不在此列：它是跨会话的全局工具，走独立工具窗口 `tools/`，不占停靠区——
 * 停靠区的开合按会话标签记忆，全局工具塞进来就得为它破例，破例比没有规则更糟。）
 */
export type DockPanelId = "ai" | "files" | "transfers" | "monitor" | "audit" | "session" | "process" | "services" | "history" | "record";

/** 全部可停靠面板（校验/默认布局用）。 */
export const DOCK_ALL: DockPanelId[] = ["ai", "files", "transfers", "monitor", "audit", "session", "process", "services", "history", "record"];

/**
 * 单个进程（后端 `commands::process::ProcInfo`，serde camelCase）。
 * `cpuPct` 在 Windows `tasklist` 通道拿不到时为 null（UI 显「—」）；`memKb` 为 RSS（KB）。
 */
export interface ProcInfo {
  pid: number;
  user: string;
  state: string;
  cpuPct: number | null;
  memKb: number;
  elapsedSec: number;
  cmd: string;
}

/** 进程排序键（`process_list` 的 sort_by 入参，服务端取热点前 N）。 */
export type ProcSort = "cpu" | "mem" | "pid";

/** 进程表可排序列（前端在已取回的行内做正/倒序，覆盖全部字段）。 */
export type ProcCol = "pid" | "user" | "state" | "cpu" | "mem" | "elapsed" | "cmd";

/**
 * 单个服务（后端 `commands::service::SvcInfo`，serde camelCase）。
 * systemd 机器全字段；SysV 降级机器 `enabled` 恒空、`sub` 仅运行时给、`state` 可能空串（未知，UI 显「?」）。
 */
export interface SvcInfo {
  /** 单元名（nginx.service）。 */
  name: string;
  /** LOAD 列：loaded / not-found / masked… */
  loaded: string;
  /** ACTIVE 列原词：active / inactive / failed（空串 = 判不了）。 */
  state: string;
  /** SUB 列：running / exited / dead… */
  sub: string;
  /** 自启态 enabled / disabled / static…（拿不到留空串）。 */
  enabled: string;
  /** MainPID，0 = 未知。 */
  pid: number;
}

/** 服务启停动作（`service_control` 的 action 入参，后端白名单同步）。 */
export type SvcAction = "start" | "stop" | "restart" | "reload";

/** 页签跨区拖拽的 dataTransfer 类型（区别于分屏拖标签的 text/session-id）。 */
export const DOCK_MIME = "text/dock-panel";

export const RAIL_TAB_LABEL: Record<RailTab, string> = {
  agent: "rail.agent",
  chat: "rail.chat",
};

export const RAIL_TAB_HINT: Record<RailTab, string> = {
  agent: "rail.agent.hint",
  chat: "rail.chat.hint",
};

/**
 * 回复详略档（后端 `ReplyVerbosity`，serde snake_case）：同时决定系统提示词的【回答风格】
 * 额度与终端落笔的行数上限。侧栏与历史始终全文，不受本项影响。
 */
export type ReplyVerbosity = "concise" | "standard" | "detailed";

export const REPLY_VERBOSITY_LABEL: Record<ReplyVerbosity, string> = {
  concise: "sc.verbConcise",
  standard: "sc.verbStandard",
  detailed: "sc.verbDetailed",
};

/**
 * 各档在终端里最多打几行（`0` = 不截）。终端打多少行属前端呈现层，
 * 后端只按同一档位控制 AI 说多少（`ReplyVerbosity::directive`）。
 */
export const REPLY_TERM_LINES: Record<ReplyVerbosity, number> = {
  concise: 8,
  standard: 20,
  detailed: 0,
};

/**
 * 「应用级问答」的保留会话 id：未连接任何会话时，首页 AI 聊天拿它当轮次/对话键。
 * 它不是真实会话（不在 sessionStore.tabs 里）：后端 `ai_send_message` 显式放行，
 * 工具面钉死为空能力集（只剩问答/出网/附件/知识库，无任何执行通路）。
 * 与 Rust 侧 `session::APP_CHAT_SESSION` 必须逐字一致。
 */
export const APP_CHAT_SESSION = "__app_chat__";

/** 一个 AI provider 配置（后端 `AiProvider` DTO，`key_ref` 被 skip，只回 `hasKey`）。 */
export interface AiProvider {
  id: string;
  name: string;
  kind: ProviderKind;
  baseUrl: string | null;
  model: string;
  /** 是否支持工具调用；`null` = 未探测。 */
  supportsTools: boolean | null;
  /** 是否支持图片输入（vision）；`null` = 未探测。设置页「测试」按钮顺带探测写入；
   *  发送带图时只有 `true` 放行（未探测/不支持一律拒发，不做转文字降级）。 */
  supportsVision: boolean | null;
  sanitizeLevel: SanitizeLevel;
  maxContextTokens: number;
  temperature: number | null;
  isDefault: boolean;
  extra: string | null;
  createdAt: number;
  updatedAt: number;
  /** 保险库里是否已存 API Key（不回显内容）。 */
  hasKey: boolean;
}

/** Provider 配置表单入参：`apiKey` 只在提交瞬间存在，写入保险库后即从组件 state 清空。 */
export interface ProviderInput {
  id?: string;
  name: string;
  kind: ProviderKind;
  baseUrl?: string | null;
  model: string;
  supportsTools?: boolean | null;
  /** 图片能力三态兜底：省略/`null` = 跟随探测结果；`true`/`false` = 强制改判。
   *  留这道口是因为兼容端点探测存在假阴风险，假阴会把图片功能永久锁死。 */
  supportsVision?: boolean | null;
  sanitizeLevel?: SanitizeLevel;
  maxContextTokens?: number | null;
  temperature?: number | null;
  isDefault?: boolean;
  extra?: string | null;
  /** 明文 API Key：仅入参；留空 = 保持原样（编辑时不重填即不动保险库）。 */
  apiKey?: string;
  /** true = 清除已存 Key。 */
  clearKey?: boolean;
}

/** 一段 AI 对话（按连接归档，后端 `AiConversation`）。 */
export interface AiConversation {
  id: string;
  connectionId: string | null;
  title: string | null;
  providerId: string | null;
  createdAt: number;
  updatedAt: number;
}

/** 对话消息角色（后端 `ChatRole`，serde lowercase）。 */
export type ChatRole = "system" | "user" | "assistant" | "tool";

/** 一条归档消息（脱敏后落库，后端 `AiChatMessage`）。 */
export interface AiChatMessage {
  id: string;
  conversationId: string | null;
  connectionId: string | null;
  sessionId: string | null;
  providerId: string | null;
  role: ChatRole;
  content: string | null;
  toolCallId: string | null;
  toolName: string | null;
  toolArgs: string | null;
  /** 本轮过程轨迹（`AiStep[]` 的 JSON 串，后端已脱敏/截断）；仅 assistant 行有值。 */
  steps: string | null;
  tokens: number | null;
  /** 末次模型调用实际发送的上下文规模（仅 assistant 行有值）；回看历史回填 ctx 环。 */
  contextUsed: number | null;
  /** 该轮 provider 上下文窗口总量（仅 assistant 行有值）；null/0 = 未知。 */
  contextWindow: number | null;
  /** 附件清单（`AiAttachment[]` 的 JSON 串）；仅 user 行有值，`null` = 无附件。
   *  存的是引用而非字节，图本体在数据目录的 attachments/ 下，缩略图经 `ai_get_attachment` 按需取；
   *  文档只存派生的抽取文本 `<id>.md`（不复制原件），经 `ai_get_attachment` 之外的通路投送。 */
  attachments: string | null;
  createdAt: number;
}

/** 附件大类（后端 `AttachKind`）。只有一个地方按它分支，加新格式不应新增一处 `if`。 */
export type AiAttachKind = "image" | "text" | "table" | "doc" | "binary";

/** 一条附件的归档元数据（后端 `AttachmentRef`，存于 `AiChatMessage.attachments` 的 JSON 数组里）。 */
export interface AiAttachment {
  id: string;
  mime: string;
  name: string | null;
  /** 解码后的真实字节数。 */
  bytes: number;
  /** 大类；旧行不带此字段，后端缺省解成 `image`（历史缩略图因此零改动）。 */
  kind?: AiAttachKind;
  /** 已归档抽取文本的字数；仅文档类有值，图片为 `null`/缺省。 */
  chars?: number | null;
  /** 抽取退化原因（`truncated` / `scanned` / `binary` / `oversize`…）：气泡出灰字说明，绝不静默丢内容。 */
  degraded?: string | null;
  /** 原件回指（仅文档类）：文档**不复制原件**，只存派生文本 + 这一组三元组。 */
  origin?: AiOriginRef | null;
}

/** 归档清单里一项的原文回指（后端 `OriginRef`）。 */
export interface AiOriginRef {
  /** 用户经系统对话框显式选中的本机路径（展示与回指用，后端不据此自动重读）。 */
  path: string;
  /** 摄取当时的字节数：与磁盘现值比对即知原件后来有没有改。 */
  size: number;
  /** 摄取当时的 mtime（秒）；0 = 取不到时间戳、不可比。 */
  mtime: number;
}

/** 随一轮消息发送的图片（后端 `TurnImage`）。`data` 为不含 `data:` 前缀的 base64。 */
export interface AiTurnImage {
  /** 已落盘的附件 id；本轮新图省略（后端归档时生成），「重试」原样重发时带上。 */
  id?: string | null;
  /** 只允许 image/png 与 image/jpeg（其余已在前端 canvas 归一重编码）。 */
  mime: string;
  name?: string | null;
  data: string;
}

/** 随一轮消息发送的本地文档（后端 `TurnFile`）：只递路径，读盘与抽取都在后端本轮开始时做。
 *  与 `AiTurnImage` 分道的理由：图片来自粘贴/拖拽只能递字节，文档本就在磁盘上，
 *  走 base64 往返是「Rust 读 → JS → Rust」的白白一圈。 */
export interface AiTurnFile {
  /** 本机绝对路径（系统文件对话框给出，本就是用户显式选择）。 */
  path: string;
  /** 展示名；缺省取路径末段。 */
  name?: string | null;
}

/** 待发区就地预探的结果（后端 `Probe`）：**只判不发** —— 只读头部判型、查体积与受保护文件。 */
export interface AttachmentProbe {
  path: string;
  name: string;
  bytes: number;
  mime: string;
  kind: AiAttachKind;
}

/** 聊天页签内的能力开关（后端 `ChatMode`）：agent=读写（全工具、写命令弹审批）/ chat=只读问答（能查不能改）。 */
export type ChatMode = "agent" | "chat";

/** 发送一条用户消息入参（后端 `SendMessage`）。 */
export interface AiSendMessage {
  sessionId: string;
  text: string;
  /** 省略 = 自动新建对话（响应回传其 id）。 */
  conversationId?: string | null;
  /** 省略 = 用默认 provider。 */
  providerId?: string | null;
  /** 对话模式；省略 = 智能体（agent）。 */
  mode?: ChatMode;
  /** 本轮主画布页签（与 `mode` 同源逐轮下发）：agent=命令打进终端 PTY / chat=恒后台静默 exec。
   *  后端库里的 `ai.rail_tab` 只是「最后一次显式持久化」的残留（页签在前端是按会话的视图态），
   *  执行通道必须看发起那一刻真正停着的页签。省略 = 后端回落库值。 */
  rail?: RailTab;
  /** 本轮单调递增的轮次序号（前端 `turnGen` 生成）：后端经 `ai_turn` 水位在每条步骤/流式事件上原样回带，
   *  前端据此丢弃「已终止轮次的迟到事件」（同会话跨轮串台兜底）。 */
  turn?: number;
  /** 前端解析后的「界面有效语言」展示名（如「日本語」）：驱动后端 `{{language}}` 宏，
   *  使 AI 回答语言跟随界面实际生效语言。省略/空 = 后端回落库里的 `ui.language`。 */
  lang?: string;
  /** AI 发起下载、需弹原生保存目录框时的**已本地化标题**（前端按 `dlg.aiDownloadPick` 解析后随轮下发）：
   *  使后端 tools.rs 不写死任何展示串。省略 = 用系统默认标题。 */
  downloadPickTitle?: string;
  /** 本轮随文的图片（前端已压缩归一）。省略/空 = 纯文本轮。 */
  images?: AiTurnImage[];
  /** 本轮随文的本地文档（只递路径，后端读盘抽取）。省略/空 = 无文档，行为与改前一致。 */
  files?: AiTurnFile[];
}

/** 「测试」按钮的回执（后端 `ProviderTest`）：连通结果 + 顺带探出的图片能力。 */
export interface ProviderTestResult {
  /** 连通回执（模型回的首若干字，或「连接成功」文案）。 */
  reply: string;
  /** 探测结论：`true` 支持图片 / `false` 不支持 / `null` 未探（连通就失败了）。 */
  vision: boolean | null;
  /** `vision === false` 时的端点报错摘要；其余情形 `null`。 */
  visionError?: string | null;
}

/** 📎 选文件入口读回的原始图片（后端 `StagedImage`），交前端压缩归一；不落盘、不写库。 */
export interface StagedImage {
  mime: string;
  /** 原始字节的 base64（不含 `data:` 前缀）。 */
  data: string;
  bytes: number;
  name: string;
}

/** 历史缩略图按需拉取的结果（后端 `AttachmentData`）。 */
export interface AttachmentData {
  mime: string;
  data: string;
}

/** 一轮对话的 token 用量与上下文占用（后端 `TurnUsage`，serde camelCase）。 */
export interface TurnUsage {
  /** 本轮全部模型调用的输入 token 之和（含工具往返）；旧归档数据为 0 = 未知。 */
  inputTokens: number;
  /** 本轮输出 token 之和。 */
  outputTokens: number;
  /** 入+出合计（落库 `tokens` 列的就是它）。 */
  totalTokens: number;
  /** 末次模型调用实际发送的上下文规模；0 = 未知。 */
  contextUsed: number;
  /** provider 上下文窗口总量；0 = 未配置，隐藏占用指示。 */
  contextWindow: number;
  /** `true` = provider 未回传 usage，数值为字符估算近似值。 */
  estimated: boolean;
}

/** 用量统计分组条目（后端 `UsageByKey`，按模型/分时/按天/按月共用）。 */
export interface UsageByKey {
  /** 展示名/轴标签（模型「名称 · model」/ 小时 / 日 / 月）。 */
  key: string;
  /** 该桶内对话轮数。 */
  count: number;
  /** token 合计。 */
  tokens: number;
}

/** 时间桶内按模型拆分的段（后端 `UsageSegment`，堆叠柱的一段）。 */
export interface UsageSegment {
  /** 模型展示名（与 byModel.key 同口径：「名称 · model」）。 */
  model: string;
  /** 该桶内该模型的对话轮数。 */
  count: number;
  /** 该桶内该模型的 token 合计。 */
  tokens: number;
}

/** 时间序列的一个桶（后端 `UsageBucket`，按天/月/时共用），内含按模型堆叠段。 */
export interface UsageBucket {
  /** 轴标签（小时 / 日 / 月）。 */
  key: string;
  /** 桶内对话轮数（各段之和）。 */
  count: number;
  /** 桶内 token 合计（各段之和）。 */
  tokens: number;
  /** 按模型拆分的段（token 降序）。 */
  segments: UsageSegment[];
}

/** 用量统计汇总（后端 `UsageStats`，`ai_usage_stats` 返回）。 */
export interface UsageStats {
  totalTokens: number;
  totalTurns: number;
  byModel: UsageByKey[];
  /** 近 30 天按小时分布（hour-of-day），按模型堆叠。 */
  byHour: UsageBucket[];
  /** 近 30 天逐日（只含有量的天），按模型堆叠。 */
  byDay: UsageBucket[];
  /** 近 12 月逐月（只含有量的月），按模型堆叠。 */
  byMonth: UsageBucket[];
}

/** 一轮对话的返回（后端 `ChatReply`）。 */
export interface ChatReply {
  conversationId: string;
  text: string;
  /** 本轮消耗的模型往返次数（1 = 直接答复未调工具）。 */
  rounds: number;
  /** 本轮自动沉淀的记忆条数。 */
  memoriesSaved: number;
  /** `true` = 本轮因达往返上限被优雅收尾（非自然答复）：出现「继续」按钮续跑。 */
  resumable: boolean;
  /** 本轮 token 用量与上下文占用（provider 真值累加；缺报时为估算）。 */
  usage?: TurnUsage;
  /** 运行期系统提示词体检告警（开发者改坏模板时回推）；`null`/缺省 = 正常。非阻断。 */
  promptWarning?: string | null;
  /** 本轮图片未能归档落盘的告警（答复已正常返回，不阻断）；`null`/缺省 = 图已存好或本轮无图。 */
  attachWarning?: string | null;
}

/** 系统提示词模板面板数据（后端 `PromptTemplate`，仅开发者模式可见）。 */
export interface PromptTemplate {
  /** 当前存的覆盖模板（空串 = 未覆盖，走内置默认）。 */
  template: string;
  /** 是否已被覆盖（true 时可「恢复默认」）。 */
  customized: boolean;
  /** 内置默认模板（编译进二进制、不可改，供展示/复制）。 */
  defaultTemplate: string;
}

/** 模板预览结果（后端 `PromptPreview`）。 */
export interface PromptPreview {
  /** 用样例动态块渲染后的成品。 */
  rendered: string;
  /** 体检告警（`null`/缺省 = 正常）。 */
  warning?: string | null;
}

/** 命令风险级别（后端 `RiskLevel`，serde SCREAMING_SNAKE_CASE）。 */
export type RiskLevel = "READ_ONLY" | "WRITE" | "DANGEROUS" | "UNKNOWN";

/** 策略裁决结果（后端 `Decision`，serde SCREAMING_SNAKE_CASE）。 */
export type AiDecision = "AUTO" | "NEEDS_APPROVAL" | "BLOCKED";

/** 策略裁决理由（后端 `Reason`）：`key` 供前端 i18n 本地化；`args` 为 `{占位符}` 插值值（程序名/子命令等）；
 *  `zh` 为词典未命中时的中文回落。 */
export interface ApprovalReason {
  key: string;
  args: Record<string, string>;
  zh: string;
}

/** AI 命令执行前的审批请求（`ai-approval-request` 事件负载，后端 `ApprovalPrompt`）。不含任何凭证。 */
export interface AiApprovalPrompt {
  requestId: string;
  sessionId: string;
  /** 发起本审批的轨（后端下发）：并发时当前页签未必等于发起轨，故为权威来源。 */
  track: "chat" | "agent";
  tool: string;
  summary: string;
  risk: RiskLevel;
  /** AI 说明为何要做这件事（授权卡主文案）。 */
  purpose: string;
  /** 策略给出的审批原因（后端下发 `Reason`：key 本地化 + args 插值 + zh 回落）。 */
  reason: ApprovalReason;
  /** 前端入队时置为 `track`（权威轨）：ApprovalBar 据此按轨过滤，避免跨页签泄漏。 */
  surface?: "chat" | "agent";
}

/** 澄清反问的一个候选项（后端 `QuestionOption`）。末位「其他（自行填写）」与 recommended 项的进卡预选由界面固定提供。 */
export interface AiQuestionOption {
  label: string;
  description?: string;
  recommended?: boolean;
}

/** 卡片内的一道子题（后端 `QuestionSub`）。 */
export interface AiQuestionSub {
  question: string;
  options: AiQuestionOption[];
  /** 是否允许多选。 */
  multi: boolean;
}

/** AI 遇到关键岔路时抛来的澄清反问（`ai-question-request` 事件负载，后端 `QuestionPrompt`）。
 * 一次可携带 1–4 道子题，前端渲染成一张可切换、统一提交的问题卡。不含凭证。 */
export interface AiQuestionPrompt {
  requestId: string;
  sessionId: string;
  /** 发起本题组的轨（后端下发）：并发时当前页签未必等于发起轨，故为权威来源。 */
  track: "chat" | "agent";
  questions: AiQuestionSub[];
  /** 前端入队时置为 `track`（权威轨）：聊天轨问的题只在聊天卡出现，Agent 轨只在 Agent 面板/终端浮层
   * 出现——避免切页签时同一 pending 题跨轨泄漏。 */
  surface?: "chat" | "agent";
}

/** 编排过程中的一个可展示步骤（后端 `AiStep`，serde `tag = "kind"`）。 */
export type AiStep =
  | { kind: "toolCall"; tool: string; summary: string; risk: RiskLevel; decision: AiDecision }
  | { kind: "toolResult"; tool: string; ok: boolean; text: string }
  | { kind: "plan"; steps: string[] }
  | { kind: "reflect"; message: string }
  | { kind: "reasoning"; text: string; durationMs: number }
  | { kind: "final"; text: string };

/** `ai-step` 事件负载（前端流式渲染工具调用时间线，后端 `StepEvent`）。 */
export interface AiStepEvent {
  sessionId: string;
  /** 归属轮次序号（发送时传入、后端原样回带）：与在途 `turns[sid].turnId` 比对，不符即丢弃（跨轮串台兜底）。 */
  turn: number;
  step: AiStep;
}

/* ===== 安全：审计 / 策略规则 / 设置 ===== */

/** 审计裁决结果（后端 `AuditDecision`，serde SCREAMING_SNAKE_CASE）。比 `AiDecision` 多了人工处置维度。 */
export type AuditDecision =
  | "AUTO"
  | "APPROVED"
  | "DENIED"
  | "BLOCKED"
  | "EDITED"
  | "EXPIRED";

/**
 * 一条审计记录（后端 `AuditEntry` DTO）：AI/用户命令的提议→裁决→执行→结果全链路留痕。
 * 只含**脱敏后**命令与输出摘要（哈希），绝无原始敏感输出/凭证。
 */
export interface AuditEntry {
  id: string;
  sessionId: string | null;
  /** 归属连接配置（profile）id；删服务器时级联清理，旧数据为 null。 */
  profileId?: string | null;
  /** `ai` | `user`。 */
  actor: string;
  proposedCommand: string | null;
  riskLevel: RiskLevel | null;
  decision: AuditDecision | null;
  /** 实际执行命令（编辑改判后可能不同于提议）。 */
  executedCommand: string | null;
  exitCode: number | null;
  /** 输出摘要 `sha256:<hex>`（不存原文）。 */
  outputDigest: string | null;
  aiMessageId: string | null;
  createdAt: number;
}

/** 策略规则类别（后端 `RuleKind`，serde snake_case）。 */
export type RuleKind = "readonly_whitelist" | "write_list" | "blacklist";

/** 规则命中动作（后端 `RuleAction`，serde lowercase）；为空则按 kind 隐含。 */
export type RuleAction = "allow" | "approval" | "block";

/** 一条用户自定义策略规则（后端 `PolicyRule` DTO）。规则只是命令模式，不含凭证。*/
export interface PolicyRule {
  id: string;
  kind: RuleKind;
  pattern: string;
  action: RuleAction | null;
  note: string | null;
  enabled: boolean;
  sortOrder: number;
  createdAt: number;
}

/** 规则保存入参：`id` 为空=新建，`Some`=编辑既有规则。 */
export interface PolicyRuleInput {
  id?: string;
  kind: RuleKind;
  pattern: string;
  action?: RuleAction | null;
  note?: string | null;
  enabled?: boolean;
  sortOrder?: number;
}

/** AI/安全设置（后端 `AiSettings`）：只存非敏感偏好，绝不含凭证。*/
export interface AiSettings {
  mem0Url?: string;
  mem0Key?: string;
  mem0User?: string;
  approvalMode: ApprovalMode;
  /** AI 出网总开关（关=仅本地模型可用）。*/
  networkEnabled: boolean;
  scanLevel: ScanLevel;
  /** 长期记忆总开关：关闭后既不召回注入也不自动提取。*/
  memoryEnabled: boolean;
  /** 自动从对话提取记忆：关=仅手动添加（仅在 memoryEnabled 时生效）。*/
  memoryAutoExtract: boolean;
  /** 记忆保留天数：`0`=永久；否则超期未命中即清理（如 90/180）。*/
  memoryRetentionDays: number;
  /** 终端上的 AI 开关：关=纯 shell / 开=命令与对话都能直接敲进终端（与右侧栏页签无关）。 */
  terminalAi: TerminalAi;
  /** 右侧栏页签：Agent 会话坞 / 聊天抽屉（纯呈现层选择，不改终端行为）。 */
  rail: RailTab;
  /** 界面语言：已注册语料 code（如 zh/en，可由插件扩展）或 "system"=跟随系统；只影响界面文案。 */
  uiLanguage: string;
  /** 回复详略：决定 AI 话多少与终端里最多打几行（侧栏/历史始终全文）。 */
  replyVerbosity: ReplyVerbosity;
  /** 明文凭证开关（默认开=宽松）：开=AI 可在命令里直接用你在对话中提供的密码（如 MySQL）；
   *  关=AI 拒绝内联明文密码、改走免密。不影响 SSH 登录凭证隔离。 */
  allowPlaintextPassword: boolean;
  /** 单轮最大工具往返数（默认 100，可设 8–200）：达到上限不再硬停，而是优雅收尾并出「继续」。 */
  maxToolRounds: number;
  /** 对话模式（默认 agent）：chat = 只读问答（只给只读工具、能查不能改）。 */
  chatMode: ChatMode;
  /** OS 系统通知横幅开关（默认开）：关=通知只走应用内 toast，不发系统横幅。 */
  osNotifications: boolean;
}

/** 设置局部更新入参（后端 `AiSettingsPatch`）：只传要改的字段。 */
export interface AiSettingsPatch {
  mem0Url?: string;
  mem0Key?: string;
  mem0User?: string;
  approvalMode?: ApprovalMode;
  networkEnabled?: boolean;
  scanLevel?: ScanLevel;
  memoryEnabled?: boolean;
  memoryAutoExtract?: boolean;
  memoryRetentionDays?: number;
  terminalAi?: TerminalAi;
  rail?: RailTab;
  uiLanguage?: string;
  replyVerbosity?: ReplyVerbosity;
  allowPlaintextPassword?: boolean;
  maxToolRounds?: number;
  chatMode?: ChatMode;
  osNotifications?: boolean;
}

/**
 * 自定义终端配色（后端 `CustomTheme`）：xterm `ITheme` 子集，从 JSON 文件导入。
 * `colors` 只含颜色字符串（background/foreground/cursor/black…brightWhite），**绝不含凭证**。
 */
export interface CustomTheme {
  name: string;
  colors: Record<string, string>;
}

/** 外观设置（后端 `AppearanceSettings`）：主题 / 字号 / 字体，只存非敏感偏好。 */
export interface AppearanceSettings {
  /** 选中主题 id：内置预设 id 或 `"custom"`（用 customTheme 的颜色）。 */
  theme: string;
  /** 终端字号（px），后端 clamp 到 8..=32。 */
  fontSize: number;
  /** 终端字体族（CSS font-family）。 */
  fontFamily: string;
  /** 用户导入的自定义主题（`theme === "custom"` 时生效）；未导入为 null。 */
  customTheme: CustomTheme | null;
}

/** 外观设置局部更新入参（后端 `AppearancePatch`）：只传要改的字段。 */
export interface AppearancePatch {
  theme?: string;
  fontSize?: number;
  fontFamily?: string;
}

/** 目录扫描发现的一个自定义主题（后端 `DiscoveredTheme`）：仅名称与文件路径，点选时才读色。 */
export interface DiscoveredTheme {
  name: string;
  path: string;
}

/** `theme_scan_dir` 返回（后端 `ThemeScanResult`）：扫描目录（供提示放文件）+ 校验通过的主题清单。 */
export interface ThemeScanResult {
  dir: string;
  themes: DiscoveredTheme[];
}

/** 终端行为偏好（后端 `TerminalSettings`）：滚动缓冲 / 光标 / 本地 shell / 交互行为。*/
export interface TerminalSettings {
  /** 滚动缓冲行数，后端 clamp 到 100..=100000。 */
  scrollback: number;
  /** 光标闪烁（xterm `cursorBlink`）。 */
  cursorBlink: boolean;
  /** 本地 shell 可执行路径；空串 = 平台默认。 */
  localShell: string;
  /** 选中文本即复制（onSelectionChange + 剪贴板）。 */
  copyOnSelect: boolean;
  /** 光标样式：block / bar / underline。 */
  cursorStyle: "block" | "bar" | "underline";
  /** 响铃样式：none / visual / sound。 */
  bellStyle: "none" | "visual" | "sound";
  /** 关键字着色总开关：按 colorize 类触发规则给终端输出上色（默认开）。 */
  keywordHighlight: boolean;
}

/** 终端偏好局部更新入参（后端 `TerminalPatch`）。 */
export interface TerminalSettingsPatch {
  scrollback?: number;
  cursorBlink?: boolean;
  localShell?: string;
  copyOnSelect?: boolean;
  cursorStyle?: "block" | "bar" | "underline";
  bellStyle?: "none" | "visual" | "sound";
  keywordHighlight?: boolean;
}

/** 记忆作用域（后端 `MemoryScope`，serde lowercase）：全局跨服务器 / 绑定单台服务器。 */
export type MemoryScope = "global" | "server";

/** 记忆来源（后端 `MemorySource`，serde lowercase）：自动提取 / 手动添加。 */
export type MemorySource = "auto" | "manual";

/**
 * 一条长期记忆（后端 `Memory` DTO）。
 * `content` 只存**脱敏后**的偏好/环境事实，绝不含密码/密钥/令牌。
 */
export interface Memory {
  id: string;
  scope: MemoryScope;
  /** `scope=server` 时绑定的连接 id；`global` 时为空。 */
  connectionId: string | null;
  content: string;
  source: MemorySource;
  /** 召回关键词（逗号分隔，可空）；相关性打分依据。 */
  keywords: string | null;
  createdAt: number;
  /** 最近命中时间（召回注入后刷新，用于淘汰排序）；从未命中为空。 */
  lastUsedAt: number | null;
}

/** 记忆保存入参：`id` 为空=新建，`Some`=编辑既有记忆。 */
export interface MemoryInput {
  id?: string;
  scope: MemoryScope;
  /** `scope=server` 时必填（绑定连接）；`global` 时忽略。 */
  connectionId?: string | null;
  content: string;
  keywords?: string | null;
}

/** 记忆作用域中文标签（UI 用）。 */
export const MEMORY_SCOPE_LABEL: Record<MemoryScope, string> = {
  global: "mem.scopeGlobal",
  server: "mem.scopeServer",
};

/** 记忆来源中文标签（UI 用）。 */
export const MEMORY_SOURCE_LABEL: Record<MemorySource, string> = {
  auto: "mem.srcAuto",
  manual: "mem.srcManual",
};

/** 记忆保留期选项（天；`0`=永久）。*/
export const MEMORY_RETENTION_OPTIONS: { value: number; label: string }[] = [
  { value: 0, label: "mem.retPermanent" },
  { value: 180, label: "mem.ret180" },
  { value: 90, label: "mem.ret90" },
  { value: 30, label: "mem.ret30" },
];

/**
 * 一条命令片段（后端 `Snippet` DTO）：常用命令模板，可含 `{{变量}}` 占位。
 * `content` **绝不含明文凭证**——敏感值用 `{{变量}}` 占位，插入时再填、值不落库。
 */
export interface Snippet {
  id: string;
  name: string;
  /** 自由分组标签（空 = 未分组）。 */
  groupName: string;
  /** 命令模板（可含 `{{变量}}` 占位）。 */
  content: string;
  /** 备注说明（可空）。 */
  description: string;
  /** 从 content 解析出的变量名（去重、按出现序）；后端计算，前端据此渲染填值表单。 */
  variables: string[];
  createdAt: number;
  updatedAt: number;
}

/** 片段保存入参：`id` 为空 = 新建，`Some` = 编辑既有片段。 */
export interface SnippetInput {
  id?: string;
  name: string;
  /** 自由分组标签（可空）。 */
  groupName?: string | null;
  content: string;
  description?: string | null;
}

/**
 * 一把已导入的 SSH 密钥（后端 `SshKey` DTO）：只含**非敏感元数据 + 公钥**。
 * 私钥材料只存后端保险库，**绝不经 IPC 回前端**；后端 `keyRef` 字段 `serde(skip)` 不下发。
 */
/** 密钥生成算法（后端 KeyGenInput.algorithm）。 */
export type KeyGenAlgorithm =
  | "ed25519"
  | "ecdsa"
  | "ecdsa-p384"
  | "ecdsa-p521"
  | "rsa"
  | "rsa-3072"
  | "rsa-4096";

export interface SshKey {
  id: string;
  /** 用户命名（列表展示）。 */
  name: string;
  /** 算法名（`ssh-ed25519` / `ssh-rsa` / `ecdsa-sha2-nistp256` …）。 */
  algorithm: string;
  /** SHA256 指纹（`SHA256:…`），供与服务器端核对。 */
  fingerprint: string;
  /** authorized_keys 格式公钥串（公钥本就公开，可安全展示 / 复制）。 */
  publicKey: string;
  /** 密钥自带注释（可空）。 */
  comment: string;
  /** 私钥是否带口令加密（据此提示连接时需口令）。 */
  encrypted: boolean;
  createdAt: number;
  updatedAt: number;
}

/**
 * 导入密钥入参（后端 `KeyInput`）：私钥来自粘贴内容或磁盘文件路径（二选一，内容优先）；口令可选。
 * 私钥内容 / 口令仅作为入参直送后端保险库，前端不留存、不回显。
 */
export interface KeyInput {
  name: string;
  /** 用户粘贴的私钥内容（OpenSSH / PEM）。 */
  privateKeyPem?: string | null;
  /** 或指向磁盘私钥文件路径。 */
  privateKeyPath?: string | null;
  /** 私钥口令（若私钥带口令加密）。 */
  passphrase?: string | null;
}

/**
 * 密钥生成器入参（后端 `KeygenInput`）：算法 + 可选口令。口令作为入参直送后端，前端不留存。
 */
export interface KeygenInput {
  algorithm?: KeyGenAlgorithm;
  passphrase?: string | null;
}

/**
 * 密钥生成器预览（后端 `KeygenPreview`）：公钥非敏感可展示 / 复制；
 * 私钥只留后端内存缓冲，凭 `token` 调保存接口写盘，**不经 IPC 回前端**。
 */
export interface KeygenPreview {
  token: string;
  algorithm: string;
  fingerprint: string;
  publicKey: string;
  encrypted: boolean;
}

/* ===== SSH 隧道 / 端口转发 ===== */

/** 隧道类型（后端 `TunnelKind`，serde lowercase）：本地 -L / 远程 -R / 动态 SOCKS -D。 */
export type TunnelKind = "local" | "remote" | "dynamic";

/** 隧道类型的界面短标签。 */
export const TUNNEL_KIND_LABEL: Record<TunnelKind, string> = {
  local: "tm.kindLocal",
  remote: "tm.kindRemote",
  dynamic: "tm.kindDynamic",
};

/**
 * 一条已保存的隧道转发规则（后端 `TunnelConfig`）：本地 ⇄ 远端 host:port 映射。
 * 只含地址端口，**绝无凭证**；开启时复用会话已认证连接（`ssh/tunnel.rs`）。
 */
export interface TunnelConfig {
  id: string;
  /** 归属连接档案 id（可空 = 不绑定）。 */
  profileId: string | null;
  name: string;
  kind: TunnelKind;
  /** 本地监听地址（默认 127.0.0.1，只本机可达）。 */
  localHost: string;
  /** 本地监听端口（0 = 系统分配随机端口）。 */
  localPort: number;
  /** 远端目标主机（经该 SSH 连接可达）。 */
  remoteHost: string;
  remotePort: number;
  enabled: boolean;
  createdAt: number;
  updatedAt: number;
}

/** 隧道配置保存入参：`id` 为空 = 新建，`Some` = 编辑既有配置。端口范围后端校验。 */
export interface TunnelInput {
  id?: string;
  profileId?: string | null;
  name: string;
  kind?: TunnelKind;
  localHost?: string | null;
  /** 本地端口；0 / 省略 = 系统分配随机端口。 */
  localPort?: number | null;
  remoteHost: string;
  remotePort: number;
  enabled?: boolean;
}

/**
 * 一条正在运行的转发（后端 `ActiveTunnel`）：端口是**实际**值（配置填 0 时为随机/服务器分配）。
 * 字段语义随 `kind`：`local` 本机监听→远端目标；`remote` 远端监听→本地目标；`dynamic` 仅本机 SOCKS。
 */
export interface ActiveTunnel {
  runId: string;
  /** 承载该转发的 SSH 会话 id。 */
  sessionId: string;
  name: string;
  kind?: TunnelKind;
  localHost: string;
  localPort: number;
  remoteHost: string;
  remotePort: number;
}

// ── 锁屏（应用级）──

/** 锁屏状态快照（后端单源，见 commands::lock）；锁定即时性靠 `lock-changed` 事件推送。 */
export interface LockStatus {
  locked: boolean;
  /** 是否已设置解锁 PIN（哈希存保险库；未设则不可锁）。 */
  pinSet: boolean;
  /** 空闲自动锁屏阈值（分钟），0 = 关闭。 */
  idleMinutes: number;
  /** 是否已设密保问答（忘记 PIN 的找回锚点）；true 才能走应用内重置。 */
  secqSet: boolean;
  /**
   * 「设置 PIN」弹窗开合——纯前端视图位（后端回包里恒为 undefined，
   * 靠 store 默认值与 patch 合并维护；refresh 会刻意保留它）。
   */
  setupOpen: boolean;
}

/** 解锁判定回包：错码/退避锁止是正常业务分支，不走错误通道（见后端模块头说明）。 */
export interface UnlockOutcome {
  ok: boolean;
  /** true = 处于试错退避锁止期，本次未做校验。 */
  blocked: boolean;
  /** `blocked` 时的剩余秒数；错码但未达锁止门槛则为 0。 */
  retryAfterSecs: number;
}

/**
 * 忘记 PIN 重置的判定回包：与 UnlockOutcome 同走返回值通道（错码/退避/无密保都是
 * 答题面板的正常分支，只有真 IPC/IO 异常才报错）。`status` 恒带当前快照，ok 时据此翻锁态。
 */
export interface ResetOutcome {
  ok: boolean;
  /** true = 处于试错退避锁止期，本次未做校验（展示倒计时）。 */
  blocked: boolean;
  /** `blocked` 时的剩余秒数。 */
  retryAfterSecs: number;
  /** true = 答案错误且未达锁止门槛。 */
  wrong: boolean;
  /** true = 未配置密保（遗留 PIN），无锚可验。 */
  noSecq: boolean;
  status: LockStatus;
}
