//! SSH 连接层：握手 → 认证（密码/私钥）→ PTY + shell → 读写分离的会话 actor。
//!
//! 安全红线：认证材料只在握手路径出现，绝不写入事件/日志；
//! 前端只拿到不透明的 `session_id`。

pub mod host_key;
pub mod kbd;
pub mod shell_integration;
pub mod tunnel;

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use russh::client;
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use zeroize::Zeroizing;

use crate::error::AppError;
use crate::session::{
    connect_timeout_secs, discard, emit_stage, emit_state, keepalive_secs, teardown, AuthMaterial,
    ConnectSpec, Negotiation, Reconnect, SessionCmd, SessionEntry, SessionState, SshHandle,
    TerminalRing,
};
use crate::state::AppState;
use crate::vault::Secret;
use crate::zmodem::{find_handshake, TermZm, ZmDir};
use host_key::{ClientHandler, Issue, Negotiated};

/// 终端输出事件：`data` 为 base64（二进制安全），前端按 `sessionId` 路由到对应 xterm。
pub const EVENT_OUTPUT: &str = "session-output";

/// 终端内 ZMODEM事件与常量。
/// `zmodem-request`：检测到 rz/sz 握手帧，通知前端弹原生文件框；
/// `zmodem-done`：传输结束/失败，通知前端提示。
pub const EVENT_ZM_REQUEST: &str = "zmodem-request";
pub const EVENT_ZM_DONE: &str = "zmodem-done";
/// `zmodem-file`：传输开始后广播文件名/大小（下载在 `FileStarted` 才知），
/// 供前端把终端内 ZMODEM 的本地传输行补全。
pub const EVENT_ZM_FILE: &str = "zmodem-file";
/// 取消/中止 ZMODEM：连续 8 个 CAN 字符（协议规定发 8 个确保对端收到）。
const CAN: [u8; 8] = [0x18; 8];
/// 握手后等前端选文件的上限；远端 rz 自己也会超时退出，这里兜底。
const ZM_PENDING_SECS: std::time::Duration = std::time::Duration::from_secs(90);
/// 传输中空闲上限：每块数据都有 ACK 往返，15 秒完全静默基本等于卡死，
/// 发 CAN 恢复终端（比固定总时长更贴近真实状态，卡死也能快速退出）。
const ZM_IDLE_SECS: std::time::Duration = std::time::Duration::from_secs(15);

/// 终端内 ZMODEM 拦截状态：检测到握手帧后停止转发，帧换手给状态机，传输完恢复。
#[allow(clippy::large_enum_variant)]
enum ZmMode {
    /// 已识别握手、等前端选完路径；`buf` 缓存后续到达的字节（远端可能重发握手）。
    Pending {
        dir: ZmDir,
        buf: Vec<u8>,
        since: tokio::time::Instant,
    },
    /// 传输进行中。
    Running {
        machine: TermZm,
        since: tokio::time::Instant,
    },
}

/// `ZmMode::Pending` 的一轮 select 结果。
enum PendingOutcome {
    /// 继续等待（带回累积的缓冲）。
    Stay(Vec<u8>),
    /// 前端已选好路径，开始传输。
    Start(String),
    /// 取消/超时：发 CAN 放弃。
    Cancel,
    /// 通道已关（是否用户主动）。
    Exit(bool),
}

/// 未显式配置时的心跳间隔默认值已移至 [`crate::session::DEFAULT_KEEPALIVE_SECS`]（会话信息面板共用口径）；
/// 连续 [`KEEPALIVE_MAX`] 次无响应即判定断线。
const KEEPALIVE_MAX: usize = 3;
const TERM: &str = "xterm-256color";

type Handle = client::Handle<ClientHandler>;
type Channel = russh::Channel<client::Msg>;

/// 输出事件负载。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Output<'a> {
    session_id: &'a str,
    data: &'a str,
}

/// 生成 X11 MIT-MAGIC-COOKIE-1 cookie：16 字节密码学随机数的 hex 串（32 字符）。
/// 客户端生成后经 x11-req 交给 sshd，由 sshd 写入远端 `~/.Xauthority`；
/// 远端 GUI 程序凭它连回，本机 X server 用同一 cookie 验真。
fn x11_cookie() -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut buf = [0u8; 16];
    SystemRandom::new()
        .fill(&mut buf)
        .expect("OS 随机源不可用");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 建立新会话并注册到会话表，返回 `session_id`。
///
/// `connect_id` 非空时（前端预加载占位标签在等），握手逐阶段 emit `session-stage`；
/// 失败时在返回 Err 前补一条 `failed` 阶段，让预加载页日志拿到最终归因。
pub async fn connect(
    app: AppHandle,
    spec: ConnectSpec,
    reconnect: Reconnect,
    connect_id: &str,
) -> Result<String, AppError> {
    let t0 = std::time::Instant::now();
    let (handle, channel, jumps, negotiation) = match handshake(&app, &spec, connect_id).await {
        Ok(v) => v,
        Err(e) => {
            emit_stage(
                &app,
                connect_id,
                &spec.host,
                "failed",
                "fail",
                &e.to_string(),
            );
            return Err(e);
        }
    };
    let latency_ms = t0.elapsed().as_millis().min(u32::MAX as u128) as u32;
    // `Handle` 不可 Clone：包 `Arc` 后一份入注册表（供 SFTP 开新通道），一份留给 actor 收尾
    let handle = Arc::new(handle);
    let sessions = &app.state::<AppState>().sessions;
    let id = sessions.next_id();
    let (tx, rx) = mpsc::channel(64);
    // 终端环形缓冲：一份入注册表（供 AI 快照读取），一份交给 actor 持续 push
    let ring = TerminalRing::new();
    sessions.insert(SessionEntry {
        id: id.clone(),
        label: spec.label(),
        host: spec.host.clone(),
        profile_id: reconnect.profile_id(),
        state: SessionState::Connected,
        kind: spec.kind,
        tx: Some(tx),
        handle: Some(handle.clone()),
        jumps,
        ftp: None,
        ftp_epoch: 0,
        reconnect,
        ring: ring.clone(),
        port: spec.port,
        latency_ms: Some(latency_ms),
        negotiation: Some(negotiation),
        ftp_info: None,
    });
    spawn_actor(
        app.clone(),
        id.clone(),
        handle,
        channel,
        rx,
        spec.startup_dir.clone(),
        spec.startup_command.clone(),
        ring,
    );
    Ok(id)
}

