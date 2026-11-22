//! 工具层：把 AI 能做的每件事收敛成一个 [`Tool`]，以 JSON Schema 暴露给 LLM，
//! 在既有会话上执行并返回**原始**结果。
//!
//! 设计铁律（开篇）：
//! - 工具入参只有业务参数，**绝无凭证**；`session_id` 由 orchestrator 经 [`ToolCtx`] 绑定，
//! **不进** LLM 可见的 schema——模型无法把工具重定向到别的会话（比 示意更收紧）。
//! - 工具本身只做「执行 + 捕获」。安全裁决（分级/审批，[`Tool::verdict`] 供 orchestrator 的
//!   `policy_check` 节点调用）与出网脱敏都在 orchestrator 的不可绕过节点里完成
//!故 [`Tool::invoke`] 返回的是**未脱敏原文**，禁止直接出网。
//!
//! 复用既有能力：命令执行走 [`crate::ai::exec`]（在已认证连接上另开 exec 通道，不碰 PTY），
//! 目录列举走 [`crate::files`] 的统一后端（SSH-SFTP / FTP 一视同仁，已落地）。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::ai::attach;
use crate::ai::context::SessionEnv;
use crate::ai::exec::{self, ExecResult};
use crate::ai::policy::{PolicyEngine, Reason, RiskLevel, Verdict};
use crate::ai::question::{QuestionOption, QuestionPrompt, QuestionSub, EVENT_AI_QUESTION};
use crate::commands::transfer::run_transfer;
use crate::db::transfer::Direction;
use crate::error::AppError;
use crate::files::{self, FileEntry};
use crate::plugin::host;
use crate::plugin::manifest::PluginAiTool;
use crate::plugin::Permission;
use crate::session::{Caps, SessionKind};
use crate::state::AppState;

/// `read_file` 未指定 `max_bytes` 时的默认上限（面向 LLM 上下文的进一步截断在 c4 token 预算里做）。
const DEFAULT_READ_BYTES: u64 = 64 * 1024;
/// `read_file` 的硬上限：模型可能传一个很大的 `max_bytes`，读回内存前必须先夹紧。
const MAX_READ_BYTES: u64 = 1024 * 1024;
/// `list_dir` 回填给 LLM 的最大条目数，防止超大目录撑爆上下文。
const MAX_LIST_ENTRIES: usize = 500;
/// `get_terminal_snapshot` 未指定 `max_bytes` 时的默认值。
const DEFAULT_SNAPSHOT_BYTES: u64 = 16 * 1024;
/// `get_terminal_snapshot` 的字节上限，防止一次拉太多终端历史挤爆 LLM 上下文。
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
/// `search_files` 单次遍历的条目上限：远端目录树可能极大，成本必须可预期。
const SEARCH_SCAN_LIMIT: usize = 20_000;
/// `search_files` 递归深度上限。
const SEARCH_MAX_DEPTH: usize = 6;
/// `search_files` 回填给 LLM 的命中数上限。
const SEARCH_MAX_HITS: usize = 200;

/// 工具执行上下文：只带路由键与访问托管状态的句柄，**无任何凭证字段**。
pub struct ToolCtx {
    /// 目标会话（唯一路由键）；由 orchestrator 绑定，不来自 LLM。
    pub session_id: String,
    /// 本轮发起轨（"agent"/"chat"）：`ask_user` 登记 pending 时随带，供按轨定向取消与前端按轨渲染。
    pub track: String,
    /// 本轮在途状态的复合键 `{track}\u{1}{session_id}`（见 [`crate::state::ai_turn_key`]）：与 `emit_step`/取消登记同键。
    pub turn_key: String,
    /// 访问 `SessionManager` / SFTP / 传输队列等托管状态（AppHandle 廉价可克隆、Send+Sync）。
    pub app: AppHandle,
    /// 本轮执行通道快照（`会话有 PTY && rail==Agent`，轮开始定死）：`run_command` 据此选
    /// 打进 PTY 当众 / 后台 exec，不再逐条实时读库——中途切页签不影响在途轮次。
    pub console: bool,
    /// 本轮已发起的 `ask_user` 澄清反问计数（[`ToolCtx::new`] 逐轮新建 → 天然按轮隔离）：
    /// 达 [`MAX_ASK_USER_PER_TURN`] 后工具直接回「请自行决定」，卡住模型连环提问打断节奏。
    pub ask_count: Arc<AtomicUsize>,
    /// AI 发起下载时原生保存目录框的**已本地化标题**（前端随轮下发）：`transfer_file` 据此
    /// `set_title`，使本文件不写死任何展示串。空串 = 不 `set_title`（系统默认标题）。
    pub dl_pick_title: String,
}

impl ToolCtx {
    #[allow(clippy::too_many_arguments)] // 逐轮新建的不可变快照：会话/轨/键/执行通道/标题皆为独立字段。
    pub fn new(
        app: AppHandle,
        session_id: impl Into<String>,
        track: impl Into<String>,
        console: bool,
        dl_pick_title: String,
    ) -> Self {
        let session_id = session_id.into();
        let track = track.into();
        let turn_key = crate::state::ai_turn_key(&track, &session_id);
        Self {
            session_id,
            track,
            turn_key,
            app,
            console,
            ask_count: Arc::new(AtomicUsize::new(0)),
            dl_pick_title,
        }
    }
}

/// 工具输出：回填给 LLM 的文本（出网前必经 Sanitizer）+ 可选结构化数据（供前端渲染卡片）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    /// 人类/模型可读的文本结果（多数工具用它）。
    pub text: String,
    /// 可选结构化数据（如目录列表、退出码），前端据此渲染富卡片；不进 LLM 上下文。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ToolOutput {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            text: s.into(),
            data: None,
        }
    }

    pub fn with_data(s: impl Into<String>, data: Value) -> Self {
        Self {
            text: s.into(),
            data: Some(data),
        }
    }
}

/// 一个可被 LLM 调用的工具。
#[async_trait]
pub trait Tool: Send + Sync {
    /// 唯一名（LLM 用它选择工具）。返回 `&str` 而非 `&'static str`：内建工具给静态字面量，
    /// 插件工具（[`PluginTool`]）的名字来自 manifest 运行期解析，只能借用自身字段。
    fn name(&self) -> &str;

    /// 本工具依赖的会话能力档位：[`SessionKind::caps`] 不覆盖它 ⇒ 对该类会话**不下发**。
    ///
    /// 刻意不给默认实现：默认 `NONE`（= 任何会话都给）会让新增工具忘记表态时静默漏过，
    /// 故障形态是「模型调了一个在必定没有通路的会话上、注定失败的工具」，比编译不过难查得多。
    ///
    /// 档位看 [`Tool::invoke`] **实际走哪条通路**，不看名字像什么：`get_terminal_snapshot` 读终端
    /// 环形缓冲 → `PTY`；`get_session_info` 跑 exec 探测 → `EXEC`；`write_plan` 只记录不执行 → `NONE`。
    fn requires(&self) -> Caps;

    /// 一句话描述（进 schema，指导模型何时用、边界在哪）。同 [`Tool::name`] 取 `&str` 容插件动态描述。
    fn description(&self) -> &str;

    /// 参数 JSON Schema（OpenAI `function.parameters` 形态，**不含 `session_id`**）。
    fn parameters(&self) -> Value;

    /// 本次调用的风险裁决：`run_command` 用 `policy` 对命令动态分级，其余工具返回静态级别。
    /// 由 orchestrator 的 `policy_check` 节点调用（不可绕过）。
    fn verdict(&self, args: &Value, policy: &PolicyEngine) -> Verdict;

    /// 执行并返回**原始未脱敏**结果。假定 orchestrator 已完成裁决与（如需）审批。
    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError>;
}

/// 工具注册表：内建工具集 + 名称查找 + 汇总 schema。
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// 装入当前阶段内建工具集（**仅测试用**：生产路径按会话能力走 [`ToolRegistry::for_caps`]）。
    ///
    /// 已落地：`run_command`、`get_session_info`、`get_terminal_snapshot`、`list_dir`、`read_file`、
    /// `search_files`、`transfer_file`（c9 收口：文件检索与传输接进同一工具表，传输复用
    /// [`crate::files::transfer`] 的队列：断点续传 / 并发闸门 / 进度事件一套不缺）。
    /// 后续候选：`write_file`（写远端文件）。
    #[cfg(test)]
    pub fn with_builtins() -> Self {
        Self::from_tools(Self::builtins())
    }

    /// 按会话能力下发工具集：只保留 [`Tool::requires`] 被 `caps` 覆盖的工具。
    ///
    /// 为什么在**构造期**筛而不是在 `schemas()`/`get()` 里各筛一次：注册表就是本轮唯一的工具真相，
    /// 两处各自过滤迟早漂移出「模型看不到但能调」（只过 schemas）或「模型拿到调不动的工具」（只过 get）。
    /// 副作用是 `get()` 对被过滤工具返回 `None`，编排器回「[未知工具 xxx]」——这正是我们想要的行为：
    /// 模型幻觉出一个 FTP 会话上根本不存在的 `run_command` 时，拿到的是明确拒绝而非半途失败。
    ///
    /// 生产路径请用 [`ToolRegistry::assemble`]（内建 + 插件工具单点合成）；本入口只筛内建，
    /// 由 `assemble` 复用（也是「无插件」退化情形与测试的直接依凭）。
    pub fn for_caps(caps: Caps) -> Self {
        Self::from_tools(
            Self::builtins()
                .into_iter()
                .filter(|t| caps.contains(t.requires()))
                .collect(),
        )
    }

    /// 本轮工具真相的**唯一构造点**：内建按档位筛（复用 [`for_caps`](Self::for_caps)）+
    /// 插件技能按 `requires`/启用状态筛，合在一张表里。
    ///
    /// 插件工具与内建走同一 [`Tool`] trait、同一 `caps` 过滤、同一 `schemas()`/`get()`：模型与编排器
    /// 分不出工具来自何处，安全裁决（[`Tool::verdict`]）也因此不可能被插件绕过。
    ///
    /// 同名冲突：**内建优先、插件让位**——内建名是稳定契约（提示词/白名单 `READONLY_TOOLS` 都按它写），
    /// 插件不得覆盖；冲突的插件工具在此丢弃并记一条日志（避免模型拿到一个悄悄被劫持的同名工具）。
    ///
    /// 判定基准是**全量内建名**（[`ToolRegistry::builtins`]）而非「本档已下发的内建名」：档位筛掉的
    /// 名字同样是契约。否则插件声明 `run_command` + `requires: none` 就能在 EXEC 缺席的会话（FTP/本地
    /// shell 档之外）钻过冲突检查入表，再被按名过滤的 `READONLY_TOOLS` 当内建放行，连宿主侧的
    /// 「问答档拒非只读命令」都一并绕过（裁决只剩插件自报的 risk）。
    pub fn assemble(caps: Caps, plugin_tools: Vec<Arc<dyn Tool>>) -> Self {
        let mut reg = Self::for_caps(caps);
        // 先握住清单再取名字切片：`builtins()` 的临时 Vec 一析构，借来的 `&str` 就悬空。
        let builtin_tools = Self::builtins();
        let reserved: Vec<&str> = builtin_tools.iter().map(|t| t.name()).collect();
        for pt in plugin_tools {
            if !caps.contains(pt.requires()) {
                continue;
            }
            if reserved.contains(&pt.name()) || reg.tools.iter().any(|t| t.name() == pt.name()) {
                debug!("[skill] 插件技能 {} 占用内建工具契约名，已丢弃", pt.name());
                continue;
            }
            reg.tools.push(pt);
        }
        reg
    }

    /// [`ToolRegistry::for_caps`] 的按协议入口（仅测试用：生产路径命令层已持有 `caps`，
    /// 直接用 `for_caps`，不重复查表）。
    #[cfg(test)]
    pub fn for_kind(kind: SessionKind) -> Self {
        Self::for_caps(kind.caps())
    }

    /// 内建工具清单：`with_builtins` 与 `for_kind` 共同的唯一来源（两条路各自 new 一遍会漏注册）。
    fn builtins() -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(WritePlan),
            Arc::new(AskUser),
            Arc::new(RunCommand),
            Arc::new(GetSessionInfo),
            Arc::new(ListServices),
            Arc::new(GetTerminalSnapshot),
            Arc::new(ListDir),
            Arc::new(ReadFile),
            Arc::new(ReadAttachment),
            Arc::new(QueryKnowledge),
            Arc::new(SearchFiles),
            Arc::new(TransferFile),
            Arc::new(WebSearch),
            Arc::new(FetchUrl),
        ]
    }

    fn from_tools(tools: Vec<Arc<dyn Tool>>) -> Self {
        Self { tools }
    }

    /// 追加工具（插件系统扩展点，**预留**：当前无调用方，接入插件工具时再启用）。
    #[allow(dead_code)]
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }

    /// 已注册工具名（诊断/测试用）。
    #[allow(dead_code)]
    pub fn names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    /// 汇总成 OpenAI `tools` 数组；orchestrator 转成 rig 的工具定义发模型。
    pub fn schemas(&self) -> Vec<Value> {
        self.tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name(),
                        "description": t.description(),
                        "parameters": t.parameters(),
                    }
                })
            })
            .collect()
    }
}

