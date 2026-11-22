//! 审计日志持久化：AI/用户命令的
//! 提议 → 裁决 → 执行 → 结果全链路留痕，是安全事件的可追溯底座。
//!
//! **只存脱敏后的命令与输出摘要（哈希），绝不落原始敏感输出，更不存凭证。**
//! 一行 = 一条命令的完整生命周期：orchestrator 在提议时 `record_audit` 建行，裁决/执行/结果
//! 阶段带着同一 `id` 再次 `record_audit`（`ON CONFLICT` 更新），`created_at` 保持提议时刻。
//!
//! 消费方：orchestrator 命令生命周期落库（p4-2）、commands/audit.rs 只读查询/清空（p4-6）。

use rusqlite::{params, Row};
use serde::Serialize;

use super::{new_id, now_ms, Db};
use crate::ai::policy::RiskLevel;
use crate::error::AppError;

/// 审计裁决结果（`decision`）。
///
/// 区别于 [`crate::ai::policy::Decision`]（策略判定 AUTO/NEEDS_APPROVAL/BLOCKED）：这里记录的是
/// 命令的**最终处置**，额外含人工批准/拒绝/编辑改判/审批超时过期。
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AuditDecision {
    /// 只读命令自动执行。
    Auto,
    /// 人工批准执行。
    Approved,
    /// 人工拒绝。
    Denied,
    /// 命中高危黑名单，直接拦截。
    Blocked,
    /// 人工编辑命令后执行（改判）。
    Edited,
    /// 审批超时过期。
    Expired,
}

impl AuditDecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "AUTO",
            Self::Approved => "APPROVED",
            Self::Denied => "DENIED",
            Self::Blocked => "BLOCKED",
            Self::Edited => "EDITED",
            Self::Expired => "EXPIRED",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "APPROVED" => Self::Approved,
            "DENIED" => Self::Denied,
            "BLOCKED" => Self::Blocked,
            "EDITED" => Self::Edited,
            "EXPIRED" => Self::Expired,
            _ => Self::Auto,
        }
    }
}

/// 一条审计记录（库内行模型 = 回前端 DTO）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    pub id: String,
    pub session_id: Option<String>,
    /// 归属的连接配置（profile）id；用于删服务器时级联清理，旧数据为 NULL。
    pub profile_id: Option<String>,
    /// `ai` | `user`。
    pub actor: String,
    /// 提议命令（脱敏后）。
    pub proposed_command: Option<String>,
    pub risk_level: Option<RiskLevel>,
    pub decision: Option<AuditDecision>,
    /// 实际执行命令（编辑改判后可能不同于提议）。
    pub executed_command: Option<String>,
    pub exit_code: Option<i64>,
    /// 输出摘要/哈希（不存原文）。
    pub output_digest: Option<String>,
    /// 关联触发的 AI 消息 id。
    pub ai_message_id: Option<String>,
    pub created_at: i64,
}

impl AuditEntry {
    /// 新建一条 AI 提议审计（id/时间戳已填，decision/执行结果待后续补全）。
    pub fn ai_proposal(session_id: Option<&str>, proposed_command: &str, risk: RiskLevel) -> Self {
        Self {
            id: new_id(),
            session_id: session_id.map(str::to_string),
            profile_id: None,
            actor: "ai".into(),
            proposed_command: Some(proposed_command.to_string()),
            risk_level: Some(risk),
            decision: None,
            executed_command: None,
            exit_code: None,
            output_digest: None,
            ai_message_id: None,
            created_at: now_ms(),
        }
    }
}

/// 输出摘要：`sha256:<hex>` 形态的 SHA256 指纹（对**已脱敏**输出计算）。
/// 审计只存摘要、绝不落原始输出；用 ring（全树唯一加密后端）。
pub fn digest_output(sanitized_output: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, sanitized_output.as_bytes());
    let hex: String = d.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

const AUDIT_COLS: &str =
    "id, session_id, profile_id, actor, proposed_command, risk_level, decision, \
     executed_command, exit_code, output_digest, ai_message_id, created_at";

