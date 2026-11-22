//! 通用系统文件 / 目录选择框（原生 dialog）。
//!
//! 统一入口：凡是「让用户选本机文件或目录 / 另存为」的地方都走这里，替代手输路径。
//!
//! 关键：**用回调式（非阻塞）API + `async` 命令**，绝不在同步命令里用 `blocking_pick_*`。
//! 同步 `#[tauri::command] fn` 跑在主线程，`blocking_*` 会卡死主线程消息循环，导致：
//! ① 整窗被 Windows 判定「无响应」；② 原生对话框内「新建文件夹 → 就地重命名」的编辑框
//! 收不到消息，像卡住一样改不了名。回调式 API 由插件在主线程**非模态**开框、用户选完后
//! 经 oneshot 回传，主线程全程在泵消息，两个症状一并消除。
//! 取消一律回调 `None`，由前端决定不动作。

use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;

use crate::error::AppError;

/// 选单个已存在文件（打开口）。`title` 为对话框标题，可空。
#[tauri::command]
pub async fn dialog_pick_file(
    app: AppHandle,
    title: Option<String>,
) -> Result<Option<String>, AppError> {
    let (tx, rx) = oneshot::channel();
    let mut b = app.dialog().file();
    if let Some(t) = title {
        b = b.set_title(t);
    }
    b.pick_file(move |p| {
        let _ = tx.send(p.map(|p| p.simplified().to_string()));
    });
    // oneshot 断链仅在「发方被丢弃」（对话框根本没能打开）时发生；用户取消会正常回调 None。
    rx.await
        .map_err(|_| AppError::Internal("无法打开文件选择框".into()))
}

/// 选目录。`title` 可空。
#[tauri::command]
pub async fn dialog_pick_folder(
    app: AppHandle,
    title: Option<String>,
) -> Result<Option<String>, AppError> {
    let (tx, rx) = oneshot::channel();
    let mut b = app.dialog().file();
    if let Some(t) = title {
        b = b.set_title(t);
    }
    b.pick_folder(move |p| {
        let _ = tx.send(p.map(|p| p.simplified().to_string()));
    });
    rx.await
        .map_err(|_| AppError::Internal("无法打开目录选择框".into()))
}

/// 另存为。可给默认文件名与扩展名过滤（`filter_name` + `extensions`，如 "JSON" + ["json"]）。
#[tauri::command]
pub async fn dialog_save_file(
    app: AppHandle,
    title: Option<String>,
    default_name: Option<String>,
    filter_name: Option<String>,
    extensions: Option<Vec<String>>,
) -> Result<Option<String>, AppError> {
    let (tx, rx) = oneshot::channel();
    let mut b = app.dialog().file();
    if let Some(t) = title {
        b = b.set_title(t);
    }
    if let Some(n) = default_name {
        b = b.set_file_name(n);
    }
    if let (Some(fname), Some(exts)) = (filter_name, extensions) {
        let refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
        b = b.add_filter(fname, &refs);
    }
    b.save_file(move |p| {
        let _ = tx.send(p.map(|p| p.simplified().to_string()));
    });
    rx.await
        .map_err(|_| AppError::Internal("无法打开另存为框".into()))
}
