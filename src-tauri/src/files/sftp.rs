//! SFTP 后端：在**已认证**的 SSH 连接上开 `sftp` 子系统通道。
//!
//! 复用会话句柄，不二次认证、不接触凭证；因此会话一断，
//! 缓存的通道立即失效，由 [`Registry::forget`] 丢弃，重连后重新打开。

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags, StatusCode};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, SeekFrom};
use tokio::time::timeout;

use super::{
    file_name, perms_string, sort_entries, CancelFlag, FileEntry, FileTransfer, Progress, CHUNK,
};
use crate::error::AppError;
use crate::session::SshHandle;
use crate::state::AppState;

/// 取该会话的 SFTP 后端：在**已认证**的 SSH 连接上开子系统通道，全程不碰凭证。
///
/// 由 [`super::backend`] 按会话类型分派过来；会话不存在与已断开给出不同提示，
/// 后者可直接引导用户重连。
pub async fn backend(app: &AppHandle, session_id: &str) -> Result<Arc<dyn FileTransfer>, AppError> {
    let st = app.state::<AppState>();
    let handle = match st.sessions.handle(session_id) {
        Some(h) => h,
        None if st.sessions.contains(session_id) => {
            return Err(AppError::Biz {
                key: "err.conn.dropped",
                zh: "连接已断开，请重连",
            })
        }
        None => return Err(AppError::NoSuchSession(session_id.to_string())),
    };
    let sftp = st.sftp.get(session_id, &handle).await?;
    Ok(Arc::new(SftpBackend::new(sftp)))
}

/// `session_id → SFTP 通道` 缓存。`SftpSession` 内部就是 `Arc<RawSftpSession>`，
/// 但它本身不是 `Clone`，因此这里再包一层 `Arc`：
/// 一次打开即可服务该会话上的全部文件操作。
#[derive(Default)]
pub struct Registry {
    inner: Mutex<HashMap<String, Arc<SftpSession>>>,
}

impl Registry {
    /// 取该会话的 SFTP 通道；首次调用时在既有连接上开子系统并缓存。
    /// `Mutex` 只在查表/写表瞬间持有，**绝不跨 `await`**。
    pub async fn get(
        &self,
        session_id: &str,
        handle: &SshHandle,
    ) -> Result<Arc<SftpSession>, AppError> {
        if let Some(cached) = self.inner.lock().unwrap().get(session_id).cloned() {
            return Ok(cached);
        }
        let sftp = Arc::new(open(handle).await?);
        self.inner
            .lock()
            .unwrap()
            .insert(session_id.to_string(), sftp.clone());
        Ok(sftp)
    }

    /// 会话断开/关闭时丢弃缓存：旧通道随连接一起失效。
    pub fn forget(&self, session_id: &str) {
        self.inner.lock().unwrap().remove(session_id);
    }
}

/// 子系统通道建立的整体超时上限（秒）。
///
/// 为什么需要：russh 的 `channel_open_session` / `request_subsystem` 在底层 TCP 半死
/// （对端不再回包、但 socket 未报错）时会**无限 await**；russh-sftp 的 10s 请求超时只覆盖
/// 会话建好之后的收发，救不了建会话本身。一旦卡在这里，`file_list` 永不返回（不是 Err），
/// `forget_on_error` 不触发、前端 loading 也清不掉 → 文件栏永久卡在「正在读取目录…」，重连
/// 若仍落到同一僵死句柄则照样进不去。兜一层上限，宁可快速报错让 UI 脱离死等。
const OPEN_TIMEOUT_SECS: u64 = 15;

/// 在既有连接上开 `sftp` 子系统通道（整体受 [`OPEN_TIMEOUT_SECS`] 保护）。
async fn open(handle: &SshHandle) -> Result<SftpSession, AppError> {
    timeout(Duration::from_secs(OPEN_TIMEOUT_SECS), open_inner(handle))
        .await
        .map_err(|_| {
            AppError::Sftp("建立 SFTP 通道超时，连接可能已失效，请重连后再试".into())
        })?
}

async fn open_inner(handle: &SshHandle) -> Result<SftpSession, AppError> {
    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    SftpSession::new(channel.into_stream())
        .await
        .map_err(sftp_err)
}

/// SFTP 后端：把 [`SftpSession`] 适配到统一的 [`FileTransfer`]。
pub struct SftpBackend {
    sftp: Arc<SftpSession>,
}

impl SftpBackend {
    pub fn new(sftp: Arc<SftpSession>) -> Self {
        Self { sftp }
    }
}

