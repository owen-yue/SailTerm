//! Telnet 会话：明文 TCP + 最小 IAC 协商，接入与 SSH/本地**相同**的会话 actor 模型。
//!
//! 无预认证——常见 Telnet 服务在 banner 之后弹登录提示，凭据由用户在终端里键入
//! （[`AuthMaterial::Interactive`]）。IAC 序列在读写两侧过滤/应答，终端只看到纯文本；
//! 应答按选项语义与方向定（服务端回显的 ECHO 与两端 SGA 答应、其余拒绝，见 [`IacFilter::feed`]）。
//!
//! 安全：Telnet 全程明文，**绝不**在其上传输 SailTerm 保险库里的凭证；
//! 本模块不读保险库、不记密码。

use std::sync::Arc;

use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use crate::error::AppError;
use crate::session::{
    discard, emit_state, teardown, Reconnect, SessionCmd, SessionEntry, SessionKind, SessionState,
    TerminalRing,
};
use crate::ssh::EVENT_OUTPUT;
use crate::state::AppState;

/// 终端输出事件负载（与 SSH/本地同构）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Output<'a> {
    session_id: &'a str,
    data: &'a str,
}

/// Telnet IAC（Interpret As Command）。
const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;

/// 建立 Telnet 会话：TCP 连通即注册（登录交互在终端里完成）。
pub async fn connect(
    app: AppHandle,
    host: String,
    port: u16,
    reconnect: Reconnect,
) -> Result<String, AppError> {
    let stream = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| AppError::Io(format!("Telnet 连接 {host}:{port} 失败：{e}")))?;
    let sessions = &app.state::<AppState>().sessions;
    let id = sessions.next_id();
    let (tx, rx) = mpsc::channel(64);
    let ring = TerminalRing::new();
    sessions.insert(SessionEntry {
        id: id.clone(),
        label: format!("telnet@{host}"),
        host,
        profile_id: reconnect.profile_id(),
        state: SessionState::Connected,
        kind: SessionKind::Telnet,
        tx: Some(tx),
        handle: None,
        jumps: Vec::new(),
        ftp: None,
        ftp_epoch: 0,
        reconnect,
        ring: ring.clone(),
        port,
        latency_ms: None,
        // 非 SSH 协议无协商快照（FTP 有自己的一份，见 files::ftp 的 FtpInfo）
        negotiation: None,
        ftp_info: None,
    });
    spawn_actor(app.clone(), id.clone(), stream, rx, ring);
    Ok(id)
}

/// 断线重连：沿用同一 `session_id` 与终端内容。
pub async fn reconnect(
    app: AppHandle,
    id: String,
    host: String,
    port: u16,
) -> Result<(), AppError> {
    let stream = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| AppError::Io(format!("Telnet 重连 {host}:{port} 失败：{e}")))?;
    let (tx, rx) = mpsc::channel(64);
    let ring = app
        .state::<AppState>()
        .sessions
        .update(&id, |e| {
            e.tx = Some(tx);
            e.state = SessionState::Connected;
            e.ring.clone()
        })
        .ok_or_else(|| AppError::NoSuchSession(id.clone()))?;
    emit_state(&app, &id, SessionState::Connected, None);
    spawn_actor(app, id, stream, rx, ring);
    Ok(())
}

/// 读侧 IAC 过滤器：剥掉协商序列，把需要应答的选项写回 `pending` 应答。
#[derive(Default)]
struct IacFilter {
    /// 0=普通；1=刚见 IAC；2=见 IAC+命令（等选项字节）；3=子协商中；4=子协商里再见 IAC
    state: u8,
    cmd: u8,
    /// 本次子协商已吞掉的字节数（见 [`MAX_SB_LEN`] 护栏）
    sb_len: usize,
}

/// Telnet 选项码（RFC 854）：ECHO 与 SUPPRESS-GO-AHEAD 是登录交互的两条命脉。
const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;
/// 子协商容忍上限：对端漏发 `IAC SE` 时，状态机若不加护栏会永久停在「子协商中」，
/// 把后续所有服务端输出无声吞掉（症状是终端突然什么都不显示）——超限即复位。
const MAX_SB_LEN: usize = 1024;

/// 服务端**自己承担**某选项（`WILL`/`WONT`）时，哪些值得答应：ECHO（它来回显我们打的字）与 SGA。
const fn ok_for_server(opt: u8) -> bool {
    matches!(opt, OPT_ECHO | OPT_SGA)
}

/// 服务端要求**客户端**承担某选项（`DO`/`DONT`）时，哪些值得答应：只 SGA。
/// 尤其不能对 `DO ECHO` 回 `WILL`——本客户端没有本地回显实现，那是空头承诺，
/// 结果是两边都以为对方负责回显，登录界面打一个字都看不见。
const fn ok_for_client(opt: u8) -> bool {
    opt == OPT_SGA
}

