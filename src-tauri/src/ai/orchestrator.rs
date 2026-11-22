//! AI 编排状态机：一个轻量循环，把「模型请求 → 工具调用 →
//! 安全裁决 → 审批中断 → 执行 → 脱敏 → 回填」串成一条**可证明无绕过**的线性路径。
//!
//! 为什么不用 LangGraph 类框架（预授权回退）：本项目的编排拓扑是固定的
//! 单环，框架的动态拓扑/子图/持久化 checkpoint 用不上；而安全节点（[`crate::ai::policy`]
//! 裁决、[`crate::ai::sanitize`] 脱敏、审批中断）无论如何都要自己实现。把重活（多 provider 协议
//! 差异、SSE、tool-call 解析、消息归一）交给 rig，这里只保留 ~150 行循环 + 安全门，反而更好
//! 证明：`policy_check → interrupt → execute_tool → sanitize` 是 [`dispatch_tool`] 里
//! 顺序排列的语句，无分支能跳过。
//!
//! 扩展点：
//! - 加工具 = `impl Tool` + `ToolRegistry::register` 一行，本文件零改动；
//! - 加 provider = [`crate::ai::providers::Client`] 一个 match 分支，本文件零改动。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rig::completion::message::{
    AssistantContent, Message, ToolCall, ToolName, ToolResultContent, UserContent,
};
use rig::completion::ToolDefinition;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use tokio::time::timeout;

use crate::ai::approval::{ApprovalOutcome, ApprovalPrompt};
use crate::ai::attach::{self, TurnImage};
use crate::ai::context;
use crate::ai::policy::{Decision, PolicyEngine, RiskLevel, ScanLevel, UserRule, Verdict};
use crate::ai::providers::{Client, CompleteParams, StreamDelta};
use crate::ai::sanitize::Sanitizer;
use crate::ai::tools::{ToolCtx, ToolRegistry};
use crate::db::ai::SanitizeLevel;
use crate::db::audit::{digest_output, AuditDecision, AuditEntry};
use crate::error::AppError;
use crate::state::AppState;

/// 单轮用户消息内允许的最大模型往返数的**默认值**（防死循环 / 失控刷调用）。
/// 装 Docker / 排障等多步任务容易超过旧的 24→40，现默认抬到 100 以少被打断；实际以设置项 `ai.max_tool_rounds`
/// 为准（见 [`crate::commands::settings`]），达到上限不再硬停而是优雅收尾、留「继续」入口。
pub const DEFAULT_MAX_TOOL_ROUNDS: usize = 100;
/// 设置项允许的下界：低于此对多步任务没意义。
pub const MIN_MAX_TOOL_ROUNDS: usize = 8;
/// 设置项允许的上界：高于此基本等于放弃兜底闸，仅限重度用户显式调高。
pub const MAX_MAX_TOOL_ROUNDS: usize = 200;

/// 把任意输入的上限钳制到 `[MIN_MAX_TOOL_ROUNDS, MAX_MAX_TOOL_ROUNDS]`（设置项落库前后双重防越界）。
pub const fn clamp_max_tool_rounds(v: usize) -> usize {
    if v < MIN_MAX_TOOL_ROUNDS {
        MIN_MAX_TOOL_ROUNDS
    } else if v > MAX_MAX_TOOL_ROUNDS {
        MAX_MAX_TOOL_ROUNDS
    } else {
        v
    }
}
/// 审批**不设超时**（用户可能离开很久）：一直等到人工裁决。
/// 超时反而危险——睡觉回来命令可能已被拒绝或（若误配成自动）执行；这里永不自动执行。
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(365 * 24 * 3600);
/// 单次工具调用内「编辑改判」的轮数上限（防反复改命令空转；编辑→重判）。
const MAX_APPROVAL_EDITS: usize = 5;
/// 审批请求事件（前端弹审批卡片）。
pub const EVENT_AI_APPROVAL: &str = "ai-approval-request";
/// 编排过程事件（工具调用/结果/最终答复/失败，前端流式展示时间线）。
pub const EVENT_AI_STEP: &str = "ai-step";
/// 文本 token 流式事件：`{sessionId, delta}`，前端累加显示。
pub const EVENT_AI_STREAM: &str = "ai-stream";

/// 一次编排运行的配置（命令层从 provider 档案 + AI 设置组装）。
#[derive(Clone, Debug)]
pub struct RunConfig {
    /// 模型名（如 `gpt-4o-mini` / `claude-3-5-sonnet` / `llama3.1`）。
    pub model: String,
    /// 系统提示词。
    pub preamble: String,
    /// 采样温度；`None` 用 provider 默认。
    pub temperature: Option<f64>,
    /// 输出 token 上限；`None` 用 provider 默认。
    pub max_tokens: Option<u64>,
    /// 命令扫描层级。
    pub scan_level: ScanLevel,
    /// 内容脱敏层级；云 provider 强制 ≥ Strict（命令层组装时保证）。
    pub sanitize_level: SanitizeLevel,
    /// 全审批模式：连只读工具也要批准。
    pub approve_readonly: bool,
    /// 本轮生效的用户自定义策略规则（命令层每轮从库加载；空则只用内置分级，p4-3）。
    pub policy_rules: Vec<UserRule>,
    /// 单轮最大模型往返数（设置项 `ai.max_tool_rounds`，命令层已钳制到合法区间）：达到则优雅收尾。
    pub max_tool_rounds: usize,
    /// 工具访问范围（由 `ai.chat_mode` 经 [`ChatMode::tool_access`] 派生）：`All` = 智能体（全部工具）；
    /// `ReadOnly` = 只读问答（只下发只读工具，非只读命令执行期直接拒绝、不弹审批）。
    pub tool_access: context::ToolAccess,
    /// 本轮执行通道快照（命令层按 `会话有 PTY && rail==Agent` 算好）：`true` = 命令打进 PTY
    /// 当众执行（像手打一样回显），`false` = 后台静默 exec。逐轮定死，中途切页签不再影响
    /// 在途轮次（tools.rs 经 ToolCtx 直读）。
    pub exec_console: bool,
    /// 本轮发起轨（"agent"/"chat"）：命令层从 `params.rail` 归一（与 `exec_console` 同源）。用于
    /// 拼在途状态复合键 `turn_key`（`ToolCtx`）与随审批/反问事件回带，使 chat/agent 并发互不串台。
    pub track: String,
    /// provider 上下文窗口总量（token，设置项原值）：仅用于轮末回传「上下文占用」分母；
    /// 0 = 未配置，前端据此隐藏占用指示。
    pub max_context_tokens: usize,
    /// AI 发起下载时原生保存目录框的**已本地化标题**（前端随轮下发）：经 ToolCtx 透传给
    /// `transfer_file`，使 tools.rs 不写死展示串。空 = 不 `set_title`（系统默认标题）。
    pub dl_pick_title: String,
}

