//! 插件设置：`(plugin_id, key)` 命名空间隔离存储。
//!
//! **凭证绝不进本表**：敏感项由能力网关写进保险库 `sail:plugin:<id>:*`。

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{now_ms, Db};
use crate::error::AppError;

/// 一条插件配置。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PluginSetting {
    pub key: String,
    pub value: String,
}

impl Db {
    /// 读某插件全部配置（按 key 排序，稳定）。
    pub fn plugin_settings(&self, plugin_id: &str) -> Result<Vec<PluginSetting>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT key, value FROM plugin_settings WHERE plugin_id=?1 ORDER BY key ASC",
            )?;
            let rows = st
                .query_map(params![plugin_id], |r| {
                    Ok(PluginSetting {
                        key: r.get(0)?,
                        value: r.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 取单个配置值（命令包渲染 / 单测用）。
    #[allow(dead_code)]
    pub fn plugin_setting(&self, plugin_id: &str, key: &str) -> Result<Option<String>, AppError> {
        self.with(|c| {
            c.query_row(
                "SELECT value FROM plugin_settings WHERE plugin_id=?1 AND key=?2",
                params![plugin_id, key],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
    }

    /// 写入/覆盖单个配置。
    pub fn set_plugin_setting(
        &self,
        plugin_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO plugin_settings (plugin_id, key, value, updated_at) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(plugin_id, key) DO UPDATE SET value=?3, updated_at=?4",
                params![plugin_id, key, value, now_ms()],
            )?;
            Ok(())
        })
    }

    /// 删除插件全部配置（卸载时清理）。
    pub fn clear_plugin_settings(&self, plugin_id: &str) -> Result<usize, AppError> {
        self.with(|c| {
            Ok(c.execute(
                "DELETE FROM plugin_settings WHERE plugin_id=?1",
                params![plugin_id],
            )?)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    /// 按 plugin_id 隔离：A 改不影响 B；clear 只清自己。
    #[test]
    fn settings_are_namespaced() {
        let db = db();
        db.set_plugin_setting("a", "log_path", "/var/log/a")
            .unwrap();
        db.set_plugin_setting("b", "log_path", "/var/log/b")
            .unwrap();
        db.set_plugin_setting("a", "tz", "UTC").unwrap();

        let a = db.plugin_settings("a").unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(
            db.plugin_setting("a", "log_path").unwrap().as_deref(),
            Some("/var/log/a")
        );
        assert_eq!(
            db.plugin_setting("b", "log_path").unwrap().as_deref(),
            Some("/var/log/b")
        );

        db.set_plugin_setting("a", "log_path", "/new").unwrap();
        assert_eq!(
            db.plugin_setting("a", "log_path").unwrap().as_deref(),
            Some("/new")
        );
        assert_eq!(
            db.plugin_setting("b", "log_path").unwrap().as_deref(),
            Some("/var/log/b")
        );

        assert_eq!(db.clear_plugin_settings("a").unwrap(), 2);
        assert!(db.plugin_settings("a").unwrap().is_empty());
        assert_eq!(db.plugin_settings("b").unwrap().len(), 1);
    }
}
