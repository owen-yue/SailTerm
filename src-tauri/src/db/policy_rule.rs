//! 用户自定义策略规则持久化：黑/白/写名单，
//! 由 PolicyEngine 分级时与内置规则合并（p4-3 接入）。
//!
//! 规则只是**命令模式**（通配/正则由 PolicyEngine 解释），本身不含凭证，也永不回传明文。
//! `action` 为空时按 `kind` 取隐含动作：白名单→放行、写名单→审批、黑名单→拦截（见 [`PolicyRule::effective_action`]）。
//!
//! 消费方：commands/ai.rs 每轮加载启用规则注入 PolicyEngine（p4-3）；commands/policy.rs 提供 CRUD（p4-6）。

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::{new_id, now_ms, Db};
use crate::error::AppError;

/// 规则类别（`kind`）。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// 只读白名单：命中即视为只读、可自动执行。
    ReadonlyWhitelist,
    /// 写名单：命中视为写操作，需人工审批。
    WriteList,
    /// 黑名单：命中视为高危，直接拦截。
    Blacklist,
}

impl RuleKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadonlyWhitelist => "readonly_whitelist",
            Self::WriteList => "write_list",
            Self::Blacklist => "blacklist",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "write_list" => Self::WriteList,
            "blacklist" => Self::Blacklist,
            _ => Self::ReadonlyWhitelist,
        }
    }
}

/// 规则命中后的动作（`action`，可空 → 按 kind 隐含）。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum RuleAction {
    /// 放行（自动执行）。
    Allow,
    /// 需人工审批。
    Approval,
    /// 直接拦截。
    Block,
}

impl RuleAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Approval => "approval",
            Self::Block => "block",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "allow" => Self::Allow,
            "block" => Self::Block,
            _ => Self::Approval,
        }
    }
}

/// 一条用户自定义策略规则（库内行模型 = 回前端 DTO）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRule {
    pub id: String,
    pub kind: RuleKind,
    /// 命令/正则模式，由 PolicyEngine 解释。
    pub pattern: String,
    /// 显式动作；为空则按 `kind` 取隐含动作。
    pub action: Option<RuleAction>,
    /// 规则说明，展示于审批弹窗。
    pub note: Option<String>,
    pub enabled: bool,
    pub sort_order: i64,
    pub created_at: i64,
}

impl PolicyRule {
    /// 新建一条启用中的规则（id/时间戳已填，action 留空按 kind 隐含）。
    pub fn new(kind: RuleKind, pattern: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            kind,
            pattern: pattern.into(),
            action: None,
            note: None,
            enabled: true,
            sort_order: 0,
            created_at: now_ms(),
        }
    }

    /// 生效动作：显式 `action` 优先，否则按 `kind` 隐含（白→放行、写→审批、黑→拦截）。
    pub fn effective_action(&self) -> RuleAction {
        self.action.unwrap_or(match self.kind {
            RuleKind::ReadonlyWhitelist => RuleAction::Allow,
            RuleKind::WriteList => RuleAction::Approval,
            RuleKind::Blacklist => RuleAction::Block,
        })
    }
}

const RULE_COLS: &str = "id, kind, pattern, action, note, enabled, sort_order, created_at";