/// 本轮 token 用量（rig `Usage` 真值逐轮累加；provider 不报数时回落字符估算）。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnUsage {
    /// 本轮全部模型调用的输入 token 之和（含工具回填后的再请求）。
    pub input_tokens: u64,
    /// 本轮全部模型调用的输出 token 之和。
    pub output_tokens: u64,
    /// `input + output` 合计（落库与展示都用这一个数）。
    pub total_tokens: u64,
    /// 末次模型调用实际发送的上下文规模（≈ 下轮起点）：配 `context_window` 算占用百分比。
    pub context_used: u64,
    /// 上下文窗口总量（来自 provider 配置）；0 = 未配置，前端隐藏占用指示。
    pub context_window: u64,
    /// `true` = provider 未回传 usage，数值为本地按字符估算的近似值（前端加 `~` 前缀）。
    pub estimated: bool,
}

/// 一轮对话的最终结果（回填命令层落库 + 返回前端）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TurnResult {
    /// 模型最终的文本答复。
    pub text: String,
    /// 本轮消耗的模型往返次数（1 表示直接答复未调工具）。
    pub rounds: usize,
    /// 本轮过程轨迹（工具调用/结果/计划/反思，已脱敏与截断）：调用方据此落库，
    /// 使「回看历史」能重现思考过程（`ai_chat_messages.steps`）。
    pub steps: Vec<AiStep>,
    /// `true` = 本轮因达单轮往返上限（`cfg.max_tool_rounds`）被**优雅收尾**（非模型自然答复）：调用方照常归档，
    /// 前端据此出现「继续」按钮，下一轮从归档重建历史即可从断点续跑。自然收尾为 `false`。
    pub resumable: bool,
    /// 本轮 token 用量与上下文占用（provider 真值累加；缺报回落估算并标记 estimated）。
    pub usage: TurnUsage,
}

// 抛给前端的审批请求负载已下沉到 [`crate::ai::approval::ApprovalPrompt`]：审批表登记时要用
// 同一结构留底（供 `ai_list_pending_approvals` 重同步），事件负载与列表快照从此同一个类型。

/// 编排过程事件负载（前端流式渲染工具调用时间线）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StepEvent {
    pub session_id: String,
    /// 本事件归属的轮次序号（发送时前端传入、经 `ai_turn` 水位回带）：前端据此丢弃
    /// 「已终止轮次的迟到事件」，避免同会话跨轮串台。
    pub turn: u64,
    pub step: AiStep,
}

/// 编排过程中的一个可展示步骤。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AiStep {
    /// 已完成一次工具调用的裁决（即将执行 / 已拦截 / 待审批）。
    ToolCall {
        tool: String,
        summary: String,
        risk: RiskLevel,
        decision: Decision,
    },
    /// 工具执行结果（`text` 已脱敏）；`ok=false` 表示拦截/拒绝/失败。
    ToolResult {
        tool: String,
        ok: bool,
        text: String,
    },
    /// 任务计划（复杂任务拆解；`write_plan` 工具产出）。
    Plan {
        /// 计划步骤文案（按执行顺序）。
        steps: Vec<String>,
    },
    /// 反思 / 重规划提示（连续工具失败后自动注入）。
    Reflect { message: String },
    /// 一段模型推理链（思考型模型某轮的 thinking）：按发生顺序作为「执行过程」里的一步，
    /// 穿插在工具调用之前；`duration_ms` 供前端显示「思考过程 · Ns」。已脱敏，仅回看不回灌模型。
    Reasoning {
        text: String,
        duration_ms: u64,
    },
    /// 模型最终答复。
    Final { text: String },
}

/// 一段思考的相位计时（毫秒偏移）：首条 reasoning 增量记 start，首条 text/tool 增量记 end。
/// 未收到收尾增量时以本轮调用结束时刻兑底（见 [`RsTiming::duration_ms`]）。
#[derive(Default)]
struct RsTiming {
    start: Option<u64>,
    end: Option<u64>,
}

impl RsTiming {
    fn touch_start(&mut self, ms: u64) {
        if self.start.is_none() {
            self.start = Some(ms);
        }
    }
    fn touch_end(&mut self, ms: u64) {
        if self.start.is_some() && self.end.is_none() {
            self.end = Some(ms);
        }
    }
    fn duration_ms(&self, fallback_end: u64) -> u64 {
        match self.start {
            Some(s) => self.end.unwrap_or(fallback_end).saturating_sub(s),
            None => 0,
        }
    }
}