/// 复杂任务先拆计划（用户在侧栏可见）；计划本身只读，不触发策略审批。
struct WritePlan;

#[async_trait]
impl Tool for WritePlan {
    fn name(&self) -> &str {
        "write_plan"
    }

    /// 只把步骤记成回显文本，不碰任何会话通路 → 无门槛（FTP 会话也该会拆计划）。
    fn requires(&self) -> Caps {
        Caps::NONE
    }

    fn description(&self) -> &str {
        "为复杂/多步任务制定执行计划。先调用本工具拆成 2–8 条步骤（每条一句话，可执行），再逐步执行。简单单步问题不要用。计划会展示给用户。"
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "steps": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "按顺序的步骤文案，2–8 条"
                }
            },
            "required": ["steps"]
        })
    }

    fn verdict(&self, _args: &Value, _policy: &PolicyEngine) -> Verdict {
        Verdict {
            risk: RiskLevel::ReadOnly,
            decision: crate::ai::policy::Decision::Auto,
            reason: Reason::plain("pol.toolPlan", "任务计划为只读元数据"),
            matched: None,
        }
    }

    async fn invoke(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        let n = args
            .get("steps")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if n == 0 {
            return Err(AppError::Invalid("steps 不能为空".into()));
        }
        Ok(ToolOutput::text(format!("计划已记录：{n} 步")))
    }
}

/// 抛给前端的澄清负载结构（[`QuestionPrompt`] 等）已下沉到 [`crate::ai::question`]：
/// 与审批的 [`crate::ai::approval::ApprovalPrompt`] 同构，事件负载与回执表留底快照共用同一结构，
/// 供 `ai_list_pending_questions` 给新挂载的前端窗口重同步卡片。
/// 单轮内向用户提问的轮次上限（护栏：防模型连环抛卡打断节奏；达上限后工具直接回「请自行决定」。
/// 一次卡片可含多题，干扰已大幅降低，故保留此上限）。
const MAX_ASK_USER_PER_TURN: usize = 3;
/// 一张卡片最多携带的子题数 / 每题最多选项数（超出截断，避免卡片过长）。
const MAX_QUESTIONS_PER_ASK: usize = 4;
const MAX_OPTIONS_PER_QUESTION: usize = 4;

/// 把模型给的 `recommended` 解析成布尔：`as_bool` 之外容错字符串 `"true"/"1"/"yes"`（小模型常把布尔写成字符串）。
fn parse_recommended(v: &Value) -> Option<bool> {
    match v.as_bool() {
        Some(b) => Some(b),
        None => v.as_str().map(|s| {
            matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "y"
            )
        }),
    }
}

/// `ask_user`：模型遇到**无法自行确定、且会显著改变后续动作**的关键岔路时，一次性向用户抛出
/// 澄清问题卡（1–4 题、每题带候选选项）并等待作答。纯交互、只读（不碰连接、不落审计）；
/// 答案作为工具结果回填历史。
struct AskUser;

#[async_trait]
impl Tool for AskUser {
    fn name(&self) -> &str {
        "ask_user"
    }

    /// 纯交互（往前端抛一张澄清卡），与会话通路无关。
    fn requires(&self) -> Caps {
        Caps::NONE
    }

    fn description(&self) -> &str {
        concat!(
            "向用户提出澄清问题并等待其作答，支持一次问多题。仅当你遇到【无法自行确定、且会显著改变后续动作】的关键岔路",
            "（关键信息缺失、或多个方案各有取舍而你无依据偏好）时才用；次要细节、能用只读命令查到的、",
            "或你能合理判断的一律自行决定，切勿用它连环提问。**把所有需要用户拍板的问题一次性放进 questions 数组**",
            "（1–4 题），禁止拆成多轮连环单题。每题给 2–4 个互斥候选项（每项一句话说明），答案可并存则设 multi=true。",
            "选项必须覆盖「维持现状/暂不操作」路径：尤其用户目标只是查看状态而目标并不存在（如服务未安装）时，",
            "必须给「不安装、仅告知现状」类选项，不得把提问变成只有推进变更的单选题。每题应恰标一项 recommended=true",
            "（界面据此进卡预选并打「推荐」徽标）。",
            "**硬性约束：无论用户用什么语言，界面总会在你给的选项之后自动追加一个「其他（自行填写）」自由文本出口。**",
            "因此**严禁**你把「其他 / 自行填写 / 自填 / 手动输入 / 直接写软件名 / 让我自己填」这类“其实就是让用户自由输入”",
            "的选项再写进 options——它和界面自带的那个出口完全重复，用户选了也收集不到有效答案（只会把你这句标签当答案回传），",
            "还白占一个名额。需要用户自由输入就交给界面自带的出口，别自己造。",
            "用户作答后点「确定/全部提交」才算提交（未提交的题可回头改）。单轮最多抛卡 3 轮，",
            "超出后本工具会要求你自行决定。",
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "要问用户的问题，1–4 题；把所有需要用户拍板的决策点一次给全，禁止拆成多轮连环单题",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string", "description": "题干，一句话讲清决策点，≤60 字" },
                            "options": {
                                "type": "array",
                                "description": "候选选项，2–4 个，互斥（除非 multi=true）。**不要包含「其他/自行填写/手动输入」类自由输入选项——界面已固定自带该出口**",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "选项短标签，≤20 字" },
                                        "description": { "type": "string", "description": "该选项的含义/取舍，可选，≤40 字" },
                                        "recommended": { "type": "boolean", "description": "你更推荐这项时置 true；每题应恰有一项为 true，界面据此预选并打「推荐」徽标" },
                                    },
                                    "required": ["label"],
                                },
                            },
                            "multi": { "type": "boolean", "description": "是否允许多选，默认 false" },
                        },
                        "required": ["question", "options"],
                    },
                },
            },
            "required": ["questions"],
        })
    }

    fn verdict(&self, _args: &Value, _policy: &PolicyEngine) -> Verdict {
        Verdict {
            risk: RiskLevel::ReadOnly,
            decision: crate::ai::policy::Decision::Auto,
            reason: Reason::plain(
                "pol.toolAskUser",
                "向用户提问为只读交互，不改动任何系统",
            ),
            matched: None,
        }
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        use crate::ai::question::QuestionOutcome;
        use crate::state::AppState;

        // questions 数组：1–4 题（超出截断）。旧单题格式不再兼容——提示词/工具描述已同步新 schema，
        // 模型仍按旧格式调用时回错文本引导重试，比静默降级更可控。
        let raw_q = args.get("questions").and_then(Value::as_array);
        let raw_q = match raw_q {
            Some(arr) => arr,
            None => {
                return Err(AppError::Invalid(
                    "ask_user 需用 questions 数组一次给出全部问题（每项含 question/options/multi）".into(),
                ))
            }
        };
        let mut questions: Vec<QuestionSub> = Vec::new();
        for q in raw_q.iter().take(MAX_QUESTIONS_PER_ASK) {
            let text = q
                .get("question")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if text.is_empty() {
                continue;
            }
            let multi = q
                .get("multi")
                .and_then(parse_recommended)
                .unwrap_or(false);
            // 选项：label 必填、description/recommended 可选；每题最多取 4 项。
            let mut options: Vec<QuestionOption> = q
                .get("options")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|o| {
                            let label = o
                                .get("label")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|s| !s.is_empty())?
                                .to_string();
                            Some(QuestionOption {
                                label,
                                description: o
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .map(str::to_string),
                                recommended: o.get("recommended").and_then(parse_recommended),
                            })
                        })
                        .take(MAX_OPTIONS_PER_QUESTION)
                        .collect()
                })
                .unwrap_or_default();
            // 契约要求「每题恰一项 recommended」（见 schema/prompt），但模型（尤其多选题）常一次标多项。
            // 解析期强制去重：只保留首个 recommended=true，其余清掉——保证界面「推荐」徽标与
            // 「推荐」按钮预选都至多一项，不会出现「一题两个推荐」的自相矛盾。
            let mut seen_rec = false;
            for opt in &mut options {
                if opt.recommended.unwrap_or(false) {
                    if seen_rec {
                        opt.recommended = None;
                    } else {
                        seen_rec = true;
                    }
                }
            }
            if options.is_empty() {
                continue;
            }
            questions.push(QuestionSub {
                question: text.to_string(),
                options,
                multi,
            });
        }
        if questions.is_empty() {
            return Err(AppError::Invalid(
                "ask_user 的 questions 里没有任何含有效题干/选项的问题".into(),
            ));
        }

        // 护栏：单轮反问封顶。fetch_add 返回旧值——旧值已达上限说明这是第 N+1 次，不再打扰用户。
        if ctx.ask_count.fetch_add(1, Ordering::Relaxed) >= MAX_ASK_USER_PER_TURN {
            return Ok(ToolOutput::text(format!(
                "[反问次数已达单轮上限（{MAX_ASK_USER_PER_TURN}）] 不要再向用户提问，请基于现有信息与最佳判断自行决定并继续。"
            )));
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        // 先组装卡片快照，再登记后抛事件（同审批序，避免应答先于登记）；state 守卫不跨 await。
        // 快照留底进回执表，供 `ai_list_pending_questions` 给新挂载/重挂载的前端窗口重同步这张卡。
        let prompt = QuestionPrompt {
            request_id,
            session_id: ctx.session_id.clone(),
            track: ctx.track.clone(),
            questions,
        };
        let rx = {
            let state = ctx.app.state::<AppState>();
            state.ai_questions.register(&prompt)
        };
        let _ = ctx.app.emit(EVENT_AI_QUESTION, prompt);
        // 挂起等待用户作答（不设超时，与审批一致；会话结束时由 teardown 按会话清通道）。
        // 前端把整卡（含多题）的作答拼成一段文本送来；AiDecides 仅兼容旧前端/兜底，新 UI 不再产生。
        match rx.await {
            Ok(QuestionOutcome::Answer(ans)) => {
                Ok(ToolOutput::text(format!("用户回答：「{ans}」")))
            }
            Ok(QuestionOutcome::AiDecides) => Ok(ToolOutput::text(
                "用户让你自行决定：请基于最佳判断继续，并在最终回复里简要说明你替他做的选择与理由。",
            )),
            // 通道关闭（会话结束 / 被取消）：不阻断，提示模型无额外信息可用（run_turn 随后也会被收掉）。
            Err(_) => Ok(ToolOutput::text("[用户未作答，会话已结束]")),
        }
    }
}

