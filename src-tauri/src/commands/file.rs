//! 文件浏览与管理命令：远端走 SFTP 或 FTP，本地走文件系统。
//!
//! 双栏浏览器左右两栏共用同一个 [`Listing`] / [`FileEntry`] 结构，前端渲染逻辑不必分叉。
//!
//! 关于本地侧权限：按用户当前浏览的目录读写是文件传输功能的应有之义，后端不再加沙箱；
//! 代价是这些命令**只能由本机 UI 触发**，不得把远端/AI 产出的路径直接喂进来。

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use tauri::AppHandle;

use super::blocking;
use crate::error::AppError;
use crate::files::{
    backend, cache, forget_on_error, is_protected_local, is_protected_unix_root, perms_string,
    sort_entries, FileEntry,
};

/// 列目录中一次完整 SFTP 往返（规范化 + read_dir + 逐项软链跟随）的整体超时上限。
///
/// 取 12s：小于前端 `call()` 对 `file_list` 的 60s 兜底，使死连接先在后端快速失败并自愈；
/// 又留足慢链路下正常大目录列完的余量（与 `sftp::open` 的 15s 同一量级）。
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// 短文件命令（cwd/mkdir/remove/rename/chmod）的单次兜底上限。
///
/// 与 `file_list` 的 12s 同一量级：russh-sftp 自带的 10s 请求超时依赖 tokio 定时器推进，
/// 运行时被同步阻塞钉住时定时器一并失效（看门狗实测过「两者皆停」），命令就永不 settle，
/// 前端弹窗永久卡死。这里再套一层同机制兜底只是「多数场景更快报错 + 语义化文案」；
/// 真正的逃生口是前端 `call()` 对这类短命令恢复 60s 墙钟超时（见 lib/tauri/client.ts）。
/// 超时按「可能已生效」处理：调用方照常失效缓存并提示用户刷新确认，不盲目重试。
const OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

fn op_timeout_err() -> AppError {
    AppError::Sftp("操作超时未响应：请求可能已送达远端，请刷新目录确认结果".into())
}

/// 一次列目录的结果：路径 + 上级 + 条目。本地与远端同构。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    /// 本次列出的目录绝对路径（已规范化）。
    pub path: String,
    /// 上级目录；已在根则为 `None`，前端据此禁用「返回上级」。
    pub parent: Option<String>,
    pub entries: Vec<FileEntry>,
}

/// 远端起始目录（SFTP 子系统默认落在用户家目录，FTP 落在登录后的当前目录）。
#[tauri::command]
pub async fn file_cwd(app: AppHandle, session_id: String) -> Result<String, AppError> {
    let ft = backend(&app, &session_id).await?;
    let r = tokio::time::timeout(OP_TIMEOUT, ft.canonicalize("."))
        .await
        .unwrap_or_else(|_| Err(op_timeout_err()));
    forget_on_error(&app, &session_id, r)
}

/// 列远端目录。
///
/// 缓存层（PR-3）：命中 TTL 20s 内的目录直接回；`refresh=Some(true)` 旁路缓存（上传完成后
/// 前端刷新列表时使用）。未命中回源后写入缓存，并对当前目录的子目录（≤ 32 个）后台预取。
#[tauri::command]
pub async fn file_list(
    app: AppHandle,
    session_id: String,
    path: String,
    refresh: Option<bool>,
) -> Result<Listing, AppError> {
    let ft = backend(&app, &session_id).await?;
    let force = refresh.unwrap_or(false);
    // 给「规范化 + 列目录」这一串真正的 SFTP 往返套整体上限。`sftp::open` 的 15s 只护住建通道那一步；
    // 通道缓存后连接半死（对端不再回包但 socket 未报错）时，`canonicalize`/`read_dir`/逐项 `metadata`
    // 会各自久挂，`file_list` 迟迟不返回 → 文件栏卡在「正在读取目录…」很久。套一层兜底上限，宁可
    // 快速报错：返回 Err 后 `forget_on_error` 丢弃这条死句柄，下次调用重开一条（自愈），不再久等。
    let result = match tokio::time::timeout(LIST_TIMEOUT, async {
        // 先规范化：面包屑与「返回上级」都要靠绝对路径；缓存 key 也用它
        let real = ft.canonicalize(&path).await?;
        if !force {
            if let Some(hit) = cache::global().get(&session_id, &real) {
                return Ok(Listing {
                    parent: remote_parent(&real),
                    path: real,
                    entries: hit,
                });
            }
        }
        let entries = ft.list(&real).await?;
        cache::global().put(&session_id, &real, entries.clone());
        // 预取（fire-and-forget）：不阻塞本次返回；失败静默。
        spawn_prefetch(app.clone(), session_id.clone(), &entries);
        Ok(Listing {
            parent: remote_parent(&real),
            path: real,
            entries,
        })
    })
    .await {
        Ok(r) => r,
        Err(_) => Err(AppError::Sftp(
            "读取目录超时：连接可能已失效，请重连后再试".into(),
        )),
    };
    forget_on_error(&app, &session_id, result)
}

