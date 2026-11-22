//! 会话注册表与生命周期。
//!
//! `session_id` 是前端与 AI 唯一可见的会话标识；连接参数（含认证材料）以 [`ConnectSpec`]
//! 形式只驻可信区内存，供断线重连使用，会话关闭即随 [`Secret`] 清零。
//!
//! 会话不一定带终端：[`SessionKind::Ftp`]/[`SessionKind::Ftps`] 只有文件传输能力，
//! 它们的 `tx`/`handle` 恒为 `None`，连接本体放在 [`SessionEntry::ftp`]。

use std::collections::HashMap;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use suppaftp::tokio::{AsyncFtpStream, AsyncRustlsFtpStream};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;

use aes_gcm::Aes256Gcm;

use crate::state::AppState;
use crate::vault::Secret;

/// 会话类型：决定建连走 SSH 握手还是 FTP 控制连接，
/// 也决定前端主区渲染终端还是文件面板。与库内 `kind` 列、前端字面量一致。
#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    /// SSH 终端，可选地复用同一条连接开 SFTP 子系统。
    #[default]
    Ssh,
    /// 明文 FTP：账号密码与文件内容都不加密。
    Ftp,
    /// 显式 TLS 的 FTP（AUTH TLS 在登录之前完成）。
    Ftps,
    /// 本地终端：用 portable-pty 在本机开一个 shell，无凭证、无主机密钥。
    Local,
    /// Telnet：明文 TCP + IAC 协商，无预认证（登录提示在终端里输）。
    Telnet,
    /// 串口：本机 COM/tty 透传，无凭证。
    Serial,
    /// Mosh：包装本机 `mosh` 客户端。
    Mosh,
}

impl SessionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::Ftp => "ftp",
            Self::Ftps => "ftps",
            Self::Local => "local",
            Self::Telnet => "telnet",
            Self::Serial => "serial",
            Self::Mosh => "mosh",
        }
    }

    /// 容错解析：库里出现未知值时退回 SSH，不让旧数据把整个档案列表带崩。
    pub fn parse(s: &str) -> Self {
        match s {
            "ftp" => Self::Ftp,
            "ftps" => Self::Ftps,
            "local" => Self::Local,
            "telnet" => Self::Telnet,
            "serial" => Self::Serial,
            "mosh" => Self::Mosh,
            _ => Self::Ssh,
        }
    }

    pub const fn is_ftp(self) -> bool {
        matches!(self, Self::Ftp | Self::Ftps)
    }

    /// 表单未显式填端口时的默认值。
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Ssh => 22,
            Self::Ftp | Self::Ftps => 21,
            Self::Telnet => 23,
            Self::Local | Self::Serial | Self::Mosh => 0,
        }
    }

    /// 建连是否不需要任何预认证材料（登录在终端里交互 / 串口本无账号）。
    pub const fn is_interactive_auth(self) -> bool {
        matches!(self, Self::Telnet | Self::Serial | Self::Mosh)
    }

    /// 该协议**物理上**具备的能力集合：有通路才算有，不按界面意愿给。
    ///
    /// 目前唯一的后端消费者是 [`crate::ai::tools::ToolRegistry::for_caps`]（按能力下发工具）。
    /// 前端 `src/lib/capabilities.ts` 的 `CAPS` 是本表的镜像，改一边就同步改另一边。
    pub const fn caps(self) -> Caps {
        match self {
            Self::Ssh => union_all(&[
                Caps::PTY,
                Caps::EXEC,
                Caps::FS,
                Caps::MONITOR,
                Caps::NEGOTIATION,
                Caps::ZMODEM,
                Caps::TUNNEL,
            ]),
            // FTP/FTPS：只有文件通路 + 一份协商快照。无 PTY、无 exec 通道（命令只能在控制连接上说），
            // 采样与端口转发都无从谈起。negotiation 指的是 [`FtpInfo`]（banner/FEAT/证书），
            // 与 SSH 那份同档不同结构，前端面板按 kind 分派渲染。
            Self::Ftp | Self::Ftps => union_all(&[Caps::FS, Caps::NEGOTIATION]),
            // 本地终端：命令在本机跑（PTY + 子进程 exec），但没有「远端」文件后端可浏览。
            Self::Local => union_all(&[Caps::PTY, Caps::EXEC, Caps::MONITOR]),
            // Telnet / 串口 / Mosh：只有终端。终端里能敲命令，但程序化 exec 通道不可靠
            // （无退出码、输出混在屏显里），故不给 EXEC——AI 的 `run_command` 因此不下发。
            Self::Telnet | Self::Serial | Self::Mosh => union_all(&[Caps::PTY]),
        }
    }
}

/// 「应用级问答」的保留会话 id：未连接任何会话时，首页 AI 聊天拿它当轮次/对话键。
///
/// 它**不注册进会话注册表**（`contains` 恒 false）：`ai_send_message` 显式放行，
/// `prepare_turn` 给它钉死 [`Caps::NONE`]——只下发不碰会话通路的工具（问答/出网/附件/知识库），
/// 不新增任何执行面。与前端 `src/types.ts` 的 `APP_CHAT_SESSION` 必须逐字一致。
pub const APP_CHAT_SESSION: &str = "__app_chat__";

/// 会话能力位标：[`SessionKind::caps`] 的值类型。
///
/// 手搓位标而不引 `bitflags`：只有这一处用，为一个类型加依赖不值（与全树极简依赖哲学一致）。
///
/// **档位含义与归属与前端 `src/lib/capabilities.ts` 的 `CAPS` 矩阵同源**：前端拿它决定「哪个面板值得
/// 占一个页签」，后端拿它决定「哪个工具真有通路」。两侧各自写 `kind == Ssh` 迟早漂移成
/// 「界面放行了、后端调不通」，故新增档位时两侧同加，并在 `tools.rs` 的单测里钉住逐协议归属。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Caps(u32);

/// 把若干档位并起来（`const fn` 里不能用 `Iterator::fold`，故手写循环）。
const fn union_all(list: &[Caps]) -> Caps {
    let mut acc = Caps::NONE;
    let mut i = 0;
    while i < list.len() {
        acc = acc.union(list[i]);
        i += 1;
    }
    acc
}

impl Caps {
    /// 无要求：任何会话都可用（纯交互、出网、只读元数据类工具）。
    pub const NONE: Self = Self(0);
    /// 交互式终端：有 PTY、能敲命令。
    pub const PTY: Self = Self(1 << 0);
    /// 非交互命令执行通道（SSH exec / 本机子进程）：拿得到 stdout 与退出码。
    /// 与 `PTY` 分开是有意为之——Telnet/串口有终端却没有可靠的 exec 通道。
    pub const EXEC: Self = Self(1 << 1);
    /// 远端文件系统后端（SSH 走 SFTP 子系统、FTP/FTPS 走控制连接），统一抽象见 [`crate::files::backend`]。
    pub const FS: Self = Self(1 << 2);
    /// 能采样主机负载（CPU/内存/磁盘/网卡）。本质派生自 `EXEC`，单列是为了让面板声明意图。
    pub const MONITOR: Self = Self(1 << 3);
    /// 建连时有协商快照可看（SSH 的 kex/算法；FTP 的 banner/FEAT/TLS，见 `FtpInfo`）。
    pub const NEGOTIATION: Self = Self(1 << 4);
    /// 终端里能走 Zmodem 收发（需 PTY + 远端 sz/rz）。
    pub const ZMODEM: Self = Self(1 << 5);
    /// 能做端口转发 / 跳板（仅 SSH）。
    pub const TUNNEL: Self = Self(1 << 6);