/// `run_command`：在会话上执行任意 shell（唯一风险不定的工具，每次调用必经 PolicyEngine）。
struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    /// 命令承诺 stdout/stderr/**退出码**，这三样只有 exec 通道给得起（console 模式把文本
    /// 打进 PTY 只是展示形态，取结果仍靠环形缓冲，拿不到退出码），故取 `EXEC`。
    /// （有 PTY 但无 EXEC 的会话——Telnet/串口——因此拿不到 run_command，打进 PTY
    /// 的通路只余能力声明；将来若要支持这类会话需把 requires 改为 PTY ∪ EXEC）
    fn requires(&self) -> Caps {
        Caps::EXEC
    }

    fn description(&self) -> &str {
        // 多行拼成一条静态字串（Rust 相邻字面量不自动拼接，靠 concat!）。
        concat!(
            "在指定会话上执行一条 shell 命令，返回 stdout/stderr/退出码。执行通道由本轮开始时的上下文定死：",
            "Agent 页签会把命令直接打进用户终端（像手打一样回显执行，继承交互 shell 的目录与环境，",
            "结果从终端画面回收、拿不到退出码）；对话页签与其他会话在后台静默 exec（不占用用户终端），",
            "这意味着每条命令都是一个全新的非交互 shell：cd 与 export 不跨命令保留——需要切目录或设环境变量时，",
            "把 `cd /path && ...` 写在同一条命令里。写操作与高危命令需用户批准后才执行；",
            "高危命令（如 rm -rf /、mkfs、关机等）会被直接拦截。绝不接受任何凭证参数。优先用只读命令诊断。",
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "要执行的 shell 命令" },
                "purpose": {
                    "type": "string",
                    "description": "一句话说明执行该命令的目的（给用户看的授权理由），≤40 字，例如「查看磁盘占用以定位异常」"
                },
                "timeout_ms": { "type": "integer", "description": "超时(毫秒)，默认 30000，最大 300000" },
            },
            "required": ["command", "purpose"],
        })
    }

    fn verdict(&self, args: &Value, policy: &PolicyEngine) -> Verdict {
        let cmd = args.get("command").and_then(Value::as_str).unwrap_or("");
        policy.classify(cmd)
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        let cmd = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::Invalid("run_command 缺少 command 参数".into()))?;
        let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(0);

        // 执行通道由命令层逐轮快照进 ToolCtx（`会话有 PTY && rail==Agent`）：Agent 页签下
        // 命令恒打进用户终端，像手打一样回显执行（继承 cwd/env、可跑 TTY 命令）；聊天页签
        // 与无 PTY 会话（FTP 等）走后台 exec。一轮开始时定死，中途切页签不再换通道。
        if ctx.console {
            return run_in_console(ctx, cmd).await;
        }

        let r = exec::exec_command(&ctx.app, &ctx.session_id, cmd, timeout_ms).await?;
        Ok(format_exec(&r))
    }
}

/// 控制台模式：把命令写入 PTY（用户可见执行），稍候从终端环形缓冲取结果。
async fn run_in_console(ctx: &ToolCtx, cmd: &str) -> Result<ToolOutput, AppError> {
    use crate::session::SessionCmd;
    use tauri::Manager;
    let st = ctx.app.state::<crate::state::AppState>();
    let tx = st
        .sessions
        .sender(&ctx.session_id)
        .ok_or_else(|| AppError::NoSuchSession(ctx.session_id.clone()))?;
    // 回车必须是 \r（Enter 键语义）：裸 \n 在 PowerShell/PSReadLine 下会被吃进多行编辑，
    // 变成未闭合的 `>>` 续行提示，命令根本不执行。
    tx.send(SessionCmd::Write(format!("{cmd}\r").into_bytes()))
        .await
        .map_err(|_| AppError::NoSuchSession(ctx.session_id.clone()))?;
    // 等命令在终端里跑出输出（粗粒度：读到环缓冲尾部）
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let snap = st
        .sessions
        .ring(&ctx.session_id)
        .map(|r| crate::ai::tools::strip_ansi(&r.snapshot(4096)))
        .unwrap_or_default();
    let tail = snap
        .rsplit('\n')
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    Ok(ToolOutput::text(format!(
        "[已在终端执行] {cmd}\n…终端近况：\n{tail}"
    )))
}

/// 把 [`ExecResult`] 组织成给 LLM 读的文本 + 给前端渲染的结构化数据。
fn format_exec(r: &ExecResult) -> ToolOutput {
    let mut text = String::new();
    if r.timed_out {
        text.push_str("[命令超时，已中断，以下为部分输出]\n");
    }
    if !r.stdout.is_empty() {
        text.push_str(&r.stdout);
        if !r.stdout.ends_with('\n') {
            text.push('\n');
        }
    }
    if !r.stderr.is_empty() {
        text.push_str("[stderr]\n");
        text.push_str(&r.stderr);
        if !r.stderr.ends_with('\n') {
            text.push('\n');
        }
    }
    match r.exit_code {
        Some(0) => {}
        Some(c) => text.push_str(&format!("[退出码 {c}]\n")),
        None => text.push_str("[无退出码：连接可能在命令结束前中断]\n"),
    }
    if r.truncated {
        text.push_str("[输出过长，已按上限截断]\n");
    }
    if text.trim().is_empty() {
        text.push_str("（命令无输出）");
    }
    ToolOutput::with_data(
        text,
        json!({
            "stdout": r.stdout,
            "stderr": r.stderr,
            "exitCode": r.exit_code,
            "timedOut": r.timed_out,
            "truncated": r.truncated,
            "durationMs": r.duration_ms,
        }),
    )
}

/// 目标机环境探测脚本（**固定常量**、纯只读，故不经 PolicyEngine；`get_session_info` 与
/// 首问预探测 `probe_session_env` 共用，系统信息卡片 `commands::system::system_info` 也共用）。
/// 采 OS/内核/架构/发行版/shell/当前目录 + 权限(root/sudo)/包管理器/init；另采
/// hostname/uptime/cpu_model/cpu_cores/mem/disk/ip 给 UI 卡片看（主机名/IP 在卡片里可见是
/// 产品需要，与 AI 侧「不泄露主机标识」不矛盾——`SessionEnv::parse` 不读这 9 个新键，
/// 【目标环境】提示词块保持不泄露，见 context.rs）。
///
/// 行式 `printf 'key=value\n'` 输出，解析器按白名单逐行取，未知键忽略（SessionEnv 与新
/// SystemInfo 从同一行集里各自投影，新加行不破坏旧侧）。
pub(crate) const SESSION_PROBE: &str = concat!(
    "printf 'os=%s\\n' \"$(uname -s 2>/dev/null)\"; ",
    "printf 'kernel=%s\\n' \"$(uname -r 2>/dev/null)\"; ",
    "printf 'arch=%s\\n' \"$(uname -m 2>/dev/null)\"; ",
    "if [ -r /etc/os-release ]; then . /etc/os-release && printf 'distro=%s\\n' \"$PRETTY_NAME\"; fi; ",
    "printf 'shell=%s\\n' \"${SHELL:-unknown}\"; ",
    "printf 'cwd=%s\\n' \"$(pwd 2>/dev/null)\"; ",
    // 权限：是否 root + 有无 sudo（都是非标识性元数据，决定要不要给提权命令）。
    "if [ \"$(id -u 2>/dev/null)\" = \"0\" ]; then printf 'root=yes\\n'; else printf 'root=no\\n'; fi; ",
    "if command -v sudo >/dev/null 2>&1; then printf 'sudo=yes\\n'; else printf 'sudo=no\\n'; fi; ",
    // 包管理器：取首个存在的（顺序兼顾 Debian/Fedora/RHEL/Alpine/Arch/openSUSE；dnf 先于 yum，
    // 因新版上 yum 常是 dnf 的软链）。
    "for p in apt-get dnf yum apk pacman zypper; do if command -v \"$p\" >/dev/null 2>&1; then printf 'pkg=%s\\n' \"$p\"; break; fi; done; ",
    // init 系统：systemd 优先（/run/systemd/system 目录或 systemctl 存在），回落 sysvinit 的 service。
    "if [ -d /run/systemd/system ] || command -v systemctl >/dev/null 2>&1; then printf 'init=systemd\\n'; elif command -v service >/dev/null 2>&1; then printf 'init=sysvinit\\n'; fi; ",
    // 以下 9 行只给系统信息卡片（commands::system::system_info）看；SessionEnv 不解析。
    // 采不到不阻断——每行都 `2>/dev/null` 吞错、缺位时输出为空，解析器自然跳过。
    "printf 'hostname=%s\\n' \"$(hostname 2>/dev/null)\"; ",
    "printf 'uptime_sec=%s\\n' \"$(awk '{print int($1)}' /proc/uptime 2>/dev/null)\"; ",
    "printf 'cpu_model=%s\\n' \"$(awk -F: '/model name/{print $2; exit}' /proc/cpuinfo 2>/dev/null | sed 's/^ //')\"; ",
    "printf 'cpu_cores=%s\\n' \"$(nproc 2>/dev/null)\"; ",
    "printf 'mem_total_kb=%s\\n' \"$(awk '/^MemTotal:/{print $2}' /proc/meminfo 2>/dev/null)\"; ",
    "printf 'mem_avail_kb=%s\\n' \"$(awk '/^MemAvailable:/{print $2}' /proc/meminfo 2>/dev/null)\"; ",
    "printf 'disk_total_kb=%s\\n' \"$(df -kP / 2>/dev/null | awk 'NR==2{print $2}')\"; ",
    "printf 'disk_used_kb=%s\\n' \"$(df -kP / 2>/dev/null | awk 'NR==2{print $3}')\"; ",
    "printf 'ip=%s\\n' \"$(ip -4 route get 1.1.1.1 2>/dev/null | awk '/src/{for(i=1;i<NF;i++)if($i==\"src\")print $(i+1)}' | head -1)\"",
);