#[async_trait]
impl FileTransfer for SftpBackend {
    async fn canonicalize(&self, path: &str) -> Result<String, AppError> {
        self.sftp.canonicalize(path).await.map_err(sftp_err)
    }

    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, AppError> {
        let mut out = Vec::new();
        // `ReadDir` 迭代器已自行过滤 `.` 与 `..`，且 `path()` 用 `/` 拼好全路径
        for item in self.sftp.read_dir(path).await.map_err(sftp_err)? {
            let md = item.metadata();
            // 软链需跟随目标定类型：read_dir 给的是 lstat 语义，目录软链（如 /sbin→usr/sbin）
            // 会被报成“非目录”→ 显示文件图标、双击变下载、点不进去。
            // metadata 是 follow 语义；断链/无权限时退回 lstat 属性，至少不丢这一行。
            let attrs = if md.is_symlink() {
                match self.sftp.metadata(item.path()).await {
                    Ok(t) => t,
                    Err(_) => md,
                }
            } else {
                md
            };
            out.push(entry(&item.file_name(), item.path(), &attrs));
        }
        // 目录在前，其余按名称（忽略大小写）——符合文件管理器直觉
        sort_entries(&mut out);
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<Option<FileEntry>, AppError> {
        match self.sftp.metadata(path).await {
            Ok(attrs) => Ok(Some(entry(&file_name(path), path.to_string(), &attrs))),
            // “不存在”是正常询问结果，不当错误抛
            Err(SftpError::Status(s)) if s.status_code == StatusCode::NoSuchFile => Ok(None),
            Err(e) => Err(sftp_err(e)),
        }
    }

    async fn download(
        &self,
        remote: &str,
        local: &Path,
        offset: u64,
        cancel: &CancelFlag,
        progress: &Progress,
    ) -> Result<u64, AppError> {
        let mut src = self.sftp.open(remote).await.map_err(sftp_err)?;
        // 续传时不截断本地已有内容（断点续传）
        let mut dst = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(offset == 0)
            .open(local)
            .await
            .map_err(|e| AppError::Io(format!("无法创建本地文件 {}：{e}", local.display())))?;
        if offset > 0 {
            src.seek(SeekFrom::Start(offset)).await.map_err(sftp_io)?;
            dst.seek(SeekFrom::Start(offset))
                .await
                .map_err(|e| AppError::Io(format!("无法定位本地文件 {}：{e}", local.display())))?;
        }

        let mut buf = vec![0u8; CHUNK];
        let mut total = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = src.close().await;
                return Err(AppError::Transfer("传输已取消".into()));
            }
            let n = src.read(&mut buf).await.map_err(sftp_io)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])
                .await
                .map_err(|e| AppError::Io(format!("写入本地文件 {} 失败：{e}", local.display())))?;
            total += n as u64;
            progress(n as u64);
        }
        dst.flush()
            .await
            .map_err(|e| AppError::Io(format!("写入本地文件 {} 失败：{e}", local.display())))?;
        dst.sync_all()
            .await
            .map_err(|e| AppError::Io(format!("落盘本地文件 {} 失败：{e}", local.display())))?;
        let _ = src.close().await;
        Ok(total)
    }

    async fn upload(
        &self,
        local: &Path,
        remote: &str,
        offset: u64,
        cancel: &CancelFlag,
        progress: &Progress,
    ) -> Result<u64, AppError> {
        let mut src = tokio::fs::File::open(local)
            .await
            .map_err(|e| AppError::Io(format!("无法读取本地文件 {}：{e}", local.display())))?;
        // 续传保留远端已写入部分，否则整体覆盖
        let flags = if offset > 0 {
            OpenFlags::CREATE | OpenFlags::WRITE
        } else {
            OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE
        };
        let mut dst = self
            .sftp
            .open_with_flags(remote, flags)
            .await
            .map_err(sftp_err)?;
        if offset > 0 {
            src.seek(SeekFrom::Start(offset))
                .await
                .map_err(|e| AppError::Io(format!("无法定位本地文件 {}：{e}", local.display())))?;
            dst.seek(SeekFrom::Start(offset)).await.map_err(sftp_io)?;
        }

        let mut buf = vec![0u8; CHUNK];
        let mut total = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = dst.close().await;
                return Err(AppError::Transfer("传输已取消".into()));
            }
            let n = src
                .read(&mut buf)
                .await
                .map_err(|e| AppError::Io(format!("读取本地文件 {} 失败：{e}", local.display())))?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await.map_err(sftp_io)?;
            total += n as u64;
            progress(n as u64);
        }
        dst.flush().await.map_err(sftp_io)?;
        dst.sync_all().await.map_err(sftp_err)?;
        dst.close().await?;
        Ok(total)
    }

    async fn mkdir(&self, path: &str) -> Result<(), AppError> {
        self.sftp.create_dir(path).await.map_err(sftp_err)
    }

    async fn remove(&self, path: &str, is_dir: bool) -> Result<(), AppError> {
        // RMD 语义上只删空目录，非空会被服务端回 Failure；这里先递归清空内容再删本身，
        // 与本地栏的 `remove_dir_all` 对齐。软链接一律当文件删（rm），绝不跟进目标目录递归。
        if is_dir {
            for item in self.list(path).await? {
                if item.is_dir && !item.is_symlink {
                    self.remove(&item.path, true).await?;
                } else {
                    self.sftp.remove_file(&item.path).await.map_err(sftp_err)?;
                }
            }
        }
        if is_dir {
            self.sftp.remove_dir(path).await.map_err(sftp_err)
        } else {
            self.sftp.remove_file(path).await.map_err(sftp_err)
        }
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), AppError> {
        self.sftp.rename(from, to).await.map_err(sftp_err)
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<(), AppError> {
        // 只改低 12 位权限，保留服务端原有的文件类型位（setstat 只发送有值的字段）
        let cur = self.sftp.metadata(path).await.map_err(sftp_err)?;
        let type_bits = cur.permissions.unwrap_or(0) & !0o7777;
        let attrs = FileAttributes {
            permissions: Some(type_bits | (mode & 0o7777)),
            ..<_>::default()
        };
        self.sftp.set_metadata(path, attrs).await.map_err(sftp_err)
    }

    async fn read_text(&self, path: &str, max_bytes: u64) -> Result<(String, u64), AppError> {
        let mut src = self.sftp.open(path).await.map_err(sftp_err)?;
        // size 拿不到（metadata 失败或字段缺失）时当“未知”读至 EOF，上限交给 max_bytes。
        // 绝不能退回 0 再夹紧出 1 字节的读取上限：外部编辑拿到残内容确认后写回，
        // 会把远端文件截断成 1 字节。
        let size = match src.metadata().await {
            Ok(m) => m.size,
            Err(_) => None,
        };
        let want = match size {
            Some(s) => max_bytes.min(s) as usize,
            None => max_bytes as usize,
        };
        let mut buf = Vec::with_capacity(want.min(64 * 1024));
        let mut tmp = vec![0u8; CHUNK.min(want.max(1))];
        while buf.len() < want {
            let n = src.read(&mut tmp).await.map_err(sftp_io)?;
            if n == 0 {
                break;
            }
            let take = n.min(want - buf.len());
            buf.extend_from_slice(&tmp[..take]);
        }
        let _ = src.close().await;
        Ok((
            String::from_utf8_lossy(&buf).into_owned(),
            size.unwrap_or(0),
        ))
    }

    async fn write_text(&self, path: &str, content: &str) -> Result<(), AppError> {
        let flags = OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE;
        let mut dst = self
            .sftp
            .open_with_flags(path, flags)
            .await
            .map_err(sftp_err)?;
        dst.write_all(content.as_bytes()).await.map_err(sftp_io)?;
        dst.flush().await.map_err(sftp_io)?;
        dst.sync_all().await.map_err(sftp_err)?;
        dst.close().await?;
        Ok(())
    }
}