/// 断线后沿用同一 `session_id` 重连：标签与终端内容保持不变。
pub async fn reconnect(app: AppHandle, id: String, spec: ConnectSpec) -> Result<(), AppError> {
    // 重连的标签已存在（不是预加载占位），无 connect_id → 不发 stage 事件。
    let (handle, channel, jumps, negotiation) = handshake(&app, &spec, "").await?;
    let handle = Arc::new(handle);
    let (tx, rx) = mpsc::channel(64);
    // 沿用既有环形缓冲：终端内容跨重连保持，actor 继续在尾部追加
    let attached = app.state::<AppState>().sessions.update(&id, |e| {
        e.tx = Some(tx);
        e.handle = Some(handle.clone());
        e.jumps = jumps;
        e.state = SessionState::Connected;
        // 重连即重新协商：快照整体刷新，不留旧连接的算法/指纹
        e.negotiation = Some(negotiation.clone());
        e.ring.clone()
    });
    let ring = match attached {
        Some(ring) => ring,
        None => return Err(AppError::NoSuchSession(id)),
    };
    spawn_actor(
        app.clone(),
        id.clone(),
        handle,
        channel,
        rx,
        spec.startup_dir.clone(),
        // 重连不重跑启动命令：它未必幂等（如 `vim foo`），会话已在时只需恢复目录。
        None,
        ring,
    );
    emit_state(&app, &id, SessionState::Connected, None);
    Ok(())
}

/// 握手 + 认证 + 开 PTY/shell。失败时把主机密钥问题翻译成可读错误。
/// 返回 `(handle, channel, jumps, negotiation)`：`jumps` 是跳板机句柄链（ProxyJump），
/// 调用方须随会话保留；`negotiation` 是目标连接的协商快照（会话信息面板数据源）。
async fn handshake(
    app: &AppHandle,
    spec: &ConnectSpec,
    connect_id: &str,
) -> Result<(Handle, Channel, Vec<SshHandle>, Negotiation), AppError> {
    let (handle, jumps, negotiated) = connect_authed(app, spec, connect_id).await?;
    emit_stage(
        app,
        connect_id,
        &spec.host,
        "shell",
        "start",
        "正在申请终端（PTY）并启动 shell",
    );
    let channel = proto("打开会话通道", handle.channel_open_session()).await?;
    // 转发类请求必须在 pty-req/shell 之前发出：sshd 在 fork 会话子进程时才按
    // channel 上的 x11/agent 标志分配代理 DISPLAY、写 xauth 条目，晚到只会被忽略
    // （现象：连接正常但 DISPLAY 始终为空，静默失效）；顺序对齐 OpenSSH 客户端
    // （x11-req → auth-agent-req → pty-req → shell）。
    // X11 转发：仅当档案开启时发 x11-req（携带本端随机 cookie，sshd 写入远端
    // `~/.Xauthority` 并设置 DISPLAY）；远端 GUI 程序的回连由 handler 桥接到本机 X server。
    // 失败（如服务器未开 X11Forwarding）不中断连接，仅记日志。
    if spec.x11_forward {
        match channel
            .request_x11(true, false, "MIT-MAGIC-COOKIE-1", x11_cookie(), 0)
            .await
        {
            Ok(()) => {}
            Err(e) => debug!("[x11] x11-req 被拒（服务端可能未开 X11Forwarding）：{e}"),
        }
    }
    // Agent 转发（-A）：仅当档案开启时向远端发 auth-agent-req；关时不发请求，
    // 远端也就不会回连 agent 通道（handler 侧另有 fail-safe 兜底拒绝）。
    if spec.agent_forward {
        let _ = channel.agent_forward(true).await;
    }
    proto(
        "申请终端(PTY)",
        channel.request_pty(false, TERM, spec.cols, spec.rows, 0, 0, &[]),
    )
    .await?;
    proto("启动 shell", channel.request_shell(false)).await?;
    emit_stage(app, connect_id, &spec.host, "shell", "ok", "终端已就绪");
    let negotiation = Negotiation::build(negotiated.lock().unwrap().take(), spec);
    Ok((handle, channel, jumps, negotiation))
}

