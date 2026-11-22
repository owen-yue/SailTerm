//! 输出触发器 CRUD IPC：管理界面据此查看 / 新建 / 编辑 / 删除规则。
//!
//! **安全红线**：`pattern` 只是正则源串，不含凭证；匹配与动作都在前端本地完成。

use serde::Deserialize;
use tauri::AppHandle;

use super::{blocking, state};
use crate::db::trigger::{normalize_color, TriggerAction, TriggerRule};
use crate::error::AppError;

const MAX_NAME_CHARS: usize = 80;
const MAX_PATTERN_CHARS: usize = 500;

/// 规则保存入参：`id` 为空 = 新建。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriggerInput {
    pub id: Option<String>,
    pub name: String,
    pub pattern: String,
    pub action: Option<TriggerAction>,
    /// 着色色键（`action=colorize` 时生效）；未知值归一化为 `red`。
    pub color: Option<String>,
    pub enabled: Option<bool>,
    pub sort_order: Option<i64>,
}

/// 列出全部触发规则。
#[tauri::command]
pub async fn trigger_list(app: AppHandle) -> Result<Vec<TriggerRule>, AppError> {
    blocking(move || state(&app).db.list_triggers()).await
}

/// 新建或编辑规则；返回落库后的规则。
#[tauri::command]
pub async fn trigger_save(app: AppHandle, input: TriggerInput) -> Result<TriggerRule, AppError> {
    blocking(move || save(&state(&app), input)).await
}

/// 删除单条，返回是否删到。
#[tauri::command]
pub async fn trigger_delete(app: AppHandle, id: String) -> Result<bool, AppError> {
    blocking(move || state(&app).db.delete_trigger(&id)).await
}

fn save(st: &crate::state::AppState, input: TriggerInput) -> Result<TriggerRule, AppError> {
    let name = input.name.trim();
    let pattern = input.pattern.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("请填写规则名".into()));
    }
    if pattern.is_empty() {
        return Err(AppError::Invalid("请填写匹配模式".into()));
    }
    if name.len() > MAX_NAME_CHARS {
        return Err(AppError::Invalid("规则名过长".into()));
    }
    if pattern.len() > MAX_PATTERN_CHARS {
        return Err(AppError::Invalid("匹配模式过长".into()));
    }
    // 前端已用 RegExp 校验过；后端再快速括号配对检查，明显残缺直接拒
    if pattern.chars().filter(|c| *c == '(').count()
        != pattern.chars().filter(|c| *c == ')').count()
    {
        return Err(AppError::Invalid("匹配模式括号不配对".into()));
    }

    let now = crate::db::now_ms();
    let mut r = match input.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => st
            .db
            .get_trigger(id)?
            .ok_or_else(|| AppError::Invalid("规则不存在".into()))?,
        None => TriggerRule::new(name, pattern),
    };
    r.name = name.to_string();
    r.pattern = pattern.to_string();
    if let Some(a) = input.action {
        r.action = a;
    }
    if let Some(c) = input.color {
        r.color = normalize_color(c.trim());
    }
    if let Some(e) = input.enabled {
        r.enabled = e;
    }
    if let Some(s) = input.sort_order {
        r.sort_order = s;
    }
    r.updated_at = now;
    st.db.upsert_trigger(&r)?;
    Ok(r)
}