/// 对当前目录的子目录后台预列，`Semaphore(4)` 限流。
/// 注意 permit 在 spawn 内部 acquire，for 循环不阻塞主目录返回。
fn spawn_prefetch(app: AppHandle, sid: String, entries: &[FileEntry]) {
    use std::sync::Arc;
    static SEM: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    let sem = SEM
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(cache::PREFETCH_CONCURRENCY)))
        .clone();
    for path in cache::prefetch_targets(entries) {
        let app = app.clone();
        let sid = sid.clone();
        let sem = sem.clone();
        tokio::spawn(async move {
            let Ok(_permit) = sem.acquire_owned().await else {
                return;
            };
            if cache::global().get(&sid, &path).is_some() {
                return;
            }
            let Ok(ft) = backend(&app, &sid).await else {
                return;
            };
            // 与 file_list 同一兑底上限：半死通道上预取同样会久挂，且 permit 被永久占住，
            // 四个名额耗尽后预取整体停摆。超时/出错即丢通道（与命令层同款自愈），静默放弃本次预取。
            let r = tokio::time::timeout(LIST_TIMEOUT, ft.list(&path)).await;
            match r {
                Ok(Ok(list)) => cache::global().put(&sid, &path, list),
                Ok(Err(e)) => {
                    let _ = forget_on_error::<Vec<FileEntry>>(&app, &sid, Err(e));
                }
                Err(_) => {
                    let _ = forget_on_error::<Vec<FileEntry>>(
                        &app,
                        &sid,
                        Err(AppError::Sftp("预取超时".into())),
                    );
                }
            }
        });
    }
}

/// 新建远端目录（不递归：父目录不存在即报错，与 mkdir 无 -p 一致）。
#[tauri::command]
pub async fn file_mkdir(app: AppHandle, session_id: String, path: String) -> Result<(), AppError> {
    let ft = backend(&app, &session_id).await?;
    let made = tokio::time::timeout(OP_TIMEOUT, ft.mkdir(&path)).await;
    let r = match made {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            // 仅对真实协议错回查：SFTP 没有「已存在」专用状态码，对同名路径 mkdir 只回笼统的 Failure
            // （FTP 同理回 550）。回查该路径：存在即名称冲突，给明确文案；这属正常业务结果，不像其它
            // 错误那样走 forget_on_error 去丢弃健康的 SFTP 通道。回查不到才原样抛出（父目录缺失/权限等真错误）。
            // 回查同样套超时，避免在半死通道上二次挂起。
            if matches!(e, AppError::Sftp(_) | AppError::Ftp(_)) {
                let st = tokio::time::timeout(OP_TIMEOUT, ft.stat(&path)).await;
                if matches!(st, Ok(Ok(Some(_)))) {
                    return Err(AppError::Conflict("该文件夹已存在".into()));
                }
            }
            forget_on_error(&app, &session_id, Err(e))
        }
        // 超时不进步骤回查：目录可能确实已建成（请求已送达），回查存在会误报「已存在」，直接按超时抛出。
        Err(_elapsed) => forget_on_error(&app, &session_id, Err(op_timeout_err())),
    };
    if r.is_ok() {
        cache::global().invalidate(&session_id, &path);
    }
    r
}

#[tauri::command]
pub async fn file_remove(
    app: AppHandle,
    session_id: String,
    path: String,
    is_dir: bool,
) -> Result<(), AppError> {
    // 系统关键路径硬护栏：只拦根/顶层关键目录本身（宽松档，其子项放行）。
    // 放在取后端之前：纯字符串判定不触碰连接，也不该因它而 forget 健康的 SFTP 通道。
    if is_dir && is_protected_unix_root(&path) {
        return Err(AppError::Biz {
            key: "err.remove.protected",
            zh: "该路径是系统关键目录，禁止删除",
        });
    }
    let ft = backend(&app, &session_id).await?;
    let made = tokio::time::timeout(OP_TIMEOUT, ft.remove(&path, is_dir))
        .await
        .unwrap_or_else(|_| Err(op_timeout_err()));
    let r = forget_on_error(&app, &session_id, made);
    if r.is_ok() {
        cache::global().invalidate(&session_id, &path);
    }
    r
}

