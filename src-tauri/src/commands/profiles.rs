//! 连接档案与分组命令（凭证入库规则见）。
//!
//! 契约纪律：入参里的明文凭证只用于写保险库，**响应体永不回传**；
//! 前端只能看到 `hasCredential` / `hasPrivateKey` 两个布尔标记。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use super::{blocking, state};
use crate::ai::providers::ProviderKind;
use crate::db::ai::{AiProvider, SanitizeLevel};
use crate::db::now_ms;
use crate::db::profile::{AuthType, Group, Profile};
use crate::db::Db;
use crate::error::AppError;
use crate::sealed;
use crate::session::SessionKind;
use crate::state::AppState;
use crate::vault::{keys, Secret, Vault};
use zeroize::Zeroizing;

/// 连接表单入参。凭证字段是一次性的：写入保险库后随 `Secret` 清零，不留副本。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileInput {
    /// `None` = 新建；`Some` = 编辑既有档案。
    pub id: Option<String>,
    pub name: String,
    pub group_id: Option<String>,
    /// 引用的密钥库私钥（ssh_key.id）；Some = 连接握手时从库取该私钥，None = 走内联路径/粘贴。
    #[serde(default)]
    pub private_key_id: Option<String>,
    /// 协议类型；缺省按 SSH 处理（旧前端不传也能用）。
    #[serde(default)]
    pub kind: SessionKind,
    pub host: String,
    pub port: Option<u16>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub auth_type: AuthType,
    /// 磁盘私钥路径（非敏感，落 `extra` 列）。
    pub private_key_path: Option<String>,
    /// 跳板机档案 id（ProxyJump，落 `extra` 列）；空 = 直连。
    pub jump_profile_id: Option<String>,
    /// 串口线参数（仅 `kind=serial`，落 `extra` 列）。
    #[serde(default)]
    pub serial: Option<crate::serial::SerialParams>,
    pub startup_dir: Option<String>,
    /// 登录后自动执行的命令（落 `extra` 列，仅 SSH 类生效）。
    #[serde(default)]
    pub startup_command: Option<String>,
    /// 连接备注（落 `extra` 列，仅展示用）。
    #[serde(default)]
    pub notes: Option<String>,
    /// Agent 转发（-A）开关（落 `extra` 列，仅 SSH 类生效）。
    #[serde(default)]
    pub agent_forward: bool,
    /// X11 转发开关（落 `extra` 列，仅 SSH 类生效）。
    #[serde(default)]
    pub x11_forward: bool,
    /// FTP 主动模式（PORT）开关（落 `extra` 列，仅 FTP/FTPS 生效）。
    #[serde(default)]
    pub ftp_active_mode: bool,
    /// 心跳间隔（秒）（落 `extra` 列）；0 = 用默认。SSH 与 FTP 保活同源。
    #[serde(default)]
    pub keepalive_secs: u64,
    /// 建连超时（秒）（落 `extra` 列）；0 = 用默认。
    #[serde(default)]
    pub connect_timeout_secs: u64,
    /// 压缩优先开关（落 `extra` 列）；仅 SSH 生效。
    #[serde(default)]
    pub compression: bool,
    /// 连上即自动录制开关及其配置（落 `extra` 列）。
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
    /// 录制加密落盘（数据密钥归 Vault）；缺键按关（明文）。
    #[serde(default)]
    pub record_encrypt: bool,
    /// 初始终端行列（落 `extra` 列）；0 = 用实时尺寸。
    #[serde(default)]
    pub term_cols: u16,
    #[serde(default)]
    pub term_rows: u16,
    /// 响铃样式覆盖（落 `extra` 列）；None = 跟随全局。
    #[serde(default)]
    pub bell_style: Option<String>,
    pub color_tag: Option<String>,
    pub sort_order: Option<i64>,
    /// 明文凭证：仅入参，密码认证用。
    pub password: Option<Secret>,
    /// 明文凭证：仅入参，用户粘贴的私钥内容。
    pub private_key_pem: Option<Secret>,
    /// 明文凭证：仅入参，私钥口令。
    pub key_passphrase: Option<Secret>,
    /// `true` = 清除该档案已保存的全部凭证。
    #[serde(default)]
    pub clear_credential: bool,
}

/// 列出全部连接（含分组归属与凭证标记）。
/// 解析本机 `~/.ssh/config`，返回可导入的连接草稿（无凭证）。
#[tauri::command]
pub async fn ssh_config_parse() -> Result<Vec<crate::ssh_config::SshConfigEntry>, AppError> {
    crate::ssh_config::load_default()
}

#[tauri::command]
pub async fn profile_list(app: AppHandle) -> Result<Vec<Profile>, AppError> {
    blocking(move || {
        let st = state(&app);
        let list = st.db.list_profiles()?;
        list.into_iter().map(|p| flags(&st, p)).collect()
    })
    .await
}

#[tauri::command]
pub async fn profile_get(app: AppHandle, id: String) -> Result<Profile, AppError> {
    blocking(move || {
        let st = state(&app);
        let p = st
            .db
            .get_profile(&id)?
            .ok_or_else(|| AppError::ProfileNotFound(id.clone()))?;
        flags(&st, p)
    })
    .await
}

/// 新建或编辑连接；返回落库后的档案（凭证字段已转为布尔标记）。
/// 阻塞段（SQLite + OS keyring）套 15s 兜底超时：keyring（Windows 凭据管理器 / lsass）偶发停顿
/// 时，命令不再永不 settle——否则前端 `busy` 永久挂住、重命名/保存弹窗卡死（历史反复踩过的坑）。
#[tauri::command]
pub async fn profile_save(app: AppHandle, input: ProfileInput) -> Result<Profile, AppError> {
    let fut = blocking(move || {
        let st = state(&app);
        save(&st, input)
    });
    tokio::time::timeout(std::time::Duration::from_secs(15), fut)
        .await
        .map_err(|_| {
            AppError::Internal("保存连接超时：本地存储或凭据保险库无响应，请重试".into())
        })?
}

/// 删除连接，并清理保险库里属于它的全部凭证条目。
#[tauri::command]
pub async fn profile_delete(app: AppHandle, id: String) -> Result<(), AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.delete_profile(&id)?;
        st.vault.delete(&keys::of_profile(&id))
    })
    .await
}

