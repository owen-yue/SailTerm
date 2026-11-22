//! 小文件打包传输（PR-4，决策 C1）——只服务**目录上传**这一条链路：
//!
//! - 触发：`kind == Ssh` 且文件数 ≥ [`MIN_FILES`] 且未压缩总字节 ≤ [`MAX_BYTES`]
//!   且远端 `uname` 非 Windows_NT（且开关打开，判定在命令层做，本模块只提供原语）。
//! - 流程：本机 `tar -czf` 打整目录 → 复用 SFTP 通道上传**单个** tgz → 远端 exec
//!   `tar -xzf` 解到目标目录 → 删远端 tgz 与本机临时包。
//! - 任一步失败：由调用方整体回退逐文件路径（不续传打包中途），本模块不 retry。
//!
//! 复用既有能力：解包/探测都走 [`crate::ai::exec::exec_command`]（在已认证连接上另开
//! exec 通道，与交互式 PTY 分离），搬运走 [`FileTransfer::upload`](crate::files::FileTransfer::upload)。
//! 打包只在 POSIX 远端做——`tar` 是 POSIX 标配，Windows OpenSSH Server 上 tar 兼容性存疑，
//! 探测到 Windows_NT 直接跳过打包退回逐文件。

use std::path::Path;
use std::process::Command;

use tauri::{AppHandle, Manager};

use crate::ai::exec::exec_command;
use crate::error::AppError;
use crate::session::SessionKind;
use crate::state::AppState;

use super::transfer::with_db;

/// 少于此文件数不值得打包（建包/解包的固定开销 > 省下的往返）。
pub const MIN_FILES: usize = 10;
/// 未压缩总字节上限：超出即退回逐文件（避免整包一次性吃掉本地/远端磁盘与内存）。
pub const MAX_BYTES: u64 = 500 * 1024 * 1024;
/// `uname` 探测超时（够快链路一次往返）。
const PROBE_TIMEOUT_MS: u64 = 5_000;
/// 远端解包超时：解 500MB 上限内的小文件包，留足 60s。
const EXTRACT_TIMEOUT_MS: u64 = 60_000;

/// 是否命中打包的**静态**条件（不含远端 OS 与开关，那两项在命令层查）。
pub fn eligible(kind: Option<SessionKind>, file_count: usize, total_bytes: u64) -> bool {
    kind == Some(SessionKind::Ssh) && file_count >= MIN_FILES && total_bytes <= MAX_BYTES
}

/// 远端是否 Windows（`uname -s` 报 `Windows_NT`，或 Git-Bash 的 `MINGW*`/`CYGWIN*`/`MSYS*`）。
/// 探测失败（无 uname / exec 出错）一律当作「不适合打包」返回 `true`，安全退回逐文件。
pub async fn remote_is_windows(app: &AppHandle, sid: &str) -> bool {
    match exec_command(app, sid, "uname -s 2>/dev/null || echo NONE", PROBE_TIMEOUT_MS).await {
        Ok(r) if r.exit_code == Some(0) => {
            let s = r.stdout.trim().to_ascii_uppercase();
            s.is_empty()
                || s == "NONE"
                || s.contains("Windows")
                || s.contains("MINGW")
                || s.contains("CYGWIN")
                || s.contains("MSYS")
        }
        // 非 0 退出或 exec 失败：无法确认可用 tar，保守判定为「不打包」。
        _ => true,
    }
}

/// 本机 `tar -czf <tgz_out> -C <local_dir.parent> <local_dir 名>`。
///
/// 以目录父级为基准打包，包内存的是 `<folder>/...` 相对路径，远端 `tar -xzf -C <dst_root>`
/// 解出来正好还原出 `dst_root/<folder>/...`（与逐文件「整夹上传在目标下建同名文件夹」一致）。
/// 走系统 `tar`（Win10+ / macOS / Linux 自带），同步 IO 放到阻塞线程上。
pub async fn compress_dir(local_dir: String, tgz_out: String) -> Result<(), AppError> {
    tokio::task::spawn_blocking(move || tar_compress(Path::new(&local_dir), Path::new(&tgz_out)))
        .await
        .map_err(|e| AppError::Transfer(format!("打包任务异常终止：{e}")))?
}