    /// 合并档位：`Caps::PTY.union(Caps::FS)` = 两条通路都要。
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `self` 是否覆盖 `need` 的全部档位（`need == NONE` 恒真）。
    pub const fn contains(self, need: Self) -> bool {
        self.0 & need.0 == need.0
    }

    /// 按档位名解析为单个档位；未知名返回 `None`。
    ///
    /// 名字与前端 `capabilities.ts` 的 `Capability` 联合、上面 `Debug` 打印名同源，
    /// 供插件清单 `commands[].requires` 查表门控（见 `commands::plugin`）。
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "pty" => Self::PTY,
            "exec" => Self::EXEC,
            "fs" => Self::FS,
            "monitor" => Self::MONITOR,
            "negotiation" => Self::NEGOTIATION,
            "zmodem" => Self::ZMODEM,
            "tunnel" => Self::TUNNEL,
            _ => return None,
        })
    }
}

impl fmt::Debug for Caps {
    /// 打印档位名而非裸位标：日志与断言失败信息要能一眼看出缺了哪档。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let named: [(&str, Caps); 7] = [
            ("pty", Caps::PTY),
            ("exec", Caps::EXEC),
            ("fs", Caps::FS),
            ("monitor", Caps::MONITOR),
            ("negotiation", Caps::NEGOTIATION),
            ("zmodem", Caps::ZMODEM),
            ("tunnel", Caps::TUNNEL),
        ];
        let list: Vec<&str> = named
            .iter()
            .filter(|(_, c)| self.contains(*c))
            .map(|(n, _)| *n)
            .collect();
        write!(f, "Caps({})", list.join("|"))
    }
}

/// 一次连接所需的全部参数。`Debug` 不含认证材料。
#[derive(Clone)]
pub struct ConnectSpec {
    pub kind: SessionKind,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: AuthMaterial,
    pub cols: u32,
    pub rows: u32,
    /// 登录后切入的初始目录。
    pub startup_dir: Option<String>,
    /// 登录后自动执行的命令（仅 SSH 类，首连注入 PTY）；None = 不执行。
    pub startup_command: Option<String>,
    /// 心跳间隔（秒）；0 表示用默认值。
    pub keepalive_secs: u64,
    /// Agent 转发（-A）：仅 SSH 类生效；true 时握手后请求转发并把本机 agent 桥接给远端。
    pub agent_forward: bool,
    /// X11 转发：仅 SSH 类生效；true 时握手后发 x11-req，远端 GUI 程序的回连由
    /// handler 桥接到本机 X server（显示端在本地）。
    pub x11_forward: bool,
    /// FTP 主动模式（PORT）：仅 FTP/FTPS 生效；false（默认）= 被动模式（PASV）。
    /// 主动模式下由客户端开监听端口、服务端回连，仅在对端网络策略要求时用（NAT 后通常不通）。
    pub ftp_active_mode: bool,
    /// 跳板机（ProxyJump）：经其 `direct-tcpip` 到本 host:port 再握手；`None` = 直连。
    /// 递归嵌套支持多级跳板（深度由调用方夹紧）。
    pub via: Option<Box<ConnectSpec>>,
    /// 串口线参数（仅 `kind=Serial`）；`host` 存设备名（COM3 / ttyUSB0）。
    pub serial: Option<crate::serial::SerialParams>,
    /// 建连超时（秒）；0 = 用默认（[`DEFAULT_CONNECT_TIMEOUT_SECS`]）。
    pub connect_timeout_secs: u64,
    /// 压缩优先（zlib）开关；仅 SSH 生效，false = none 优先（安全默认）。
    pub compression: bool,
    /// 连上即自动录制：建连成功后按下列配置开启 `TerminalRing` 录制。
    pub auto_record: bool,
    /// 录制存放子目录名（相对 app data dir）；None = `recordings/`。
    pub record_dir: Option<String>,
    /// 录制文件名模式（`%n %Y %m %d %H %M %S`）；None = 内置默认。
    pub record_file_pattern: Option<String>,
    /// 录制覆盖写（true）/ 追加写（false）。
    pub record_overwrite: bool,
    /// 录制每行加时间戳前缀。
    pub record_timestamp_prefix: bool,
    /// 录制文件加密落盘（密钥归 Vault）；默认开，程序内回放透明解密。
    pub record_encrypt: bool,
}

impl ConnectSpec {
    pub fn label(&self) -> String {
        format!("{}@{}", self.username, self.host)
    }
}

impl fmt::Debug for ConnectSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectSpec")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

/// 认证材料：明文只在握手路径使用。
#[derive(Clone)]
pub enum AuthMaterial {
    Password(Secret),
    /// 私钥 PEM 内容（来自保险库或用户粘贴）。
    PrivateKey {
        pem: Secret,
        passphrase: Option<Secret>,
    },
    /// 磁盘私钥文件路径（口令来自保险库）。
    PrivateKeyFile {
        path: String,
        passphrase: Option<Secret>,
    },
    /// 无预认证协议（Telnet）：登录提示在终端里交互输入，建连时不带凭证。
    Interactive,
    /// SSH keyboard-interactive（RFC 4252）：档案不带材料，连接时服务端逐轮出题、用户经
    /// `ssh-kbd-request` 事件作答（见 `ssh::kbd`）。
    KbdInteractive,
    /// 自动协商（SSH 新默认）：连接时先发 `none` 探测服务端可用方法，
    /// 按优先级 kbd > password（档案已存）自动选路。
    Auto {
        password: Option<Secret>,
    },
}

impl fmt::Debug for AuthMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::Password(_) => "Password",
            Self::PrivateKey { .. } => "PrivateKey",
            Self::PrivateKeyFile { .. } => "PrivateKeyFile",
            Self::Interactive => "Interactive",
            Self::KbdInteractive => "KbdInteractive",
            Self::Auto { .. } => "Auto",
        };
        write!(f, "{kind}(***)")
    }
}

/// 重连来源：决定重连时从哪里重新取得认证材料。
#[derive(Clone)]
pub enum Reconnect {
    /// 已保存档案：凭证按需回保险库取，内存不留副本。
    Profile(String),
    /// 一次性连接：认证材料仅驻可信区内存，会话关闭即释放。
    Direct(Box<ConnectSpec>),
    /// 本地终端：无凭证，“重连”即用同一 shell 配置重开一个本机进程。
    Local { shell: Option<String> },
}

impl Reconnect {
    pub fn profile_id(&self) -> Option<String> {
        match self {
            Self::Profile(id) => Some(id.clone()),
            Self::Direct(_) => None,
            Self::Local { .. } => None,
        }
    }
}

impl fmt::Debug for Reconnect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(id) => write!(f, "Reconnect::Profile({id})"),
            Self::Direct(_) => f.write_str("Reconnect::Direct(***)"),
            Self::Local { shell } => write!(f, "Reconnect::Local({shell:?})"),
        }
    }
}

/// 已认证 SSH 连接的控制句柄。
///
/// russh 的 `Handle` **不可 `Clone`**（内含独占的 reply 接收端），因此用 `Arc` 共享：
/// 会话 actor 持一份用于收尾 `disconnect`，注册表持一份用于**在既有连接上再开通道**
/// （SFTP 子系统）——复用已认证连接，绝不二次索取凭证。
pub type SshHandle = Arc<russh::client::Handle<crate::ssh::host_key::ClientHandler>>;

