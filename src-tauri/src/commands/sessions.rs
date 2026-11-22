//! 会话命令：建连 / 重连 / 写入 / 尺寸 / 关闭 / 列表。
//!
//! 前端只拿到不透明的 `session_id` 与不含凭证的 [`SessionDto`]；
//! 认证材料在命令层从保险库取出后直接交给握手路径，不出可信区。
//!
//! SSH / FTP / Telnet / 串口 共用这组命令，差异全部收在 [`open`]/[`reopen`] 两个分派点上。

use serde::Deserialize;
use tauri::AppHandle;

use super::{blocking, state};
use crate::db::profile::{AuthType, Profile};
use crate::error::AppError;
use crate::files::ftp;
use crate::local;
use crate::session::{
    discard, AuthMaterial, ConnectSpec, FtpInfo, Negotiation, Reconnect, SessionCmd, SessionDto,
    SessionKind,
};
use crate::ssh;
use crate::state::AppState;
use crate::vault::Secret;

const DEFAULT_COLS: u32 = 80;
const DEFAULT_ROWS: u32 = 24;

/// 免档案的一次性连接入参（凭证随 `Secret` 用完即清零）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectConnect {
    /// 协议类型；缺省按 SSH 处理。
    #[serde(default)]
    pub kind: SessionKind,
    pub host: String,
    pub port: Option<u16>,
    pub username: String,
    #[serde(default)]
    pub auth_type: AuthType,
    pub password: Option<Secret>,
    /// 用户粘贴的私钥内容。
    pub private_key_pem: Option<Secret>,
    /// 引用的密钥库私钥（ssh_key.id）；优先于粘贴/路径。
    #[serde(default)]
    pub private_key_id: Option<String>,
    /// 或指向磁盘私钥文件（口令仍走 `key_passphrase`）。
    pub private_key_path: Option<String>,
    pub key_passphrase: Option<Secret>,
    pub startup_dir: Option<String>,
    /// 登录后自动执行的命令（仅 SSH 类）。
    #[serde(default)]
    pub startup_command: Option<String>,
    pub cols: Option<u32>,
    pub rows: Option<u32>,
    pub keepalive_secs: Option<u64>,
    /// Agent 转发（-A）；仅 SSH 类生效，缺省关。
    #[serde(default)]
    pub agent_forward: bool,
    /// X11 转发；仅 SSH 类生效，缺省关（显示端需本机跑 X server）。
    #[serde(default)]
    pub x11_forward: bool,
    /// FTP 主动模式（PORT）；仅 FTP/FTPS 生效，缺省关（= 被动）。
    #[serde(default)]
    pub ftp_active_mode: bool,
    /// 串口线参数（仅 `kind=serial`）；`host` 存设备名。
    #[serde(default)]
    pub serial: Option<crate::serial::SerialParams>,
    /// 建连超时（秒）；0 = 用默认。
    #[serde(default)]
    pub connect_timeout_secs: u64,
    /// 压缩优先开关；仅 SSH 生效。
    #[serde(default)]
    pub compression: bool,
    /// 连上即自动录制及其配置（一次性连接同样生效）。
    #[serde(default)]
    pub auto_record: bool,
    #[serde(default)]
    pub record_dir: Option<String>,
    #[serde(default)]
    pub record_file_pattern: Option<String>,
    #[serde(default)]
    pub record_overwrite: bool,
    #[serde(default)]
    pub record_timestamp_prefix: bool,
    /// 录制加密落盘；缺键按关（= 明文，与新建档案一致）。
    #[serde(default)]
    pub record_encrypt: bool,
    /// 初始终端行列；0/None = 用实时尺寸。
    #[serde(default)]
    pub term_cols: u16,
    #[serde(default)]
    pub term_rows: u16,
}

