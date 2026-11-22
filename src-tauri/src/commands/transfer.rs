//! 传输命令：提交 / 取消 / 重试 / 列表 / 清理。
//!
//! 命令只做校验与入队，**立即返回任务行**；分块搬运在 `files::transfer` 的 worker 协程里跑，
//! 进度与终态通过 `transfer-progress` / `transfer-state` 事件推给前端。
//!
//! 提交类逻辑（`run_*`）被命令层与 AI 工具层（`transfer_file`）共用：
//! AI 不另写一套搬运，只套一层「等终态」——同一队列、同一断点续传、同一去重口径。
//! 目录递归传输双向均已实现：本地/远端 walk 成逐文件任务（`run_upload_dir` / `run_download_dir`），
//! 达阈的整夹上传另走打包分流（tar → 传包 → 远端解压，失败自动回退逐文件）。

use tauri::AppHandle;

use super::{blocking, state};
use crate::commands::file::resolve_local_dir;
use crate::db::transfer::{Direction, TaskRow, TaskStatus};
use crate::error::AppError;
use crate::files::transfer::{self, OnConflict, Request, SubmitOutcome};
use crate::files::{backend, file_name, forget_on_error, FileTransfer};

/// 上传本地文件到远端目录（前端命令）。
#[tauri::command]
pub async fn transfer_upload(
    app: AppHandle,
    session_id: String,
    local_path: String,
    remote_dir: Option<String>,
    on_conflict: Option<OnConflict>,
) -> Result<SubmitOutcome, AppError> {
    run_upload(app, session_id, local_path, remote_dir, on_conflict).await
}

/// 下载远端文件到本地目录（前端命令）。
#[tauri::command]
pub async fn transfer_download(
    app: AppHandle,
    session_id: String,
    remote_path: String,
    local_dir: Option<String>,
    on_conflict: Option<OnConflict>,
) -> Result<SubmitOutcome, AppError> {
    run_download(app, session_id, remote_path, local_dir, on_conflict).await
}

/// 上传/下载的统一落地动作（AI 工具 `transfer_file` 用）：提交后**等到终态**才返回。
///
/// AI 这条路径没有冲突弹窗的通道，故 `onConflict` 缺省给 `Rename`（`Ask` 会让任务静静停在原地）。
/// 返回给模型的文本已含落地路径；文本前缀 `[传输未完成]` 表示失败/取消，调用方据此判成败。
pub async fn run_transfer(
    app: AppHandle,
    session_id: String,
    direction: Direction,
    src: String,
    dst_dir: Option<String>,
    on_conflict: Option<OnConflict>,
) -> Result<String, AppError> {
    let conflict = on_conflict.unwrap_or(OnConflict::Rename);
    let outcome = match direction {
        Direction::Upload => {
            run_upload(
                app.clone(),
                session_id.clone(),
                src.clone(),
                dst_dir,
                Some(conflict),
            )
            .await?
        }
        Direction::Download => {
            run_download(
                app.clone(),
                session_id.clone(),
                src.clone(),
                dst_dir,
                Some(conflict),
            )
            .await?
        }
    };
    Ok(match outcome {
        // 单文件传 Rename 不返 Skipped；Skipped 只来自「path 其实是目录」已被整夹入队。
        SubmitOutcome::Skipped => format!("{src} 是目录，已按整夹传输入队（逐个文件各自排队）"),
        SubmitOutcome::Conflict { .. } => unreachable!("AI 侧冲突策略不取 Ask"),
        SubmitOutcome::Queued { task } => wait_transfer(app, task).await,
    })
}

