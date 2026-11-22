//! 文件传输领域层。目录名取 `files/` 而非 `ftp/`：
//! 它同时装着 SFTP、FTP/FTPS 与传输队列，而 SFTP 并不是 FTP。
//!
//! [`FileTransfer`] 把 SFTP 与 FTP/FTPS 收敛到同一组签名：[`sftp::SftpBackend`] 在已认证的
//! SSH 连接上开子系统通道，[`ftp::FtpBackend`] 自己持一条 FTP 控制连接，
//! [`backend`] 按会话类型分派，上层（命令层与传输队列）对两者一视同仁。
//!
//! 安全红线：两个后端都**复用已认证的连接**，
//! 本模块任何路径都不接触凭证，也不需要凭证。

pub mod ftp;
pub mod ftps_trust;
pub mod sftp;
pub mod transfer;

pub mod cache;

pub mod bundle;

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::error::AppError;
use crate::state::AppState;

/// 分块大小（取 32KB~64KB）：大文件不整块载入内存。
pub const CHUNK: usize = 64 * 1024;

/// 远端目录项（「属性查看」）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub name: String,
    /// 完整远端路径，前端可直接回传给其它命令。
    pub path: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    /// 修改时间（unix 毫秒）；服务端未提供则为 `None`。
    pub modified_ms: Option<i64>,
    /// POSIX 权限位（如 `0o644`）；非 Unix 服务端可能为 `None`。
    pub mode: Option<u32>,
    /// 人类可读权限串（如 `rw-r--r--`），直接供列表展示。
    pub perms: Option<String>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub user: Option<String>,
    pub group: Option<String>,
}

/// 进度回调：每传完一块调用一次，参数为本块字节数。
pub type Progress = Arc<dyn Fn(u64) + Send + Sync>;

/// 取消标志：传输循环逐块检查，置位即中止。
pub type CancelFlag = Arc<AtomicBool>;

/// SFTP 与 FTP/FTPS 的统一文件传输抽象。
#[async_trait]
pub trait FileTransfer: Send + Sync {
    /// 规范化远端路径（SFTP `realpath` / FTP `PWD`）。
    async fn canonicalize(&self, path: &str) -> Result<String, AppError>;

    /// 列目录；`.` 与 `..` 由后端过滤掉。
    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, AppError>;

    /// 取单个路径的属性；不存在返回 `None`。
    /// 冲突检测与断点续传定位（拿已有字节数）都走这里。
    async fn stat(&self, path: &str) -> Result<Option<FileEntry>, AppError>;

    /// 下载远端文件到 `local`；`offset` > 0 表示本地已有该字节数，从其后续传。
    /// 返回本次实际写入的字节数。
    async fn download(
        &self,
        remote: &str,
        local: &Path,
        offset: u64,
        cancel: &CancelFlag,
        progress: &Progress,
    ) -> Result<u64, AppError>;

    /// 上传 `local` 到远端；`offset` > 0 表示远端已有该字节数，从其后续写。
    async fn upload(
        &self,
        local: &Path,
        remote: &str,
        offset: u64,
        cancel: &CancelFlag,
        progress: &Progress,
    ) -> Result<u64, AppError>;

    async fn mkdir(&self, path: &str) -> Result<(), AppError>;

    /// 删除文件/目录；目录为**递归删除**（先清空内容再删本身，与本地 `remove_dir_all` 对齐）。
    async fn remove(&self, path: &str, is_dir: bool) -> Result<(), AppError>;

    async fn rename(&self, from: &str, to: &str) -> Result<(), AppError>;

    /// 修改权限位（如 `0o644`）；FTP 无对应语义时应返回不支持。
    async fn chmod(&self, path: &str, mode: u32) -> Result<(), AppError>;

    /// 读取文本文件内容（远程编辑器）：返回 `(UTF-8 有损文本, 文件总字节数)`。
    /// `max_bytes` 为读取上限（调用方负责截断提示）；二进制检测在命令层做。
    async fn read_text(&self, path: &str, max_bytes: u64) -> Result<(String, u64), AppError>;

    /// 覆盖写入文本；路径已存在则整体替换。
    async fn write_text(&self, path: &str, content: &str) -> Result<(), AppError>;
}