/// 已认证的 FTP 控制连接。
///
/// 明文与 TLS 两种底层流类型不同（方法签名一致但无法用同一个名字指代），
/// 因此用枚举分支包住：注册表只存 [`FtpHandle`]，`ftp` 模块内部再按分支实例化
/// 各自的 `FtpBackend<T>`，最后统一擦除成 `Arc<dyn FileTransfer>`。
///
/// 内层是 tokio 的异步锁：FTP 的所有命令都是 `&mut self`，且一条控制连接
/// 同时只容一个数据连接，串行化是协议要求而非实现偷懒。
#[derive(Clone)]
pub enum FtpHandle {
    Plain(Arc<AsyncMutex<AsyncFtpStream>>),
    Secure(Arc<AsyncMutex<AsyncRustlsFtpStream>>),
}

impl fmt::Debug for FtpHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Plain(_) => "FtpHandle::Plain(***)",
            Self::Secure(_) => "FtpHandle::Secure(***)",
        })
    }
}

/// 发往「会话 actor」任务的控制指令。
pub enum SessionCmd {
    /// 用户键入的原始字节，送入远端 PTY。
    Write(Vec<u8>),
    /// 终端尺寸变化（列, 行）。
    Resize(u32, u32),
    /// 用户主动关闭会话。
    Close,
    /// 终端内 ZMODEM：用户在原生文件框里选好了本地文件/目录，开始传输。
    ZmodemStart { local_path: String },
    /// 终端内 ZMODEM：用户取消了选择，放弃本次握手（actor 发 CAN 恢复终端）。
    ZmodemCancel,
}

/// 会话状态；`Closed` 仅出现在事件里（条目随即从注册表移除）。
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    Connected,
    Disconnected,
    Closed,
}

/// 会话状态变更事件：`connected` / `disconnected` / `closed`。
///
/// 定义放在注册表这一层而非 `ssh` 模块：FTP 会话没有 actor 协程，
/// 断线只能在文件命令返回连接级错误时就地发事件。
pub const EVENT_STATE: &str = "session-state";

/// 状态事件负载。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StateChanged<'a> {
    session_id: &'a str,
    state: SessionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
}

/// 广播会话状态变更。前端据 `sessionId` 切换标签上的状态点。
pub fn emit_state(app: &AppHandle, id: &str, state: SessionState, reason: Option<&str>) {
    let _ = app.emit(
        EVENT_STATE,
        StateChanged {
            session_id: id,
            state,
            reason,
        },
    );
}

/// 建连阶段事件：`session-stage`。
///
/// 与 [`EVENT_STATE`] 刻意分列——state 是「已存在会话」的生命周期跃迁，
/// stage 是「一次建连尝试」的过程流（预加载页据此画进度线与日志）。
/// 会话在握手成功前还没有 `session_id`，故按**前端生成的 `connect_id`** 归集。
pub const EVENT_STAGE: &str = "session-stage";

/// 阶段事件负载。`stage` 为粗粒度枚举名（前端映射进度标签），
/// `status` ∈ `start`/`ok`/`fail`，`detail` 为人类可读明细（进可折叠日志，也供后期扩展）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StageEvent<'a> {
    connect_id: &'a str,
    host: &'a str,
    stage: &'a str,
    status: &'a str,
    detail: &'a str,
}

/// 广播一个建连阶段。`connect_id` 为空（无预加载占位标签在等，如后台重连/工具连接）时静默跳过，
/// 因此各建连路径可无条件调用而不必到处判空。
pub fn emit_stage(
    app: &AppHandle,
    connect_id: &str,
    host: &str,
    stage: &str,
    status: &str,
    detail: &str,
) {
    if connect_id.is_empty() {
        return;
    }
    let _ = app.emit(
        EVENT_STAGE,
        StageEvent {
            connect_id,
            host,
            stage,
            status,
            detail,
        },
    );
}

/// 连接终止后的连带清理：缓存的 SFTP 子系统通道随连接一起失效，
/// 尚在跑的传输也没意义了（置取消位，worker 在下一块边界收尾），
/// 经该连接建立的本地端口转发隧道也全部关闭（监听套接字 + 在转的连接），
/// **在途的 AI 编排轮次也一并终止**（见 [`abort_ai_turn`]）。
pub fn teardown(app: &AppHandle, id: &str) {
    let st = app.state::<AppState>();
    st.sftp.forget(id);
    st.transfers.cancel_session(id);
    st.tunnels.close_session(id);
    // 首问预探测的环境缓存随连接失效（cwd/权限等都是这条连接的快照，不能残留到重连后）。
    st.ai_env.lock().unwrap().remove(id);
    // 系统信息卡片缓存同理随连接失效（hostname/uptime/cpu 等都是这一刻的快照，见 commands::system）。
    st.sys_info.lock().unwrap().remove(id);
    // 监控系统分流缓存同理（重连可能换机器，uname 结果不能残留）。
    st.monitor_os.lock().unwrap().remove(id);
    crate::files::cache::global().forget_session(id);
    abort_ai_turn(&st, id);
}

/// 会话结束时终止绑定在它上面的 AI 编排轮次（与 `ai_cancel_turn` 命令同一机制，幂等）。
///
/// 不加这一刀会死锁 AI 能力：审批/反问等待本身没有有效时限（`APPROVAL_TIMEOUT` 为一年，语义
/// = 等到用户裁决为止），连接一断那一轮就永远挂在 `request_approval`/`ask_user` 上。
///
/// chat 与 Agent 是同一条连接上的**两轨**（各自独立在途轮），故连接销毁时按两轨的 `turn_key`
/// 一并终止（见 [`crate::state::ai_turn_keys_for_session`]），挂起的审批/反问也按会话整体清掉。
/// 语义与手动「终止」一致：只停 AI 编排，不向远端发任何中断（已经跑起来的命令由用户自行处理）。
/// 同时清掉「总是允许」信任：随连接失效（不能跨一次连接残留到重连之后）。
pub fn abort_ai_turn(st: &AppState, id: &str) {
    for key in crate::state::ai_turn_keys_for_session(id) {
        if let Some(tx) = st.ai_cancel.lock().unwrap().remove(&key) {
            let _ = tx.send(());
        }
        // run_turn 被 drop 后不会自行取走轨迹/轮次号，这里补上，避免按会话累积泄漏。
        st.ai_trace.lock().unwrap().remove(&key);
        st.ai_turn.lock().unwrap().remove(&key);
    }
    st.ai_approvals.cancel_session(id);
    st.ai_approvals.untrust_session(id);
    // 挂在澄清反问上的编排轮次同样按会话收掉（drop 发送端 → run_turn 见通道关闭）
    st.ai_questions.cancel_session(id);
}

/// 摘除会话条目并广播 `closed`，避免注册表泄漏。
///
/// 与协议无关：SSH 会话在用户关掉断线标签时调用（actor 已退出），
/// FTP 会话没有 actor，关标签时直接走这里。
pub fn discard(app: &AppHandle, id: &str) -> bool {
    if app.state::<AppState>().sessions.remove(id).is_none() {
        return false;
    }
    teardown(app, id);
    emit_state(app, id, SessionState::Closed, None);
    true
}