/// 等一个已入队任务跑到终态（轮询库，与 `transfer_cancel` 读同一张表）。
/// 超时**不取消**任务：它仍在队列里按并发闸门推进，前端传输面板照常显示与操作。
async fn wait_transfer(app: AppHandle, task: TaskRow) -> String {
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(120);
    let (id, name) = (task.id.clone(), task.remote_path.clone());
    loop {
        let current = blocking({
            let app = app.clone();
            let id = id.clone();
            move || state(&app).db.get_task(&id)
        })
        .await
        .unwrap_or(None)
        .unwrap_or(task.clone());
        if current.status.is_final() {
            let secs = started.elapsed().as_secs_f64().max(0.001);
            let rate = current.done_bytes as f64 / secs / 1024.0;
            return match current.status {
                TaskStatus::Done => format!(
                    "传输完成：{}（{} 字节，{:.0} KB/s）→ {}",
                    name, current.done_bytes, rate, current.local_path
                ),
                TaskStatus::Canceled => format!("[传输未完成] 已被取消：{name}"),
                other => format!(
                    "[传输未完成] 传输失败：{name}（{:?}：{}）",
                    other,
                    current.error.as_deref().unwrap_or("无详细信息")
                ),
            };
        }
        if std::time::Instant::now() > deadline {
            return format!(
                "传输仍在进行（任务 {id}）：大文件会慢，进度在传输面板可见，可稍后再问结果"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

/// 上传一个本地路径到远端目录（命令层与 AI 工具共用）。
///
/// `localPath` 是目录时走整夹上传；目录传输没有可交互的冲突弹窗，`onConflict` 缺省一律覆盖。
pub async fn run_upload(
    app: AppHandle,
    session_id: String,
    local_path: String,
    remote_dir: Option<String>,
    on_conflict: Option<OnConflict>,
) -> Result<SubmitOutcome, AppError> {
    let (is_dir, total) = blocking({
        let path = local_path.clone();
        move || {
            let md = std::fs::metadata(&path)
                .map_err(|e| AppError::Io(format!("本地文件不可读 {path}：{e}")))?;
            Ok((md.is_dir(), md.len()))
        }
    })
    .await?;
    if is_dir {
        // 整夹上传：一次调用落地多个任务，逐个弹窗不现实，目录树按覆盖整包落地。
        run_upload_dir(
            app,
            session_id,
            local_path,
            remote_dir,
            Some(OnConflict::Overwrite),
        )
        .await?;
        return Ok(SubmitOutcome::Skipped);
    }

    let ft = backend(&app, &session_id).await?;
    let result = async {
        // 目标目录用远端视角规范化，顺带确认它存在且可写得到
        let dst = remote_dst(&*ft, remote_dir.as_deref()).await?;
        transfer::submit(
            &app,
            ft,
            Request {
                session_id: session_id.clone(),
                direction: Direction::Upload,
                src: local_path,
                dst_dir: dst,
                total_bytes: total,
                on_conflict: on_conflict.unwrap_or_default(),
            },
        )
        .await
    }
    .await;
    forget_on_error(&app, &session_id, result)
}

/// 下载一个远端路径到本地目录（命令层与 AI 工具共用）。
///
/// `remotePath` 是目录（含 /bin 这类指向目录的符号链接）时走整夹下载，`onConflict` 缺省一律覆盖；
/// `localDir` 为空时用用户主目录，本地目录不可写就不起任务。
pub async fn run_download(
    app: AppHandle,
    session_id: String,
    remote_path: String,
    local_dir: Option<String>,
    on_conflict: Option<OnConflict>,
) -> Result<SubmitOutcome, AppError> {
    let dst_dir = pick_local_dir(local_dir.as_deref()).await?;

    let ft = backend(&app, &session_id).await?;
    let result = async {
        let src = ft
            .stat(&remote_path)
            .await?
            .ok_or_else(|| AppError::Sftp(format!("远端文件不存在：{remote_path}")))?;
        if src.is_dir {
            // 同整夹上传：目录树无冲突弹窗通道，一律覆盖落地。
            run_download_dir(
                app.clone(),
                session_id.clone(),
                remote_path.clone(),
                local_dir,
                Some(OnConflict::Overwrite),
            )
            .await?;
            return Ok(SubmitOutcome::Skipped);
        }
        transfer::submit(
            &app,
            ft,
            Request {
                session_id: session_id.clone(),
                direction: Direction::Download,
                src: remote_path,
                dst_dir,
                total_bytes: src.size,
                on_conflict: on_conflict.unwrap_or_default(),
            },
        )
        .await
    }
    .await;
    forget_on_error(&app, &session_id, result)
}

/// 上传本地目录树：先建远端目录树，再对每个文件各起一个传输任务。返回入队任务数。
///
/// **打包分流**（决策 C1）：开关开 + SSH + 文件数/字节达阈时，改起一条 bundle 任务
/// （本机 tar → 传单包 → 远端解压），worker 内部任一步失败自动回退逐文件；命中返回 1。
pub async fn run_upload_dir(
    app: AppHandle,
    session_id: String,
    local_path: String,
    remote_dir: Option<String>,
    on_conflict: Option<OnConflict>,
) -> Result<u32, AppError> {
    let files = blocking({
        let path = local_path.clone();
        move || walk_local_files(&path)
    })
    .await?;
    if files.is_empty() {
        return Ok(0);
    }
    let ft = backend(&app, &session_id).await?;
    let dst_root = remote_dst(&*ft, remote_dir.as_deref()).await?;
    let folder_name = file_name(&local_path);
    // 目录传输无冲突弹窗通道，缺省一律覆盖整夹落地。
    let conflict = on_conflict.unwrap_or(OnConflict::Overwrite);

    // 未压缩总字节：命中与否都只用于打包判定（逐文件路径不依赖它）。
    let total = blocking({
        let list = files.clone();
        move || {
            let mut sum = 0u64;
            for (p, _) in &list {
                if let Ok(m) = std::fs::metadata(p) {
                    sum += m.len();
                }
            }
            Ok(sum)
        }
    })
    .await?;

    let kind = crate::files::bundle::session_kind(&app, &session_id);
    let bundle_on = blocking({
        let app = app.clone();
        move || crate::commands::settings::bundle_transfer_enabled(&state(&app).db)
    })
    .await
    .unwrap_or(false);

    if bundle_on && crate::files::bundle::eligible(kind, files.len(), total) {
        tokio::spawn(transfer::run_bundle(
            app.clone(),
            ft,
            session_id,
            local_path,
            dst_root,
            folder_name,
            files,
            conflict,
        ));
        return Ok(1);
    }

    transfer::upload_dir_files(&app, &ft, &session_id, &files, &dst_root, &folder_name, conflict)
        .await
}

/// 递归下载远端目录到本地目录。返回入队文件数。
pub async fn run_download_dir(
    app: AppHandle,
    session_id: String,
    remote_path: String,
    local_dir: Option<String>,
    on_conflict: Option<OnConflict>,
) -> Result<u32, AppError> {
    let ft = backend(&app, &session_id).await?;
    let files = walk_remote_files(&*ft, &remote_path).await?;
    if files.is_empty() {
        return Ok(0);
    }
    let dst_root = pick_local_dir(local_dir.as_deref()).await?;
    // 整夹下载：本地也建同名文件夹（myfolder/...），而不是只摊开内容
    let folder_name = remote_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let root = if folder_name.is_empty() {
        std::path::PathBuf::from(&dst_root)
    } else {
        std::path::PathBuf::from(&dst_root).join(&folder_name)
    };
    std::fs::create_dir_all(&root)
        .map_err(|e| AppError::Io(format!("创建本地目录失败 {}: {e}", root.display())))?;
    let mut n = 0u32;
    for (remote, rel) in files {
        let target_dir = if rel.as_os_str().is_empty() {
            root.clone()
        } else {
            root.join(&rel)
        };
        std::fs::create_dir_all(&target_dir)
            .map_err(|e| AppError::Io(format!("创建本地目录失败 {}: {e}", target_dir.display())))?;
        let meta = ft
            .stat(&remote)
            .await?
            .ok_or_else(|| AppError::Sftp(format!("远端文件消失：{remote}")))?;
        // 目录下载默认整包落地：同名直接覆盖，避免 skip 掉半截树
        let _ = transfer::submit(
            &app,
            ft.clone(),
            Request {
                session_id: session_id.clone(),
                direction: Direction::Download,
                src: remote.clone(),
                dst_dir: target_dir.to_string_lossy().into_owned(),
                total_bytes: meta.size,
                on_conflict: on_conflict.unwrap_or(OnConflict::Overwrite),
            },
        )
        .await?;
        n += 1;
    }
    Ok(n)
}

/// 本地目录解析移出 async 执行器（空值取主目录，校验确实是目录）。
async fn pick_local_dir(dir: Option<&str>) -> Result<String, AppError> {
    let dir = dir.unwrap_or_default().to_string();
    blocking(move || resolve_local_dir(Some(dir.as_str()).filter(|s| !s.is_empty()))).await
}

/// 目标目录用远端视角规范化，顺带确认它存在；空值取远端当前目录。
async fn remote_dst(ft: &dyn FileTransfer, dir: Option<&str>) -> Result<String, AppError> {
    match dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(d) => ft.canonicalize(d).await,
        None => ft.canonicalize(".").await,
    }
}

/// 请求取消；返回取消后的任务行（前端据此就地更新，不必等事件）。
#[tauri::command]
pub async fn transfer_cancel(app: AppHandle, task_id: String) -> Result<TaskRow, AppError> {
    // 活动任务只置标志位，worker 会在下一块边界收尾并广播 `canceled`
    let live = state(&app).transfers.cancel(&task_id);
    blocking({
        let app = app.clone();
        let id = task_id.clone();
        move || {
            let db = &state(&app).db;
            let mut task = db
                .get_task(&id)?
                .ok_or_else(|| AppError::NoSuchTask(id.clone()))?;
            // 不在活动表却仍是非终态：上次进程退出遗留的行，直接落终态
            if !live && !task.status.is_final() {
                db.set_task_status(&id, TaskStatus::Canceled, Some("用户已取消"))?;
                task.status = TaskStatus::Canceled;
                task.error = Some("用户已取消".into());
            }
            Ok(task)
        }
    })
    .await
}

/// 重试一个已结束的任务：失败/取消的从已传字节续传，已完成的重头再传一次。
#[tauri::command]
pub async fn transfer_retry(app: AppHandle, task_id: String) -> Result<TaskRow, AppError> {
    let session_id = blocking({
        let app = app.clone();
        let id = task_id.clone();
        move || {
            let task = state(&app)
                .db
                .get_task(&id)?
                .ok_or_else(|| AppError::NoSuchTask(id.clone()))?;
            Ok(task.session_id)
        }
    })
    .await?;

    let ft = backend(&app, &session_id).await?;
    let result = transfer::retry(&app, ft, &task_id).await;
    forget_on_error(&app, &session_id, result)
}

/// 任务列表（最新在前）。两者都为空则返回全部会话的任务；
/// 同给 session + profile 时取并集（跨重连看本服务器完整历史）。
#[tauri::command]
pub async fn transfer_list(
    app: AppHandle,
    session_id: Option<String>,
    profile_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<TaskRow>, AppError> {
    blocking(move || {
        state(&app).db.list_tasks(
            session_id.as_deref(),
            profile_id.as_deref(),
            limit.unwrap_or(200),
        )
    })
    .await
}

/// 清掉已结束的任务行；进行中的保留。作用域=当前会话（面板只显本会话任务）。
/// 返回删除条数。
#[tauri::command]
pub async fn transfer_clear(app: AppHandle, session_id: Option<String>) -> Result<usize, AppError> {
    blocking(move || state(&app).db.clear_tasks(session_id.as_deref())).await
}

/// 上传本地目录树（前端命令）。
#[tauri::command]
pub async fn transfer_upload_dir(
    app: AppHandle,
    session_id: String,
    local_path: String,
    remote_dir: Option<String>,
    on_conflict: Option<OnConflict>,
) -> Result<u32, AppError> {
    run_upload_dir(app, session_id, local_path, remote_dir, on_conflict).await
}

/// 下载远端目录树（前端命令）。
#[tauri::command]
pub async fn transfer_download_dir(
    app: AppHandle,
    session_id: String,
    remote_path: String,
    local_dir: Option<String>,
    on_conflict: Option<OnConflict>,
) -> Result<u32, AppError> {
    run_download_dir(app, session_id, remote_path, local_dir, on_conflict).await
}

/// 本地目录 → (绝对路径, 相对父目录) 列表（只含文件）。
fn walk_local_files(root: &str) -> Result<Vec<(std::path::PathBuf, std::path::PathBuf)>, AppError> {
    let root_p = std::path::Path::new(root);
    if !root_p.is_dir() {
        return Err(AppError::Invalid(format!("不是目录：{root}")));
    }
    let mut out = Vec::new();
    fn rec(
        dir: &std::path::Path,
        base: &std::path::Path,
        out: &mut Vec<(std::path::PathBuf, std::path::PathBuf)>,
    ) -> std::io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            let p = e.path();
            if p.is_dir() {
                rec(&p, base, out)?;
            } else if p.is_file() {
                let rel = p.strip_prefix(base).unwrap_or(&p).to_path_buf();
                out.push((p, rel));
            }
        }
        Ok(())
    }
    rec(root_p, root_p, &mut out).map_err(|e| AppError::Io(format!("遍历本地目录失败：{e}")))?;
    Ok(out)
}

/// 远端目录 → (远端路径, 相对父目录) 列表（只含文件）。
async fn walk_remote_files(
    ft: &dyn crate::files::FileTransfer,
    root: &str,
) -> Result<Vec<(String, std::path::PathBuf)>, AppError> {
    let mut out = Vec::new();
    rec_remote(ft, root, root, &mut out).await?;
    Ok(out)
}

async fn rec_remote(
    ft: &dyn crate::files::FileTransfer,
    root: &str,
    dir: &str,
    out: &mut Vec<(String, std::path::PathBuf)>,
) -> Result<(), AppError> {
    let entries = ft.list(dir).await?;
    for e in entries {
        if e.name == "." || e.name == ".." {
            continue;
        }
        // 优先用后端给出的完整路径，避免拼接出错
        let path = if e.path.contains('/') || e.path.contains('\\') {
            e.path.clone()
        } else if dir.ends_with('/') {
            format!("{dir}{}", e.name)
        } else {
            format!("{dir}/{}", e.name)
        };
        if e.is_dir {
            Box::pin(rec_remote(ft, root, &path, out)).await?;
        } else {
            let root_trim = root.trim_end_matches('/');
            let rel_s = path
                .strip_prefix(root_trim)
                .unwrap_or(&path)
                .trim_start_matches('/')
                .to_string();
            let rel = std::path::Path::new(&rel_s)
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            out.push((path, rel));
        }
    }
    Ok(())
}