/// 本地会话的 Windows 探测（替换 `SESSION_PROBE` 的 uname 那套）：
/// 采 OS/版本/架构/shell/当前目录 + 主机名/uptime/CPU/内存/磁盘/IP（卡片用，AI 侧不采）。
pub(crate) const SESSION_PROBE_WIN: &str = concat!(
    "\"os=Windows\"; ",
    "\"kernel=\" + [System.Environment]::OSVersion.Version.ToString(); ",
    "\"arch=\" + $env:PROCESSOR_ARCHITECTURE; ",
    "\"distro=\" + (Get-CimInstance Win32_OperatingSystem).Caption; ",
    "\"shell=\" + $PSVersionTable.PSVersion.ToString(); ",
    "\"cwd=\" + (Get-Location).Path; ",
    "\"hostname=\" + $env:COMPUTERNAME; ",
    "$os = Get-CimInstance Win32_OperatingSystem; ",
    "\"uptime_sec=\" + [int]((Get-Date) - $os.LastBootUpTime).TotalSeconds; ",
    "\"cpu_model=\" + (Get-CimInstance Win32_Processor | Select-Object -First 1).Name.Trim(); ",
    "\"cpu_cores=\" + [Environment]::ProcessorCount; ",
    "\"mem_total_kb=\" + $os.TotalVisibleMemorySize; ",
    "\"mem_avail_kb=\" + $os.FreePhysicalMemory; ",
    "$c = Get-CimInstance Win32_LogicalDisk -Filter \"DeviceID='C:'\"; ",
    "\"disk_total_kb=\" + [long]($c.Size/1024); ",
    "\"disk_used_kb=\" + [long](($c.Size - $c.FreeSpace)/1024); ",
    "\"ip=\" + (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.PrefixOrigin -ne 'WellKnown' } | Select-Object -First 1).IPAddress",
);

/// `get_session_info`：读取目标机 OS/发行版/shell/架构/当前目录（喂给系统提示词）。
pub struct GetSessionInfo;

#[async_trait]
impl Tool for GetSessionInfo {
    fn name(&self) -> &str {
        "get_session_info"
    }

    /// 本质是在会话上跑一条 [`SESSION_PROBE`] 探测命令 → `EXEC`。
    fn requires(&self) -> Caps {
        Caps::EXEC
    }

    fn description(&self) -> &str {
        "获取目标服务器的操作系统、内核、架构、发行版、shell 与当前工作目录（已排除主机名/IP 等标识）。用于给出与目标环境匹配的命令。只读。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {}, "required": [] })
    }

    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        policy.verdict_for(
            RiskLevel::ReadOnly,
            Reason::plain("pol.toolEnv", "读取目标机环境信息（固定只读探测）"),
        )
    }

    async fn invoke(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput, AppError> {
        // 本地会话在 Windows 上没有 uname：换 PowerShell 原生探测（纯本机，无主机标识）
        let probe = match ctx.app.state::<AppState>().sessions.kind(&ctx.session_id) {
            Some(SessionKind::Local) if cfg!(windows) => SESSION_PROBE_WIN,
            _ => SESSION_PROBE,
        };
        let r = exec::exec_command(&ctx.app, &ctx.session_id, probe, 10_000).await?;
        let text = r.stdout.trim();
        Ok(ToolOutput::text(if text.is_empty() {
            "（未能获取环境信息）"
        } else {
            text
        }))
    }
}

/// `list_services`：列出目标机服务（Linux systemd / SysV 降级、macOS launchctl，复用服务面板同一采样核心）。
/// 只读；启停动作不单独建工具——模型走 `run_command`，由内置策略分级弹审批。
pub struct ListServices;

#[async_trait]
impl Tool for ListServices {
    fn name(&self) -> &str {
        "list_services"
    }

    /// 采样走 exec 通道（与 [`GetSessionInfo`] 同档）；FTP/Telnet/串口不下发。
    fn requires(&self) -> Caps {
        Caps::EXEC
    }

    fn description(&self) -> &str {
        "列出目标机上的服务及其状态（Linux：单元名、加载/激活/子状态、自启、主 PID；macOS：launchd 作业的标签/状态/PID）。Linux systemd 机器全量，非 systemd 降级为 /etc/init.d 最小集；macOS 走 launchctl list。只读；启停服务请用 run_command。"
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {}, "required": [] })
    }

    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        policy.verdict_for(
            RiskLevel::ReadOnly,
            Reason::plain("pol.toolSvc", "列出目标机服务（固定只读采样）"),
        )
    }

    async fn invoke(&self, ctx: &ToolCtx, _args: Value) -> Result<ToolOutput, AppError> {
        let svcs = crate::commands::service::sample(&ctx.app, &ctx.session_id).await?;
        let running = svcs.iter().filter(|s| s.state == "active").count();
        let mut text = format!("共 {} 个服务，{} 个运行中\n", svcs.len(), running);
        for s in &svcs {
            text.push_str(&format!(
                "{}  {} {} {}\n",
                s.name, s.state, s.sub, s.enabled
            ));
        }
        Ok(ToolOutput::with_data(
            text,
            serde_json::to_value(&svcs).unwrap_or(Value::Null),
        ))
    }
}

/// 首问预探测的超时（毫秒）：比 `get_session_info` 的 10s 更短——预探测只是「锦上添花」，
/// 失败/超时不阻断本轮，直接落回系统提示的占位语（模型仍可按需自调 `get_session_info`）。
const ENV_PROBE_TIMEOUT_MS: u64 = 4_000;

/// 预探测目标机环境并解析为 [`SessionEnv`]，供系统提示词【目标环境】块**开局预填**，
/// 省掉模型为摸环境（要不要 sudo、哪个包管理器、systemctl 还是 service）浪费的一轮工具往返。
///
/// 与 [`GetSessionInfo`] 复用同一 `SESSION_PROBE`（本地 Windows 换 WIN 版），但：短超时、
/// 失败/空返回 `None`（调用方据此落回占位，绝不报错或阻断）。仅对走 exec 通道（SSH/本地）
/// 的会话有效；FTP/Telnet/Serial/Mosh 无 exec 通道，`exec_command` 会直接失败返回 `None`。
pub async fn probe_session_env(app: &AppHandle, session_id: &str) -> Option<SessionEnv> {
    let probe = match app.state::<AppState>().sessions.kind(session_id) {
        Some(SessionKind::Local) if cfg!(windows) => SESSION_PROBE_WIN,
        _ => SESSION_PROBE,
    };
    let r = exec::exec_command(app, session_id, probe, ENV_PROBE_TIMEOUT_MS)
        .await
        .ok()?;
    let env = SessionEnv::parse(r.stdout.trim());
    if env.is_empty() {
        None
    } else {
        Some(env)
    }
}

/// `get_terminal_snapshot`：取会话终端最近输出（喂系统提示词，数据源是 c3s 的终端环形缓冲）。
pub struct GetTerminalSnapshot;

#[async_trait]
impl Tool for GetTerminalSnapshot {
    fn name(&self) -> &str {
        "get_terminal_snapshot"
    }

    /// 读的是终端环形缓冲（不是 exec 通道）→ `PTY`：FTP 会话没有 ring，以前总是回
    /// 「（终端暂无输出）」白占一轮往返；现在整轮都不下发。Telnet/串口有终端，因此可用。
    fn requires(&self) -> Caps {
        Caps::PTY
    }

    fn description(&self) -> &str {
        "获取当前会话终端最近的输出快照（已去除 ANSI 颜色/光标控制符，有字节上限）。用于了解用户刚看到或刚执行的命令及其输出，从而给出贴合当前上下文的判断。只读。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "max_bytes": { "type": "integer", "description": "最多返回的字节数，默认 16384，上限 65536" },
            },
            "required": [],
        })
    }

    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        policy.verdict_for(
            RiskLevel::ReadOnly,
            Reason::plain("pol.toolSnap", "读取终端输出快照（只读）"),
        )
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        let want = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_SNAPSHOT_BYTES) as usize;
        let max = want.clamp(1, MAX_SNAPSHOT_BYTES);
        let ring = ctx
            .app
            .state::<AppState>()
            .sessions
            .ring(&ctx.session_id)
            .ok_or_else(|| AppError::NoSuchSession(ctx.session_id.clone()))?;
        // ring 存的是终端原始字节（含 ANSI），喂模型前先降噪
        let cleaned = strip_ansi(&ring.snapshot(max));
        let text = cleaned.trim();
        Ok(ToolOutput::text(if text.is_empty() {
            "（终端暂无输出）"
        } else {
            text
        }))
    }
}

/// 出网闸门：与 AI 同一套 `network_enabled`（关=禁止任何出网工具）。
fn require_egress(ctx: &ToolCtx) -> Result<(), AppError> {
    let st = ctx.app.state::<AppState>();
    let s = crate::commands::settings::load_ai_settings(&st.db)?;
    s.check_egress(crate::ai::providers::ProviderKind::Openai)
        .map_err(|_| AppError::Ai("出网已关闭，无法进行网页查询".into()))
}

/// 网页搜索（只读出网）：DuckDuckGo HTML，返回标题+摘要列表。
struct WebSearch;

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }

    /// 出网工具，与目标会话无关（能不能用只看 `network_enabled` 闸，见 [`require_egress`]）。
    fn requires(&self) -> Caps {
        Caps::NONE
    }

    fn description(&self) -> &str {
        "在互联网搜索资料（只读）。用于查文档、报错、软件用法、版本差异等。返回若干条标题+摘要。需要出网（AI 出网开关开启时才可用）。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "搜索关键词，建议用英文技术词" },
                "max_results": { "type": "integer", "description": "最多条数，默认 5，上限 10" },
            },
            "required": ["query"],
        })
    }

    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        policy.verdict_for(
            RiskLevel::ReadOnly,
            Reason::plain("pol.toolWebSearch", "网页搜索（只读出网）"),
        )
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        require_egress(ctx)?;
        let q = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if q.is_empty() {
            return Err(AppError::Invalid("web_search 缺少 query".into()));
        }
        let max = args
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .clamp(1, 10) as usize;
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}",
            urlencoding_lite(q)
        );
        let body = http_get(&url).await?;
        let hits = parse_ddg_hits(&body, max);
        if hits.is_empty() {
            return Ok(ToolOutput::text("（无搜索结果）"));
        }
        let mut out = String::new();
        for (i, (t, s)) in hits.iter().enumerate() {
            out.push_str(&format!("{}. {}\n   {}\n", i + 1, t, s));
        }
        Ok(ToolOutput::text(out))
    }
}

/// 抓取网页正文（只读出网）：去 HTML 标签后返回摘要文本。
struct FetchUrl;

#[async_trait]
impl Tool for FetchUrl {
    fn name(&self) -> &str {
        "fetch_url"
    }

    /// 同上：只出网，不碰会话通路。
    fn requires(&self) -> Caps {
        Caps::NONE
    }

    fn description(&self) -> &str {
        "抓取一个 http(s) 网页并返回正文摘要（只读）。用于读文档、Issue、博客等。需要出网。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "http/https 地址" },
                "max_chars": { "type": "integer", "description": "返回字符数上限，默认 4000" },
            },
            "required": ["url"],
        })
    }

    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        policy.verdict_for(
            RiskLevel::ReadOnly,
            Reason::plain("pol.toolWebFetch", "抓取网页（只读出网）"),
        )
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        require_egress(ctx)?;
        let url = args
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(AppError::Invalid("url 必须以 http(s):// 开头".into()));
        }
        let max = args
            .get("max_chars")
            .and_then(Value::as_u64)
            .unwrap_or(4000)
            .clamp(200, 20_000) as usize;
        let body = http_get(&url).await?;
        let text = html_to_text(&body);
        let cut = text.chars().take(max).collect::<String>();
        Ok(ToolOutput::text(if text.len() > max {
            format!("{cut}\n…（已截断）")
        } else {
            cut
        }))
    }
}

async fn http_get(url: &str) -> Result<String, AppError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("SailTerm/0.1")
        .build()
        .map_err(|e| AppError::Ai(format!("HTTP 客户端失败：{e}")))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Ai(format!("请求失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Ai(format!("HTTP {}", resp.status())));
    }
    resp.text()
        .await
        .map_err(|e| AppError::Ai(format!("读取响应失败：{e}")))
}