/// 用已保存的连接档案建连，返回新会话。
/// `connect_id` 非空时前端已先立一个预加载占位标签，握手逐阶段回推 `session-stage`。
#[tauri::command]
pub async fn session_connect(
    app: AppHandle,
    profile_id: String,
    cols: u32,
    rows: u32,
    connect_id: Option<String>,
) -> Result<SessionDto, AppError> {
    let connect_id = connect_id.unwrap_or_default();
    let spec = blocking({
        let app = app.clone();
        let id = profile_id.clone();
        move || {
            let st = state(&app);
            let p = st
                .db
                .get_profile(&id)?
                .ok_or_else(|| AppError::ProfileNotFound(id.clone()))?;
            spec_from_profile(&st, &p, cols, rows)
        }
    })
    .await?;

    let id = open(
        app.clone(),
        spec,
        Reconnect::Profile(profile_id.clone()),
        &connect_id,
    )
    .await?;
    // 崩溃泄漏的远端 bundle 临时包补删（决策 D-b）：仅 SSH、best-effort、静默、不阻断连接。
    if state(&app).sessions.kind(&id) == Some(SessionKind::Ssh) {
        let app2 = app.clone();
        let sid = id.clone();
        tokio::spawn(async move {
            crate::files::bundle::cleanup_leaked_remotes(app2, sid, profile_id).await;
        });
    }
    dto(&app, &id)
}

/// 不保存档案直接连（快速连接）；认证材料只驻内存，供本会话重连使用。
#[tauri::command]
pub async fn session_connect_direct(
    app: AppHandle,
    params: DirectConnect,
    connect_id: Option<String>,
) -> Result<SessionDto, AppError> {
    let connect_id = connect_id.unwrap_or_default();
    let spec = blocking({
        let app = app.clone();
        move || {
            let st = state(&app);
            spec_from_direct(&st, params)
        }
    })
    .await?;
    // 重连需要同一份材料：克隆一份留在会话条目里（始终不出可信区）
    let id = open(
        app.clone(),
        spec.clone(),
        Reconnect::Direct(Box::new(spec)),
        &connect_id,
    )
    .await?;
    dto(&app, &id)
}

/// 打开一个本地终端会话（本机 shell）：无凭证、无主机密钥，纯本机子进程。
/// `shell` 为空时用平台默认 shell；输出走既有 `session-output` 事件，前端零改动复用终端。
#[tauri::command]
pub async fn session_connect_local(
    app: AppHandle,
    cols: u32,
    rows: u32,
    shell: Option<String>,
) -> Result<SessionDto, AppError> {
    let id = local::connect(app.clone(), cols, rows, shell).await?;
    dto(&app, &id)
}

/// 按会话类型建连：SSH 走握手 + PTY + actor，FTP 只建一条控制连接，
/// Telnet 走明文 TCP + IAC，串口走本机 COM/tty 透传。
/// `connect_id` 非空时（预加载占位标签在等）SSH/FTP 逐阶段 emit `session-stage`；
/// 其余协议（本地/Telnet/串口/Mosh）秒连或无阶段概念，不传。
async fn open(
    app: AppHandle,
    spec: ConnectSpec,
    reconnect: Reconnect,
    connect_id: &str,
) -> Result<String, AppError> {
    // 连上即录：建连前从 spec 抽出录制意图（避免为触发录制而克隆含凭证的 spec），
    // 会话建立后在各协议共同的输出环上统一起录；失败只记日志，不打断已建立的连接。
    let trigger = spec
        .auto_record
        .then(|| super::record::RecordTrigger::from_spec(&spec));
    let id = if spec.kind.is_ftp() {
        ftp::connect(app.clone(), spec, reconnect, connect_id).await
    } else if spec.kind == SessionKind::Telnet {
        crate::telnet::connect(app.clone(), spec.host.clone(), spec.port, reconnect).await
    } else if spec.kind == SessionKind::Serial {
        let params = spec.serial.clone().unwrap_or_default();
        crate::serial::connect(app.clone(), spec.host.clone(), params, reconnect).await
    } else if spec.kind == SessionKind::Mosh {
        crate::mosh::connect(
            app.clone(),
            crate::mosh::MoshSpec {
                user: spec.username.clone(),
                host: spec.host.clone(),
                port: spec.port,
            },
            spec.cols,
            spec.rows,
            reconnect,
        )
        .await
    } else {
        ssh::connect(app.clone(), spec, reconnect, connect_id).await
    }?;
    if let Some(t) = trigger {
        if let Some(ring) = state(&app).sessions.ring(&id) {
            if let Err(e) = super::record::apply(&app, &ring, &t) {
                debug!("[record] 会话 {id} 自动录制开启失败：{e}");
            }
        }
    }
    Ok(id)
}