/// 复制连接：连同保险库里的凭证一并复制到新档案，复制后可直接连接、无需重填密码。
/// 凭证只在可信区内从源 key 搬到新 id 的对应 key，明文绝不经 IPC。
#[tauri::command]
pub async fn profile_duplicate(app: AppHandle, id: String) -> Result<Profile, AppError> {
    blocking(move || {
        let st = state(&app);
        let src = st
            .db
            .get_profile(&id)?
            .ok_or_else(|| AppError::ProfileNotFound(id.clone()))?;
        let mut p = Profile::new(
            format!("{} copy", src.name),
            src.host.clone(),
            src.port,
            src.username.clone(),
            src.auth_type,
        );
        p.kind = src.kind;
        p.group_id = src.group_id.clone();
        p.private_key_path = src.private_key_path.clone();
        p.jump_profile_id = src.jump_profile_id.clone();
        p.serial = src.serial.clone();
        p.startup_dir = src.startup_dir.clone();
        p.startup_command = src.startup_command.clone();
        p.notes = src.notes.clone();
        p.agent_forward = src.agent_forward;
        p.x11_forward = src.x11_forward;
        p.ftp_active_mode = src.ftp_active_mode;
        p.keepalive_secs = src.keepalive_secs;
        p.connect_timeout_secs = src.connect_timeout_secs;
        p.compression = src.compression;
        p.auto_record = src.auto_record;
        p.record_dir = src.record_dir.clone();
        p.record_file_pattern = src.record_file_pattern.clone();
        p.record_overwrite = src.record_overwrite;
        p.record_timestamp_prefix = src.record_timestamp_prefix;
        p.record_encrypt = src.record_encrypt;
        p.term_cols = src.term_cols;
        p.term_rows = src.term_rows;
        p.bell_style = src.bell_style.clone();
        p.color_tag = src.color_tag.clone();
        p.sort_order = src.sort_order;
        // 引用密钥库私钥的复制：新档案共用同一把库内私钥（复用即本意）。
        p.private_key_id = src.private_key_id.clone();
        // 凭证槽按认证方式定位：密码 → password key；私钥 → key_passphrase key；kbd 无凭证
        let cred_key = match src.auth_type {
            AuthType::Password => keys::password(&p.id),
            AuthType::PrivateKey => keys::key_passphrase(&p.id),
            AuthType::KeyboardInteractive => keys::password(&p.id),
            AuthType::Auto => keys::password(&p.id),
        };
        p.credential_ref = copy_secret(&st, src.credential_ref.as_deref(), &cred_key)?;
        p.private_key_ref = copy_secret(
            &st,
            src.private_key_ref.as_deref(),
            &keys::private_key(&p.id),
        )?;
        st.db.upsert_profile(&p)?;
        flags(&st, p)
    })
    .await
}

/// 把 `src_key` 指向的凭证原样复制到 `dst_key`；源不存在则返回 `None`（允许「无凭证复制」）。
fn copy_secret(
    st: &AppState,
    src_key: Option<&str>,
    dst_key: &str,
) -> Result<Option<String>, AppError> {
    match src_key {
        Some(k) => match st.vault.get(k)? {
            Some(sec) if !sec.is_empty() => {
                st.vault.store(dst_key, &sec)?;
                Ok(Some(dst_key.to_string()))
            }
            _ => Ok(None),
        },
        None => Ok(None),
    }
}

#[tauri::command]
pub async fn group_list(app: AppHandle) -> Result<Vec<Group>, AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.list_groups()
    })
    .await
}

#[tauri::command]
pub async fn group_create(
    app: AppHandle,
    name: String,
    parent_id: Option<String>,
) -> Result<Group, AppError> {
    if name.trim().is_empty() {
        return Err(AppError::Invalid("分组名称不能为空".into()));
    }
    blocking(move || {
        let st = state(&app);
        st.db.create_group(name.trim(), parent_id.as_deref())
    })
    .await
}

/// 删除分组；组内连接保留，仅置为未分组。
#[tauri::command]
pub async fn group_delete(app: AppHandle, id: String) -> Result<(), AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.delete_group(&id)
    })
    .await
}

/// 重命名分组（侧栏分组行右键→重命名）。
#[tauri::command]
pub async fn group_rename(app: AppHandle, id: String, name: String) -> Result<(), AppError> {
    if name.trim().is_empty() {
        return Err(AppError::Invalid("分组名称不能为空".into()));
    }
    blocking(move || {
        let st = state(&app);
        st.db.rename_group(&id, name.trim())
    })
    .await
}

/// 拖拽重排连接：前端传入期望顺序的 id 列表，后端按写下标到 `sort_order`。
#[tauri::command]
pub async fn profile_reorder(app: AppHandle, ids: Vec<String>) -> Result<(), AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.reorder_profiles(&ids)
    })
    .await
}

/// 拖拽重排分组。
#[tauri::command]
pub async fn group_reorder(app: AppHandle, ids: Vec<String>) -> Result<(), AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.reorder_groups(&ids)
    })
    .await
}

fn save(st: &AppState, mut input: ProfileInput) -> Result<Profile, AppError> {
    let port = input.port.unwrap_or_else(|| input.kind.default_port());
    validate(&input.name, &input.host, port, &input.username, input.kind)?;
    // FTP/FTPS 只有 USER/PASS，私钥是 SSH 的概念；在写保险库之前就拦住，
    // 否则会存进去一份永远用不上的私钥
    if input.kind.is_ftp() && input.auth_type == AuthType::PrivateKey {
        return Err(AppError::Invalid("FTP/FTPS 只支持密码认证".into()));
    }

    let mut p = match input.id.as_deref() {
        Some(id) => st
            .db
            .get_profile(id)?
            .ok_or_else(|| AppError::ProfileNotFound(id.to_string()))?,
        None => Profile::new(
            input.name.clone(),
            input.host.clone(),
            port,
            input.username.clone(),
            input.auth_type,
        ),
    };

    p.name = input.name.trim().to_string();
    p.group_id = input.group_id.take();
    p.private_key_id = input
        .private_key_id
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    p.kind = input.kind;
    p.host = input.host.trim().to_string();
    p.port = port;
    p.username = input.username.trim().to_string();
    p.auth_type = input.auth_type;
    p.private_key_path = input.private_key_path.take();
    p.jump_profile_id = input
        .jump_profile_id
        .take()
        .filter(|s| !s.trim().is_empty());
    p.serial = if input.kind == SessionKind::Serial {
        input.serial.take().map(|s| s.normalized())
    } else {
        None
    };
    p.startup_dir = input.startup_dir.take();
    p.startup_command = input
        .startup_command
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    p.notes = input
        .notes
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    p.agent_forward = input.agent_forward;
    p.x11_forward = input.x11_forward;
    p.ftp_active_mode = input.ftp_active_mode;
    p.keepalive_secs = input.keepalive_secs;
    p.connect_timeout_secs = input.connect_timeout_secs;
    p.compression = input.compression;
    p.auto_record = input.auto_record;
    p.record_dir = input
        .record_dir
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    p.record_file_pattern = input
        .record_file_pattern
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    p.record_overwrite = input.record_overwrite;
    p.record_timestamp_prefix = input.record_timestamp_prefix;
    p.record_encrypt = input.record_encrypt;
    p.term_cols = input.term_cols;
    p.term_rows = input.term_rows;
    p.bell_style = input
        .bell_style
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    p.color_tag = input.color_tag.take();
    p.sort_order = input.sort_order.unwrap_or(p.sort_order);
    p.updated_at = now_ms();

    apply_credentials(st, &mut p, &mut input)?;
    st.db.upsert_profile(&p)?;
    flags(st, p)
}

fn validate(
    name: &str,
    host: &str,
    port: u16,
    username: &str,
    kind: SessionKind,
) -> Result<(), AppError> {
    if name.trim().is_empty() {
        return Err(AppError::Invalid("连接名称不能为空".into()));
    }
    if host.trim().is_empty() {
        return Err(AppError::Invalid(
            if kind == SessionKind::Serial {
                "串口设备名不能为空"
            } else {
                "主机地址不能为空"
            }
            .into(),
        ));
    }
    // 串口无 TCP 端口、无账号；Telnet / FTP / FTPS 允许空用户名（FTP 留空即匿名）
    if kind == SessionKind::Serial {
        return Ok(());
    }
    if port == 0 {
        return Err(AppError::Invalid("端口不能为 0".into()));
    }
    if username.trim().is_empty() && kind != SessionKind::Telnet && !kind.is_ftp() {
        return Err(AppError::Invalid("用户名不能为空".into()));
    }
    Ok(())
}

