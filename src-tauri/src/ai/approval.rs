//! AI 命令审批的进程内回执表。
//!
//! 复用 主机密钥 TOFU 已验证过的 oneshot 回执模式（见 [`crate::ssh::host_key::Pending`]）：
//! orchestrator 在 `interrupt` 节点用 [`Pending::register`] 登记一个 `request_id` 拿到 `Receiver`，
//! 抛审批事件给前端；前端应答经 IPC 命令（c5）调 [`Pending::respond`] 送回裁决，orchestrator
//! `await` 该 `Receiver` 挂起循环，直到用户裁决（批准/拒绝/编辑改判）或超时。
//!
//! 裁决通道承载 [`ApprovalOutcome`]，**仍绝不承载凭证**：「编辑改判」传的是用户改后的**命令串**
//! （落审计前经脱敏），不是机密。
//!
//! 与 `host_keys` 用**各自独立**的表：两类审批语义不同、id 空间互不干扰，便于分别清理与审计。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tokio::sync::oneshot;

use crate::ai::policy::{Reason, RiskLevel};

/// 用户对一次 AI 命令审批的裁决（批准 / 编辑 / 拒绝）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// 批准，按 AI 原样执行。
    Approved,
    /// 批准并记住：**本会话**后续需审批命令一律自动通过（会话结束即失效）。
    ApprovedAlways,
    /// 拒绝，不执行。
    Denied,
    /// 编辑改判：用户改了命令，orchestrator 对改后命令**重新分级**再决定执行/拦截/再审批。
    /// 载荷是改后的命令串（仅 `run_command` 有意义）。
    Edited(String),
}

/// 抛给前端的审批请求负载（`ai-approval-request` 事件与 [`Pending::list`] 共用同一结构）。
/// **不含任何凭证**，只带展示与裁决所需信息。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalPrompt {
    /// 回执 id（前端应答时回传，对应 [`Pending`]）。
    pub request_id: String,
    /// 目标会话。
    pub session_id: String,
    /// 发起本审批的**轨**（"agent"/"chat"）：前端据此把审批卡归到来源轨渲染（并发时当前页签
    /// 未必等于发起轨，故必须由后端随事件带下来、不能前端猜 rail）。不属展示字段之外无含凭证。
    pub track: String,
    /// 工具名（如 `run_command`）。
    pub tool: String,
    /// 人类可读的调用摘要（如具体命令）。
    pub summary: String,
    /// 风险级别。
    pub risk: RiskLevel,
    /// AI 说明「为什么要做这件事」（授权卡主文案；工具 `purpose` 入参）。
    pub purpose: String,
    /// 判定理由（PolicyEngine 给出）：`key` 供前端 i18n 本地化，`zh` 为未命中词典时的中文回落。
    pub reason: Reason,
}

/// 表的值元组：(入队序号, 审批上下文快照, 应答通道)。抽出别名避开 `type_complexity`。
type PendingEntry = (u64, ApprovalPrompt, oneshot::Sender<ApprovalOutcome>);

/// 待审批请求表：`request_id → (入队序号, 审批上下文快照, 应答通道)`。廉价构造，内部自带锁，绝不跨 `await` 持锁。
///
/// 记下归属会话是必需的：审批等待本身没有有效时限（`orchestrator` 侧 `APPROVAL_TIMEOUT` 实为
/// 不过期），连接一旦断开/关闭，挂在上面的 `run_turn` 就会永久挂住并撑住前端的全局发送态，
/// 所以会话结束时必须按会话批量清掉这些请求（[`Pending::cancel_session`]）。
#[derive(Default)]
pub struct Pending {
    /// 快照为 [`Pending::list`] 留底（前端新挂载窗口/重挂载时重同步审批卡）；序号保证列表按入队序稳定排序。
    map: Mutex<HashMap<String, PendingEntry>>,
    seq: AtomicU64,
    /// 「本次连接总是允许」：会话 id 集合；命中则后续 NeedsApproval 直接放行。
    always_sessions: Mutex<HashSet<String>>,
}