#[tauri::command]
pub async fn file_rename(
    app: AppHandle,
    session_id: String,
    from: String,
    to: String,
) -> Result<(), AppError> {
    let ft = backend(&app, &session_id).await?;
    let moved = tokio::time::timeout(OP_TIMEOUT, ft.rename(&from, &to))
        .await
        .unwrap_or_else(|_| Err(op_timeout_err()));
    let r = forget_on_error(&app, &session_id, moved);
    // 超时也清缓存：改名可能已送达远端执行（正是弹窗卡死现场的成因），
    // 失效后前端重列才能拿到真相，不能留着旧条目假装无事发生。
    cache::global().invalidate(&session_id, &from);
    cache::global().invalidate(&session_id, &to);
    r
}

/// 改远端权限。`mode` 收八进制字符串（如 `"644"`），与 UI 上的写法一致。
#[tauri::command]
pub async fn file_chmod(
    app: AppHandle,
    session_id: String,
    path: String,
    mode: String,
) -> Result<(), AppError> {
    let bits = parse_mode(&mode)?;
    let ft = backend(&app, &session_id).await?;
    let done = tokio::time::timeout(OP_TIMEOUT, ft.chmod(&path, bits))
        .await
        .unwrap_or_else(|_| Err(op_timeout_err()));
    let r = forget_on_error(&app, &session_id, done);
    if r.is_ok() {
        cache::global().invalidate(&session_id, &path);
    }
    r
}

/// 编辑器可打开的最大文本文件（字节）；超出拒绝，引导用传输面板。
const EDIT_MAX_BYTES: u64 = 512 * 1024;

/// 远程文件读取（编辑器）：返回文本 + 是否被截断 + 总字节数。
/// 含 NUL 判定为二进制并拒绝；只走既有 SFTP/FTP 通道，不二次认证。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileText {
    pub content: String,
    pub truncated: bool,
    pub size: u64,
}

#[tauri::command]
pub async fn file_read(
    app: AppHandle,
    session_id: String,
    path: String,
) -> Result<FileText, AppError> {
    let ft = backend(&app, &session_id).await?;
    let result = async {
        let (content, size) = ft.read_text(&path, EDIT_MAX_BYTES).await?;
        reject_binary(&content)?;
        // size 为 0 可能是服务端未回 size（read_text 已改读至 EOF），此时内容本身即全量
        if size > EDIT_MAX_BYTES {
            return Err(AppError::Invalid(format!(
                "文件过大（{size} > 上限 {EDIT_MAX_BYTES} 字节），请用传输面板下载"
            )));
        }
        Ok(FileText {
            truncated: false,
            content,
            size,
        })
    }
    .await;
    forget_on_error(&app, &session_id, result)
}

/// 远程文件覆盖写入（编辑器保存）。
#[tauri::command]
pub async fn file_write(
    app: AppHandle,
    session_id: String,
    path: String,
    content: String,
) -> Result<(), AppError> {
    if content.len() as u64 > EDIT_MAX_BYTES {
        return Err(AppError::Invalid("内容超过编辑上限，未保存".into()));
    }
    let ft = backend(&app, &session_id).await?;
    let r = forget_on_error(&app, &session_id, ft.write_text(&path, &content).await);
    if r.is_ok() {
        cache::global().invalidate(&session_id, &path);
    }
    r
}

fn reject_binary(s: &str) -> Result<(), AppError> {
    if s.bytes().take(8192).any(|b| b == 0) {
        return Err(AppError::Invalid("二进制文件不支持在线编辑".into()));
    }
    Ok(())
}

/// 本地文件读取；路径来自用户在文件面板的选择/输入。
#[tauri::command]
pub async fn local_read(path: String) -> Result<FileText, AppError> {
    blocking(move || {
        let p = PathBuf::from(&path);
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if size > EDIT_MAX_BYTES {
            return Err(AppError::Invalid(format!(
                "文件过大（{size} > 上限 {EDIT_MAX_BYTES} 字节）"
            )));
        }
        let bytes = std::fs::read(&p).map_err(|e| AppError::Io(format!("读取失败：{e}")))?;
        let content = String::from_utf8_lossy(&bytes).into_owned();
        reject_binary(&content)?;
        Ok(FileText {
            content,
            truncated: false,
            size,
        })
    })
    .await
}

/// 本地文件覆盖写入。
#[tauri::command]
pub async fn local_write(path: String, content: String) -> Result<(), AppError> {
    blocking(move || {
        if content.len() as u64 > EDIT_MAX_BYTES {
            return Err(AppError::Invalid("内容超过编辑上限，未保存".into()));
        }
        std::fs::write(&path, content.as_bytes())
            .map_err(|e| AppError::Io(format!("保存失败：{e}")))
    })
    .await
}