/// 回前端的会话视图（不含任何凭证）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionDto {
    pub id: String,
    pub label: String,
    pub host: String,
    pub profile_id: Option<String>,
    pub state: SessionState,
    /// 前端据此决定主区渲染终端还是文件面板（FTP 会话没有 PTY）。
    pub kind: SessionKind,
    /// 建连/TCP 探测 RTT（毫秒）；无数据为 `None`。
    pub latency_ms: Option<u32>,
}

/// 每个会话保留的终端回滚字节上限（约 256KB：够几屏 + 上下文，单会话内存可控）。
pub const RING_CAP_BYTES: usize = 256 * 1024;

/// 终端输出的服务端环形缓冲（`{recent_terminal_snapshot}` 的数据源）。
///
/// SSH 会话 actor 每收到一块远端输出就 [`TerminalRing::push`]，只保留**最近**
/// [`RING_CAP_BYTES`] 字节；AI 的 `get_terminal_snapshot` 工具据此取会话近况喂系统提示词。
/// 存的是**原始字节**（含 ANSI 转义，前端 xterm 渲染所需），降噪与脱敏在读取侧/编排层做
///自带 `Mutex`：actor 高频 push 只在极短临界区内 memcpy，**不触碰会话
/// 注册表大锁**，与 [`SessionManager`] 的锁分离，避免每块输出都争用注册表。
///
/// 可选**录制**：开启后 `push` 把 VT 仿真还原后的**原样**文本追加到磁盘文件。
/// 录制默认不脱敏；机密保护落在存储层（权限/可选加密）。
pub struct TerminalRing {
    cap: usize,
    inner: Mutex<Vec<u8>>,
    recorder: Mutex<Option<Recorder>>,
}

/// 录制写入器：BufWriter 攒一批再刷盘，避免每块输出都 syscall。
struct Recorder {
    writer: std::io::BufWriter<std::fs::File>,
    path: PathBuf,
    /// 每块落盘前加时间戳前缀 `[%Y-%m-%d %H:%M:%S] `（UTC）。
    timestamp_prefix: bool,
    /// 轻量 VT 屏幕仿真：跨 push 保留当前行状态，把原地重画还原成最终行再落盘。
    screen: LineScreen,
    /// 加密数据 cipher：`Some` = 每行密封成分帧密文落盘（文件以 `SAILREC\x01` 头起）；
    /// `None` = 原样明文。密钥归 Vault，程序内回放透明解密。
    cipher: Option<Aes256Gcm>,
}

impl Recorder {
    /// 落一行：按 `timestamp_prefix` 拼 UTC 前缀 + 行内容 + `\n` 为完整载荷；加密模式
    /// 把载荷密封成一个分帧块写入，明文模式直接写字节。写失败只丢这一行，不影响终端。
    fn write_line(&mut self, line: &str) {
        let mut payload: Vec<u8> = Vec::new();
        if !line.is_empty() && self.timestamp_prefix {
            payload.extend_from_slice(
                format!("[{}] ", crate::commands::record::ts_prefix_utc()).as_bytes(),
            );
        }
        payload.extend_from_slice(line.as_bytes());
        payload.push(b'\n');
        match &self.cipher {
            Some(c) => {
                let _ = self
                    .writer
                    .write_all(&crate::commands::record::seal_chunk(c, &payload));
            }
            None => {
                let _ = self.writer.write_all(&payload);
            }
        }
    }

    /// 把 BufWriter 里攒的行刷给操作系统：每个数据块后调一次，使「你看到的」即刻落盘。
    /// 只到 OS 页缓存（非 fsync），足以抵御**进程意外退出**丢缓冲数据；同时让「录制中」文件
    /// 的回放能即时看到已录内容（否则攒不满 8KB 前磁盘近乎空）。写失败忽略（与逐行同口径）。
    fn flush(&mut self) {
        let _ = self.writer.flush();
    }
}

/// 单行缓冲上限：交互式 shell 折行前的逻辑行远小于此；防御异常光标定位撑爆内存。
const MAX_LINE: usize = 4096;

/// 录制侧的转义解析状态机档位（跨 push 保留，兼容块边界切断的转义序列）。
#[derive(Default, PartialEq)]
enum Mode {
    /// 普通文本：可打印字符入行，控制符按终端语义处理。
    #[default]
    Ground,
    /// 刚收到 ESC，等待判定后续类型。
    Esc,
    /// CSI（`ESC [ … 最终字节`）参数累积中。
    Csi,
    /// OSC（`ESC ] … BEL/ST`）内容吞弃中。
    Osc,
    /// OSC 内遇到 ESC，等待 ST 的 `\`。
    OscEsc,
    /// DCS（`ESC P … ST`）内容吞弃中。
    Dcs,
    /// DCS 内遇到 ESC，等待 ST 的 `\`。
    DcsEsc,
    /// 字符集指定（`ESC ( B` 之类）的下一字节。
    Charset,
}

/// 轻量 VT 屏幕仿真（仅录制路径用）：把 PTY 输出流里靠 `\r`、`\b`、光标定位、擦除行
/// 「原地重画」的内容还原成用户实际看到的**最终行**，替代直接 `strip_ansi` 删控制符后
/// 顺序拼接的做法——后者会把重画片段当成新文本追加，导致 `lls` 字符重复、提示符错位。
///
/// 覆盖范围（与需求一致）：`\r` 回车、`\b` 退格、`\n` 换行、`\t` 制表、擦除行
/// `ESC[K`/`ESC[1K`/`ESC[2K`、光标定位 `ESC[<n>G`/`ESC[<n>C`/`ESC[<n>D`。其余 CSI
/// （颜色 SGR、光标上下移、清屏等）与 OSC/DCS 序列按不可见吞掉——全屏程序（vim/htop）
/// 本就无法用纯文本忠实记录，此处只保证交互式 shell 的行重画正确。
#[derive(Default)]
struct LineScreen {
    /// 当前行缓冲（按列索引，可被光标定位覆写）。
    line: Vec<char>,
    /// 光标列（0 基）。
    col: usize,
    /// 转义解析档位，跨 push 保留。
    mode: Mode,
    /// CSI 参数字节流。
    params: String,
}

impl LineScreen {
    /// 喂入一块原始输出，返回本次「已定稿的整行」（不含换行符）。
    /// 未遇换行的当前行留在缓冲里，等下一次 `\n` 或 [`LineScreen::finish`] 收尾。
    fn feed(&mut self, input: &str) -> Vec<String> {
        let mut done = Vec::new();
        for c in input.chars() {
            match self.mode {
                Mode::Ground => match c {
                    '\x1b' => self.mode = Mode::Esc,
                    '\r' => self.col = 0,
                    '\n' => self.flush(&mut done),
                    '\u{8}' => self.col = self.col.saturating_sub(1),
                    '\t' => {
                        let next = ((self.col / 8) + 1) * 8;
                        self.pad(next);
                        self.col = next.min(MAX_LINE);
                    }
                    c if (c as u32) < 0x20 || c == '\u{7f}' => {}
                    c => self.put(c),
                },
                Mode::Esc => {
                    self.mode = match c {
                        '[' => {
                            self.params.clear();
                            Mode::Csi
                        }
                        ']' => Mode::Osc,
                        'P' => Mode::Dcs,
                        '(' | ')' | '#' | '%' => Mode::Charset,
                        // 其余单字符转义（ESC = / ESC > 等）：已消费，直接回地面
                        _ => Mode::Ground,
                    };
                }
                Mode::Charset => self.mode = Mode::Ground,
                Mode::Csi => {
                    let u = c as u32;
                    if (0x30..=0x3f).contains(&u) {
                        self.params.push(c);
                    } else if (0x20..=0x2f).contains(&u) {
                        // 中间字节：吞掉，不支持私有语义
                    } else if (0x40..=0x7e).contains(&u) {
                        self.apply_csi(c);
                        self.mode = Mode::Ground;
                    } else {
                        self.mode = Mode::Ground;
                    }
                }
                Mode::Osc => {
                    if c == '\x07' {
                        self.mode = Mode::Ground;
                    } else if c == '\x1b' {
                        self.mode = Mode::OscEsc;
                    }
                    // 否则吞掉 OSC 内容（shell-integration 标记、标题、cwd 等）
                }
                Mode::OscEsc => self.mode = Mode::Ground,
                Mode::Dcs => {
                    if c == '\x1b' {
                        self.mode = Mode::DcsEsc;
                    }
                }
                Mode::DcsEsc => self.mode = Mode::Ground,
            }
        }
        done
    }