/// 取该会话的文件后端：按会话类型分派到 SFTP 子系统通道或 FTP 控制连接。
///
/// 命令层的唯一入口：两条路径都把「会话不存在」与「已断开」分开报错，
/// 后者可直接引导用户重连。
pub async fn backend(app: &AppHandle, session_id: &str) -> Result<Arc<dyn FileTransfer>, AppError> {
    let kind = app
        .state::<AppState>()
        .sessions
        .kind(session_id)
        .ok_or_else(|| AppError::NoSuchSession(session_id.to_string()))?;
    if kind.is_ftp() {
        // FTP 控制连接在建档时就已经认证好了，这里只是包一层适配器
        return ftp::backend(app, session_id);
    }
    sftp::backend(app, session_id).await
}

/// 文件命令的统一收尾：SFTP 出错即丢弃缓存通道，下次调用重开一条（自愈）。
///
/// 子系统通道可能单独死掉（服务端 sftp 进程退出）而 SSH 连接还在，
/// 此时缓存里留着的就是个死句柄；多开一次通道的代价远低于让用户卡在错误里。
///
/// FTP 会话不在此列：它没有可丢弃的缓存通道，断线由 [`ftp::FtpBackend`] 就地判定。
pub fn forget_on_error<T>(
    app: &AppHandle,
    session_id: &str,
    result: Result<T, AppError>,
) -> Result<T, AppError> {
    if result.is_err() {
        app.state::<AppState>().sftp.forget(session_id);
    }
    result
}

/// 拼接远端路径：统一用 `/` 分隔，且不产生 `//`。
pub fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() || parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

/// `ls -l` 风格的 10 字符权限串（类型位 + rwxrwxrwx）。
/// 本地与远端共用同一格式，前端不必分叉渲染。
pub fn perms_string(is_dir: bool, is_symlink: bool, mode: u32) -> String {
    const BITS: [u32; 9] = [
        0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001,
    ];
    const CHARS: [char; 9] = ['r', 'w', 'x', 'r', 'w', 'x', 'r', 'w', 'x'];
    let mut s = String::with_capacity(10);
    s.push(if is_symlink {
        'l'
    } else if is_dir {
        'd'
    } else {
        '-'
    });
    for (bit, c) in BITS.iter().zip(CHARS) {
        s.push(if mode & bit != 0 { c } else { '-' });
    }
    s
}

/// 目录项排序：目录在前，其余按名称忽略大小写（本地与远端共用同一观感）。
pub fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// 取路径的最后一段作为名称（远端用 `/`，本地用 [`Path::file_name`]）。
pub fn file_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_string()
}

/// 冲突时自动改名：`a.txt` → `a (1).txt`、`logs` → `logs (1)`。
pub fn dedup_name(name: &str, seq: u32) -> String {
    match name.rsplit_once('.') {
        // 隐藏文件（`.bashrc`）与无扩展名的目录不该被拆开加点
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({seq}).{ext}"),
        _ => format!("{name} ({seq})"),
    }
}

// ── 系统关键路径硬护栏 ────────────────────────────────────────────────
//
// 递归删除上线后，误删顶层系统目录的代价陡增。这里做一层**不可绕过**的后端拦截：
// 只拒删「根 / 盘符根」与「顶层关键目录本身」（宽松档：它们的子项照常可删，
// 不影响日常运维）。纯字符串判定、无 IO，远端与本地栏共用，便于穷举单测。

/// Unix 顶层关键目录名（不含前导 `/`）：这些目录**本身**禁止删除。
/// 远端（Linux 服务器）与 macOS/Linux 本地栏共用同一份判定。
const UNIX_PROTECTED_DIRS: &[&str] = &[
    "bin", "boot", "dev", "etc", "home", "lib", "lib32", "lib64", "libx32", "media", "mnt",
    "opt", "proc", "root", "run", "sbin", "srv", "sys", "tmp", "usr", "var",
    // macOS 关键顶层目录（Linux 服务器几乎不会出现，列上无害）
    "system", "library", "applications", "private", "volumes", "cores",
];

/// 判断一个 Unix 风格绝对路径（`/` 分隔）是否为「根 `/`」或「顶层关键目录本身」。
/// 只认绝对路径；相对路径交调用方规范化，这里一律放行。
pub fn is_protected_unix_root(path: &str) -> bool {
    let trimmed = path.trim_end_matches('/');
    // 根："/"（trim 后为空）
    if trimmed.is_empty() {
        return true;
    }
    // 必须是绝对路径才继续
    let Some(rest) = trimmed.strip_prefix('/') else {
        return false;
    };
    // 只有一层（形如 "etc"）才可能是顶层关键目录；含 "/" 的是其子项，放行
    !rest.contains('/') && UNIX_PROTECTED_DIRS.iter().any(|d| d.eq_ignore_ascii_case(rest))
}