/// 跑一轮对话（状态机）：`user_text` 入历史后循环
/// `call_model → route`——模型要调工具则经 [`dispatch_tool`]（内含不可绕过的
/// `policy_check → interrupt → execute_tool → sanitize`）把脱敏结果回填历史继续；模型直接给
/// 文本则结束并返回 [`TurnResult`]。
///
/// `history` 由调用方持有、本轮内被追加（用户消息 / assistant 消息 / 工具结果），跨轮复用即
/// 是多轮对话记忆。凭证与 API Key 永不进 `history`。
///
/// `ai_message_id` 是本轮 assistant 消息的**预生成 id**，仅作审计关联键：写入每条 audit_log 的
/// `ai_message_id`，把本轮所有命令审计挂到同一条 AI 消息；调用方随后用它落库 assistant 消息，二者对齐。
///
/// `images` 是本轮随文的图片（已完成入站校验）：为空时推入历史的仍是原来的 `Message::user(text)`
/// （**零行为变化**）；非空时拼成「文本 + 图部分」的单条 `Message::User`。工具结果回填仍只走
/// 文本（本期不给工具开看图口）。
#[allow(clippy::too_many_arguments)] // 编排入口：client/配置/工具表/历史/审计关联 id 皆为独立不可变借用，聚成结构体反损可读性。
pub async fn run_turn(
    app: &AppHandle,
    client: &Client,
    cfg: &RunConfig,
    registry: &ToolRegistry,
    session_id: &str,
    history: &mut Vec<Message>,
    user_text: &str,
    ai_message_id: &str,
    images: &[TurnImage],
) -> Result<TurnResult, AppError> {
    let policy = PolicyEngine::new(cfg.scan_level, cfg.approve_readonly)
        .with_rules(cfg.policy_rules.clone());
    let sanitizer = Sanitizer::new(cfg.sanitize_level);
    let ctx = ToolCtx::new(app.clone(), session_id, cfg.track.clone(), cfg.exec_console, cfg.dl_pick_title.clone());
    // 本轮先登记一个空轨迹（按 `turn_key`）：emit_step 顺手往里写，轮末 take_trace 取走落库（按 (会话,轨) 隔离）。
    begin_trace(app, &ctx.turn_key);
    // 工具集按访问范围下发：智能体=全部；只读问答=只读白名单（非只读命令执行期再兜一道）。
    let readonly_only = matches!(cfg.tool_access, context::ToolAccess::ReadOnly);
    let defs = tool_defs(registry, readonly_only);
    let params = CompleteParams {
        preamble: Some(cfg.preamble.clone()),
        temperature: cfg.temperature,
        max_tokens: cfg.max_tokens,
    };

    // 本轮用户轮：无图维持原来的纯文本形状；有图则把每张拼成 image part。拼不出即拒本轮
    // （入站已校过 mime/体积，走到这里只可能是 base64 编码异常，不该默默丢图继续跑）。
    let mut parts = Vec::with_capacity(images.len());
    for img in images {
        parts.push(attach::turn_part(img)?);
    }
    history.push(attach::user_message(user_text, parts));

    let mut rounds = 0usize;
    let mut fail_streak = 0usize;
    // token 累加器：input 逐轮求和（每轮请求都带全量上下文），context_used 记末轮实际发送量。
    let mut usage = TurnUsage::default();
    loop {
        rounds += 1;
        // call_model：流式拉取，增量按类别（正文/推理/工具开始）实时推给前端（真 token 流）。
        let (tx_app, tx_sid, tx_key) = (app.clone(), session_id.to_string(), ctx.turn_key.clone());
        // 本轮思考相位计时：首条 reasoning 增量 → 首条 text/tool 增量 之间 = 这段思考耗时（供「思考过程 · Ns」）。
        let t0 = Instant::now();
        let rs = Arc::new(Mutex::new(RsTiming::default()));
        let rs_cb = rs.clone();
        let resp = match client
            .complete_streaming(&cfg.model, history.clone(), defs.clone(), params.clone(), {
                move |delta| {
                    let turn = tx_app
                        .state::<AppState>()
                        .ai_turn
                        .lock()
                        .ok()
                        .and_then(|m| m.get(&tx_key).copied())
                        .unwrap_or(0);
                    let ms = t0.elapsed().as_millis() as u64;
                    // kind 分三类：正文走字、推理链单独走字、工具起手只报相位（delta 空）。
                    let (kind, text) = match delta {
                        StreamDelta::Text(t) => {
                            if let Ok(mut g) = rs_cb.lock() {
                                g.touch_end(ms);
                            }
                            ("text", t)
                        }
                        StreamDelta::Reasoning(t) => {
                            if let Ok(mut g) = rs_cb.lock() {
                                g.touch_start(ms);
                            }
                            ("reasoning", t)
                        }
                        StreamDelta::ToolStart => {
                            if let Ok(mut g) = rs_cb.lock() {
                                g.touch_end(ms);
                            }
                            ("tool", String::new())
                        }
                    };
                    let _ = tx_app.emit(
                        EVENT_AI_STREAM,
                        serde_json::json!({ "sessionId": tx_sid, "delta": text, "kind": kind, "turn": turn }),
                    );
                }
            })
            .await
        {
            Ok(r) => r,
            // 模型调用失败：本轮不落库，但轨迹登记要收掉（否则残留在 state 里）。
            Err(e) => {
                take_trace(app, &ctx.turn_key);
                return Err(e);
            }
        };

        // route：从 choice 分离文本与工具调用。
        let mut text = String::new();
        let mut round_reasoning = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        for item in &resp.choice {
            match item {
                AssistantContent::Text(t) => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&t.text);
                }
                AssistantContent::ToolCall(tc) => calls.push(tc.clone()),
                // 推理链：本轮 thinking 段收集起来（轮末脱敏后并入累加器，供历史回看）。
                AssistantContent::Reasoning(r) => round_reasoning.push_str(&r.text),
                // 图像等其余内容本轮不回填。
                _ => {}
            }
        }
        // 本轮有思考增量 → 作为「执行过程」里的一步落轨迹（脱敏），排在工具调用之前，随流按序穿插。
        if !round_reasoning.trim().is_empty() {
            let dur = {
                let g = rs.lock().unwrap_or_else(|e| e.into_inner());
                g.duration_ms(t0.elapsed().as_millis() as u64)
            };
            emit_step(
                &ctx,
                AiStep::Reasoning {
                    text: sanitizer.scrub(round_reasoning.trim()),
                    duration_ms: dur,
                },
            );
        }

        // token 用量：rig 聚合流式终止记录（OpenAI 兼容流由 rig 自动带 include_usage），
        // provider 一个计数都没报 → 回落本地字符估算并标记 estimated，统计口径不断档。
        // rig 0.44 的 usage 各字段均为 Option<u64>（没报 = None，不写 0）。
        if resp.usage.is_reported() {
            usage.input_tokens += resp.usage.input_tokens.unwrap_or(0);
            usage.output_tokens += resp.usage.output_tokens.unwrap_or(0);
            usage.total_tokens += resp.usage.total_tokens.unwrap_or(0);
        } else {
            usage.estimated = true;
            let est_in = history.iter().map(context::message_tokens).sum::<usize>()
                + context::estimate_tokens(&cfg.preamble);
            let est_out = context::estimate_tokens(&text);
            usage.input_tokens += est_in as u64;
            usage.output_tokens += est_out as u64;
            usage.total_tokens += (est_in + est_out) as u64;
        }
        // 末轮实际发送量 = 本轮 input tokens；缺报时用刚发出的历史规模近似（不含 schema/preamble，略低）。
        usage.context_used = match resp.usage.input_tokens {
            Some(n) if n > 0 => n,
            _ => history.iter().map(context::message_tokens).sum::<usize>() as u64,
        };

        // assistant 这一轮原样入历史（含 tool_call，供后续轮次与 tool_result 对齐）。
        // rig 0.44：历史里的 assistant 轮携带来源与停止原因（`resp.message()` 统一构造），
        // 空 choice 不入历史（部分 provider 会拒收空 assistant 消息）。
        history.extend(resp.message());

        // 无工具调用 → 模型已给出最终答复，结束。
        if calls.is_empty() {
            let final_text = text.trim().to_string();
            emit_step(
                &ctx,
                AiStep::Final {
                    text: final_text.clone(),
                },
            );
            return Ok(TurnResult {
                text: final_text,
                rounds,
                steps: take_trace(app, &ctx.turn_key),
                resumable: false,
                usage: TurnUsage {
                    context_window: cfg.max_context_tokens as u64,
                    ..usage
                },
            });
        }

        // 有工具调用但已达往返上限 → **优雅收尾**（不再丢弃整轮）：保留已执行轨迹并让调用方归档本轮，
        // 标记 resumable，前端出现「继续」按钮，下一轮从归档重建历史即可从断点接上，不必从头再来。
        // 与网络/审批「真错误」不同：那些照旧返回 Err 不落库，避免把半截脏轮次写进历史。
        if rounds >= cfg.max_tool_rounds {
            let msg = format!(
                "已达单轮最大工具调用次数（{}），为避免失控自动暂停。以上步骤均已执行并保留，点「继续」可从中断处接着做（不必重复已完成的动作）。",
                cfg.max_tool_rounds
            );
            emit_step(&ctx, AiStep::Final { text: msg.clone() });
            return Ok(TurnResult {
                text: msg,
                rounds,
                steps: take_trace(app, &ctx.turn_key),
                resumable: true,
                usage: TurnUsage {
                    context_window: cfg.max_context_tokens as u64,
                    ..usage
                },
            });
        }

        // execute_tool：逐个执行，结果脱敏后作为 tool_result 回填历史。
        let mut results: Vec<UserContent> = Vec::with_capacity(calls.len());
        for tc in &calls {
            let (safe, ok) = dispatch_tool(
                app,
                &ctx,
                registry,
                &policy,
                &sanitizer,
                session_id,
                ai_message_id,
                readonly_only,
                tc,
            )
            .await;
            // 成败由 dispatch_tool 结构化返回（退出码/超时/安全门处置），不再猜文本前缀：
            // 输出首字符为 `[`（grep 命中 `[xxx]` 行、仅 stderr 告警等）曾被误判为失败，
            // 导致「连续失败→反思」在成功命令后频繁冒出。
            if ok {
                fail_streak = 0;
            } else {
                fail_streak += 1;
            }
            // `ToolCall::result` 用本次调用的 id 与工具名拼出结果（rig 0.44 不再带 provider 字段）。
            results.push(UserContent::ToolResult(
                tc.result(vec![ToolResultContent::Text(safe.into())]),
            ));
        }
        history.push(Message::User { content: results });
        // 连续失败 → 让模型停下来反思并重规划（自主决定是否 write_plan）
        if fail_streak >= 2 {
            fail_streak = 0;
            let hint = "【系统提醒】连续工具失败。请先反思原因（权限/路径/前提条件），必要时用 write_plan 重写计划或改换策略，不要重复同一命令。";
            emit_step(
                &ctx,
                AiStep::Reflect {
                    message: "连续失败，要求反思并可能重规划".into(),
                },
            );
            history.push(Message::user(hint));
        }
    }
}

