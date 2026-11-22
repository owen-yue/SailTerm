//! 会话录制索引：录制文件绝对路径 → 主机名。
//!
//! 录制文件按各档案配置的目录散落，全局回放库需跨目录聚合并标明每条来自哪台服务器。
//! 文件名内嵌主机只对默认命名可靠，故起录时把真实主机名按绝对路径登记一次（见 [`super::super::commands::record`]）。

use rusqlite::params;

use super::{now_ms, Db};
use crate::error::AppError;

/// 一条录制索引（路径 + 主机）。
pub struct RecordedFile {
    pub path: String,
    pub host: String,
}

impl Db {
    /// 登记（或覆盖）一条录制文件的主机归属。
    pub fn record_index_put(&self, path: &str, host: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO record_index (path, host, created_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(path) DO UPDATE SET host = excluded.host",
                params![path, host, now_ms()],
            )?;
            Ok(())
        })
    }

    /// 列出全部索引（路径 + 主机）。全局回放库据此把散落各目录的文件标注来源服务器。
    pub fn record_index_list(&self) -> Result<Vec<RecordedFile>, AppError> {
        self.with(|c| {
            let mut st = c.prepare("SELECT path, host FROM record_index")?;
            let rows = st
                .query_map([], |r| {
                    Ok(RecordedFile {
                        path: r.get(0)?,
                        host: r.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 删除一条索引（文件被删时同步清理，避免残留）。
    pub fn record_index_forget(&self, path: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute("DELETE FROM record_index WHERE path = ?1", params![path])?;
            Ok(())
        })
    }

    /// 清理索引里文件已不存在的行（外部删除/移动后收敛），返回删除条数。
    pub fn record_index_prune(&self, existing: &[String]) -> Result<usize, AppError> {
        self.with(|c| {
            let all: Vec<String> = c
                .prepare("SELECT path FROM record_index")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let keep: std::collections::HashSet<&str> =
                existing.iter().map(String::as_str).collect();
            let mut n = 0usize;
            for p in all.iter().filter(|p| !keep.contains(p.as_str())) {
                n += c.execute("DELETE FROM record_index WHERE path = ?1", params![p])?;
            }
            Ok(n)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn put_list_forget_prune() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        db.record_index_put("/a/x.log", "srv1").unwrap();
        db.record_index_put("/b/y.log", "srv2").unwrap();
        assert_eq!(db.record_index_list().unwrap().len(), 2);

        // 覆盖同路径只改主机，不新增行
        db.record_index_put("/a/x.log", "srv1b").unwrap();
        assert_eq!(db.record_index_list().unwrap().len(), 2);

        db.record_index_forget("/b/y.log").unwrap();
        assert_eq!(db.record_index_list().unwrap().len(), 1);

        // 只保留 /a/x.log，其它（含已消失的）被清
        db.record_index_put("/c/gone.log", "srv3").unwrap();
        let pruned = db.record_index_prune(&["/a/x.log".to_string()]).unwrap();
        assert_eq!(pruned, 1);
        assert_eq!(db.record_index_list().unwrap().len(), 1);
    }
}