/// 极简 URL 编码（查询词）。
fn urlencoding_lite(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 从 DDG html 结果里抠标题/摘要（够用即可，不追求完美解析）。
fn parse_ddg_hits(html: &str, max: usize) -> Vec<(String, String)> {
    let mut hits = Vec::new();
    let mut rest = html;
    while hits.len() < max {
        let Some(i) = rest.find("result__a") else {
            break;
        };
        let slice = &rest[i..];
        let title = first_tag_text(slice, "result__a");
        let snip = first_tag_text(slice, "result__snippet");
        rest = &rest[i + 10..];
        if !title.is_empty() {
            hits.push((title, snip));
        }
    }
    hits
}

fn first_tag_text(s: &str, class: &str) -> String {
    let Some(i) = s.find(class) else {
        return String::new();
    };
    let after = &s[i..];
    let Some(gt) = after.find('>') else {
        return String::new();
    };
    let body = &after[gt + 1..];
    let end = body.find("</").unwrap_or(body.len().min(400));
    html_to_text(&body[..end])
        .trim()
        .chars()
        .take(200)
        .collect()
}

fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 去除 ANSI 转义序列与不可打印控制符，只保留可读文本 + 换行/制表（喂 LLM 前降噪）。
///
/// 环形缓冲存的是终端原始字节（含颜色/光标控制，前端 xterm 渲染需要）；直接喂模型会浪费
/// token 且干扰理解，故在此剥离 CSI（`ESC [ … 终止符`）、OSC（`ESC ] … BEL/ST`）与常见单/双
/// 字符转义，其余 C0 控制符（含 `\r`：终端 `\r\n` 只留 `\n`）与 DEL 一并丢弃。
pub(crate) fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut it = input.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            match it.next() {
                // CSI：参数字节 0x30–0x3F、中间字节 0x20–0x2F，遇终止字节 0x40–0x7E 结束
                Some('[') => {
                    for c2 in it.by_ref() {
                        if ('\x40'..='\x7e').contains(&c2) {
                            break;
                        }
                    }
                }
                // OSC：直到 BEL(\x07)，或 ST(ESC \)
                Some(']') => {
                    while let Some(c2) = it.next() {
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' {
                            if it.peek() == Some(&'\\') {
                                let _ = it.next();
                            }
                            break;
                        }
                    }
                }
                // 字符集指定等双字符转义（ESC ( B 之类）：再吞一个
                Some('(' | ')' | '#' | '%') => {
                    let _ = it.next();
                }
                // 其余单字符转义（ESC = / ESC > 等）：已随上面的 it.next() 消费
                _ => {}
            }
        } else if c == '\n' || c == '\t' {
            out.push(c); // 换行/制表保留
        } else if c >= '\x20' && c != '\x7f' {
            out.push(c); // 其余可打印字符（含 Unicode）保留；C0 控制符与 DEL 丢弃
        }
    }
    out
}

/// `list_dir`：列远端目录（复用 的统一文件后端，SSH-SFTP / FTP 皆可）。
pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }

    /// 走 [`crate::files::backend`] 的统一文件抽象（SSH-SFTP / FTP 都接），因此 FTP 会话本来就能用。
    fn requires(&self) -> Caps {
        Caps::FS
    }

    fn description(&self) -> &str {
        "列出远端目录下的条目（名称/类型/大小/权限/修改时间）。用于浏览文件系统结构。只读。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "远端目录路径，缺省为当前目录" },
            },
            "required": ["path"],
        })
    }

    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        policy.verdict_for(
            RiskLevel::ReadOnly,
            Reason::plain("pol.toolListDir", "列目录（只读）"),
        )
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        let raw = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .trim();
        let path = if raw.is_empty() { "." } else { raw };
        let backend = files::backend(&ctx.app, &ctx.session_id).await?;
        // SFTP 通道出错即丢弃缓存，下次重开（自愈，与文件命令层一致）
        let entries = files::forget_on_error(&ctx.app, &ctx.session_id, backend.list(path).await)?;

        let total = entries.len();
        let shown: Vec<&FileEntry> = entries.iter().take(MAX_LIST_ENTRIES).collect();
        let mut text = if shown.is_empty() {
            format!("{path} 是空目录")
        } else {
            shown
                .iter()
                .map(|e| fmt_entry(e))
                .collect::<Vec<_>>()
                .join("\n")
        };
        if total > MAX_LIST_ENTRIES {
            text.push_str(&format!(
                "\n[仅显示前 {MAX_LIST_ENTRIES} 项，共 {total} 项，请缩小路径范围]"
            ));
        }
        let data: Vec<Value> = shown
            .iter()
            .map(|e| {
                json!({
                    "name": e.name,
                    "path": e.path,
                    "isDir": e.is_dir,
                    "isSymlink": e.is_symlink,
                    "size": e.size,
                    "perms": e.perms,
                    "modifiedMs": e.modified_ms,
                })
            })
            .collect();
        Ok(ToolOutput::with_data(text, json!(data)))
    }
}

/// 目录项 → `ls -l` 风格单行（类型 + 权限 + 大小 + 名称）。
fn fmt_entry(e: &FileEntry) -> String {
    let kind = if e.is_dir {
        "dir "
    } else if e.is_symlink {
        "link"
    } else {
        "file"
    };
    let perms = e.perms.as_deref().unwrap_or("-");
    format!("{kind} {perms:>10} {:>12}  {}", e.size, e.name)
}

/// `read_file`：读远端文本文件（经 exec `cat`，有字节上限）。
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    /// 同 [`ListDir`]：统一文件后端，FTP 可用。
    fn requires(&self) -> Caps {
        Caps::FS
    }

    fn description(&self) -> &str {
        "读取远端文本文件的内容（有字节上限，超出截断）。用于查看配置、日志片段。只读；二进制文件可能产生乱码。\
         SSH 与 FTP 会话可用；本地会话请改用 run_command（cat / type）。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "远端文件路径" },
                "max_bytes": { "type": "integer", "description": "最多读取字节数，默认 65536" },
            },
            "required": ["path"],
        })
    }

    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        policy.verdict_for(
            RiskLevel::ReadOnly,
            Reason::plain("pol.toolReadFile", "读取文件内容（只读）"),
        )
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::Invalid("read_file 缺少 path 参数".into()))?;
        let max = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_READ_BYTES)
            .clamp(1, MAX_READ_BYTES);
        // FTP/FTPS 没有 exec 通道，`cat` 只会回「FTP 会话不支持执行 shell 命令」；
        // 但它的文件后端本来就提供 `read_text`——按能力分派，而不是让一个 requires(FS)
        // 的工具在 FTP 上必然失败（模型每次调都得吃一条[工具执行失败]）。
        let has_exec = ctx
            .app
            .state::<AppState>()
            .sessions
            .kind(&ctx.session_id)
            .is_some_and(|k| k.caps().contains(Caps::EXEC));
        if !has_exec {
            let ft = match files::backend(&ctx.app, &ctx.session_id).await {
                Ok(ft) => ft,
                // 与 cat 失败同口径：回可读文本而非硬报错，让模型接着换别的路
                Err(e) => return Ok(ToolOutput::text(format!("读取失败：{e}"))),
            };
            let (mut text, size) = match ft.read_text(path, max).await {
                Ok(v) => v,
                Err(e) => return Ok(ToolOutput::text(format!("读取失败：{e}"))),
            };
            if size > text.len() as u64 || text.len() as u64 >= max {
                text.push_str(&format!("\n[文件较大，仅返回前 {} 字节]", text.len()));
            }
            return Ok(ToolOutput::text(text));
        }
        // 路径用单引号转义，防止把 `; rm -rf /` 之类拼进命令（read_file 静态只读，不经 policy）
        let cmd = format!("cat -- {}", shell_quote(path));
        let r = exec::exec_command(&ctx.app, &ctx.session_id, &cmd, 15_000).await?;
        // cat 失败（不存在/无权限/是目录）：把 stderr 作为可读结果回给模型，而非硬报错
        if r.exit_code != Some(0) && r.stdout.is_empty() {
            let why = r.stderr.trim();
            return Ok(ToolOutput::text(if why.is_empty() {
                format!("读取失败：{path}（退出码 {:?}）", r.exit_code)
            } else {
                format!("读取失败：{why}")
            }));
        }
        let original_len = r.stdout.len();
        let mut text = r.stdout;
        truncate_to_bytes(&mut text, max);
        if r.truncated || text.len() < original_len {
            text.push_str(&format!("\n[文件较大，仅返回前 {} 字节]", text.len()));
        }
        Ok(ToolOutput::text(text))
    }
}

/// `read_attachment`：按需取回一个已归档附件的抽取正文片段。
///
/// 为什么必须有它：三档投送为了控体积必然截断，而「截断了就没了」会把大文件切成不可用。
/// 附件正文需要按需可取：先给目录/大纲，正文按需取回——本工具就是那个「按需取」的口。
///
/// **只读盘上的抽取产物**（`attachments/<id>.md`）：不重读原件、不出网、不接受任意路径。
/// 所以 `requires()` 是 `Caps::NONE`（与会话通路无关）、风险级只读不过审批；但 id 走
/// [`attach::is_safe_id`] 闸门，拼不出穿越路径。脱敏两道：落盘时一次、回填模型前编排层再一次。
pub struct ReadAttachment;

#[async_trait]
impl Tool for ReadAttachment {
    fn name(&self) -> &str {
        "read_attachment"
    }

    /// 读的是本机数据目录里的归档文本，不碰任何会话通路 → 无门槛（与 `write_plan` 同理）。
    fn requires(&self) -> Caps {
        Caps::NONE
    }

    fn description(&self) -> &str {
        "按附件 id 取回用户附件已抽取的正文片段（日志/配置/CSV 等已转成文本的附件）。
当消息里的 <<<UNTRUSTED_FILE>>> 块只给了大纲或头尾采样、而你需要更完整内容时才用。
id 取自标记里的 id；可选 offset_lines（起始行，从 0 计）与 max_bytes（默认 65536）。
只能读已归档的附件，不能读本机任意路径；图片附件无正文，本工具读不到。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "附件 id（来自 UNTRUSTED_FILE 标记头）" },
                "offset_lines": { "type": "integer", "description": "从第几行开始，默认 0" },
                "max_bytes": { "type": "integer", "description": "最多返回字节数，默认 65536" },
            },
            "required": ["id"],
        })
    }

    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        policy.verdict_for(
            RiskLevel::ReadOnly,
            Reason::plain("pol.toolReadAttach", "读取已归档附件的抽取文本（只读）"),
        )
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        let id = args
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if !attach::is_safe_id(id) {
            return Err(AppError::Invalid("read_attachment 缺少合法 id".into()));
        }
        let offset = args
            .get("offset_lines")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let max = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_READ_BYTES)
            .clamp(1, MAX_READ_BYTES) as usize;
        let ir =
            match attach::read_ir(&ctx.app.state::<AppState>().data_dir, id) {
                Some(t) => t,
                // 没产物也要回一句可读的话（而非硬报错）：模型据此知道该改问用户或换个附件，
                // 而不是收到一条「[工具执行失败]」后反复重试同一个 id。
                None => return Ok(ToolOutput::text(
                    "附件无已归档的抽取文本（可能是图片、二进制、未支持的格式，或归档已被删除）",
                )),
            };
        // 按行切而不是按字节硬切：模型拿到的应是完整记录，中途截一行会产生「看不完上半行」的误读。
        let lines: Vec<&str> = ir.lines().collect();
        let total = lines.len();
        if offset >= total {
            return Ok(ToolOutput::text(format!(
                "偏移超出范围：本附件共 {total} 行"
            )));
        }
        let mut out = String::new();
        let mut n = offset;
        while n < total {
            let len = lines[n].len();
            if out.len() + len > max {
                break;
            }
            out.push_str(lines[n]);
            out.push('\n');
            n += 1;
        }
        if n < total {
            out.push_str(&format!(
                "\n……（已给至第 {n} 行 / 共 {total} 行，继续可取 offset_lines={n}）"
            ));
        } else {
            out.push_str(&format!("（已是末尾：共 {total} 行）"));
        }
        Ok(ToolOutput::text(out))
    }
}