/// 执行一次工具调用，串起安全路径上不可绕过的三个节点：
/// `policy_check`（[`crate::ai::tools::Tool::verdict`]）→ 如需审批则 `interrupt`
/// （[`request_approval`]）→ `execute_tool`（[`crate::ai::tools::Tool::invoke`]）→ `sanitize`
/// （[`Sanitizer::scrub`]）。审批支持「编辑改判」改后的命令会**重新过 PolicyEngine**（不可绕过），
/// 降到自动档才执行、仍高危则拦截、仍需审批则再抛事件。每个出口（拦截/拒绝/改后拦截/过期/执行失败/成功）都就地写入审计。
///
/// 返回**已脱敏**、可安全回填模型 / 出网的文本与成败标志。拦截/拒绝/失败都转成给模型看的
/// 说明串（而非 `Err`），让模型能据此改走别的路径，同时经 [`AiStep::ToolResult`]（`ok=false`）
/// 上报前端。成败看结构化信号（安全门处置 / 退出码 / 超时），不看输出文本长相。
#[allow(clippy::too_many_arguments)] // 派发需同时持有 app/ctx/工具表/策略/脱敏器/会话/审计关联 id/本次调用，皆不可变借用。
async fn dispatch_tool(
    app: &AppHandle,
    ctx: &ToolCtx,
    registry: &ToolRegistry,
    policy: &PolicyEngine,
    sanitizer: &Sanitizer,
    session_id: &str,
    ai_message_id: &str,
    readonly_only: bool,
    tc: &ToolCall,
) -> (String, bool) {
    let name = tc.function.name.as_str();
    // rig 0.44 将 tool 参数从 `Value` 改成 `serde_json::Map`（按定义必为对象）；下面策略/摘要/
    // 执行仍按 `&Value` 取字段，故包回 `Value::Object`（参数只是一条命令/一个路径，量极小）。
    let args = Value::Object(tc.function.arguments.clone());
    let args = &args;
    // 授权卡「目的」：工具显式 purpose；缺省按工具类型给一句，避免只甩命令不解释
    let purpose = args
        .get("purpose")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match name {
            "run_command" => "执行命令以继续分析/操作".into(),
            "read_file" | "list_dir" | "search_files" => "查看/检索文件以了解现状".into(),
            "transfer_file" => "在本地与服务器之间传输文件".into(),
            _ => "继续完成当前任务".into(),
        });

    let Some(tool) = registry.get(name) else {
        return fail(ctx, name, &format!("[未知工具 {name}]"));
    };

    // 空命令护栏：模型偶发吐 `run_command` 但不带 `command` 字段（摘要回退成 `{}`）。这种审批卡
    // 上只有一个花括号，用户无从判断要执行什么，批准后还会往 shell 灌一条空命令。直接以工具错误
    // 回给模型，让它要么补全命令要么改用文本答复。
    if name == "run_command"
        && args
            .get("command")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return fail(
            ctx,
            name,
            "[参数缺失] run_command 未提供 command，请补全命令后重试，或改用文本答复",
        );
    }

    // policy_check（不可绕过）：run_command 动态分级，其余工具静态只读裁决。
    let verdict = tool.verdict(args, policy);
    // 只读问答模式不改判决：只读命令是自动放行还是逐条批准，仍由用户审批模式设置决定
    // （默认 AutoReadonly=放行不打断；选了「全部审批」则只读也弹框，确认条在聊天面板内）。
    // 只读档的特殊性在下面 NeedsApproval 分支：非只读的待审批命令直接拒绝（能查不能改）。
    let summary = summarize(name, args);
    emit_step(
        ctx,
        AiStep::ToolCall {
            tool: name.to_string(),
            summary: summary.clone(),
            risk: verdict.risk,
            decision: verdict.decision,
        },
    );
    // 计划：额外发一条结构化 Plan，供侧栏展示
    if name == "write_plan" {
        let steps = args
            .get("steps")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !steps.is_empty() {
            emit_step(ctx, AiStep::Plan { steps });
        }
    }

    // 审计范围：所有 run_command（在目标机执行任意 shell，安全关键）+ 任何被策略门拦下/要求审批
    // 的调用（含全审批模式下的只读工具）。自动放行的只读探针（会话信息/终端快照/列目录/读文件）
    // 不落库，避免刷屏（「每条 AI 命令在审计日志可查」）。
    let auditable = name == "run_command" || verdict.decision != Decision::Auto;
    // 落库命令先脱敏：审计记录本身也 grep 不到明文凭证。
    let mut audit = auditable.then(|| {
        let mut a =
            AuditEntry::ai_proposal(Some(session_id), &sanitizer.scrub(&summary), verdict.risk);
        // 归属服务器：删连接配置时据此级联清理审计（Direct/Local 会话无 profile）。
        a.profile_id = app.state::<AppState>().sessions.profile_id(session_id);
        a.ai_message_id = Some(ai_message_id.to_string());
        a
    });

    // 最终要执行的入参与审计裁决。只读自动 / 高危拦截直接定案；需审批则进入可多轮「编辑改判」的
    // interrupt 循环：用户改命令 → 对改后命令重新分级（编辑→重判）→ 自动则执行、
    // 仍高危则拦截、仍需审批则用改后命令再抛事件。安全门顺序不可绕过。
    let mut eff_args = args.clone();
    let mut eff_summary = summary.clone();
    let audit_decision: AuditDecision = match verdict.decision {
        Decision::Blocked => {
            set_decision(&mut audit, AuditDecision::Blocked);
            persist_audit(app, audit.as_ref());
            return fail(
                ctx,
                name,
                &format!("[已拦截] {}（高危操作，禁止执行）", verdict.reason.zh),
            );
        }
        Decision::Auto => AuditDecision::Auto,
        // interrupt（不可绕过）：挂起等待用户裁决；支持编辑改判 → 改后命令重新分级。
        Decision::NeedsApproval => {
            // 只读问答模式：非只读（写/未知）的待审批命令直接拒绝，不弹审批、不碰连接。
            // 「能查不能改」是只读档的定义边界；只读命令的 NeedsApproval 不在此拦——
            // 那是用户选了「全部审批」的结果，尊重设置、照常弹确认条（聊天面板内承担）。
            if readonly_only && verdict.risk != RiskLevel::ReadOnly {
                set_decision(&mut audit, AuditDecision::Denied);
                persist_audit(app, audit.as_ref());
                return fail(
                    ctx,
                    name,
                    &format!(
                        "[只读模式，未执行改动] {}；此操作需「读写」权限——请告知用户把对话页签下方的开关切到「读写」后重试，或把完整命令交由用户自行执行",
                        verdict.reason.zh
                    ),
                );
            }
            // 本次连接「总是允许」：同会话后续需审批命令直接放行（会话结束即失效）
            if app
                .state::<AppState>()
                .ai_approvals
                .session_trusted(session_id)
            {
                AuditDecision::Approved
            } else {
                let mut cur = verdict.clone();
                let mut edited = false;
                let mut edits = 0usize;
                loop {
                    match request_approval(ctx, name, &eff_summary, &purpose, &cur)
                        .await
                    {
                        Ok(ApprovalOutcome::Approved) => {
                            break if edited {
                                AuditDecision::Edited
                            } else {
                                AuditDecision::Approved
                            }
                        }
                        Ok(ApprovalOutcome::ApprovedAlways) => {
                            app.state::<AppState>()
                                .ai_approvals
                                .trust_session(session_id);
                            break if edited {
                                AuditDecision::Edited
                            } else {
                                AuditDecision::Approved
                            };
                        }
                        Ok(ApprovalOutcome::Denied) => {
                            set_decision(&mut audit, AuditDecision::Denied);
                            persist_audit(app, audit.as_ref());
                            return fail(ctx, name, "[用户拒绝执行]");
                        }
                        Ok(ApprovalOutcome::Edited(cmd)) => {
                            edits += 1;
                            if edits > MAX_APPROVAL_EDITS {
                                set_decision(&mut audit, AuditDecision::Edited);
                                persist_audit(app, audit.as_ref());
                                return fail(
                                    ctx,
                                    name,
                                    &format!("[编辑改判超过 {MAX_APPROVAL_EDITS} 次，已中止]"),
                                );
                            }
                            edited = true;
                            eff_args = with_edited_command(name, &eff_args, &cmd);
                            eff_summary = summarize(name, &eff_args);
                            // 改后命令重新过 PolicyEngine（不可绕过）：这是「编辑改判」的安全核心。
                            cur = tool.verdict(&eff_args, policy);
                            emit_step(
                                ctx,
                                AiStep::ToolCall {
                                    tool: name.to_string(),
                                    summary: eff_summary.clone(),
                                    risk: cur.risk,
                                    decision: cur.decision,
                                },
                            );
                            match cur.decision {
                                // 改成高危：拦截，绝不执行。
                                Decision::Blocked => {
                                    set_decision(&mut audit, AuditDecision::Edited);
                                    persist_audit(app, audit.as_ref());
                                    return fail(
                                        ctx,
                                        name,
                                        &format!("[改后命令仍被拦截] {}（未执行）", cur.reason.zh),
                                    );
                                }
                                // 降到自动档（如去掉危险部分）：执行改后命令。
                                Decision::Auto => break AuditDecision::Edited,
                                // 仍需审批：用改后命令再抛一轮审批。
                                Decision::NeedsApproval => continue,
                            }
                        }
                        Err(e) => {
                            // 审批超时/通道关闭 → 按过期处置（fail-safe，与主机密钥 TOFU 一致）。
                            set_decision(&mut audit, AuditDecision::Expired);
                            persist_audit(app, audit.as_ref());
                            return fail(ctx, name, &format!("[审批未完成] {e}"));
                        }
                    }
                }
            }
        }
    };
    set_decision(&mut audit, audit_decision);

    // execute_tool（不可绕过）：在已认证会话上执行（可能是改判后的命令），拿到**未脱敏**原文。
    let out = match tool.invoke(ctx, eff_args).await {
        Ok(o) => o,
        Err(e) => {
            // 执行前基础设施失败（会话断开/通道错误）：命令未在目标机落地，exit_code/摘要留空。
            persist_audit(app, audit.as_ref());
            return fail(ctx, name, &format!("[工具执行失败] {e}"));
        }
    };

    // sanitize（不可绕过）：回填模型 / 出网前抹除明文机密。
    let safe = sanitizer.scrub(&out.text);
    // 工具级成败：看结构化退出码/超时（run_command 的 data 带 exitCode/timedOut；
    // 无 data 的只读工具执行即成功）。以前用「文本以 [ 开头」猜测，成功输出也会被误判。
    let exec_ok = out
        .data
        .as_ref()
        .map(|d| {
            d.get("exitCode")
                .and_then(Value::as_i64)
                .map_or(true, |c| c == 0)
                && !d.get("timedOut").and_then(Value::as_bool).unwrap_or(false)
        })
        .unwrap_or(true);
    // 结果入审计：命令确已执行 → 补 executed_command（改判后可能与提议不同）/ exit_code / 输出摘要。
    if let Some(a) = audit.as_mut() {
        a.executed_command = Some(sanitizer.scrub(&eff_summary));
        a.exit_code = out
            .data
            .as_ref()
            .and_then(|d| d.get("exitCode"))
            .and_then(Value::as_i64);
        a.output_digest = Some(digest_output(&safe));
    }
    persist_audit(app, audit.as_ref());
    emit_step(
        ctx,
        AiStep::ToolResult {
            tool: name.to_string(),
            ok: exec_ok,
            text: safe.clone(),
        },
    );
    // 注：out.data（结构化数据）本轮暂不回填模型、不进事件；c4 补其脱敏、c7 补富卡片渲染。
    (safe, exec_ok)
}