impl IacFilter {
    /// 喂入一块原始字节，返回可展示的纯文本 + 待发送的应答字节。
    fn feed(&mut self, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut text = Vec::with_capacity(input.len());
        let mut reply = Vec::new();
        for &b in input {
            match self.state {
                0 => {
                    if b == IAC {
                        self.state = 1;
                    } else {
                        text.push(b);
                    }
                }
                1 => match b {
                    IAC => {
                        // 转义的 255 → 数据里一个 255
                        text.push(IAC);
                        self.state = 0;
                    }
                    WILL | WONT | DO | DONT => {
                        self.cmd = b;
                        self.state = 2;
                    }
                    SB => {
                        self.state = 3;
                        self.sb_len = 0;
                    }
                    _ => self.state = 0, // 其它单字节命令（NOP / GA 等）直接丢
                },
                2 => {
                    // 应答按**选项语义**与**方向**逐条定，不能一律拒绝：
                    // - 服务端 `WILL ECHO` 必须回 `DO`。登录提示阶段靠服务端回显，
                    //   回 `DONT` 等于命令它别回显——用户打字屏幕毫无反应（光标都不动）。
                    //   密码阶段服务端自己发 `WONT ECHO`，那时回 `DONT` 才是对的。
                    // - 服务端 `DO ECHO`（要客户端回显）反而回 `WONT`：见 [`ok_for_client`]。
                    // - SGA 两端都答应：拒了服务端可能等 `IAC GA` 才继续下发，输出成块卡住。
                    // - 其余选项（BINARY / NEW-ENVIRON / STATUS …）拒绝，保持纯文本模式。
                    let neg = match self.cmd {
                        WILL => {
                            if ok_for_server(b) {
                                DO
                            } else {
                                DONT
                            }
                        }
                        WONT => DONT,
                        DO => {
                            if ok_for_client(b) {
                                WILL
                            } else {
                                WONT
                            }
                        }
                        _ => WONT, // DONT
                    };
                    reply.extend_from_slice(&[IAC, neg, b]);
                    self.state = 0;
                }
                3 => {
                    if b == IAC {
                        self.state = 4;
                    } else if self.bump_sb() {
                        self.state = 0;
                    }
                }
                _ => {
                    // 子协商结束（IAC SE）；漏发 SE 时由 sb_len 护栏兜底复位。
                    if b == SE {
                        self.state = 0;
                        self.sb_len = 0;
                    } else {
                        self.state = if self.bump_sb() { 0 } else { 3 };
                    }
                }
            }
        }
        (text, reply)
    }

    /// 累计子协商长度；超 [`MAX_SB_LEN`] 返回 true（调用方复位状态机）。
    fn bump_sb(&mut self) -> bool {
        self.sb_len += 1;
        if self.sb_len > MAX_SB_LEN {
            self.sb_len = 0;
            return true;
        }
        false
    }

    /// 写侧：数据里的 255 需双写转义。
    fn escape_out(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 8);
        for &b in data {
            if b == IAC {
                out.push(IAC);
            }
            out.push(b);
        }
        out
    }
}

