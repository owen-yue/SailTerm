//! AI 澄清反问的进程内回执表（与命令审批 [`crate::ai::approval`] 各自独立）。
//!
//! 复用审批同款 oneshot 挂起-回执模式：`ask_user` 工具在 `invoke` 里 [`PendingQuestion::register`]
//! 拿到 `Receiver`，emit `ai-question-request` 给前端渲染澄清卡，`await` 挂起本轮直到用户作答；
//! 前端经 IPC 命令 [`crate::commands::ai::ai_respond_question`] 调 [`PendingQuestion::respond`]
//! 送回答案，编排循环随即解冻、把答案作为工具结果回填历史。
//!
//! 与审批分表的理由：两者语义不同——审批问「能不能做这件事」，反问问「该按哪种方案做」，
//! id 空间独立便于分别清理与审计。载荷只带用户选择/输入的文本，**绝无凭证**。
//!
//! 抛给前端的澄清负载（[`QuestionPrompt`] 等）与审批同构地**下沉在本模块**：事件负载与
//! [`PendingQuestion::list`] 快照共用同一结构，登记时留底，供新挂载的前端窗口重同步卡片。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tokio::sync::oneshot;

/// 抛给前端的澄清反问选项（serde camelCase）。界面固定提供末位「其他（自行填写）」出口，
/// 并把标了 recommended 的项进卡预选（带「推荐」徽标），故模型只需给候选项（每题恰标一项
/// recommended），无需自己塞这些出口。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<bool>,
}

/// 卡片内的一道子题：题干 + 候选项 + 单选/多选语义。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QuestionSub {
    pub question: String,
    pub options: Vec<QuestionOption>,
    pub multi: bool,
}

/// 抛给前端的澄清反问负载（`ai-question-request` 事件与 [`PendingQuestion::list`] 共用同一结构）。
/// 一次可携带 1–4 道子题，前端渲染成一张可切换、统一提交的问题卡。**不含任何凭证**。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QuestionPrompt {
    pub request_id: String,
    pub session_id: String,
    /// 发起本题组的轨（"agent"/"chat"）：前端据此把澄清卡归到来源轨渲染（并发时当前页签未必等于发起轨）。
    pub track: String,
    pub questions: Vec<QuestionSub>,
}

/// 澄清反问请求事件名（前端渲染澄清卡；与审批事件 [`crate::ai::orchestrator::EVENT_AI_APPROVAL`] 并列）。
pub const EVENT_AI_QUESTION: &str = "ai-question-request";

/// 用户对一次澄清反问的裁决。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionOutcome {
    /// 用户给出答案（选项文本 / 自定义输入；多题/多选卡片由前端拼成一段文本）。
    Answer(String),
    /// 用户让 AI 自行决定（兜底出口：新 UI 已不提供「你替我定」按钮，仅剩旧前端/未作答回落在用）。
    AiDecides,
}

/// 表的值元组：(入队序号, 反问卡片快照, 应答通道)。抽出别名避开 `type_complexity`。
type PendingQEntry = (u64, QuestionPrompt, oneshot::Sender<QuestionOutcome>);

/// 待回答请求表：`request_id → (入队序号, 反问卡片快照, 应答通道)`。廉价构造、内部自带锁、绝不跨 `await` 持锁。
///
/// 记下归属会话是必需的：反问等待本身不设超时（与审批一致，绝不自动替用户作答），
/// 连接一旦断开/关闭，挂在上面的 `run_turn` 会永久挂住并撑住前端全局发送态，
/// 故会话结束时必须按会话批量清掉这些请求（[`PendingQuestion::cancel_session`]）。
/// 请求里归属轨（`prompt.track`）用于**按轨定向取消**（[`PendingQuestion::cancel_track`]）：chat/agent 并发时
/// 终止一轨不应误清另一轨挂起的反问。
#[derive(Default)]
pub struct PendingQuestion {
    /// 快照为 [`PendingQuestion::list`] 留底（前端新挂载窗口/重挂载时重同步澄清卡）；序号保证列表按入队序稳定排序。
    map: Mutex<HashMap<String, PendingQEntry>>,
    seq: AtomicU64,
}

impl PendingQuestion {
    /// 登记一个待回答请求，返回接收答案的 `Receiver`（[`QuestionOutcome`]）。
    /// `prompt` 快照留底供 [`PendingQuestion::list`] 重同步；归属会话（`prompt.session_id`）用于
    /// 会话结束时定向清理，归属轨（`prompt.track`）供按轨定向取消（请求本身仍按 `request_id` 应答）。
    pub fn register(&self, prompt: &QuestionPrompt) -> oneshot::Receiver<QuestionOutcome> {
        let (tx, rx) = oneshot::channel();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.map
            .lock()
            .unwrap()
            .insert(prompt.request_id.clone(), (seq, prompt.clone(), tx));
        rx
    }