/// 就地把最终处置写进审计行（若该行存在，即本次调用可审计）。
fn set_decision(audit: &mut Option<AuditEntry>, decision: AuditDecision) {
    if let Some(a) = audit {
        a.decision = Some(decision);
    }
}

/// 尽力而为落库审计：拿不到 Db / 写入失败都不阻断编排主流程（审计是旁路，绝不能反过来卡住执行）。
/// 与 [`emit_step`] 同为 best-effort；命令是否执行以安全门裁决为准，不因审计失败而改变。
fn persist_audit(app: &AppHandle, audit: Option<&AuditEntry>) {
    if let Some(a) = audit {
        let _ = app.state::<AppState>().db.record_audit(a);
    }
}

/// 把用户「编辑改判」后的命令塞回工具入参：仅 `run_command` 有可编辑的 `command` 字段；其余工具
/// 原样返回（它们的裁决是静态只读的，无可改判的命令语义，前端也不对其提供 ✎编辑）。
fn with_edited_command(name: &str, args: &Value, edited: &str) -> Value {
    if name != "run_command" {
        return args.clone();
    }
    let mut a = args.clone();
    if let Some(obj) = a.as_object_mut() {
        obj.insert("command".to_string(), Value::String(edited.to_string()));
    }
    a
}

/// 统一失败出口：发一条 `ok=false` 的工具结果事件（按 `ctx` 的轨回带轮次号/落轨迹），并把给模型看的
/// 说明串返回（成败=false）。
fn fail(ctx: &ToolCtx, tool: &str, message: &str) -> (String, bool) {
    emit_step(
        ctx,
        AiStep::ToolResult {
            tool: tool.to_string(),
            ok: false,
            text: message.to_string(),
        },
    );
    (message.to_string(), false)
}