/// 「另存为」导出用文本落盘的上限：远高于编辑器（导出终端输出 / 会话日志可能很大）。
const SAVE_TEXT_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// 把一段文本写到用户选定的路径（终端「导出输出…」等另存为场景）。
///
/// 与 `local_write` 同为“本机 UI 触发”的信任级：`path` 来自系统保存框（用户亲自选定），
/// 不得把远端 / AI 产出的路径喂进来。单独开一条是为了解除编辑器的 512KB 上限。
#[tauri::command]
pub async fn local_save_text(path: String, content: String) -> Result<(), AppError> {
    blocking(move || {
        if content.len() as u64 > SAVE_TEXT_MAX_BYTES {
            return Err(AppError::Invalid("内容过大，未导出".into()));
        }
        std::fs::write(&path, content.as_bytes())
            .map_err(|e| AppError::Io(format!("导出失败：{e}")))
    })
    .await
}

/// 列本地目录。
/// Windows：`path` 为空 / `computer` / `此电脑` 时列**盘符**（此电脑视图）；
/// 其它平台或已给真实路径时照旧走目录。
#[tauri::command]
pub async fn local_list(path: Option<String>) -> Result<Listing, AppError> {
    blocking(move || {
        let raw = path.as_deref().map(str::trim).unwrap_or("");
        // 仅显式「此电脑 / computer」列盘符；空路径回主目录（默认仍是文件夹视图）
        if is_computer_root(raw) {
            return list_drives();
        }
        let dir = canonical_local_dir(path.as_deref())?;
        let mut entries = Vec::new();
        for item in std::fs::read_dir(&dir)
            .map_err(|e| AppError::Io(format!("无法打开目录 {}：{e}", shown(&dir))))?
        {
            let item = item.map_err(|e| AppError::Io(e.to_string()))?;
            entries.push(local_entry(&item)?);
        }
        sort_entries(&mut entries);
        let shown_path = shown(&dir);
        // 盘符根（C:\）的上级是「此电脑」，而不是 None
        let parent = if is_drive_root(&shown_path) {
            Some("此电脑".to_string())
        } else {
            dir.parent().map(shown)
        };
        Ok(Listing {
            parent,
            path: shown_path,
            entries,
        })
    })
    .await
}

#[tauri::command]
pub async fn local_mkdir(path: String) -> Result<(), AppError> {
    blocking(move || {
        std::fs::create_dir_all(&path).map_err(|e| AppError::Io(format!("新建目录失败：{e}")))
    })
    .await
}

#[tauri::command]
pub async fn local_remove(path: String, is_dir: bool) -> Result<(), AppError> {
    // 与远端同构的硬护栏：本地盘符根 / 系统目录本身禁删（防误伤整块系统）。
    if is_dir && is_protected_local(&path) {
        return Err(AppError::Biz {
            key: "err.remove.protected",
            zh: "该路径是系统关键目录，禁止删除",
        });
    }
    blocking(move || {
        let r = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        r.map_err(|e| AppError::Io(format!("删除失败：{e}")))
    })
    .await
}

#[tauri::command]
pub async fn local_rename(from: String, to: String) -> Result<(), AppError> {
    blocking(move || {
        std::fs::rename(&from, &to).map_err(|e| AppError::Io(format!("重命名失败：{e}")))
    })
    .await
}

/// 本地目录解析（对外）：返回去掉 Windows `\\?\` 前缀的展示路径，可直接入库/回显。
pub(crate) fn resolve_local_dir(path: Option<&str>) -> Result<String, AppError> {
    canonical_local_dir(path).map(|p| shown(&p))
}

/// 本地目录解析：空值取主目录，并校验确实是个目录。
fn canonical_local_dir(path: Option<&str>) -> Result<PathBuf, AppError> {
    let dir = match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => expand_tilde(p)?,
        None => home_dir()?,
    };
    // canonicalize 顺带校验存在性；符号链接目录会被解析到真实路径
    let real = std::fs::canonicalize(&dir)
        .map_err(|e| AppError::Io(format!("本地路径不可用 {}：{e}", dir.display())))?;
    if !real.is_dir() {
        return Err(AppError::Invalid(format!("不是目录：{}", shown(&real))));
    }
    Ok(real)
}

/// 展开开头的 `~` / `~/` / `~\` 为主目录：AI 与用户习惯用 Unix 的 `~`，
/// Windows 下它不是特殊字符，不展开会被当成字面目录名而 canonicalize 失败（os error 2）。
/// 仅处理裸 `~` 与 `~` 后跟分隔符；`~user` 之类不在此列，按字面处理。
fn expand_tilde(raw: &str) -> Result<PathBuf, AppError> {
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        let home = home_dir()?;
        // "~/" 这类尾随分隔符：rest 为空即主目录本身
        return Ok(if rest.trim().is_empty() {
            home
        } else {
            home.join(rest)
        });
    }
    Ok(PathBuf::from(raw))
}

fn is_computer_root(raw: &str) -> bool {
    raw == "computer" || raw == "此电脑" || raw == "Computer"
}

