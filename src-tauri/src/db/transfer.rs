//! 传输任务持久化：进度落库支撑断点续传，历史行支撑审计回看。
//!
//! 表里只有路径与字节数，**不含任何凭证**。

use rusqlite::params;
use serde::Serialize;

use super::{now_ms, Db};
use crate::error::AppError;

/// 每个会话保留的已结束任务上限（防止长时间使用把表撑爆，对应 的保留策略）。
const MAX_FINISHED: i64 = 500;

// rusqlite 未给 `u64` 实现 `ToSql`/`FromSql`（SQLite 只有有符号 64 位整数），
// 字节数在 SQL 边界上转一道；文件大小不可能真碰到 `i64::MAX`。
fn to_sql_bytes(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn from_sql_bytes(v: i64) -> u64 {
    v.max(0) as u64
}

/// 传输方向。
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    Upload,
    Download,
}

impl Direction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Download => "download",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "upload" => Some(Self::Upload),
            "download" => Some(Self::Download),
            _ => None,
        }
    }
}

/// 任务状态机：`pending → running → done | failed | canceled`。
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Pending,
    Running,
    Done,
    Failed,
    Canceled,
}

impl TaskStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            _ => return None,
        })
    }

    /// 终态：不会再变化，可被裁剪、可被重试。
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Canceled)
    }
}

/// 一条传输任务（同时是回前端的 DTO）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TaskRow {
    pub id: String,
    pub session_id: String,
    /// 归属的连接配置（profile）id；旧数据为 NULL，删除服务器时按此级联清理。
    pub profile_id: Option<String>,
    pub direction: Direction,
    pub remote_path: String,
    pub local_path: String,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub status: TaskStatus,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// 打包传输阶段：`compress` / `transfer` / `extract`；逐文件模式恒 `transfer`。
    pub phase: String,
}

/// 打包传输的三个阶段（[`TaskRow::phase`] 的取值域）；逐文件恒 [`PHASE_TRANSFER`]。
pub const PHASE_COMPRESS: &str = "compress";
pub const PHASE_TRANSFER: &str = "transfer";
pub const PHASE_EXTRACT: &str = "extract";

const COLS: &str = "id, session_id, direction, remote_path, local_path, total_bytes, \
                    done_bytes, status, error, created_at, updated_at, profile_id, phase";

