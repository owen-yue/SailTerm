//! 传输队列：并发闸门 + 分块进度事件 + 取消 + 退避重试 + 冲突裁决。
//!
//! 本模块只认 [`FileTransfer`]、不认协议：A 喂 [`SftpBackend`](super::sftp::SftpBackend)，
//! B 换 FTP 后端即可，队列/进度/取消/重试逻辑完全复用。
//!
//! 进度走**事件广播**（与 `session-output` 一致）而非 Tauri Channel：
//! 传输面板与文件浏览器都要看进度，广播比逐调用者建管道简单，也不会漏给后开的视图。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Semaphore;

use super::{dedup_name, file_name, join, CancelFlag, FileTransfer, Progress};
use super::{bundle, cache};
use crate::db::transfer::{
    Direction, TaskRow, TaskStatus, PHASE_COMPRESS, PHASE_EXTRACT, PHASE_TRANSFER,
};
use crate::db::{new_id, now_ms, Db};
use crate::error::AppError;
use crate::state::AppState;

/// 进度事件：`{taskId, transferred, total, speed}`。
pub const EVENT_PROGRESS: &str = "transfer-progress";
/// 状态事件：`pending → running → done | failed | canceled`。
pub const EVENT_STATE: &str = "transfer-state";
/// 打包阶段事件：`{ taskId, phase: "compress" | "transfer" | "extract" }`。
pub const EVENT_PHASE: &str = "transfer-phase";
/// 打包失败回退事件：`{ taskId, reason }`（bundle 行已删除，前端刷新会见到逐文件新行）。
pub const EVENT_FALLBACK: &str = "bundle-fallback";

/// 同时进行的传输数上限（默认 1~3，取中间值）。
const CONCURRENCY: usize = 2;
/// 进度节流间隔：64KB 分块在快链路上每秒可产生上千次回调，不能逐块广播。
const PROGRESS_INTERVAL_MS: i64 = 200;
/// 首次失败后的自动重试次数（退避 1s、2s），续传不重头开始。
const RETRIES: u32 = 2;
/// 冲突改名的探测上限，超出即让用户自己指定名字。
const RENAME_TRIES: u32 = 50;

/// 目标已存在时的处置策略（「冲突询问覆盖/跳过/重命名」）。
#[derive(Deserialize, Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OnConflict {
    /// 不擅自处理，回 [`SubmitOutcome::Conflict`] 让前端问用户。
    #[default]
    Ask,
    /// 直接覆盖（上传截断远端、下载截断本地）。
    Overwrite,
    /// 什么都不做。
    Skip,
    /// 自动改名 `a.txt → a (1).txt`，直到不冲突。
    Rename,
}

/// 提交结果：入队成功，或需要前端裁决/已被跳过。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum SubmitOutcome {
    Queued {
        task: TaskRow,
    },
    Conflict {
        /// 已存在的目标路径（上传为远端路径，下载为本地路径）。
        existing_path: String,
        existing_bytes: u64,
    },
    Skipped,
}

/// 一次传输请求。`src` / `dstDir` 的含义随方向翻转，命令层只管填对。
pub struct Request {
    pub session_id: String,
    pub direction: Direction,
    /// 源文件：上传 = 本地绝对路径，下载 = 远端绝对路径。
    pub src: String,
    /// 目标目录：上传 = 远端目录，下载 = 本地目录。
    pub dst_dir: String,
    /// 源文件字节数；未知填 0，进度条退化为不定长。
    pub total_bytes: u64,
    pub on_conflict: OnConflict,
}

/// 运行期状态：只在任务活动期驻留内存，结束即摘除（库里的行才是真相源）。
struct Active {
    session_id: String,
    cancel: CancelFlag,
    done: AtomicU64,
    total: AtomicU64,
}

/// 传输队列：活动任务表 + 并发闸门。
pub struct TransferManager {
    active: Mutex<HashMap<String, Arc<Active>>>,
    /// `Arc` 出来给 worker 独占持有许可，避免许可生命周期挂在 tauri `State` 借用上。
    gate: Arc<Semaphore>,
}

