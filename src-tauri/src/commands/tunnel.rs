//! SSH 隧道 / 端口转发 IPC。
//!
//! 两组命令：**配置 CRUD**（`tunnel_list/save/delete`，落 SQLite，走 [`blocking`]）与
//! **运行时转发**（`tunnel_open/close/active`，在既有会话上开/关 `-L`/`-R`/`-D`）。
//!
//! 安全红线：隧道只存/传**地址与端口**，绝不含任何凭证；`tunnel_open`
//! 复用会话**已认证**的连接句柄，绝不二次索取密码/私钥。

use serde::Deserialize;
use tauri::AppHandle;

use super::{blocking, state};
use crate::db::tunnel::{TunnelConfig, TunnelKind};
use crate::error::AppError;
use crate::ssh::tunnel::ActiveTunnel;
use crate::state::AppState;

/// 名称字符上限（超长截断，避免异常长名拖累列表渲染）。
const MAX_NAME_CHARS: usize = 80;

/// 隧道配置保存入参：`id` 为空 = 新建，`Some` = 编辑既有配置。端口用 `u32` 收，
/// 越界时回**可读**的 `Invalid`（而非 serde 的裸反序列化错误）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelInput {
    pub id: Option<String>,
    /// 归属连接档案（可空 = 不绑定，供临时会话手填目标）。
    pub profile_id: Option<String>,
    pub name: String,
    /// `local` / `remote` / `dynamic`；缺省 `local`。
    pub kind: Option<String>,
    /// 本地监听地址；空 = `127.0.0.1`（只本机可达）。
    pub local_host: Option<String>,
    /// 本地监听端口；0/缺省 = 由系统分配随机可用端口。
    pub local_port: Option<u32>,
    pub remote_host: String,
    pub remote_port: u32,
    /// 是否启用；缺省 `true`。
    pub enabled: Option<bool>,
}

/// 列已保存的隧道配置：`profile_id` 为空列全部，否则只列该连接的。
#[tauri::command]
pub async fn tunnel_list(
    app: AppHandle,
    profile_id: Option<String>,
) -> Result<Vec<TunnelConfig>, AppError> {
    blocking(move || state(&app).db.list_tunnels(profile_id.as_deref())).await
}

/// 新建或编辑一条隧道配置；返回落库后的配置。
#[tauri::command]
pub async fn tunnel_save(app: AppHandle, input: TunnelInput) -> Result<TunnelConfig, AppError> {
    blocking(move || save_tunnel(&state(&app), input)).await
}

/// 删除一条隧道配置，返回是否删到（存在与否）。删除配置**不影响**已在跑的转发。
#[tauri::command]
pub async fn tunnel_delete(app: AppHandle, id: String) -> Result<bool, AppError> {
    blocking(move || state(&app).db.delete_tunnel(&id)).await
}

/// 在某条**已连接**的 SSH 会话上开启一条转发（按已保存的配置 id）。
///
/// 复用会话已认证句柄，绝不二次索取凭证；
/// 返回运行态（含**实际**端口，配置填 0 时此处是系统分配值）。
#[tauri::command]
pub async fn tunnel_open(
    app: AppHandle,
    session_id: String,
    tunnel_id: String,
) -> Result<ActiveTunnel, AppError> {
    let cfg = blocking({
        let app = app.clone();
        move || {
            state(&app)
                .db
                .get_tunnel(&tunnel_id)?
                .ok_or_else(|| AppError::Invalid(format!("隧道配置不存在：{tunnel_id}")))
        }
    })
    .await?;

    if !cfg.kind.is_supported() {
        return Err(AppError::Invalid(format!(
            "不支持的隧道类型：{}",
            cfg.kind.as_str()
        )));
    }

    // 会话句柄：断线/不存在 → NoSuchSession（前端提示先连上再开隧道）
    let st = state(&app);
    let handle = st
        .sessions
        .handle(&session_id)
        .ok_or_else(|| AppError::NoSuchSession(session_id.clone()))?;

    if cfg.kind == TunnelKind::Remote {
        // -R：remote_host:port = 远端监听；local_host:port = 回连的本地目标
        return st
            .tunnels
            .open_remote(
                &session_id,
                &cfg.name,
                &cfg.remote_host,
                cfg.remote_port,
                &cfg.local_host,
                cfg.local_port,
                handle,
            )
            .await;
    }
    if cfg.kind == TunnelKind::Dynamic {
        // -D：本机 SOCKS 代理，目标由客户端 CONNECT 指定
        return st
            .tunnels
            .open_socks(
                &session_id,
                &cfg.name,
                &cfg.local_host,
                cfg.local_port,
                handle,
            )
            .await;
    }

    st.tunnels
        .open_local(
            &session_id,
            &cfg.name,
            &cfg.local_host,
            cfg.local_port,
            &cfg.remote_host,
            cfg.remote_port,
            handle,
        )
        .await
}