    /// 用既有原始输出重建「当前未定稿的行」，作为录制起点。
    ///
    /// 录制常在提示符已打印、命令未敲时启动：若 `Recorder` 从空行开始，就会丢掉前面这段
    /// 提示符，回放首行从命令名突兀开始，这里要让起始行完整呈现。喂入 ring 里的全部
    /// 字节，丢弃 `feed` 返回的已定稿历史行，只保留最后这条未换行的行（通常是提示符）及其列位。
    fn prime(&mut self, raw: &[u8]) {
        let s = String::from_utf8_lossy(raw);
        let _ = self.feed(&s);
    }

    /// 收尾：返回尚未遇到换行的当前行（通常是最后一条提示符）；无内容则 `None`。
    fn finish(&mut self) -> Option<String> {
        let s: String = self.line.iter().collect();
        let s = s.trim_end().to_string();
        self.line.clear();
        self.col = 0;
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    /// 在当前列写入字符并右移一格；越界补空格，超上限丢弃（防御异常光标定位）。
    fn put(&mut self, c: char) {
        if self.col >= MAX_LINE {
            return;
        }
        if self.col >= self.line.len() {
            self.line.resize(self.col + 1, ' ');
        }
        self.line[self.col] = c;
        self.col += 1;
    }

    /// 把行缓冲补齐到 `to` 列（制表符跳过空格用）。
    fn pad(&mut self, to: usize) {
        let to = to.min(MAX_LINE);
        if to > self.line.len() {
            self.line.resize(to, ' ');
        }
    }

    /// 定稿当前行：剥尾部空格后入 `done`，复位到空行首列。
    fn flush(&mut self, done: &mut Vec<String>) {
        let s: String = self.line.iter().collect();
        done.push(s.trim_end().to_string());
        self.line.clear();
        self.col = 0;
    }

    /// 执行一条 CSI：只落地关心的行编辑序列，其余（颜色/光标上下/清屏等）忽略。
    fn apply_csi(&mut self, final_byte: char) {
        let n = self
            .params
            .split(';')
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        match final_byte {
            // ED/EL 中的「擦除行」：0=列到尾、1=首到尾、2=整行
            'K' => match n {
                0 => {
                    let keep = self.col.min(self.line.len());
                    self.line.truncate(keep);
                }
                1 => {
                    let upto = self.col.min(self.line.len());
                    for cell in self.line.iter_mut().take(upto) {
                        *cell = ' ';
                    }
                }
                _ => {
                    self.line.clear();
                    self.col = 0;
                }
            },
            // HPA：光标绝对列（1 基）
            'G' => self.col = n.saturating_sub(1).min(MAX_LINE),
            // CUF/CUB：光标右移/左移（缺省 1）
            'C' => self.col = (self.col + n.max(1)).min(MAX_LINE),
            'D' => self.col = self.col.saturating_sub(n.max(1)),
            _ => {}
        }
    }
}

impl TerminalRing {
    /// 新建空环（会话建档时调用）；`Arc` 一份入注册表供读取，一份交给 actor 持续 push。
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cap: RING_CAP_BYTES,
            inner: Mutex::new(Vec::new()),
            recorder: Mutex::new(None),
        })
    }

    /// 追加一块远端输出；超过上限即从头部丢弃最旧字节（保留最近 `cap` 字节）。
    /// 若正在录制，同步追加**原样**文本到文件（录制路径不做内容脱敏，见模块头）。
    pub fn push(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        {
            let mut buf = self.inner.lock().unwrap();
            buf.extend_from_slice(data);
            if buf.len() > self.cap {
                let overflow = buf.len() - self.cap;
                buf.drain(..overflow);
            }
        }
        // 录制：先经轻量 VT 屏幕仿真还原「用户实际看到的最终行」，再**原样**逐行落盘。
        // 直接 strip_ansi 会把 `\r`/光标定位/擦除行等原地重画的控制符删掉后顺序拼接，导致
        // `lls` 字符重复、提示符错位；仿真还原整行后与终端所见一致。写失败只丢这段。
        // 注：录制默认不做内容脱敏——启发式打码
        // 既会漏掉非标准形状的机密（假安全）又会误伤正常内容（破忠实），机密保护落在存储层。
        if let Some(rec) = self.recorder.lock().unwrap().as_mut() {
            let raw = String::from_utf8_lossy(data);
            for line in rec.screen.feed(&raw) {
                rec.write_line(&line);
            }
            // 每块输出后刷盘：兼顾意外退出不丢数据、录制中文件可即时回放。
            rec.flush();
        }
    }

    /// 开始录制到指定路径。`overwrite` = true 截断重写，false 追加（同路径重开即续写）。
    /// `timestamp_prefix` = true 时每块落盘前加 UTC 时间戳前缀。`cipher` = Some 时按行
    /// 密封加密落盘（文件头写 `SAILREC\x01`），None 时原样明文。
    pub fn start_recording(
        &self,
        path: PathBuf,
        overwrite: bool,
        timestamp_prefix: bool,
        cipher: Option<Aes256Gcm>,
    ) -> Result<(), std::io::Error> {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true);
        // 追加模式先探测既有大小：仅对新建/清空的文件写加密头，续写不重复写头。
        let existing_len = if overwrite {
            0
        } else {
            std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
        };
        if overwrite {
            opts.write(true).truncate(true);
        } else {
            opts.append(true);
        }
        let file = opts.open(&path)?;
        let mut writer = std::io::BufWriter::new(file);
        if cipher.is_some() && existing_len == 0 {
            writer.write_all(crate::commands::record::RECORD_MAGIC)?;
        }
        // 起始行补齐：用 ring 里已有的原始输出重建「当前未定稿的那一行」（通常是已打印的提示符），
        // 作为录制起点。否则在提示符已出、命令未敲时启动录制，回放首行会从命令名突兀开始。
        let mut screen = LineScreen::default();
        {
            let raw = self.inner.lock().unwrap().clone();
            screen.prime(&raw);
        }
        *self.recorder.lock().unwrap() = Some(Recorder {
            writer,
            path,
            timestamp_prefix,
            screen,
            cipher,
        });
        Ok(())
    }

    /// 停止录制并刷盘，返回文件路径；未在录制则 `None`。
    pub fn stop_recording(&self) -> Option<PathBuf> {
        self.recorder.lock().unwrap().take().map(|mut r| {
            // 收尾：把尚未遇到换行的当前行（通常是最后一条提示符）原样落盘，避免丢失。
            if let Some(line) = r.screen.finish() {
                if !line.is_empty() {
                    r.write_line(&line);
                }
            }
            let _ = r.writer.flush();
            r.path
        })
    }

    /// 是否正在录制。
    pub fn is_recording(&self) -> bool {
        self.recorder.lock().unwrap().is_some()
    }

    /// 当前正在写入的录制文件绝对路径；未在录制则 `None`。供录制库标记「录制中」条目。
    pub fn recording_path(&self) -> Option<PathBuf> {
        self.recorder.lock().unwrap().as_ref().map(|r| r.path.clone())
    }

    /// 取最近至多 `max_bytes` 字节的**原始**缓冲（给终端补帧；含 ANSI / CR/LF）。
    pub fn raw(&self, max_bytes: usize) -> Vec<u8> {
        let buf = self.inner.lock().unwrap();
        let start = buf.len().saturating_sub(max_bytes);
        buf[start..].to_vec()
    }

    /// 取最近至多 `max_bytes` 字节的快照（UTF-8 有损）；起点对齐到 char 边界，
    /// 避免把多字节字符切成半个而渲染出多余替换符。
    pub fn snapshot(&self, max_bytes: usize) -> String {
        let buf = self.inner.lock().unwrap();
        let mut start = buf.len().saturating_sub(max_bytes);
        // 跳过开头的 UTF-8 后续字节（10xxxxxx）
        while start < buf.len() && buf[start] & 0xC0 == 0x80 {
            start += 1;
        }
        String::from_utf8_lossy(&buf[start..]).into_owned()
    }

    /// 当前缓冲的字节数。
    // 环形缓冲自省：单测与后续会话状态展示使用；len 需与 is_empty 成对（clippy）。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 未显式配置时的心跳间隔（秒）；与 `ssh::DEFAULT_KEEPALIVE_SECS` 同一口径，
