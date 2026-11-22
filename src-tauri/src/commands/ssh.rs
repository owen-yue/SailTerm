//! SSH 认证/裁决类命令：主机密钥（TOFU）用户裁决 / 查看 / 移除；
//! keyboard-interactive 作答回传（与后端 `ssh::host_key` / `ssh::kbd` 模块边界对齐）。

use tauri::AppHandle;

use super::{blocking, state};
use crate::db::host_key::HostKeyRecord;
use crate::error::AppError;
use crate::vault::Secret;

/// 应答 `host-key-unknown` 事件；返回 `false` 表示请求已失效（超时或重复应答）。
#[tauri::command]
pub async fn host_key_respond(
    app: AppHandle,
    request_id: String,
    accept: bool,
) -> Result<bool, AppError> {
    Ok(state(&app).host_keys.respond(&request_id, accept))
}

/// 应答 `ssh-kbd-request` 事件：把本轮提示的答案送回握手协程（入向凭证，同表单密码）；
/// `answers = None` 表示用户取消，后端按失败短路、绝不向服务端提交任何东西。
/// 返回 `false` 表示请求已失效（超时或重复应答）。答案保持 `Secret` 贯穿待答生命周期
/// （不落盘、不进事件/日志），仅在交给协议的那一刻由 russh 侧展开明文。
#[tauri::command]
pub async fn ssh_kbd_respond(
    app: AppHandle,
    request_id: String,
    answers: Option<Vec<Secret>>,
) -> Result<bool, AppError> {
    Ok(state(&app).ssh_kbd.respond(&request_id, answers))
}

/// 已信任的主机密钥列表（公钥与指纹属公开信息，可回前端展示）。
#[tauri::command]
pub async fn host_key_list(app: AppHandle) -> Result<Vec<HostKeyRecord>, AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.list_host_keys()
    })
    .await
}

/// 移除某主机已记录的全部密钥（服务器重装/密钥轮换后用），返回删除条数。
#[tauri::command]
pub async fn host_key_forget(app: AppHandle, host: String, port: u16) -> Result<usize, AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.forget_host_keys(&host, port)
    })
    .await
}