/// `C:\` / `D:/` 这类盘符根。
fn is_drive_root(path: &str) -> bool {
    let p = path.trim_end_matches(['\\', '/']);
    // 形如 C: 或 C:/
    let bytes = p.as_bytes();
    bytes.len() == 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

/// Windows 盘符列表（此电脑）；非 Windows 回退主目录。
fn list_drives() -> Result<Listing, AppError> {
    #[cfg(windows)]
    {
        let mut entries = Vec::new();
        for letter in b'A'..=b'Z' {
            let root = format!("{}:\\", letter as char);
            if std::path::Path::new(&root).is_dir() {
                entries.push(FileEntry {
                    name: format!("{}:", letter as char),
                    path: root,
                    is_dir: true,
                    is_symlink: false,
                    size: 0,
                    modified_ms: None,
                    mode: None,
                    perms: None,
                    uid: None,
                    gid: None,
                    user: None,
                    group: None,
                });
            }
        }
        if entries.is_empty() {
            return home_dir().and_then(|h| {
                let dir = std::fs::canonicalize(h).map_err(|e| AppError::Io(e.to_string()))?;
                Ok(Listing {
                    parent: None,
                    path: shown(&dir),
                    entries: Vec::new(),
                })
            });
        }
        Ok(Listing {
            parent: None,
            path: "此电脑".to_string(),
            entries,
        })
    }
    #[cfg(not(windows))]
    {
        let dir = home_dir()?;
        Ok(Listing {
            parent: None,
            path: shown(&dir),
            entries: Vec::new(),
        })
    }
}

fn home_dir() -> Result<PathBuf, AppError> {
    // 不为此引入额外依赖：Windows 看 USERPROFILE，类 Unix 看 HOME
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| AppError::Invalid(format!("无法定位用户主目录（{key} 未设置）")))
}

fn local_entry(item: &std::fs::DirEntry) -> Result<FileEntry, AppError> {
    let symlink = item.file_type().map(|t| t.is_symlink()).unwrap_or(false);
    // 断链的符号链接取不到 metadata，退回 lstat 结果，至少能列出名字
    let md = item
        .metadata()
        .or_else(|_| std::fs::symlink_metadata(item.path()))
        .map_err(|e| AppError::Io(e.to_string()))?;
    let path = shown(&item.path());
    let mode = unix_mode(&md);
    Ok(FileEntry {
        name: item.file_name().to_string_lossy().into_owned(),
        path: path.clone(),
        is_dir: md.is_dir(),
        is_symlink: symlink,
        size: md.len(),
        modified_ms: mtime_ms(&md),
        perms: mode.map(|m| perms_string(md.is_dir(), symlink, m)),
        mode,
        // 本地侧不回传属主：Windows 无此概念，Linux 上对用户也基本无用
        uid: None,
        gid: None,
        user: None,
        group: None,
    })
}

fn mtime_ms(md: &Metadata) -> Option<i64> {
    md.modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

#[cfg(unix)]
fn unix_mode(md: &Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(md.permissions().mode() & 0o7777)
}

/// Windows 没有 POSIX 权限位，按只读标志给个近似值，够列表展示用。
#[cfg(not(unix))]
fn unix_mode(md: &Metadata) -> Option<u32> {
    Some(if md.permissions().readonly() {
        0o444
    } else {
        0o666
    })
}

/// 远端上级目录；已在根（`/`）则为 `None`。
fn remote_parent(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.rfind('/') {
        Some(0) => Some("/".to_string()),
        Some(i) => Some(trimmed[..i].to_string()),
        None => None,
    }
}

/// Windows 的 `canonicalize` 会加 `\\?\` 扩展前缀，展示给用户前去掉。
fn shown(p: &Path) -> String {
    let s = p.display().to_string();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_string(),
        None => s,
    }
}

fn parse_mode(mode: &str) -> Result<u32, AppError> {
    let text = mode
        .trim()
        .trim_start_matches("0o")
        .trim_start_matches("0O");
    let bits = u32::from_str_radix(text, 8)
        .map_err(|_| AppError::Invalid(format!("权限应为八进制数字，如 644（收到：{mode}）")))?;
    if bits > 0o7777 {
        return Err(AppError::Invalid("权限超出 0777 范围".into()));
    }
    Ok(bits)
}

#[cfg(test)]
mod tests {
    use super::{parse_mode, remote_parent, shown, sweep_dir};
    use std::path::Path;

