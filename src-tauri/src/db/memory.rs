//! 记忆持久化：跨对话记住**用户偏好 / 服务器环境 / 常用操作**，
//! 请求时按相关性召回注入上下文，让 AI「越用越懂你」。
//!
//! **安全红线**：`content` 只存**脱敏后**的偏好/环境事实，
//! **绝不存凭证/密钥/密码**——自动提取阶段即过滤（[`crate::ai::memory`]），命中凭证即丢弃不入库；
//! 手动添加的内容入库前也由命令层过一遍 [`crate::ai::sanitize::Sanitizer`]。`embedding` 列为
//! **可选本地向量**（不出网）预留，当前召回以关键词匹配为主，未启用向量检索，故 repo 不读写该列。
//!
//! 分层：本层只管存取与新近度排序；相关性（关键词打分）与 token 预算裁剪在上层
//! [`crate::ai::memory`] 完成，CRUD 命令在 [`crate::commands::memory`]。

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::{new_id, now_ms, Db};
use crate::error::AppError;

/// 记忆作用域（`scope`）。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    /// 全局：跨所有服务器生效（如「回答先给结论再给命令」）。
    Global,
    /// 按服务器：绑定 `connection_id`（如「web-01 用 apt、偏好 systemctl」）。
    Server,
}

impl MemoryScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Server => "server",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "global" => Self::Global,
            _ => Self::Server,
        }
    }
}

/// 记忆来源（`source`）。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum MemorySource {
    /// 自动提取（从对话沉淀，可开关）。
    Auto,
    /// 手动添加（用户在记忆管理里增删改）。
    Manual,
}

impl MemorySource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "manual" => Self::Manual,
            _ => Self::Auto,
        }
    }
}

/// 一条记忆（库内行模型 = 回前端 DTO）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub id: String,
    pub scope: MemoryScope,
    /// `scope=server` 时绑定的连接 id；`global` 时为空。
    pub connection_id: Option<String>,
    /// 记忆内容（脱敏后；绝不存凭证）。
    pub content: String,
    pub source: MemorySource,
    /// 召回关键词（可空，逗号分隔）；上层据此做相关性打分。
    pub keywords: Option<String>,
    pub created_at: i64,
    /// 最近命中时间（召回注入后刷新，用于淘汰排序）；从未命中为空。
    pub last_used_at: Option<i64>,
}

impl Memory {
    /// 新建一条记忆（id/时间戳已填，`last_used_at` 空、`connection_id`/`keywords` 待调用方按需补）。
    pub fn new(scope: MemoryScope, content: impl Into<String>, source: MemorySource) -> Self {
        Self {
            id: new_id(),
            scope,
            connection_id: None,
            content: content.into(),
            source,
            keywords: None,
            created_at: now_ms(),
            last_used_at: None,
        }
    }
}

/// 回前端 / 查询用的列顺序，必须与 [`map_memory`] 的下标一一对应。
const MEMORY_COLS: &str =
    "id, scope, connection_id, content, source, keywords, created_at, last_used_at";