/// 会话 actor：TCP 读循环 + 指令循环（镜像 `local.rs`）。
/// 读侧 IAC 应答也要写回，故写端用 `Mutex` 共享。
fn spawn_actor(
    app: AppHandle,
    id: String,
    stream: TcpStream,
    mut rx: mpsc::Receiver<SessionCmd>,
    ring: Arc<TerminalRing>,
) {
    let (mut rd, wr) = stream.into_split();
    let wr = Arc::new(tokio::sync::Mutex::new(wr));
    let (eof_tx, mut eof_rx) = oneshot::channel::<()>();

    {
        let app = app.clone();
        let id = id.clone();
        let wr = wr.clone();
        tokio::spawn(async move {
            let mut filter = IacFilter::default();
            let mut buf = [0u8; 8192];
            loop {
                match rd.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let (text, reply) = filter.feed(&buf[..n]);
                        if !reply.is_empty() {
                            let mut w = wr.lock().await;
                            let _ = w.write_all(&reply).await;
                            let _ = w.flush().await;
                        }
                        if !text.is_empty() {
                            ring.push(&text);
                            let b64 = base64::engine::general_purpose::STANDARD.encode(&text);
                            let _ = app.emit(
                                EVENT_OUTPUT,
                                Output {
                                    session_id: &id,
                                    data: &b64,
                                },
                            );
                        }
                    }
                }
            }
            let _ = eof_tx.send(());
        });
    }

    tokio::spawn(async move {
        // true = 用户主动关闭
        let closed_by_user = loop {
            tokio::select! {
                _ = &mut eof_rx => break false,
                cmd = rx.recv() => match cmd {
                    Some(SessionCmd::Write(bytes)) => {
                        let esc = IacFilter::escape_out(&bytes);
                        let mut w = wr.lock().await;
                        let _ = w.write_all(&esc).await;
                        let _ = w.flush().await;
                    }
                    // Telnet 无窗口协商语义，Resize 忽略（远端由 stty 自己管）
                    Some(SessionCmd::Resize(_, _)) => {}
                    // 终端内 ZMODEM 仅 SSH 通道支持，这里忽略
                    Some(SessionCmd::ZmodemStart { .. }) | Some(SessionCmd::ZmodemCancel) => {}
                    Some(SessionCmd::Close) | None => break true,
                },
            }
        };

        if closed_by_user {
            discard(&app, &id);
        } else {
            teardown(&app, &id);
            app.state::<AppState>().sessions.update(&id, |e| {
                e.tx = None;
                e.state = SessionState::Disconnected;
            });
            emit_state(
                &app,
                &id,
                SessionState::Disconnected,
                Some("Telnet 连接已断开"),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iac_filter_strips_and_negotiates() {
        let mut f = IacFilter::default();
        // 普通文本
        let (t, r) = f.feed(b"hello");
        assert_eq!(t, b"hello");
        assert!(r.is_empty());
        // 服务端 WILL ECHO → 必须 DO 接受：登录提示靠它回显，拒了用户打字没反应
        let (t, r) = f.feed(&[IAC, WILL, OPT_ECHO]);
        assert!(t.is_empty());
        assert_eq!(r, vec![IAC, DO, OPT_ECHO]);
        // 服务端 WONT ECHO（密码阶段）→ 回 DONT，不强行要求回显
        let (_, r) = f.feed(&[IAC, WONT, OPT_ECHO]);
        assert_eq!(r, vec![IAC, DONT, OPT_ECHO]);
        // DO SGA → 答应，否则服务端可能等 GA 才继续下发
        let (t, r) = f.feed(&[IAC, DO, OPT_SGA]);
        assert!(t.is_empty());
        assert_eq!(r, vec![IAC, WILL, OPT_SGA]);
        // 未知选项（BINARY=0）仍一律拒绝，保持纯文本
        let (_, r) = f.feed(&[IAC, DO, 0]);
        assert_eq!(r, vec![IAC, WONT, 0]);
        // 服务端要**客户端**回显（DO ECHO）时必须拒绝：我们没有本地回显，
        // 答应它 = 两边都不回显，登录界面一个字都看不见。
        let (_, r) = f.feed(&[IAC, DO, OPT_ECHO]);
        assert_eq!(r, vec![IAC, WONT, OPT_ECHO]);
        // IAC IAC → 数据 255
        let (t, _) = f.feed(&[IAC, IAC]);
        assert_eq!(t, vec![255]);
        // 子协商整段丢弃
        let (t, r) = f.feed(&[IAC, SB, 24, 0, b'x', IAC, SE, b'Z']);
        assert_eq!(t, b"Z");
        assert!(r.is_empty());
    }

    /// 对端漏发 `IAC SE` 时不能永久停在子协商态：否则后续所有输出被无声吞掉。
    #[test]
    fn iac_filter_recovers_from_unterminated_subnegotiation() {
        let mut f = IacFilter::default();
        let mut junk = vec![IAC, SB, 24, 0]; // 有头无尾的子协商
        let xs = vec![b'x'; MAX_SB_LEN * 4];
        let nx = xs.len();
        junk.extend_from_slice(&xs);
        let (t, _) = f.feed(&junk);
        assert_eq!(f.state, 0, "超 MAX_SB_LEN 后应复位到普通态");
        // 护栏之前的子协商内容被吞、复位后的字节归回正文：两头都要成立。
        assert!(!t.is_empty(), "复位后的数据不得被永久吞掉");
        assert!(t.len() < nx, "超限前的内容应被吞掉");
        assert!(t.iter().all(|&b| b == b'x'));
        let (t, _) = f.feed(b"back to text");
        assert_eq!(t, b"back to text", "复位后正常输出必须重新可见");
    }

    /// 正常收尾的子协商不触发护栏，且不影响后续协商。
    #[test]
    fn iac_filter_terminated_subnegotiation_keeps_state_clean() {
        let mut f = IacFilter::default();
        let (t, _) = f.feed(&[IAC, SB, 24, 0, b'a', b'b', IAC, SE]);
        assert!(t.is_empty());
        assert_eq!(f.sb_len, 0, "SE 收尾后计数应归零");
        let (_, r) = f.feed(&[IAC, WILL, OPT_ECHO]);
        assert_eq!(r, vec![IAC, DO, OPT_ECHO]);
    }

    #[test]
    fn escape_out_doubles_iac() {
        assert_eq!(IacFilter::escape_out(&[1, IAC, 2]), vec![1, IAC, IAC, 2]);
    }
}