    /// 目录不存在静默返回；刚写入的文件未过 7 天阈值不被删（子目录同理）。
    /// （真过期的 mtime 制造需平台 API，阈值判定逻辑由 [`sweep_dir`] 的
    /// `modified() < stale` 单一表达式保证，不另拉依赖做集成覆盖。）
    #[test]
    fn sweep_tolerates_missing_dir_and_keeps_fresh_files() {
        let root = std::env::temp_dir().join(format!("sailterm-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // 不存在：不 panic、不报错
        sweep_dir(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("fresh.txt"), "x").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        sweep_dir(&root);
        assert!(root.join("fresh.txt").exists());
        assert!(root.join("sub").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_octal_mode_strings() {
        assert_eq!(parse_mode("644").unwrap(), 0o644);
        assert_eq!(parse_mode("0755").unwrap(), 0o755);
        assert_eq!(parse_mode("0o600").unwrap(), 0o600);
        assert!(parse_mode("999").is_err()); // 8/9 不是八进制位
        assert!(parse_mode("7777777").is_err());
    }

    #[test]
    fn finds_remote_parent_until_root() {
        assert_eq!(
            remote_parent("/var/log/app.log").as_deref(),
            Some("/var/log")
        );
        assert_eq!(remote_parent("/var").as_deref(), Some("/"));
        assert_eq!(remote_parent("/var/").as_deref(), Some("/"));
        assert_eq!(remote_parent("/"), None);
        assert_eq!(remote_parent("relative/path").as_deref(), Some("relative"));
    }

    #[test]
    fn strips_windows_verbatim_prefix() {
        assert_eq!(shown(Path::new(r"\\?\C:\Users\x")), r"C:\Users\x");
        assert_eq!(shown(Path::new("/home/x")), "/home/x");
    }
}

/// 外部编辑临时副本的根目录（供 [`sweep_edit_tmp`] 启动清扫与 `file_edit_external` 共用）。
pub(crate) fn edit_tmp_root() -> PathBuf {
    std::env::temp_dir().join("sailterm-edit")
}

/// 启动清扫：删掉 7 天前的编辑残留（崩溃 / 未提交即放弃的兜底），目录不存在静默跳过。
/// 远端文件可能是配置或密钥，不能让它们在 %TEMP% 永久遗留。
pub(crate) fn sweep_edit_tmp() {
    sweep_dir(&edit_tmp_root());
}

/// 实际清扫逻辑；`dir` 可注入供单测。只删普通文件，子目录即便 mtime 过期也跳过。
fn sweep_dir(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let stale = std::time::SystemTime::now() - std::time::Duration::from_secs(7 * 24 * 3600);
    for item in rd.flatten() {
        let md = match item.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let old = md.modified().ok().is_some_and(|t| t < stale);
        if old && md.is_file() {
            let _ = std::fs::remove_file(item.path());
        }
    }
}

/// 用本机编辑器编辑远程/本地文件：拉到临时目录 → 用 `editor` 或系统关联程序打开 →
/// 后台监视 mtime，变更即写回。返回临时路径便于前端提示。
#[tauri::command]
pub async fn file_edit_external(
    app: AppHandle,
    session_id: Option<String>,
    path: String,
    editor: Option<String>,
) -> Result<String, AppError> {
    // 同步 FS 段（建目录/写临时文件）移进 spawn_blocking：本机 IO（尤其 %TEMP%
    // 被重定向/网盘化时）可长挂，钉在 async worker 上会连带定时器失效（历次
    // 运行时冻结的成因形态）；远端 read_text 本就是 tokio 侧异步，留在外面。
    let local_str = blocking({
        let path = path.clone();
        let session_id = session_id.clone();
        move || {
            let tmp_root = edit_tmp_root();
            std::fs::create_dir_all(&tmp_root)
                .map_err(|e| AppError::Io(format!("创建临时目录失败：{e}")))?;
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "edit.txt".into());
            let local = tmp_root.join(format!("{}-{}", uuid::Uuid::new_v4().simple(), name));
            let text = match &session_id {
                Some(_) => None,
                None => Some(
                    std::fs::read_to_string(&path)
                        .map_err(|e| AppError::Io(format!("读取 {path} 失败：{e}")))?,
                ),
            };
            if let Some(t) = &text {
                std::fs::write(&local, t)
                    .map_err(|e| AppError::Io(format!("写临时文件失败：{e}")))?;
            }
            Ok::<_, AppError>(local.to_string_lossy().into_owned())
        }
    })
    .await?;

    // 拉内容到临时文件（远端：异步通道读；本地已在上面一次性拷好）
    let local = PathBuf::from(&local_str);
    if let Some(sid) = &session_id {
        let ft = crate::files::backend(&app, sid).await?;
        let t = ft.read_text(&path, 8 * 1024 * 1024).await?.0;
        blocking({
            let local = local.clone();
            move || {
                std::fs::write(&local, t).map_err(|e| AppError::Io(format!("写临时文件失败：{e}")))
            }
        })
        .await?;
    }

    // 打开：优先指定编辑器，否则系统默认
    let editor = editor
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match editor {
        Some(prog) => {
            let st = std::process::Command::new(&prog)
                .arg(&local_str)
                .spawn()
                .map_err(|e| AppError::Io(format!("启动编辑器 {prog} 失败：{e}")))?;
            let app2 = app.clone();
            let session_id2 = session_id.clone();
            let path2 = path.clone();
            let local2 = local.clone();
            std::thread::spawn(move || {
                let mut child = st;
                let _ = child.wait();
                let _ = notify_or_push_back(&app2, session_id2.as_deref(), &path2, &local2);
            });
        }
        None => {
            open_with_system(&app, &local_str)?;
            // 无进程句柄：定时监视 10 分钟，mtime 变化即触发回写/确认
            let app2 = app.clone();
            let session_id2 = session_id.clone();
            let path2 = path.clone();
            let local2 = local.clone();
            std::thread::spawn(move || {
                let mut last = std::fs::metadata(&local2)
                    .ok()
                    .and_then(|m| m.modified().ok());
                for _ in 0..600 {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    if let Ok(md) = std::fs::metadata(&local2) {
                        if let Ok(t) = md.modified() {
                            if last != Some(t) {
                                last = Some(t);
                                let _ = notify_or_push_back(
                                    &app2,
                                    session_id2.as_deref(),
                                    &path2,
                                    &local2,
                                );
                            }
                        }
                    }
                }
            });
        }
    }
    Ok(local_str)
}

/// 用系统默认程序打开远端文件：先整文件下载到临时副本，再交给原生 open。
///
/// 与 [`file_edit_external`] 的区别：不做文本读取、不监视回写——图片/PDF/压缩包等
/// 二进制「看一眼」而已，写回既无意义也会损坏文件（且 read_to_string 对二进制会直接报错）。
///
/// [`OPEN_MAX_BYTES`]：这条路径无进度、不可取消，不判量就会被一个伪装成 `logo.png`
/// 的数十 GB 远端文件整灌进临时目录（界面在这期间无任何反馈）；超阈拒掉，引导走传输队列。
const OPEN_MAX_BYTES: u64 = 256 * 1024 * 1024;

#[tauri::command]
pub async fn file_open_remote(
    app: AppHandle,
    session_id: String,
    path: String,
) -> Result<String, AppError> {
    let ft = backend(&app, &session_id).await?;
    // 先判量再拉：拿不到 size 的后端（`stat` 回 `None`）保守放行，不给正常小文件加一道必错的门。
    let st = forget_on_error(&app, &session_id, ft.stat(&path).await)?;
    if let Some(st) = st.filter(|e| e.size > OPEN_MAX_BYTES) {
        return Err(AppError::Invalid(format!(
            "文件过大（{} 字节 > 上限 {OPEN_MAX_BYTES} 字节），请用传输队列下载",
            st.size
        )));
    }
    let tmp_root = edit_tmp_root();
    std::fs::create_dir_all(&tmp_root)
        .map_err(|e| AppError::Io(format!("创建临时目录失败：{e}")))?;
    let name = Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download.bin".into());
    let local = tmp_root.join(format!("{}-{}", uuid::Uuid::new_v4().simple(), name));
    // 一次性整文件下载：无取消位、进度丢弃（不像传输面板那样需要实时反馈）。
    let cancel: crate::files::CancelFlag =
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let progress: crate::files::Progress = std::sync::Arc::new(|_| {});
    forget_on_error(
        &app,
        &session_id,
        ft.download(&path, &local, 0, &cancel, &progress).await,
    )?;
    let local_str = local.to_string_lossy().into_owned();
    open_with_system(&app, &local_str)?;
    Ok(local_str)
}

fn open_with_system(app: &AppHandle, local: &str) -> Result<(), AppError> {
    #[cfg(windows)]
    {
        // 安全红线：禁止 `cmd /C start`——远端文件名可含 & | 等 cmd 元字符
        //（恶意 SFTP/FTP 服务器给 `x&calc` 即在本机执行任意命令），
        // explorer 按 argv 收路径、不经 shell，关联程序语义与 start 一致。
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("explorer")
            .arg(local)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| AppError::Io(format!("打开文件失败：{e}")))?;
        let _ = app;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(local)
            .spawn()
            .map_err(|e| AppError::Io(format!("打开文件失败：{e}")))?;
        let _ = app;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(local)
            .spawn()
            .map_err(|e| AppError::Io(format!("打开文件失败：{e}")))?;
        let _ = app;
        Ok(())
    }
}

/// 监视点触发：远端文件**不自动写回**，发 `file-edit-dirty` 让前端确认（
/// 保存先问一声，不静默改服务器上的文件）；本地文件直接写回——改的就是它自己，无需确认。
fn notify_or_push_back(
    app: &AppHandle,
    session_id: Option<&str>,
    path: &str,
    local: &std::path::Path,
) -> Result<(), AppError> {
    if session_id.is_some() {
        use tauri::{Emitter, Manager};
        let _ = app.emit(
            "file-edit-dirty",
            serde_json::json!({ "path": path, "local": local.to_string_lossy() }),
        );
        // 用户此时大概率在外部编辑器里，把主窗口叫到前台让他看到确认弹窗；
        // 聚焦被系统限制时退化为任务栏闪烁（Windows）/ Dock 图标跳动（macOS）。
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.set_focus();
            let _ = w.request_user_attention(Some(tauri::UserAttentionType::Critical));
        }
        return Ok(());
    }
    push_back_blocking(app, session_id, path, local)
}

/// 写回前预览：对比本地临时文件与**当前远端**内容，给出逐行 diff（`-`旧 `+`新）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditPreview {
    pub diff: Vec<String>,
    pub remote_bytes: usize,
    pub local_bytes: usize,
}

