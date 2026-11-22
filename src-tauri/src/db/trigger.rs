//! 输出触发器持久化：对终端输出做模式匹配的规则表。
//!
//! **安全红线**：`pattern` 只是匹配规则（正则源串），**绝不含凭证**；
//! 触发动作在前端本地执行（toast / 高亮），不落盘输出、不出网。
//!
//! 分层：本层管存取与排序；CRUD 命令在 [`crate::commands::trigger`]；匹配在前端 `Terminal.tsx`。

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::{new_id, now_ms, Db};
use crate::error::AppError;

/// 触发动作：匹配到输出后做什么。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TriggerAction {
    /// 弹 toast 告警（默认）。
    Alert,
    /// 在终端里高亮匹配文本（前端 decoration）。
    Highlight,
    /// 客户端语义着色：按 `color` 色键给匹配文本换前景色（关键字高亮）。
    Colorize,
}

impl TriggerAction {
    pub fn parse(s: &str) -> Self {
        match s {
            "highlight" => Self::Highlight,
            "colorize" => Self::Colorize,
            _ => Self::Alert,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alert => "alert",
            Self::Highlight => "highlight",
            Self::Colorize => "colorize",
        }
    }
}

/// 着色规则支持的语义色键（= xterm 主题里的 ANSI 槽位名，随主题切换取色）。
pub const TRIGGER_COLORS: [&str; 6] = ["red", "green", "yellow", "blue", "magenta", "cyan"];

/// 归一化色键：未知值落 `red`（fail-safe，坏配置不能让终端不可读）。
pub fn normalize_color(s: &str) -> String {
    TRIGGER_COLORS
        .iter()
        .find(|c| **c == s)
        .map(|c| c.to_string())
        .unwrap_or_else(|| "red".into())
}

/// 一条触发规则（库内行模型 = 回前端 DTO）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TriggerRule {
    pub id: String,
    /// 规则名（列表展示）。
    pub name: String,
    /// 正则源串（前端 `RegExp` 编译；后端只存不执行）。
    pub pattern: String,
    pub action: TriggerAction,
    /// 着色色键（仅 [`TriggerAction::Colorize`] 用）；其余动作存而不用。
    pub color: String,
    pub enabled: bool,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TriggerRule {
    pub fn new(name: impl Into<String>, pattern: impl Into<String>) -> Self {
        let now = now_ms();
        Self {
            id: new_id(),
            name: name.into(),
            pattern: pattern.into(),
            action: TriggerAction::Alert,
            color: "red".into(),
            enabled: true,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        }
    }
}

const COLS: &str = "id, name, pattern, action, color, enabled, sort_order, created_at, updated_at";

impl Db {
    /// 列出全部触发规则（按 sort_order → name 稳定排序）。
    pub fn list_triggers(&self) -> Result<Vec<TriggerRule>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {COLS} FROM trigger_rule ORDER BY sort_order ASC, name ASC, rowid ASC"
            ))?;
            let rows = st
                .query_map([], map_trigger)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 新建或更新一条规则（`id` 已存在则覆盖字段）。
    pub fn upsert_trigger(&self, r: &TriggerRule) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                &format!(
                    "INSERT INTO trigger_rule ({COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) \
                     ON CONFLICT(id) DO UPDATE SET name=?2, pattern=?3, action=?4, color=?5, \
                     enabled=?6, sort_order=?7, updated_at=?9"
                ),
                params![
                    r.id,
                    r.name,
                    r.pattern,
                    r.action.as_str(),
                    r.color,
                    r.enabled as i64,
                    r.sort_order,
                    r.created_at,
                    r.updated_at
                ],
            )?;
            Ok(())
        })
    }

    /// 删除单条，返回是否删到。
    pub fn delete_trigger(&self, id: &str) -> Result<bool, AppError> {
        self.with(|c| Ok(c.execute("DELETE FROM trigger_rule WHERE id=?1", params![id])? > 0))
    }

    /// 取单条。
    pub fn get_trigger(&self, id: &str) -> Result<Option<TriggerRule>, AppError> {
        self.with(|c| {
            c.query_row(
                &format!("SELECT {COLS} FROM trigger_rule WHERE id=?1"),
                params![id],
                map_trigger,
            )
            .optional()
            .map_err(Into::into)
        })
    }
}