/// `query_knowledge` 回填给 LLM 的命中条数上限：语料条目短，3 条足够覆盖一个问题面；
/// 再多会挤占上下文，且低分命中的噪声大于价值。
const KNOWLEDGE_TOP_K: usize = 3;

/// `query_knowledge`：查 SailTerm 本应用自身的知识库（内置帮助语料，见 [`crate::ai::knowledge`]）。
///
/// 内置技能（SkillSource::Knowledge）：不碰会话通路也不出网 → `Caps::NONE`，任意会话下发。
/// 存在动机：模型对「客户端」的训练先验指向其他 FTP/终端工具，不给它自查入口，它会拿
/// 别人的功能回答本应用的问题。
pub struct QueryKnowledge;

#[async_trait]
impl Tool for QueryKnowledge {
    fn name(&self) -> &str {
        "query_knowledge"
    }

    /// 读的是编译期内嵌的语料常量，不碰任何会话通路 → 无门槛（与 `read_attachment` 同理）。
    fn requires(&self) -> Caps {
        Caps::NONE
    }

    fn description(&self) -> &str {
        "查 SailTerm 本应用自身的功能、用法、记录位置、能力边界（内置知识库，只读、离线）。\
用户问「在哪能看到××」「××记录还查得到吗」「能不能××」这类关于本应用的问题时优先用本工具，\
不得凭训练先验把「客户端」指认为 FileZilla/WinSCP 等其他软件。本应用不记录文件删除事件，\
知识库无相关条目时如实告知现状，不得臆造。未命中时可改用 web_search 查公开资料。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "想查的问题，用自然语言描述即可（中文/英文均可）" },
            },
            "required": ["query"],
        })
    }

    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        policy.verdict_for(
            RiskLevel::ReadOnly,
            Reason::plain("pol.toolQueryKnowledge", "查询内置知识库（只读）"),
        )
    }

    async fn invoke(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        let q = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if q.is_empty() {
            return Err(AppError::Invalid("query_knowledge 缺少 query".into()));
        }
        let hits = crate::ai::knowledge::search(q, KNOWLEDGE_TOP_K);
        if hits.is_empty() {
            // 零命中回文本而非报错：模型需要知道「库里没这条」是事实，而不是换个词反复重试。
            return Ok(ToolOutput::text(
                "（知识库无相关条目。若确需外部资料可用 web_search；若是本应用尚无的功能，如实告知用户现状）",
            ));
        }
        let mut out = String::from("知识库命中（来源：SailTerm 内置帮助语料，按相关度降序）：\n");
        for (i, h) in hits.iter().enumerate() {
            out.push_str(&format!(
                "\n【{}】({}语料) {}\n{}\n",
                i + 1,
                if h.lang == "zh" { "中文" } else { "英文" },
                h.entry.q,
                h.entry.a
            ));
        }
        Ok(ToolOutput::text(out))
    }
}

/// 插件贡献的 AI 技能（`SkillSource::Plugin`）：把 manifest `aiTools` 的一条声明适配成 [`Tool`]。
///
/// 与内建工具的唯一区别：schema（name/description/parameters/requires）来自 manifest 运行期解析，
/// `invoke` 不碰会话通路而是加载本插件 wasm、经 [`crate::plugin::host::call_ai_tool`] 跑导出。
/// 安全不因此放松：`verdict` 按 manifest 静态 risk 走同一 PolicyEngine 审批链；wasm 在沙箱内带 fuel；
/// `ai_tool` 权限在 `call_ai_tool` 内硬校（防绕注册表直调）。
pub struct PluginTool {
    /// 归属插件 id（invoke 时据此从注册表取回 manifest + wasm 路径 + 生效设置）。
    plugin_id: String,
    /// manifest 里的技能声明（name/description/parameters/requires/risk/export/id）。
    def: PluginAiTool,
}

impl PluginTool {
    pub fn new(plugin_id: String, def: PluginAiTool) -> Self {
        Self { plugin_id, def }
    }
}

#[async_trait]
impl Tool for PluginTool {
    fn name(&self) -> &str {
        &self.def.name
    }

    /// 技能声明的档位（`none` = 与会话通路无关，多数技能如此）；脏值按 `NONE` 保守下发。
    fn requires(&self) -> Caps {
        if self.def.requires == "none" {
            Caps::NONE
        } else {
            Caps::from_name(&self.def.requires).unwrap_or(Caps::NONE)
        }
    }

    fn description(&self) -> &str {
        &self.def.description
    }

    fn parameters(&self) -> Value {
        self.def.parameters.clone()
    }

    /// 静态风险：`readonly` → 只读档（默认直通）；`confirm` → 写档（每次过审批卡）。
    /// 走 `policy.verdict_for` 而非硬编码 Decision，使「全审批」模式对插件技能同样生效。
    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        let risk = if self.def.risk == "confirm" {
            RiskLevel::Write
        } else {
            RiskLevel::ReadOnly
        };
        policy.verdict_for(
            risk,
            Reason::plain("pol.toolPlugin", "调用插件提供的 AI 技能"),
        )
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        // 逐次从注册表取回插件（而非缓存 manifest）：插件可能本轮被禁用/卸载，取活的真相。
        let st = ctx.app.state::<AppState>();
        let plugin = st.plugins.get(&self.plugin_id).ok_or_else(|| {
            AppError::Invalid(format!("插件不存在：{}", self.plugin_id))
        })?;
        if !plugin.enabled || !plugin.has_wasm {
            return Err(AppError::Conflict(format!(
                "插件 {} 已禁用或缺少 wasm，技能不可用",
                self.plugin_id
            )));
        }
        // 权限硬校（兵家必争）：即便注册表下发处漏筛，这里也不放无 ai_tool 权限的插件过。
        if !plugin.manifest.has_permission(Permission::AiTool) {
            return Err(AppError::Conflict(format!(
                "插件 {} 未声明 ai_tool 权限",
                self.plugin_id
            )));
        }
        let dir = st.plugins.plugin_dir(&self.plugin_id);
        let wasm_path = dir.join(plugin.manifest.wasm.as_deref().unwrap_or("plugin.wasm"));
        let bytes = std::fs::read(&wasm_path)
            .map_err(|e| AppError::Io(format!("读取插件 wasm 失败：{e}")))?;
        let settings = st.plugins.effective_settings(&ctx.app, &self.plugin_id)?;
        // 入参按 LLM 给的 JSON object 原样序列化传给导出；缺省补空对象防 null 崩解析。
        let args_json = serde_json::to_string(&args).unwrap_or_else(|_| "{}".into());
        // 沙箱内跑导出（同步 wasmi，fuel 限额兜底失控）：AI 工具执行路径非终端热路径，可接受。
        let raw = host::call_ai_tool(
            &plugin.manifest,
            &bytes,
            &self.def.export,
            &self.def.name,
            &args_json,
            settings,
        )?;
        interpret_skill_output(&self.def.name, &raw)
    }
}

/// 把插件技能回传的字符串解释成 [`ToolOutput`]：约定形状
/// `{"ok":true,"text":...,"data":...}` 或 `{"error":...}`。
///
/// 宿主不担模型协议形状：解析失败/字段缺失一律降级为「把原文当文本回填」，插件即使不规范
/// 也不至于让本轮崩；`error`/`ok:false` 回**可读文本而非硬错误**，让模型据失败原因换路而非反复重试。
fn interpret_skill_output(name: &str, raw: &str) -> Result<ToolOutput, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(ToolOutput::text(format!("（技能 {name} 无输出）")));
    }
    let v: Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        // 非 JSON：原样当纯文本回填（比报错更可用），截断防超长撑爆上下文。
        Err(_) => {
            let mut s = trimmed.chars().take(4000).collect::<String>();
            if s.chars().count() < trimmed.chars().count() {
                s.push_str("…（已截断）");
            }
            return Ok(ToolOutput::text(s));
        }
    };
    if let Some(err) = v.get("error").and_then(Value::as_str) {
        return Ok(ToolOutput::text(format!("[技能 {name} 返回错误] {err}")));
    }
    if v.get("ok").and_then(Value::as_bool) == Some(false) {
        let msg = v
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("插件未说明失败原因");
        return Ok(ToolOutput::text(format!("[技能 {name} 执行失败] {msg}")));
    }
    let text = v
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| trimmed.to_string());
    Ok(ToolOutput {
        text,
        data: v.get("data").cloned(),
    })
}

/// 从当前启用的插件生成 AI 技能工具列表（供 [`ToolRegistry::assemble`] 单点合成）。
///
/// 过滤链：已安装 → 启用 → 有 wasm → 声明 `ai_tool` 权限 → 单条技能未被 UI 停用。
/// 权限/启用在此筛一道是「少发 schema」的优化；`call_ai_tool` 内还会再硬校权限（防绕注册表直调）。
/// 停用开关走插件隔离设置命名空间：键 `skill_off:<技能id>` 取值 `1/true/yes/on` 即本轮不下发。
pub fn plugin_tools(app: &AppHandle) -> Vec<Arc<dyn Tool>> {
    let st = app.state::<AppState>();
    let mut out: Vec<Arc<dyn Tool>> = Vec::new();
    for p in st.plugins.list() {
        if !p.enabled || !p.has_wasm || !p.manifest.has_permission(Permission::AiTool) {
            continue;
        }
        if p.manifest.ai_tools.is_empty() {
            continue;
        }
        let settings = st
            .plugins
            .effective_settings(app, &p.manifest.id)
            .unwrap_or_default();
        for t in &p.manifest.ai_tools {
            let off = settings
                .get(&format!("skill_off:{}", t.id))
                .map(|v| {
                    matches!(
                        v.trim().to_ascii_lowercase().as_str(),
                        "1" | "true" | "yes" | "on"
                    )
                })
                .unwrap_or(false);
            if off {
                continue;
            }
            out.push(Arc::new(PluginTool::new(p.manifest.id.clone(), t.clone())));
        }
    }
    out
}

/// `transfer_file` 的方向归一化：取 `direction` 参数去空白转小写（缺失/脏值回空串）。
/// verdict 与 invoke 共用，避免两处各自 `get/trim/lowercase` 漂移出口径。
fn direction_of(args: &Value) -> String {
    args.get("direction")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default()
}

/// `transfer_file`：在本地与远端之间传一个文件/目录（上传或下载）。
///
/// 不另写一套搬运：直接复用 [`crate::commands::transfer`] 的 `run_transfer`（与前端拖拽
/// 传输同一入口），共享传输队列的断点续传 / 并发闸门 / 进度事件；工具只多叠一层「等终态」。
pub struct TransferFile;

#[async_trait]
impl Tool for TransferFile {
    fn name(&self) -> &str {
        "transfer_file"
    }