/// `interrupt` 节点：登记一个 oneshot 回执 → 抛审批事件给前端 → 挂起等待用户
/// 裁决或超时。复用主机密钥 TOFU 已验证的模式（[`crate::ssh::host_key`]），走独立的
/// [`crate::ai::approval::Pending`] 表。`Ok(outcome)`=批准/拒绝/编辑改判、`Err`=超时/通道关闭。
async fn request_approval(
    ctx: &ToolCtx,
    tool: &str,
    summary: &str,
    purpose: &str,
    verdict: &Verdict,
) -> Result<ApprovalOutcome, AppError> {
    let prompt = ApprovalPrompt {
        request_id: uuid::Uuid::new_v4().to_string(),
        session_id: ctx.session_id.clone(),
        track: ctx.track.clone(),
        tool: tool.to_string(),
        summary: summary.to_string(),
        risk: verdict.risk,
        purpose: purpose.to_string(),
        reason: verdict.reason.clone(),
    };
    // 先登记再抛事件（同 host_key 序，避免应答先于登记到达）；state 守卫不跨 await。
    // 登记时带上归属会话与快照：`APPROVAL_TIMEOUT` 实为不过期，连接断开/关闭时由
    // `session::teardown` 按会话清掉本条请求，下面的 await 才会以「通道已关闭」收场；
    // 快照则供新挂载的前端窗口经 `ai_list_pending_approvals` 重同步这张审批卡。
    let reply = {
        let state = ctx.app.state::<AppState>();
        state.ai_approvals.register(&prompt)
    };
    let _ = ctx.app.emit(EVENT_AI_APPROVAL, prompt);
    match timeout(APPROVAL_TIMEOUT, reply).await {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(_)) => Err(AppError::Biz {
            key: "err.ai.approvalClosed",
            zh: "审批确认已取消或通道关闭",
        }),
        Err(_) => Err(AppError::Biz {
            key: "err.ai.approvalTimeout",
            zh: "审批确认超时",
        }),
    }
}

