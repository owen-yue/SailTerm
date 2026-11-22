//! 命令历史：入库前一律经 [`redact_command`](crate::sanitize::redact_command) 脱敏。

use rusqlite::params;
use serde::Serialize;

use super::{new_id, now_ms, Db};
use crate::error::AppError;
use crate::sanitize::redact_command;

/// 一条命令历史（回前端）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub session_id: Option<String>,
    pub profile_id: Option<String>,
    pub command: String,
    pub source: String,
    pub created_at: i64,
}

/// 每个连接保留的历史条数上限。
const MAX_PER_PROFILE: i64 = 1000;

impl Db {
    /// 记录用户/AI 执行的命令（`source`: user|ai）。空命令忽略；超限自动裁剪最旧记录。
    pub fn insert_command(
        &self,
        session_id: Option<&str>,
        profile_id: Option<&str>,
        command: &str,
        source: &str,
    ) -> Result<(), AppError> {
        let command = redact_command(command);
        if command.trim().is_empty() {
            return Ok(());
        }
        self.with(|c| {
            c.execute(
                "INSERT INTO command_history (id, session_id, profile_id, command, source, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![new_id(), session_id, profile_id, command, source, now_ms()],
            )?;
            if let Some(profile_id) = profile_id {
                c.execute(
                    "DELETE FROM command_history WHERE profile_id = ?1 AND id NOT IN (
                        SELECT id FROM command_history WHERE profile_id = ?1
                        ORDER BY created_at DESC, rowid DESC LIMIT ?2)",
                    params![profile_id, MAX_PER_PROFILE],
                )?;
            }
            Ok(())
        })
    }

    /// 按连接（或全局）取最近命令，`limit` 上限 500。
    pub fn list_commands(
        &self,
        profile_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<HistoryEntry>, AppError> {
        let limit = limit.clamp(1, 500);
        self.with(|c| {
            let (sql, use_profile) = match profile_id {
                Some(_) => (
                    "SELECT id, session_id, profile_id, command, source, created_at FROM command_history
                     WHERE profile_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2",
                    true,
                ),
                None => (
                    "SELECT id, session_id, profile_id, command, source, created_at FROM command_history
                     ORDER BY created_at DESC, rowid DESC LIMIT ?1",
                    false,
                ),
            };
            let mut st = c.prepare(sql)?;
            let map = |r: &rusqlite::Row<'_>| -> rusqlite::Result<HistoryEntry> {
                Ok(HistoryEntry {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    profile_id: r.get(2)?,
                    command: r.get(3)?,
                    source: r.get(4)?,
                    created_at: r.get(5)?,
                })
            };
            let rows = if use_profile {
                st.query_map(params![profile_id, limit], map)?.collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                st.query_map(params![limit], map)?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            Ok(rows)
        })
    }

    /// 删除单条命令历史（按主键 id），返回删除条数（0 或 1）。
    pub fn delete_command(&self, id: &str) -> Result<usize, AppError> {
        self.with(|c| Ok(c.execute("DELETE FROM command_history WHERE id = ?1", params![id])?))
    }

    /// 清空某连接（或全部）的命令历史。
    pub fn clear_commands(&self, profile_id: Option<&str>) -> Result<usize, AppError> {
        self.with(|c| {
            Ok(match profile_id {
                Some(id) => c.execute(
                    "DELETE FROM command_history WHERE profile_id = ?1",
                    params![id],
                )?,
                None => c.execute("DELETE FROM command_history", [])?,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn records_redacted_and_lists_recent_first() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        db.insert_command(Some("s1"), Some("p1"), "ls -la", "user")
            .unwrap();
        db.insert_command(Some("s1"), Some("p1"), "curl -u admin:pw https://x", "user")
            .unwrap();
        db.insert_command(Some("s1"), Some("p1"), "   ", "user")
            .unwrap(); // 空命令忽略

        let rows = db.list_commands(Some("p1"), 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].command, "curl -u *** https://x");
        assert_eq!(rows[1].command, "ls -la");
        assert!(!db.list_commands(None, 10).unwrap().is_empty());

        assert_eq!(db.clear_commands(Some("p1")).unwrap(), 2);
        assert!(db.list_commands(Some("p1"), 10).unwrap().is_empty());
    }

    #[test]
    fn delete_single_command_by_id() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        db.insert_command(Some("s1"), Some("p1"), "ls -la", "user")
            .unwrap();
        db.insert_command(Some("s1"), Some("p1"), "df -h", "user")
            .unwrap();
        let rows = db.list_commands(Some("p1"), 10).unwrap();
        let target = rows.iter().find(|r| r.command == "ls -la").unwrap().id.clone();
        assert_eq!(db.delete_command(&target).unwrap(), 1);
        let left = db.list_commands(Some("p1"), 10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].command, "df -h");
        // 不存在的 id：幂等返回 0，不报错
        assert_eq!(db.delete_command(&target).unwrap(), 0);
    }
}