    /// [`run_transfer`] 与文件命令层共用同一条后端通路（FTP 的上传/下载正是它的主用法）。
    fn requires(&self) -> Caps {
        Caps::FS
    }

    fn description(&self) -> &str {
        "在本地机器与目标服务器之间传一个文件或目录。direction=upload 把本地文件（path）传到远端目录（destDir）；\
         direction=download 把远端文件（path）下载到本地目录（destDir，缺省弹框让用户选保存位置）。上传会改动远端文件系统，\
         需用户批准后才执行；下载对服务器只读，自动执行。绝不接受任何凭证参数，只复用已建立的会话。目标目录不存在会报错；大文件可能超时，可稍后再查进度。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "direction": {
                    "type": "string",
                    "enum": ["upload", "download"],
                    "description": "upload=本地→远端；download=远端→本地",
                },
                "path": {
                    "type": "string",
                    "description": "源文件路径：上传为本地绝对路径，下载为远端绝对路径",
                },
                "destDir": {
                    "type": "string",
                    "description": "目标目录：上传为远端目录，下载为本地目录；缺省 upload 用远端当前目录、download 弹框请用户选择保存位置",
                },
                "purpose": {
                    "type": "string",
                    "description": "一句话说明传输目的（给用户看的授权理由），≤ 40 字，例如「下发修正后的 nginx.conf」",
                },
            },
            "required": ["direction", "path", "purpose"],
        })
    }

    // 按方向分流风险：上传写远端（改动被保护的服务器资产）→ Write 需审批；
    // 下载对服务器只是读、仅往本机落一个文件，敏感度不高于已免审批的 read_file → ReadOnly 自动执行。
    // 全审批档（approve_all）仍由 PolicyEngine 兜底，只读也需批准，安全不被绕过。
    fn verdict(&self, args: &Value, policy: &PolicyEngine) -> Verdict {
        match direction_of(args).as_str() {
            "download" => policy.verdict_for(
                RiskLevel::ReadOnly,
                Reason::plain("pol.toolDownload", "下载远端文件到本机（对服务器只读）"),
            ),
            _ => policy.verdict_for(
                RiskLevel::Write,
                Reason::plain("pol.toolUpload", "上传会改动远端文件系统，需审批"),
            ),
        }
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        let dir = direction_of(&args);
        let direction = match dir.as_str() {
            "upload" => Direction::Upload,
            "download" => Direction::Download,
            _ => {
                return Ok(ToolOutput::text("direction 必须为 upload 或 download"));
            }
        };
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::Invalid("transfer_file 缺少 path 参数（源文件路径）".into()))?
            .to_string();
        let dest_dir = args
            .get("destDir")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // 下载未指定本地目录：弹原生目录选择框让用户挑落点，不再静默丢进主目录
        //（避免「不知道下载到哪」）。取消则不发起，回文本让模型向用户说明。
        // 回调式非阻塞 + oneshot（同 commands/dialog.rs）：同步 blocking_pick 会卡主线程消息循环。
        let dest_dir = match (direction, dest_dir) {
            (Direction::Download, None) => {
                use tauri_plugin_dialog::DialogExt;
                let (tx, rx) = tokio::sync::oneshot::channel();
                // 标题由前端随轮下发（已本地化）；为空则不 set_title，回落系统默认标题。
                let mut picker = ctx.app.dialog().file();
                if !ctx.dl_pick_title.is_empty() {
                    picker = picker.set_title(ctx.dl_pick_title.clone());
                }
                picker.pick_folder(move |p| {
                    let _ = tx.send(p.map(|p| p.simplified().to_string()));
                });
                match rx.await.ok().flatten() {
                    Some(dir) if !dir.trim().is_empty() => Some(dir),
                    // 用户关掉选择框：任务没发起，比默默存到陌生位置更诚实
                    _ => {
                        return Ok(ToolOutput::with_data(
                            "用户取消了目录选择，未发起下载；如需继续，请重新调用并让用户选择保存目录",
                            json!({ "exitCode": 1 }),
                        ));
                    }
                }
            }
            (_, d) => d,
        };

        // run_transfer 已叠上「等终态」；进不了终态（超时）时它自己回可读文本，不取消任务。
        let text = run_transfer(
            ctx.app.clone(),
            ctx.session_id.clone(),
            direction,
            path,
            dest_dir,
            None,
        )
        .await?;
        // 传输失败/取消以 `[传输未完成]` 开头；回一个 exitCode 形状的信号供编排器判成败
        //（与 run_command 同层：编排器的 exec_ok 就是看 data.exitCode）。目录入队/skip 不算失败。
        let incomplete = text.starts_with("[传输未完成]");
        Ok(ToolOutput::with_data(
            text,
            json!({ "exitCode": if incomplete { 1 } else { 0 } }),
        ))
    }
}

/// `search_files`：按名字子串检索远端目录树（复用统一文件后端的递归列举）。
///
/// 是 `list` 命令层的有界递归：与 [`crate::commands::transfer`] 同一条后端通路，
/// 但额外限定深度 / 扫描数 / 命中数，避免 `find /` 这类调用把会话卡死。
pub struct SearchFiles;

#[async_trait]
impl Tool for SearchFiles {
    fn name(&self) -> &str {
        "search_files"
    }

    /// 同 [`ListDir`]：递归列举走的也是统一文件后端，FTP 可用。
    fn requires(&self) -> Caps {
        Caps::FS
    }

    fn description(&self) -> &str {
        "在远端目录下按文件名子串递归检索（大小写不敏感），返回完整路径/类型/大小/修改时间。\
         用于定位配置文件、日志、证书等待传文件。只读；有深度与命中数上限，命中过多请缩小目录。\
         SSH-SFTP 与 FTP 会话可用；本地终端会话请改用 run_command（find / dir）。"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "检索起始的远端目录，例如 /etc 或 /var/log" },
                "pattern": { "type": "string", "description": "文件名子串（大小写不敏感），例如 nginx.conf 或 error.log" },
                "max_results": { "type": "integer", "description": "最多返回条数，默认 50，上限 200" },
            },
            "required": ["path", "pattern"],
        })
    }

    fn verdict(&self, _args: &Value, policy: &PolicyEngine) -> Verdict {
        policy.verdict_for(
            RiskLevel::ReadOnly,
            Reason::plain("pol.toolFind", "递归列目录检索文件（只读）"),
        )
    }

    async fn invoke(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AppError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::Invalid("search_files 缺少 path 参数（要检索的远端目录）".into())
            })?;
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::Invalid("search_files 缺少 pattern 参数（文件名子串）".into())
            })?;
        let max = args
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(50)
            .clamp(1, SEARCH_MAX_HITS as u64) as usize;

        // 只有 SSH-SFTP / FTP 会话有统一文件后端；本地/Telnet/串口等无此通路。
        match ctx.app.state::<AppState>().sessions.kind(&ctx.session_id) {
            Some(SessionKind::Ssh) | Some(SessionKind::Ftp) | Some(SessionKind::Ftps) => {}
            Some(SessionKind::Local) => {
                return Ok(ToolOutput::text(
                    "search_files 只面向 SSH/FTP 会话；本地会话请用 run_command（find / dir）检索",
                ));
            }
            None => return Err(AppError::NoSuchSession(ctx.session_id.clone())),
            Some(other) => {
                return Ok(ToolOutput::text(format!(
                    "{} 会话没有文件系统，search_files 不适用",
                    other.as_str()
                )));
            }
        }
        let backend = files::backend(&ctx.app, &ctx.session_id).await?;
        let result = search_remote(&*backend, path, pattern, max).await;
        let (hits, scanned) = files::forget_on_error(&ctx.app, &ctx.session_id, result)?;

        if hits.is_empty() {
            return Ok(ToolOutput::text(format!(
                "{path} 下未找到名字含「{pattern}」的条目（已扫描 {scanned} 项，深度≤{SEARCH_MAX_DEPTH}）"
            )));
        }
        let mut text = hits.iter().map(fmt_entry).collect::<Vec<_>>().join("\n");
        let paths: Vec<Value> = hits
            .iter()
            .map(|e| {
                json!({
                    "name": e.name,
                    "path": e.path,
                    "isDir": e.is_dir,
                    "size": e.size,
                    "modifiedMs": e.modified_ms,
                })
            })
            .collect();
        if hits.len() == max {
            text.push_str(&format!(
                "\n[已达 {max} 条上限，可能还有更多；请缩小目录或加长关键词]"
            ));
        }
        Ok(ToolOutput::with_data(text, json!(paths)))
    }
}

/// 有界递归检索：返回 `(命中条目, 已扫描条目数)`。命中表按路径去重（符号链接可能重复触达）。
async fn search_remote(
    ft: &dyn files::FileTransfer,
    root: &str,
    pattern: &str,
    max: usize,
) -> Result<(Vec<FileEntry>, usize), AppError> {
    let needle = pattern.to_lowercase();
    let mut hits: BTreeMap<String, FileEntry> = BTreeMap::new();
    let (mut scanned, mut depth) = (0usize, 0usize);
    rec_search(ft, root, &needle, max, &mut hits, &mut scanned, &mut depth).await?;
    Ok((hits.into_values().collect(), scanned))
}

/// 单层遍历 + 递归下钻。`dir` 恒为完整路径，`depth` 充当下钻层数（超限即停）。
#[allow(clippy::type_complexity)] // 递归累加器（命中表/计数）一体传入，避免为一次性遍历造结构体
async fn rec_search(
    ft: &dyn files::FileTransfer,
    dir: &str,
    needle: &str,
    max: usize,
    hits: &mut BTreeMap<String, FileEntry>,
    scanned: &mut usize,
    depth: &mut usize,
) -> Result<(), AppError> {
    if *scanned >= SEARCH_SCAN_LIMIT || *depth >= SEARCH_MAX_DEPTH {
        return Ok(());
    }
    *depth += 1;
    let entries = ft.list(dir).await?;
    for e in entries {
        if e.name == "." || e.name == ".." {
            continue;
        }
        *scanned += 1;
        if *scanned >= SEARCH_SCAN_LIMIT {
            return Ok(());
        }
        // 优先用后端给出的完整路径（与 walk_remote_files 同一口径，避免拼接出错）
        let path = if e.path.contains('/') || e.path.contains('\\') {
            e.path.clone()
        } else if dir.ends_with('/') {
            format!("{dir}{}", e.name)
        } else {
            format!("{dir}/{}", e.name)
        };
        let is_root = path == dir;
        if !is_root && e.name.to_lowercase().contains(needle) && hits.len() < max {
            hits.entry(path.clone()).or_insert_with(|| FileEntry {
                path: path.clone(),
                ..e.clone()
            });
        }
        if e.is_dir && !is_root {
            Box::pin(rec_search(ft, &path, needle, max, hits, scanned, depth)).await?;
        }
    }
    Ok(())
}

