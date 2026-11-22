// 连接类型元数据：**「表单该怎么画、该收哪些参数」的唯一事实源**。
//
// 为什么要有这个文件：协议差异原先以 `kind === "ssh"` 一类裸判断散落在 ConnectForm
// 各处（页签显隐、认证方式开关、参数清洗、校验，约 20 处），每加一种协议都要翻遍
// 整个表单找判断点，是典型的漂移温床。收敛成一张表后，加协议 = 加一行表项，
// 表单只消费语义字段、不再各自写死协议名。
//
// 与 capabilities.ts 的分工：那边回答「会话建立后物理上能做什么」（运行能力），
// 这边回答「创建/编辑连接时表单怎么表现」（编辑语义）。两套语义刻意分文件存放，
// 改一边不需要动另一边。
import type { AuthType, SessionKind } from "../types";

/** 某个连接类型的表单编辑语义。字段即表单的全部差异点，协议特有的块由字段开开关。 */
export interface KindMeta {
  /** 有无「认证」页签与预认证材料（Telnet/串口/mosh 登录在终端里输，表单不收）。 */
  hasAuth: boolean;
  /** 可选的认证方式（顺序即按钮顺序）。FTP 家族只有密码；私钥/协商是 SSH 的概念。 */
  authModes: AuthType[];
  /** 用户名是否必填（仅 SSH；FTP 留空按匿名，Telnet/mosh 可选）。 */
  usernameRequired: boolean;
  /** 密码是否可留空（FTP 家族匿名登录；编辑态复用已存凭证不算留空，判断留在表单里）。 */
  passwordOptional: boolean;
  /** 仅 SSH 生效的参数组开关：跳板机 / 启动命令 / Agent 转发 / X11 转发。 */
  sshExclusive: boolean;
  /** FTP 家族标识：主动模式（PORT）开关与「仅文件面板」类提示的挂载依据。 */
  ftpExclusive: boolean;
  /** 有无心跳保活设置（SSH keepalive 与 FTP NOOP 保活同源；其余协议无此概念）。 */
  keepalive: boolean;
  /** 主机框是否为串口设备名（COM3 / ttyUSB0）：标签切换、隐藏端口、显示波特率与线参数。 */
  hostLabelIsDevice: boolean;
  /** 是否收集串口线参数（随档案 extra.serial 持久化）。 */
  serialParams: boolean;
  /** 有无「日志/录制」分区：协议是否有终端输出环可录制（FTP 家族走文件面板，无终端流）。 */
  hasRecord: boolean;
  /** 有无「终端」分区（初始行列 / Bell 覆盖）：协议是否有交互 PTY 终端。 */
  hasTerminal: boolean;
  /** 是否收集建连超时（仅经网络且走 TCP 握手的协议有意义；串口无此概念）。 */
  connectTimeout: boolean;
}

/** 缺省值：新增协议时只写与缺省不同的字段，避免整行复读造成两份真相。 */
const BASE: KindMeta = {
  hasAuth: true,
  authModes: ["password"],
  usernameRequired: false,
  passwordOptional: false,
  sshExclusive: false,
  ftpExclusive: false,
  keepalive: false,
  hostLabelIsDevice: false,
  serialParams: false,
  hasRecord: false,
  hasTerminal: false,
  connectTimeout: false,
};

const META: Record<SessionKind, KindMeta> = {
  ssh: {
    ...BASE,
    authModes: ["password", "privateKey", "auto"],
    usernameRequired: true,
    sshExclusive: true,
    keepalive: true,
    hasRecord: true,
    hasTerminal: true,
    connectTimeout: true,
  },
  // FTP/FTPS 只有 USER/PASS 一种认证；密码可空（匿名）；额外有主动模式。
  // 无 PTY 终端流（主区是文件面板），故不露录制/终端区。
  ftp: { ...BASE, authModes: ["password"], passwordOptional: true, ftpExclusive: true, keepalive: true },
  ftps: { ...BASE, authModes: ["password"], passwordOptional: true, ftpExclusive: true, keepalive: true },
  // 本机 shell 不经连接表单创建（顶栏直接开），表项仅保证 Record 全覆盖。
  local: { ...BASE, hasAuth: false, authModes: [] },
  telnet: { ...BASE, hasAuth: false, authModes: [], hasRecord: true, hasTerminal: true },
  mosh: { ...BASE, hasAuth: false, authModes: [], hasRecord: true, hasTerminal: true },
  // 串口：本机设备无账号概念，用户名输入框都不出现；有终端流可录，但无网络握手故不收建连超时。
  serial: {
    ...BASE,
    hasAuth: false,
    authModes: [],
    hostLabelIsDevice: true,
    serialParams: true,
    hasRecord: true,
    hasTerminal: true,
  },
};

/** 取某连接类型的表单元数据（只读；调用方不该改它）。 */
export function kindMetaOf(kind: SessionKind): KindMeta {
  return META[kind] ?? BASE;
}