/// 建连 + 认证（不开 PTY）；有 `via` 时先连跳板机再在其上开 `direct-tcpip` 作底层流。
/// 返回的 `Vec<SshHandle>` 是整条跳板链（自近而远），目标流挂在它们的通道上，调用方必须持有；
/// `negotiated` 槽位指向**目标连接** handler 的协商快照（跳板机的不入会话）。
async fn connect_authed(
    app: &AppHandle,
    spec: &ConnectSpec,
    connect_id: &str,
) -> Result<(Handle, Vec<SshHandle>, Arc<Mutex<Option<Negotiated>>>), AppError> {
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(keepalive_secs(spec.keepalive_secs))),
        keepalive_max: KEEPALIVE_MAX,
        // 空闲不主动断开，断线判定交给 keepalive
        inactivity_timeout: None,
        // 压缩：档案开启时把首选表调成 zlib 优先（zlib → zlib@openssh.com → none），
        // 关闭时保持默认（none 优先，不主动启用压缩）；实际协商结果由会话信息面板如实展示。
        preferred: if spec.compression {
            russh::Preferred {
                compression: std::borrow::Cow::Owned(vec![
                    russh::compression::ZLIB,
                    russh::compression::ZLIB_LEGACY,
                    russh::compression::NONE,
                ]),
                ..russh::Preferred::DEFAULT
            }
        } else {
            russh::Preferred::DEFAULT
        },
        ..<_>::default()
    });
    let (handler, issue, negotiated) = ClientHandler::new(
        app.clone(),
        spec.host.clone(),
        spec.port,
        spec.agent_forward,
        spec.x11_forward,
    );

    let (mut handle, jumps) = match &spec.via {
        // ProxyJump：经跳板机的 direct-tcpip 流再做一次 SSH 握手（多级 via 递归）
        Some(via) => {
            // 递归 async fn 需装箱（E0733）；跳板机自身的协商快照不入会话，忽略
            let (jump_h, mut jumps, _) = Box::pin(connect_authed(app, via, connect_id)).await?;
            emit_stage(
                app,
                connect_id,
                &spec.host,
                "tcp",
                "start",
                &format!(
                    "经跳板机 {} 打开到 {}:{} 的转发",
                    via.host, spec.host, spec.port
                ),
            );
            let ch = tokio::time::timeout(
                JUMP_OPEN_TIMEOUT,
                jump_h.channel_open_direct_tcpip(
                    spec.host.clone(),
                    spec.port as u32,
                    "127.0.0.1".to_string(),
                    0u32,
                ),
            )
            .await
            .map_err(|_| {
                AppError::Ssh(format!(
                    "经跳板机 {} 打开到 {}:{} 的转发超时：{}s 内未建立通道，跳板连接可能已失效",
                    via.host,
                    spec.host,
                    spec.port,
                    JUMP_OPEN_TIMEOUT.as_secs()
                ))
            })?
            .map_err(|e| {
                AppError::Ai(format!(
                    "经跳板机 {} 打开到 {}:{} 的转发失败：{e}",
                    via.host, spec.host, spec.port
                ))
            })?;
            let stream = ch.into_stream();
            emit_stage(
                app,
                connect_id,
                &spec.host,
                "kex",
                "start",
                "正在进行密钥协商（KEX）与主机密钥校验",
            );
            let h = guard_handshake(
                app,
                client::connect_stream(config, stream, handler),
                &spec.host,
                spec.port,
            )
            .await?
            .map_err(|e| connect_error(&issue, e))?;
            emit_stage(app, connect_id, &spec.host, "kex", "ok", "密钥协商完成");
            jumps.push(Arc::new(jump_h));
            (h, jumps)
        }
        None => {
            // 先做带超时的 TCP 建连，再交给 connect_stream 走 SSH 握手；
            // 避免不可达主机卡在 OS 的 SYN 重试（Windows 约 21s）导致前端「像卡死」。
            emit_stage(
                app,
                connect_id,
                &spec.host,
                "tcp",
                "start",
                &format!("正在解析并连接 {}:{}", spec.host, spec.port),
            );
            let stream = connect_tcp(
                spec.host.as_str(),
                spec.port,
                Duration::from_secs(connect_timeout_secs(spec.connect_timeout_secs)),
            )
            .await?;
            emit_stage(app, connect_id, &spec.host, "tcp", "ok", "TCP 连接已建立");
            emit_stage(
                app,
                connect_id,
                &spec.host,
                "kex",
                "start",
                "正在进行密钥协商（KEX）与主机密钥校验",
            );
            let h = guard_handshake(
                app,
                client::connect_stream(config, stream, handler),
                &spec.host,
                spec.port,
            )
            .await?
            .map_err(|e| connect_error(&issue, e))?;
            emit_stage(app, connect_id, &spec.host, "kex", "ok", "密钥协商完成");
            (h, Vec::new())
        }
    };

    emit_stage(
        app,
        connect_id,
        &spec.host,
        "auth",
        "start",
        &format!("正在认证用户 {}", spec.username),
    );
    guard_auth(app, authenticate(app, &mut handle, spec), &spec.host).await?;
    emit_stage(app, connect_id, &spec.host, "auth", "ok", "认证成功");
    Ok((handle, jumps, negotiated))
}

/// TCP 建连超时缺省值已移至 [`crate::session::DEFAULT_CONNECT_TIMEOUT_SECS`]（档案未配置时用）；
/// 不可达主机若不设限会卡在 OS 的 SYN 重试（Windows 约 21s），期间前端毫无反馈、像卡死。
/// 连不上即在配置的超时窗口内报错，让 UI 及时给出失败提示。