/// 发一条编排过程事件（前端流式渲染）。发送失败不影响编排主流程（尽力而为）。
///
/// 同时把该步骤攒进本轮轨迹（按 `ctx.turn_key` 取，若 [`begin_trace`] 已登记）：事件是瞬时的，轨迹才落库，
/// 两者共用同一份数据才能做到「当时看到什么，回看历史还是什么」。攒入前按长度截断，
/// 一次 `du -a` / `tail -f` 的几十 KB 输出不该永久占住归档表；`final` 与回复正文重复，不攒。
fn emit_step(ctx: &ToolCtx, step: AiStep) {
    if !matches!(step, AiStep::Final { .. }) {
        if let Ok(mut map) = ctx.app.state::<AppState>().ai_trace.lock() {
            if let Some(trace) = map.get_mut(&ctx.turn_key) {
                trace.push(shrink_for_archive(step.clone()));
            }
        }
    }
    // 读本轨当前在途轮次序号随事件回带（未登记时为 0，前端不会匹配任何真实轮 → 丢弃）。
    let turn = ctx
        .app
        .state::<AppState>()
        .ai_turn
        .lock()
        .ok()
        .and_then(|m| m.get(&ctx.turn_key).copied())
        .unwrap_or(0);
    let _ = ctx.app.emit(
        EVENT_AI_STEP,
        StepEvent {
            session_id: ctx.session_id.clone(),
            turn,
            step,
        },
    );
}

/// 归档单条工具输出的字符上限：再长的内容侧栏/历史里也没人逐字看。
const ARCHIVE_STEP_CHARS: usize = 2000;

/// 为落库瘦身：只截 `ToolResult` 的输出文本（其余字段本就是展示用短文本），按字符截断
/// 以免切坏 UTF-8，并补一句省略提示。
fn shrink_for_archive(step: AiStep) -> AiStep {
    match step {
        AiStep::ToolResult { tool, ok, text } if text.chars().count() > ARCHIVE_STEP_CHARS => {
            let cut: String = text.chars().take(ARCHIVE_STEP_CHARS).collect();
            AiStep::ToolResult {
                tool,
                ok,
                text: format!("{cut}\n…（输出过长，归档仅保留开头 {ARCHIVE_STEP_CHARS} 字）"),
            }
        }
        other => other,
    }
}

/// 登记本轮轨迹（同 `turn_key` 直接覆盖：上一轮若因异常没取走，也不至于串到本轮）。
fn begin_trace(app: &AppHandle, key: &str) {
    if let Ok(mut map) = app.state::<AppState>().ai_trace.lock() {
        map.insert(key.to_string(), Vec::new());
    }
}

/// 取走并注销本轮轨迹（按 `turn_key`；没登记时返回空，不影响主流程）。
fn take_trace(app: &AppHandle, key: &str) -> Vec<AiStep> {
    app.state::<AppState>()
        .ai_trace
        .lock()
        .ok()
        .and_then(|mut map| map.remove(key))
        .unwrap_or_default()
}