/// 断线后沿用同一 `session_id` 重连：标签与终端内容保持不变。
#[tauri::command]
pub async fn session_reconnect(
    app: AppHandle,
    id: String,
    cols: u32,
    rows: u32,
) -> Result<SessionDto, AppError> {
    let plan = state(&app)
        .sessions
        .reconnect_plan(&id)
        .ok_or_else(|| AppError::NoSuchSession(id.clone()))?;

    // 本地终端没有 ConnectSpec / 凭证：“重连”即用同一 shell 配置重开一个本机进程。
    if let Reconnect::Local { shell } = &plan {
        local::reconnect(app.clone(), id.clone(), shell.clone(), cols, rows).await?;
        return dto(&app, &id);
    }

    let spec = match plan {
        Reconnect::Profile(profile_id) => {
            blocking({
                let app = app.clone();
                move || {
                    let st = state(&app);
                    let p = st
                        .db
                        .get_profile(&profile_id)?
                        .ok_or(AppError::ProfileNotFound(profile_id))?;
                    spec_from_profile(&st, &p, cols, rows)
                }
            })
            .await?
        }
        Reconnect::Direct(mut spec) => {
            spec.cols = cols;
            spec.rows = rows;
            *spec
        }
        // 上一分支已拦截 Local；此臂仅为穷尽匹配，运行期不可达。
        Reconnect::Local { .. } => {
            return Err(AppError::Internal("本地会话重连分派异常".into()));
        }
    };

    reopen(app.clone(), id.clone(), spec).await?;
    dto(&app, &id)
}

/// 重连同样按类型分派；两条路径都保持 `session_id` 不变。
async fn reopen(app: AppHandle, id: String, spec: ConnectSpec) -> Result<(), AppError> {
    if spec.kind.is_ftp() {
        ftp::reconnect(app, id, spec).await
    } else if spec.kind == SessionKind::Telnet {
        crate::telnet::reconnect(app, id, spec.host.clone(), spec.port).await
    } else if spec.kind == SessionKind::Serial {
        let params = spec.serial.clone().unwrap_or_default();
        crate::serial::reconnect(app, id, spec.host.clone(), params).await
    } else if spec.kind == SessionKind::Mosh {
        crate::mosh::reconnect(
            app,
            id,
            crate::mosh::MoshSpec {
                user: spec.username.clone(),
                host: spec.host.clone(),
                port: spec.port,
            },
            spec.cols,
            spec.rows,
        )
        .await
    } else {
        ssh::reconnect(app, id, spec).await
    }
}

#[tauri::command]
pub async fn session_list(app: AppHandle) -> Result<Vec<SessionDto>, AppError> {
    Ok(state(&app).sessions.list())
}

/// 会话信息面板数据：协商快照 + RTT + 该会话上的活跃隧道（均不含凭证）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session: SessionDto,
    /// 仅 SSH 连接有值；其余协议为 `None`（前端展示不支持空态）。
    pub negotiation: Option<Negotiation>,
    /// 仅 FTP/FTPS 连接有值。与 `negotiation` 分列：两种「协商」没有共同字段。
    pub ftp_info: Option<FtpInfo>,
    pub tunnels: Vec<crate::ssh::tunnel::ActiveTunnel>,
}

