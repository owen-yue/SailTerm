//! 多模型 BYOK provider 接入：基于 rig 统一封装 openai / anthropic /
//! ollama / openai-compatible（通义千问、DeepSeek、Kimi、智谱等走兼容协议 + 自定义 base_url）。
//!
//! API Key 从 Vault 取（[`crate::vault::keys::ai_provider`]），仅在构造 rig client 时注入
//! HTTP header，绝不进对话上下文/日志/事件。
//!
//! 各 provider 的 rig 客户端类型不同（rig 0.44 起为「配置 + 传输」的 `Model<W>` 驱动模型），
//! 但都可用 `chat` / `completion` 产出一个 completion 模型，故归一到 [`Client`] 枚举；
//! 上层 orchestrator 只经 [`Client::complete`] / [`Client::complete_streaming`] 发请求，
//! 在本文件里补齐 preamble/工具/采样参数。

use std::time::Duration;

use futures::StreamExt;
use rig::completion::{CompletionRequest, CompletionResponse, Message, ToolDefinition};
use rig::driver::{Model, Transport};
use rig::error::{ErrorKind, ProviderError};
use rig::operation::Completion;
use rig::providers::{anthropic, ollama, openai};
use rig::streaming::{Item, PartKind, StreamEvent};
use rig::wire::Wire;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::vault::Secret;

/// provider 协议类型（的 `kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    /// OpenAI 官方端点。
    Openai,
    /// Anthropic Claude 官方端点（库内/IPC 统一用文档口径 `claude`）。
    #[serde(rename = "claude")]
    Anthropic,
    /// 本地 Ollama（可无 API Key，默认 localhost:11434）。
    Ollama,
    /// OpenAI 兼容端点（通义千问/DeepSeek/Kimi/智谱等），**必须**填 base_url。
    OpenaiCompatible,
}

impl ProviderKind {
    /// 库内/IPC 统一字符串（openai|claude|ollama|openai-compatible）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "claude",
            Self::Ollama => "ollama",
            Self::OpenaiCompatible => "openai-compatible",
        }
    }

    /// 新建 provider 时预填的**上下文窗口默认值**（驱动历史/快照裁剪预算，也是
    /// 前端 ctx% 的分母）。取该协议的**保守主流下限**——宁小勿大：填小了百分比虚高但安全，
    /// 填大了会把超量上下文发给 provider 触发 400。`kind` 只到协议级，分不清兼容端点底下
    /// 究竟是 DeepSeek(64K) 还是 Kimi(128K+)，故 openai-compatible 取公共下限 64K，用户可上调。
    pub const fn default_context_tokens(self) -> i64 {
        match self {
            Self::Openai => 128_000,
            Self::Anthropic => 200_000,
            Self::OpenaiCompatible => 64_000,
            // 本地模型窗口取决于 num_ctx（默认普遍偏小），给大了直接截断/报错，保守取 8192。
            Self::Ollama => 8_192,
        }
    }

    /// 解析库内字符串；未知值回退 OpenAI（最通用）。兼容历史别名 `anthropic`。
    pub fn parse(s: &str) -> Self {
        match s {
            "claude" | "anthropic" => Self::Anthropic,
            "ollama" => Self::Ollama,
            "openai-compatible" => Self::OpenaiCompatible,
            _ => Self::Openai,
        }
    }

    /// 是否需要 API Key（Ollama 本地可空）。
    pub fn needs_key(self) -> bool {
        !matches!(self, Self::Ollama)
    }

    /// 是否本地 provider（命令上下文/输出**不离开本机**）：目前仅 Ollama（默认 localhost:11434）。
    /// 出网开关关闭时只有本地 provider 可用（出网控制、完全离线模式）。
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Ollama)
    }
}

/// 取 API Key：需要 Key 的 provider（除 Ollama 外）缺键即 `E_PROVIDER`。
fn require_key(key: Option<&str>) -> Result<&str, AppError> {
    key.ok_or_else(|| AppError::Provider("缺少 API Key".into()))
}