/// 凭证只在「显式提供」或「显式清除」时改动保险库；编辑连接不填密码即保持原样。
fn apply_credentials(
    st: &AppState,
    p: &mut Profile,
    input: &mut ProfileInput,
) -> Result<(), AppError> {
    if input.clear_credential {
        st.vault.delete(&keys::of_profile(&p.id))?;
        p.credential_ref = None;
        p.private_key_ref = None;
        return Ok(());
    }

    // 密码认证存密码，私钥认证存私钥口令，二者共用 credential_ref 槽位；kbd 认证不落任何凭证
    let slot = match p.auth_type {
        AuthType::Password => (input.password.take(), keys::password(&p.id)),
        AuthType::PrivateKey => (input.key_passphrase.take(), keys::key_passphrase(&p.id)),
        AuthType::KeyboardInteractive => (None, keys::password(&p.id)),
        AuthType::Auto => (input.password.take(), keys::password(&p.id)),
    };
    if let (Some(secret), key) = slot {
        if !secret.is_empty() {
            st.vault.store(&key, &secret)?;
            p.credential_ref = Some(key);
        }
    }

    // 粘贴的私钥内容优先于磁盘路径（路径本身非敏感，留在 extra 列）
    if let Some(pem) = input.private_key_pem.take() {
        if !pem.is_empty() {
            let key = keys::private_key(&p.id);
            st.vault.store(&key, &pem)?;
            p.private_key_ref = Some(key);
        }
    }
    Ok(())
}

/// 依保险库回填布尔标记（**不回传任何凭证内容**）。
fn flags(st: &AppState, mut p: Profile) -> Result<Profile, AppError> {
    p.has_credential = stored(st, p.credential_ref.as_deref())?;
    p.has_private_key = stored(st, p.private_key_ref.as_deref())?;
    Ok(p)
}

fn stored(st: &AppState, key: Option<&str>) -> Result<bool, AppError> {
    match key {
        Some(k) => st.vault.contains(k),
        None => Ok(false),
    }
}

// ─────────────────────── 连接导入 / 导出───────────────────────
//
// 备份是**去凭证**的连接配置 JSON：只含 host/port/username/kind/分组/外观等非敏感字段，
// 密码、私钥内容、私钥口令一律不进文件（它们只活在保险库）。
// 导入到新机器后凭证为空，用户按需重填——这是刻意的安全设计，不是缺陷。

/// 备份信封的 `app` 标识与当前格式版本（版本高于此值的文件拒绝导入）。
pub const EXPORT_APP: &str = "sailterm";
pub const EXPORT_VERSION: i64 = 2;
/// 单次导入条目上限，防御损坏 / 恶意超大文件。
const MAX_IMPORT_ITEMS: usize = 5000;

/// 备份信封。字段名 camelCase，与前端类型对齐。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExportBundle {
    pub app: String,
    pub version: i64,
    #[serde(default)]
    pub exported_at: i64,
    #[serde(default)]
    pub groups: Vec<ExportedGroup>,
    #[serde(default)]
    pub profiles: Vec<ExportedProfile>,
    /// AI 模型配置（v2 起；全局配置不属分组，仅整库导出携带）。旧备份无此字段按空表读。
    #[serde(default)]
    pub providers: Vec<ExportedProvider>,
    /// 「完整备份（含密钥）」时才有：主密码加密的凭证包（[`CredentialBundle`]  的密文）。
    /// 默认导出为 `None`——明文备份里绝不含任何凭证。旧版本读到时按未知字段忽略（降级为无凭证导入）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<sealed::Sealed>,
}

/// 导出的分组：`export_id` 是原库分组 id，仅供导入期把连接的分组引用重映射到新 id。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExportedGroup {
    pub export_id: String,
    pub name: String,
    #[serde(default)]
    pub sort_order: i64,
}

/// 导出的连接。**刻意不含** `credential_ref` / `private_key_ref` / 任何明文凭证。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExportedProfile {
    pub name: String,
    /// 原库档案 id（非敏感的不透明 uuid）；仅供「完整备份」导入期把加密凭证重映射到新建档案的新 id。
    /// 与 [`ExportedGroup::export_id`] 同理；普通（无凭证）导出带着它也无害。
    #[serde(default)]
    pub export_id: String,
    #[serde(default)]
    pub group_export_id: Option<String>,
    /// 引用的密钥库私钥（= 某条 [`ManagedKeyCreds::export_id`]，即原 `ssh_key.id`）；
    /// None = 不引用（内联路径/粘贴或无密钥）。含密钥完整备份导入时据此重接新密钥 id。
    #[serde(default)]
    pub private_key_export_id: Option<String>,
    #[serde(default)]
    pub kind: SessionKind,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub auth_type: AuthType,
    #[serde(default)]
    pub private_key_path: Option<String>,
    #[serde(default)]
    pub startup_dir: Option<String>,
    /// Agent 转发开关：改变连到别的机器后的授权面，备份往返须保留。
    #[serde(default)]
    pub agent_forward: bool,
    /// X11 转发开关：备份往返须保留。
    #[serde(default)]
    pub x11_forward: bool,
    /// FTP 主动模式（PORT）开关：仅 FTP/FTPS 生效，备份往返须保留。
    #[serde(default)]
    pub ftp_active_mode: bool,
    /// 心跳间隔（秒）；0 = 用默认。
    #[serde(default)]
    pub keepalive_secs: u64,
    /// 建连超时（秒）；0 = 用默认。备份往返须保留。
    #[serde(default)]
    pub connect_timeout_secs: u64,
    /// 压缩优先开关。备份往返须保留。
    #[serde(default)]
    pub compression: bool,
    /// 自动录制及其配置。备份往返须保留（均为非敏感参数）。
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
    /// 录制加密落盘；缺键按关（明文）。
    #[serde(default)]
    pub record_encrypt: bool,
    /// 初始终端行列；0 = 用实时尺寸。
    #[serde(default)]
    pub term_cols: u16,
    #[serde(default)]
    pub term_rows: u16,
    /// 响铃样式覆盖；None = 跟随全局。
    #[serde(default)]
    pub bell_style: Option<String>,
    #[serde(default)]
    pub color_tag: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
}

/// 导出的 AI 模型配置。**刻意不含** `key_ref` / 任何 Key 内容（Key 走 sealed 凭证包）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExportedProvider {
    /// 原库 provider id（非敏感 uuid）；供加密包里的 AI Key 在导入期重映射到新建模型。
    #[serde(default)]
    pub export_id: String,
    pub name: String,
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    pub sanitize_level: SanitizeLevel,
    pub max_context_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
}

/// 导出结果摘要（回前端）。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    pub path: String,
    pub groups: usize,
    pub profiles: usize,
    /// 随备份带出的 AI 模型配置条数（分组导出恒为 0）。
    pub providers: usize,
    /// 本次封装进加密包的凭证条目数（连接数 + 密钥库条数 + AI Key 数）；无凭证导出恒为 0。
    pub secrets: usize,
}

/// 导入结果摘要（回前端）。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub groups_created: usize,
    pub profiles_created: usize,
    pub profiles_skipped: usize,
    pub providers_created: usize,
    pub providers_skipped: usize,
    /// 解密并回写保险库的凭证条目数（连接凭证 + 密钥库密钥 + AI Key 合计）。
    pub credentials_applied: usize,
    pub errors: Vec<String>,
}