/// 取单会话的连接协商信息（会话信息面板用）。
#[tauri::command]
pub async fn session_info(app: AppHandle, id: String) -> Result<SessionInfo, AppError> {
    let st = state(&app);
    let (session, negotiation, ftp_info) = st
        .sessions
        .update(&id, |e| {
            (e.dto(), e.negotiation.clone(), e.ftp_info.clone())
        })
        .ok_or_else(|| AppError::NoSuchSession(id.clone()))?;
    let tunnels = st.tunnels.list(Some(&id));
    Ok(SessionInfo {
        session,
        negotiation,
        ftp_info,
        tunnels,
    })
}

/// 文件栏「跟随终端」：在会话既有的 exec 通道跑一次 `pwd`，返回终端当前工作目录。
///
/// 刻意只开放这一条固定命令，不把通用 exec 暴露给前端：通用执行通道会绕过
/// AI 侧的策略裁决与人工审批（`ai::policy`），而界面本来也没有理由跑任意命令。
/// 完整的「cd 实时跟随」需要 shell 集成（OSC 7 / 注入 hook），本机与远端 shell 都不受我们
/// 控制，故只做这个用户主动点击的一次快照。
#[tauri::command]
pub async fn session_cwd(app: AppHandle, id: String) -> Result<String, AppError> {
    /// 拿目录是个瞬间动作；不等 30s 默认超时，不让按钮看似卡死。
    const CWD_TIMEOUT_MS: u64 = 5_000;
    let r = crate::ai::exec::exec_command(&app, &id, "pwd", CWD_TIMEOUT_MS).await?;
    // 只取首行：个别服务端会在 pwd 后附带提示行。路径形状不在这里猜平台，
    // 直接交给文件后端（它按会话类型分 SFTP/FTP 两套语义）。
    let path = r.stdout.lines().next().unwrap_or("").trim().to_string();
    if path.is_empty() || r.timed_out || r.exit_code != Some(0) {
        return Err(AppError::Ai(format!(
            "读取终端当前目录失败：{}",
            r.stderr.trim()
        )));
    }
    Ok(path)
}

/// 枚举本机串口设备名（Windows `COM*` / Unix `/dev/tty*`），供连接表单下拉。
#[tauri::command]
pub async fn serial_ports() -> Result<Vec<String>, AppError> {
    Ok(crate::serial::available_ports())
}

/// 探测本机 WSL（供「本地终端」选择器）：返回引擎是否安装 + 可交互发行版列表。
/// 非 Windows / 未装 WSL / 命令失败均返回 installed=false、distros 空表。
#[tauri::command]
pub async fn wsl_list() -> Result<local::WslInfo, AppError> {
    // `wsl.exe -l -q` 是同步子进程等待：WSL 服务挂起时会永久占住调用线程。
    // 必须走 spawn_blocking：跑在 async worker 上会把 tokio 定时器一并钉死
    //（历次「弹窗卡死且有超时却不触发」的根因形态之一）。
    blocking(|| Ok(local::wsl_info())).await
}

/// 对会话目标做一次 TCP 探测，返回 RTT（毫秒）；成功则写回会话元数据。
#[tauri::command]
pub async fn session_rtt(app: AppHandle, id: String) -> Result<Option<u32>, AppError> {
    use std::time::Instant;
    let st = state(&app);
    let (host, port) = st
        .sessions
        .update(&id, |e| (e.host.clone(), e.port))
        .ok_or_else(|| AppError::NoSuchSession(id.clone()))?;
    if port == 0 {
        return Ok(None);
    }
    let t0 = Instant::now();
    match tokio::net::TcpStream::connect((host.as_str(), port)).await {
        Ok(_s) => {
            let ms = t0.elapsed().as_millis().min(u32::MAX as u128) as u32;
            let _ = state(&app).sessions.update(&id, |e| {
                e.latency_ms = Some(ms);
            });
            Ok(Some(ms))
        }
        Err(e) => Err(AppError::Io(format!("探测 {host}:{port} 失败：{e}"))),
    }
}