/// 放注册表层供会话信息面板展示生效值。
pub const DEFAULT_KEEPALIVE_SECS: u64 = 30;

/// 建连超时缺省值（秒）：档案未配置（0）时采用。与历史「russh 级 await 无超时」
/// 挂死隐患同源——给握手/PTY 一个明确上限，坏网络下不再无限等待。
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 20;

/// 把档案里的建连超时配置折算成生效秒数（0 = 未配置用默认）。
pub const fn connect_timeout_secs(configured: u64) -> u64 {
    if configured == 0 {
        DEFAULT_CONNECT_TIMEOUT_SECS
    } else {
        configured
    }
}

/// 把档案里的心跳配置折算成生效值（0 = 未配置用默认）。
pub const fn keepalive_secs(configured: u64) -> u64 {
    if configured == 0 {
        DEFAULT_KEEPALIVE_SECS
    } else {
        configured
    }
}

/// 从服务器标识串（`SSH-2.0-OpenSSH_8.0`）提取软件部分；畸形输入回退原文。
/// 会话信息面板展示用，纯字符串处理故可直接单测。
pub fn parse_server_software(sshid: &str) -> String {
    sshid
        .strip_prefix("SSH-2.0-")
        .or_else(|| sshid.strip_prefix("SSH-1.99-"))
        .unwrap_or(sshid)
        .to_string()
}

/// 回前端的会话协商快照（会话信息面板数据源；不含任何凭证/密钥材料）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Negotiation {
    /// 服务器软件标识（已剥 `SSH-2.0-` 前缀）
    pub server_software: String,
    pub kex: String,
    pub cipher: String,
    pub mac_client: String,
    pub mac_server: String,
    pub compression_client: String,
    pub compression_server: String,
    pub host_key_type: String,
    pub host_key_fingerprint: String,
    /// Agent 转发（-A）：客户端已发请求（服务端应答协议上不可见，前端文案诚实归因）
    pub agent_forward: bool,
    /// X11 转发：客户端已发 x11-req（同 Agent，服务端应答协议上不可见）
    pub x11_forward: bool,
    /// 生效心跳间隔（秒）
    pub keepalive_secs: u64,
    /// 跳板链（ProxyJump，自近而远）`host:port` 列表；直连为空
    pub jumps: Vec<String>,
}

impl Negotiation {
    /// 握手槽位 + 连接参数 → 展示快照。槽位为空（理论上仅当服务器未发 kex，
    /// 不会发生）时算法字段留空串，前端按缺字段显示“—”。
    pub fn build(negotiated: Option<crate::ssh::host_key::Negotiated>, spec: &ConnectSpec) -> Self {
        let (
            kex,
            cipher,
            mac_client,
            mac_server,
            compression_client,
            compression_server,
            server_software,
            host_key_type,
            host_key_fingerprint,
        ) = match negotiated {
            Some(n) => (
                n.kex,
                n.cipher,
                n.mac_client,
                n.mac_server,
                n.compression_client,
                n.compression_server,
                parse_server_software(&n.remote_sshid),
                n.host_key_type,
                n.host_key_fingerprint,
            ),
            None => Default::default(),
        };
        Self {
            server_software,
            kex,
            cipher,
            mac_client,
            mac_server,
            compression_client,
            compression_server,
            host_key_type,
            host_key_fingerprint,
            agent_forward: spec.agent_forward,
            x11_forward: spec.x11_forward,
            keepalive_secs: keepalive_secs(spec.keepalive_secs),
            jumps: jump_chain(spec),
        }
    }
}

/// 跳板链的 `host:port` 展示列表（自近而远，与 `via` 递归方向一致）。
fn jump_chain(spec: &ConnectSpec) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = spec.via.as_deref();
    while let Some(v) = cur {
        out.push(format!("{}:{}", v.host, v.port));
        cur = v.via.as_deref();
    }
    out
}

/// FTP/FTPS 的协商快照（会话信息面板数据源；不含凭证）。
///
/// 与 [`Negotiation`] **刻意不合并**：一边是 kex/算法/主机密钥，一边是 banner/FEAT/证书，
/// 字段语义毫无重叠，合成一个结构只会造出一半永远是 `None` 的空位（前端还得逐字段判空）。
///
/// 也没有 `tls_version`/`tls_cipher`：suppaftp 12 的控制通道只透出 TCP 句柄
/// （`ControlSocket` 的 `Deref` 目标是 `TcpStream`），拿不到协商后的 TLS 会话。与其放两个恒为
/// `None` 的假字段，不如只报真采到的四项——同理也没放「被动模式」与「传输类型」：当前实现固定
/// PASV、类型逐次传输时现设，写进展示面板等于把常量伪装成实况。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FtpInfo {
    /// 220 welcome 原文：只去掉 `220 ` / `220-` 状态码前缀，**不做任何软件名解析**
    /// （各家 banner 格式互不相容，猜必错；proftpd/pure-ftpd/IIS 各写各的）。
    pub server_software: String,
    /// `SYST` 响应体（例如 `UNIX Type: L8`）；服务端不应答时为空串。
    pub system: String,
    /// `FEAT` 清单（按名字排序）。服务端是否声明 `UTF8` 直接在此可见——FTP 文件名乱码的第一手证据。
    pub feat: Vec<FtpFeat>,
    /// FTPS：本次握手服务端**呈现**的叶子证书 SHA-256 指纹（不是已固定那一条，两者不符时根本连不上）；
    /// 明文 FTP 恒为 `None`。
    pub cert_fingerprint: Option<String>,
}