/// 备份信封元信息预检（导入前判断是否需要主密码；**不含任何凭证内容**）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub version: i64,
    pub groups: usize,
    pub profiles: usize,
    pub providers: usize,
    /// 备份是否含加密凭证包（为真则导入需主密码）。
    pub has_credentials: bool,
}

/// 加密凭证包的**明文负载**（序列化后由 [`sealed`] 加密，本身绝不落盘/不过 IPC）。
/// 连接凭证按 `profile_id`（= [`ExportedProfile::export_id`]）关联；文件路径型私钥
/// 在导出时已被读取成 PEM 内容，导入即转为保险库内部私钥（见 [`restore_credentials`]）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CredentialBundle {
    #[serde(default)]
    pub profiles: Vec<ProfileCreds>,
    #[serde(default)]
    pub ssh_keys: Vec<ManagedKeyCreds>,
    /// AI 模型的 API Key（v2 起）；旧备份无此字段按空表读。
    #[serde(default)]
    pub ai_keys: Vec<AiKeyCreds>,
}

impl CredentialBundle {
    fn count(&self) -> usize {
        self.profiles.len() + self.ssh_keys.len() + self.ai_keys.len()
    }
}

/// 单个 AI 模型的 API Key 材料：按 `export_id`（= 原 provider id）关联 [`ExportedProvider`]。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiKeyCreds {
    pub export_id: String,
    pub api_key: String,
}

/// 单条连接的凭证（字段可空；导出时按保险库规范 key 直读，导入时依 auth_type 落槽）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCreds {
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_key_pem: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_passphrase: Option<String>,
}

/// 密钥管理器（设置 → SSH 密钥）里一把独立密钥的凭证材料。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ManagedKeyCreds {
    pub name: String,
    pub private_key_pem: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
    /// 原库 `ssh_key.id`（非敏感 uuid）；供连接的 `private_key_export_id` 在导入期重映射到新密钥 id。
    #[serde(default)]
    pub export_id: String,
}

/// 导出到磁盘：`group_id` 为空导出全部，否则只导出该分组内的连接与该分组。
/// `include_credentials=true` 时额外生成「完整备份」：用 `master_password` 加密全部凭证（密码/私钥/口令
/// + 密钥管理器独立密钥 + AI 模型 API Key）封进 `sealed` 字段。默认（false）行为不变——明文 JSON、不含任何凭证。
#[tauri::command]
pub async fn profile_export_file(
    app: AppHandle,
    path: String,
    group_id: Option<String>,
    master_password: Option<Secret>,
    include_credentials: bool,
) -> Result<ExportReport, AppError> {
    blocking(move || {
        let st = state(&app);
        let path = path.trim();
        if path.is_empty() {
            return Err(AppError::Invalid("导出路径不能为空".into()));
        }
        let all_profiles = st.db.list_profiles()?;
        let all_groups = st.db.list_groups()?;
        let (profiles, groups) = match group_id.as_deref().map(str::trim).filter(|s| !s.is_empty())
        {
            Some(gid) => (
                all_profiles
                    .into_iter()
                    .filter(|p| p.group_id.as_deref() == Some(gid))
                    .collect(),
                all_groups.into_iter().filter(|g| g.id == gid).collect(),
            ),
            None => (all_profiles, all_groups),
        };
        // AI 模型是全局配置（不属任何分组）：仅整库导出携带；分组导出保持原语义。
        let providers = if group_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        {
            st.db.list_providers()?
        } else {
            Vec::new()
        };
        let mut bundle = build_bundle(&profiles, &groups, &providers);
        let mut secrets = 0usize;
        if include_credentials {
            let master = master_password
                .as_ref()
                .filter(|m| !m.is_empty())
                .ok_or_else(|| AppError::Invalid("请设置一个非空主密码".into()))?;
            let creds = collect_credentials(&st, &profiles, &providers)?;
            secrets = creds.count();
            let plain = Zeroizing::new(
                serde_json::to_vec(&creds)
                    .map_err(|e| AppError::Internal(format!("序列化凭据包失败：{e}")))?,
            );
            bundle.sealed = Some(sealed::seal(master.expose(), plain.as_slice())?);
        }
        let report = ExportReport {
            path: path.to_string(),
            groups: bundle.groups.len(),
            profiles: bundle.profiles.len(),
            providers: bundle.providers.len(),
            secrets,
        };
        let json = serde_json::to_string_pretty(&bundle)
            .map_err(|e| AppError::Internal(format!("序列化备份失败：{e}")))?;
        std::fs::write(path, json).map_err(|e| AppError::Io(format!("写入 {} 失败：{e}", path)))?;
        Ok(report)
    })
    .await
}

/// 从磁盘导入：`skip_duplicates` 为真（默认）时跳过与现有连接完全同键的条目。
/// 备份若含加密凭证包（`sealed`）则必须传 `master_password`，解密后把凭证回写本机保险库。
#[tauri::command]
pub async fn profile_import_file(
    app: AppHandle,
    path: String,
    skip_duplicates: Option<bool>,
    master_password: Option<Secret>,
) -> Result<ImportReport, AppError> {
    blocking(move || {
        let st = state(&app);
        let path = path.trim();
        if path.is_empty() {
            return Err(AppError::Invalid("导入路径不能为空".into()));
        }
        let json = std::fs::read_to_string(path)
            .map_err(|e| AppError::Io(format!("读取 {} 失败：{e}", path)))?;
        let bundle = parse_bundle(&json)?;
        // sealed 只是密文（非敏感），先取副本，再把整个 bundle 交给 apply_bundle 消费。
        let sealed_blob = bundle.sealed.clone();
        let (mut report, id_map, provider_map) =
            apply_bundle(&st.db, bundle, skip_duplicates.unwrap_or(true))?;
        if let Some(sealed_blob) = sealed_blob {
            let master = master_password
                .as_ref()
                .filter(|m| !m.is_empty())
                .ok_or_else(|| {
                    AppError::Invalid("该备份含加密凭据，请输入主密码后再导入".into())
                })?;
            let plain = sealed::open(&sealed_blob, master.expose())?;
            let creds: CredentialBundle = serde_json::from_slice(&plain)
                .map_err(|e| AppError::Invalid(format!("凭据包解析失败：{e}")))?;
            report.credentials_applied = restore_credentials(&st, &id_map, &provider_map, &creds)?;
        }
        Ok(report)
    })
    .await
}

/// 预检备份文件：只读 + 解析元信息，回前端「是否含加密凭证/条数」，供导入前判定要不要问主密码。
#[tauri::command]
pub async fn profile_backup_inspect(path: String) -> Result<BackupInfo, AppError> {
    blocking(move || {
        let path = path.trim();
        if path.is_empty() {
            return Err(AppError::Invalid("导入路径不能为空".into()));
        }
        let json = std::fs::read_to_string(path)
            .map_err(|e| AppError::Io(format!("读取 {} 失败：{e}", path)))?;
        let bundle = parse_bundle(&json)?;
        Ok(BackupInfo {
            version: bundle.version,
            groups: bundle.groups.len(),
            profiles: bundle.profiles.len(),
            providers: bundle.providers.len(),
            has_credentials: bundle.sealed.is_some(),
        })
    })
    .await
}