/// 发送用户键入的字节到远端 PTY（xterm.js onData → 此处）。
/// 断线中的会话保留标签内容，键入直接丢弃（重连后再输入）。
#[tauri::command]
pub async fn session_write(app: AppHandle, id: String, data: String) -> Result<(), AppError> {
    write_bytes(&app, &id, data.into_bytes()).await
}

/// [`session_write`] 的字节级同族：直发 `0x00`–`0xFF` 任意原始字节。
///
/// 为什么要单独一条：`session_write` 的入参是 `String`（UTF-8），而 Hex 发送要的是
/// 任意字节序列——`\x00`、`\x80`–`\xff` 这些在 Rust `String` 里根本不存在，硬塞只会得到
/// 替换字符或报错。文本通路保持不动（xterm 键入仍走原命令），这里只多一个 `Vec<u8>` 入参，
/// 落点与 `session_write` 完全同一个 [`SessionCmd::Write`]：信任级别一致（用户在本机对自己
/// 的会话按键，不经 AI、不过策略引擎、不落盘），不新增越路面。
#[tauri::command]
pub async fn session_write_bytes(
    app: AppHandle,
    id: String,
    data: Vec<u8>,
) -> Result<(), AppError> {
    // 单条上限：脚本条目本就是「一行/一个字符/一段 hex」，超此量只可能是误粘巨型 blob。
    const MAX_ENTRY_BYTES: usize = 1024 * 1024;
    if data.len() > MAX_ENTRY_BYTES {
        return Err(AppError::Invalid(format!(
            "单条发送内容不得超过 {}MB",
            MAX_ENTRY_BYTES / 1024 / 1024
        )));
    }
    write_bytes(&app, &id, data).await
}

/// 投递原始字节到会话 actor；断线中（有标签无 sender）静默丢弃，与 `session_write` 同语义。
async fn write_bytes(app: &AppHandle, id: &str, data: Vec<u8>) -> Result<(), AppError> {
    match state(app).sessions.sender(id) {
        Some(tx) => tx
            .send(SessionCmd::Write(data))
            .await
            .map_err(|_| AppError::NoSuchSession(id.to_string())),
        None if state(app).sessions.contains(id) => Ok(()),
        None => Err(AppError::NoSuchSession(id.to_string())),
    }
}

/// 取会话终端最近输出（降噪后纯文本，群发结果聚合 / 调试用）。
/// 数据源是与 AI `get_terminal_snapshot` 同一份 [`TerminalRing`]，但**不再过 AI 脱敏**——
/// 这是用户在本机查看自己的终端近况（等价于盯着 xterm 看），不出网、不落盘。
#[tauri::command]
pub async fn session_snapshot(
    app: AppHandle,
    id: String,
    max_bytes: Option<usize>,
) -> Result<String, AppError> {
    blocking(move || {
        let ring = state(&app)
            .sessions
            .ring(&id)
            .ok_or_else(|| AppError::NoSuchSession(id.clone()))?;
        let max = max_bytes.unwrap_or(4096).clamp(1, 64 * 1024);
        Ok(crate::ai::tools::strip_ansi(&ring.snapshot(max))
            .trim()
            .to_string())
    })
    .await
}

/// 终端**原样**回放用环缓冲（base64，含 ANSI / `\r\n`）。
/// 前端 xterm 补帧必须走这个，不能用 `session_snapshot`——那条会 `strip_ansi` 丢掉 `\r`，
/// 把「登录行 + 提示符」粘成一行。
#[tauri::command]
pub async fn session_ring_b64(
    app: AppHandle,
    id: String,
    max_bytes: Option<usize>,
) -> Result<String, AppError> {
    use base64::Engine;
    blocking(move || {
        let ring = state(&app)
            .sessions
            .ring(&id)
            .ok_or_else(|| AppError::NoSuchSession(id.clone()))?;
        let max = max_bytes.unwrap_or(16 * 1024).clamp(1, 256 * 1024);
        Ok(base64::engine::general_purpose::STANDARD.encode(ring.raw(max)))
    })
    .await
}

