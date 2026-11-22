//! IPC 命令层：只做参数校验与路由，业务在 `db` / `vault` / `session` / `ssh` / `ftp`。
//!
//! 契约纪律：**任何命令都不返回明文凭证**；凭证类字段只作为入参
//! 从前端表单直送保险库，响应体里只有 `has_credential` 之类的布尔标记。

pub mod ai;
pub mod app;
pub mod audit;
pub mod dialog;
pub mod file;
pub mod ftps_cert;
pub mod history;
pub mod lock;
pub mod memory;
pub mod monitor;
pub mod net;
pub mod plugin;
pub mod policy;
pub mod process;
pub mod profiles;
pub mod record;
pub mod service;
pub mod sessions;
pub mod settings;
pub mod snippet;
pub mod ssh;
pub mod ssh_key;
pub mod system;
pub mod transfer;
pub mod trigger;
pub mod tunnel;
pub mod zmodem;

use tauri::{AppHandle, Manager};

use crate::error::AppError;
use crate::state::AppState;

/// 阻塞型本地 IO（SQLite / 保险库 / 文件系统）统一移出 async 执行器。
pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => Err(AppError::Internal(format!("后台任务异常终止：{e}"))),
    }
}

/// 取托管状态（命令层的统一入口）。
pub fn state(app: &AppHandle) -> tauri::State<'_, AppState> {
    app.state::<AppState>()
}