/// 目录项 → 前端视图；`path` 由调用方拼好，不依赖服务端回传格式。
fn entry(name: &str, path: String, attrs: &FileAttributes) -> FileEntry {
    let is_dir = attrs.is_dir();
    let is_symlink = attrs.is_symlink();
    let raw_mode = attrs.permissions;
    FileEntry {
        name: name.to_string(),
        path,
        is_dir,
        is_symlink,
        size: attrs.len(),
        modified_ms: attrs.mtime.map(|s| i64::from(s) * 1000),
        perms: raw_mode.map(|m| perms_string(is_dir, is_symlink, m)),
        mode: raw_mode.map(|m| m & 0o7777),
        uid: attrs.uid,
        gid: attrs.gid,
        user: attrs.user.clone(),
        group: attrs.group.clone(),
    }
}

/// 远端数据流（`sftp::File` 的 `AsyncRead/AsyncWrite/AsyncSeek` 实现）抛的 IO 错误。
///
/// 必须与本地文件 IO 分开：两者都是 `std::io::Error`，若都走 `?` 会统一变成
/// 「文件读写失败：…」，用户会去查本地路径，而真正坏的是远端通道。
fn sftp_io(e: std::io::Error) -> AppError {
    AppError::Sftp(format!("远端数据传输失败：{e}"))
}

/// SFTP 协议/IO 错误 → 可读中文（错误码统一 `E_SFTP`，见）。
fn sftp_err(e: SftpError) -> AppError {
    match e {
        SftpError::Status(s) => {
            let head = match s.status_code {
                StatusCode::NoSuchFile => "远端路径不存在",
                StatusCode::PermissionDenied => "权限不足",
                StatusCode::Eof => "远端提前结束传输",
                StatusCode::NoConnection | StatusCode::ConnectionLost => "SFTP 连接已中断",
                StatusCode::OpUnsupported => "服务端不支持该操作",
                _ => "远端返回错误",
            };
            if s.error_message.is_empty() {
                AppError::Sftp(head.to_string())
            } else {
                AppError::Sftp(format!("{head}（{}）", s.error_message))
            }
        }
        SftpError::Timeout => AppError::Sftp("SFTP 操作超时".into()),
        other => AppError::Sftp(other.to_string()),
    }
}