/// 通知远端终端尺寸变化（列, 行）。
#[tauri::command]
pub async fn session_resize(
    app: AppHandle,
    id: String,
    cols: u32,
    rows: u32,
) -> Result<(), AppError> {
    send(&app, &id, SessionCmd::Resize(cols, rows)).await
}

/// 关闭会话。SSH 连着时给 actor 发 `Close`（由它收尾 disconnect）；
/// 断线状态下 actor 已退出，FTP 会话根本没有 actor，两者都直接摘除条目。
#[tauri::command]
pub async fn session_close(app: AppHandle, id: String) -> Result<(), AppError> {
    // FTP 会话没有 actor：先取走控制连接发 QUIT，再摘除条目并广播 closed
    if let Some(handle) = state(&app).sessions.take_ftp(&id) {
        ftp::quit(handle).await;
        discard(&app, &id);
        return Ok(());
    }
    if let Some(tx) = state(&app).sessions.sender(&id) {
        return tx
            .send(SessionCmd::Close)
            .await
            .map_err(|_| AppError::NoSuchSession(id));
    }
    discard(&app, &id);
    Ok(())
}

/// 取会话发送端并投递指令；会话不存在或已断开则返回 `NoSuchSession`。
async fn send(app: &AppHandle, id: &str, cmd: SessionCmd) -> Result<(), AppError> {
    let tx = state(app)
        .sessions
        .sender(id)
        .ok_or_else(|| AppError::NoSuchSession(id.to_string()))?;
    tx.send(cmd)
        .await
        .map_err(|_| AppError::NoSuchSession(id.to_string()))
}

fn dto(app: &AppHandle, id: &str) -> Result<SessionDto, AppError> {
    state(app)
        .sessions
        .update(id, |e| e.dto())
        .ok_or_else(|| AppError::NoSuchSession(id.to_string()))
}

/// 从档案 + 保险库组装连接参数；缺凭证时给出可操作的错误提示。
/// 若档案配置了跳板机（`jump_profile_id`），递归组装 `via` 链（深度夹紧，防环）。
fn spec_from_profile(
    st: &AppState,
    p: &Profile,
    cols: u32,
    rows: u32,
) -> Result<ConnectSpec, AppError> {
    spec_from_profile_depth(st, p, cols, rows, 0)
}

/// 跳板链最大深度（含目标；即最多 2 级跳板）。
const MAX_JUMP_DEPTH: u32 = 3;

/// 解析连接生效的私钥保险库引用：
/// - 引用密钥库（`private_key_id` = ssh_key.id）：取该密钥的 `key_ref`；
/// - 否则回落内联私钥 `private_key_ref`（用户粘贴的 PEM 落库）。
/// 私钥本体只在本可信区按引用读取，绝不经 IPC 回前端。
fn resolve_privkey_ref(st: &AppState, p: &Profile) -> Result<Option<String>, AppError> {
    if let Some(kid) = p
        .private_key_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(k) = st.db.get_ssh_key(kid)? {
            return Ok(Some(k.key_ref));
        }
    }
    Ok(p.private_key_ref.clone())
}