impl Db {
    /// 新建任务行，并顺手裁剪该会话超限的历史任务。
    pub fn insert_task(&self, t: &TaskRow) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO transfer_tasks
                    (id, session_id, direction, remote_path, local_path,
                     total_bytes, done_bytes, status, error, created_at, updated_at, profile_id, phase)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                params![
                    t.id,
                    t.session_id,
                    t.direction.as_str(),
                    t.remote_path,
                    t.local_path,
                    to_sql_bytes(t.total_bytes),
                    to_sql_bytes(t.done_bytes),
                    t.status.as_str(),
                    t.error,
                    t.created_at,
                    t.updated_at,
                    t.profile_id,
                    t.phase,
                ],
            )?;
            // 只裁剪已结束的：排队/进行中的任务绝不能被误删
            c.execute(
                "DELETE FROM transfer_tasks WHERE session_id = ?1
                   AND status IN ('done','failed','canceled')
                   AND id NOT IN (
                       SELECT id FROM transfer_tasks WHERE session_id = ?1
                         AND status IN ('done','failed','canceled')
                       ORDER BY created_at DESC, rowid DESC LIMIT ?2)",
                params![t.session_id, MAX_FINISHED],
            )?;
            Ok(())
        })
    }

    /// 刷新进度/状态。`done`、`total` 每次都写全量值（不做增量累加，避免并发丢更新）。
    pub fn touch_task(
        &self,
        id: &str,
        status: TaskStatus,
        done: u64,
        total: u64,
        error: Option<&str>,
    ) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "UPDATE transfer_tasks
                    SET status = ?1, done_bytes = ?2, total_bytes = ?3, error = ?4, updated_at = ?5
                  WHERE id = ?6",
                params![
                    status.as_str(),
                    to_sql_bytes(done),
                    to_sql_bytes(total),
                    error,
                    now_ms(),
                    id
                ],
            )?;
            Ok(())
        })
    }

    /// 只改状态与错误文案，不碰进度（状态迁移专用，避免读回再写全量）。
    pub fn set_task_status(
        &self,
        id: &str,
        status: TaskStatus,
        error: Option<&str>,
    ) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "UPDATE transfer_tasks SET status = ?1, error = ?2, updated_at = ?3 WHERE id = ?4",
                params![status.as_str(), error, now_ms(), id],
            )?;
            Ok(())
        })
    }

    /// 只改打包阶段（compress/transfer/extract）；逐文件任务不走这里。
    pub fn set_task_phase(&self, id: &str, phase: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "UPDATE transfer_tasks SET phase = ?1, updated_at = ?2 WHERE id = ?3",
                params![phase, now_ms(), id],
            )?;
            Ok(())
        })
    }

    pub fn get_task(&self, id: &str) -> Result<Option<TaskRow>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!("SELECT {COLS} FROM transfer_tasks WHERE id = ?1"))?;
            let mut rows = st.query_map(params![id], map_task)?;
            Ok(match rows.next() {
                Some(r) => Some(r?),
                None => None,
            })
        })
    }

    /// 按会话/服务器（或全局）取任务，最新在前。
    /// session 与 profile 同时给时取并集：同一台服务器重连后 session id 会变，
    /// 按 profile 归组才能看到本服务器的完整历史（跨重连）。
    pub fn list_tasks(
        &self,
        session_id: Option<&str>,
        profile_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<TaskRow>, AppError> {
        let limit = limit.clamp(1, 500);
        self.with(|c| {
            let (sql, has_session, has_profile) = match (session_id, profile_id) {
                (Some(_), Some(_)) => (
                    format!(
                        "SELECT {COLS} FROM transfer_tasks \
                             WHERE session_id = ?1 OR profile_id = ?2 \
                             ORDER BY created_at DESC, rowid DESC LIMIT ?3"
                    ),
                    true,
                    true,
                ),
                (Some(_), None) => (
                    format!(
                        "SELECT {COLS} FROM transfer_tasks WHERE session_id = ?1 \
                             ORDER BY created_at DESC, rowid DESC LIMIT ?2"
                    ),
                    true,
                    false,
                ),
                (None, Some(_)) => (
                    format!(
                        "SELECT {COLS} FROM transfer_tasks WHERE profile_id = ?1 \
                             ORDER BY created_at DESC, rowid DESC LIMIT ?2"
                    ),
                    false,
                    true,
                ),
                (None, None) => (
                    format!(
                        "SELECT {COLS} FROM transfer_tasks \
                             ORDER BY created_at DESC, rowid DESC LIMIT ?1"
                    ),
                    false,
                    false,
                ),
            };
            let mut st = c.prepare(&sql)?;
            let rows = match (has_session, has_profile) {
                (true, true) => st.query_map(params![session_id, profile_id, limit], map_task)?,
                (true, false) => st.query_map(params![session_id, limit], map_task)?,
                (false, true) => st.query_map(params![profile_id, limit], map_task)?,
                (false, false) => st.query_map(params![limit], map_task)?,
            }
            .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 清掉已结束的任务行；进行中的保留。作用域=当前会话：
    /// 传输面板只展示本次会话的任务，旧会话历史行不露出
    /// （重传它们会报「会话不存在或已关闭」），留在库里供审计，删服务器时级联清理。
    pub fn clear_tasks(&self, session_id: Option<&str>) -> Result<usize, AppError> {
        self.with(|c| {
            Ok(match session_id {
                Some(id) => c.execute(
                    "DELETE FROM transfer_tasks WHERE session_id = ?1
                       AND status IN ('done','failed','canceled')",
                    params![id],
                )?,
                None => c.execute(
                    "DELETE FROM transfer_tasks WHERE status IN ('done','failed','canceled')",
                    [],
                )?,
            })
        })
    }

    /// 删除单个任务行（打包任务失败回退逐文件时清掉那条瞬时 bundle 行，不留半成品记录）。
    pub fn delete_task(&self, id: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute("DELETE FROM transfer_tasks WHERE id = ?1", params![id])?;
            Ok(())
        })
    }

    /// 启动时收尾：上次进程退出时还挂在 `pending`/`running` 的任务，
    /// 其传输协程早已不在，一律标为 `failed`（前端可点重试续传）。
    pub fn reap_stale_tasks(&self) -> Result<usize, AppError> {
        self.with(|c| {
            Ok(c.execute(
                "UPDATE transfer_tasks SET status = 'failed', updated_at = ?1,
                        error = COALESCE(error, '应用已退出，传输中断')
                  WHERE status IN ('pending','running')",
                params![now_ms()],
            )?)
        })
    }

    /// 该 profile 下疑似崩溃泄漏的远端 bundle 临时包行（供下次连接补删，决策 D-b）。
    /// 判据：`upload` + `failed` + 未清理 + `remote_path` 命中 `.sailterm_bundle_*.tgz` 命名
    /// （普通传输的 remote_path 是真实目标路径，绝不会长这样）。返回 `(行id, 远端路径)`。
    pub fn pending_remote_cleanup(&self, profile_id: &str) -> Result<Vec<(String, String)>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, remote_path FROM transfer_tasks
                  WHERE profile_id = ?1 AND direction = 'upload' AND status = 'failed'
                    AND remote_cleaned = 0
                    AND remote_path LIKE '%/.sailterm_bundle_%.tgz'",
            )?;
            let rows = st
                .query_map(params![profile_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 标记某行的远端临时包已 best-effort 处理（成功或已尽力），防每次连接重复删。
    pub fn mark_remote_cleaned(&self, id: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "UPDATE transfer_tasks SET remote_cleaned = 1 WHERE id = ?1",
                params![id],
            )?;
            Ok(())
        })
    }
}