impl Default for TransferManager {
    fn default() -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
            gate: Arc::new(Semaphore::new(CONCURRENCY)),
        }
    }
}

impl TransferManager {
    /// 登记活动任务，返回共享的运行期状态。
    fn register(&self, task: &TaskRow) -> Arc<Active> {
        let active = Arc::new(Active {
            session_id: task.session_id.clone(),
            cancel: Arc::new(AtomicBool::new(false)),
            done: AtomicU64::new(task.done_bytes),
            total: AtomicU64::new(task.total_bytes),
        });
        self.active
            .lock()
            .unwrap()
            .insert(task.id.clone(), active.clone());
        active
    }

    fn peek(&self, id: &str) -> Option<Arc<Active>> {
        self.active.lock().unwrap().get(id).cloned()
    }

    fn forget(&self, id: &str) {
        self.active.lock().unwrap().remove(id);
    }

    fn gate(&self) -> Arc<Semaphore> {
        self.gate.clone()
    }

    /// 任务是否仍在排队/传输中。
    pub fn is_active(&self, id: &str) -> bool {
        self.active.lock().unwrap().contains_key(id)
    }

    /// 请求取消：置位标志，传输循环在下一块边界中止。
    /// 返回 `false` 表示任务不在活动表里（已结束，或从未存在）。
    pub fn cancel(&self, id: &str) -> bool {
        match self.peek(id) {
            Some(a) => {
                a.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// 会话断开/关闭时取消其全部传输：连接已死，继续重试没有意义。
    pub fn cancel_session(&self, session_id: &str) -> usize {
        let mut n = 0;
        for a in self.active.lock().unwrap().values() {
            if a.session_id == session_id {
                a.cancel.store(true, Ordering::Relaxed);
                n += 1;
            }
        }
        n
    }
}

/// 提交一个传输任务；冲突策略为 `Ask` 且目标已存在时**不入队**，交前端裁决。
pub async fn submit(
    app: &AppHandle,
    backend: Arc<dyn FileTransfer>,
    req: Request,
) -> Result<SubmitOutcome, AppError> {
    let name = file_name(&req.src);
    if name.is_empty() {
        return Err(AppError::Invalid("源文件路径不合法".into()));
    }
    // 上传：本地固定为源、远端可改名；下载：远端固定为源、本地可改名
    let (mut remote_path, mut local_path) = target_paths(&req, &name);

    if let Some(bytes) = target_size(&*backend, req.direction, &remote_path, &local_path).await? {
        match req.on_conflict {
            OnConflict::Ask => {
                return Ok(SubmitOutcome::Conflict {
                    existing_path: if req.direction == Direction::Upload {
                        remote_path
                    } else {
                        local_path.display().to_string()
                    },
                    existing_bytes: bytes,
                })
            }
            OnConflict::Skip => return Ok(SubmitOutcome::Skipped),
            OnConflict::Overwrite => {}
            OnConflict::Rename => {
                let renamed = pick_free_name(&*backend, &req, &name).await?;
                (remote_path, local_path) = target_paths(&req, &renamed);
            }
        }
    }

    // 归属服务器（profile）：重连后 session id 会变，按 profile 归组才能跨重连看到本服务器历史，
    // 删服务器时也能级联清理（Direct/Local 会话无 profile，不落归属）。
    let profile_id = app.state::<AppState>().sessions.profile_id(&req.session_id);
    let task = TaskRow {
        id: new_id(),
        session_id: req.session_id.clone(),
        profile_id,
        direction: req.direction,
        remote_path,
        local_path: local_path.display().to_string(),
        total_bytes: req.total_bytes,
        done_bytes: 0,
        status: TaskStatus::Pending,
        error: None,
        created_at: now_ms(),
        updated_at: now_ms(),
        phase: crate::db::transfer::PHASE_TRANSFER.to_string(),
    };
    with_db(app, {
        let row = task.clone();
        move |db| db.insert_task(&row)
    })
    .await?;

    launch(app, backend, &task, 0);
    Ok(SubmitOutcome::Queued { task })
}

/// 重试一个已结束的任务：`failed`/`canceled` 从目标端已有字节续传，`done` 重头再来。
pub async fn retry(
    app: &AppHandle,
    backend: Arc<dyn FileTransfer>,
    id: &str,
) -> Result<TaskRow, AppError> {
    if app.state::<AppState>().transfers.is_active(id) {
        return Err(AppError::Transfer("该任务正在进行中".into()));
    }
    let task = with_db(app, {
        let id = id.to_string();
        move |db| db.get_task(&id)
    })
    .await?
    .ok_or_else(|| AppError::NoSuchTask(id.to_string()))?;
    if !task.status.is_final() {
        return Err(AppError::Transfer("任务尚未结束，无法重试".into()));
    }

    let offset = if task.status == TaskStatus::Done {
        0
    } else {
        // 以目标端实际字节数为准：库里的 `done_bytes` 可能因进程退出而落后
        resume_offset(&*backend, &task).await.unwrap_or(0)
    };

    let mut task = task;
    task.status = TaskStatus::Pending;
    task.done_bytes = offset;
    task.error = None;
    task.updated_at = now_ms();
    with_db(app, {
        let row = task.clone();
        move |db| {
            db.touch_task(
                &row.id,
                TaskStatus::Pending,
                row.done_bytes,
                row.total_bytes,
                None,
            )
        }
    })
    .await?;

    launch(app, backend, &task, offset);
    Ok(task)
}

/// 登记活动状态、广播 `pending`、拉起 worker 协程。
fn launch(app: &AppHandle, backend: Arc<dyn FileTransfer>, task: &TaskRow, offset: u64) {
    let st = app.state::<AppState>();
    let active = st.transfers.register(task);
    active.done.store(offset, Ordering::Relaxed);
    active.total.store(task.total_bytes, Ordering::Relaxed);
    emit_state(
        app,
        &task.id,
        TaskStatus::Pending,
        offset,
        task.total_bytes,
        None,
    );
    tokio::spawn(run(app.clone(), backend, task.clone(), offset));
}

/// worker：等并发许可 → 分块传输 → 失败退避续传 → 落终态。
async fn run(app: AppHandle, backend: Arc<dyn FileTransfer>, task: TaskRow, base_offset: u64) {
    let (active, gate) = {
        let st = app.state::<AppState>();
        (st.transfers.peek(&task.id), st.transfers.gate())
    };
    let Some(active) = active else { return };

    // 排队（并发上限）；闸门关闭只发生在应用退出中，直接放弃
    let Ok(_permit) = gate.acquire_owned().await else {
        return close(
            &app,
            &task,
            &active,
            TaskStatus::Failed,
            Some("传输队列已关闭"),
        )
        .await;
    };
    if active.cancel.load(Ordering::Relaxed) {
        return close(&app, &task, &active, TaskStatus::Canceled, None).await;
    }

    set_status(&app, &task.id, TaskStatus::Running, None).await;

    let mut offset = base_offset;
    let mut last_err: Option<String> = None;
    for attempt in 0..=RETRIES {
        if attempt > 0 {
            // 指数退避后按目标端实际字节续传，不重头开始
            tokio::time::sleep(Duration::from_secs(u64::from(1u32 << (attempt - 1)))).await;
            if active.cancel.load(Ordering::Relaxed) {
                break;
            }
            offset = resume_offset(&*backend, &task).await.unwrap_or(0);
        }
        active.done.store(offset, Ordering::Relaxed);

        let progress = progress_cb(&app, &task.id, active.clone(), offset);
        let local = PathBuf::from(&task.local_path);
        let result = match task.direction {
            Direction::Download => {
                backend
                    .download(&task.remote_path, &local, offset, &active.cancel, &progress)
                    .await
            }
            Direction::Upload => {
                backend
                    .upload(&local, &task.remote_path, offset, &active.cancel, &progress)
                    .await
            }
        };
        match result {
            Ok(_) => return close(&app, &task, &active, TaskStatus::Done, None).await,
            Err(e) => {
                if active.cancel.load(Ordering::Relaxed) {
                    break;
                }
                last_err = Some(e.to_string());
            }
        }
    }

    let status = if active.cancel.load(Ordering::Relaxed) {
        TaskStatus::Canceled
    } else {
        TaskStatus::Failed
    };
    close(&app, &task, &active, status, last_err.as_deref()).await;
}

/// 收尾：落库 + 广播终态 + 摘除活动表。成功时把进度补齐到 100%。
async fn close(
    app: &AppHandle,
    task: &TaskRow,
    active: &Active,
    status: TaskStatus,
    error: Option<&str>,
) {
    let transferred = active.done.load(Ordering::Relaxed);
    let done = if status == TaskStatus::Done {
        task.total_bytes.max(transferred)
    } else {
        transferred
    };
    let id = task.id.clone();
    let total = task.total_bytes;
    let owned_err = error.map(str::to_string);
    let _ = with_db(app, move |db| {
        db.touch_task(&id, status, done, total, owned_err.as_deref())
    })
    .await;
    emit_state(app, &task.id, status, done, task.total_bytes, error);
    app.state::<AppState>().transfers.forget(&task.id);
}

/// 续传起点：下载看本地已有字节，上传看远端已有字节。
async fn resume_offset(backend: &dyn FileTransfer, task: &TaskRow) -> Result<u64, AppError> {
    let offset = match task.direction {
        Direction::Download => tokio::fs::metadata(&task.local_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0),
        Direction::Upload => backend
            .stat(&task.remote_path)
            .await?
            .map(|e| e.size)
            .unwrap_or(0),
    };
    // 目标比源还大说明状态不一致，退回重传更安全
    Ok(match task.total_bytes {
        0 => offset,
        total => offset.min(total),
    })
}

/// 目标端（上传=远端，下载=本地）已有字节数；不存在返回 `None`。
async fn target_size(
    backend: &dyn FileTransfer,
    direction: Direction,
    remote: &str,
    local: &Path,
) -> Result<Option<u64>, AppError> {
    match direction {
        Direction::Upload => Ok(backend.stat(remote).await?.map(|e| e.size)),
        Direction::Download => match tokio::fs::metadata(local).await {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        },
    }
}

/// 按名字拼出这对目标路径。
fn target_paths(req: &Request, name: &str) -> (String, PathBuf) {
    match req.direction {
        Direction::Upload => (join(&req.dst_dir, name), PathBuf::from(&req.src)),
        Direction::Download => (req.src.clone(), PathBuf::from(&req.dst_dir).join(name)),
    }
}

/// `Rename` 策略：从 `a (1).txt` 起递增探测，取第一个空位。
async fn pick_free_name(
    backend: &dyn FileTransfer,
    req: &Request,
    name: &str,
) -> Result<String, AppError> {
    for seq in 1..=RENAME_TRIES {
        let candidate = dedup_name(name, seq);
        let (remote, local) = target_paths(req, &candidate);
        if target_size(backend, req.direction, &remote, &local)
            .await?
            .is_none()
        {
            return Ok(candidate);
        }
    }
    Err(AppError::Transfer(format!(
        "改名 {RENAME_TRIES} 次仍然冲突，请手动指定目标名"
    )))
}

/// 进度回调：累加计数 + 节流广播（含实时速度）。
fn progress_cb(app: &AppHandle, task_id: &str, active: Arc<Active>, base: u64) -> Progress {
    let app = app.clone();
    let id = task_id.to_string();
    let last = Arc::new(AtomicI64::new(0));
    let started = Instant::now();
    Arc::new(move |n| {
        let done = active.done.fetch_add(n, Ordering::Relaxed) + n;
        let now = now_ms();
        let prev = last.load(Ordering::Relaxed);
        if now.saturating_sub(prev) < PROGRESS_INTERVAL_MS {
            return;
        }
        // CAS 抢发送权：并发分块回调可能同时越过节流窗口
        if last
            .compare_exchange(prev, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let elapsed = started.elapsed().as_secs_f64().max(0.001);
        let _ = app.emit(
            EVENT_PROGRESS,
            ProgressPayload {
                task_id: &id,
                transferred: done,
                total: active.total.load(Ordering::Relaxed),
                speed: ((done.saturating_sub(base)) as f64 / elapsed) as u64,
            },
        );
    })
}

async fn set_status(app: &AppHandle, id: &str, status: TaskStatus, error: Option<&str>) {
    let owned = id.to_string();
    let error_owned = error.map(str::to_string);
    let _ = with_db(app, move |db| {
        db.set_task_status(&owned, status, error_owned.as_deref())
    })
    .await;
    let st = app.state::<AppState>();
    if let Some(a) = st.transfers.peek(id) {
        emit_state(
            app,
            id,
            status,
            a.done.load(Ordering::Relaxed),
            a.total.load(Ordering::Relaxed),
            error,
        );
    }
}

fn emit_state(
    app: &AppHandle,
    task_id: &str,
    status: TaskStatus,
    transferred: u64,
    total: u64,
    error: Option<&str>,
) {
    let _ = app.emit(
        EVENT_STATE,
        StatePayload {
            task_id,
            status,
            transferred,
            total,
            error,
        },
    );
}

/// 在阻塞线程上碰数据库：SQLite 是同步 IO，不能在 async 执行器上直接跑。
pub(crate) async fn with_db<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce(&Db) -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    let app = app.clone();
    match tokio::task::spawn_blocking(move || f(&app.state::<AppState>().db)).await {
        Ok(result) => result,
        Err(e) => Err(AppError::Internal(format!("后台任务异常终止：{e}"))),
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct ProgressPayload<'a> {
    task_id: &'a str,
    transferred: u64,
    total: u64,
    /// 字节/秒（本次运行的平均值，不含续传前已传部分）。
    speed: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct StatePayload<'a> {
    task_id: &'a str,
    status: TaskStatus,
    transferred: u64,
    total: u64,
    error: Option<&'a str>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct PhasePayload<'a> {
    task_id: &'a str,
    phase: &'a str,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct FallbackPayload<'a> {
    task_id: &'a str,
    reason: &'a str,
}

/// 广播打包阶段（`compress`/`transfer`/`extract`）。
fn emit_phase(app: &AppHandle, task_id: &str, phase: &str) {
    let _ = app.emit(
        EVENT_PHASE,
        PhasePayload { task_id, phase },
    );
}

/// 落库阶段（与 `emit_phase` 成对：事件驱动实时 UI，落库保证 refresh/重载后仍显对）。
async fn persist_phase(app: &AppHandle, task_id: &str, phase: &str) {
    let id = task_id.to_string();
    let p = phase.to_string();
    let _ = with_db(app, move |db| db.set_task_phase(&id, &p)).await;
}

/// 广播打包失败回退（携带失败原因，前端弹 info toast「已改逐文件传输 · {reason}」）。
fn emit_fallback(app: &AppHandle, task_id: &str, reason: &str) {
    let _ = app.emit(
        EVENT_FALLBACK,
        FallbackPayload { task_id, reason },
    );
}

/// 本地目录树 → 逐文件入队（非打包路径与打包失败回退**共用**，保证两条路径口径一致）。
///
/// `dst_root` 为远端基准目录、`folder` 为在其下新建的同名文件夹（空则直接摊到 `dst_root`）；
/// `files` 为 `(绝对路径, 相对目录根)`，由 [`crate::commands::transfer`] 的 `walk_local_files` 产出。
pub async fn upload_dir_files(
    app: &AppHandle,
    ft: &Arc<dyn FileTransfer>,
    session_id: &str,
    files: &[(PathBuf, PathBuf)],
    dst_root: &str,
    folder: &str,
    conflict: OnConflict,
) -> Result<u32, AppError> {
    let root = if folder.is_empty() {
        dst_root.to_string()
    } else {
        let r = join(dst_root, folder);
        let _ = ft.mkdir(&r).await;
        r
    };
    let mut n = 0u32;
    for (local, rel) in files {
        let rel_parent = rel.parent().map(|p| p.to_string_lossy().into_owned());
        let mut dst_dir = root.clone();
        if let Some(sub) = rel_parent.filter(|s| !s.is_empty() && s != ".") {
            // 逐级 mkdir -p
            let mut acc = root.clone();
            for part in sub.replace('\\', "/").split('/') {
                if part.is_empty() || part == "." {
                    continue;
                }
                acc = join(&acc, part);
                let _ = ft.mkdir(&acc).await;
            }
            dst_dir = acc;
        }
        let md = tokio::fs::metadata(local)
            .await
            .map_err(|e| AppError::Io(format!("{}: {e}", local.display())))?;
        submit(
            app,
            ft.clone(),
            Request {
                session_id: session_id.to_string(),
                direction: Direction::Upload,
                src: local.to_string_lossy().into_owned(),
                dst_dir,
                total_bytes: md.len(),
                on_conflict: conflict,
            },
        )
        .await?;
        n += 1;
    }
    Ok(n)
}

/// 打包上传一个本地目录：`compress → upload tgz → extract → cleanup`。
///
/// 由命令层 `run_upload_dir` 判定命中打包后 `tokio::spawn` 拉起。中段（压缩/上传/解压）任一步
/// 失败 → 删远端与本地临时包 + 删 bundle 行 + 整体回退逐文件（emit `bundle-fallback`）；
/// 用户取消 → 只清临时包与远端 tgz、行落 `canceled`、**不回退**（取消是用户意图）。
/// 预探测（远端 Windows / 无 uname）发生在建行之前，静默逐文件、不发回退事件。
pub async fn run_bundle(
    app: AppHandle,
    ft: Arc<dyn FileTransfer>,
    session_id: String,
    local_dir: String,
    dst_root: String,
    folder: String,
    files: Vec<(PathBuf, PathBuf)>,
    conflict: OnConflict,
) {
    let st = app.state::<AppState>();
    // 与其它传输共用并发闸门：先取许可再探测远端，避免占着许可空跑 exec 往返。
    let Ok(_permit) = st.transfers.gate().acquire_owned().await else {
        return;
    };

    // 预探测：Windows / 拿不到 uname → 逐文件（打包从未真正开始，不发 fallback）。
    if bundle::remote_is_windows(&app, &session_id).await {
        let _ =
            upload_dir_files(&app, &ft, &session_id, &files, &dst_root, &folder, conflict).await;
        return;
    }

    let id = new_id();
    let profile_id = app.state::<AppState>().sessions.profile_id(&session_id);
    let remote_tgz = join(&dst_root, &bundle::remote_tgz_name(&id));
    let tgz_local = std::env::temp_dir().join(format!("sailterm_{id}.tgz"));
    let tgz_local_str = tgz_local.to_string_lossy().into_owned();

    let task = TaskRow {
        id: id.clone(),
        session_id: session_id.clone(),
        profile_id,
        direction: Direction::Upload,
        remote_path: remote_tgz.clone(),
        local_path: local_dir.clone(),
        // 打包行 total 先填 0（compress 阶段不定长），压完拿到 tgz 大小再回填 → 进度条诚实 0..100%。
        total_bytes: 0,
        done_bytes: 0,
        status: TaskStatus::Pending,
        error: None,
        created_at: now_ms(),
        updated_at: now_ms(),
        phase: PHASE_COMPRESS.to_string(),
    };
    let _ = with_db(&app, { let row = task.clone(); move |db| db.insert_task(&row) }).await;
    let active = st.transfers.register(&task);
    emit_state(&app, &id, TaskStatus::Pending, 0, 0, None);
    emit_phase(&app, &id, PHASE_COMPRESS);
    set_status(&app, &id, TaskStatus::Running, None).await;

    // 1) 本机 tar
    if let Err(e) = bundle::compress_dir(local_dir.clone(), tgz_local_str.clone()).await {
        if active.cancel.load(Ordering::Relaxed) {
            return cancel_bundle(&app, &ft, &task, &active, &remote_tgz, &tgz_local).await;
        }
        return fallback_bundle(&app, &ft, &session_id, &id, &remote_tgz, &tgz_local, &files, &dst_root, &folder, conflict, e.to_string()).await;
    }
    if active.cancel.load(Ordering::Relaxed) {
        return cancel_bundle(&app, &ft, &task, &active, &remote_tgz, &tgz_local).await;
    }

    // 压缩完成：回填真实字节、进入 transfer 阶段。
    let tgz_size = tokio::fs::metadata(&tgz_local).await.map(|m| m.len()).unwrap_or(0);
    active.total.store(tgz_size, Ordering::Relaxed);
    {
        let id2 = id.clone();
        let _ = with_db(&app, move |db| db.touch_task(&id2, TaskStatus::Running, 0, tgz_size, None)).await;
    }
    emit_phase(&app, &id, PHASE_TRANSFER);
    persist_phase(&app, &id, PHASE_TRANSFER).await;

    // 2) 上传单个 tgz（复用 SFTP 通道 + 同款进度事件，taskId=bundle 行 id）。
    let progress = progress_cb(&app, &id, active.clone(), 0);
    if let Err(e) = ft.upload(&tgz_local, &remote_tgz, 0, &active.cancel, &progress).await {
        if active.cancel.load(Ordering::Relaxed) {
            return cancel_bundle(&app, &ft, &task, &active, &remote_tgz, &tgz_local).await;
        }
        return fallback_bundle(&app, &ft, &session_id, &id, &remote_tgz, &tgz_local, &files, &dst_root, &folder, conflict, format!("上传失败：{e}")).await;
    }
    if active.cancel.load(Ordering::Relaxed) {
        return cancel_bundle(&app, &ft, &task, &active, &remote_tgz, &tgz_local).await;
    }

    // 3) 远端解包
    emit_phase(&app, &id, PHASE_EXTRACT);
    persist_phase(&app, &id, PHASE_EXTRACT).await;
    if let Err(e) = bundle::extract_remote(&app, &session_id, &remote_tgz, &dst_root).await {
        return fallback_bundle(&app, &ft, &session_id, &id, &remote_tgz, &tgz_local, &files, &dst_root, &folder, conflict, e.to_string()).await;
    }

    // 4) 清理 + 落 done
    let _ = ft.remove(&remote_tgz, false).await;
    let _ = tokio::fs::remove_file(&tgz_local).await;
    cache::global().invalidate(&session_id, &dst_root);
    let done_task = TaskRow { total_bytes: tgz_size, ..task.clone() };
    close(&app, &done_task, &active, TaskStatus::Done, None).await;
}

/// 打包失败整体回退：清临时包 → 删 bundle 行 → 逐文件重入队 → 通知前端。
#[allow(clippy::too_many_arguments)]
async fn fallback_bundle(
    app: &AppHandle,
    ft: &Arc<dyn FileTransfer>,
    session_id: &str,
    id: &str,
    remote_tgz: &str,
    tgz_local: &Path,
    files: &[(PathBuf, PathBuf)],
    dst_root: &str,
    folder: &str,
    conflict: OnConflict,
    reason: String,
) {
    let _ = ft.remove(remote_tgz, false).await; // best-effort：远端可能根本没建成
    let _ = tokio::fs::remove_file(tgz_local).await;
    let id_owned = id.to_string();
    let _ = with_db(app, move |db| db.delete_task(&id_owned)).await;
    app.state::<AppState>().transfers.forget(id);
    // 先重入队逐文件（新行先于回退事件存在，前端 drop+refresh 拉回来不扑空），
    // 整包从未落地 → 逐文件从头重传（不续传打包中途）——与用户确认的降级口径一致。
    let _ = upload_dir_files(app, ft, session_id, files, dst_root, folder, conflict).await;
    emit_fallback(app, id, &reason);
}

/// 用户取消打包：清临时包 + 删远端 tgz，行落 `canceled`，不回退。
async fn cancel_bundle(
    app: &AppHandle,
    ft: &Arc<dyn FileTransfer>,
    task: &TaskRow,
    active: &Arc<Active>,
    remote_tgz: &str,
    tgz_local: &Path,
) {
    let _ = ft.remove(remote_tgz, false).await;
    let _ = tokio::fs::remove_file(tgz_local).await;
    close(app, task, active, TaskStatus::Canceled, None).await;
}