fn spec_from_profile_depth(
    st: &AppState,
    p: &Profile,
    cols: u32,
    rows: u32,
    depth: u32,
) -> Result<ConnectSpec, AppError> {
    if depth >= MAX_JUMP_DEPTH {
        return Err(AppError::Invalid(
            "跳板链过深或存在循环（最多 2 级跳板）".into(),
        ));
    }
    let privkey_ref = resolve_privkey_ref(st, p)?;
    let passphrase = secret(st, p.credential_ref.as_deref())?;
    // Telnet / 串口无预认证
    let auth = if p.kind.is_interactive_auth() {
        AuthMaterial::Interactive
    } else {
        match p.auth_type {
            AuthType::Password => match passphrase {
                Some(pw) => AuthMaterial::Password(pw),
                // FTP/FTPS 匿名档案本就无凭证，缺密码不是错误
                None if p.kind.is_ftp() => AuthMaterial::Password(Secret::new("")),
                None => {
                    return Err(AppError::Biz {
                        key: "err.auth.noPassword",
                        zh: "未保存密码，请编辑连接后重试",
                    })
                }
            },
            AuthType::PrivateKey => match secret(st, privkey_ref.as_deref())? {
                Some(pem) => AuthMaterial::PrivateKey { pem, passphrase },
                None => {
                    let path = p.private_key_path.clone().ok_or_else(|| AppError::Biz {
                        key: "err.key.notConfigured",
                        zh: "未配置私钥内容或路径",
                    })?;
                    AuthMaterial::PrivateKeyFile { path, passphrase }
                }
            },
            // 档案不带材料：连接时服务端逐轮出题，用户经 ssh-kbd-request 弹窗作答
            AuthType::KeyboardInteractive => AuthMaterial::KbdInteractive,
            // 自动协商：取档案已存密码作回退材料（无密码也可，kbd 优先）
            AuthType::Auto => AuthMaterial::Auto { password: passphrase },
        }
    };
    // 跳板机必须是 SSH；从库取其档案并递归（凭证同样走保险库，绝不进 ConnectSpec 日志）
    let via = match p
        .jump_profile_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(jid) => {
            let jp = st
                .db
                .get_profile(jid)?
                .ok_or_else(|| AppError::ProfileNotFound(jid.to_string()))?;
            if jp.kind != SessionKind::Ssh {
                return Err(AppError::Invalid("跳板机必须是 SSH 连接".into()));
            }
            Some(Box::new(spec_from_profile_depth(
                st,
                &jp,
                cols,
                rows,
                depth + 1,
            )?))
        }
        None => None,
    };
    Ok(ConnectSpec {
        kind: p.kind,
        host: p.host.clone(),
        port: p.port,
        username: p.username.clone(),
        auth,
        // 档案显式配置了初始终端行列（>0）则覆盖前端传入的实时尺寸；0 = 沿用 cols/rows。
        cols: if p.term_cols > 0 { p.term_cols as u32 } else { cols },
        rows: if p.term_rows > 0 { p.term_rows as u32 } else { rows },
        startup_dir: p.startup_dir.clone(),
        startup_command: p.startup_command.clone(),
        keepalive_secs: p.keepalive_secs,
        agent_forward: p.agent_forward,
        x11_forward: p.x11_forward,
        ftp_active_mode: p.ftp_active_mode,
        via,
        serial: p.serial.clone(),
        connect_timeout_secs: p.connect_timeout_secs,
        compression: p.compression,
        auto_record: p.auto_record,
        record_dir: p.record_dir.clone(),
        record_file_pattern: p.record_file_pattern.clone(),
        record_overwrite: p.record_overwrite,
        record_timestamp_prefix: p.record_timestamp_prefix,
        record_encrypt: p.record_encrypt,
    })
}