/// 各 provider 的 rig client 归一到一个枚举（rig 0.44：客户端 = 一份 provider 配置 + 共享 reqwest 传输）。
/// 三个变体体量悬殊（Anthropic 近 800 字节），统一 `Box` 把枚举压到指针大小：每次请求构建后
/// 按值入栈/入结构时只搬 8 字节而非几百字节。
pub enum Client {
    Openai(Box<openai::OpenAI>),
    Anthropic(Box<anthropic::Anthropic>),
    Ollama(Box<ollama::Ollama>),
}

/// 单轮 completion 的系统提示 + 采样参数（打包成一个结构，避免 `complete`/`send_turn`
/// 参数过多）。全部可选：`None` 表示该项交给 provider 默认。
#[derive(Default, Clone, Debug)]
pub struct CompleteParams {
    /// 系统提示词；注入为 rig preamble（builder 会转成首条 system 消息）。
    pub preamble: Option<String>,
    /// 采样温度。
    pub temperature: Option<f64>,
    /// 输出 token 上限（Anthropic 建议显式给）。
    pub max_tokens: Option<u64>,
}

impl Client {
    /// 按 kind 构造 rig client。
    ///
    /// - `api_key`：从 Vault 取的明文 Key（Ollama 可 `None`，则不发 `Authorization`）；仅用于 HTTP header。
    /// - `base_url`：`OpenaiCompatible` 必填；`Openai`/`Anthropic`/`Ollama` 可选（覆盖默认端点）。
    ///
    /// 构造 client 不发起任何网络请求（只有发 completion 才会），故可离线单测。
    /// 注意：reqwest 用 `rustls-no-provider` 编译，调用前须先 [`super::install_crypto_provider`]。
    pub fn build(
        kind: ProviderKind,
        base_url: Option<&str>,
        api_key: Option<&Secret>,
    ) -> Result<Self, AppError> {
        // 借自 api_key 参数（生命周期同 &Secret），match 各分支就地用于构造 header。
        // Ollama 本地 daemon 可以无凭证，其余缺键在上面直接报错。
        let key: Option<&str> = match api_key {
            Some(s) if !s.is_empty() => Some(s.expose()),
            _ if kind.needs_key() => return Err(AppError::Provider("缺少 API Key".into())),
            _ => None,
        };
        // 先 trim 再过滤：纯空白的 base_url 等同未填（openai-compatible 据此报 E_PROVIDER），
        // 有效值也去掉首尾空格再交给配置，避免把 " https://x " 之类的脏值带进请求。
        let url = base_url.map(str::trim).filter(|s| !s.is_empty());

        Ok(match kind {
            ProviderKind::Openai => {
                let mut cfg = openai::OpenAIConfig::new(require_key(key)?);
                if let Some(u) = url {
                    cfg = cfg.with_base_url(u);
                }
                Self::Openai(Box::new(cfg.client()))
            }
            ProviderKind::OpenaiCompatible => {
                let u = url.ok_or_else(|| {
                    AppError::Provider("openai-compatible 必须填 base_url".into())
                })?;
                Self::Openai(Box::new(
                    openai::OpenAIConfig::new(require_key(key)?)
                        .with_base_url(u)
                        .client(),
                ))
            }
            ProviderKind::Anthropic => {
                let mut cfg = anthropic::AnthropicConfig::new(require_key(key)?);
                if let Some(u) = url {
                    cfg = cfg.with_base_url(u);
                }
                Self::Anthropic(Box::new(cfg.client()))
            }
            ProviderKind::Ollama => {
                let mut cfg = ollama::OllamaConfig::new();
                if let Some(k) = key {
                    cfg = cfg.with_api_key(k);
                }
                if let Some(u) = url {
                    cfg = cfg.with_base_url(u);
                }
                Self::Ollama(Box::new(cfg.client()))
            }
        })
    }