/// 生成工具调用的人类可读摘要（审批卡片 / 步骤时间线用）：优先取业务字段，回退整个参数 JSON。
/// 摘要只用于展示；工具入参本就无凭证字段，故不含机密。
fn summarize(tool: &str, args: &Value) -> String {
    // 传输/检索是多字段工具，单独拼一句人类可读摘要（审批卡/时间线展示）。
    match tool {
        "ask_user" => {
            // 新 schema：questions 数组。摘要取首题题干；多题时补「共 N 题」。旧顶层 question 字段兼容回落。
            if let Some(arr) = args.get("questions").and_then(Value::as_array) {
                let first = arr
                    .first()
                    .and_then(|q| q.get("question").and_then(Value::as_str))
                    .unwrap_or("向用户提问");
                return if arr.len() > 1 {
                    format!("{first}（共 {} 题）", arr.len())
                } else {
                    first.to_string()
                };
            }
            return args
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or("向用户提问")
                .to_string();
        }
        "transfer_file" => {
            let dir = args
                .get("direction")
                .and_then(Value::as_str)
                .unwrap_or("传输");
            let path = args.get("path").and_then(Value::as_str).unwrap_or("");
            let dst = args
                .get("destDir")
                .and_then(Value::as_str)
                .unwrap_or("默认目录");
            return format!("{dir} {path} → {dst}");
        }
        "search_files" => {
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let pat = args.get("pattern").and_then(Value::as_str).unwrap_or("");
            return format!("在 {path} 检索文件名含「{pat}」");
        }
        _ => {}
    }
    let field = match tool {
        "run_command" => Some("command"),
        "read_file" | "list_dir" => Some("path"),
        _ => None,
    };
    field
        .and_then(|f| args.get(f).and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| args.to_string())
}

/// 只读问答模式下暴露给模型的工具白名单：全是「读」——看环境、看终端、看文件、跑只读命令、查资料。
/// 不含 `transfer_file`（写文件系统）与 `write_plan`（多步改动任务编排）。
/// `run_command` 在列，但其非只读命令会在 [`dispatch_tool`] 被「问答模式只读」直接拒绝（不弹审批）。
const READONLY_TOOLS: &[&str] = &[
    "ask_user",
    "run_command",
    "get_session_info",
    "list_services",
    "get_terminal_snapshot",
    "list_dir",
    "read_file",
    // 附件投送被截断时，提示词会指引模型用它按需取回片段（Caps::NONE、纯读归档文本）；
    // 不收进白名单就是「承诺了不存在的路」——附件主用例（只读档问大日志）直接失效。
    "read_attachment",
    // 查本应用自身知识库：只读、离线、`Caps::NONE`——只读问答档里「××在哪看」这类问题全靠它，
    // 不收进来就是让模型凭训练先验（其他 FTP/终端工具）瞎答本客户端。
    "query_knowledge",
    "search_files",
    "web_search",
    "fetch_url",
];

/// 把注册表的 OpenAI 形态 schema 转成 rig 的 [`ToolDefinition`]（发模型用）。rig 0.44
/// 把工具名收成 [`ToolName`]（非空校验），空名 / 缺字段直接跳过该工具。
/// `readonly_only` = 只读问答模式：只保留 [`READONLY_TOOLS`] 白名单内的工具。
fn tool_defs(registry: &ToolRegistry, readonly_only: bool) -> Vec<ToolDefinition> {
    registry
        .schemas()
        .into_iter()
        .filter_map(|s| {
            let f = s.get("function")?;
            let name = f.get("name")?.as_str()?;
            if readonly_only && !READONLY_TOOLS.contains(&name) {
                return None;
            }
            Some(ToolDefinition::new(
                ToolName::new(name).ok()?,
                f.get("description")?.as_str()?,
                f.get("parameters").cloned().unwrap_or(Value::Null),
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_defs_cover_builtins_without_session_id() {
        let registry = ToolRegistry::with_builtins();
        let defs = tool_defs(&registry, false);
        // 每个内建工具都转出了定义。
        assert_eq!(defs.len(), registry.names().len());
        assert_eq!(defs.len(), 14);
        for d in &defs {
            assert!(!d.name.is_empty());
            assert!(!d.description.is_empty());
            // 参数是 object 形态的 JSON Schema。
            assert_eq!(
                d.parameters.get("type").and_then(Value::as_str),
                Some("object"),
                "{} 参数应为 object",
                d.name
            );
            // session_id 绝不进 LLM 可见 schema（由 ToolCtx 绑定）。
            assert!(
                d.parameters["properties"].get("session_id").is_none(),
                "{} 不应含 session_id",
                d.name
            );
        }
    }

    /// 只读问答模式：工具集只保留只读白名单，两个写/编排工具（transfer_file 写文件、
    /// write_plan 多步编排）被排除，run_command 仍在（其非只读命令靠执行期兜底拒绝）。
    #[test]
    fn readonly_tool_defs_exclude_writes() {
        let registry = ToolRegistry::with_builtins();
        let all = tool_defs(&registry, false);
        let ro = tool_defs(&registry, true);
        assert_eq!(all.len(), 14);
        assert_eq!(ro.len(), READONLY_TOOLS.len());
        let names: Vec<String> = ro.iter().map(|d| d.name.to_string()).collect();
        assert!(!names.iter().any(|n| n == "transfer_file"));
        assert!(!names.iter().any(|n| n == "write_plan"));
        assert!(names.iter().any(|n| n == "run_command"));
    }

    #[test]
    fn summarize_prefers_human_fields() {
        assert_eq!(
            summarize("run_command", &json!({ "command": "ls -la" })),
            "ls -la"
        );
        assert_eq!(
            summarize("read_file", &json!({ "path": "/etc/hosts" })),
            "/etc/hosts"
        );
        // 无对应业务字段的工具回退到整个参数 JSON。
        assert_eq!(summarize("get_session_info", &json!({})), "{}");
        // 字段缺失也回退。
        assert_eq!(summarize("run_command", &json!({})), "{}");
    }
}