/// 枚举待加密的凭证（只在可信区内读保险库/磁盘私钥文件，明文绝不返回前端）。
/// 连接侧按规范 key 直读三条槽（密码 / 私钥 PEM / 私钥口令）；私钥无保险库内容但配了磁盘
/// 路径时，读文件内容转成 PEM（即「文件路径密钥转内部密钥」的导出侧）。密钥管理器逐条一并取；
/// AI 模型按 `key_ref` 取 API Key（v2 起，随「完整备份」走）。
fn collect_credentials(
    st: &AppState,
    profiles: &[Profile],
    providers: &[AiProvider],
) -> Result<CredentialBundle, AppError> {
    let read = |key: String| -> Result<Option<String>, AppError> {
        Ok(st
            .vault
            .get(&key)?
            .map(|s| s.expose().to_string())
            .filter(|s| !s.is_empty()))
    };
    let mut prof_creds = Vec::new();
    for p in profiles {
        let password = read(keys::password(&p.id))?;
        let key_passphrase = read(keys::key_passphrase(&p.id))?;
        let mut private_key_pem = read(keys::private_key(&p.id))?;
        if private_key_pem.is_none() {
            // 文件路径型私钥：读磁盘内容（读不到就跳过——仍会随普通导出带路径，不阻断整体）。
            if let Some(path) = p
                .private_key_path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                private_key_pem = std::fs::read_to_string(path).ok().filter(|s| !s.is_empty());
            }
        }
        if password.is_some() || key_passphrase.is_some() || private_key_pem.is_some() {
            prof_creds.push(ProfileCreds {
                profile_id: p.id.clone(),
                password,
                private_key_pem,
                key_passphrase,
            });
        }
    }
    let mut ssh_keys = Vec::new();
    for k in st.db.list_ssh_keys()? {
        if let Some(pem) = read(k.key_ref.clone())? {
            let passphrase = read(keys::ssh_key_passphrase(&k.id))?;
            ssh_keys.push(ManagedKeyCreds {
                name: k.name.clone(),
                private_key_pem: pem,
                passphrase,
                export_id: k.id.clone(),
            });
        }
    }
    let mut ai_keys = Vec::new();
    for p in providers {
        if let Some(kr) = &p.key_ref {
            if let Some(api_key) = read(kr.clone())? {
                ai_keys.push(AiKeyCreds {
                    export_id: p.id.clone(),
                    api_key,
                });
            }
        }
    }
    Ok(CredentialBundle {
        profiles: prof_creds,
        ssh_keys,
        ai_keys,
    })
}

/// 把解密出的凭证回写新机保险库并接回各档案。profile_id 经 `id_map`（export_id → 新建 id）重映射，
/// 与分组的 export_id 重映射同构。落槽严格对齐 [`apply_credentials`]：密码/口令按 auth_type 择一入
/// `credential_ref`，PEM 入 `private_key_ref`（握手时保险库引用优先于磁盘路径，故文件密钥即转内部）。
fn restore_credentials(
    st: &AppState,
    id_map: &HashMap<String, String>,
    provider_map: &HashMap<String, String>,
    creds: &CredentialBundle,
) -> Result<usize, AppError> {
    let mut applied = 0usize;
    for pc in &creds.profiles {
        // 连接被去重跳过时 id_map 无此项：现有档案应已自带凭证，不覆盖。
        let Some(new_id) = id_map.get(&pc.profile_id) else {
            continue;
        };
        let Some(mut p) = st.db.get_profile(new_id)? else {
            continue;
        };
        let mut touched = false;
        match p.auth_type {
            AuthType::PrivateKey => {
                if let Some(pp) = &pc.key_passphrase {
                    let key = keys::key_passphrase(&p.id);
                    st.vault.store(&key, &Secret::new(pp.clone()))?;
                    p.credential_ref = Some(key);
                    touched = true;
                }
            }
            AuthType::Password | AuthType::Auto | AuthType::KeyboardInteractive => {
                if let Some(pw) = &pc.password {
                    let key = keys::password(&p.id);
                    st.vault.store(&key, &Secret::new(pw.clone()))?;
                    p.credential_ref = Some(key);
                    touched = true;
                }
            }
        }
        if let Some(pem) = &pc.private_key_pem {
            let key = keys::private_key(&p.id);
            st.vault.store(&key, &Secret::new(pem.clone()))?;
            p.private_key_ref = Some(key);
            touched = true;
        }
        if touched {
            st.db.upsert_profile(&p)?;
            applied += 1;
        }
    }
    let mut key_map: HashMap<String, String> = HashMap::new();
    for mk in &creds.ssh_keys {
        // 复用密钥管理器导入路径（解析元数据 + 指纹查重 + 入保险库），不另起炉灶。
        let input = super::ssh_key::KeyInput {
            name: mk.name.clone(),
            private_key_pem: Some(Secret::new(mk.private_key_pem.clone())),
            private_key_path: None,
            passphrase: mk.passphrase.clone().map(Secret::new),
        };
        match super::ssh_key::import_key(st, input) {
            Ok(k) => {
                // 记下「原 ssh_key.id → 新机导入后的新密钥 id」，供下方把连接的私钥引用重接。
                if !mk.export_id.is_empty() {
                    key_map.insert(mk.export_id.clone(), k.id);
                }
                applied += 1;
            }
            // 已存在同指纹 / 解析失败：跳过该条，不影响其余导入。
            Err(AppError::Invalid(_)) | Err(AppError::Key(_)) => {}
            Err(e) => return Err(e),
        }
    }
    // 「完整备份」含密钥：apply_bundle 把连接引用的旧 ssh_key.id 原样带过（新机多半没有），
    // 这里经 key_map 就地重接到导入后的新密钥 id；接不上的留原值，握手时自然回落内联路径/引用。
    if !key_map.is_empty() {
        for new_pid in id_map.values() {
            if let Some(mut p) = st.db.get_profile(new_pid)? {
                if let Some(old) = p.private_key_id.as_deref() {
                    if let Some(new_kid) = key_map.get(old) {
                        p.private_key_id = Some(new_kid.clone());
                        st.db.upsert_profile(&p)?;
                    }
                }
            }
        }
    }
    applied += restore_ai_keys(&st.db, &st.vault, provider_map, creds)?;
    Ok(applied)
}

/// 把解密出的 AI Key 回写保险库并接回新建模型（纯函数，便于用文件后端保险库单测）。
/// `export_id` 经 `provider_map` 重映射；被去重跳过的模型无映射——现有配置保留自己的 Key，不覆盖。
fn restore_ai_keys(
    db: &Db,
    vault: &Vault,
    provider_map: &HashMap<String, String>,
    creds: &CredentialBundle,
) -> Result<usize, AppError> {
    let mut applied = 0usize;
    for ak in &creds.ai_keys {
        let Some(new_id) = provider_map.get(&ak.export_id) else {
            continue;
        };
        let Some(mut p) = db.get_provider(new_id)? else {
            continue;
        };
        let key = keys::ai_provider(&p.id);
        vault.store(&key, &Secret::new(ak.api_key.clone()))?;
        p.key_ref = Some(key);
        db.upsert_provider(&p)?;
        applied += 1;
    }
    Ok(applied)
}