/// Windows 关键目录判定：盘符根（`C:\`）与若干系统目录**本身**禁止删除。
/// 大小写不敏感，`/` 与 `\` 混用都归一；其子项（除明确列出的 System32 等）放行。
pub fn is_protected_windows_root(path: &str) -> bool {
    let lower = path.replace('/', "\\").to_lowercase();
    let lower = lower.trim_end_matches('\\');
    let b = lower.as_bytes();
    // 需要盘符前缀 "x:"（index 0/1 必为 ASCII，故 lower[2..] 落在字符边界）
    if b.len() < 2 || b[1] != b':' || !b[0].is_ascii_alphabetic() {
        return false;
    }
    let rest = lower[2..].trim_end_matches('\\');
    if rest.is_empty() {
        return true; // 盘符根 C:\
    }
    const PROTECTED: &[&str] = &[
        "\\windows",
        "\\program files",
        "\\program files (x86)",
        "\\programdata",
        "\\users",
        "\\windows\\system32",
        "\\windows\\syswow64",
        "\\windows\\sysnative",
    ];
    PROTECTED.iter().any(|p| rest == *p)
}

/// 本地栏删除护栏：按当前编译平台选对应判定。
pub fn is_protected_local(path: &str) -> bool {
    if cfg!(windows) {
        is_protected_windows_root(path)
    } else {
        is_protected_unix_root(path)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        dedup_name, file_name, is_protected_unix_root, is_protected_windows_root, join,
        perms_string,
    };

    #[test]
    fn joins_remote_paths_without_double_slash() {
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("", "etc"), "/etc");
        assert_eq!(join("/var/www/", "index.html"), "/var/www/index.html");
        assert_eq!(join("/var/www", "index.html"), "/var/www/index.html");
    }

    #[test]
    fn renders_ls_style_permission_string() {
        assert_eq!(perms_string(false, false, 0o644), "-rw-r--r--");
        assert_eq!(perms_string(true, false, 0o755), "drwxr-xr-x");
        assert_eq!(perms_string(false, true, 0o777), "lrwxrwxrwx");
    }

    #[test]
    fn takes_last_segment_as_name() {
        assert_eq!(file_name("/var/log/app.log"), "app.log");
        assert_eq!(file_name("/var/log/"), "log");
        assert_eq!(file_name("app.log"), "app.log");
    }

    #[test]
    fn renames_on_conflict_without_breaking_dots() {
        assert_eq!(dedup_name("app.log", 1), "app (1).log");
        assert_eq!(dedup_name("logs", 2), "logs (2)");
        assert_eq!(dedup_name(".bashrc", 1), ".bashrc (1)");
    }

    #[test]
    fn blocks_protected_unix_roots_only() {
        // 根与顶层关键目录本身：拦
        assert!(is_protected_unix_root("/"));
        assert!(is_protected_unix_root("/etc"));
        assert!(is_protected_unix_root("/usr/"));
        assert!(is_protected_unix_root("/var"));
        // 子项：宽松档放行
        assert!(!is_protected_unix_root("/var/log"));
        assert!(!is_protected_unix_root("/etc/ssh"));
        // 非关键顶层与相对路径：放行
        assert!(!is_protected_unix_root("/data"));
        assert!(!is_protected_unix_root("etc"));
    }

    #[test]
    fn blocks_protected_windows_roots_only() {
        // 盘符根与系统目录本身：拦（大小写 / 分隔符 / 盘符都归一）
        assert!(is_protected_windows_root("C:\\"));
        assert!(is_protected_windows_root("C:"));
        assert!(is_protected_windows_root("c:\\windows"));
        assert!(is_protected_windows_root("D:/Program Files"));
        assert!(is_protected_windows_root(r"C:\Windows\System32"));
        // 子项与用户目录：放行
        assert!(!is_protected_windows_root(r"C:\Windows\Temp"));
        assert!(!is_protected_windows_root(r"C:\Users\admin"));
        assert!(!is_protected_windows_root(r"E:\projects"));
    }
}
