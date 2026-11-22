//! 审计日志命令：只读查询 + 清空。
//!
//! 安全纪律：审计是安全事件的可追溯底座，**不提供增/改命令**——前端只能查询与清空，
//! 记录只由 orchestrator 在命令生命周期内落库（p4-2），杜绝前端篡改留痕。
//! 返回体只含脱敏命令与输出摘要（哈希），绝无原始敏感输出/凭证。

use tauri::AppHandle;

use super::{blocking, state};
use crate::db::audit::AuditEntry;
use crate::error::AppError;

/// 默认返回条数（`list_audit` 内部还会 clamp 到 `1..=2000`）。
const DEFAULT_LIMIT: i64 = 200;

/// 列审计（最新在前）。`sessionId`/`since` 为空表示不过滤该维度。
#[tauri::command]
pub async fn audit_list(
    app: AppHandle,
    session_id: Option<String>,
    since: Option<i64>,
    limit: Option<i64>,
) -> Result<Vec<AuditEntry>, AppError> {
    blocking(move || {
        state(&app)
            .db
            .list_audit(session_id.as_deref(), since, limit.unwrap_or(DEFAULT_LIMIT))
    })
    .await
}

/// 清空审计（`sessionId` 为空则清全部），返回删除条数。
#[tauri::command]
pub async fn audit_clear(app: AppHandle, session_id: Option<String>) -> Result<usize, AppError> {
    blocking(move || state(&app).db.clear_audit(session_id.as_deref())).await
}