/// 带超时的 TCP 连接；DNS 解析也在超时窗口内（`TcpStream::connect` 内部完成）。
/// `timeout` 由调用方按档案的建连超时配置折算传入（见 [`connect_timeout_secs`]）。
async fn connect_tcp(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<tokio::net::TcpStream, AppError> {
    tokio::time::timeout(timeout, tokio::net::TcpStream::connect((host, port)))
    .await
    .map_err(|_| {
        AppError::Ssh(format!(
            "连接超时：{host}:{port} 在 {}s 内无响应",
            timeout.as_secs()
        ))
    })?
    .map_err(|e| AppError::Ssh(format!("无法连接 {host}:{port}：{e}")))
}

/// 握手指超时：TCP 已连上但对端不发 SSH banner / 卡在密钥协商（端口填错指到 HTTP、MySQL
/// 这类非 SSH 服务，或中间设备透明 accept 了连接）时，`connect_stream` 会无限 await。
/// 不兜住就是一条永不 settle 的命令：占位标签退不出去，前端全局 busy 也被永久挂住。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);
/// 握手指看门狗的轮询粒度。
const HANDSHAKE_TICK: Duration = Duration::from_secs(1);
/// 经跳板机打开 direct-tcpip 的超时：russh 的开通道原语没有 per-call 超时，
/// 半死的跳板连接会把它一起挂住，与 [`HANDSHAKE_TIMEOUT`] 同一类坑。
const JUMP_OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// 给 SSH 握手指加「无进展即中止」的看门狗：正常握手数秒内完成，对端不回包时在
/// [`HANDSHAKE_TIMEOUT`] 内报错脱离死等。
///
/// 唯一例外是首次连接的主机密钥确认：那段要等人点弹窗（上限 120s，见 host_key 的
/// `DECISION_TIMEOUT`），故只要还有待裁决请求就把累计清零，交回裁决自己的超时收场。
/// 只透传内层结果，错误归因仍由调用方按 `issue` 槽位处理。
async fn guard_handshake<T, E>(
    app: &AppHandle,
    fut: impl Future<Output = Result<T, E>>,
    host: &str,
    port: u16,
) -> Result<Result<T, E>, AppError> {
    let mut fut = std::pin::pin!(fut);
    let mut stalled = Duration::ZERO;
    loop {
        tokio::select! {
            r = &mut fut => return Ok(r),
            _ = tokio::time::sleep(HANDSHAKE_TICK) => {
                if app.state::<AppState>().host_keys.has_pending() {
                    stalled = Duration::ZERO;
                    continue;
                }
                stalled += HANDSHAKE_TICK;
                if stalled >= HANDSHAKE_TIMEOUT {
                    return Err(AppError::Ssh(format!(
                        "SSH 握手超时：{host}:{port} TCP 已连通，但 {}s 内未完成密钥协商（对端可能不是 SSH 服务或已无响应）",
                        HANDSHAKE_TIMEOUT.as_secs()
                    )));
                }
            }
        }
    }
}

/// 连接失败的归因：主机密钥槽位有内容则优先它，否则按普通 SSH 错误处理。
fn connect_error(issue: &Arc<Mutex<Option<Issue>>>, e: russh::Error) -> AppError {
    match issue.lock().unwrap().take() {
        Some(issue) => issue.into_error(),
        None => AppError::from(e),
    }
}

/// 认证阶段超时：TCP 已连、KEX 已过，但服务端在认证往返上不发包（PAM 挂起、畸形实现、
/// 中间设备透明 accept）时 russh 的 `authenticate_*` 会无限 await，命令永不 settle、
/// 占位标签永久「正在连接…」。与握手指 [`HANDSHAKE_TIMEOUT`] 同一类坑。
const AUTH_TIMEOUT: Duration = Duration::from_secs(20);
/// 认证看门狗的轮询粒度。
const AUTH_TICK: Duration = Duration::from_secs(1);

/// 给认证阶段加「无进展即中止」的看门狗：纯网络往返在 [`AUTH_TIMEOUT`] 内无进展即报错脱离死等。
/// 唯一例外是 keyboard-interactive / 无密码弹窗：那段要等人作答（另有 120s 裁决超时），
/// 故只要还有待作答的 kbd 请求就把累计清零，交回裁决自己的超时收场（同 [`guard_handshake`]）。
async fn guard_auth<F>(app: &AppHandle, fut: F, host: &str) -> Result<(), AppError>
where
    F: Future<Output = Result<(), AppError>>,
{
    let mut fut = std::pin::pin!(fut);
    let mut stalled = Duration::ZERO;
    loop {
        tokio::select! {
            r = &mut fut => return r,
            _ = tokio::time::sleep(AUTH_TICK) => {
                if app.state::<AppState>().ssh_kbd.has_pending() {
                    stalled = Duration::ZERO;
                    continue;
                }
                stalled += AUTH_TICK;
                if stalled >= AUTH_TIMEOUT {
                    return Err(AppError::Ssh(format!(
                        "SSH 认证超时：{host} TCP 已连通并通过密钥协商，但 {}s 内未完成认证（服务端认证子系统可能已挂起或无响应）",
                        AUTH_TIMEOUT.as_secs()
                    )));
                }
            }
        }
    }
}

/// 开通道 / 申请 PTY / 启动 shell 等纯协议往返的 per-call 超时：russh 这些原语无内建超时，
/// 半死的连接会把它一起挂住（与 [`JUMP_OPEN_TIMEOUT`] 同类）。均无人工裁决参与，直接套墙钟。
const PROTO_TIMEOUT: Duration = Duration::from_secs(15);

async fn proto<T>(
    what: &str,
    fut: impl Future<Output = Result<T, russh::Error>>,
) -> Result<T, AppError> {
    tokio::time::timeout(PROTO_TIMEOUT, fut)
        .await
        .map_err(|_| {
            AppError::Ssh(format!(
                "{what}超时：服务端 {}s 内未响应",
                PROTO_TIMEOUT.as_secs()
            ))
        })?
        .map_err(AppError::from)
}

async fn authenticate(
    app: &AppHandle,
    handle: &mut Handle,
    spec: &ConnectSpec,
) -> Result<(), AppError> {
    // 自动协商：先发 none 探测服务端可用方法，按优先级 kbd > password 选路
    if let AuthMaterial::Auto { password } = &spec.auth {
        return auto_authenticate(app, handle, spec, password.as_ref()).await;
    }
    // SSH keyboard-interactive（强制路由，不探测）：见 `kbd` 模块
    if matches!(spec.auth, AuthMaterial::KbdInteractive) {
        return kbd::authenticate(app, &spec.host, handle, spec).await;
    }
    let result = match &spec.auth {
        AuthMaterial::Password(pw) => {
            handle
                .authenticate_password(spec.username.clone(), pw.expose())
                .await
        }
        AuthMaterial::KbdInteractive | AuthMaterial::Auto { .. } => {
            unreachable!("已在函数入口分流")
        }
        auth @ (AuthMaterial::PrivateKey { .. } | AuthMaterial::PrivateKeyFile { .. }) => {
            let key = Arc::new(load_private_key(auth).await?);
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(AppError::from)?
                .flatten();
            handle
                .authenticate_publickey(
                    spec.username.clone(),
                    PrivateKeyWithHashAlg::new(key, hash),
                )
                .await
        }
        AuthMaterial::Interactive => {
            return Err(AppError::Biz {
                key: "err.auth.noPreauth",
                zh: "该协议不支持预认证，请用 SSH/FTP",
            });
        }
    };
    if result.map_err(|e| AppError::Auth(e.to_string()))?.success() {
        Ok(())
    } else {
        Err(AppError::Auth("认证被拒绝：请检查用户名与密码/私钥".into()))
    }
}

/// 自动协商实现：探测 → 有 kbd 走 kbd，否则有密码走密码，否则报错。
async fn auto_authenticate(
    app: &AppHandle,
    handle: &mut Handle,
    spec: &ConnectSpec,
    password: Option<&Secret>,
) -> Result<(), AppError> {
    use russh::MethodKind;

    let probe = handle
        .authenticate_none(spec.username.clone())
        .await
        .map_err(|e| AppError::Auth(format!("认证探测失败：{e}")))?;
    let remaining = match probe {
        russh::client::AuthResult::Success => return Ok(()), // 服务端允许无认证
        russh::client::AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods,
    };
    // 优先级：keyboard-interactive > password（档案已存）
    if remaining.contains(&MethodKind::KeyboardInteractive) {
        return kbd::authenticate(app, &spec.host, handle, spec).await;
    }
    if remaining.contains(&MethodKind::Password) {
        let pw = match password {
            Some(p) => p,
            None => {
                // 无存储密码 → 弹窗向用户要一个（复用 kbd 基础设施）
                let secret = kbd::prompt_password(app, &spec.host).await?;
                let r = handle
                    .authenticate_password(spec.username.clone(), secret.expose())
                    .await
                    .map_err(|e| AppError::Auth(e.to_string()))?;
                if r.success() {
                    return Ok(());
                }
                return Err(AppError::Auth("认证被拒绝：密码不正确".into()));
            }
        };
        let r = handle
            .authenticate_password(spec.username.clone(), pw.expose())
            .await
            .map_err(|e| AppError::Auth(e.to_string()))?;
        if r.success() {
            return Ok(());
        }
        return Err(AppError::Auth("认证被拒绝：密码不正确".into()));
    }
    let methods: Vec<&str> = remaining.iter().map(|m| (&*m).into()).collect();
    Err(AppError::Auth(format!(
        "服务端支持的认证方式({}) 无可用材料（需密码或私钥）",
        methods.join(", ")
    )))
}

/// 解析私钥：PEM 内容直接用，路径则读文件；加密私钥用口令解密。明文用后即清零。
async fn load_private_key(auth: &AuthMaterial) -> Result<PrivateKey, AppError> {
    let (pem, passphrase) = match auth {
        AuthMaterial::PrivateKey { pem, passphrase } => {
            (Zeroizing::new(pem.expose().to_string()), passphrase)
        }
        AuthMaterial::PrivateKeyFile { path, passphrase } => (
            tokio::fs::read_to_string(path)
                .await
                .map(Zeroizing::new)
                .map_err(|e| AppError::Key(format!("读取私钥文件失败：{e}")))?,
            passphrase,
        ),
        AuthMaterial::Password(_)
        | AuthMaterial::Interactive
        | AuthMaterial::KbdInteractive
        | AuthMaterial::Auto { .. } => return Err(AppError::Key("认证方式不是私钥".into())),
    };

    let key = PrivateKey::from_openssh(pem.as_bytes())
        .or_else(|_| PrivateKey::from_bytes(pem.as_bytes()))
        .map_err(|e| AppError::Key(format!("私钥解析失败：{e}")))?;
    if !key.is_encrypted() {
        return Ok(key);
    }
    let pass = passphrase
        .as_ref()
        .ok_or_else(|| AppError::Key("私钥已加密，需要提供口令".into()))?;
    key.decrypt(pass.expose())
        .map_err(|e| AppError::Key(format!("私钥口令错误或已损坏：{e}")))
}

/// 剥掉 ANSI 转义序列后的纯文本（只用于提示符特征判定，不丢普通字节）。
fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            match bytes[i + 1] {
                // CSI：ESC [ … 最终字节 @~
                b'[' => {
                    i += 2;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                // OSC：ESC ] … BEL 或 ESC ] … ESC \
                b']' => {
                    i += 2;
                    while i < bytes.len() && bytes[i] != 0x07 {
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    i = i.saturating_add(1).min(bytes.len());
                }
                _ => i += 2,
            }
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// 输出尾部是否像一个 shell 提示符：去 ANSI 后取最后一行，以 `#` `$` `%` `>` 结尾。
/// 只认最常见的 bash/zsh 形态（含 `[root@host dir]#` 这种带色 PS1）；认不出时
/// 调用方会退回“静置窗口”策略，不会错杀。
fn looks_like_prompt(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&strip_ansi(bytes))
        .trim_end()
        .to_string();
    match text.lines().last() {
        Some(last) => matches!(last.as_bytes().last(), Some(b'#' | b'$' | b'%' | b'>')),
        None => false,
    }
}

/// 登录静默期探测：持续读取并原样转发远端输出，直到确认对端 shell 已画出提示符。
/// - 输出尾部命中提示符特征 → 立即结束（最快路径，不等静置）
/// - 只见过 banner、提示符还没来（profile/bashrc 跑得慢、PS1 非主流）→ 静置窗口到期后再给
///   一次长窗口，仍无则结束
/// - 完全没输出（裸 shell / 无 banner）或硬超时 → 结束
/// 仅用于启动目录注入前的对齐；此时远端尚未执行任何命令，无需 ZMODEM 帧检测。
async fn wait_shell_ready(
    app: &AppHandle,
    id: &str,
    rd: &mut russh::ChannelReadHalf,
    ring: &TerminalRing,
) {
    const QUIET: Duration = Duration::from_millis(400);
    const LONG_QUIET: Duration = Duration::from_millis(1500);
    const CAP: Duration = Duration::from_secs(5);
    const TAIL: usize = 4096;
    let t0 = tokio::time::Instant::now();
    // 最近输出尾部（与正常模式一样先转发再判定，不延迟显示）
    let mut tail: Vec<u8> = Vec::new();
    let mut quiet = QUIET;
    let mut extended = false;
    loop {
        match tokio::time::timeout(quiet, rd.wait()).await {
            // 静置窗口到期：提示符特征命中则收工；否则（只见过 banner）延长一次窗口再等
            Err(_) => {
                if !extended && !tail.is_empty() && !looks_like_prompt(&tail) {
                    extended = true;
                    quiet = LONG_QUIET;
                    continue;
                }
                break;
            }
            Ok(msg) => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    forward(app, id, ring, &data);
                    tail.extend_from_slice(&data);
                    if tail.len() > TAIL {
                        let cut = tail.len() - TAIL;
                        tail.drain(..cut);
                    }
                    if looks_like_prompt(&tail) {
                        break;
                    }
                    extended = false;
                    quiet = QUIET;
                }
                // 通道提前关闭：不再等待
                _ => break,
            },
        }
        if tokio::time::Instant::now() >= t0 + CAP {
            break;
        }
    }
}