/// 单引号包裹并转义内部单引号（`a'b` → `'a'\''b'`），与 `ssh/mod.rs` 的 startup_dir 一致。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 按字节上限截断字符串，落在 char 边界上（不切断多字节 UTF-8）。
fn truncate_to_bytes(s: &mut String, max: u64) {
    let max = max as usize;
    if s.len() <= max {
        return;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::policy::{Decision, ScanLevel};

    fn reg() -> ToolRegistry {
        ToolRegistry::with_builtins()
    }

    /// 能力表逐协议归属。这不是「凑数的快照测」：档位归属同时被前端
    /// `src/lib/capabilities.ts` 的 `CAPS` 用了一遍，单测只能钉住后端这一侧，
    /// 下次改表时会先在这里报错，而不是到用户面上才体现为「面板给了、工具调不通」。
    #[test]
    fn session_kind_caps_matrix() {
        assert_eq!(
            format!("{:?}", SessionKind::Ssh.caps()),
            "Caps(pty|exec|fs|monitor|negotiation|zmodem|tunnel)"
        );
        assert_eq!(
            format!("{:?}", SessionKind::Ftp.caps()),
            "Caps(fs|negotiation)"
        );
        assert_eq!(
            format!("{:?}", SessionKind::Ftps.caps()),
            "Caps(fs|negotiation)"
        );
        assert_eq!(
            format!("{:?}", SessionKind::Local.caps()),
            "Caps(pty|exec|monitor)"
        );
        for k in [SessionKind::Telnet, SessionKind::Serial, SessionKind::Mosh] {
            assert_eq!(format!("{:?}", k.caps()), "Caps(pty)", "{k:?} 只该有终端");
        }
    }

    /// FTP 会话的工具面 = 「文件助手」：列目录/读文件/搜文件/传输/拆计划/反问/出网/读附件/查知识库共 10 个；
    /// 靠 exec 落地的 `run_command`、`get_session_info` 与靠终端 ring 的 `get_terminal_snapshot` 不下发。
    #[test]
    fn ftp_registry_excludes_exec_and_pty_tools() {
        let r = ToolRegistry::for_kind(SessionKind::Ftp);
        assert_eq!(r.names().len(), 10, "FTP 应下发 10 个工具：{:?}", r.names());
        assert!(r.get("run_command").is_none(), "FTP 无 exec 通道");
        assert!(r.get("get_session_info").is_none(), "FTP 跑不了探测");
        assert!(r.get("get_terminal_snapshot").is_none(), "FTP 无终端 ring");
        for n in [
            "list_dir",
            "read_file",
            "search_files",
            "transfer_file",
            "write_plan",
            "ask_user",
            "read_attachment",
            "query_knowledge",
            "web_search",
            "fetch_url",
        ] {
            assert!(r.get(n).is_some(), "FTP 应可用 {n}");
        }
        // 同源过滤的冒烟检查：发给模型的 schema 集合必须与能执行的工具集合是同一批。
        let schemas = r.schemas();
        let schema_names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s["function"]["name"].as_str())
            .collect();
        assert_eq!(schema_names.len(), r.names().len(), "schemas 与 get 不同源");
        for n in &schema_names {
            assert!(r.get(n).is_some(), "模型看得到 {n} 却调不动");
        }
    }

    /// 其余协议的工具数：SSH 全量，本地没有远端文件后端，Telnet/串口/Mosh 只留下终端与出网。
    #[test]
    fn registry_size_tracks_kind() {
        assert_eq!(ToolRegistry::for_kind(SessionKind::Ssh).names().len(), 14);
        assert_eq!(ToolRegistry::for_kind(SessionKind::Local).names().len(), 10);
        let t = ToolRegistry::for_kind(SessionKind::Telnet);
        assert_eq!(t.names().len(), 7);
        assert!(
            t.get("get_terminal_snapshot").is_some(),
            "Telnet 有终端，快照可用"
        );
        assert!(t.get("run_command").is_none(), "Telnet 无可靠 exec");
        assert!(t.get("list_dir").is_none(), "Telnet 无远端文件后端");
        // 不过滤的全集仍给测试用（编排器的 schema 完整性用例依它）。
        assert_eq!(reg().names().len(), 14);
    }

    /// 应用级问答（首页 AI 聊天，保留会话 id → Caps::NONE）：只剩不碰会话通路的工具。
    /// 这是「不连接也能用 AI」的安全面底线：任何需要 EXEC/FS/PTY 的工具都不得下发。
    #[test]
    fn registry_none_caps_keeps_only_session_free_tools() {
        let r = ToolRegistry::for_caps(Caps::NONE);
        assert_eq!(r.names().len(), 6);
        for n in [
            "write_plan",
            "ask_user",
            "web_search",
            "fetch_url",
            "read_attachment",
            "query_knowledge",
        ] {
            assert!(r.get(n).is_some(), "应用级问答应可用 {n}");
        }
        for n in [
            "run_command",
            "get_session_info",
            "get_terminal_snapshot",
            "list_dir",
            "read_file",
            "search_files",
            "transfer_file",
            "list_services",
        ] {
            assert!(r.get(n).is_none(), "应用级问答不得下发 {n}");
        }
    }

    #[test]
    fn registry_exposes_builtin_tools() {
        let r = reg();
        let names = r.names();
        for n in [
            "run_command",
            "get_session_info",
            "get_terminal_snapshot",
            "list_dir",
            "read_file",
            "search_files",
            "transfer_file",
        ] {
            assert!(names.contains(&n), "缺工具 {n}");
            assert!(r.get(n).is_some(), "查不到 {n}");
        }
        assert!(r.get("nope").is_none());
    }

    /// 冒用内建契约名的插件技能必须丢弃——即使该内建在当前档位已被裁掉。
    /// 回归用例：`assemble` 曾只与「本档已下发的内建」比名，于是 FTP 档（无 EXEC）下
    /// 一个 `requires: none` 的 `run_command` 能钻过冲突检查入表，再被按名过滤的
    /// 只读白名单 `READONLY_TOOLS` 当内建放行（裁决只剩插件自报的 risk）。
    #[test]
    fn assemble_drops_plugin_tool_shadowing_builtin_name() {
        let skill = |name: &str| {
            Arc::new(PluginTool::new(
                "p".into(),
                PluginAiTool {
                    id: name.into(),
                    name: name.into(),
                    description: "t".into(),
                    requires: "none".into(),
                    risk: "readonly".into(),
                    export: String::new(),
                    parameters: json!({"type": "object", "properties": {}}),
                },
            )) as Arc<dyn Tool>
        };
        let reg = ToolRegistry::assemble(Caps::FS, vec![skill("run_command"), skill("my_skill")]);
        assert!(reg.get("run_command").is_none(), "插件不得冒用内建契约名");
        assert!(reg.get("my_skill").is_some(), "非冲突技能应正常入表");
    }

    #[test]
    fn schemas_are_openai_function_shape() {
        for s in reg().schemas() {
            assert_eq!(s["type"], "function");
            assert!(s["function"]["name"].is_string());
            assert!(s["function"]["description"].is_string());
            assert_eq!(s["function"]["parameters"]["type"], "object");
            // session_id 绝不进 LLM 可见 schema（由 ToolCtx 绑定）
            let props = &s["function"]["parameters"]["properties"];
            assert!(
                props.get("session_id").is_none(),
                "schema 不应含 session_id"
            );
        }
    }

    #[test]
    fn run_command_verdict_follows_policy() {
        let policy = PolicyEngine::new(ScanLevel::Standard, false);
        let t = reg().get("run_command").unwrap();
        assert_eq!(
            t.verdict(&json!({ "command": "ls -la" }), &policy).decision,
            Decision::Auto
        );
        assert_eq!(
            t.verdict(&json!({ "command": "systemctl restart nginx" }), &policy)
                .decision,
            Decision::NeedsApproval
        );
        assert_eq!(
            t.verdict(&json!({ "command": "rm -rf /" }), &policy)
                .decision,
            Decision::Blocked
        );
        // 缺 command 视作空命令（只读，不崩）
        assert_eq!(t.verdict(&json!({}), &policy).risk, RiskLevel::ReadOnly);
    }

    #[test]
    fn readonly_tools_are_auto() {
        let policy = PolicyEngine::default();
        for n in [
            "get_session_info",
            "get_terminal_snapshot",
            "list_dir",
            "read_file",
            "search_files",
        ] {
            let t = reg().get(n).unwrap();
            let v = t.verdict(&json!({}), &policy);
            assert_eq!(v.risk, RiskLevel::ReadOnly, "{n}");
            assert_eq!(v.decision, Decision::Auto, "{n}");
        }
    }

    #[test]
    fn transfer_file_verdict_by_direction() {
        // 上传改远端→写档需审批；下载对服务器只读→自动执行（与 read_file 同口径）。
        let policy = PolicyEngine::new(ScanLevel::Standard, false);
        let t = reg().get("transfer_file").unwrap();
        let up = t.verdict(&json!({ "direction": "upload", "path": "/tmp/a" }), &policy);
        assert_eq!(up.risk, RiskLevel::Write);
        assert_eq!(up.decision, Decision::NeedsApproval);
        let down = t.verdict(&json!({ "direction": "download", "path": "/var/log/b" }), &policy);
        assert_eq!(down.risk, RiskLevel::ReadOnly);
        assert_eq!(down.decision, Decision::Auto);
    }

    #[test]
    fn transfer_download_gated_in_approve_all() {
        // 全审批档：连下载这种只读传输也要批准，确认方向降级没绕过安全底线。
        let policy = PolicyEngine::new(ScanLevel::Standard, true);
        let v = reg()
            .get("transfer_file")
            .unwrap()
            .verdict(&json!({ "direction": "download", "path": "/var/log/b" }), &policy);
        assert_eq!(v.decision, Decision::NeedsApproval);
    }

    #[test]
    fn approve_readonly_gates_even_read_tools() {
        // 全审批模式：连只读工具也要批准
        let policy = PolicyEngine::new(ScanLevel::Standard, true);
        let v = reg().get("list_dir").unwrap().verdict(&json!({}), &policy);
        assert_eq!(v.decision, Decision::NeedsApproval);
    }

    #[test]
    fn shell_quotes_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote("/etc/passwd"), "'/etc/passwd'");
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let mut t = String::from("héllo");
        truncate_to_bytes(&mut t, 3); // 'h'(1) + 'é'(2) = 3 字节，正好
        assert_eq!(t, "hé");

        let mut u = String::from("héllo");
        truncate_to_bytes(&mut u, 2); // 装不下 'é'（需 3 字节），只留 'h'
        assert_eq!(u, "h");

        let mut v = String::from("abc");
        truncate_to_bytes(&mut v, 10); // 未超限，原样
        assert_eq!(v, "abc");
    }

    #[test]
    fn format_exec_surfaces_status() {
        let ok = ExecResult {
            stdout: "hi\n".into(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
            truncated: false,
            duration_ms: 5,
        };
        assert_eq!(format_exec(&ok).text, "hi\n");

        let failed = ExecResult {
            stdout: String::new(),
            stderr: "boom".into(),
            exit_code: Some(2),
            timed_out: false,
            truncated: false,
            duration_ms: 5,
        };
        let t = format_exec(&failed).text;
        assert!(t.contains("[stderr]") && t.contains("boom") && t.contains("退出码 2"));

        let empty = ExecResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
            truncated: false,
            duration_ms: 1,
        };
        assert_eq!(format_exec(&empty).text, "（命令无输出）");
    }

    #[test]
    fn strip_ansi_removes_csi_osc_and_control() {
        // 颜色码 + 文本 + \r\n
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m\r\n"), "red\n");
        // OSC 标题（BEL 结尾）
        assert_eq!(strip_ansi("\x1b]0;title\x07body"), "body");
        // OSC 标题（ST = ESC \ 结尾）
        assert_eq!(strip_ansi("\x1b]0;t\x1b\\body"), "body");
        // 光标控制 + 制表保留
        assert_eq!(strip_ansi("\x1b[2Ka\tb"), "a\tb");
        // 纯文本原样（空格是 0x20，保留）
        assert_eq!(strip_ansi("plain text"), "plain text");
    }
}