#[tauri::command]
pub async fn file_edit_preview(
    app: AppHandle,
    session_id: Option<String>,
    path: String,
    local: String,
) -> Result<EditPreview, AppError> {
    let local_txt = std::fs::read_to_string(&local)
        .map_err(|e| AppError::Io(format!("读取临时文件失败：{e}")))?;
    let remote_txt = match &session_id {
        Some(sid) => {
            let ft = backend(&app, sid).await?;
            ft.read_text(&path, 8 * 1024 * 1024).await?.0
        }
        None => std::fs::read_to_string(&path).unwrap_or_default(),
    };
    let diff = line_diff(&remote_txt, &local_txt);
    Ok(EditPreview {
        diff,
        remote_bytes: remote_txt.len(),
        local_bytes: local_txt.len(),
    })
}

/// 极简逐行 diff（LCS）。
fn line_diff(old: &str, new: &str) -> Vec<String> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let (n, m) = (a.len(), b.len());
    if n > 2000 || m > 2000 {
        return vec![
            "（文件过长，仅显示规模）".into(),
            format!("- 远端 {} 行 / + 本地 {} 行", n, m),
        ];
    }
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push(format!("  {}", a[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            out.push(format!("- {}", a[i]));
            i += 1;
        } else {
            out.push(format!("+ {}", b[j]));
            j += 1;
        }
    }
    while i < n {
        out.push(format!("- {}", a[i]));
        i += 1;
    }
    while j < m {
        out.push(format!("+ {}", b[j]));
        j += 1;
    }
    out
}

