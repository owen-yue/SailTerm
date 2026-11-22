//! 长期记忆的 CRUD IPC：记忆管理界面据此查看/手动添加/编辑/删除/清空。
//!
//! **安全红线**：`content` 只存**脱敏后**的偏好/环境事实，
//! **绝不存凭证/密钥/密码**——手动入库前先过 [`Sanitizer`]（Loose，只抹硬凭证），命中任何
//! `[REDACTED*]` 占位即**拒绝保存**（不像自动提取那样静默丢弃，这里回明确错误让用户改）。
//! 召回注入时的「按本轮层级再脱敏」在 [`crate::ai::memory::select_memories`] 完成。

use serde::Deserialize;
use tauri::AppHandle;

use super::{blocking, state};
use crate::ai::sanitize::Sanitizer;
use crate::db::ai::SanitizeLevel;
use crate::db::memory::{Memory, MemoryScope, MemorySource};
use crate::error::AppError;
use crate::state::AppState;

/// 单条手动记忆的字符上限（超长截断，避免一条记忆吃掉召回预算）。
const MAX_CONTENT_CHARS: usize = 500;

/// 记忆保存入参：`id` 为空 = 新建，`Some` = 编辑既有记忆。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryInput {
    pub id: Option<String>,
    pub scope: MemoryScope,
    /// `scope=server` 时必填（绑定连接）；`global` 时忽略。
    pub connection_id: Option<String>,
    pub content: String,
    /// 召回关键词（逗号分隔，可空）；同样过脱敏，命中凭证则丢弃关键词但保留内容。
    pub keywords: Option<String>,
}

/// 列记忆（管理界面）：`scope` 为空则不限作用域；`connection_id` 非空则只看该连接的 server 记忆。
#[tauri::command]
pub async fn memory_list(
    app: AppHandle,
    scope: Option<MemoryScope>,
    connection_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<Memory>, AppError> {
    blocking(move || {
        state(&app)
            .db
            .list_memory(scope, connection_id.as_deref(), limit.unwrap_or(500))
    })
    .await
}

/// 新建或编辑记忆；返回落库后的记忆（内容已脱敏）。
#[tauri::command]
pub async fn memory_save(app: AppHandle, input: MemoryInput) -> Result<Memory, AppError> {
    blocking(move || save_memory(&state(&app), input)).await
}

/// 删除单条记忆，返回是否删到（存在与否）。
#[tauri::command]
pub async fn memory_delete(app: AppHandle, id: String) -> Result<bool, AppError> {
    blocking(move || state(&app).db.delete_memory(&id)).await
}

/// 清空记忆：`scope` 为空清全部；否则按作用域清（`server` 再按 `connection_id` 限定）。返回删除条数。
#[tauri::command]
pub async fn memory_clear(
    app: AppHandle,
    scope: Option<MemoryScope>,
    connection_id: Option<String>,
) -> Result<usize, AppError> {
    blocking(move || state(&app).db.clear_memory(scope, connection_id.as_deref())).await
}

fn save_memory(st: &AppState, input: MemoryInput) -> Result<Memory, AppError> {
    // 内容先脱敏（Loose 只抹硬凭证，保留 IP/发行版之类环境事实）；命中凭证占位即拒绝——
    // 记忆绝不存密码/密钥/令牌。与自动提取的「静默丢弃」不同，手动路径回明确错误。
    let loose = Sanitizer::new(SanitizeLevel::Loose);
    let scrubbed = loose.scrub(&input.content);
    if scrubbed.contains("[REDACTED") {
        return Err(AppError::Invalid("记忆不得包含密码/密钥/令牌等凭据".into()));
    }
    let content = clip_chars(scrubbed.trim(), MAX_CONTENT_CHARS);
    if content.is_empty() {
        return Err(AppError::Invalid("记忆内容不能为空".into()));
    }

    // server 作用域必须绑定连接；否则报错（管理界面应先选连接），避免产生无主的 server 记忆。
    let connection_id = match input.scope {
        MemoryScope::Server => {
            let cid = input
                .connection_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AppError::Invalid("按服务器的记忆必须指定连接".into()))?;
            Some(cid.to_string())
        }
        MemoryScope::Global => None,
    };

    let id = input.id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let mut m = match id {
        Some(id) => st
            .db
            .get_memory(id)?
            .ok_or_else(|| AppError::Invalid(format!("记忆不存在：{id}")))?,
        None => Memory::new(input.scope, content.clone(), MemorySource::Manual),
    };
    m.scope = input.scope;
    m.connection_id = connection_id;
    m.content = content;
    // 经手动保存的一律标记为 manual（此命令仅由记忆管理界面调用）。
    m.source = MemorySource::Manual;
    m.keywords = input
        .keywords
        .map(|k| loose.scrub(&k))
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty() && !k.contains("[REDACTED"));
    st.db.upsert_memory(&m)?;
    Ok(m)
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
