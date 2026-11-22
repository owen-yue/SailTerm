//! SQLite 存储层：连接管理 + 版本化迁移。
//!
//! 桌面端并发不高，单 `Connection` + `Mutex` 即可；`Mutex` 只在一次 SQL 期间持有，
//! **绝不跨 `await`**。所有 SQL 走参数化查询。

pub mod ai;
pub mod audit;
pub mod ftps_cert;
pub mod history;
pub mod host_key;
pub mod memory;
pub mod plugin_settings;
pub mod policy_rule;
pub mod profile;
pub mod record_index;
pub mod settings;
pub mod snippet;
pub mod ssh_key;
pub mod transfer;
pub mod trigger;
pub mod tunnel;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use crate::error::AppError;

/// 迁移脚本：版本号 + SQL（幂等，可重复执行）。新增迁移即追加一项，勿改已发布脚本。
const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("migrations/001_init.sql"))];

/// 当前 schema 版本（= `MIGRATIONS` 中最大的版本号）。
pub const SCHEMA_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].0;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// 打开（必要时创建）数据库文件并升级到最新 schema。
    pub fn open(path: &Path) -> Result<Self, AppError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        migrate(&conn)?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        // 迁移后校验：版本没升到预期值说明脚本注册表与实库不一致，宁可不启动。
        // 版本号高于当前注册表属旧版开发库的历史迁移链，schema 只会更新不会缺，放行。
        let version = db.schema_version()?;
        if version < SCHEMA_VERSION {
            return Err(AppError::Db(format!(
                "schema 版本异常：至少应为 {SCHEMA_VERSION}，实际 {version}"
            )));
        }
        // 上次进程退出时残留的“进行中”传输任务已无人推进，一律收尾为失败
        db.reap_stale_tasks()?;
        Ok(db)
    }

    /// 在连接上执行一段读写；闭包内禁止 `await`。
    pub(crate) fn with<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        f(&self.conn.lock().unwrap())
    }

    /// 当前 schema 版本。
    pub fn schema_version(&self) -> Result<i64, AppError> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |r| r.get(0),
            )?)
        })
    }
}

/// unix 毫秒时间戳。
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn migrate(conn: &Connection) -> Result<(), AppError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version     INTEGER PRIMARY KEY,
            applied_at  INTEGER NOT NULL,
            checksum    TEXT
        );",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |r| r.get(0),
    )?;

    for (version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
        // 迁移失败即中止：rusqlite 逐句执行，未完成的 DDL 由幂等脚本在下次启动补齐。
        conn.execute_batch(sql)?;
        conn.execute(
            "INSERT INTO schema_version(version, applied_at, checksum) VALUES(?1, ?2, ?3)",
            params![version, now_ms(), checksum(sql)],
        )?;
    }
    Ok(())
}

/// 脚本校验和：防止迁移文件被意外改动。
fn checksum(sql: &str) -> String {
    let mut h = DefaultHasher::new();
    sql.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_migrates_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sailterm.db");
        let db = Db::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), SCHEMA_VERSION);
        // 重复打开不重复迁移，且不报错
        assert_eq!(
            Db::open(&path).unwrap().schema_version().unwrap(),
            SCHEMA_VERSION
        );
        assert!(path.exists());
    }
}