/// 由现有档案/分组/模型构造去凭证备份信封（纯函数，便于单测）。
fn build_bundle(profiles: &[Profile], groups: &[Group], providers: &[AiProvider]) -> ExportBundle {
    ExportBundle {
        app: EXPORT_APP.into(),
        version: EXPORT_VERSION,
        exported_at: now_ms(),
        sealed: None,
        groups: groups
            .iter()
            .map(|g| ExportedGroup {
                export_id: g.id.clone(),
                name: g.name.clone(),
                sort_order: g.sort_order,
            })
            .collect(),
        profiles: profiles
            .iter()
            .map(|p| ExportedProfile {
                name: p.name.clone(),
                export_id: p.id.clone(),
                group_export_id: p.group_id.clone(),
                private_key_export_id: p.private_key_id.clone(),
                kind: p.kind,
                host: p.host.clone(),
                port: p.port,
                username: p.username.clone(),
                auth_type: p.auth_type,
                private_key_path: p.private_key_path.clone(),
                startup_dir: p.startup_dir.clone(),
                agent_forward: p.agent_forward,
                x11_forward: p.x11_forward,
                ftp_active_mode: p.ftp_active_mode,
                keepalive_secs: p.keepalive_secs,
                connect_timeout_secs: p.connect_timeout_secs,
                compression: p.compression,
                auto_record: p.auto_record,
                record_dir: p.record_dir.clone(),
                record_file_pattern: p.record_file_pattern.clone(),
                record_overwrite: p.record_overwrite,
                record_timestamp_prefix: p.record_timestamp_prefix,
                record_encrypt: p.record_encrypt,
                term_cols: p.term_cols,
                term_rows: p.term_rows,
                bell_style: p.bell_style.clone(),
                color_tag: p.color_tag.clone(),
                sort_order: p.sort_order,
            })
            .collect(),
        providers: providers
            .iter()
            .map(|p| ExportedProvider {
                export_id: p.id.clone(),
                name: p.name.clone(),
                kind: p.kind,
                base_url: p.base_url.clone(),
                model: p.model.clone(),
                supports_tools: p.supports_tools,
                supports_vision: p.supports_vision,
                sanitize_level: p.sanitize_level,
                max_context_tokens: p.max_context_tokens,
                temperature: p.temperature,
                is_default: p.is_default,
                extra: p.extra.clone(),
            })
            .collect(),
    }
}

/// 解析并校验备份 JSON：格式非法 / 版本过高 / 条目过多一律拒绝。
fn parse_bundle(json: &str) -> Result<ExportBundle, AppError> {
    let bundle: ExportBundle = serde_json::from_str(json)
        .map_err(|e| AppError::Invalid(format!("不是有效的 SailTerm 连接备份：{e}")))?;
    if bundle.version > EXPORT_VERSION {
        return Err(AppError::Invalid(format!(
            "备份版本 {} 高于当前支持的 {}，请先升级 SailTerm 再导入",
            bundle.version, EXPORT_VERSION
        )));
    }
    if bundle.profiles.len() > MAX_IMPORT_ITEMS
        || bundle.groups.len() > MAX_IMPORT_ITEMS
        || bundle.providers.len() > MAX_IMPORT_ITEMS
    {
        return Err(AppError::Invalid("备份条目过多，文件可能已损坏".into()));
    }
    Ok(bundle)
}

/// 把备份写入目标库（纯 Db 操作，便于单测）：分组按名复用、连接/模型一律新建且不带凭证。
/// 返回摘要 + `export_id → 新建档案 id` / `export_id → 新建模型 id` 两张映射，
/// 供命令层把加密凭证（含 AI Key）重接回新建条目。
fn apply_bundle(
    db: &Db,
    bundle: ExportBundle,
    skip_duplicates: bool,
) -> Result<(ImportReport, HashMap<String, String>, HashMap<String, String>), AppError> {
    let mut report = ImportReport::default();
    let mut id_map: HashMap<String, String> = HashMap::new();
    let mut provider_map: HashMap<String, String> = HashMap::new();

    // 现有分组按名字建索引，导入时同名复用、不重复建组
    let mut name_to_gid: HashMap<String, String> = db
        .list_groups()?
        .into_iter()
        .map(|g| (g.name, g.id))
        .collect();
    let mut export_to_gid: HashMap<String, String> = HashMap::new();
    for eg in &bundle.groups {
        let name = eg.name.trim();
        if name.is_empty() {
            continue;
        }
        let gid = match name_to_gid.get(name) {
            Some(gid) => gid.clone(),
            None => {
                let g = db.create_group(name, None)?;
                report.groups_created += 1;
                name_to_gid.insert(name.to_string(), g.id.clone());
                g.id
            }
        };
        export_to_gid.insert(eg.export_id.clone(), gid);
    }

    // 去重键集合：现有连接 + 本批次已导入的
    let mut seen: HashSet<String> = db
        .list_profiles()?
        .iter()
        .map(|p| dedup_key(&p.name, &p.host, p.port, &p.username, p.kind.as_str()))
        .collect();

    for ep in &bundle.profiles {
        let name = ep.name.trim();
        let host = ep.host.trim();
        let username = ep.username.trim();
        if name.is_empty() || host.is_empty() || ep.port == 0 || username.is_empty() {
            report.errors.push(format!(
                "跳过无效连接：{}",
                if name.is_empty() { "(未命名)" } else { name }
            ));
            continue;
        }
        let key = dedup_key(name, host, ep.port, username, ep.kind.as_str());
        if skip_duplicates && seen.contains(&key) {
            report.profiles_skipped += 1;
            continue;
        }
        let mut p = Profile::new(
            name.to_string(),
            host.to_string(),
            ep.port,
            username.to_string(),
            ep.auth_type,
        );
        p.kind = ep.kind;
        p.group_id = ep
            .group_export_id
            .as_ref()
            .and_then(|eid| export_to_gid.get(eid))
            .cloned();
        // 引用的密钥库私钥：export 期把 ssh_key.id 原样带过（新机多半没有），
        // 含密钥的「完整备份」由 restore_credentials 经 key_map 重接到新密钥 id。
        p.private_key_id = ep
            .private_key_export_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        p.private_key_path = ep.private_key_path.clone();
        p.startup_dir = ep.startup_dir.clone();
        p.agent_forward = ep.agent_forward;
        p.x11_forward = ep.x11_forward;
        p.ftp_active_mode = ep.ftp_active_mode;
        p.keepalive_secs = ep.keepalive_secs;
        p.connect_timeout_secs = ep.connect_timeout_secs;
        p.compression = ep.compression;
        p.auto_record = ep.auto_record;
        p.record_dir = ep.record_dir.clone();
        p.record_file_pattern = ep.record_file_pattern.clone();
        p.record_overwrite = ep.record_overwrite;
        p.record_timestamp_prefix = ep.record_timestamp_prefix;
        p.record_encrypt = ep.record_encrypt;
        p.term_cols = ep.term_cols;
        p.term_rows = ep.term_rows;
        p.bell_style = ep.bell_style.clone();
        p.color_tag = ep.color_tag.clone();
        p.sort_order = ep.sort_order;
        // 凭证一律不在此处导入：credential_ref / private_key_ref 保持 None；含密钥的
        // 「完整备份」由命令层解密后经 restore_credentials 回写并登记新 id 映射。
        db.upsert_profile(&p)?;
        if !ep.export_id.is_empty() {
            id_map.insert(ep.export_id.clone(), p.id.clone());
        }
        seen.insert(key);
        report.profiles_created += 1;
    }

    // AI 模型（v2）：同键去重（协议+端点+模型名+名称），一律新建且不带 Key；
    // 「完整备份」里的 Key 由 restore_ai_keys 经 provider_map 重映射后回写。
    let existing_providers = db.list_providers()?;
    let had_default = existing_providers.iter().any(|p| p.is_default);
    let mut prov_seen: HashSet<String> = existing_providers
        .iter()
        .map(|p| provider_dedup_key(&p.name, p.kind.as_str(), p.base_url.as_deref(), &p.model))
        .collect();
    let mut first_created: Option<String> = None;
    let mut imported_default = false;
    for ep in &bundle.providers {
        let name = ep.name.trim();
        let model = ep.model.trim();
        if name.is_empty() || model.is_empty() {
            report.errors.push(format!(
                "跳过无效模型：{}",
                if name.is_empty() { "(未命名)" } else { name }
            ));
            continue;
        }
        let key = provider_dedup_key(name, ep.kind.as_str(), ep.base_url.as_deref(), model);
        if skip_duplicates && prov_seen.contains(&key) {
            report.providers_skipped += 1;
            continue;
        }
        let mut p = AiProvider::new(name.to_string(), ep.kind, model.to_string());
        p.base_url = ep
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        p.supports_tools = ep.supports_tools;
        p.supports_vision = ep.supports_vision;
        p.sanitize_level = ep.sanitize_level;
        p.max_context_tokens = ep.max_context_tokens.max(1024);
        p.temperature = ep.temperature;
        p.is_default = ep.is_default;
        p.extra = ep.extra.clone();
        // key_ref 保持 None：Key 绝不在明文信封里，含密钥备份由命令层解密后回写。
        db.upsert_provider(&p)?;
        if !ep.export_id.is_empty() {
            provider_map.insert(ep.export_id.clone(), p.id.clone());
        }
        prov_seen.insert(key);
        report.providers_created += 1;
        if first_created.is_none() {
            first_created = Some(p.id.clone());
        }
        imported_default |= ep.is_default;
    }
    // 目标库原本没有默认位、备份也没带默认位：把首条导入的设为默认，避免「配了却无默认」。
    if !had_default && !imported_default {
        if let Some(fid) = first_created {
            db.set_default_provider(&fid)?;
        }
    }

    Ok((report, id_map, provider_map))
}