/// 一条 FEAT 项：名字 + 可选描述（如 `MLST` 后跟的 `type ...;size ...;`）。名字按服务端原样保留。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FtpFeat {
    pub name: String,
    pub param: Option<String>,
}

/// 注册表中的一条会话。
pub struct SessionEntry {
    pub id: String,
    pub label: String,
    pub host: String,
    pub profile_id: Option<String>,
    pub state: SessionState,
    pub kind: SessionKind,
    /// `None` = 已断开（等待重连）；FTP 会话恒为 `None`（无终端 actor）。
    pub tx: Option<mpsc::Sender<SessionCmd>>,
    /// 已认证连接句柄；断线后置 `None`（此时无法再开 SFTP 通道）。
    pub handle: Option<SshHandle>,
    /// 跳板机连接句柄链（ProxyJump，自近而远）：目标流挂在这些通道上，**必须随会话保留**。
    pub jumps: Vec<SshHandle>,
    /// FTP/FTPS 会话的控制连接；SSH 会话恒为 `None`。
    pub ftp: Option<FtpHandle>,
    /// FTP 连接的代数：每次（重）连自增，心跳任务据此辨认自己押注的那条连接是否仍在。
    /// 非 FTP 会话恒为 0。
    pub ftp_epoch: u64,
    pub reconnect: Reconnect,
    /// 终端输出环形缓冲（AI 快照数据源）；SSH 会话由 actor 持续填充，FTP 会话恒空。
    pub ring: Arc<TerminalRing>,
    /// 目标端口（RTT 探测用）；本地终端 / 串口为 0。
    pub port: u16,
    /// 最近一次 RTT（毫秒）。
    pub latency_ms: Option<u32>,
    /// 协商快照（会话信息面板）；仅 SSH 会话有值，其余协议恒 `None`。
    pub negotiation: Option<Negotiation>,
    /// FTP 协商快照（与 `negotiation` 分开放：两个协议的「协商」根本没有共同字段）；仅 FTP/FTPS 有值。
    pub ftp_info: Option<FtpInfo>,
}

impl SessionEntry {
    pub fn dto(&self) -> SessionDto {
        SessionDto {
            id: self.id.clone(),
            label: self.label.clone(),
            host: self.host.clone(),
            profile_id: self.profile_id.clone(),
            state: self.state,
            kind: self.kind,
            latency_ms: self.latency_ms,
        }
    }
}

/// 全局会话注册表：`session_id → 会话`。
/// `Mutex` 只在取出/写入的瞬间持有，**绝不跨 `await`**。
pub struct SessionManager {
    entries: Mutex<HashMap<String, SessionEntry>>,
    seq: AtomicU64,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            // 随机起点：session id 会随传输任务落库，若每次重启都从 s0 计数，
            // 新会话会与库内旧 session id 撞车（刚连接就“冒出”上次的传输历史，
            // 重传还会报「会话不存在或已关闭」）；1e9 量级起点 + 递增保可读，跨重启碰撞可忽略。
            seq: AtomicU64::new(uuid::Uuid::new_v4().as_u128() as u64 % 1_000_000_000 + 1_000),
        }
    }
}

impl SessionManager {
    pub fn next_id(&self) -> String {
        format!("s{}", self.seq.fetch_add(1, Ordering::Relaxed))
    }

    pub fn insert(&self, entry: SessionEntry) {
        self.entries.lock().unwrap().insert(entry.id.clone(), entry);
    }

