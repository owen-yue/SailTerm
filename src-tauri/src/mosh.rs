//! Mosh 会话：包装本机 `mosh` 客户端进程，复用本地终端的 PTY 通道。
//!
//! 不重写 MOSH 协议（SSP/AES/UDP 过重）；要求 PATH 上有 `mosh`。凭证由 mosh 自己
//! 走 ssh-agent / 密钥，**SailTerm 保险库不代填**（与 Telnet 同属无预认证交互）。

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tauri::{AppHandle, Manager};

use crate::error::AppError;
use crate::local::{self, PtyBundle};
use crate::session::{
    emit_state, Reconnect, SessionEntry, SessionKind, SessionState, TerminalRing,
};
use crate::state::AppState;

/// Mosh 连接参数（host 为必填；user/port 可选）。
#[derive(Clone, Debug, Default)]
pub struct MoshSpec {
    pub user: String,
    pub host: String,
    /// MOSH_SERVER_PORT；0 = 默认。
    pub port: u16,
}

impl MoshSpec {
    fn argv(&self) -> (String, Vec<String>, Vec<(String, String)>) {
        let target = if self.user.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.user, self.host)
        };
        let mut env = Vec::new();
        if self.port > 0 {
            env.push(("MOSH_SERVER_PORT".to_string(), self.port.to_string()));
        }
        ("mosh".to_string(), vec![target], env)
    }
}

/// 打开 Mosh 会话（拉起本机 mosh）。
pub async fn connect(
    app: AppHandle,
    spec: MoshSpec,
    cols: u32,
    rows: u32,
    reconnect: Reconnect,
) -> Result<String, AppError> {
    let bundle = spawn_mosh(&spec, cols, rows)?;
    let sessions = &app.state::<AppState>().sessions;
    let id = sessions.next_id();
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let ring = TerminalRing::new();
    sessions.insert(SessionEntry {
        id: id.clone(),
        label: format!("mosh@{}", spec.host),
        host: spec.host.clone(),
        profile_id: reconnect.profile_id(),
        state: SessionState::Connected,
        kind: SessionKind::Mosh,
        tx: Some(tx),
        handle: None,
        jumps: Vec::new(),
        ftp: None,
        ftp_epoch: 0,
        reconnect,
        ring: ring.clone(),
        port: spec.port,
        latency_ms: None,
        // 非 SSH 协议无协商快照（FTP 有自己的一份，见 files::ftp 的 FtpInfo）
        negotiation: None,
        ftp_info: None,
    });
    local::spawn_actor(app.clone(), id.clone(), bundle, rx, ring);
    Ok(id)
}

/// 重连：同一 session_id 重开 mosh 进程。
pub async fn reconnect(
    app: AppHandle,
    id: String,
    spec: MoshSpec,
    cols: u32,
    rows: u32,
) -> Result<(), AppError> {
    let bundle = spawn_mosh(&spec, cols, rows)?;
    let (tx, rx) = tokio::sync::mpsc::channel(64);
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
    local::spawn_actor(app, id, bundle, rx, ring);
    Ok(())
}

fn spawn_mosh(spec: &MoshSpec, cols: u32, rows: u32) -> Result<PtyBundle, AppError> {
    if spec.host.trim().is_empty() {
        return Err(AppError::Invalid("Mosh 主机不能为空".into()));
    }
    if which_mosh().is_none() {
        return Err(AppError::Io(
            "未找到 mosh 客户端：请先安装 mosh（Windows 可用 scoop/choco 或 WSL）".into(),
        ));
    }
    let (prog, args, envs) = spec.argv();
    let sys = native_pty_system();
    let pair = sys
        .openpty(PtySize {
            rows: rows.max(2) as u16,
            cols: cols.max(2) as u16,
            pixel_width: (cols.max(2) as u16).saturating_mul(8),
            pixel_height: (rows.max(2) as u16).saturating_mul(16),
        })
        .map_err(|e| AppError::Io(format!("创建 PTY 失败：{e}")))?;
    let mut cb = CommandBuilder::new(&prog);
    for a in &args {
        cb.arg(a);
    }
    for (k, v) in &envs {
        cb.env(k, v);
    }
    let child = pair
        .slave
        .spawn_command(cb)
        .map_err(|e| AppError::Io(format!("启动 mosh 失败：{e}")))?;
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| AppError::Io(format!("克隆 PTY 读端失败：{e}")))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| AppError::Io(format!("取 PTY 写端失败：{e}")))?;
    Ok(PtyBundle {
        master: pair.master,
        child,
        reader,
        writer,
    })
}

/// `mosh` 是否在 PATH 上。
fn which_mosh() -> Option<std::path::PathBuf> {
    let name = if cfg!(windows) { "mosh.exe" } else { "mosh" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}