    /// 前端应答；返回 `false` 表示请求已失效（重复应答 / 未知 id / 已被会话清理）。
    pub fn respond(&self, id: &str, outcome: QuestionOutcome) -> bool {
        self.map
            .lock()
            .unwrap()
            .remove(id)
            .is_some_and(|(_, _, tx)| tx.send(outcome).is_ok())
    }

    /// 会话关闭时清掉其全部挂起问题（由 [`crate::session::teardown`] 调用）：
    /// 取走即 drop 发送端，挂在 `Receiver` 上的编排轮次收到 `RecvError`、按「通道已关闭」收尾，
    /// 不会把轮次永久挂在一个已不存在的连接上。返回清掉的条数。
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

    /// 按 (会话, 轨) 定向清掉该轨挂起的反问（终止单轨在途轮时由 `ai_cancel_turn` 调用）：
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

    /// 当前挂起反问的快照（按入队序号排序）：供 `ai_list_pending_questions` 命令给**新挂载的前端
    /// 窗口/重挂载**重同步澄清卡（事件是瞬时的，错过即看不到，而后端 `run_turn` 正挂在回执上等作答）。
    /// 只读出快照，不动应答通道。
    pub fn list(&self) -> Vec<QuestionPrompt> {
        let mut v: Vec<(u64, QuestionPrompt)> = self
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

    /// 构造一份最小反问快照（只关心 id/会话/轨，题目内容占位）。
    fn prompt(id: &str, sid: &str, track: &str) -> QuestionPrompt {
        QuestionPrompt {
            request_id: id.to_string(),
            session_id: sid.to_string(),
            track: track.to_string(),
            questions: vec![QuestionSub {
                question: "选哪个方案？".into(),
                options: vec![QuestionOption {
                    label: "A".into(),
                    description: None,
                    recommended: None,
                }],
                multi: false,
            }],
        }
    }

    #[test]
    fn register_then_respond_delivers_answer() {
        let p = PendingQuestion::default();
        let mut rx = p.register(&prompt("q1", "s1", "chat"));
        assert!(p.respond("q1", QuestionOutcome::Answer("方案 A".into())));
        assert_eq!(
            rx.try_recv().unwrap(),
            QuestionOutcome::Answer("方案 A".into())
        );
    }

    #[test]
    fn respond_unknown_id_is_false() {
        let p = PendingQuestion::default();
        assert!(!p.respond("nope", QuestionOutcome::AiDecides));
    }

    #[test]
    fn second_respond_is_false() {
        let p = PendingQuestion::default();
        let _rx = p.register(&prompt("q2", "s1", "chat"));
        assert!(p.respond("q2", QuestionOutcome::AiDecides));
        assert!(!p.respond("q2", QuestionOutcome::AiDecides));
    }

    /// 会话结束定向清理：只动归属该会话的请求，其他会话不受牵连；被清理的接收端看到通道关闭。
    #[test]
    fn cancel_session_only_touches_that_session() {
        let p = PendingQuestion::default();
        let mut mine = p.register(&prompt("a1", "s-mine", "chat"));
        let mut other = p.register(&prompt("b1", "s-other", "chat"));
        assert_eq!(p.cancel_session("s-mine"), 1);
        assert!(matches!(
            mine.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(!p.respond("a1", QuestionOutcome::AiDecides));
        assert!(p.respond("b1", QuestionOutcome::AiDecides));
        assert_eq!(other.try_recv().unwrap(), QuestionOutcome::AiDecides);
    }

    /// 并发轨隔离：同一会话两轨各挂一道反问，按轨取消只 drop 该轨通道，另一轨不受牵连、仍可应答。
    #[test]
    fn cancel_track_only_touches_that_track() {
        let p = PendingQuestion::default();
        let mut agent = p.register(&prompt("q-agent", "s1", "agent"));
        let mut chat = p.register(&prompt("q-chat", "s1", "chat"));
        assert_eq!(p.cancel_track("s1", "agent"), 1);
        // agent 轨挂起的接收端看到通道关闭
        assert!(matches!(
            agent.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        // chat 轨不受牵连，仍可正常作答
        assert!(!p.respond("q-agent", QuestionOutcome::AiDecides));
        assert!(p.respond("q-chat", QuestionOutcome::Answer("仍有效".into())));
        assert_eq!(chat.try_recv().unwrap(), QuestionOutcome::Answer("仍有效".into()));
    }

    /// 快照重同步：list 按入队序返回挂起卡片；应答/清理后即从快照中消失（新窗口据此补卡或不再补）。
    #[test]
    fn list_returns_pending_snapshots_in_order() {
        let p = PendingQuestion::default();
        let _r1 = p.register(&prompt("q1", "s1", "chat"));
        let _r2 = p.register(&prompt("q2", "s1", "chat"));
        let ids: Vec<String> = p.list().into_iter().map(|x| x.request_id).collect();
        assert_eq!(ids, vec!["q1", "q2"]);
        p.respond("q1", QuestionOutcome::AiDecides);
        assert_eq!(p.list().len(), 1);
        p.cancel_session("s1");
        assert!(p.list().is_empty());
    }
}
