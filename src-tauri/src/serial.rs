//! 串口会话：本机 COM / tty 透传，接入与 SSH/本地/Telnet **相同**的会话 actor 模型。
//!
//! 无网络、无凭证（`AuthMaterial::Interactive` 仅表示建连不预取密码）；串口参数
//! （波特率 / 数据位 / 停止位 / 校验 / 流控）随连接档案 `extra.serial` 持久化。
//!
//! 实现：`serial2` 是阻塞 API——读循环跑独立 `std::thread`（读超时轮询取消），
//! 写与关闭由 async 指令循环驱动（键入是小而稀疏的写，同步执行即可，同 `local.rs`）。

use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serial2::{CharSize, FlowControl, Parity, SerialPort, Settings, StopBits};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot};

use crate::error::AppError;
use crate::session::{
    discard, emit_state, teardown, Reconnect, SessionCmd, SessionEntry, SessionKind, SessionState,
    TerminalRing,
};
use crate::ssh::EVENT_OUTPUT;
use crate::state::AppState;

/// 终端输出事件负载（与 SSH/本地/Telnet 同构）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Output<'a> {
    session_id: &'a str,
    data: &'a str,
}

/// 校验位。
#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SerialParity {
    #[default]
    None,
    Even,
    Odd,
}

/// 流控。
#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SerialFlow {
    #[default]
    None,
    Hardware,
    Software,
}

/// 串口线参数（非敏感，可进档案 `extra` / 表单）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct SerialParams {
    /// 波特率；0 = 用默认 115200。
    pub baud: u32,
    /// 数据位 5–8，默认 8。
    pub data_bits: u8,
    /// 停止位 1 或 2，默认 1。
    pub stop_bits: u8,
    pub parity: SerialParity,
    pub flow_control: SerialFlow,
}

impl Default for SerialParams {
    fn default() -> Self {
        Self {
            baud: 115_200,
            data_bits: 8,
            stop_bits: 1,
            parity: SerialParity::None,
            flow_control: SerialFlow::None,
        }
    }
}

impl SerialParams {
    /// 归一化：夹紧越界值到最近合法档，避免设备侧配置炸掉。
    pub fn normalized(mut self) -> Self {
        if self.baud == 0 {
            self.baud = 115_200;
        }
        self.data_bits = self.data_bits.clamp(5, 8);
        if self.stop_bits != 2 {
            self.stop_bits = 1;
        }
        self
    }

    /// 生成 `serial2` 的配置闭包（`IntoSettings`）：先 `set_raw` 再按参数逐项覆盖。
    fn into_settings(self) -> impl FnOnce(Settings) -> std::io::Result<Settings> {
        move |mut s: Settings| {
            s.set_raw();
            s.set_baud_rate(self.baud)?;
            s.set_char_size(match self.data_bits {
                5 => CharSize::Bits5,
                6 => CharSize::Bits6,
                7 => CharSize::Bits7,
                _ => CharSize::Bits8,
            });
            s.set_stop_bits(if self.stop_bits == 2 {
                StopBits::Two
            } else {
                StopBits::One
            });
            s.set_parity(match self.parity {
                SerialParity::None => Parity::None,
                SerialParity::Even => Parity::Even,
                SerialParity::Odd => Parity::Odd,
            });
            s.set_flow_control(match self.flow_control {
                SerialFlow::None => FlowControl::None,
                // serial2 命名：硬件 = RTS/CTS，软件 = XON/XOFF
                SerialFlow::Hardware => FlowControl::RtsCts,
                SerialFlow::Software => FlowControl::XonXoff,
            });
            Ok(s)
        }
    }
}

/// 枚举本机可用串口设备名（Windows: `COM3`；Unix: `/dev/ttyUSB0`）。
pub fn available_ports() -> Vec<String> {
    SerialPort::available_ports()
        .map(|v| {
            v.into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn open_port(name: &str, params: &SerialParams) -> Result<SerialPort, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("串口设备名不能为空".into()));
    }
    let params = params.clone().normalized();
    let mut port = SerialPort::open(name, params.into_settings())
        .map_err(|e| AppError::Io(format!("打开串口 {name} 失败（设备占用或不存在）：{e}")))?;
    // 读超时用于轮询取消；写保持阻塞（键入量小）
    let _ = port.set_read_timeout(Duration::from_millis(200));
    Ok(port)
}

