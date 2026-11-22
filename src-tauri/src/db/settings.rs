//! 通用键值设置存储（`settings` 表）。
//!
//! 表结构：`key TEXT PK` / `value TEXT`（JSON 或标量字符串）/ `updated_at`。此层只做**原样读写**，
//! 不认识具体键的语义；类型化视图（`AiSettings`：出网开关 / 审批模式 / 扫描层级）与键常量在命令层
//! [`crate::commands::settings`]，那里负责字符串 ↔ 枚举/布尔的解析与默认值。
//!
//! 安全纪律：设置项**只存非敏感偏好**（开关、层级、模式），
//! **绝不落凭证/API Key**——凭证一律走 Vault。故本层无需脱敏，也不得被用来存明文密钥。

use rusqlite::{params, OptionalExtension};

use super::{now_ms, Db};
use crate::error::AppError;

impl Db {
    /// 取一个设置值；键不存在返回 `None`（调用方据此落默认值）。
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, AppError> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
    }

    /// 写入（UPSERT）一个设置值，并刷新 `updated_at`。空键忽略（防御性，正常调用方不会传空）。
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), AppError> {
        if key.is_empty() {
            return Ok(());
        }
        self.with(|c| {
            c.execute(
                "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![key, value, now_ms()],
            )?;
            Ok(())
        })
    }

    /// 删除一个设置键（恢复该键的默认值），返回是否删掉了一行。消费方：commands/settings.rs 的「恢复默认」。
    pub fn delete_setting(&self, key: &str) -> Result<bool, AppError> {
        self.with(|c| Ok(c.execute("DELETE FROM settings WHERE key = ?1", params![key])? > 0))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn absent_key_is_none_and_empty_key_ignored() {
        let db = db();
        assert_eq!(db.get_setting("nope").unwrap(), None);
        // 空键写入被忽略，不产生行
        db.set_setting("", "x").unwrap();
        assert_eq!(db.get_setting("").unwrap(), None);
    }

    #[test]
    fn set_get_upsert_delete_roundtrip() {
        let db = db();
        db.set_setting("ai.network_enabled", "true").unwrap();
        assert_eq!(
            db.get_setting("ai.network_enabled").unwrap().as_deref(),
            Some("true")
        );

        // UPSERT 覆盖同键，不新增行
        db.set_setting("ai.network_enabled", "false").unwrap();
        assert_eq!(
            db.get_setting("ai.network_enabled").unwrap().as_deref(),
            Some("false")
        );

        // 多键互不干扰
        db.set_setting("approval.mode", "all").unwrap();
        assert_eq!(
            db.get_setting("approval.mode").unwrap().as_deref(),
            Some("all")
        );
        assert_eq!(
            db.get_setting("ai.network_enabled").unwrap().as_deref(),
            Some("false")
        );

        // 删除后回到 None；删不存在的键返回 false
        assert!(db.delete_setting("approval.mode").unwrap());
        assert_eq!(db.get_setting("approval.mode").unwrap(), None);
        assert!(!db.delete_setting("approval.mode").unwrap());
    }
}