    /// 单轮 completion（轻量状态机唯一的模型调用落点）：发送 provider 无关的
    /// `messages` + `tools`，返回 provider 无关的 [`CompletionResponse`]。各 provider 的
    /// `Model<W>` 具体类型不同，用泛型 [`send_turn`] / [`stream_turn`] 在 match 各分支就地收敛。
    ///
    /// 路由选择刻意与升级前保持一致：OpenAI 与兼容端点走 **Chat Completions**（`client.chat(..)`）
    /// 而非 rig 0.44 默认的 Responses API——绝大多数第三方兼容端点只实现了前者；Ollama 走原生
    /// `/api/chat`（`native_completion`），Anthropic 走 Messages。
    ///
    /// `messages` 末条视为本轮 prompt、其余为历史（rig 拼成 `[preamble] ++ 历史 ++ [prompt]`）。
    /// 凭证只在建 client 时进 HTTP header，绝不经此进入模型上下文。
    pub async fn complete(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        params: CompleteParams,
    ) -> Result<CompletionResponse, AppError> {
        let (prompt, prior) = split_prompt(messages)?;
        match self {
            Self::Openai(c) => send_turn(&c.chat(model), prompt, prior, tools, params).await,
            Self::Anthropic(c) => {
                send_turn(&c.completion(model), prompt, prior, tools, params).await
            }
            Self::Ollama(c) => {
                send_turn(&c.native_completion(model), prompt, prior, tools, params).await
            }
        }
    }

    /// 流式 completion：每段增量按类别回调 `on_delta`（正文 / 推理链 / 工具调用开始），收完再转成
    /// 统一 [`CompletionResponse`]（工具调用等仍走聚合 choice，编排逻辑不变）。
    pub async fn complete_streaming(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        params: CompleteParams,
        on_delta: impl FnMut(StreamDelta) + Send,
    ) -> Result<CompletionResponse, AppError> {
        let (prompt, prior) = split_prompt(messages)?;
        match self {
            Self::Openai(c) => {
                stream_turn(&c.chat(model), prompt, prior, tools, params, on_delta).await
            }
            Self::Anthropic(c) => {
                stream_turn(&c.completion(model), prompt, prior, tools, params, on_delta).await
            }
            Self::Ollama(c) => {
                stream_turn(
                    &c.native_completion(model),
                    prompt,
                    prior,
                    tools,
                    params,
                    on_delta,
                )
                .await
            }
        }
    }
}

/// 一段流式增量的类别：让前端把「模型正在产出什么」如实分相位显示，并对推理链单独走字。
///
/// 正文与推理都是逐 token 的文本增量；`ToolStart` 只在某个工具调用部件**开始**时发一次（参数
/// 尚在生成、还没到能出 `AiStep::ToolCall` 的时机），供前端把「思考中」细分成「生成命令中」。
#[derive(Debug, Clone)]
pub enum StreamDelta {
    /// 最终答复正文的 token 增量。
    Text(String),
    /// 推理/思考链的 token 增量（仅推理型模型有）。
    Reasoning(String),
    /// 模型开始生成一个工具调用。
    ToolStart,
}

/// 拆出末条消息作 prompt、其余作历史（rig 的 builder 要求 prompt 单独传）。
fn split_prompt(mut messages: Vec<Message>) -> Result<(Message, Vec<Message>), AppError> {
    let prompt = messages
        .pop()
        .ok_or_else(|| AppError::Invalid("对话消息为空，无法请求模型".into()))?;
    Ok((prompt, messages))
}

