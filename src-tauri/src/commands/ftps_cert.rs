//! FTPS 服务端证书（TOFU）命令：用户裁决 / 查看 / 移除。
//!
//! 与 [`host_key`] 同构，只是对象从 SSH 主机公钥换成 TLS 叶子证书。
//! 证书属公钥材料，可以回前端展示；表里也**只存公钥材料**，不含任何凭证。

use tauri::AppHandle;

use super::{blocking, state};
use crate::db::ftps_cert::FtpsCertRecord;
use crate::error::AppError;

/// 应答 `ftps-cert-unknown` 事件；返回 `false` 表示请求已失效（超时或重复应答）。
#[tauri::command]
pub async fn ftps_cert_respond(
    app: AppHandle,
    request_id: String,
    accept: bool,
) -> Result<bool, AppError> {
    Ok(state(&app).ftps_certs.respond(&request_id, accept))
}

/// 已信任的 FTPS 证书列表。
#[tauri::command]
pub async fn ftps_cert_list(app: AppHandle) -> Result<Vec<FtpsCertRecord>, AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.list_ftps_certs()
    })
    .await
}

/// 移除某 `host:port` 的证书记录（服务器换证书 / 怀疑中间人后重走一次 TOFU）。
#[tauri::command]
pub async fn ftps_cert_forget(app: AppHandle, host: String, port: u16) -> Result<usize, AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.forget_ftps_cert(&host, port)
    })
    .await
}
