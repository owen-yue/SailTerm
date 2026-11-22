//! 用户自定义策略规则命令：黑/白/写名单 CRUD。
//!
//! 规则只是**命令模式**（由 PolicyEngine 解释），本身不含凭证、也永不回传明文。
//! 启用中的规则由 commands/ai.rs 每轮加载注入 PolicyEngine（p4-3），与内置分级合并生效；
//! 内置高危黑名单绝对优先，用户规则无法把 `rm -rf /` 之类降级放行。

use serde::Deserialize;
use tauri::AppHandle;

use super::{blocking, state};
use crate::db::policy_rule::{PolicyRule, RuleAction, RuleKind};
use crate::error::AppError;
use crate::state::AppState;

/// 规则保存入参：`id` 为空 = 新建，`Some` = 编辑既有规则。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRuleInput {
    pub id: Option<String>,
    pub kind: RuleKind,
    pub pattern: String,
    /// 显式动作；为空则按 `kind` 取隐含动作（白→放行、写→审批、黑→拦截）。
    pub action: Option<RuleAction>,
    pub note: Option<String>,
    pub enabled: Option<bool>,
    pub sort_order: Option<i64>,
}

/// 列规则：`kind` 为空则不限类别；`enabledOnly` 为真则只取启用中的。
#[tauri::command]
pub async fn policy_rule_list(
    app: AppHandle,
    kind: Option<RuleKind>,
    enabled_only: Option<bool>,
) -> Result<Vec<PolicyRule>, AppError> {
    blocking(move || {
        state(&app)
            .db
            .list_policy_rules(kind, enabled_only.unwrap_or(false))
    })
    .await
}

/// 新建或编辑规则；返回落库后的规则。
#[tauri::command]
pub async fn policy_rule_save(
    app: AppHandle,
    input: PolicyRuleInput,
) -> Result<PolicyRule, AppError> {
    blocking(move || save_rule(&state(&app), input)).await
}

/// 删除规则，返回是否删到（存在与否）。
#[tauri::command]
pub async fn policy_rule_delete(app: AppHandle, id: String) -> Result<bool, AppError> {
    blocking(move || state(&app).db.delete_policy_rule(&id)).await
}

fn save_rule(st: &AppState, input: PolicyRuleInput) -> Result<PolicyRule, AppError> {
    let pattern = input.pattern.trim().to_string();
    if pattern.is_empty() {
        return Err(AppError::Invalid("规则模式不能为空".into()));
    }
    let mut r = match input.id.as_deref() {
        Some(id) => st
            .db
            .get_policy_rule(id)?
            .ok_or_else(|| AppError::Invalid(format!("策略规则不存在：{id}")))?,
        None => PolicyRule::new(input.kind, pattern.clone()),
    };
    r.kind = input.kind;
    r.pattern = pattern;
    r.action = input.action;
    r.note = input
        .note
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    r.enabled = input.enabled.unwrap_or(r.enabled);
    r.sort_order = input.sort_order.unwrap_or(r.sort_order);
    st.db.upsert_policy_rule(&r)?;
    Ok(r)
}