impl Db {
    /// 插入或按 `id` 更新一条审计（`created_at` 保持首次写入值）。
    /// orchestrator 用它「建行 → 补裁决 → 补执行结果」，同一 `id` 多次调用即渐进更新。
    pub fn record_audit(&self, e: &AuditEntry) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO audit_log (id, session_id, profile_id, actor, proposed_command, risk_level, decision,
                    executed_command, exit_code, output_digest, ai_message_id, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
                 ON CONFLICT(id) DO UPDATE SET
                    session_id=excluded.session_id, actor=excluded.actor,
                    profile_id=COALESCE(excluded.profile_id, audit_log.profile_id),
                    proposed_command=excluded.proposed_command, risk_level=excluded.risk_level,
                    decision=excluded.decision, executed_command=excluded.executed_command,
                    exit_code=excluded.exit_code, output_digest=excluded.output_digest,
                    ai_message_id=excluded.ai_message_id",
                params![
                    e.id,
                    e.session_id,
                    e.profile_id,
                    e.actor,
                    e.proposed_command,
                    e.risk_level.map(|r| r.as_str()),
                    e.decision.map(|d| d.as_str()),
                    e.executed_command,
                    e.exit_code,
                    e.output_digest,
                    e.ai_message_id,
                    e.created_at,
                ],
            )?;
            Ok(())
        })
    }

    /// 按会话/时间倒序列审计（最新在前）。`session_id`/`since` 为空表示不过滤该维度。
    pub fn list_audit(
        &self,
        session_id: Option<&str>,
        since: Option<i64>,
        limit: i64,
    ) -> Result<Vec<AuditEntry>, AppError> {
        let limit = limit.clamp(1, 2000);
        self.with(|c| {
            // `?n IS NULL OR col >=/= ?n` 让「传 None 即不过滤」无需动态拼 SQL；NULL 参数天然放行。
            let mut st = c.prepare(&format!(
                "SELECT {AUDIT_COLS} FROM audit_log \
                 WHERE (?1 IS NULL OR session_id = ?1) AND (?2 IS NULL OR created_at >= ?2) \
                 ORDER BY created_at DESC, rowid DESC LIMIT ?3"
            ))?;
            let rows = st
                .query_map(params![session_id, since, limit], map_audit)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 清空审计（`session_id` 为空则清全部），返回删除条数。
    pub fn clear_audit(&self, session_id: Option<&str>) -> Result<usize, AppError> {
        self.with(|c| {
            let n = match session_id {
                Some(sid) => {
                    c.execute("DELETE FROM audit_log WHERE session_id = ?1", params![sid])?
                }
                None => c.execute("DELETE FROM audit_log", [])?,
            };
            Ok(n)
        })
    }
}

fn map_audit(r: &Row<'_>) -> rusqlite::Result<AuditEntry> {
    let risk: Option<String> = r.get(5)?;
    let decision: Option<String> = r.get(6)?;
    Ok(AuditEntry {
        id: r.get(0)?,
        session_id: r.get(1)?,
        profile_id: r.get(2)?,
        actor: r.get(3)?,
        proposed_command: r.get(4)?,
        risk_level: risk.as_deref().map(RiskLevel::parse),
        decision: decision.as_deref().map(AuditDecision::parse),
        executed_command: r.get(7)?,
        exit_code: r.get(8)?,
        output_digest: r.get(9)?,
        ai_message_id: r.get(10)?,
        created_at: r.get(11)?,
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
    fn migrates_and_roundtrips_audit() {
        let db = db();
        assert!(db.schema_version().unwrap() >= 1);
        assert!(db.list_audit(None, None, 100).unwrap().is_empty());

        // 建行：AI 提议一条写命令
        let mut e =
            AuditEntry::ai_proposal(Some("s1"), "systemctl restart nginx", RiskLevel::Write);
        e.ai_message_id = Some("m1".into());
        db.record_audit(&e).unwrap();

        // 补裁决 + 执行结果（同 id 再记一次 → 更新）
        e.decision = Some(AuditDecision::Approved);
        e.executed_command = Some("systemctl restart nginx".into());
        e.exit_code = Some(0);
        e.output_digest = Some("sha256:abc123".into());
        db.record_audit(&e).unwrap();

        let rows = db.list_audit(Some("s1"), None, 100).unwrap();
        assert_eq!(rows.len(), 1);
        let got = &rows[0];
        assert_eq!(got.actor, "ai");
        assert_eq!(got.risk_level, Some(RiskLevel::Write));
        assert_eq!(got.decision, Some(AuditDecision::Approved));
        assert_eq!(got.exit_code, Some(0));
        assert_eq!(got.output_digest.as_deref(), Some("sha256:abc123"));
        assert_eq!(got.ai_message_id.as_deref(), Some("m1"));
        // created_at 保持首次提议时刻（更新未覆盖）
        assert_eq!(got.created_at, e.created_at);

        // 其它会话过滤不到
        assert!(db.list_audit(Some("sX"), None, 100).unwrap().is_empty());
    }

    #[test]
    fn audit_filters_by_since_and_clears() {
        let db = db();
        let mut a = AuditEntry::ai_proposal(Some("s1"), "ls", RiskLevel::ReadOnly);
        a.created_at = 1_000;
        a.decision = Some(AuditDecision::Auto);
        db.record_audit(&a).unwrap();
        let mut b = AuditEntry::ai_proposal(Some("s1"), "df -h", RiskLevel::ReadOnly);
        b.created_at = 2_000;
        db.record_audit(&b).unwrap();

        // since 过滤：只要 created_at >= 1500 的
        let recent = db.list_audit(None, Some(1_500), 100).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].created_at, 2_000);
        // 倒序：最新在前
        let all = db.list_audit(None, None, 100).unwrap();
        assert_eq!(all[0].created_at, 2_000);
        assert_eq!(all[1].created_at, 1_000);

        // 按会话清（不匹配的会话删 0 条）
        assert_eq!(db.clear_audit(Some("sX")).unwrap(), 0);
        assert_eq!(db.clear_audit(Some("s1")).unwrap(), 2);
        assert!(db.list_audit(None, None, 100).unwrap().is_empty());
    }

    ///审计表只存 output_digest（摘要/哈希），没有任何存原始输出的列。
    #[test]
    fn audit_table_stores_digest_not_raw_output() {
        let db = db();
        let conn = crate::db::profile::raw(&db);
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('audit_log')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(cols.iter().any(|c| c == "output_digest"));
        assert!(!cols
            .iter()
            .any(|c| c == "output" || c == "stdout" || c == "raw_output"));
    }

    /// output_digest 是确定性的 `sha256:<64 hex>`，且摘要里 grep 不到原文。
    #[test]
    fn digest_output_is_sha256_hex_and_hides_plaintext() {
        let d1 = digest_output("password=hunter2");
        let d2 = digest_output("password=hunter2");
        let d3 = digest_output("password=other-secret");
        let hex = d1.strip_prefix("sha256:").expect("应以 sha256: 开头");
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        // 确定性 + 雪崩（不同输入→不同摘要）
        assert_eq!(d1, d2);
        assert_ne!(d1, d3);
        //审计只存摘要，原文不可从摘要反推/grep
        assert!(!d1.contains("hunter2"));
    }
}