fn map_task(r: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    let direction: String = r.get(2)?;
    let status: String = r.get(7)?;
    let total_bytes: i64 = r.get(5)?;
    let done_bytes: i64 = r.get(6)?;
    Ok(TaskRow {
        id: r.get(0)?,
        session_id: r.get(1)?,
        // 库内取值受 CHECK 约束；万一被外部改动也不该让整表读不出来
        direction: Direction::parse(&direction).unwrap_or(Direction::Download),
        remote_path: r.get(3)?,
        local_path: r.get(4)?,
        total_bytes: from_sql_bytes(total_bytes),
        done_bytes: from_sql_bytes(done_bytes),
        status: TaskStatus::parse(&status).unwrap_or(TaskStatus::Failed),
        error: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        profile_id: r.get(11)?,
        phase: r.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn row(id: &str, session: &str, status: TaskStatus) -> TaskRow {
        TaskRow {
            id: id.to_string(),
            session_id: session.to_string(),
            profile_id: None,
            direction: Direction::Upload,
            remote_path: "/var/www/index.html".to_string(),
            local_path: "C:/tmp/index.html".to_string(),
            total_bytes: 1024,
            done_bytes: 0,
            status,
            error: None,
            created_at: now_ms(),
            updated_at: now_ms(),
            phase: PHASE_TRANSFER.to_string(),
        }
    }

    #[test]
    fn migrates_and_roundtrips_task() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        // v2 迁移已应用（transfer_tasks 可用）；用 >= 断言以免后续迁移再次打破本用例
        assert!(db.schema_version().unwrap() >= 1);

        db.insert_task(&row("t1", "s1", TaskStatus::Pending))
            .unwrap();
        db.touch_task("t1", TaskStatus::Running, 512, 1024, None)
            .unwrap();

        let t = db.get_task("t1").unwrap().unwrap();
        assert_eq!(t.direction, Direction::Upload);
        assert_eq!(t.status, TaskStatus::Running);
        assert_eq!(t.done_bytes, 512);
        assert!(t.error.is_none());
    }

    #[test]
    fn reaps_tasks_left_running_by_previous_exit() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        db.insert_task(&row("t1", "s1", TaskStatus::Running))
            .unwrap();
        db.insert_task(&row("t2", "s1", TaskStatus::Done)).unwrap();

        assert_eq!(db.reap_stale_tasks().unwrap(), 1);
        let t1 = db.get_task("t1").unwrap().unwrap();
        assert_eq!(t1.status, TaskStatus::Failed);
        assert!(t1.error.unwrap().contains("应用已退出"));
        // 已结束的不动
        assert_eq!(db.get_task("t2").unwrap().unwrap().status, TaskStatus::Done);
    }

    #[test]
    fn pending_remote_cleanup_targets_only_leaked_bundle_rows() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        // 泄漏的 bundle 行：upload + failed + 命中 .sailterm_bundle_*.tgz + 有 profile
        let mut leak = row("b1", "s1", TaskStatus::Failed);
        leak.profile_id = Some("p1".into());
        leak.remote_path = "/home/u/dst/.sailterm_bundle_deadbeef.tgz".into();
        db.insert_task(&leak).unwrap();
        // 干扰项：普通 failed 上传（真实目标路径）不应被选中
        let mut normal = row("n1", "s1", TaskStatus::Failed);
        normal.profile_id = Some("p1".into());
        db.insert_task(&normal).unwrap();

        let got = db.pending_remote_cleanup("p1").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "b1");
        assert!(got[0].1.ends_with(".sailterm_bundle_deadbeef.tgz"));

        // 标记后不再出现（幂等，防每次连接重复删）
        db.mark_remote_cleaned("b1").unwrap();
        assert!(db.pending_remote_cleanup("p1").unwrap().is_empty());
        // 别的 profile 查不到
        assert!(db.pending_remote_cleanup("p2").unwrap().is_empty());
    }

    #[test]
    fn clear_keeps_active_and_drops_finished() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        db.insert_task(&row("t1", "s1", TaskStatus::Running))
            .unwrap();
        db.insert_task(&row("t2", "s1", TaskStatus::Failed))
            .unwrap();
        // 旧会话的行不在本会话作用域内，不受牵连
        db.insert_task(&row("t3", "s0", TaskStatus::Done)).unwrap();

        assert_eq!(db.clear_tasks(Some("s1")).unwrap(), 1);
        assert!(db.get_task("t1").unwrap().is_some());
        assert!(db.get_task("t2").unwrap().is_none());
        assert!(db.get_task("t3").unwrap().is_some());
    }

    #[test]
    fn prunes_finished_beyond_cap_per_session() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        for i in 0..(MAX_FINISHED + 5) {
            let id = format!("t{i}");
            let mut r = row(&id, "s1", TaskStatus::Done);
            r.created_at = 1_000 + i;
            db.insert_task(&r).unwrap();
        }
        // 其它会话不受牵连
        db.insert_task(&row("other", "s2", TaskStatus::Done))
            .unwrap();

        let rows = db.list_tasks(Some("s1"), None, 500).unwrap();
        assert_eq!(rows.len() as i64, MAX_FINISHED);
        assert!(db.get_task("other").unwrap().is_some());
    }
}