fn map_trigger(row: &Row<'_>) -> rusqlite::Result<TriggerRule> {
    Ok(TriggerRule {
        id: row.get(0)?,
        name: row.get(1)?,
        pattern: row.get(2)?,
        action: TriggerAction::parse(&row.get::<_, String>(3)?),
        color: normalize_color(&row.get::<_, String>(4)?),
        enabled: row.get::<_, i64>(5)? != 0,
        sort_order: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    /// 规则表增删改往返一致。
    ///
    /// 迁移 019 会种入内置着色规则，故本用例只在「基线之外」验证增删改，不断言绝对条数。
    #[test]
    fn upsert_list_delete_roundtrip() {
        let db = db();
        let base = db.list_triggers().unwrap().len();
        let mut r = TriggerRule::new("磁盘告警", r"disk.*full");
        r.action = TriggerAction::Highlight;
        db.upsert_trigger(&r).unwrap();
        let list = db.list_triggers().unwrap();
        assert_eq!(list.len(), base + 1);
        let mine = list.iter().find(|x| x.id == r.id).unwrap();
        assert_eq!(mine.pattern, r"disk.*full");
        assert_eq!(mine.action, TriggerAction::Highlight);
        assert!(mine.enabled);
        // 编辑
        let mut edited = mine.clone();
        edited.enabled = false;
        edited.name = "改名".into();
        db.upsert_trigger(&edited).unwrap();
        let list = db.list_triggers().unwrap();
        assert_eq!(list.len(), base + 1);
        assert!(!list.iter().find(|x| x.id == r.id).unwrap().enabled);
        assert_eq!(list.iter().find(|x| x.id == r.id).unwrap().name, "改名");
        assert!(db.delete_trigger(&r.id).unwrap());
        assert!(!db.delete_trigger(&r.id).unwrap());
        assert_eq!(db.list_triggers().unwrap().len(), base);
    }

    /// 迁移 019 种入的内置着色规则：固定 id、动作 colorize、色键合法。
    #[test]
    fn seeded_colorize_rules_exist() {
        let db = db();
        let list = db.list_triggers().unwrap();
        let seeded: Vec<_> = list
            .iter()
            .filter(|r| r.id.starts_with("sys-trg-"))
            .collect();
        assert_eq!(seeded.len(), 5, "应种入 5 条内置着色规则");
        for r in &seeded {
            assert_eq!(r.action, TriggerAction::Colorize);
            assert!(TRIGGER_COLORS.contains(&r.color.as_str()), "非法色键 {}", r.color);
            assert!(r.enabled);
        }
    }

    /// 表内无凭证列（护栏，同 snippet/ssh_key/tunnel）。
    #[test]
    fn trigger_table_has_no_credential_column() {
        let db = db();
        let cols: Vec<String> = db
            .with(|c| {
                let mut st = c.prepare("PRAGMA table_info(trigger_rule)")?;
                let rows = st
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .unwrap();
        for forbidden in [
            "password",
            "passphrase",
            "secret",
            "credential",
            "private_key",
        ] {
            assert!(
                !cols.iter().any(|c| c.contains(forbidden)),
                "trigger_rule 不应有凭证列：{forbidden}"
            );
        }
    }

    /// action 解析：未知值落 Alert（fail-safe）。
    #[test]
    fn action_parse_defaults_to_alert() {
        assert_eq!(TriggerAction::parse("alert"), TriggerAction::Alert);
        assert_eq!(TriggerAction::parse("highlight"), TriggerAction::Highlight);
        assert_eq!(TriggerAction::parse("colorize"), TriggerAction::Colorize);
        assert_eq!(TriggerAction::parse("nope"), TriggerAction::Alert);
    }

    /// 色键归一化：未知值落 red。
    #[test]
    fn color_normalize_defaults_to_red() {
        assert_eq!(normalize_color("magenta"), "magenta");
        assert_eq!(normalize_color(""), "red");
        assert_eq!(normalize_color("hotpink"), "red");
    }
}