impl Pending {
    /// 登记一个待审批请求，返回接收裁决的 `Receiver`（[`ApprovalOutcome`]）。
    /// `prompt` 快照留底供 [`Pending::list`] 重同步；归属会话（`prompt.session_id`）用于会话结束时
    /// 定向清理（请求本身仍按 `request_id` 应答）。
    pub fn register(&self, prompt: &ApprovalPrompt) -> oneshot::Receiver<ApprovalOutcome> {
        let (tx, rx) = oneshot::channel();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.map
            .lock()
            .unwrap()
            .insert(prompt.request_id.clone(), (seq, prompt.clone(), tx));
        rx
    }

    /// 本会话是否已「总是允许」。
    pub fn session_trusted(&self, session_id: &str) -> bool {
        self.always_sessions.lock().unwrap().contains(session_id)
    }

    /// 记住会话信任（本次连接总是允许）。
    pub fn trust_session(&self, session_id: &str) {
        self.always_sessions
            .lock()
            .unwrap()
            .insert(session_id.to_string());
    }

    /// 会话关闭时清掉信任，避免 id 复用串权。
    pub fn untrust_session(&self, session_id: &str) {
        self.always_sessions.lock().unwrap().remove(session_id);
    }

    /// 前端应答；返回 `false` 表示请求已失效（超时/重复应答/未知 id）。
    pub fn respond(&self, id: &str, outcome: ApprovalOutcome) -> bool {
        self.map
            .lock()
            .unwrap()
            .remove(id)
            .is_some_and(|(_, _, tx)| tx.send(outcome).is_ok())
    }