/// 把历史 + 工具 + 采样参数装成 rig 请求（非流式与流式共用）。
///
/// rig 0.44 的 [`CompletionRequest`]：`new(prompt)` 后再 `.messages(prior)` 会把 prior 插在
/// prompt 之前，顺序仍为 `历史 ++ prompt`（与升级前一致）。
fn build_request(
    prompt: Message,
    prior: Vec<Message>,
    tools: Vec<ToolDefinition>,
    params: CompleteParams,
) -> CompletionRequest {
    let mut r = CompletionRequest::new(prompt).messages(prior).tools(tools);
    if let Some(p) = params.preamble {
        r = r.preamble(p);
    }
    if let Some(t) = params.temperature {
        r = r.temperature(t);
    }
    if let Some(m) = params.max_tokens {
        r = r.max_tokens(m);
    }
    r
}

/// 厂商错误的业务语义分类：从 rig `ProviderError` 的结构化状态码/错误种类派生，
/// 使摘要与上层分支（限流退避、鉴权引导重填 Key）共用同一口径，不再各自 parse 字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderErrKind {
    Auth,
    Balance,
    NotFound,
    RateLimit,
    Server,
    Network,
    ContextOverflow,
    Other,
}

impl ProviderErrKind {
    /// 归类 → `err.ai.*` 稳定 i18n 键（前端词典据此渲染当前语言的类别文案）。取代旧版
    /// 直接烤进摘要的中文 `label`：后端不再产出中文，归类文案的翻译权交给前端词典。
    fn i18n_key(self) -> &'static str {
        match self {
            Self::Auth => "err.ai.auth",
            Self::Balance => "err.ai.balance",
            Self::NotFound => "err.ai.notFound",
            Self::RateLimit => "err.ai.rateLimit",
            Self::Server => "err.ai.server",
            Self::Network => "err.ai.network",
            Self::ContextOverflow => "err.ai.contextOverflow",
            Self::Other => "err.ai.other",
        }
    }

    /// 从状态码 + 是否上下文超长 + 是否传输失败派生分类（纯逻辑，离线可测）。
    fn classify(status: Option<u16>, overflow: bool, transport: bool) -> Self {
        match status {
            Some(401) | Some(403) => Self::Auth,
            Some(402) => Self::Balance,
            Some(404) => Self::NotFound,
            Some(429) => Self::RateLimit,
            Some(s) if s >= 500 => Self::Server,
            Some(400) if overflow => Self::ContextOverflow,
            Some(_) => Self::Other,
            None if transport => Self::Network,
            None => Self::Other,
        }
    }

    /// 是否为可安全重试的瞬时错误：限流 / 服务端 / 网络。鉴权·余额·不存在·上下文超长等
    /// 是确定性失败，重试只会白等，必须直接暴露给用户。
    fn is_retryable(self) -> bool {
        matches!(self, Self::RateLimit | Self::Server | Self::Network)
    }
}

/// 从 rig `ProviderError` 派生业务分类：错误摘要与重试判定共用此单一口径，
/// 避免两处各 parse 一次而分叉（比如摘要说「限流」而重试却又不去重试）。
fn classify_provider_err(e: &ProviderError) -> ProviderErrKind {
    let status = e.provider_response_status().map(|s| s.as_u16());
    let detail = provider_err_detail(e);
    let transport = matches!(e.kind(), ErrorKind::Http);
    let overflow = detail.as_deref().is_some_and(is_context_overflow);
    ProviderErrKind::classify(status, overflow, transport)
}