fn tar_compress(local_dir: &Path, tgz_out: &Path) -> Result<(), AppError> {
    let folder = local_dir
        .file_name()
        .ok_or_else(|| AppError::Invalid("目录路径无末段，无法打包".into()))?;
    let parent = local_dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // tar -czf <out> -C <parent> <folder>：stdout/stderr 收进退出码判定即可。
    let out = Command::new("tar")
        .arg("-czf")
        .arg(tgz_out)
        .arg("-C")
        .arg(parent)
        .arg(folder)
        .output()
        .map_err(|e| AppError::Transfer(format!("调用本机 tar 失败（需 Win10+/macOS/Linux 自带 tar）：{e}")))?;
    if !out.status.success() {
        return Err(AppError::Transfer(format!(
            "本机打包失败：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// 远端解包：`tar -xzf <remote_tgz> -C <dest>`。退出码非 0 视为失败（调用方整体回退逐文件）。
pub async fn extract_remote(
    app: &AppHandle,
    sid: &str,
    remote_tgz: &str,
    dest: &str,
) -> Result<(), AppError> {
    let cmd = format!(
        "tar -xzf {} -C {}",
        sh_quote(remote_tgz),
        sh_quote(dest)
    );
    let r = exec_command(app, sid, &cmd, EXTRACT_TIMEOUT_MS).await?;
    if r.timed_out {
        return Err(AppError::Transfer("远端解压超时".into()));
    }
    match r.exit_code {
        Some(0) => Ok(()),
        Some(code) => Err(AppError::Transfer(format!(
            "远端解压失败（退出码 {code}）：{}",
            r.stderr.trim()
        ))),
        None => Err(AppError::Transfer("远端解压未回传退出码".into())),
    }
}

/// 本任务在远端的临时包名（隐藏、带会话后缀避免并发碰撞）。
pub fn remote_tgz_name(task_id: &str) -> String {
    format!(".sailterm_bundle_{}.tgz", &task_id[..8.min(task_id.len())])
}

/// POSIX shell 单引号包裹：内部单引号按 `'\''` 规则转义，杜绝路径空格/特殊字符注入。
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 供命令层拿当前会话 kind（打包判定用），集中一处避免各处摸 state。
pub fn session_kind(app: &AppHandle, sid: &str) -> Option<SessionKind> {
    app.state::<AppState>().sessions.kind(sid)
}

/// 启动清扫（决策 D-a）：删掉本机 temp 下崩溃遗留的打包临时包 `sailterm_*.tgz`。
/// 全新进程不可能有自己正在跑的 bundle，但为避开同机多实例误删，只删 mtime 超 1 小时的。
/// best-effort、静默、失败不阻断启动（无日志依赖，直接吞错）。
pub fn sweep_local_tmp() {
    let dir = std::env::temp_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for item in rd.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("sailterm_") && name.ends_with(".tgz")) {
            continue;
        }
        let Ok(md) = item.metadata() else { continue };
        if md.is_file() && md.modified().is_ok_and(|t| t < stale) {
            let _ = std::fs::remove_file(item.path());
        }
    }
}

/// 崩溃泄漏的远端 bundle 临时包补删（决策 D-b）：在**新连上的 SSH 会话**上，把该 profile 历史里
/// 疑似泄漏（`db.pending_remote_cleanup` 判据）的远端临时包 best-effort `rm -f`。无论成败都标
/// `remote_cleaned=1`，避免每次连接重试。全程静默、失败不阻断连接。
pub async fn cleanup_leaked_remotes(app: AppHandle, session_id: String, profile_id: String) {
    let rows = match with_db(&app, move |db| db.pending_remote_cleanup(&profile_id)).await {
        Ok(r) if !r.is_empty() => r,
        _ => return,
    };
    for (id, path) in rows {
        // rm -f 对不存在文件也返回 0；exec 失败同样只标记不重试（下次连接也不再尝试）。
        let cmd = format!("rm -f {}", sh_quote(&path));
        let _ = exec_command(&app, &session_id, &cmd, PROBE_TIMEOUT_MS).await;
        let _ = with_db(&app, move |db| db.mark_remote_cleaned(&id)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eligible_requires_ssh_count_and_size() {
        // SSH + 达标文件数 + 达标字节 → 命中
        assert!(eligible(Some(SessionKind::Ssh), MIN_FILES, 1024));
        // 文件数不足
        assert!(!eligible(Some(SessionKind::Ssh), MIN_FILES - 1, 1024));
        // 字节超上限
        assert!(!eligible(Some(SessionKind::Ssh), MIN_FILES, MAX_BYTES + 1));
        // 边界：正好等于上限命中
        assert!(eligible(Some(SessionKind::Ssh), MIN_FILES, MAX_BYTES));
        // 非 SSH（FTP / Local / 未知）一律不打包
        assert!(!eligible(Some(SessionKind::Ftp), MIN_FILES, 1024));
        assert!(!eligible(Some(SessionKind::Local), MIN_FILES, 1024));
        assert!(!eligible(None, MIN_FILES, 1024));
    }

    #[test]
    fn remote_tgz_name_is_hidden_and_short() {
        let n = remote_tgz_name("abcdef1234567890");
        assert!(n.starts_with('.'));
        assert!(n.ends_with(".tgz"));
        // 只取前 8 位做后缀，控制长度
        assert!(n.contains("abcdef12"));
        assert!(!n.contains("34567890"));
    }

    #[test]
    fn sh_quote_wraps_and_escapes_single_quotes() {
        assert_eq!(sh_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(sh_quote("it's"), r#"'it'\''s'"#);
    }
}
