//! 命令片段 / 快捷指令的 CRUD + 渲染 IPC：片段管理界面据此
//! 查看 / 新建 / 编辑 / 删除，插入时用变量值渲染命令模板再写入终端 PTY。
//!
//! **安全红线**：`content` 是命令模板，**绝不存明文凭证**——保存前先过
//! [`Sanitizer`]（Loose，只抹硬凭证），命中任何 `[REDACTED*]` 占位即**拒绝保存**（回明确错误，
//! 提示改用 `{{变量}}` 占位，插入时再填、值不落库）。[`snippet_render`] 是无状态纯变换，只把
//! 占位替换为前端传来的值，不触碰 db、不出网。

use std::collections::BTreeMap;

use serde::Deserialize;
use tauri::AppHandle;

use super::{blocking, state};
use crate::ai::sanitize::Sanitizer;
use crate::db::ai::SanitizeLevel;
use crate::db::snippet::{render_snippet, Snippet};
use crate::error::AppError;
use crate::state::AppState;

/// 各字段字符上限（超长截断，避免异常长的片段拖累列表渲染）。
const MAX_NAME_CHARS: usize = 80;
const MAX_GROUP_CHARS: usize = 40;
const MAX_DESC_CHARS: usize = 300;
const MAX_CONTENT_CHARS: usize = 4000;

/// 片段保存入参：`id` 为空 = 新建，`Some` = 编辑既有片段。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnippetInput {
    pub id: Option<String>,
    pub name: String,
    /// 自由分组标签（可空，空 = 未分组）。
    pub group_name: Option<String>,
    /// 命令模板（可含 `{{变量}}` 占位）。
    pub content: String,
    pub description: Option<String>,
}

/// 列片段：`group` 为空列全部，否则只列该分组（管理界面按分组筛选）。
#[tauri::command]
pub async fn snippet_list(app: AppHandle, group: Option<String>) -> Result<Vec<Snippet>, AppError> {
    blocking(move || state(&app).db.list_snippets(group.as_deref())).await
}

/// 新建或编辑片段；返回落库后的片段（含解析出的 `variables`）。命中明文凭证则拒绝。
#[tauri::command]
pub async fn snippet_save(app: AppHandle, input: SnippetInput) -> Result<Snippet, AppError> {
    blocking(move || save_snippet(&state(&app), input)).await
}

/// 删除单条片段，返回是否删到（存在与否）。
#[tauri::command]
pub async fn snippet_delete(app: AppHandle, id: String) -> Result<bool, AppError> {
    blocking(move || state(&app).db.delete_snippet(&id)).await
}

/// 用变量值渲染命令模板（插入前调用）：无状态纯变换，返回可直接写入终端的字符串。
#[tauri::command]
pub async fn snippet_render(
    content: String,
    values: BTreeMap<String, String>,
) -> Result<String, AppError> {
    blocking(move || Ok(render_snippet(&content, &values))).await
}

fn save_snippet(st: &AppState, input: SnippetInput) -> Result<Snippet, AppError> {
    // 命令模板先脱敏（Loose 只抹硬凭证）；命中凭证占位即拒绝——片段绝不存密码/密钥/令牌，
    // 引导用户改用 {{变量}} 占位（插入时再填，值不落库）。
    let loose = Sanitizer::new(SanitizeLevel::Loose);
    let scrubbed = loose.scrub(&input.content);
    if scrubbed.contains("[REDACTED") {
        return Err(AppError::Invalid(
            "命令片段不得包含明文密码/密钥/令牌，请改用 {{变量}} 占位，插入时再填".into(),
        ));
    }

    let name = clip_chars(input.name.trim(), MAX_NAME_CHARS);
    if name.is_empty() {
        return Err(AppError::Invalid("片段名称不能为空".into()));
    }
    let content = clip_chars(scrubbed.trim(), MAX_CONTENT_CHARS);
    if content.is_empty() {
        return Err(AppError::Invalid("片段内容不能为空".into()));
    }
    let group_name = clip_chars(input.group_name.unwrap_or_default().trim(), MAX_GROUP_CHARS);
    let description = clip_chars(input.description.unwrap_or_default().trim(), MAX_DESC_CHARS);

    let id = input.id.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let mut s = match id {
        Some(id) => st
            .db
            .get_snippet(id)?
            .ok_or_else(|| AppError::Invalid(format!("片段不存在：{id}")))?,
        None => Snippet::new(name.clone(), content.clone()),
    };
    s.name = name;
    s.group_name = group_name;
    s.content = content;
    s.description = description;
    s.sync_variables(); // content 变了，派生的 variables 同步重算
    s.updated_at = crate::db::now_ms();
    st.db.upsert_snippet(&s)?;
    Ok(s)
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
