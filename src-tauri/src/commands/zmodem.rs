//! ZMODEM 上传/下载 IPC：远端 `rz`/`sz` + `zmodem2` 状态机。
//! 路径来自文件面板；写本地目录用既有 `std::fs` 模式，不引入 fs 插件。

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::error::AppError;
use crate::session::SessionCmd;
use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZmodemResult {
    pub files: Vec<String>,
}

/// `sz` 下载远端文件到本地目录。
#[tauri::command]
pub async fn zmodem_download(
    app: AppHandle,
    session_id: String,
    remote_path: String,
    local_dir: String,
) -> Result<ZmodemResult, AppError> {
    let files = crate::zmodem::download(app, session_id, remote_path, local_dir).await?;
    Ok(ZmodemResult { files })
}

/// `rz` 上传本地文件到远端目录。
#[tauri::command]
pub async fn zmodem_upload(
    app: AppHandle,
    session_id: String,
    local_path: String,
    remote_dir: String,
) -> Result<(), AppError> {
    crate::zmodem::upload(app, session_id, local_path, remote_dir).await
}

/// 检查远端是否装有 lrzsz（`sz`/`rz`）。
#[tauri::command]
pub async fn zmodem_probe(app: AppHandle, session_id: String) -> Result<bool, AppError> {
    use crate::ai::exec::exec_command;
    let r = exec_command(
        &app,
        &session_id,
        "command -v sz >/dev/null && command -v rz >/dev/null",
        5_000,
    )
    .await?;
    Ok(r.exit_code == Some(0))
}

/// 终端内 ZMODEM：用户在原生文件框里选好了本地文件/目录，通知会话 actor 开传。
/// （`zmodem-request` 事件 → 前端弹框 → 本命令，与面板右键版互不影响。）
#[tauri::command]
pub async fn zmodem_terminal_start(
    app: AppHandle,
    session_id: String,
    local_path: String,
) -> Result<(), AppError> {
    let tx = app
        .state::<AppState>()
        .sessions
        .sender(&session_id)
        .ok_or_else(|| AppError::NoSuchSession(session_id.clone()))?;
    tx.send(SessionCmd::ZmodemStart { local_path })
        .await
        .map_err(|_| AppError::NoSuchSession(session_id))
}

/// 弹系统文件框（回调式非阻塞 + `async` 命令）：上传选文件、下载选保存目录。
/// 用 `AppHandle`（非 `Window`）开框 = 不设父窗口、非模态，规避远程桌面下被禁用的 WebView
/// 光标“消失”；回调式不卡主线程消息循环，故整窗不会“无响应”。取消返回 None，前端转成 ZmodemCancel。
#[tauri::command]
pub async fn zmodem_pick(
    app: AppHandle,
    direction: String,
    title: Option<String>,
) -> Result<Option<String>, AppError> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    // 标题由前端按界面语言经 i18n 字典下发；未传时回落英文（开源默认语），不写死具体语言。
    let fallback = if direction == "upload" {
        "Select file to upload (rz)"
    } else {
        "Select download folder (sz)"
    };
    let dialog = app.dialog().file().set_title(title.unwrap_or_else(|| fallback.into()));
    if direction == "upload" {
        dialog.pick_file(move |p| {
            let _ = tx.send(p.map(|p| p.simplified().to_string()));
        });
    } else {
        dialog.pick_folder(move |p| {
            let _ = tx.send(p.map(|p| p.simplified().to_string()));
        });
    }
    rx.await
        .map_err(|_| AppError::Internal("无法打开文件选择框".into()))
}

/// 用户取消文件选择：通知 actor 放弃本次握手（发 CAN 恢复终端）。
#[tauri::command]
pub async fn zmodem_terminal_cancel(app: AppHandle, session_id: String) -> Result<(), AppError> {
    let tx = app
        .state::<AppState>()
        .sessions
        .sender(&session_id)
        .ok_or_else(|| AppError::NoSuchSession(session_id.clone()))?;
    tx.send(SessionCmd::ZmodemCancel)
        .await
        .map_err(|_| AppError::NoSuchSession(session_id))
}
