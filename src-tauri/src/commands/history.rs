//! 命令历史命令：入库前由后端脱敏，前端拿不到未脱敏文本。

use tauri::AppHandle;

use super::{blocking, state};
use crate::db::history::HistoryEntry;
use crate::error::AppError;

/// 默认返回条数（前端不传 `limit` 时）。
const DEFAULT_LIMIT: i64 = 100;

/// 取最近命令；`profile_id` 为空表示全局。
#[tauri::command]
pub async fn history_list(
    app: AppHandle,
    profile_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<HistoryEntry>, AppError> {
    blocking(move || {
        let st = state(&app);
        st.db
            .list_commands(profile_id.as_deref(), limit.unwrap_or(DEFAULT_LIMIT))
    })
    .await
}

/// 记录用户在终端敲下的一行命令（前端在回车时上报），`profile_id` 由会话反查。
#[tauri::command]
pub async fn history_record(
    app: AppHandle,
    session_id: String,
    command: String,
) -> Result<(), AppError> {
    blocking(move || {
        let st = state(&app);
        let profile_id = st.sessions.profile_id(&session_id);
        st.db
            .insert_command(Some(&session_id), profile_id.as_deref(), &command, "user")
    })
    .await
}

/// 删除单条历史（面板行悬停 × 用），返回删除条数（0 或 1）。
#[tauri::command]
pub async fn history_delete(app: AppHandle, entry_id: String) -> Result<usize, AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.delete_command(&entry_id)
    })
    .await
}

/// 清空某连接（或全部）的历史，返回删除条数。
#[tauri::command]
pub async fn history_clear(app: AppHandle, profile_id: Option<String>) -> Result<usize, AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.clear_commands(profile_id.as_deref())
    })
    .await
}