    /// 清掉某会话的全部挂起审批（连接关闭/断线时由 [`crate::session::teardown`] 调用）：
    /// 取走即 drop 发送端，挂在 `Receiver` 上的 `run_turn` 会收到 `RecvError`、按「审批通道已
    /// 关闭」收尾，不会把编排轮次永久挂在一个已经不存在的连接上。返回清掉的条数。
    pub fn cancel_session(&self, session_id: &str) -> usize {
        let mut map = self.map.lock().unwrap();
        let dead: Vec<String> = map
            .iter()
            .filter(|(_, (_, p, _))| p.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        let n = dead.len();
        for id in dead {
            map.remove(&id);
        }
        n
    }

    /// 按 (会话, 轨) 定向清掉该轨挂起的审批（终止单轨在途轮时由 `ai_cancel_turn` 调用）：
    /// 只动归属该轨的请求，另一轨不受牵连。返回清掉的条数。
    pub fn cancel_track(&self, session_id: &str, track: &str) -> usize {
        let mut map = self.map.lock().unwrap();
        let dead: Vec<String> = map
            .iter()
            .filter(|(_, (_, p, _))| p.session_id == session_id && p.track == track)
            .map(|(id, _)| id.clone())
            .collect();
        let n = dead.len();
        for id in dead {
            map.remove(&id);
        }
        n
    }

    /// 主动丢弃某请求（按 id），返回此前是否存在。
    /// 目前仅供测试与定点作废使用；会话级清理走 [`Pending::cancel_session`]。
    #[allow(dead_code)]
    pub fn cancel(&self, id: &str) -> bool {
        self.map.lock().unwrap().remove(id).is_some()
    }

    /// 当前挂起审批的快照（按入队序号排序）：供 `ai_list_pending_approvals` 命令给**新挂载的前端
    /// 窗口/重挂载**重同步审批卡（事件是瞬时的，错过即看不到，而后端 `run_turn` 正挂在回执上等裁决）。
    /// 只读出快照，不动应答通道。
    pub fn list(&self) -> Vec<ApprovalPrompt> {
        let mut v: Vec<(u64, ApprovalPrompt)> = self
            .map
            .lock()
            .unwrap()
            .values()
            .map(|(seq, p, _)| (*seq, p.clone()))
            .collect();
        v.sort_by_key(|(seq, _)| *seq);
        v.into_iter().map(|(_, p)| p).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一份最小审批快照（只关心 id/会话/轨，其余字段占位）。
    fn prompt(id: &str, sid: &str) -> ApprovalPrompt {
        prompt_track(id, sid, "chat")
    }

    fn prompt_track(id: &str, sid: &str, track: &str) -> ApprovalPrompt {
        ApprovalPrompt {
            request_id: id.to_string(),
            session_id: sid.to_string(),
            track: track.to_string(),
            tool: "run_command".into(),
            summary: "ls".into(),
            risk: RiskLevel::Write,
            purpose: String::new(),
            reason: Reason::plain("pol.readonly", ""),
        }
    }

    #[test]
    fn register_then_respond_delivers_decision() {
        let p = Pending::default();
        let mut rx = p.register(&prompt("r1", "s1"));
        assert!(p.respond("r1", ApprovalOutcome::Approved));
        assert_eq!(rx.try_recv().unwrap(), ApprovalOutcome::Approved);
    }

    #[test]
    fn respond_unknown_id_is_false() {
        let p = Pending::default();
        assert!(!p.respond("nope", ApprovalOutcome::Approved));
    }

    #[test]
    fn second_respond_is_false() {
        let p = Pending::default();
        let _rx = p.register(&prompt("r2", "s1"));
        assert!(p.respond("r2", ApprovalOutcome::Denied));
        // 已被移除，二次应答失效
        assert!(!p.respond("r2", ApprovalOutcome::Approved));
    }

    #[test]
    fn cancel_removes_pending() {
        let p = Pending::default();
        let _rx = p.register(&prompt("r3", "s1"));
        assert!(p.cancel("r3"));
        assert!(!p.cancel("r3"));
        assert!(!p.respond("r3", ApprovalOutcome::Approved));
    }

    /// 列表反映挂起集：登记按入队序可见、应答/会话清理后即从列表移出。
    #[test]
    fn list_reflects_pending_in_enqueue_order() {
        let p = Pending::default();
        let _a = p.register(&prompt("a", "s1"));
        let mut b = p.register(&prompt("b", "s2"));
        let got: Vec<String> = p.list().into_iter().map(|x| x.request_id).collect();
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
        // 应答 a → a 出列，b 仍在
        assert!(p.respond("a", ApprovalOutcome::Approved));
        let got: Vec<String> = p.list().into_iter().map(|x| x.request_id).collect();
        assert_eq!(got, vec!["b".to_string()]);
        // 会话 s2 清理 → b 出列（drop 发送端，b 通道已关）
        assert_eq!(p.cancel_session("s2"), 1);
        assert!(p.list().is_empty());
        assert!(matches!(
            b.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
    }

    /// 会话结束定向清理：只动归属该会话的请求，其他会话的挂起审批不受牵连。
    #[test]
    fn cancel_session_only_touches_that_session() {
        let p = Pending::default();
        let mut mine = p.register(&prompt("a1", "s-mine"));
        p.register(&prompt("a2", "s-mine"));
        let mut other = p.register(&prompt("b1", "s-other"));
        assert_eq!(p.cancel_session("s-mine"), 2);
        // 发送端已被 drop → 挂在上面的编排轮次会看到通道关闭，不会永久等下去
        assert!(matches!(
            mine.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(!p.respond("a1", ApprovalOutcome::Approved));
        assert!(!p.respond("a2", ApprovalOutcome::Approved));
        // 另一会话仍可正常应答
        assert!(p.respond("b1", ApprovalOutcome::Approved));
        assert_eq!(other.try_recv().unwrap(), ApprovalOutcome::Approved);
    }

    /// 编辑改判：回执携带改后命令串（orchestrator 据此重新分级）。
    #[test]
    fn edited_outcome_carries_command() {
        let p = Pending::default();
        let mut rx = p.register(&prompt("r4", "s1"));
        assert!(p.respond("r4", ApprovalOutcome::Edited("ls -la".into())));
        assert_eq!(
            rx.try_recv().unwrap(),
            ApprovalOutcome::Edited("ls -la".into())
        );
    }

    /// 并发轨隔离：同一会话两轨各挂一张审批，按轨取消只 drop 该轨通道，另一轨不受牵连、仍可应答。
    #[test]
    fn cancel_track_only_touches_that_track() {
        let p = Pending::default();
        let mut agent = p.register(&prompt_track("a", "s1", "agent"));
        let mut chat = p.register(&prompt_track("c", "s1", "chat"));
        assert_eq!(p.cancel_track("s1", "agent"), 1);
        assert!(matches!(
            agent.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(!p.respond("a", ApprovalOutcome::Approved));
        assert!(p.respond("c", ApprovalOutcome::Approved));
        assert_eq!(chat.try_recv().unwrap(), ApprovalOutcome::Approved);
    }
}