/// 打开串口会话：注册即 Connected，终端透传原始字节。
pub async fn connect(
    app: AppHandle,
    port_name: String,
    params: SerialParams,
    reconnect: Reconnect,
) -> Result<String, AppError> {
    let port = open_port(&port_name, &params)?;
    let sessions = &app.state::<AppState>().sessions;
    let id = sessions.next_id();
    let (tx, rx) = mpsc::channel(64);
    let ring = TerminalRing::new();
    sessions.insert(SessionEntry {
        id: id.clone(),
        label: format!("serial@{}", port_name.trim()),
        host: port_name.trim().to_string(),
        profile_id: reconnect.profile_id(),
        state: SessionState::Connected,
        kind: SessionKind::Serial,
        tx: Some(tx),
        handle: None,
        jumps: Vec::new(),
        ftp: None,
        ftp_epoch: 0,
        reconnect,
        ring: ring.clone(),
        port: 0,
        latency_ms: None,
        // 非 SSH 协议无协商快照（FTP 有自己的一份，见 files::ftp 的 FtpInfo）
        negotiation: None,
        ftp_info: None,
    });
    spawn_actor(app.clone(), id.clone(), port, rx, ring);
    Ok(id)
}

/// 断线/拔线后重开同一串口，沿用 `session_id` 与终端内容。
pub async fn reconnect(
    app: AppHandle,
    id: String,
    port_name: String,
    params: SerialParams,
) -> Result<(), AppError> {
    let port = open_port(&port_name, &params)?;
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
    spawn_actor(app, id, port, rx, ring);
    Ok(())
}

/// 会话 actor：读线程 + 指令循环（镜像 `telnet.rs` / `local.rs`）。
fn spawn_actor(
    app: AppHandle,
    id: String,
    port: SerialPort,
    mut rx: mpsc::Receiver<SessionCmd>,
    ring: Arc<TerminalRing>,
) {
    let reader = match port.try_clone() {
        Ok(r) => r,
        Err(e) => {
            emit_state(
                &app,
                &id,
                SessionState::Disconnected,
                Some(&format!("串口句柄克隆失败：{e}")),
            );
            return;
        }
    };
    let writer = Arc::new(port);
    let stop = Arc::new(AtomicBool::new(false));
    let (eof_tx, mut eof_rx) = oneshot::channel::<()>();

    {
        let app = app.clone();
        let id = id.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        ring.push(&buf[..n]);
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                        let _ = app.emit(
                            EVENT_OUTPUT,
                            Output {
                                session_id: &id,
                                data: &b64,
                            },
                        );
                    }
                    Err(e)
                        if e.kind() == ErrorKind::TimedOut
                            || e.kind() == ErrorKind::Interrupted =>
                    {
                        continue;
                    }
                    Err(_) => break,
                }
            }
            let _ = eof_tx.send(());
        });
    }

    tokio::spawn(async move {
        let closed_by_user = loop {
            tokio::select! {
                _ = &mut eof_rx => break false,
                cmd = rx.recv() => match cmd {
                    Some(SessionCmd::Write(bytes)) => {
                        let w = writer.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            let _ = w.write_all(&bytes);
                            let _ = w.flush();
                        })
                        .await;
                    }
                    // 串口无窗口语义
                    Some(SessionCmd::Resize(_, _)) => {}
                    // 终端内 ZMODEM 仅 SSH 通道支持，这里忽略
                    Some(SessionCmd::ZmodemStart { .. }) | Some(SessionCmd::ZmodemCancel) => {}
                    Some(SessionCmd::Close) | None => break true,
                },
            }
        };

        stop.store(true, Ordering::Relaxed);
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
                Some("串口连接已断开"),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_params_default_is_8n1_115200() {
        let p = SerialParams::default();
        assert_eq!(p.baud, 115_200);
        assert_eq!(p.data_bits, 8);
        assert_eq!(p.stop_bits, 1);
        assert_eq!(p.parity, SerialParity::None);
        assert_eq!(p.flow_control, SerialFlow::None);
    }

    #[test]
    fn serial_params_normalize_clamps() {
        let p = SerialParams {
            baud: 0,
            data_bits: 99,
            stop_bits: 5,
            parity: SerialParity::Odd,
            flow_control: SerialFlow::Hardware,
        }
        .normalized();
        assert_eq!(p.baud, 115_200);
        assert_eq!(p.data_bits, 8);
        assert_eq!(p.stop_bits, 1);
        assert_eq!(p.parity, SerialParity::Odd);
    }

    #[test]
    fn serial_params_serde_roundtrip() {
        let p = SerialParams::default();
        let s = serde_json::to_string(&p).unwrap();
        let got: SerialParams = serde_json::from_str(&s).unwrap();
        assert_eq!(got, p);
        // 旧 JSON 缺字段时用默认值
        let got: SerialParams = serde_json::from_str("{}").unwrap();
        assert_eq!(got, SerialParams::default());
    }
}