fn spec_from_direct(st: &AppState, mut params: DirectConnect) -> Result<ConnectSpec, AppError> {
    if params.host.trim().is_empty() {
        return Err(AppError::Invalid(
            if params.kind == SessionKind::Serial {
                "串口设备名不能为空"
            } else {
                "主机地址不能为空"
            }
            .into(),
        ));
    }
    // FTP/FTPS 允许空用户名：留空即匿名登录（见 files::ftp::prepare 的 anonymous 映射）
    if !params.kind.is_interactive_auth()
        && !params.kind.is_ftp()
        && params.username.trim().is_empty()
    {
        return Err(AppError::Invalid("用户名不能为空".into()));
    }
    if params.kind.is_ftp() && params.auth_type == AuthType::PrivateKey {
        return Err(AppError::Invalid("FTP/FTPS 只支持密码认证".into()));
    }
    // keyboard-interactive / auto 是 SSH 独有的认证方式（RFC 4252）
    if matches!(
        params.auth_type,
        AuthType::KeyboardInteractive | AuthType::Auto
    ) && params.kind != SessionKind::Ssh
    {
        return Err(AppError::Invalid(
            "keyboard-interactive/auto 仅支持 SSH 连接".into(),
        ));
    }
    let port = params.port.unwrap_or_else(|| params.kind.default_port());
    // Telnet / 串口无预认证，建连不带凭证
    let auth = if params.kind.is_interactive_auth() {
        AuthMaterial::Interactive
    } else {
        match params.auth_type {
            AuthType::Password => {
                let pw = params.password.take().filter(|s| !s.is_empty());
                match pw {
                    Some(pw) => AuthMaterial::Password(pw),
                    // FTP/FTPS 匿名：允许空密码（发空 PASS）
                    None if params.kind.is_ftp() => AuthMaterial::Password(Secret::new("")),
                    None => return Err(AppError::Invalid("请输入密码".into())),
                }
            }
            AuthType::PrivateKey => {
                let passphrase = params.key_passphrase.take().filter(|s| !s.is_empty());
                // 密钥库引用优先：按 ssh_key.id 取库内私钥 PEM；
                // 再回落用户粘贴内容，最后磁盘路径。PEM 只在可信区读取，绝不回前端。
                let store_pem = match params
                    .private_key_id
                    .take()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    Some(kid) => match st.db.get_ssh_key(&kid)? {
                        Some(k) => secret(st, Some(&k.key_ref))?,
                        None => None,
                    },
                    None => None,
                };
                match store_pem.or_else(|| params.private_key_pem.take().filter(|s| !s.is_empty()))
                {
                    Some(pem) => AuthMaterial::PrivateKey { pem, passphrase },
                    None => match params.private_key_path.take() {
                        Some(path) if !path.trim().is_empty() => {
                            AuthMaterial::PrivateKeyFile { path, passphrase }
                        }
                        _ => {
                            return Err(AppError::Invalid(
                                "请从密钥库选择私钥，或提供私钥内容/路径".into(),
                            ))
                        }
                    },
                }
            }
            AuthType::KeyboardInteractive => AuthMaterial::KbdInteractive,
            AuthType::Auto => AuthMaterial::Auto {
                password: params.password.take().filter(|s| !s.is_empty()),
            },
        }
    };
    Ok(ConnectSpec {
        kind: params.kind,
        host: params.host.trim().to_string(),
        port,
        username: params.username.trim().to_string(),
        auth,
        cols: if params.term_cols > 0 {
            params.term_cols as u32
        } else {
            params.cols.unwrap_or(DEFAULT_COLS)
        },
        rows: if params.term_rows > 0 {
            params.term_rows as u32
        } else {
            params.rows.unwrap_or(DEFAULT_ROWS)
        },
        startup_dir: params.startup_dir.take(),
        startup_command: params
            .startup_command
            .take()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        keepalive_secs: params.keepalive_secs.unwrap_or(0),
        agent_forward: params.agent_forward,
        x11_forward: params.x11_forward,
        ftp_active_mode: params.ftp_active_mode,
        via: None,
        serial: params.serial.take().map(|s| s.normalized()),
        connect_timeout_secs: params.connect_timeout_secs,
        compression: params.compression,
        auto_record: params.auto_record,
        record_dir: params.record_dir.take(),
        record_file_pattern: params.record_file_pattern.take(),
        record_overwrite: params.record_overwrite,
        record_timestamp_prefix: params.record_timestamp_prefix,
        record_encrypt: params.record_encrypt,
    })
}

fn secret(st: &AppState, key: Option<&str>) -> Result<Option<Secret>, AppError> {
    match key {
        Some(k) => st.vault.get(k),
        None => Ok(None),
    }
}