    /// clone 出发送端；`None` 表示会话不存在或已断开。
    pub fn sender(&self, id: &str) -> Option<mpsc::Sender<SessionCmd>> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .and_then(|e| e.tx.clone())
    }

    /// 取已认证连接句柄，用于在既有连接上开新通道（SFTP 子系统）。
    /// `None` = 会话不存在或已断开。
    pub fn handle(&self, id: &str) -> Option<SshHandle> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .and_then(|e| e.handle.clone())
    }

    /// 取终端输出环形缓冲（AI `get_terminal_snapshot` 用）；`None` = 会话不存在。
    ///
    /// 克隆出 `Arc` 后即释放注册表锁，读取快照不再与建连/断连争锁。
    pub fn ring(&self, id: &str) -> Option<Arc<TerminalRing>> {
        self.entries.lock().unwrap().get(id).map(|e| e.ring.clone())
    }

    /// 所有会话当前正在写入的录制文件绝对路径（供录制库标记「录制中」）。
    pub fn active_record_paths(&self) -> Vec<PathBuf> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .filter_map(|e| e.ring.recording_path())
            .collect()
    }

    /// 条目是否仍在注册表里（含已断开等待重连的会话）。
    pub fn contains(&self, id: &str) -> bool {
        self.entries.lock().unwrap().contains_key(id)
    }

    /// 会话类型；命令层据此分派文件后端（SFTP 通道 / FTP 控制连接）。
    pub fn kind(&self, id: &str) -> Option<SessionKind> {
        self.entries.lock().unwrap().get(id).map(|e| e.kind)
    }

    /// 取 FTP 控制连接的共享句柄；`None` = 非 FTP 会话或已断开。
    pub fn ftp(&self, id: &str) -> Option<FtpHandle> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .and_then(|e| e.ftp.clone())
    }

    /// 取 FTP 控制连接并校验连接代数：`epoch` 与当前代数不符（已被重连替换）时返回 `None`。
    ///
    /// 供心跳任务使用：`epoch` 是任务建立时押注的代数，重连自增后旧任务在这里对不上号
    /// 就退役，不会继续往新连接发 NOOP（对比堆地址身份，地址会在旧连接释放后被 ABA 复用）。
    pub fn ftp_at(&self, id: &str, epoch: u64) -> Option<FtpHandle> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .filter(|e| e.ftp_epoch == epoch)
            .and_then(|e| e.ftp.clone())
    }

    /// 会话关闭时取走 FTP 连接（以便发 QUIT 后丢弃）。
    pub fn take_ftp(&self, id: &str) -> Option<FtpHandle> {
        self.entries
            .lock()
            .unwrap()
            .get_mut(id)
            .and_then(|e| e.ftp.take())
    }

    /// 就地修改会话（不存在则返回 `None`）。
    pub fn update<T>(&self, id: &str, f: impl FnOnce(&mut SessionEntry) -> T) -> Option<T> {
        self.entries.lock().unwrap().get_mut(id).map(f)
    }

    pub fn remove(&self, id: &str) -> Option<SessionEntry> {
        self.entries.lock().unwrap().remove(id)
    }

    pub fn list(&self) -> Vec<SessionDto> {
        self.entries
            .lock()
            .unwrap()
            .values()
            .map(SessionEntry::dto)
            .collect()
    }

    /// 会话所属连接（用于命令历史归档）。
    pub fn profile_id(&self, id: &str) -> Option<String> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .and_then(|e| e.profile_id.clone())
    }

    /// 重连所需的来源信息（`Direct` 会克隆一份认证材料，仅可信区内流转）。
    pub fn reconnect_plan(&self, id: &str) -> Option<Reconnect> {
        self.entries
            .lock()
            .unwrap()
            .get(id)
            .map(|e| e.reconnect.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        connect_timeout_secs, keepalive_secs, parse_server_software, LineScreen, TerminalRing,
        DEFAULT_CONNECT_TIMEOUT_SECS, DEFAULT_KEEPALIVE_SECS, RING_CAP_BYTES,
    };

    /// 会话信息面板的服务器标识解析：常规剥前缀，畸形输入回退原文不致 panic。
    #[test]
    fn server_software_strips_prefix_or_falls_back() {
        assert_eq!(parse_server_software("SSH-2.0-OpenSSH_8.0"), "OpenSSH_8.0");
        assert_eq!(parse_server_software("SSH-1.99-Cisco-1.25"), "Cisco-1.25");
        // 无前缀 / 空串：原样回退
        assert_eq!(parse_server_software("OpenSSH_9.6"), "OpenSSH_9.6");
        assert_eq!(parse_server_software(""), "");
        // 只有前缀：空软件名，不 panic
        assert_eq!(parse_server_software("SSH-2.0-"), "");
    }

    #[test]
    fn keepalive_zero_falls_back_to_default() {
        assert_eq!(keepalive_secs(0), DEFAULT_KEEPALIVE_SECS);
        assert_eq!(keepalive_secs(15), 15);
    }

    /// 建连超时：0（未配置）折成默认 20s，非 0 原样采用。
    #[test]
    fn connect_timeout_zero_falls_back_to_default() {
        assert_eq!(connect_timeout_secs(0), DEFAULT_CONNECT_TIMEOUT_SECS);
        assert_eq!(DEFAULT_CONNECT_TIMEOUT_SECS, 20);
        assert_eq!(connect_timeout_secs(5), 5);
    }

    /// 协商快照 serde 字段名：前端 SessionPanel 按 camelCase 消费，改名即破约。
    #[test]
    fn negotiation_serializes_camel_case() {
        use super::Negotiation;
        use crate::session::{AuthMaterial, ConnectSpec, SessionKind};
        let spec = ConnectSpec {
            kind: SessionKind::Ssh,
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth: AuthMaterial::Interactive,
            cols: 80,
            rows: 24,
            startup_dir: None,
            startup_command: None,
            keepalive_secs: 0,
            agent_forward: true,
            x11_forward: true,
            ftp_active_mode: false,
            via: None,
            serial: None,
            connect_timeout_secs: 0,
            compression: false,
            auto_record: false,
            record_dir: None,
            record_file_pattern: None,
            record_overwrite: false,
            record_timestamp_prefix: false,
            record_encrypt: true,
        };
        let json = serde_json::to_value(Negotiation::build(None, &spec)).unwrap();
        for key in [
            "serverSoftware",
            "kex",
            "cipher",
            "macClient",
            "macServer",
            "compressionClient",
            "compressionServer",
            "hostKeyType",
            "hostKeyFingerprint",
            "agentForward",
            "x11Forward",
            "keepaliveSecs",
            "jumps",
        ] {
            assert!(json.get(key).is_some(), "missing field {key}");
        }
        assert_eq!(json["agentForward"], serde_json::json!(true));
        assert_eq!(json["x11Forward"], serde_json::json!(true));
        assert_eq!(
            json["keepaliveSecs"],
            serde_json::json!(DEFAULT_KEEPALIVE_SECS)
        );
    }

    #[test]
    fn ring_starts_empty() {
        let r = TerminalRing::new();
        assert!(r.is_empty());
        assert_eq!(r.snapshot(1024), "");
    }

    #[test]
    fn ring_keeps_recent_bytes() {
        let r = TerminalRing::new();
        r.push(b"hello ");
        r.push(b"world");
        assert_eq!(r.snapshot(1024), "hello world");
        // 只取最近 5 字节
        assert_eq!(r.snapshot(5), "world");
        // 空 push 不改变内容
        r.push(b"");
        assert_eq!(r.snapshot(1024), "hello world");
    }

    #[test]
    fn ring_evicts_oldest_beyond_cap() {
        let r = TerminalRing::new();
        r.push(&vec![b'a'; RING_CAP_BYTES]);
        r.push(&[b'b'; 100]);
        assert_eq!(r.len(), RING_CAP_BYTES);
        let snap = r.snapshot(RING_CAP_BYTES);
        assert!(snap.starts_with('a'));
        assert!(snap.ends_with(&"b".repeat(100)));
    }

    #[test]
    fn ring_snapshot_aligns_to_char_boundary() {
        let r = TerminalRing::new();
        r.push("héllo".as_bytes()); // 6 字节：h + é(2) + llo
                                    // 取最近 4 字节的起点落在 'é' 的后续字节上，对齐后跳到 'l'，不产生替换符
        assert_eq!(r.snapshot(4), "llo");
        // 取最近 5 字节起点正好是 'é' 的首字节，无需跳过
        assert_eq!(r.snapshot(5), "éllo");
    }

    /// 原地重画（`\r` + 覆写）不应产生重复字符：`ls` 不会录成 `lls`。
    #[test]
    fn screen_redraw_does_not_duplicate_chars() {
        let mut s = LineScreen::default();
        // 内核先回显 'l'，随后 ZLE `\r` 原地重画整行 'ls'
        assert!(s.feed("l").is_empty());
        assert_eq!(s.feed("\rls\n"), vec!["ls".to_string()]);
    }

    /// 光标绝对列 ESC[<n>G + 擦除到行尾 ESC[K：重画内容落回列 0，旧尾巴被抹掉。
    #[test]
    fn screen_cursor_to_column_repositions() {
        let mut s = LineScreen::default();
        assert_eq!(
            s.feed("garbage\x1b[1G\x1b[Kclean\n"),
            vec!["clean".to_string()]
        );
    }

    /// 擦除整行 ESC[2K：旧内容清空，只留重画后的新内容。
    #[test]
    fn screen_erase_line_clears() {
        let mut s = LineScreen::default();
        assert_eq!(s.feed("old\x1b[2Knew\n"), vec!["new".to_string()]);
    }

    /// 跨 push 分包的转义序列不应被当成可见字符泄漏（ESC 序列在块边界被切断）。
    #[test]
    fn screen_survives_escape_split_across_chunks() {
        let mut s = LineScreen::default();
        assert!(s.feed("\x1b[2").is_empty());
        assert_eq!(s.feed("KX\n"), vec!["X".to_string()]);
    }

    /// OSC（shell-integration 标记等）整段吞掉，不落可见文本。
    #[test]
    fn screen_consumes_osc() {
        let mut s = LineScreen::default();
        assert_eq!(s.feed("\x1b]133;A\x07prompt\n"), vec!["prompt".to_string()]);
    }

    /// CRLF 只定稿一行；未遇换行的当前行由 finish 收尾（最后一条提示符不丢）。
    #[test]
    fn screen_crlf_single_flush_and_finish() {
        let mut s = LineScreen::default();
        assert_eq!(s.feed("foo\r\nbar"), vec!["foo".to_string()]);
        assert_eq!(s.finish(), Some("bar".to_string()));
    }

    /// 起始行补齐：`prime` 用既有原始输出重建「当前未定稿的行」，丢弃已定稿的历史行。
    /// 模拟录制在提示符已打印、命令未敲时启动：起点应是完整提示符，而非从命令名突兀开始。
    #[test]
    fn screen_prime_keeps_current_unterminated_line() {
        let mut s = LineScreen::default();
        // 历史里有一整条已定稿的旧输出，末尾是未换行的新提示符
        s.prime(b"prev output\nuser@host ~ % ");
        // 已定稿的 "prev output" 被丢弃，只留当前提示符作为录制起点
        assert_eq!(s.finish(), Some("user@host ~ %".to_string()));
    }
}