/// 端点自报的错误详情：从 rig 已解析的 JSON body 取 `error.message`（次选 `type`/`code`）；
/// 少数端点把 `error` 直接给成字符串，也一并接住。取不到返回 `None`。
fn provider_err_detail(e: &ProviderError) -> Option<String> {
    let v = e.provider_response_json().ok().flatten()?;
    let node = match v.get("error") {
        Some(err) => err.clone(),
        None => v,
    };
    if let Some(s) = node.as_str() {
        let s = s.trim();
        return (!s.is_empty()).then(|| s.to_string());
    }
    ["message", "type", "code"]
        .iter()
        .find_map(|k| node.get(*k).and_then(|m| m.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 上下文超长判定：各家措辞不一（OpenAI `context_length`、Anthropic `prompt is too long`、
/// 兼容端点 `maximum context`/`too many tokens`），统一小写关键词匹配。
fn is_context_overflow(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    [
        "context_length",
        "context length",
        "maximum context",
        "too many tokens",
        "prompt is too long",
        "reduce your prompt",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// 截断到 `max` 个字符，超长补省略号（按 char 计，中文安全）。
fn truncate_chars(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        head + "…"
    } else {
        head
    }
}

/// 把 rig `ProviderError` 拆成 `(i18n 归类键, 不可翻译细节串)`：归类键交前端词典本地化，
/// 细节串只含 HTTP 状态与端点自报原文（本就非中文），后端不再烤中文。状态码 / JSON / 错误种类
/// 全走 rig 结构化访问器（`provider_response_status` / `provider_response_json` / `kind`），
/// 绝不把原始 JSON 上屏。取代旧版把中文烤成一行的 `provider_err_summary`。
fn provider_err_parts(e: &ProviderError) -> (&'static str, String) {
    let kind = classify_provider_err(e);
    let status = e.provider_response_status().map(|s| s.as_u16());
    let detail = provider_err_detail(e);
    let mut segs: Vec<String> = Vec::new();
    if let Some(s) = status {
        segs.push(format!("HTTP {s}"));
    }
    if let Some(d) = detail {
        segs.push(d);
    }
    // 无任何结构化线索（无状态、无端点详情，如纯传输失败）：回落 Display 截断，至少留可诊断文本。
    let detail_str = if segs.is_empty() {
        truncate_chars(&e.to_string(), 160)
    } else {
        truncate_chars(&segs.join(" · "), 200)
    };
    (kind.i18n_key(), detail_str)
}

/// 非流式 [`send_turn`] 对瞬时错误（限流 / 5xx / 网络抖动）的自动重试策略：最多
/// [`SEND_MAX_ATTEMPTS`] 次尝试，失败后按 [`SEND_BACKOFF_MS`] 阶梯退避再重来。请求幂等
/// 且尚未产出任何增量，重试无副作用；流式主聊天走 [`stream_turn`]（出文后不可盲目重来），
/// 故不在此覆盖。旨在把「保存自动测试 / 连接诊断」从偶发网络或限流抖动中救回来。
const SEND_MAX_ATTEMPTS: u32 = 3;
const SEND_BACKOFF_MS: [u64; 2] = [200, 600];

/// 在任一 provider 的 completion 模型上发一轮请求。泛型 `W`/`T` 收敛 openai/anthropic/ollama
/// 各自不同的 wire 与传输类型（只要该 wire 的操作是 [`Completion`](rig::operation::Completion)），
/// 使 [`Client::complete`] 上层只见统一的 [`CompletionResponse`]。瞬时错误按上述策略自动重试。
async fn send_turn<W, T>(
    model: &Model<W, T>,
    prompt: Message,
    prior: Vec<Message>,
    tools: Vec<ToolDefinition>,
    params: CompleteParams,
) -> Result<CompletionResponse, AppError>
where
    W: Wire<Op = Completion>,
    T: Transport<W>,
{
    let mut attempt: u32 = 0;
    loop {
        // 每次重试都重建请求（`build_request` 消费入参，故此处按 clone 复用）。
        let req = build_request(prompt.clone(), prior.clone(), tools.clone(), params.clone());
        match model.call(req).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                // 确定性失败（鉴权 / 余额 / 不存在 / 上下文超长）或额度用尽：就地报错，绝不重试。
                if !classify_provider_err(&e).is_retryable() || attempt >= SEND_MAX_ATTEMPTS - 1 {
                    let (key, detail) = provider_err_parts(&e);
                    return Err(AppError::AiProvider { key, detail });
                }
                // 瞬时错误：短退避后重来（429 限流窗口常为亚秒级，几次即可跨过）。
                tokio::time::sleep(Duration::from_millis(
                    SEND_BACKOFF_MS[(attempt as usize).min(SEND_BACKOFF_MS.len() - 1)],
                ))
                .await;
                attempt += 1;
            }
        }
    }
}

/// 流式发送：边收文本增量边回调，收完由 rig 折叠成 [`CompletionResponse`]
/// （工具调用、usage 等仍走聚合结果，编排逻辑不变）。
async fn stream_turn<W, T>(
    model: &Model<W, T>,
    prompt: Message,
    prior: Vec<Message>,
    tools: Vec<ToolDefinition>,
    params: CompleteParams,
    mut on_delta: impl FnMut(StreamDelta) + Send,
) -> Result<CompletionResponse, AppError>
where
    W: Wire<Op = Completion>,
    T: Transport<W>,
{
    // rig 0.44：`stream` 同步返回（请求编码失败就地报错），首个帧要等到轮询才发出；
    // 错误作为流的最后一项送达，因此下面的 Err 分支就是「流中途中断」。
    let mut stream = model
        .stream(build_request(prompt, prior, tools, params))
        .map_err(|e| {
            let (key, detail) = provider_err_parts(&e);
            AppError::AiProvider { key, detail }
        })?;
    while let Some(item) = stream.next().await {
        match item {
            // 部件开始：工具调用起手就报一次相位（正文/推理无需在此报，各自的增量分支已足够）。
            Ok(Item::Event(StreamEvent::Start { kind, .. })) => {
                if kind == PartKind::ToolCall {
                    on_delta(StreamDelta::ToolStart);
                }
            }
            Ok(Item::Event(StreamEvent::Text { text, .. })) => {
                if !text.is_empty() {
                    on_delta(StreamDelta::Text(text));
                }
            }
            // 推理链增量：以前整段丢弃（`Ok(_) => {}`），现单独转发，让「思考」过程实时可见。
            Ok(Item::Event(StreamEvent::Reasoning { text, .. })) => {
                if !text.is_empty() {
                    on_delta(StreamDelta::Reasoning(text));
                }
            }
            // 工具调用参数 / 部件结束以及 provider 未建模的 payload：不转增量，
            // 它们由下面的 `finish()` 统一折叠进 choice。
            Ok(_) => {}
            Err(e) => {
                let (key, detail) = provider_err_parts(&e);
                return Err(AppError::AiProvider { key, detail });
            }
        }
    }
    stream
        .finish()
        .await
        .map_err(|e| {
            let (key, detail) = provider_err_parts(&e);
            AppError::AiProvider { key, detail }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 离线构造各 provider 的 client（不发网络请求）；缺 Key / 缺 base_url 报错。
    #[test]
    fn builds_clients_offline() {
        // reqwest 用 rustls-no-provider，建 client 前必须先装 ring 全局 provider。
        crate::ai::install_crypto_provider();
        let key = Secret::new("sk-test-dummy");

        assert!(Client::build(ProviderKind::Openai, None, Some(&key)).is_ok());
        assert!(Client::build(
            ProviderKind::OpenaiCompatible,
            Some("https://x/v1"),
            Some(&key)
        )
        .is_ok());
        assert!(Client::build(ProviderKind::Anthropic, None, Some(&key)).is_ok());
        assert!(Client::build(ProviderKind::Ollama, None, None).is_ok());

        // 护栏：需要 Key 的 provider 缺 Key → E_PROVIDER；兼容端点缺 base_url → E_PROVIDER。
        assert!(matches!(
            Client::build(ProviderKind::Openai, None, None),
            Err(AppError::Provider(_))
        ));
        assert!(matches!(
            Client::build(ProviderKind::OpenaiCompatible, None, Some(&key)),
            Err(AppError::Provider(_))
        ));
        assert!(matches!(
            Client::build(ProviderKind::OpenaiCompatible, Some("  "), Some(&key)),
            Err(AppError::Provider(_))
        ));
    }

    #[test]
    fn only_ollama_skips_key() {
        assert!(ProviderKind::Openai.needs_key());
        assert!(!ProviderKind::Ollama.needs_key());
        assert!(ProviderKind::Anthropic.needs_key());
        assert!(ProviderKind::OpenaiCompatible.needs_key());
    }

    /// 状态码 → `err.ai.*` 归类键（纯逻辑，不依赖 rig；中文归类已交前端词典）。
    #[test]
    fn classifies_status_to_i18n_key() {
        let key =
            |status, overflow, transport| ProviderErrKind::classify(status, overflow, transport).i18n_key();
        assert_eq!(key(Some(402), false, false), "err.ai.balance");
        assert_eq!(key(Some(401), false, false), "err.ai.auth");
        assert_eq!(key(Some(403), false, false), "err.ai.auth");
        assert_eq!(key(Some(404), false, false), "err.ai.notFound");
        assert_eq!(key(Some(429), false, false), "err.ai.rateLimit");
        assert_eq!(key(Some(503), false, false), "err.ai.server");
        assert_eq!(key(Some(400), true, false), "err.ai.contextOverflow");
        assert_eq!(key(Some(400), false, false), "err.ai.other");
        assert_eq!(key(None, false, true), "err.ai.network");
    }

    /// `provider_err_parts`：归类键 + 不可翻译细节（端点原文），细节里绝不含中文。
    #[test]
    fn parts_keep_detail_chinese_free() {
        let e = ProviderError::from_provider_body(
            r#"{"error":{"message":"Insufficient Balance (request_id: abc)","type":"x"}}"#,
        );
        let (key, detail) = provider_err_parts(&e);
        assert_eq!(detail, "Insufficient Balance (request_id: abc)");
        assert_eq!(key, "err.ai.other"); // 无状态、非传输、无超长关键词 → Other
        assert!(!detail.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)), "细节不应含中文");

        // 端点 message 超长：封顶 200 并补省略号。
        let long = "x".repeat(300);
        let e = ProviderError::from_provider_body(&format!(r#"{{"error":{{"message":"{long}"}}}}"#));
        let (_, detail) = provider_err_parts(&e);
        assert!(detail.chars().count() <= 201 && detail.ends_with('…'), "{}", detail.chars().count());
    }

    /// 只有瞬时类错误会自动重试；确定性失败（鉴权 / 余额 / 不存在 / 上下文超长）必须直接暴露。
    #[test]
    fn only_transient_errors_retry() {
        let retryable =
            |status, overflow, transport| ProviderErrKind::classify(status, overflow, transport).is_retryable();
        assert!(retryable(Some(429), false, false), "限流应重试");
        assert!(retryable(Some(503), false, false), "服务端应重试");
        assert!(retryable(None, false, true), "网络/传输应重试");
        assert!(!retryable(Some(401), false, false), "鉴权不该重试");
        assert!(!retryable(Some(402), false, false), "余额不该重试");
        assert!(!retryable(Some(404), false, false), "不存在不该重试");
        assert!(!retryable(Some(400), true, false), "上下文超长不该重试");
        assert!(!retryable(Some(400), false, false), "其他 4xx 不该重试");
    }

    /// 从 rig 结构化 body 提取端点详情：OpenAI 嵌套 / error 为字符串 / 非 JSON 三种形态。
    #[test]
    fn extracts_detail_from_provider_body() {
        let e = ProviderError::from_provider_body(
            r#"{"error":{"message":"boom (request_id: abc)","type":"invalid_request_error"}}"#,
        );
        assert_eq!(provider_err_detail(&e).as_deref(), Some("boom (request_id: abc)"));

        let e = ProviderError::from_provider_body(r#"{"error":"model not found"}"#);
        assert_eq!(provider_err_detail(&e).as_deref(), Some("model not found"));

        let e = ProviderError::from_provider_body("plain text, not json");
        assert_eq!(provider_err_detail(&e), None);
    }
}