impl Db {
    /// 召回候选：**全部 global 记忆** + 绑定到 `connection_id` 的 **server 记忆**，
    /// 按「最近命中/创建」倒序（新近优先）返回至多 `limit` 条。`connection_id` 为空（通用问答、
    /// 未连服务器）则只召回 global。相关性排序（关键词打分）与 token 预算裁剪在上层完成。
    pub fn recall_memory(
        &self,
        connection_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Memory>, AppError> {
        let limit = limit.clamp(1, 500);
        self.with(|c| {
            // `scope='global'` 恒召回；`server` 仅在给定 connection_id 且匹配时召回。
            // 排序：按「最近命中/创建」倒序；同一时刻（now_ms 粗到毫秒，touch 与插入常落在同一 ms）
            // 时先给「曾被命中」的行（`last_used_at IS NULL` 升序：0=命中过在前），使命中行必优先于
            // 从未使用的同刻行，不因 `rowid DESC` 被后插入行反超。
            let mut st = c.prepare(&format!(
                "SELECT {MEMORY_COLS} FROM memory \
                 WHERE scope = 'global' OR (?1 IS NOT NULL AND scope = 'server' AND connection_id = ?1) \
                 ORDER BY COALESCE(last_used_at, created_at) DESC, (last_used_at IS NULL), rowid DESC LIMIT ?2"
            ))?;
            let rows = st
                .query_map(params![connection_id, limit], map_memory)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 列记忆（管理界面用）：`scope` 为空则不限作用域；`connection_id` 非空则只看该连接的 server 记忆。
    /// 按创建倒序（最新在前）。
    pub fn list_memory(
        &self,
        scope: Option<MemoryScope>,
        connection_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Memory>, AppError> {
        let limit = limit.clamp(1, 2000);
        self.with(|c| {
            // `?n IS NULL OR col = ?n` 让「传 None 即不过滤」无需动态拼 SQL（同 audit/policy 惯例）。
            let mut st = c.prepare(&format!(
                "SELECT {MEMORY_COLS} FROM memory \
                 WHERE (?1 IS NULL OR scope = ?1) AND (?2 IS NULL OR connection_id = ?2) \
                 ORDER BY created_at DESC, rowid DESC LIMIT ?3"
            ))?;
            let rows = st
                .query_map(
                    params![scope.map(|s| s.as_str()), connection_id, limit],
                    map_memory,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 按 id 取单条记忆（编辑回填用）。
    pub fn get_memory(&self, id: &str) -> Result<Option<Memory>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!("SELECT {MEMORY_COLS} FROM memory WHERE id = ?1"))?;
            Ok(st.query_row(params![id], map_memory).optional()?)
        })
    }

    /// 插入或按 `id` 更新记忆（`created_at` 保持首次写入值）。自动提取沉淀与手动增改共用。
    pub fn upsert_memory(&self, m: &Memory) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO memory (id, scope, connection_id, content, source, keywords, created_at, last_used_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(id) DO UPDATE SET
                    scope=excluded.scope, connection_id=excluded.connection_id, content=excluded.content,
                    source=excluded.source, keywords=excluded.keywords, last_used_at=excluded.last_used_at",
                params![
                    m.id,
                    m.scope.as_str(),
                    m.connection_id,
                    m.content,
                    m.source.as_str(),
                    m.keywords,
                    m.created_at,
                    m.last_used_at,
                ],
            )?;
            Ok(())
        })
    }

    /// 删除单条记忆，返回是否删到（存在与否）。
    pub fn delete_memory(&self, id: &str) -> Result<bool, AppError> {
        self.with(|c| {
            let n = c.execute("DELETE FROM memory WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
    }

    /// 清空记忆：`scope` 为空清全部；否则按作用域清（`server` 再按 `connection_id` 限定，未给则清所有 server）。
    /// 返回删除条数。对应 「用户可随时删除」与「清除本地数据」入口。
    pub fn clear_memory(
        &self,
        scope: Option<MemoryScope>,
        connection_id: Option<&str>,
    ) -> Result<usize, AppError> {
        self.with(|c| {
            let n = match (scope, connection_id) {
                (Some(MemoryScope::Server), Some(cid)) => c.execute(
                    "DELETE FROM memory WHERE scope = 'server' AND connection_id = ?1",
                    params![cid],
                )?,
                (Some(s), _) => {
                    c.execute("DELETE FROM memory WHERE scope = ?1", params![s.as_str()])?
                }
                (None, _) => c.execute("DELETE FROM memory", [])?,
            };
            Ok(n)
        })
    }

    /// 刷新一批记忆的 `last_used_at`（召回命中并注入后调用，喂给淘汰排序）。空列表直接返回。
    pub fn touch_memory(&self, ids: &[String]) -> Result<(), AppError> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = now_ms();
        self.with(|c| {
            let mut st = c.prepare("UPDATE memory SET last_used_at = ?1 WHERE id = ?2")?;
            for id in ids {
                st.execute(params![now, id])?;
            }
            Ok(())
        })
    }

    /// 按保留期清理（「保留期限可配」）：删除「最近命中/创建」早于 `retention_days`
    /// 天前的记忆。`retention_days <= 0` 表示永久保留（不清理）。返回删除条数。
    pub fn prune_memory(&self, retention_days: i64) -> Result<usize, AppError> {
        if retention_days <= 0 {
            return Ok(0);
        }
        let cutoff = now_ms() - retention_days.saturating_mul(86_400_000);
        self.with(|c| {
            let n = c.execute(
                "DELETE FROM memory WHERE COALESCE(last_used_at, created_at) < ?1",
                params![cutoff],
            )?;
            Ok(n)
        })
    }
}

fn map_memory(r: &Row<'_>) -> rusqlite::Result<Memory> {
    let scope: String = r.get(1)?;
    let source: String = r.get(4)?;
    Ok(Memory {
        id: r.get(0)?,
        scope: MemoryScope::parse(&scope),
        connection_id: r.get(2)?,
        content: r.get(3)?,
        source: MemorySource::parse(&source),
        keywords: r.get(5)?,
        created_at: r.get(6)?,
        last_used_at: r.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    fn mem(scope: MemoryScope, conn: Option<&str>, content: &str, source: MemorySource) -> Memory {
        let mut m = Memory::new(scope, content, source);
        m.connection_id = conn.map(str::to_string);
        m
    }

    #[test]
    fn migrates_and_roundtrips_memory() {
        let db = db();
        assert!(db.schema_version().unwrap() >= 1);
        assert!(db.list_memory(None, None, 100).unwrap().is_empty());

        let mut m = mem(
            MemoryScope::Server,
            Some("conn-1"),
            "web-01 用 apt，偏好 systemctl",
            MemorySource::Manual,
        );
        m.keywords = Some("apt,systemctl".into());
        db.upsert_memory(&m).unwrap();

        let got = db.get_memory(&m.id).unwrap().unwrap();
        assert_eq!(got.scope, MemoryScope::Server);
        assert_eq!(got.connection_id.as_deref(), Some("conn-1"));
        assert_eq!(got.source, MemorySource::Manual);
        assert_eq!(got.keywords.as_deref(), Some("apt,systemctl"));
        assert!(got.last_used_at.is_none());

        // scope 过滤
        let globals = db
            .list_memory(Some(MemoryScope::Global), None, 100)
            .unwrap();
        assert!(globals.is_empty());
        // connection_id 过滤
        assert_eq!(db.list_memory(None, Some("conn-1"), 100).unwrap().len(), 1);
        assert!(db
            .list_memory(None, Some("conn-x"), 100)
            .unwrap()
            .is_empty());

        // upsert 更新：改 content，created_at 不被覆盖
        let mut edited = got.clone();
        edited.content = "web-01 用 apt；服务用 systemctl 管理".into();
        db.upsert_memory(&edited).unwrap();
        let after = db.get_memory(&m.id).unwrap().unwrap();
        assert_eq!(after.content, edited.content);
        assert_eq!(after.created_at, m.created_at);

        // 删除
        assert!(db.delete_memory(&m.id).unwrap());
        assert!(!db.delete_memory(&m.id).unwrap());
        assert!(db.get_memory(&m.id).unwrap().is_none());
    }

    #[test]
    fn recall_returns_global_plus_matching_server() {
        let db = db();
        let g = mem(
            MemoryScope::Global,
            None,
            "先给结论再给命令",
            MemorySource::Manual,
        );
        let s1 = mem(
            MemoryScope::Server,
            Some("conn-1"),
            "web-01 用 apt",
            MemorySource::Auto,
        );
        let s2 = mem(
            MemoryScope::Server,
            Some("conn-2"),
            "db-01 用 yum",
            MemorySource::Auto,
        );
        for m in [&g, &s1, &s2] {
            db.upsert_memory(m).unwrap();
        }

        // 连到 conn-1：召回 global + conn-1 的 server（不含 conn-2）
        let recalled = db.recall_memory(Some("conn-1"), 100).unwrap();
        let contents: Vec<&str> = recalled.iter().map(|m| m.content.as_str()).collect();
        assert!(contents.contains(&"先给结论再给命令"));
        assert!(contents.contains(&"web-01 用 apt"));
        assert!(!contents.contains(&"db-01 用 yum"));

        // 无 connection（通用问答）：只召回 global
        let only_global = db.recall_memory(None, 100).unwrap();
        assert_eq!(only_global.len(), 1);
        assert_eq!(only_global[0].scope, MemoryScope::Global);

        // limit 生效
        assert_eq!(db.recall_memory(Some("conn-1"), 1).unwrap().len(), 1);
    }

    #[test]
    fn touch_updates_last_used_and_recall_prefers_recent() {
        let db = db();
        let a = mem(MemoryScope::Global, None, "旧记忆 A", MemorySource::Auto);
        let b = mem(MemoryScope::Global, None, "旧记忆 B", MemorySource::Auto);
        db.upsert_memory(&a).unwrap();
        db.upsert_memory(&b).unwrap();

        // 命中 A → last_used_at 刷新，召回时 A 排到最前
        db.touch_memory(std::slice::from_ref(&a.id)).unwrap();
        let recalled = db.recall_memory(None, 100).unwrap();
        assert_eq!(recalled[0].id, a.id);
        assert!(recalled[0].last_used_at.is_some());
        // 空列表 touch 不报错
        db.touch_memory(&[]).unwrap();
    }

    #[test]
    fn clear_by_scope_and_connection() {
        let db = db();
        db.upsert_memory(&mem(MemoryScope::Global, None, "g", MemorySource::Manual))
            .unwrap();
        db.upsert_memory(&mem(
            MemoryScope::Server,
            Some("c1"),
            "s1",
            MemorySource::Auto,
        ))
        .unwrap();
        db.upsert_memory(&mem(
            MemoryScope::Server,
            Some("c2"),
            "s2",
            MemorySource::Auto,
        ))
        .unwrap();

        // 只清 conn-1 的 server 记忆
        assert_eq!(
            db.clear_memory(Some(MemoryScope::Server), Some("c1"))
                .unwrap(),
            1
        );
        assert_eq!(db.list_memory(None, None, 100).unwrap().len(), 2);
        // 清全部 global
        assert_eq!(db.clear_memory(Some(MemoryScope::Global), None).unwrap(), 1);
        // 清全部（剩余 server）
        assert_eq!(db.clear_memory(None, None).unwrap(), 1);
        assert!(db.list_memory(None, None, 100).unwrap().is_empty());
    }

    #[test]
    fn prune_by_retention_days() {
        let db = db();
        let mut old = mem(MemoryScope::Global, None, "很久以前", MemorySource::Auto);
        old.created_at = now_ms() - 200 * 86_400_000; // 200 天前
        let fresh = mem(MemoryScope::Global, None, "刚记的", MemorySource::Auto);
        db.upsert_memory(&old).unwrap();
        db.upsert_memory(&fresh).unwrap();

        // 永久保留（0）不删任何
        assert_eq!(db.prune_memory(0).unwrap(), 0);
        // 保留 90 天：200 天前的被清，新记忆保留
        assert_eq!(db.prune_memory(90).unwrap(), 1);
        let left = db.list_memory(None, None, 100).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].content, "刚记的");
    }

    ///memory 表没有任何明文凭证列（凭证只可能在 content 里，而 content 入库前必脱敏）。
    #[test]
    fn memory_table_has_no_credential_column() {
        let db = db();
        let conn = crate::db::profile::raw(&db);
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('memory')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(cols.iter().any(|c| c == "content"));
        assert!(!cols.iter().any(|c| c == "password"
            || c == "api_key"
            || c == "secret"
            || c == "token"
            || c == "private_key"));
    }
}