impl Db {
    /// 列规则：`kind` 为空则不限类别；`enabled_only` 为真则只取启用中的。
    /// 排序稳定：`sort_order ASC, created_at ASC, rowid ASC`。
    pub fn list_policy_rules(
        &self,
        kind: Option<RuleKind>,
        enabled_only: bool,
    ) -> Result<Vec<PolicyRule>, AppError> {
        self.with(|c| {
            // `?n IS NULL / = 0 OR ...` 让「不过滤」无需动态拼 SQL。
            let mut st = c.prepare(&format!(
                "SELECT {RULE_COLS} FROM policy_rules \
                 WHERE (?1 IS NULL OR kind = ?1) AND (?2 = 0 OR enabled = 1) \
                 ORDER BY sort_order ASC, created_at ASC, rowid ASC"
            ))?;
            let rows = st
                .query_map(
                    params![kind.map(|k| k.as_str()), i64::from(enabled_only)],
                    map_rule,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 按 id 取单条规则。
    pub fn get_policy_rule(&self, id: &str) -> Result<Option<PolicyRule>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {RULE_COLS} FROM policy_rules WHERE id = ?1"
            ))?;
            Ok(st.query_row(params![id], map_rule).optional()?)
        })
    }

    /// 插入或按 `id` 更新规则（`created_at` 保持首次写入值）。
    pub fn upsert_policy_rule(&self, r: &PolicyRule) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO policy_rules (id, kind, pattern, action, note, enabled, sort_order, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(id) DO UPDATE SET
                    kind=excluded.kind, pattern=excluded.pattern, action=excluded.action,
                    note=excluded.note, enabled=excluded.enabled, sort_order=excluded.sort_order",
                params![
                    r.id,
                    r.kind.as_str(),
                    r.pattern,
                    r.action.map(|a| a.as_str()),
                    r.note,
                    i64::from(r.enabled),
                    r.sort_order,
                    r.created_at,
                ],
            )?;
            Ok(())
        })
    }

    /// 删除规则，返回是否删到（存在与否）。
    pub fn delete_policy_rule(&self, id: &str) -> Result<bool, AppError> {
        self.with(|c| {
            let n = c.execute("DELETE FROM policy_rules WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
    }
}

fn map_rule(r: &Row<'_>) -> rusqlite::Result<PolicyRule> {
    let kind: String = r.get(1)?;
    let action: Option<String> = r.get(3)?;
    let enabled: i64 = r.get(5)?;
    Ok(PolicyRule {
        id: r.get(0)?,
        kind: RuleKind::parse(&kind),
        pattern: r.get(2)?,
        action: action.as_deref().map(RuleAction::parse),
        note: r.get(4)?,
        enabled: enabled != 0,
        sort_order: r.get(6)?,
        created_at: r.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn policy_rule_effective_action_defaults() {
        // kind 隐含动作：白→放行、写→审批、黑→拦截
        assert_eq!(
            PolicyRule::new(RuleKind::ReadonlyWhitelist, "ls *").effective_action(),
            RuleAction::Allow
        );
        assert_eq!(
            PolicyRule::new(RuleKind::WriteList, "systemctl *").effective_action(),
            RuleAction::Approval
        );
        assert_eq!(
            PolicyRule::new(RuleKind::Blacklist, "rm -rf /").effective_action(),
            RuleAction::Block
        );
        // 显式 action 覆盖 kind 隐含
        let mut r = PolicyRule::new(RuleKind::Blacklist, "reboot");
        r.action = Some(RuleAction::Approval);
        assert_eq!(r.effective_action(), RuleAction::Approval);
    }

    #[test]
    fn policy_rule_roundtrip_filter_upsert_delete() {
        let db = db();
        assert!(db.list_policy_rules(None, false).unwrap().is_empty());

        let wl = PolicyRule::new(RuleKind::ReadonlyWhitelist, "ls *");
        let mut bl = PolicyRule::new(RuleKind::Blacklist, "rm -rf /");
        bl.note = Some("绝对禁止".into());
        db.upsert_policy_rule(&wl).unwrap();
        db.upsert_policy_rule(&bl).unwrap();

        // kind 过滤
        let blacks = db
            .list_policy_rules(Some(RuleKind::Blacklist), false)
            .unwrap();
        assert_eq!(blacks.len(), 1);
        assert_eq!(blacks[0].pattern, "rm -rf /");
        assert_eq!(blacks[0].note.as_deref(), Some("绝对禁止"));

        // get by id
        let got = db.get_policy_rule(&wl.id).unwrap().unwrap();
        assert_eq!(got.kind, RuleKind::ReadonlyWhitelist);
        assert!(got.enabled);

        // upsert 更新：停用 + 改 note，created_at 不被覆盖
        let mut wl2 = wl.clone();
        wl2.enabled = false;
        wl2.note = Some("临时停用".into());
        db.upsert_policy_rule(&wl2).unwrap();
        let after = db.get_policy_rule(&wl.id).unwrap().unwrap();
        assert!(!after.enabled);
        assert_eq!(after.note.as_deref(), Some("临时停用"));
        assert_eq!(after.created_at, wl.created_at);

        // enabled_only：过滤掉停用的白名单，只剩黑名单
        let enabled = db.list_policy_rules(None, true).unwrap();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].kind, RuleKind::Blacklist);

        // 删除
        assert!(db.delete_policy_rule(&wl.id).unwrap());
        assert!(!db.delete_policy_rule(&wl.id).unwrap());
        assert!(db.get_policy_rule(&wl.id).unwrap().is_none());
    }
}