/// 关闭一条运行中的转发（按 `run_id`），返回是否确实关掉了。
#[tauri::command]
pub async fn tunnel_close(app: AppHandle, run_id: String) -> Result<bool, AppError> {
    Ok(state(&app).tunnels.close(&run_id))
}

/// 列运行中的转发：`session_id` 为空列全部，否则只列该会话的。
#[tauri::command]
pub async fn tunnel_active(
    app: AppHandle,
    session_id: Option<String>,
) -> Result<Vec<ActiveTunnel>, AppError> {
    Ok(state(&app).tunnels.list(session_id.as_deref()))
}

/// 校验 + 落库（新建或按 id 更新，`created_at` 保持首次写入值）。
fn save_tunnel(st: &AppState, input: TunnelInput) -> Result<TunnelConfig, AppError> {
    let name = clip_chars(input.name.trim(), MAX_NAME_CHARS);
    if name.is_empty() {
        return Err(AppError::Invalid("隧道名称不能为空".into()));
    }

    // 本地监听地址：空则退回回环地址（默认只本机可达，最安全）
    let local_host = match input.local_host.unwrap_or_default().trim() {
        "" => "127.0.0.1".to_string(),
        h => h.to_string(),
    };
    let kind = TunnelKind::parse(input.kind.as_deref().unwrap_or("local"));

    // dynamic(-D) 只用本地 SOCKS 监听；remote(-R) 的 remote_* 是远端监听、local_* 是回连目标。
    let (remote_host, remote_port) = if kind == TunnelKind::Dynamic {
        (String::new(), 0)
    } else {
        let h = input.remote_host.trim().to_string();
        if h.is_empty() {
            return Err(AppError::Invalid("目标主机不能为空".into()));
        }
        let rp = input.remote_port;
        if rp == 0 || rp > 65535 {
            return Err(AppError::Invalid("目标端口须在 1-65535 之间".into()));
        }
        (h, rp)
    };

    let local_port = input.local_port.unwrap_or(0);
    if local_port > 65535 {
        return Err(AppError::Invalid(
            "监听端口须在 0-65535 之间（0 = 随机）".into(),
        ));
    }
    let enabled = input.enabled.unwrap_or(true);
    let profile_id = input
        .profile_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let id = input.id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let mut cfg = match id {
        Some(id) => st
            .db
            .get_tunnel(id)?
            .ok_or_else(|| AppError::Invalid(format!("隧道配置不存在：{id}")))?,
        None => TunnelConfig::new(
            profile_id.clone(),
            name.as_str(),
            kind,
            local_host.as_str(),
            local_port as u16,
            remote_host.as_str(),
            remote_port as u16,
            enabled,
        ),
    };
    // 统一覆盖可变字段（新建时值相同，编辑时更新）；id / created_at 保持不变
    cfg.profile_id = profile_id;
    cfg.name = name;
    cfg.kind = kind;
    cfg.local_host = local_host;
    cfg.local_port = local_port as u16;
    cfg.remote_host = remote_host;
    cfg.remote_port = remote_port as u16;
    cfg.enabled = enabled;
    cfg.updated_at = crate::db::now_ms();
    st.db.upsert_tunnel(&cfg)?;
    Ok(cfg)
}

/// 按字符数截断（多字节安全），超长加省略号。
fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}