#[tauri::command]
pub async fn file_edit_commit(
    app: AppHandle,
    session_id: Option<String>,
    path: String,
    local: String,
) -> Result<(), AppError> {
    let content = std::fs::read_to_string(&local)
        .map_err(|e| AppError::Io(format!("读取临时文件失败：{e}")))?;
    let result = match &session_id {
        Some(sid) => {
            let ft = backend(&app, sid).await?;
            ft.write_text(&path, &content).await
        }
        None => std::fs::write(&path, content).map_err(|e| AppError::Io(e.to_string())),
    };
    if result.is_ok() {
        use tauri::Emitter;
        let _ = app.emit("file-edit-synced", serde_json::json!({ "path": path }));
        if let Some(sid) = &session_id {
            cache::global().invalidate(sid, &path);
        }
        // 远端文件的临时副本用完即删，不在 %TEMP% 遗留（可能含配置/密钥）；
        // 本地文件编辑（无 session_id）的 local 就是被监视的源文件，不能删。
        if session_id.is_some() {
            let _ = std::fs::remove_file(&local);
        }
    }
    match &session_id {
        Some(sid) => forget_on_error(&app, sid, result),
        None => result,
    }
}

fn push_back_blocking(
    app: &AppHandle,
    session_id: Option<&str>,
    path: &str,
    local: &std::path::Path,
) -> Result<(), AppError> {
    use tauri::Emitter;
    let content = std::fs::read_to_string(local)
        .map_err(|e| AppError::Io(format!("读回临时文件失败：{e}")))?;
    let app_c = app.clone();
    let path_s = path.to_string();
    let session_id = session_id.map(|s| s.to_string());
    let rt = tokio::runtime::Runtime::new().map_err(|e| AppError::Io(e.to_string()))?;
    let r = rt.block_on(async move {
        match &session_id {
            Some(sid) => {
                let ft = crate::files::backend(&app_c, sid).await?;
                ft.write_text(&path_s, &content).await
            }
            None => std::fs::write(&path_s, content).map_err(|e| AppError::Io(e.to_string())),
        }
    });
    if r.is_ok() {
        let _ = app.emit("file-edit-synced", serde_json::json!({ "path": path }));
    }
    r
}