/// 模型去重键：协议 + 端点 + 模型名 + 显示名（与连接同一套 \u{1} 分隔纪律）。
fn provider_dedup_key(name: &str, kind: &str, base_url: Option<&str>, model: &str) -> String {
    format!(
        "{kind}\u{1}{}\u{1}{model}\u{1}{name}",
        base_url.unwrap_or("")
    )
}

/// 连接去重键：协议 + 主机 + 端口 + 用户名 + 名称（用不可能出现在数据里的分隔符拼接）。
fn dedup_key(name: &str, host: &str, port: u16, username: &str, kind: &str) -> String {
    format!("{kind}\u{1}{host}\u{1}{port}\u{1}{username}\u{1}{name}")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn mem() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    fn sample_profile(group_id: Option<String>) -> Profile {
        let mut p = Profile::new(
            "web-01".into(),
            "10.0.0.1".into(),
            2222,
            "root".into(),
            AuthType::PrivateKey,
        );
        p.group_id = group_id;
        p.private_key_path = Some("/home/me/.ssh/id_ed25519".into());
        // 敏感引用：绝不应出现在导出文件里
        p.credential_ref = Some("sail:conn:x:key_passphrase".into());
        p.private_key_ref = Some("sail:conn:x:private_key".into());
        p
    }

    ///导出去凭证、导入不带凭证、分组引用重映射到新库新 id。
    #[test]
    fn export_import_roundtrip_strips_credentials() {
        let db = mem();
        let g = db.create_group("生产", None).unwrap();
        db.upsert_profile(&sample_profile(Some(g.id.clone())))
            .unwrap();

        let bundle = build_bundle(&db.list_profiles().unwrap(), &db.list_groups().unwrap(), &[]);
        let json = serde_json::to_string_pretty(&bundle).unwrap();

        // 导出文件里搜不到任何凭证痕迹
        assert!(!json.contains("sail:conn"));
        assert!(!json.contains("credentialRef") && !json.contains("privateKeyRef"));
        assert!(!json.to_lowercase().contains("passphrase"));
        // 非敏感字段保留（私钥「路径」非敏感）
        assert!(json.contains("web-01") && json.contains("10.0.0.1"));
        assert!(json.contains("id_ed25519"));

        // 导入到全新库
        let db2 = mem();
        let (report, _, _) = apply_bundle(&db2, parse_bundle(&json).unwrap(), true).unwrap();
        assert_eq!(report.groups_created, 1);
        assert_eq!(report.profiles_created, 1);
        assert_eq!(report.profiles_skipped, 0);
        assert!(report.errors.is_empty());

        let imported = db2.list_profiles().unwrap();
        assert_eq!(imported.len(), 1);
        let ip = &imported[0];
        assert_eq!(ip.name, "web-01");
        assert_eq!(ip.port, 2222);
        assert_eq!(ip.auth_type, AuthType::PrivateKey);
        assert!(ip.credential_ref.is_none() && ip.private_key_ref.is_none());

        // 分组关联经 export_id 重映射到新库的新分组
        let g2 = db2.list_groups().unwrap();
        assert_eq!(g2.len(), 1);
        assert_eq!(g2[0].name, "生产");
        assert_ne!(g2[0].id, g.id);
        assert_eq!(ip.group_id.as_deref(), Some(g2[0].id.as_str()));
    }

    /// 去重：同键连接 skip=true 跳过、skip=false 允许重复；分组同名复用不重复建。
    #[test]
    fn import_dedup_and_group_reuse() {
        let db = mem();
        db.upsert_profile(&Profile::new(
            "web".into(),
            "h".into(),
            22,
            "u".into(),
            AuthType::Password,
        ))
        .unwrap();
        let bundle = ExportBundle {
            app: EXPORT_APP.into(),
            version: EXPORT_VERSION,
            exported_at: 0,
            sealed: None,
            providers: vec![],
            groups: vec![ExportedGroup {
                export_id: "g1".into(),
                name: "生产".into(),
                sort_order: 0,
            }],
            profiles: vec![ExportedProfile {
                name: "web".into(),
                export_id: "p1".into(),
                group_export_id: Some("g1".into()),
                private_key_export_id: None,
                kind: SessionKind::Ssh,
                host: "h".into(),
                port: 22,
                username: "u".into(),
                auth_type: AuthType::Password,
                private_key_path: None,
                startup_dir: None,
                agent_forward: false,
                x11_forward: false,
                ftp_active_mode: false,
                keepalive_secs: 0,
                connect_timeout_secs: 0,
                compression: false,
                auto_record: false,
                record_dir: None,
                record_file_pattern: None,
                record_overwrite: false,
                record_timestamp_prefix: false,
                record_encrypt: true,
                term_cols: 0,
                term_rows: 0,
                bell_style: None,
                color_tag: None,
                sort_order: 0,
            }],
        };

        // skip=true：重复连接被跳过，但分组仍创建
        let (r, _, _) = apply_bundle(&db, bundle.clone(), true).unwrap();
        assert_eq!(r.profiles_skipped, 1);
        assert_eq!(r.profiles_created, 0);
        assert_eq!(r.groups_created, 1);
        assert_eq!(db.list_profiles().unwrap().len(), 1);

        // 再导一次：同名分组复用不重复建，skip=false 允许重复连接
        let (r2, _, _) = apply_bundle(&db, bundle, false).unwrap();
        assert_eq!(r2.groups_created, 0);
        assert_eq!(r2.profiles_created, 1);
        assert_eq!(db.list_profiles().unwrap().len(), 2);
        assert_eq!(db.list_groups().unwrap().len(), 1);
    }

    /// 健壮性：版本过高 / 非法 JSON 拒绝；无效条目（空 host）记 error 不 panic。
    #[test]
    fn import_guards_and_invalid_rows() {
        let future = format!(
            r#"{{"app":"sailterm","version":{},"exportedAt":0,"groups":[],"profiles":[]}}"#,
            EXPORT_VERSION + 1
        );
        assert!(parse_bundle(&future).is_err());
        assert!(parse_bundle("not json").is_err());

        let db = mem();
        let bundle = ExportBundle {
            app: EXPORT_APP.into(),
            version: EXPORT_VERSION,
            exported_at: 0,
            sealed: None,
            providers: vec![],
            groups: vec![],
            profiles: vec![ExportedProfile {
                name: "x".into(),
                export_id: "px".into(),
                group_export_id: None,
                private_key_export_id: None,
                kind: SessionKind::Ssh,
                host: "".into(), // 空 host = 无效
                port: 22,
                username: "u".into(),
                auth_type: AuthType::Password,
                private_key_path: None,
                startup_dir: None,
                agent_forward: false,
                x11_forward: false,
                ftp_active_mode: false,
                keepalive_secs: 0,
                connect_timeout_secs: 0,
                compression: false,
                auto_record: false,
                record_dir: None,
                record_file_pattern: None,
                record_overwrite: false,
                record_timestamp_prefix: false,
                record_encrypt: true,
                term_cols: 0,
                term_rows: 0,
                bell_style: None,
                color_tag: None,
                sort_order: 0,
            }],
        };
        let r = apply_bundle(&db, bundle, true).unwrap();
        assert_eq!(r.0.profiles_created, 0);
        assert_eq!(r.0.errors.len(), 1);
    }

    /// AI 模型往返：导出不含 Key 痕迹、导入新建+重映射、同键去重、默认位兜底。
    #[test]
    fn providers_roundtrip_dedup_and_default() {
        let db = mem();
        let mut p = AiProvider::new(
            "deepseek".into(),
            ProviderKind::OpenaiCompatible,
            "deepseek-chat".into(),
        );
        p.base_url = Some("https://api.deepseek.com".into());
        p.is_default = true;
        p.max_context_tokens = 64000;
        // 敏感引用：绝不应出现在导出文件里
        p.key_ref = Some(keys::ai_provider(&p.id));
        db.upsert_provider(&p).unwrap();

        let bundle = build_bundle(&[], &[], &db.list_providers().unwrap());
        let json = serde_json::to_string_pretty(&bundle).unwrap();
        assert!(!json.contains("sail:ai:") && !json.contains("keyRef"));
        assert!(json.contains("deepseek-chat") && json.contains("api.deepseek.com"));

        // 导入全新库：配置原样落地、默认位保留、Key 引用留空（待 sealed 凭证包重接）
        let db2 = mem();
        let (r, _, pmap) = apply_bundle(&db2, parse_bundle(&json).unwrap(), true).unwrap();
        assert_eq!(r.providers_created, 1);
        assert_eq!(pmap.len(), 1);
        let imported = db2.list_providers().unwrap();
        assert_eq!(imported.len(), 1);
        assert!(imported[0].is_default);
        assert!(imported[0].key_ref.is_none());
        assert_eq!(imported[0].max_context_tokens, 64000);
        assert_ne!(imported[0].id, p.id);
        assert_eq!(pmap.get(&p.id), Some(&imported[0].id));

        // 再导一次：同键（协议+端点+模型+名称）去重跳过，不产生映射（Key 不覆盖现有）
        let (r2, _, pmap2) = apply_bundle(&db2, parse_bundle(&json).unwrap(), true).unwrap();
        assert_eq!(r2.providers_skipped, 1);
        assert_eq!(r2.providers_created, 0);
        assert!(pmap2.is_empty());

        // 备份没带默认位 + 目标库无模型：首条导入的自动设默认
        let q = AiProvider::new("local".into(), ProviderKind::Ollama, "qwen2.5".into());
        let db3 = mem();
        let (r3, _, _) = apply_bundle(&db3, build_bundle(&[], &[], &[q]), true).unwrap();
        assert_eq!(r3.providers_created, 1);
        assert!(db3.list_providers().unwrap()[0].is_default);
    }

    /// 无效模型行（空名）记 error 不 panic；旧 v1 信封无 providers 字段按空表读。
    #[test]
    fn providers_invalid_rows_and_legacy_bundle() {
        let v1 = r#"{"app":"sailterm","version":1,"exportedAt":0,"groups":[],"profiles":[]}"#;
        assert!(parse_bundle(v1).unwrap().providers.is_empty());

        let mut bad = build_bundle(&[], &[], &[]);
        bad.providers.push(ExportedProvider {
            export_id: "x1".into(),
            name: "".into(), // 空名 = 无效
            kind: ProviderKind::Openai,
            base_url: None,
            model: "gpt-4o".into(),
            supports_tools: None,
            supports_vision: None,
            sanitize_level: SanitizeLevel::default(),
            max_context_tokens: 8192,
            temperature: None,
            is_default: false,
            extra: None,
        });
        let db = mem();
        let (r, _, pmap) = apply_bundle(&db, bad, true).unwrap();
        assert_eq!(r.providers_created, 0);
        assert_eq!(r.errors.len(), 1);
        assert!(pmap.is_empty());
    }

    /// AI Key 回写：经 provider_map 重映射接回新模型；未映射（去重跳过）的不落保险库。
    #[test]
    fn restore_ai_keys_remaps_and_skips_unmapped() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::file_backend_for_test(dir.path());
        let db = mem();
        let p = AiProvider::new("m".into(), ProviderKind::Openai, "gpt-4o".into());
        db.upsert_provider(&p).unwrap();
        let mut pmap = HashMap::new();
        pmap.insert("old-id".to_string(), p.id.clone());
        let creds = CredentialBundle {
            ai_keys: vec![
                AiKeyCreds {
                    export_id: "old-id".into(),
                    api_key: "sk-test-123".into(),
                },
                AiKeyCreds {
                    export_id: "unmapped".into(),
                    api_key: "sk-other".into(),
                },
            ],
            ..Default::default()
        };
        assert_eq!(restore_ai_keys(&db, &vault, &pmap, &creds).unwrap(), 1);
        let got = db.get_provider(&p.id).unwrap().unwrap();
        let want = keys::ai_provider(&p.id);
        assert_eq!(got.key_ref.as_deref(), Some(want.as_str()));
        assert_eq!(vault.get(&want).unwrap().unwrap().expose(), "sk-test-123");
        assert!(!vault.contains(&keys::ai_provider("unmapped")).unwrap());
    }

    /// 旧版凭证包（无 aiKeys 字段）解析为空表，count 不含 AI Key。
    #[test]
    fn legacy_credential_bundle_without_ai_keys() {
        let creds: CredentialBundle =
            serde_json::from_str(r#"{"profiles":[],"sshKeys":[]}"#).unwrap();
        assert!(creds.ai_keys.is_empty());
        assert_eq!(creds.count(), 0);
    }
}