/// 终端输出转发：入环形缓冲 + base64 广播给前端 xterm。
fn forward(app: &AppHandle, id: &str, ring: &TerminalRing, data: &[u8]) {
    if data.is_empty() {
        return;
    }
    ring.push(data);
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    let _ = app.emit(
        EVENT_OUTPUT,
        Output {
            session_id: id,
            data: &b64,
        },
    );
}

/// ZMODEM 结束/失败通知（前端 toast 提示）。
fn emit_zm_done(app: &AppHandle, id: &str, ok: bool, message: &str) {
    let _ = app.emit(
        EVENT_ZM_DONE,
        serde_json::json!({ "sessionId": id, "ok": ok, "message": message }),
    );
}

/// 启动会话 actor：读写分离，`rd.wait()` 收远端输出，`wr` 送键入/改窗口尺寸。
fn spawn_actor(
    app: AppHandle,
    id: String,
    handle: SshHandle,
    channel: Channel,
    mut rx: mpsc::Receiver<SessionCmd>,
    startup_dir: Option<String>,
    startup_command: Option<String>,
    ring: Arc<TerminalRing>,
) {
    let (mut rd, wr) = channel.split();
    tokio::spawn(async move {
        // 启动目录/启动命令：必须等远端 shell 画出首个提示符再注入。登录瞬间 sshd 尚未打印
        // `Last login`、bash 尚未进入 readline，此时直接写 `cd …\n` 会被 PTY 回显一次
        // （落在 banner/提示符之前）、readline 重绘再回显一次——屏幕看起来像登了两个 shell。
        if startup_dir.is_some() || startup_command.is_some() {
            wait_shell_ready(&app, &id, &mut rd, &ring).await;
        }
        if let Some(dir) = startup_dir {
            let _ = wr
                .data_bytes(format!("cd '{}'\n", dir.replace('\'', "'\\''")).into_bytes())
                .await;
        }
        // 登录后自动执行：原样下发用户命令（不转义，它本身就是一行 shell）+ 换行执行。
        if let Some(cmd) = startup_command {
            let _ = wr.data_bytes(format!("{cmd}\n").into_bytes()).await;
        }

        // OSC 133 shell 集成：整段一行执行，只产生**一个**提示符（多条命令会各打一条 prompt）。
        if shell_integration::ENABLED {
            let _ = wr
                .data_bytes(shell_integration::one_liner().into_bytes())
                .await;
        }

        // 终端内 ZMODEM：None = 正常转发（带握手检测）
        let mut zm: Option<ZmMode> = None;
        // 检测窗口：只存“已转发”数据的尾部 5 字节（`**\x18B` + 方向首字节），
        // 仅用于跨块拼接判定；未命中立即转发，显示零延迟、无滞留
        let mut det: Vec<u8> = Vec::new();

        // true = 用户主动关闭；false = 远端断开/网络中断（可重连）
        let closed_by_user = loop {
            if let Some(mode) = zm.take() {
                match mode {
                    ZmMode::Pending {
                        dir,
                        mut buf,
                        since,
                    } => {
                        // 等前端确认；期间远端可能重发握手帧，累积即可
                        let outcome: PendingOutcome = tokio::select! {
                            msg = rd.wait() => match msg {
                                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                                    buf.extend_from_slice(&data);
                                    PendingOutcome::Stay(buf.clone())
                                }
                                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => PendingOutcome::Exit(false),
                                _ => PendingOutcome::Stay(buf.clone()),
                            },
                            cmd = rx.recv() => match cmd {
                                Some(SessionCmd::ZmodemStart { local_path }) => PendingOutcome::Start(local_path),
                                Some(SessionCmd::ZmodemCancel) => PendingOutcome::Cancel,
                                Some(SessionCmd::Close) | None => PendingOutcome::Exit(true),
                                Some(SessionCmd::Resize(cols, rows)) => {
                                    let _ = wr.window_change(cols, rows, 0, 0).await;
                                    PendingOutcome::Stay(buf.clone())
                                }
                                Some(SessionCmd::Write(_)) => PendingOutcome::Stay(buf.clone()),
                            },
                            _ = tokio::time::sleep_until(since + ZM_PENDING_SECS) => PendingOutcome::Cancel,
                        };
                        match outcome {
                            PendingOutcome::Stay(buf) => {
                                zm = Some(ZmMode::Pending { dir, buf, since });
                            }
                            PendingOutcome::Start(local_path) => {
                                debug!("[zm] start {dir:?} path={local_path} buf={}B", buf.len());
                                let p = std::path::Path::new(&local_path);
                                let started = match dir {
                                    ZmDir::Upload => TermZm::upload(p, &buf),
                                    ZmDir::Download => TermZm::download(p, &buf),
                                };
                                match started {
                                    Ok(machine) => {
                                        zm = Some(ZmMode::Running {
                                            machine,
                                            since: tokio::time::Instant::now(),
                                        });
                                    }
                                    Err(e) => {
                                        let _ = wr.data_bytes(CAN.to_vec()).await;
                                        emit_zm_done(&app, &id, false, &e.to_string());
                                    }
                                }
                            }
                            PendingOutcome::Cancel => {
                                debug!("[zm] pending cancel (dismissed or timeout)");
                                let _ = wr.data_bytes(CAN.to_vec()).await;
                            }
                            PendingOutcome::Exit(by_user) => break by_user,
                        }
                    }
                    ZmMode::Running { mut machine, since } => {
                        // since 语义：最后活动时间（任何收发都刷新），超时判定见下
                        let mut last = since;
                        // 1) 泵本地侧（上传读文件产帧 / 下载发应答）
                        match machine.pump() {
                            Ok((wb, done)) => {
                                if !wb.is_empty() {
                                    debug!("[zm] pump -> wire {}B done={done}", wb.len());
                                    let _ = wr.data_bytes(wb).await;
                                    last = tokio::time::Instant::now();
                                }
                                if let Some((name, size)) = machine.take_file_info() {
                                    debug!("[zm] file {name} {size}B");
                                    let _ = app.emit(
                                        EVENT_ZM_FILE,
                                        serde_json::json!({
                                            "sessionId": id,
                                            "name": name,
                                            "size": size,
                                        }),
                                    );
                                }
                                if done {
                                    emit_zm_done(&app, &id, true, "");
                                    continue;
                                }
                            }
                            Err(e) => {
                                let _ = wr.data_bytes(CAN.to_vec()).await;
                                emit_zm_done(&app, &id, false, &e.to_string());
                                continue;
                            }
                        }
                        // 2) 等远端帧 / 用户指令；stay=false 表示本轮已终态
                        let idle_deadline = last + ZM_IDLE_SECS;
                        let stay: bool = tokio::select! {
                            msg = rd.wait() => match msg {
                                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                                    debug!(
                                        "[zm] feed {}B {:02x?}",
                                        data.len(),
                                        &data[..data.len().min(80)]
                                    );
                                    last = tokio::time::Instant::now();
                                    match machine.feed(&data) {
                                        Ok((wb, done, rest)) => {
                                            if !wb.is_empty() {
                                                debug!("[zm] feed -> wire {}B done={done}", wb.len());
                                                let _ = wr.data_bytes(wb).await;
                                            }
                                            // 状态机不消费的字节（rz 退出后的提示符等）照常显示，
                                            // 否则控制台看起来像“卡住”
                                            if !rest.is_empty() {
                                                debug!("[zm] feed -> show {}B", rest.len());
                                                forward(&app, &id, &ring, &rest);
                                            }
                                            if done {
                                                emit_zm_done(&app, &id, true, "");
                                                false
                                            } else {
                                                true
                                            }
                                        }
                                        Err(e) => {
                                            let _ = wr.data_bytes(CAN.to_vec()).await;
                                            emit_zm_done(&app, &id, false, &e.to_string());
                                            false
                                        }
                                    }
                                }
                                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break false,
                                _ => true,
                            },
                            cmd = rx.recv() => match cmd {
                                Some(SessionCmd::Close) => break true,
                                Some(SessionCmd::ZmodemStart { .. }) => true,
                                Some(SessionCmd::ZmodemCancel) => {
                                    let _ = wr.data_bytes(CAN.to_vec()).await;
                                    emit_zm_done(&app, &id, false, "已取消");
                                    false
                                }
                                Some(SessionCmd::Resize(cols, rows)) => {
                                    let _ = wr.window_change(cols, rows, 0, 0).await;
                                    true
                                }
                                Some(SessionCmd::Write(_)) => true,
                                None => break true,
                            },
                            _ = tokio::time::sleep_until(idle_deadline) => {
                                let _ = wr.data_bytes(CAN.to_vec()).await;
                                emit_zm_done(&app, &id, false, "ZMODEM 传输超时");
                                false
                            }
                        };
                        if stay {
                            zm = Some(ZmMode::Running {
                                machine,
                                since: last,
                            });
                        }
                    }
                }
            } else {
                // ---- 正常模式：转发 + 握手检测 ----
                tokio::select! {
                    msg = rd.wait() => match msg {
                        Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                            // 零滞留检测：窗口只存“已转发”的尾部字节，新数据先拼接判定，
                            // 未命中立即转发（显示零延迟）；命中则自帧头起进入 ZMODEM。
                            let mut combo = det.clone();
                            combo.extend_from_slice(&data);
                            if let Some((pos, dir)) = find_handshake(&combo) {
                                // pos 落在已转发的窗口内 = 帧前缀已显示（不可见控制序列，无害），
                                // 此时 data 整体属于帧及其后，全部进 Pending
                                let rel = pos.saturating_sub(det.len());
                                let text = data[..rel].to_vec();
                                let frame = combo[pos..].to_vec();
                                det.clear();
                                debug!("[zm] handshake {dir:?} pos={pos} frame={}B", frame.len());
                                forward(&app, &id, &ring, &text);
                                let _ = app.emit(
                                    EVENT_ZM_REQUEST,
                                    serde_json::json!({ "sessionId": id, "direction": dir.as_str() }),
                                );
                                zm = Some(ZmMode::Pending {
                                    dir,
                                    buf: frame,
                                    since: tokio::time::Instant::now(),
                                });
                            } else {
                                forward(&app, &id, &ring, &data);
                                // 窗口只留尾部 5 字节（`**\x18B` + 方向首字节），跨块可拼接
                                det.extend_from_slice(&data);
                                const KEEP: usize = 5;
                                if det.len() > KEEP {
                                    let cut = det.len() - KEEP;
                                    det.drain(..cut);
                                }
                            }
                        }
                        Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break false,
                        _ => {}
                    },
                    cmd = rx.recv() => match cmd {
                        Some(SessionCmd::Write(bytes)) => { let _ = wr.data_bytes(bytes).await; }
                        Some(SessionCmd::Resize(cols, rows)) => { let _ = wr.window_change(cols, rows, 0, 0).await; }
                        Some(SessionCmd::Close) | None => break true,
                        Some(SessionCmd::ZmodemStart { .. }) | Some(SessionCmd::ZmodemCancel) => {}
                    },
                }
            }
        };

        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;
        if closed_by_user {
            // `discard` 内部已含 teardown（丢 SFTP 通道 + 取消在跑的传输）与 `closed` 广播
            discard(&app, &id);
        } else {
            teardown(&app, &id);
            // 断开但保留条目：前端显示断线状态并提供重连
            // 句柄一并丢弃：旧连接已不可用，SFTP 需等重连后重建
            app.state::<AppState>().sessions.update(&id, |e| {
                e.tx = None;
                e.handle = None;
                e.state = SessionState::Disconnected;
            });
            emit_state(
                &app,
                &id,
                SessionState::Disconnected,
                Some("远端已断开连接"),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::looks_like_prompt;

    /// 启动目录注入时机判定：banner 不算提示符，真提示符（含带色 PS1）要能认出。
    #[test]
    fn prompt_detection_matches_real_prompts_only() {
        // 普通 PS1 与 root 提示符（尾部带空格）
        assert!(looks_like_prompt(b"[root@xdy-train ~]# "));
        assert!(looks_like_prompt(b"user@host:~$ "));
        // 带颜色的 PS1：CSI 序列必须在判定前剥掉
        assert!(looks_like_prompt(
            "\x1b[01;32mroot@host\x1b[0m \x1b[38;5;208m~\x1b[0m# ".as_bytes()
        ));
        // banner / MOTD 不是提示符，不能提前注入 cd
        assert!(!looks_like_prompt(
            b"Last login: Mon Sep 28 15:33:45 2026 from 10.168.80.225\r\n"
        ));
        assert!(!looks_like_prompt(b"Have a lot of fun...\r\n"));
        assert!(!looks_like_prompt(b""));
    }
}
