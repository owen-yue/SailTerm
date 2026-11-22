//! AI 命令层：对话编排入口 + 审批应答 + provider/对话/消息的 CRUD。
//!
//! 契约纪律：**API Key 只在 Vault**，命令层取出后仅注入 rig client 的 HTTP
//! header，构造完 client 即随 `Secret` 清零，绝不进对话历史/日志/事件/响应体；provider DTO 只回
//! `hasKey` 布尔标记（`key_ref` 被 `#[serde(skip)]`）。出网内容统一由 orchestrator 的不可绕过节点
//! 脱敏，本层不另开口子。
//!
//! 分层：本地 IO（SQLite / Vault / 终端环形缓冲）一律走 [`blocking`] 移出 async 执行器；
//! 只有 [`ai_send_message`] 与 [`ai_provider_test`] 的网络段在 async 里 `await`。

use base64::Engine;
use rig::completion::message::{AssistantContent, ImageMediaType, UserContent};
use rig::completion::Message;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::AppHandle;

use super::{blocking, state};
use crate::ai::approval::{ApprovalOutcome, ApprovalPrompt};
use crate::ai::attach::{self, AttachKind, TurnFile, TurnImage};
use crate::ai::context::{self, build_system_prompt_with, Budget, ChatMode};
use crate::ai::orchestrator::{run_turn, RunConfig, TurnUsage};
use crate::ai::policy::{RuleEffect, UserRule};
use crate::ai::providers::{Client, CompleteParams, ProviderKind};
use crate::ai::question::{QuestionOutcome, QuestionPrompt};
use crate::ai::sanitize::Sanitizer;
use crate::ai::tools::ToolRegistry;
use crate::db::ai::{
    AiChatMessage, AiConversation, AiProvider, ChatRole, SanitizeLevel, UsageStats,
};
use crate::db::memory::Memory;
use crate::db::policy_rule::{PolicyRule, RuleAction};
use crate::db::{new_id, now_ms, Db};
use crate::error::AppError;
use crate::session::{Caps, SessionKind, APP_CHAT_SESSION};
use crate::state::AppState;
use crate::vault::{keys, Secret};

/// 终端快照拉取上限（字节）；随后由 [`build_system_prompt`] 按 token 预算二次裁剪。
const SNAPSHOT_BYTES: usize = 32 * 1024;
/// 重建对话历史 / 拉消息时回看的最大条数（防超长对话一次性全载）。
const HISTORY_LOOKBACK: i64 = 200;
/// 自动新建对话时，用首条消息前若干字符作标题。
const AUTO_TITLE_CHARS: usize = 24;
/// 视觉能力探测用的图：16×16 PNG（79 字节，base64 108 字符）。刻意不用 1×1——部分兼容端点会把
/// 尺寸过小的图判为无效图并报错，那会被误读成「不支持 vision」。探测只关心**端点肯不肯接受
/// image part**，不要求模型真看图说话，故体积小到可以编译进二进制、不占仓库文件。
///
/// 写成**单行不续接**：这个常量手改一次就可能碎掉，而碎掉的后果不是报错而是**假阴性**——
/// 端点在解码阶段就拒，`probe_vision` 又把任何失败归为「不支持」，真能看图的模型会被误标。
/// 下面 `probe_image_decodes_as_png`（本文件单测）钉住它的完整性（base64 往返 + PNG 收尾）。
const PROBE_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFklEQVR42mMwSJhAEmIY1TCqYfhqAAC82CAQcG9bKwAAAABJRU5ErkJggg==";

// ─────────────────────────── AI Provider（设置）───────────────────────────

/// Provider 配置表单入参。**API Key 是一次性的**：写入 Vault 后随 `Secret` 清零，响应体只回 `hasKey`。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInput {
    /// `None` = 新建；`Some` = 编辑既有 provider。
    pub id: Option<String>,
    pub name: String,
    pub kind: ProviderKind,
    /// `openai-compatible` 必填；其余可选（覆盖默认端点）。
    pub base_url: Option<String>,
    pub model: String,
    pub supports_tools: Option<bool>,
    /// 图片能力三态兜底：`None` = 跟随探测结果（默认）；`Some(true/false)` = 用户在设置里
    /// 强制改判。留这一道口是因为第三方兼容端点探例多，探测存在**假阴**风险，而假阴的代价
    /// 是图片功能永久不可用且用户无从自救。它改的是「能力判定」，不是加第二条降级通路。
    #[serde(default)]
    pub supports_vision: Option<bool>,
    /// 缺省 `Strict`；云 provider 会被强制抬到 ≥ `Strict`。
    #[serde(default)]
    pub sanitize_level: SanitizeLevel,
    pub max_context_tokens: Option<i64>,
    pub temperature: Option<f64>,
    #[serde(default)]
    pub is_default: bool,
    /// 非敏感扩展 JSON（自定义 header / 组织 id 等）。
    pub extra: Option<String>,
    /// 明文 API Key：仅入参；`None`/空 = 保持原样（编辑时不重填即不动 Vault）。
    pub api_key: Option<Secret>,
    /// `true` = 清除已存 Key。
    #[serde(default)]
    pub clear_key: bool,
}

/// 列出全部 provider（回填 `hasKey`，不含 Key 内容）。
#[tauri::command]
pub async fn ai_provider_list(app: AppHandle) -> Result<Vec<AiProvider>, AppError> {
    blocking(move || {
        let st = state(&app);
        st.db
            .list_providers()?
            .into_iter()
            .map(|p| flags_provider(&st, p))
            .collect()
    })
    .await
}

#[tauri::command]
pub async fn ai_provider_get(app: AppHandle, id: String) -> Result<AiProvider, AppError> {
    blocking(move || {
        let st = state(&app);
        let p = st
            .db
            .get_provider(&id)?
            .ok_or_else(|| AppError::Provider(format!("未找到 {id}")))?;
        flags_provider(&st, p)
    })
    .await
}

/// 新建或编辑 provider；返回落库后的 DTO（`key_ref` 已转 `hasKey` 布尔标记）。
#[tauri::command]
pub async fn ai_provider_save(
    app: AppHandle,
    input: ProviderInput,
) -> Result<AiProvider, AppError> {
    blocking(move || {
        let st = state(&app);
        save_provider(&st, input)
    })
    .await
}

/// 删除 provider，并清理 Vault 里属于它的 API Key 条目。
#[tauri::command]
pub async fn ai_provider_delete(app: AppHandle, id: String) -> Result<(), AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.delete_provider(&id)?;
        st.vault.delete(&keys::of_ai_provider(&id))
    })
    .await
}

/// 设为默认 provider（先清全部再置本行，`ai_send_message` 未显式指定时用它）。
#[tauri::command]
pub async fn ai_provider_set_default(app: AppHandle, id: String) -> Result<(), AppError> {
    blocking(move || state(&app).db.set_default_provider(&id)).await
}

/// 「测试」按钮的回执：连通结果 + 顺带探出的图片能力。
///
/// 图片能力不单独开一个按钮：探测本质是「往端点发一条带 image part 的 completion」，与连通
/// 自检同一条通路、同一个 `max_tokens=1` 量级的成本，顺带探完最省用户操作。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTest {
    /// 连通回执（模型回的首若干字，或「连接成功」文案）。
    pub reply: String,
    /// 本轮探测结论：`Some(true)` 支持图片 / `Some(false)` 不支持 / `None` 未探（连通就失败了）。
    pub vision: Option<bool>,
    /// `vision=Some(false)` 时的端点报错摘要（截断），供设置页说明原因；其余情形 `None`。
    pub vision_error: Option<String>,
}

/// 连通性自检：建 client 后发一条极简 completion，再顺带探测图片能力，结果写回 provider。
/// 不写对话、不进历史；仅验证 base_url / API Key / 模型名是否可用。
#[tauri::command]
pub async fn ai_provider_test(app: AppHandle, id: String) -> Result<ProviderTest, AppError> {
    let probe_app = app.clone();
    // 探测完要把结果写回这一行：`id` 会被下面的建 client 闭包 move 走，先留一份。
    let probe_target = id.clone();
    let (client, model) = blocking({
        let app = app.clone();
        move || {
            let st = state(&app);
            let p = st
                .db
                .get_provider(&id)?
                .ok_or_else(|| AppError::Provider(format!("未找到 {id}")))?;
            // 出网闸门：出网关闭时云端 provider 的连通性自检也阻断（自检会真实发一条 ping 出网）。
            super::settings::load_ai_settings(&st.db)?.check_egress(p.kind)?;
            let key = match &p.key_ref {
                Some(k) => st.vault.get(k)?,
                None => None,
            };
            if p.kind.needs_key() && key_missing(&key) {
                return Err(AppError::Biz {
                    key: "err.provider.noApiKey",
                    zh: "未保存 API Key，请在设置中填写",
                });
            }
            let client = Client::build(p.kind, p.base_url.as_deref(), key.as_ref())?;
            Ok((client, p.model))
        }
    })
    .await?;

    let reply = ping_completion(&client, &model).await?;
    // 探测失败（端点不认图）不改「连通正常」结论，只附原因；网络级错误则整体报错（上面已拦）。
    let (vision, vision_error) = match probe_vision(&client, &model).await {
        Ok(()) => (Some(true), None),
        Err(msg) => (Some(false), Some(msg)),
    };
    // 探测结果落库（写回 supports_vision）：发送带图时以此为准。失败不阻断回执（旧值仍可用）。
    let _ = blocking(move || {
        let st = state(&probe_app);
        let mut p = st
            .db
            .get_provider(&probe_target)?
            .ok_or_else(|| AppError::Provider(format!("未找到 {probe_target}")))?;
        p.supports_vision = vision;
        st.db.upsert_provider(&p)
    })
    .await;
    Ok(ProviderTest {
        reply,
        vision,
        vision_error,
    })
}

/// 发一条极简 completion，回传模型文本前若干字；`ai_provider_test` 与草稿自检共用。
/// 连通自检只需证明「端点可达 + 鉴权通过 + 模型名有效」，模型名无效会直接 400（与 max_tokens 无关），
/// 故把生成上限压到 1 token：拿到首个 token 即返回，避免为“测试”白等一整句生成（与主流客户端一致的快速 ping）。
async fn ping_completion(client: &Client, model: &str) -> Result<String, AppError> {
    let resp = client
        .complete(
            model,
            vec![Message::user("ping")],
            vec![],
            CompleteParams {
                max_tokens: Some(1),
                ..Default::default()
            },
        )
        .await?;
    let mut out = String::new();
    for item in &resp.choice {
        if let AssistantContent::Text(t) = item {
            out.push_str(&t.text);
        }
    }
    Ok(if out.trim().is_empty() {
        "连接成功（模型未返回文本）".to_string()
    } else {
        out.trim().chars().take(200).collect()
    })
}

/// 用表单草稿直连自检（不必先落库，新建模型流程无需先保存）：验证 base_url / API Key / 模型名是否可用。
/// 编辑态 Key 留空时回落保险库已存 Key（与 `save`「不重填即不动 Vault」一致）；不写库、不进对话。
/// 图片能力同样顺带探测，但**不落库**（新建未保存态）：结果随表单存进 `ProviderInput`，保存时写入。
#[tauri::command]
pub async fn ai_provider_test_draft(
    app: AppHandle,
    input: ProviderInput,
) -> Result<ProviderTest, AppError> {
    let (client, model) = blocking({
        let app = app.clone();
        move || {
            let st = state(&app);
            // 出网闸门：出网关闭时草稿自检同样阻断（会真实发一条 ping 出网）。
            super::settings::load_ai_settings(&st.db)?.check_egress(input.kind)?;
            let id = input.id.clone();
            let mut key = input.api_key;
            if key.as_ref().map_or(true, |s| s.is_empty()) {
                if let Some(id) = &id {
                    if let Some(p) = st.db.get_provider(id)? {
                        if let Some(k) = &p.key_ref {
                            key = st.vault.get(k)?;
                        }
                    }
                }
            }
            if input.kind.needs_key() && key_missing(&key) {
                return Err(AppError::Biz {
                    key: "err.provider.noApiKey",
                    zh: "未保存 API Key，请在设置中填写",
                });
            }
            let client = Client::build(input.kind, input.base_url.as_deref(), key.as_ref())?;
            Ok((client, input.model))
        }
    })
    .await?;
    let reply = ping_completion(&client, &model).await?;
    let (vision, vision_error) = match probe_vision(&client, &model).await {
        Ok(()) => (Some(true), None),
        Err(msg) => (Some(false), Some(msg)),
    };
    Ok(ProviderTest {
        reply,
        vision,
        vision_error,
    })
}

/// 探测模型是否接受图片输入：发一条「文本 + 16×16 PNG」、`max_tokens=1` 的 completion。
///
/// 判定依据是**端点肯不接受 image part**（不支持视觉的模型/通道会在请求校验阶段就 4xx），
/// 无需模型真生成内容，故 1 token 足够，成本与 [`ping_completion`] 同级。
/// `Ok(())` = 支持；`Err(msg)` = 不支持（`msg` 为错误摘要，截断 200 字供前端展示）。
async fn probe_vision(client: &Client, model: &str) -> Result<(), String> {
    client
        .complete(
            model,
            vec![probe_message()],
            vec![],
            CompleteParams {
                max_tokens: Some(1),
                ..Default::default()
            },
        )
        .await
        .map(|_| ())
        .map_err(|e| {
            // provider 层已把错误拆成「归类键 + 不可翻译细节」（见 provider_err_parts）；这里只取
            // 纯细节（HTTP 状态 + 端点自报原文，本就非中文）作回执行，归类文案由前端词典按语言渲染。
            let s = e.detail().map(str::to_string).unwrap_or_else(|| e.to_string());
            s.chars().take(200).collect()
        })
}

/// 探测用的那条 user 消息：文本 + 一张 16×16 PNG。抽成纯函数只为单测能钉住
/// 「请求里真的拼上了 image part」（拼不上时端点会当成普通 ping 回 OK，假阴地报「支持」）。
/// 真判定力在 wire 层：本函数不发网络，只能证明我们把图递进了消息体。
fn probe_message() -> Message {
    Message::User {
        content: vec![
            UserContent::text("reply OK"),
            // 16×16 而非 1×1：部分严格端点会把过小尺寸判成无效图（假阴）。
            UserContent::image_base64(PROBE_PNG, Some(ImageMediaType::PNG), None),
        ],
    }
}

/// 连接失败诊断：失败的连接**根本没有活动会话**，走不了 `ai_send_message`（它强校验 session 存在）。
/// 这里复用 provider client 跑一条**单轮、无工具、不写库、不进对话**的 completion，把前端拼好的
/// 连接日志交给模型给出失败归因与下一步建议。与 `ai_provider_test` 同属「会话外的一次性调用」。
#[tauri::command]
pub async fn ai_diagnose_connect(
    app: AppHandle,
    provider_id: Option<String>,
    log: String,
) -> Result<String, AppError> {
    let log = log.trim().to_string();
    if log.is_empty() {
        return Err(AppError::Invalid("诊断日志不能为空".into()));
    }
    let (client, model) = blocking({
        let app = app.clone();
        let pid = provider_id.clone();
        move || {
            let st = state(&app);
            let p = match &pid {
                Some(id) => st
                    .db
                    .get_provider(id)?
                    .ok_or_else(|| AppError::Provider(format!("未找到 {id}")))?,
                None => st.db.get_default_provider()?.ok_or_else(|| AppError::Biz {
                    key: "err.provider.noDefault",
                    zh: "未配置默认 AI Provider，请在设置中添加",
                })?,
            };
            // 出网闸门：出网关闭时仅本地 Ollama 放行（与 ai_send_message 同口径）。
            super::settings::load_ai_settings(&st.db)?.check_egress(p.kind)?;
            let key = match &p.key_ref {
                Some(k) => st.vault.get(k)?,
                None => None,
            };
            if p.kind.needs_key() && key_missing(&key) {
                return Err(AppError::Biz {
                    key: "err.provider.noApiKey",
                    zh: "未保存 API Key，请在设置中填写",
                });
            }
            let client = Client::build(p.kind, p.base_url.as_deref(), key.as_ref())?;
            Ok((client, p.model))
        }
    })
    .await?;
    diagnose_completion(&client, &model, &log).await
}

/// 连接诊断的单轮 completion：系统提示限定「SSH/FTP 连接失败归因 + 下一步」，无工具、不落库。
async fn diagnose_completion(client: &Client, model: &str, log: &str) -> Result<String, AppError> {
    const PREAMBLE: &str = "你是终端 SSH/FTP 客户端的连接故障诊断助手。用户会给你一段连接过程日志\
（含各阶段：解析地址 / 建立连接 / 密钥协商 / TLS / 身份认证 / 登录 / 启动终端，以及最终错误）。\
请：① 用一句话点明最可能的失败原因；② 给出 2-4 条按优先级排列、可操作的下一步排查或修复建议\
（尽量具体，如改端口、核对用户名/密码/私钥、检查防火墙或 fail2ban、主机密钥变更如何处理等）。\
回答简洁、使用与日志一致的语言，不要复述日志本身、不要臆造日志里没有的信息。";
    let resp = client
        .complete(
            model,
            vec![Message::user(log)],
            vec![],
            CompleteParams {
                preamble: Some(PREAMBLE.to_string()),
                temperature: Some(0.3),
                max_tokens: Some(700),
            },
        )
        .await?;
    let mut out = String::new();
    for item in &resp.choice {
        if let AssistantContent::Text(t) = item {
            out.push_str(&t.text);
        }
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        return Err(AppError::Provider("模型未返回诊断内容".into()));
    }
    Ok(out)
}

// ─────────────────────────── 对话与消息归档 ───────────────────────────

/// 按连接（或全局）列对话，最近更新在前。
#[tauri::command]
pub async fn ai_conversation_list(
    app: AppHandle,
    connection_id: Option<String>,
) -> Result<Vec<AiConversation>, AppError> {
    blocking(move || state(&app).db.list_conversations(connection_id.as_deref())).await
}

#[tauri::command]
pub async fn ai_conversation_create(
    app: AppHandle,
    connection_id: Option<String>,
    title: Option<String>,
    provider_id: Option<String>,
) -> Result<AiConversation, AppError> {
    blocking(move || {
        state(&app).db.create_conversation(
            connection_id.as_deref(),
            title.as_deref(),
            provider_id.as_deref(),
        )
    })
    .await
}

#[tauri::command]
pub async fn ai_conversation_rename(
    app: AppHandle,
    id: String,
    title: String,
) -> Result<(), AppError> {
    if title.trim().is_empty() {
        return Err(AppError::Invalid("对话标题不能为空".into()));
    }
    blocking(move || state(&app).db.rename_conversation(&id, title.trim())).await
}

/// 删除对话及其全部消息（db 层手动级联），并级联删掉附件目录里属于它的图片文件。
/// 图片清理是**尽力而为**：单个文件删不掉只跳过，绝不该因此让「删除对话」失败。
#[tauri::command]
pub async fn ai_conversation_delete(app: AppHandle, id: String) -> Result<(), AppError> {
    blocking(move || {
        let st = state(&app);
        // 清单要先取：消息行一删就再也认不出哪些图属于本对话。
        let manifests = st.db.list_attachment_manifests(&id).unwrap_or_default();
        st.db.delete_conversation(&id)?;
        let refs: Vec<_> = manifests
            .iter()
            .flat_map(|m| attach::parse_manifest(Some(m.as_str())))
            .collect();
        attach::remove_many(&st.data_dir, &refs);
        Ok(())
    })
    .await
}

/// 取一个对话的最近 `limit` 条消息（时间正序，前端渲染 + 重建上下文）。
#[tauri::command]
pub async fn ai_message_list(
    app: AppHandle,
    conversation_id: String,
    limit: Option<i64>,
) -> Result<Vec<AiChatMessage>, AppError> {
    blocking(move || {
        state(&app)
            .db
            .list_messages(&conversation_id, limit.unwrap_or(HISTORY_LOOKBACK))
    })
    .await
}

/// 用量统计汇总（「用量统计」弹窗）：总计 + 按模型 + 按对话 + 近 14 天。
/// `tzOffsetSecs` 为前端本地时区偏移（秒，JS getTimezoneOffset 取反），供逐日分组换算。
#[tauri::command]
pub async fn ai_usage_stats(app: AppHandle, tz_offset_secs: i64) -> Result<UsageStats, AppError> {
    blocking(move || state(&app).db.usage_stats(tz_offset_secs)).await
}

// ─────────────────────────── 对话编排入口 ───────────────────────────

/// 发送一条用户消息入参。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessage {
    /// 目标会话（工具在此会话上执行；由 orchestrator 经 `ToolCtx` 绑定，不进 LLM schema）。
    pub session_id: String,
    pub text: String,
    /// `None` = 自动新建对话（响应回传其 id）。
    pub conversation_id: Option<String>,
    /// `None` = 用默认 provider。
    pub provider_id: Option<String>,
    /// 本轮有效能力（前端逐轮算好发来）：Agent 页签恒 `Agent`（全工具）；聊天页签按只读/读写
    /// 开关取 `chat`（只读问答，能查不能改）/`agent`。缺省取 `Agent`。
    #[serde(default)]
    pub mode: ChatMode,
    /// 本轮**主画布页签**（前端逐轮算好发来，与 `mode` 同源）：`agent` = 控制台主场（命令打进
    /// 会话 PTY，像手打一样回显执行）；`chat` = 聊天窗主场（命令恒后台静默 exec）。
    ///
    /// 为何不能只读库里的 `ai.rail_tab`：rail 在前端已兼作「按会话标签的视图态」
    /// （`collapseStore.restoreRailMode` 只改本地不落库），库里那份只是「最后一次显式持久化」
    /// 的残留——用户从快捷入口被切到聊天后回来，库里会停在 `chat`，于是终端页签里的命令
    /// 永远静默跑（屏上看不见执行）。缺省空串 = 旧前端/旁路，回落库值。
    #[serde(default)]
    pub rail: String,
    /// 前端本轮单调递增的轮次序号：`ai_send_message` 写入 `ai_turn[session_id]`，编排期每条
    /// `ai-step`/`ai-stream` 事件原样回带，供前端丢弃「已终止轮次的迟到事件」（跨轮串台兜底）。
    /// 缺省 0（旧前端/诊断旁路不传时），前端不会匹配任何真实轮 → 相关事件被丢弃。
    #[serde(default)]
    pub turn: u64,
    /// 前端解析后的「界面有效语言」（如「日本語」「English」「简体中文」，或插件语言名）。
    /// 后端用它驱动 `{{language}}` 宏，使 AI 回答语言严格跟随界面实际生效的语言（而非裸
    /// `ui.language` 里可能是 `system` 的原始值）。缺省空 = 回落库里的 `ui.language`。
    #[serde(default)]
    pub lang: String,
    /// AI 发起下载、需弹原生保存目录框时前端下发的**已本地化标题**（走前端 i18n 字典）：
    /// 经 `RunConfig → ToolCtx` 透传给 `transfer_file`，使 tools.rs 不写死任何展示串。
    /// 缺省空 = 不 `set_title`（用系统默认标题）。
    #[serde(default)]
    pub download_pick_title: String,
    /// 本轮随文发送的图片（前端已压缩归一为 png/jpeg）。空 = 纯文本轮，行为与改前完全一致。
    /// 带图先过 [`check_turn_images`]：能力位非 `Some(true)` 直接拒发（不做转文字降级）。
    /// `id` 非空 = 历史已落盘的图（「重试」原样重发）；为空 = 本轮新图（轮末归档时落盘）。
    #[serde(default)]
    pub images: Vec<TurnImage>,
    /// 本轮随发的本地文档（只递路径，读盘/抽取都在后端本轮开始时做）。空 = 无文档，行为与改前一致。
    ///
    /// 与 `images` 分两条入参（而不是合成一个附件数组）：图片必须递字节（粘贴/拖拽没有本机路径），
    /// 文档必须递路径（走 base64 往返是纯浪费）——两者在「谁读盘」上本就不同形。
    #[serde(default)]
    pub files: Vec<TurnFile>,
}

/// [`ai_stage_image`] 的返回体：原始字节 + 元信息，交前端 canvas 压缩归一。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StagedImage {
    /// 归一前的原始 mime。
    pub mime: String,
    /// 原始字节的 base64（不含 `data:` 前缀）。
    pub data: String,
    /// 解码后的真实字节数。
    pub bytes: usize,
    /// 文件名（已去目录），供缩略图条展示。
    pub name: String,
}

/// [`ai_get_attachment`] 的返回体。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentData {
    pub mime: String,
    pub data: String,
}

/// 📎 选文件入口的读图命令：**仅**供该入口使用——粘贴/拖拽拿得到 `File` 对象，前端自己能读；
/// 本地文件选择器只会回一个路径，WebView 读不了任意本地字节，故多走一道后端。
/// 不写库、不落盘：字节递过 IPC 交前端压缩，真正要发的 base64 回到 [`SendMessage::images`]
/// 才走落盘通道。
///
/// 纪律：只按扩展名白名单收图、只读 [`attach::MAX_STAGED_BYTES`] 以内的量；路径由系统文件
/// 对话框给出、本就是用户显式选择，不据此做任何目录跳转，不新增越路面。
#[tauri::command]
pub async fn ai_stage_image(path: String) -> Result<StagedImage, AppError> {
    blocking(move || {
        let p = Path::new(path.trim());
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        // 此处允许 png/jpg/webp/gif/bmp：收进来后由前端 canvas 统归一成 png/jpeg，
        // 故原始格式只决定「能不能读」，不决定「模型最终收到什么」。
        let mime = match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            _ => {
                return Err(AppError::Invalid(if ext.is_empty() {
                    "所选文件没有图片扩展名，请选 png/jpg/webp/gif/bmp".into()
                } else {
                    format!("不支持的图片格式：.{ext}（仅 png/jpg/webp/gif/bmp）")
                }));
            }
        };
        // 体积闸先于读盘：先 `fs::read` 再比大小等于拿 2GB 的 png 先把内存灌满再报错，
        // 与本模块 `probe`/`read_local` 的口径对齐（先 metadata 后读）。
        let md = std::fs::metadata(p).map_err(|e| AppError::Io(format!("读取图片信息失败：{e}")))?;
        if md.len() as usize > attach::MAX_STAGED_BYTES {
            return Err(AppError::Invalid(format!(
                "图片不得超过 {}MB，请先裁图或换图",
                attach::MAX_STAGED_BYTES / 1024 / 1024
            )));
        }
        let bytes = std::fs::read(p).map_err(|e| AppError::Io(format!("读取图片失败：{e}")))?;
        if bytes.is_empty() {
            return Err(AppError::Invalid("图片内容为空".into()));
        }
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image")
            .to_string();
        Ok(StagedImage {
            mime: mime.to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            bytes: bytes.len(),
            name,
        })
    })
    .await
}

/// 📎 待发区选完文件后的就地预探：只读头部判类型、查体积与受保护文件，**不抽取、不落盘、不发送**。
///
/// 为什么要这一道（而不是等发送时再拆）：目录、500MB 日志、`vault.key` 这类「选完就知道」的错
/// 必须当场回到 chip 上，否则表现是「点了发送但什么也没发生」——与本项目「失败内联展示」的既有规范相左。
#[tauri::command]
pub async fn ai_probe_attachment(path: String) -> Result<attach::ingest::Probe, AppError> {
    blocking(move || attach::ingest::probe(&path)).await
}

/// 历史气泡缩略图的按需拉取（按附件 id 直取）。清单元数据已随消息一起回传，字节只在这一步、
/// 只对本屏真要看的这张取（一次 IPC 换一张，不批量）。
///
/// **不开 asset protocol scope**：那是新增一个 CSP / 本地可读面，与本项目收紧权限的既有
/// 取向相左；按 id 取回 base64 的命令已足够，且 id 由本机随机生成、不可枚举。
#[tauri::command]
pub async fn ai_get_attachment(app: AppHandle, id: String) -> Result<AttachmentData, AppError> {
    blocking(move || {
        let id = id.trim().to_string();
        // id 由 `new_id` 生成（带连字符的 UUID）：拼路径前先硬拦一道，防参数越出附件目录。
        // 判据与 `attach::file_path` 同源——这里若只收字母数字，UUID 的 `-` 会被误杀，
        // 历史缩略图就永远取不到字节（只能退成文件名占位）。
        if !attach::is_safe_id(&id) {
            return Err(AppError::Invalid("附件 id 不合法".into()));
        }
        let st = state(&app);
        let (bytes, mime) = attach::read(&st.data_dir, &id)
            .ok_or_else(|| AppError::Invalid("附件不存在或已被删除".into()))?;
        Ok(AttachmentData {
            mime,
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        })
    })
    .await
}

/// 一轮对话的返回（在 orchestrator 的 [`crate::ai::orchestrator::TurnResult`] 上补 conversationId）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub conversation_id: String,
    pub text: String,
    pub rounds: usize,
    /// 本轮自动沉淀的记忆条数（前端提示「已记住 N 条」）。
    #[serde(default)]
    pub memories_saved: u32,
    /// `true` = 本轮因达往返上限优雅收尾（[`crate::ai::orchestrator::run_turn`] 返回 resumable）：
    /// 本轮已照常归档，前端出现「继续」按钮续跑；自然答复为 `false`。
    #[serde(default)]
    pub resumable: bool,
    /// 本轮 token 用量与上下文占用（provider 真值累加；缺报时为字符估算并标记 estimated）。
    #[serde(default)]
    pub usage: TurnUsage,
    /// 运行期系统提示词体检告警（分层兜底第 2 层）：dev 把模板改坏时回推一句提示，
    /// **不改配置、不自动回退**；`None` = 正常。前端据此弹非阻断告警，引导去检查或「恢复默认」。
    #[serde(default)]
    pub prompt_warning: Option<String>,
    /// 本轮图片未能归档落盘的一句话告警（答复已正常返回，**不阻断**）；`None` = 图已存好或本轮无图。
    #[serde(default)]
    pub attach_warning: Option<String>,
}

/// 跑一轮对话：解析 provider → 取 Key 建 client → 建/取对话 → 从归档重建历史 → 组装系统提示与
/// 预算 → 交给 [`run_turn`]（内部串起不可绕过的 `policy_check → interrupt → execute → sanitize`，
/// 并按需 emit 步骤/审批事件）→ 成功后归档本轮 user+assistant 文本。
///
/// 失败（网络/审批超时）时**不落库**，避免重试把同一句用户消息重复计入历史；达往返上限属
/// **优雅收尾**（run_turn 返回 Ok + resumable），本轮照常归档，前端据此提供「继续」续跑。
#[tauri::command]
pub async fn ai_send_message(app: AppHandle, params: SendMessage) -> Result<ChatReply, AppError> {
    let text = params.text.trim().to_string();
    // 带图无话是合法输入（贴张报错截图让 AI 看），故只在不带图时强约文本非空；
    // 纯文本轮的报错文案与行为与改前一致。
    if text.is_empty() && params.images.is_empty() && params.files.is_empty() {
        return Err(AppError::Invalid("消息不能为空".into()));
    }
    // 应用级问答（首页 AI 聊天）拿保留 id 当轮次键：它不在注册表里，显式放行；
    // 工具面在 prepare_turn 里钉死 Caps::NONE，不碰任何会话通路。
    if params.session_id != APP_CHAT_SESSION && !state(&app).sessions.contains(&params.session_id) {
        return Err(AppError::NoSuchSession(params.session_id.clone()));
    }

    // 带图轮先过入站闸门（能力位 + 张数/体积/mime）：不过即拒发，**不降级**、不悄悄改成只发文字。
    // 放在最前是为了绕开后面的环境预探测与记忆召回（它们有真开销，图根本发不出去时不该做）。
    if !params.images.is_empty() {
        check_turn_images(&app, params.provider_id.as_deref(), &params.images)?;
    }
    // 文档轮只过张数这一道零成本预检（读盘会报路径/体积错，见 `ingest::read_local`）。
    // **不查 vision 能力位**：文档走的是文字通路，跟看不看得见图无关。
    check_turn_files(&params.files)?;

    // 首问懒预探测目标环境（按会话缓存，只探一次）：开局就把 OS/权限/包管理器/init 喂进
    // 系统提示词，省掉模型为摸环境浪费的一轮工具往返。失败/超时静默落回占位语，不阻断本轮。
    ensure_session_env(&app, &params.session_id).await;

    // Mem0 语义召回（可选框架）：失败不阻断，回落本库关键词记忆
    let mut extra_mem = String::new();
    {
        let st = state(&app);
        if let Ok(s) = crate::commands::settings::load_ai_settings(&st.db) {
            if s.memory_enabled {
                if let Some(m0) =
                    crate::ai::mem0::from_settings(&s.mem0_url, &s.mem0_key, &s.mem0_user)
                {
                    if let Ok(hits) = m0.search(&params.session_id, &text, 5).await {
                        for h in hits {
                            if !extra_mem.is_empty() {
                                extra_mem.push('\n');
                            }
                            extra_mem.push_str("- ");
                            extra_mem.push_str(&h);
                        }
                    }
                }
            }
        }
    }

    // 本地 IO 段：把 client / RunConfig / 历史 / 工具注册表一次性备好（Key 只在此段出现，用完即弃）。
    let prepared = blocking({
        let app = app.clone();
        let session_id = params.session_id.clone();
        let provider_id = params.provider_id.clone();
        let conversation_id = params.conversation_id.clone();
        let text = text.clone();
        let extra_mem = extra_mem.clone();
        let mode = params.mode;
        let rail = params.rail.clone();
        let lang = params.lang.clone();
        let dl_pick_title = params.download_pick_title.clone();
        let files = params.files.clone();
        move || {
            prepare_turn(
                &app,
                &session_id,
                provider_id.as_deref(),
                conversation_id.as_deref(),
                &text,
                &extra_mem,
                mode,
                &rail,
                &lang,
                &files,
                &dl_pick_title,
            )
        }
    })
    .await?;

    let Prepared {
        client,
        cfg,
        registry,
        mut history,
        session_id,
        conversation_id,
        provider_id,
        connection_id,
        auto_extract,
        prompt_warning,
        files: file_refs,
        file_errors,
        model_text,
    } = prepared;

    // 预生成本轮 assistant 消息 id：既作 run_turn 内审计的关联键（audit_log.ai_message_id），
    // 又用于轮末落库该 assistant 消息，使「命令审计 ↔ AI 消息」双向可追。
    let assistant_msg_id = new_id();

    // 取消通道：进入网络段前按**轨**登记本轮的 oneshot 发送端（键 = `turn_key`），`ai_cancel_turn` 携同一
    // track 触发即由下面的 select 丢弃该轨 run_turn future（另一轨不受牵连）。语义=只停该轨 AI 编排，
    // 已经在远端跑起来的命令不受影响（终止不动远程）。
    let turn_key = crate::state::ai_turn_key(&cfg.track, &session_id);
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    state(&app)
        .ai_cancel
        .lock()
        .unwrap()
        .insert(turn_key.clone(), cancel_tx);
    // 登记本轮序号（按 `turn_key`）：run_turn 期间 emit_step/流式事件按该轨键读它回带 `turn`，前端据此丢弃
    // 已终止轮次的迟到事件并定位归属轨（chat/agent 各持一个在途 turn，互不覆盖）。进入网络段前写、轮次结束即清。
    state(&app)
        .ai_turn
        .lock()
        .unwrap()
        .insert(turn_key.clone(), params.turn);

    // 网络段：run_turn 自身持 `&app` emit 事件、按需挂起等审批。
    // `params.images` 为空时 run_turn 推入历史的仍是原来的 `Message::user(text)`（零行为变化）。
    // 传出去的是 `model_text`（原文 + 文档投送块）；库里存的仍是原文（见 prepare_turn 内注释）。
    let turn = run_turn(
        &app,
        &client,
        &cfg,
        &registry,
        &session_id,
        &mut history,
        &model_text,
        &assistant_msg_id,
        &params.images,
    );
    let result = tokio::select! {
        r = turn => r,
        _ = cancel_rx => {
            // 用户终止该轨：撤登记 + 清本轮轨迹（run_turn 被 drop，不会自行取走），返回专用码。
            state(&app).ai_cancel.lock().unwrap().remove(&turn_key);
            state(&app).ai_trace.lock().unwrap().remove(&turn_key);
            state(&app).ai_turn.lock().unwrap().remove(&turn_key);
            return Err(AppError::Canceled);
        }
    }?;
    // run_turn 正常返回（含其内部错误）：撤掉取消登记，避免 id 复用串到下一轮。
    state(&app).ai_cancel.lock().unwrap().remove(&turn_key);
    state(&app).ai_turn.lock().unwrap().remove(&turn_key);

    // 归档段：user + assistant 各一条（assistant 额外带本轮过程轨迹，回看历史时重现思考过程）。
    // 轨迹先序列化成串：失败不阻断归档（宁可不存过程，也不能不存答复）。
    let steps_json = serde_json::to_string(&result.steps).ok();
    // 本轮用量先拷出来：result.text 稍后才被 move 进闭包，usage 要在闭包里落真值 tokens。
    let result_usage = result.usage.clone();
    let (memories_saved, attach_warning) = blocking({
        let app = app.clone();
        let conversation_id = conversation_id.clone();
        let session_id = session_id.clone();
        let provider_id = provider_id.clone();
        let text = text.clone();
        let reply = result.text.clone();
        let assistant_msg_id = assistant_msg_id.clone();
        let connection_id = connection_id.clone();
        let steps_json = steps_json.clone();
        let result_usage = result_usage.clone();
        let images = params.images.clone();
        let file_refs = file_refs.clone();
        let file_errors = file_errors.join("；");
        move || {
            let st = state(&app);
            // 图片落盘（轮末、与归档同一段）：写失败只记一句告警、**不阻断归档**——
            // 宁可丢图（本轮已回答完，图只影响以后重看），不能连整轮对话一起丢。
            // 文档在 prepare 段已抽完并落了 IR，这里只把两份清单合成一份（同一列、同一条级联删除）。
            let (mut refs, warn_img) = match attach::persist(&st.data_dir, &images) {
                Ok(r) => (r, None),
                Err(e) => (
                    Vec::new(),
                    Some(format!("图片未能归档保存（本轮答复不受影响）：{e}")),
                ),
            };
            refs.extend(file_refs);
            let manifest = attach::manifest_json(&refs);
            let attach_warning = match (warn_img, file_errors.is_empty()) {
                (w, true) => w,
                (None, false) => Some(format!("部分附件未能读取：{file_errors}")),
                (Some(w), false) => Some(format!("{w}；部分附件未能读取：{file_errors}")),
            };
            let mut user = text_message(
                &conversation_id,
                &session_id,
                &provider_id,
                ChatRole::User,
                &text,
            );
            user.attachments = manifest;
            st.db.append_message(&user)?;
            // assistant 消息用预生成 id 落库，与本轮审计的 ai_message_id 对齐。
            let mut assistant = text_message(
                &conversation_id,
                &session_id,
                &provider_id,
                ChatRole::Assistant,
                &reply,
            );
            assistant.id = assistant_msg_id;
            assistant.steps = steps_json;
            // tokens 落本轮真值合计（入+出，含工具往返）；provider 缺报时 result_usage
            // 已是字符估算，口径与旧数据一致；user 条保留 text_message 内的单条估算。
            assistant.tokens = Some(result_usage.total_tokens as i64);
            // 上下文占用一并落库：重开历史时据此回填工具栏 ctx 环（修切换会话清空的 bug）。
            assistant.context_used = Some(result_usage.context_used as i64);
            assistant.context_window = Some(result_usage.context_window as i64);
            st.db.append_message(&assistant)?;
            // 自动提取记忆：仅开关开启时从本轮用户话语沉淀偏好/环境事实，
            // 命中凭证即丢弃（ai::memory::extract 内）；best-effort，失败不阻断对话。
            if auto_extract {
                // uteke 优先；失败/未启用再走规则抽取
                if let Some(u) = &st.uteke {
                    let ns = connection_id.as_deref();
                    let mut n = 0;
                    for m in crate::ai::memory::extract(&text, connection_id.as_deref()) {
                        let _ = u.remember(&m.content, &[], ns).map(|_| n += 1);
                    }
                    Ok((n, attach_warning))
                } else {
                    let candidates = crate::ai::memory::extract(&text, connection_id.as_deref());
                    let n = store_extracted(&st.db, candidates);
                    Ok((n, attach_warning))
                }
            } else {
                Ok((0, attach_warning))
            }
        }
    })
    .await
    .unwrap_or_default();

    // Mem0 框架：把本轮用户话语交给 Mem0 抽取入库（失败不阻断）
    if auto_extract {
        let st = state(&app);
        if let Ok(s) = crate::commands::settings::load_ai_settings(&st.db) {
            if let Some(m0) = crate::ai::mem0::from_settings(&s.mem0_url, &s.mem0_key, &s.mem0_user)
            {
                let _ = m0.add(&session_id, &text).await;
            }
        }
    }

    Ok(ChatReply {
        conversation_id,
        text: result.text,
        rounds: result.rounds,
        memories_saved,
        resumable: result.resumable,
        usage: result_usage,
        prompt_warning,
        attach_warning,
    })
}

/// 应答 `ai-approval-request` 事件；返回 `false` 表示请求已失效（超时/重复应答/未知 id）。
/// 与主机密钥裁决各自独立（[`crate::ai::approval::Pending`]）。裁决：
/// - `accept=false` → 拒绝（忽略 `edited` / `always`）；
/// - `accept=true` 且 `edited` 为空 → 批准；`always=true` 时 **本会话后续同类审批自动通过**；
/// - `accept=true` 且 `edited` 非空 → **编辑改判**：对改后命令重新分级。
///
/// `edited` 是用户改后的**命令串**（非凭证）；orchestrator 落审计前会脱敏。
#[tauri::command]
pub async fn ai_respond_approval(
    app: AppHandle,
    request_id: String,
    accept: bool,
    edited: Option<String>,
    always: Option<bool>,
) -> Result<bool, AppError> {
    let always = always.unwrap_or(false);
    let outcome = match edited
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(cmd) if accept => ApprovalOutcome::Edited(cmd),
        None if accept && always => ApprovalOutcome::ApprovedAlways,
        None if accept => ApprovalOutcome::Approved,
        _ => ApprovalOutcome::Denied,
    };
    Ok(state(&app).ai_approvals.respond(&request_id, outcome))
}

/// 应答 `ai-question-request` 事件（澄清反问）；返回 `false` 表示请求已失效（重复应答/未知 id/会话已清）。
/// 与命令审批各自独立（[`crate::ai::question::PendingQuestion`]）。裁决：
/// - `answer` 非空 → 用户作答（整卡选项/自填文本，多题/多选由前端拼成一段）；
/// - `answer` 空且 `ai_decides=true` → 用户让 AI 自行决定（新 UI 已无此出口，仅兼容兜底）；
/// - 两者皆空 → 视为未作答（回落让 AI 自行决定，不阻断编排）。
///
/// `answer` 只是用户选择的文本，**不含凭证**。
#[tauri::command]
pub async fn ai_respond_question(
    app: AppHandle,
    request_id: String,
    answer: Option<String>,
    ai_decides: Option<bool>,
) -> Result<bool, AppError> {
    let outcome = match answer
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(ans) => QuestionOutcome::Answer(ans),
        None if ai_decides.unwrap_or(false) => QuestionOutcome::AiDecides,
        None => QuestionOutcome::AiDecides,
    };
    Ok(state(&app).ai_questions.respond(&request_id, outcome))
}

/// 终止某会话**某一轨**（`track` = "agent"/"chat"）正在进行的 AI 编排轮次：按 `turn_key` 取出
/// [`AppState::ai_cancel`] 登记的发送端并触发，`ai_send_message` 的 select 随即丢弃该轨 run_turn future。
/// 无在途轮次则静默成功（幂等）。另一轨不受牵连（chat/agent 并发各自独立）。同时按轨清掉该轨挂起
/// 的审批/反问（drop 发送端、避免重同步列表再误冒这张卡的幽灵）。只中断 AI 侧，不向远端发任何中断信号。
#[tauri::command]
pub async fn ai_cancel_turn(app: AppHandle, session_id: String, track: String) -> Result<(), AppError> {
    let key = crate::state::ai_turn_key(&track, &session_id);
    if let Some(tx) = state(&app).ai_cancel.lock().unwrap().remove(&key) {
        let _ = tx.send(());
    }
    state(&app).ai_approvals.cancel_track(&session_id, &track);
    state(&app).ai_questions.cancel_track(&session_id, &track);
    Ok(())
}

/// 列出当前挂起的 AI 命令审批（[`crate::ai::approval::Pending`] 快照）。
/// 供**新挂载的前端窗口/重挂载**后重同步审批卡：`ai-approval-request` 事件是瞬时的，错过即
/// 看不到卡，而后端 `run_turn` 正挂在回执通道上等裁决。纯内存读、幂等，负载只含展示/裁决信息，
/// **绝不带凭证**（与事件同源 [`ApprovalPrompt`]）。
#[tauri::command]
pub async fn ai_list_pending_approvals(app: AppHandle) -> Result<Vec<ApprovalPrompt>, AppError> {
    Ok(state(&app).ai_approvals.list())
}

/// 列出当前挂起的 AI 澄清反问（[`crate::ai::question::PendingQuestion`] 快照）。
/// 与审批同口径的**双保险**：`ai-question-request` 事件是瞬时的，会话在反问挂起期间被弹出
/// 到新窗（或监听挂上前已挂起）就两窗都无卡可答，而后端 `run_turn` 正挂在回执上等作答。
/// 纯内存读、幂等，负载只含题干/选项（与事件同源 [`QuestionPrompt`]），**绝不带凭证**。
#[tauri::command]
pub async fn ai_list_pending_questions(app: AppHandle) -> Result<Vec<QuestionPrompt>, AppError> {
    Ok(state(&app).ai_questions.list())
}

// ─────────────────────────── 内部装配 ───────────────────────────

/// 一轮对话所需的、已装配好的运行件（全部 owned，可跨 `blocking` 边界返回）。
struct Prepared {
    client: Client,
    cfg: RunConfig,
    registry: ToolRegistry,
    history: Vec<Message>,
    session_id: String,
    conversation_id: String,
    provider_id: String,
    /// 会话所属连接（profile_id）：记忆 server 作用域绑定与召回键（可空：一次性连接）。
    connection_id: Option<String>,
    /// 是否自动提取记忆（= 记忆总开关 && 自动提取开关）。
    auto_extract: bool,
    /// 系统提示词体检告警（见 [`ChatReply::prompt_warning`]）：随本轮结果回推前端。
    prompt_warning: Option<String>,
    /// 本轮文档类附件的归档清单（图片仍在 `params.images`，轮末才落盘；两份清单归档时合并）。
    files: Vec<attach::AttachmentRef>,
    /// 读不上的附件原因（体积/权限/非文件…）：**同时**进模型正文与前端告警，不许静默。
    file_errors: Vec<String>,
    /// 发给模型的用户正文 = 原文 + 各文档投送块。库里存的仍是原文：气泡里塞 20k 字日志既没法看、
    /// 也让 token 统计与历史列表读出都跟着膨胀（与图片只存清单同一个理由）。
    model_text: String,
}

/// 首问懒预探测目标环境并写入会话缓存（`AppState.ai_env`）：供 [`prepare_turn`] 同步读缓存喂
/// 进系统提示词的【目标环境】块，省掉模型开局为摸环境浪费的一轮 `get_session_info` 往返。
///
/// 纪律：
/// - **每会话只探一次**——已登记（无论探测成败）即跳过，避免慢/断会话每轮都阻塞在短超时上；
///   成败都写（失败写空 `SessionEnv`），[`prepare_turn`] 用 `filter(!is_empty)` 区分有效与占位；
/// - **仅走 exec 通道的会话**（SSH/本地）能探；FTP/Telnet/Serial/Mosh 无后台执行通道，不登记、
///   不阻塞，系统提示直接落回占位语；
/// - 探测失败/超时静默吞掉（`unwrap_or_default`），绝不因拿不到环境而报错或阻断本轮。
async fn ensure_session_env(app: &AppHandle, session_id: &str) {
    if state(app).ai_env.lock().unwrap().contains_key(session_id) {
        return;
    }
    match state(app).sessions.kind(session_id) {
        Some(SessionKind::Ssh) | Some(SessionKind::Local) => {}
        _ => return,
    }
    let env = crate::ai::tools::probe_session_env(app, session_id)
        .await
        .unwrap_or_default();
    state(app)
        .ai_env
        .lock()
        .unwrap()
        .insert(session_id.to_string(), env);
}

/// 带图轮的入站校验（**只判不发**）：先张数/mime/体积（零成本的入参预检），再能力位（要读库）。
///
/// 能力位严格判 `Some(true)`：`None`（未探测）与 `Some(false)`（探测为不支持）同样拒发——
/// 本期**不做任何降级**（不转文字、不另找模型代看），拒发是唯一诚实的表现（宁不回答也不给出
/// 一张根本没看见的图的答案）。错误走 `err.*` 词典回前端，文案由前端掌握。
///
/// 体积预检只比 base64 串长度（不解码）：本轮最多 3 张、每张几 MB，把字节再解一遍纯粹为报错
/// 不值得；精确校验仍在 `attach::decode`（落盘/拼 part 时）跑第二道。
fn check_turn_images(
    app: &AppHandle,
    provider_id: Option<&str>,
    imgs: &[TurnImage],
) -> Result<(), AppError> {
    check_image_limits(imgs)?;
    let st = state(app);
    let provider = match provider_id {
        Some(id) => st
            .db
            .get_provider(id)?
            .ok_or_else(|| AppError::Provider(format!("未找到 {id}")))?,
        // 这里用 `ok_or` 而非 `ok_or_else`：结构体字面量无需延迟求值，写成闭包会被 clippy 判成多余。
        None => st.db.get_default_provider()?.ok_or(AppError::Biz {
            key: "err.provider.noDefault",
            zh: "未配置默认 AI Provider，请在设置中添加",
        })?,
    };
    if !vision_allowed(provider.supports_vision) {
        return Err(AppError::Biz {
            key: "err.ai.visionBlocked",
            zh: "当前模型未确认支持图片，请先在设置里测试或换模型",
        });
    }
    Ok(())
}

/// 张数 / mime / 体积三项入参预检（不碰库、不拼模型，抽成纯函数便于单测）。
///
/// 体积只比 base64 串长度（不解码）：本轮最多 3 张、每张几 MB，把字节再解一遍纯粹为报错
/// 不值得；精确校验仍在 `attach::decode`（落盘/拼 part 时）跑第二道。
fn check_image_limits(imgs: &[TurnImage]) -> Result<(), AppError> {
    if imgs.len() > attach::MAX_IMAGES_PER_TURN {
        return Err(AppError::Invalid(format!(
            "一次最多发送 {} 张图片",
            attach::MAX_IMAGES_PER_TURN
        )));
    }
    for img in imgs {
        if attach::mime_ext(&img.mime).is_none() {
            return Err(AppError::Invalid(format!(
                "不支持的图片类型：{}（仅 png/jpeg）",
                img.mime
            )));
        }
        // base64 → 字节的理论上界（含 padding）：超过就一定能判定超限，无需真解。
        if img.data.len() > attach::MAX_IMAGE_BYTES * 4 / 3 + 4 {
            return Err(AppError::Invalid(format!(
                "单张图片不得超过 {}MB",
                attach::MAX_IMAGE_BYTES / 1024 / 1024
            )));
        }
    }
    Ok(())
}

/// 能力闸门判据：**只有 `Some(true)` 放行**。`None`（未探测）与 `Some(false)`（探测不支持）
/// 同拒：未探测就发图，等于拿用户的一句话去堵一个不知道能不能看图的模型。
fn vision_allowed(supports_vision: Option<bool>) -> bool {
    supports_vision == Some(true)
}

/// 文档轮的入站预检：只看个数（零成本）。体积/路径/受保护文件这些要真读盘才能知道，
/// 交给 [`crate::ai::attach::ingest::read_local`] 逐份报错（一份坏不影响其余）。
fn check_turn_files(files: &[TurnFile]) -> Result<(), AppError> {
    if files.len() > attach::MAX_FILES_PER_TURN {
        return Err(AppError::Invalid(format!(
            "一次最多附加 {} 个文档",
            attach::MAX_FILES_PER_TURN
        )));
    }
    Ok(())
}

/// 组装一轮运行件（纯本地 IO，无网络）：解析 provider → 取 Key 建 client → 建/取对话 →
/// 重建历史 → 快照 + 系统提示 + token 预算 → `RunConfig`。
///
/// API Key 仅用于 [`Client::build`] 的 HTTP header，函数返回前 `Secret` 即离开作用域清零，
/// **绝不进 `history` / `cfg`**。
fn prepare_turn(
    app: &AppHandle,
    session_id: &str,
    provider_id: Option<&str>,
    conversation_id: Option<&str>,
    text: &str,
    extra_mem: &str,
    mode: ChatMode,
    // 本轮主画布页签（前端逐轮发来）：`"agent"` = 命令打进 PTY，`"chat"` = 恒静默 exec；
    // 空串 = 未传（旧前端/旁路），回落库里的 `ai.rail_tab`。见 [`SendMessage::rail`]。
    rail: &str,
    lang: &str,
    files: &[TurnFile],
    dl_pick_title: &str,
) -> Result<Prepared, AppError> {
    let st = state(app);

    let provider = match provider_id {
        Some(id) => st
            .db
            .get_provider(id)?
            .ok_or_else(|| AppError::Provider(format!("未找到 {id}")))?,
        None => st.db.get_default_provider()?.ok_or_else(|| AppError::Biz {
            key: "err.provider.noDefault",
            zh: "未配置默认 AI Provider，请在设置中添加",
        })?,
    };

    // AI/安全设置（出网开关 / 审批模式 / 命令风险扫描层级）每轮从库加载：先过出网闸门，再驱动下方 PolicyEngine。
    let settings = super::settings::load_ai_settings(&st.db)?;
    // 出网闸门：出网关闭时云端 provider 一律阻断，仅本地 Ollama（数据不出本机）放行。
    settings.check_egress(provider.kind)?;

    let api_key = match &provider.key_ref {
        Some(k) => st.vault.get(k)?,
        None => None,
    };
    if provider.kind.needs_key() && key_missing(&api_key) {
        return Err(AppError::Biz {
            key: "err.provider.noApiKey",
            zh: "未保存 API Key，请在设置中填写",
        });
    }
    let client = Client::build(
        provider.kind,
        provider.base_url.as_deref(),
        api_key.as_ref(),
    )?;

    let conv_id = match conversation_id {
        Some(id) => {
            st.db
                .get_conversation(id)?
                .ok_or_else(|| AppError::Invalid(format!("对话不存在：{id}")))?;
            id.to_string()
        }
        None => {
            // 「只贴图不写字」的首轮没有文本可取名：回落到一句固定标题，不让对话顶一个空标题。
            let src = if text.is_empty() {
                "图片提问"
            } else {
                text
            };
            let title: String = src.chars().take(AUTO_TITLE_CHARS).collect();
            // 对话绑定连接档案，上下文按服务器隔离（不串台）
            let conn = st.sessions.profile_id(session_id);
            st.db
                .create_conversation(conn.as_deref(), Some(&title), Some(&provider.id))?
                .id
        }
    };

    // 从归档重建历史：仅 user/assistant 文本轮。工具结果是轮内瞬态、系统提示每轮现组装，均不重建。
    //
    // 带图的 user 轮只有**最近 `attach::RESEND_WINDOW` 个**会把原图重新拼回消息：API 无状态、
    // 历史整体重发，若每张旧图都重发就是重复计费。更早的退化为纯文本，且**不加占位说明**：
    // 同轮的 assistant 回复已在历史里，它本身就是图片内容的文字化载体（这即「不做 caption」
    // 决策的落地形态）。图文件被手工删了也静默退纯文本，不报错、不阻断本轮。
    //
    // 文档则**每轮都带**（走 [`attach::file_blocks`]，与本轮发送逐字同一条）：文本比图像便宜一到
    // 两个量级，不存在「窗口外失忆」——这正是行业里没人为文档做重发窗口的原因。
    let archived = st.db.list_messages(&conv_id, HISTORY_LOOKBACK)?;
    let resend_ids = attach::resend_ids(
        &archived
            .iter()
            .map(|m| {
                (
                    m.id.as_str(),
                    // 「本轮带图」要看清单里的真 kind，不能只看 `attachments.is_some()`：
                    // 文档轮现在也会占那一列，误计会把图片重发窗口白白吃掉。
                    m.role == ChatRole::User
                        && attach::parse_manifest(m.attachments.as_deref())
                            .iter()
                            .any(|r| r.kind == AttachKind::Image),
                )
            })
            .collect::<Vec<_>>(),
    );
    let mut history: Vec<Message> = Vec::new();
    for m in archived {
        let Some(content) = m.content.as_deref() else {
            continue;
        };
        match m.role {
            ChatRole::User => {
                let refs = attach::parse_manifest(m.attachments.as_deref());
                let mut body = String::from(content);
                let blocks = attach::file_blocks(&st.data_dir, &refs);
                if !blocks.is_empty() {
                    body.push_str("\n\n");
                    body.push_str(&blocks);
                }
                let imgs: Vec<&attach::AttachmentRef> = refs
                    .iter()
                    .filter(|r| r.kind == AttachKind::Image)
                    .collect();
                if imgs.is_empty() || !resend_ids.contains(&m.id) {
                    history.push(Message::user(body));
                    continue;
                }
                let mut parts = Vec::with_capacity(imgs.len());
                for r in imgs {
                    let Some((bytes, mime)) = attach::read(&st.data_dir, &r.id) else {
                        continue;
                    };
                    // mime 被改过/拼不出 part 的那张直接跳过：宁可本轮少带一张图，
                    // 也不能因为一张残图把整段历史重建顶掉。
                    if let Ok(part) = attach::image_part(&bytes, &mime) {
                        parts.push(part);
                    }
                }
                history.push(attach::user_message(&body, parts));
            }
            ChatRole::Assistant => history.push(Message::assistant(content)),
            ChatRole::System | ChatRole::Tool => {}
        }
    }

    let budget = Budget::new(provider.max_context_tokens);
    // 会话能力档位（协议定死的物理事实，不是运行期状态）：决定本轮下发哪些工具。
    // 应用级问答（保留 id）钉死 NONE：只留不碰会话通路的工具（问答/出网/附件/知识库）。
    // 绝不能走下面的 unwrap_or_default 回落（= SSH 全量工具面，那是给「刚断开」的兑底），
    // 否则模型会拿到一批必然失败的工具。
    // 其余会话已不在注册表（刚断开）时退回默认 SSH = 全量工具面，让后面的通路各自报错引导重连，
    // 而不是在这里把工具集静默削成「只剩出网查询」——那种表现更难诊断。
    let caps = if session_id == APP_CHAT_SESSION {
        Caps::NONE
    } else {
        st.sessions.kind(session_id).unwrap_or_default().caps()
    };
    let snapshot = st
        .sessions
        .ring(session_id)
        .map(|r| r.snapshot(SNAPSHOT_BYTES))
        .unwrap_or_default();
    // 运行期脱敏层级（云 provider 强制 ≥ Strict）：既驱动 RunConfig，也用于记忆召回注入前的再脱敏。
    let sanitize_level = enforce_cloud_sanitize(provider.kind, provider.sanitize_level);
    // 本轮待发文档：读盘 → 判型 → 抽取 → 脱敏 → 落 IR，全部在本段（blocking 线程、网络段之前）。
    // 单个文件失败不阻断本轮，但原因既要给模型（不然它会凭文件名瞎猜）也要给用户（不然他以为 AI 看了全文）。
    let sanitizer = Sanitizer::new(sanitize_level);
    let mut file_refs: Vec<attach::AttachmentRef> = Vec::new();
    let mut file_errors: Vec<String> = Vec::new();
    for f in files {
        match attach::ingest::ingest_turn_file(&st.data_dir, f, &sanitizer) {
            Ok(r) => file_refs.push(r),
            Err(e) => file_errors.push(format!(
                "{}：{e}",
                f.name.clone().unwrap_or_else(|| f.path.clone())
            )),
        }
    }
    // 会话所属连接（profile_id）：记忆的 server 作用域绑定与召回键（后端权威，前端不可伪造）。
    let connection_id = st.sessions.profile_id(session_id);
    // 长期记忆召回：总开关开启时按 connection_id + 全局召回、相关性打分、小额预算内
    // 注入；注入前按 sanitize_level 再脱敏。顺带按保留期清理过期记忆（best-effort）。
    let mut mem_block = if settings.memory_enabled {
        if let Some(u) = &st.uteke {
            match u.recall(text, 8, connection_id.as_deref()) {
                Ok(hits) if !hits.is_empty() => hits
                    .into_iter()
                    .map(|h| format!("- {}", h.trim()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => crate::ai::memory::recall_block(
                    &st.db,
                    connection_id.as_deref(),
                    text,
                    sanitize_level,
                    budget.memory_tokens(),
                ),
            }
        } else {
            let _ = st.db.prune_memory(settings.memory_retention_days);
            crate::ai::memory::recall_block(
                &st.db,
                connection_id.as_deref(),
                text,
                sanitize_level,
                budget.memory_tokens(),
            )
        }
    } else {
        String::new()
    };
    if !extra_mem.trim().is_empty() {
        if !mem_block.trim().is_empty() {
            mem_block.push('\n');
        }
        mem_block.push_str(extra_mem.trim());
    }
    // env 从首问预探测的会话缓存读取（见 ensure_session_env）：命中且非空就喂进系统提示词；
    // 未命中（非 exec 会话 / 探测失败 / 尚未登记）传 None，落回「按需自调 get_session_info」占位语。
    let cached_env = st
        .ai_env
        .lock()
        .unwrap()
        .get(session_id)
        .filter(|e| !e.is_empty())
        .cloned();
    // 一次读设置拿齐两个偏好：回答语言 + 回复详略档（精简/标准/详细）。
    let (ui_language, verbosity) = crate::commands::settings::load_ai_settings(&state(app).db)
        .map(|s| (s.ui_language.clone(), s.reply_verbosity))
        .unwrap_or_else(|_| ("system".into(), Default::default()));
    // 回答语言以前端每轮解析后发来的「界面有效语言」为准（前端才知道 system→OS→已装语料包
    // 的最终落地值）；前端未传（旧前端/旁路）时回落库里的 `ui.language` 原值。
    let lang = if lang.trim().is_empty() {
        ui_language.as_str()
    } else {
        lang.trim()
    };
    // 系统提示词模板：取 dev 覆盖版（空即内置 DEFAULT_TEMPLATE，由 effective_prompt_template 收敛）。
    let template = super::settings::effective_prompt_template(&st.db)?;
    let preamble = build_system_prompt_with(
        cached_env.as_ref(),
        &snapshot,
        budget.snapshot_tokens(),
        &mem_block,
        &lang,
        verbosity,
        settings.allow_plaintext_password,
        mode,
        caps,
        &template,
    );
    // 运行期体检（第 2 层兜底）：照常把 dev 配的提示词发给模型，仅额外回推一条告警——
    // 不自动回退、不阻断本轮（这是 dev 明确写下去的东西，系统不该背着他改）。
    let prompt_warning = context::prompt_health_warning(&preamble);
    let history = context::trim_history_to_budget(&history, budget.history_tokens());

    // 用户自定义黑/白/写名单每轮从库加载（仅启用中的），注入 PolicyEngine 与内置分级合并（p4-3）。
    let policy_rules: Vec<UserRule> = st
        .db
        .list_policy_rules(None, true)?
        .iter()
        .map(user_rule_from)
        .collect();

    // 本轮发起轨（与 `exec_console` 同源归一，但不受 PTY 影响）：前端逐轮发来的 `rail` 为准，
    // 空串（旧前端/旁路）回落库里的 `ai.rail_tab`。用于在途状态复合键 `turn_key` 与随审批/反问
    // 事件回带，使同一条连接的 chat/agent 并发轮次互不串台。
    let track = match rail.trim() {
        "agent" => "agent",
        "chat" => "chat",
        _ => {
            if matches!(settings.rail, crate::commands::settings::RailTab::Agent) {
                "agent"
            } else {
                "chat"
            }
        }
    };
    let cfg = RunConfig {
        model: provider.model.clone(),
        preamble,
        temperature: provider.temperature,
        // 输出上限交给 provider 默认；窗口预算已用于历史/快照裁剪。
        max_tokens: None,
        scan_level: settings.scan_level,
        // 运行期再兜一次云 provider 强制 ≥ Strict（与 save 时一致，双保险）。
        sanitize_level,
        // 全审批模式：只读命令也需人工批准；由设置项 approval.mode 驱动。
        approve_readonly: settings.approval_mode.approve_readonly(),
        policy_rules,
        // 单轮最大往返数：设置项 ai.max_tool_rounds（load 已钳制），达上限走优雅收尾+「继续」。
        max_tool_rounds: settings.max_tool_rounds as usize,
        // 工具访问范围 = 前端每轮算好发来的有效能力 `mode`：Agent 页签恒为 agent（全能力）；
        // 聊天页签按「只读/读写」开关取 chatMode。ReadOnly 时只下发只读工具、非只读命令执行期直接拒绝。
        tool_access: mode.tool_access(),
        // 执行通道逐轮快照（与 mode 同构）：Agent 页签下命令恒打进会话 PTY（像手打一样回显执行），
        // 聊天页签恒后台 exec；无 PTY 的会话（FTP 等）也落回 exec。本轮开始定死，
        // 中途切页签/改设置不再影响在途轮次（tools.rs 直读此字段，不再实时读库）。
        // 页签以前端逐轮发来的为准（库里 `ai.rail_tab` 只是按会话视图态的残留，见 SendMessage::rail）；
        // 前端未传时才回落库值。
        exec_console: caps.contains(Caps::PTY)
            && match rail.trim() {
                "agent" => true,
                "chat" => false,
                _ => matches!(settings.rail, crate::commands::settings::RailTab::Agent),
            },
        // 发起轨（与 exec_console 同源归一，见上）：供 `ToolCtx` 拼 `turn_key`、随审批/反问事件回带。
        track: track.to_string(),
        // 窗口总量透传给编排层，仅供轮末回传「上下文占用」分母（0=未配置时前端隐藏）。
        max_context_tokens: provider.max_context_tokens.max(0) as usize,
        // AI 下载原生保存目录框标题：前端本地化后随轮下发，经 ToolCtx 透传给 transfer_file。
        dl_pick_title: dl_pick_title.to_string(),
    };

    // 给模型的用户正文：原文 + 投送块。走 [`attach::file_blocks`] 而不是直接拼 `Ingested.ir`，
    // 是为了让「本轮发送」与「历史重建」逐字同一条渲染通路（两处不一致最后都变成难查的时序 bug）。
    let blocks = attach::file_blocks(&st.data_dir, &file_refs);
    let mut model_text = text.trim().to_string();
    if !blocks.is_empty() {
        if !model_text.is_empty() {
            model_text.push_str("\n\n");
        }
        model_text.push_str(&blocks);
    }
    for e in &file_errors {
        model_text.push_str("\n\n（附件未能读取：");
        model_text.push_str(e);
        model_text.push_str("）");
    }

    Ok(Prepared {
        client,
        cfg,
        registry: ToolRegistry::assemble(caps, crate::ai::tools::plugin_tools(app)),
        history,
        session_id: session_id.to_string(),
        conversation_id: conv_id,
        provider_id: provider.id.clone(),
        connection_id,
        auto_extract: settings.memory_enabled && settings.memory_auto_extract,
        prompt_warning,
        files: file_refs,
        file_errors,
        model_text,
    })
}

fn save_provider(st: &AppState, mut input: ProviderInput) -> Result<AiProvider, AppError> {
    if input.name.trim().is_empty() {
        return Err(AppError::Invalid("Provider 名称不能为空".into()));
    }
    if input.model.trim().is_empty() {
        return Err(AppError::Invalid("模型名不能为空".into()));
    }

    let mut p = match input.id.as_deref() {
        Some(id) => st
            .db
            .get_provider(id)?
            .ok_or_else(|| AppError::Provider(format!("未找到 {id}")))?,
        None => AiProvider::new(input.name.clone(), input.kind, input.model.clone()),
    };

    p.name = input.name.trim().to_string();
    p.kind = input.kind;
    p.base_url = input
        .base_url
        .take()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    p.model = input.model.trim().to_string();
    p.supports_tools = input.supports_tools;
    // 三态兜底：`None` 也照写（= 清回「未探测」）。用户在表单里改判即覆盖探测结果；
    // 而下次点「测试」会把探测值重新写回（测试是显式动作，重探是它的本义，不算推翻用户意图）。
    p.supports_vision = input.supports_vision;
    p.max_context_tokens = input
        .max_context_tokens
        .unwrap_or(p.max_context_tokens)
        .max(1024);
    p.temperature = input.temperature;
    p.extra = input.extra.take();
    // 云 provider 脱敏层级强制 ≥ Strict（自动升级）。
    p.sanitize_level = enforce_cloud_sanitize(p.kind, input.sanitize_level);
    // 勾选状态完整同步（取消勾选要能清掉默认位）；库里尚无默认时自动把本条设为默认
    p.is_default = input.is_default;
    p.updated_at = now_ms();

    apply_provider_key(st, &mut p, &mut input)?;
    if !p.is_default && st.db.get_default_provider()?.is_none() {
        p.is_default = true;
    }
    // upsert 内部已处理 is_default 互斥（置位时先清其余行），无需再调 set_default_provider。
    st.db.upsert_provider(&p)?;
    flags_provider(st, p)
}

/// API Key 只在「显式提供」或「显式清除」时改动 Vault；编辑不填 Key 即保持原样。
fn apply_provider_key(
    st: &AppState,
    p: &mut AiProvider,
    input: &mut ProviderInput,
) -> Result<(), AppError> {
    if input.clear_key {
        st.vault.delete(&keys::of_ai_provider(&p.id))?;
        p.key_ref = None;
        return Ok(());
    }
    if let Some(secret) = input.api_key.take() {
        if !secret.is_empty() {
            let key = keys::ai_provider(&p.id);
            st.vault.store(&key, &secret)?;
            p.key_ref = Some(key);
        }
    }
    Ok(())
}

/// 依 Vault 回填 `hasKey`（**绝不回传 Key 内容**）。
fn flags_provider(st: &AppState, mut p: AiProvider) -> Result<AiProvider, AppError> {
    p.has_key = match &p.key_ref {
        Some(k) => st.vault.contains(k)?,
        None => false,
    };
    Ok(p)
}

/// 云 provider（非本地 Ollama）脱敏层级强制 ≥ Strict；`Custom`（偏执）已高于 Strict，保留。
pub(crate) fn enforce_cloud_sanitize(kind: ProviderKind, level: SanitizeLevel) -> SanitizeLevel {
    if kind == ProviderKind::Ollama {
        return level;
    }
    match level {
        SanitizeLevel::Loose | SanitizeLevel::Standard => SanitizeLevel::Strict,
        SanitizeLevel::Strict | SanitizeLevel::Custom => level,
    }
}

/// db 层策略规则 → policy 层 [`UserRule`]（policy 保持纯逻辑、不依赖 db，故在命令层桥接）。
/// 用 `effective_action()`：显式 action 优先，否则按 kind 取隐含动作（白→放行/写→审批/黑→拦截）。
fn user_rule_from(r: &PolicyRule) -> UserRule {
    let effect = match r.effective_action() {
        RuleAction::Allow => RuleEffect::Allow,
        RuleAction::Approval => RuleEffect::NeedsApproval,
        RuleAction::Block => RuleEffect::Block,
    };
    UserRule::new(effect, r.pattern.clone(), r.note.clone())
}

/// Key 是否缺失/为空（需要 Key 的 provider 据此拦在建 client 之前）。
fn key_missing(key: &Option<Secret>) -> bool {
    match key {
        Some(s) => s.is_empty(),
        None => true,
    }
}

/// 沉淀自动提取的记忆候选：去重后入库（同作用域+连接下相同内容不重复存）。
/// best-effort：任何失败都不阻断对话主流程（记忆是增强项）。
fn store_extracted(db: &Db, candidates: Vec<Memory>) -> u32 {
    let mut n = 0;
    for m in candidates {
        let existing = db
            .list_memory(Some(m.scope), m.connection_id.as_deref(), 500)
            .unwrap_or_default();
        let dup = existing
            .iter()
            .any(|e| e.content.trim() == m.content.trim());
        if !dup && db.upsert_memory(&m).is_ok() {
            n += 1;
        }
    }
    n
}

/// 造一条文本归档消息（user/assistant 共用）；`tokens` 用近似估算，够统计用。
fn text_message(
    conversation_id: &str,
    session_id: &str,
    provider_id: &str,
    role: ChatRole,
    content: &str,
) -> AiChatMessage {
    AiChatMessage {
        id: new_id(),
        conversation_id: Some(conversation_id.to_string()),
        connection_id: None,
        session_id: Some(session_id.to_string()),
        provider_id: Some(provider_id.to_string()),
        role,
        content: Some(content.to_string()),
        tool_call_id: None,
        tool_name: None,
        tool_args: None,
        steps: None,
        tokens: Some(context::estimate_tokens(content) as i64),
        context_used: None,
        context_window: None,
        // 默认无附件：带图的 user 轮由调用方在归档前覆写（见 `ai_send_message`）。
        attachments: None,
        created_at: now_ms(),
    }
}

// ────────────────── 系统提示词模板（开发者模式）──────────────────

/// 模板面板 DTO：当前存的覆盖模板 + 是否被 dev 改过 + 内置默认（供编辑初值/展示）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    /// 库里存的覆盖模板（空 = 未覆盖，走内置默认）。
    pub template: String,
    /// 是否已被 dev 覆盖（`true` 时前端亮「恢复默认」）。
    pub customized: bool,
    /// 内置默认模板（真相源，编译进二进制，供展示/一键复制）。
    pub default_template: String,
}

/// 预览结果：用样例动态块渲染后的成品 + 体检告警（不写库、不影响真实轮次）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptPreview {
    pub rendered: String,
    pub warning: Option<String>,
}

/// 组装模板 DTO（get/set/reset 共用，避免三处重复拼库读+回落）。
fn prompt_template_view(db: &crate::db::Db) -> Result<PromptTemplate, AppError> {
    let raw = super::settings::get_prompt_template(db)?.unwrap_or_default();
    Ok(PromptTemplate {
        customized: !raw.trim().is_empty(),
        template: raw,
        default_template: context::DEFAULT_TEMPLATE.to_string(),
    })
}

/// 读取系统提示词模板（开发者模式面板用；纯文本、无凭证）。
#[tauri::command]
pub async fn ai_prompt_get(app: AppHandle) -> Result<PromptTemplate, AppError> {
    blocking(move || prompt_template_view(&state(&app).db)).await
}

/// 写入 dev 覆盖模板；传空/纯空白 = 清除覆盖（回落内置默认）。返回落库后的视图。
#[tauri::command]
pub async fn ai_prompt_set(app: AppHandle, template: String) -> Result<PromptTemplate, AppError> {
    blocking(move || {
        let db = &state(&app).db;
        super::settings::set_prompt_template(db, &template)?;
        prompt_template_view(db)
    })
    .await
}

/// 恢复默认：删覆盖键，模板回到内置。
#[tauri::command]
pub async fn ai_prompt_reset(app: AppHandle) -> Result<PromptTemplate, AppError> {
    blocking(move || {
        let db = &state(&app).db;
        super::settings::reset_prompt_template(db)?;
        prompt_template_view(db)
    })
    .await
}

/// 预览：用传入模板（空则用内置默认）以样例动态块渲染，让 dev 直观看到每个宏展开成什么。
/// 不走真实会话（用写死的样例 env/snapshot/memory）、不碰数据库，纯字符串计算，故无 `app`/`blocking`。
#[tauri::command]
pub async fn ai_prompt_preview(template: String, lang: String) -> Result<PromptPreview, AppError> {
    let tmpl = if template.trim().is_empty() {
        context::DEFAULT_TEMPLATE.to_string()
    } else {
        template
    };
    // 回答语言宏以前端解析后的界面有效语言展开；未传时用样例值（体现“跟随提问语言”默认形态）。
    let reply_lang = if lang.trim().is_empty() {
        "system"
    } else {
        lang.trim()
    };
    // 样例输入：SSH 全能力 + 常见环境块，让 dev 看到宏展开后的真实形态。
    let env = context::SessionEnv::parse(
        "os=Linux\nkernel=5.15.0\narch=x86_64\ndistro=Ubuntu 22.04 LTS\nshell=/bin/bash\ncwd=/home/demo\nroot=no\nsudo=yes\npkg=apt-get\ninit=systemd\n",
    );
    let rendered = build_system_prompt_with(
        Some(&env),
        "$ ls\nnotes.txt  deploy.sh",
        4096,
        "- 偏好 systemctl 管服务",
        reply_lang,
        context::ReplyVerbosity::Concise,
        true,
        ChatMode::Agent,
        SessionKind::Ssh.caps(),
        &tmpl,
    );
    let warning = context::prompt_health_warning(&rendered);
    Ok(PromptPreview { rendered, warning })
}

#[cfg(test)]
mod tests {
    use super::*;

    ///云端 provider（非 Ollama）脱敏层级强制 ≥ Strict；Custom（偏执）更高，保留不下调。
    #[test]
    fn cloud_sanitize_forced_to_at_least_strict() {
        for kind in [
            ProviderKind::Openai,
            ProviderKind::Anthropic,
            ProviderKind::OpenaiCompatible,
        ] {
            assert_eq!(
                enforce_cloud_sanitize(kind, SanitizeLevel::Loose),
                SanitizeLevel::Strict,
                "{kind:?} 基础应被抬到严格"
            );
            assert_eq!(
                enforce_cloud_sanitize(kind, SanitizeLevel::Standard),
                SanitizeLevel::Strict,
                "{kind:?} 标准应被抬到严格"
            );
            assert_eq!(
                enforce_cloud_sanitize(kind, SanitizeLevel::Strict),
                SanitizeLevel::Strict
            );
            // 偏执已高于严格，不被下调
            assert_eq!(
                enforce_cloud_sanitize(kind, SanitizeLevel::Custom),
                SanitizeLevel::Custom
            );
        }
    }

    /// 本地 Ollama 数据不出本机，脱敏层级原样保留（离线模式）。
    #[test]
    fn local_ollama_sanitize_preserved() {
        for level in [
            SanitizeLevel::Loose,
            SanitizeLevel::Standard,
            SanitizeLevel::Strict,
            SanitizeLevel::Custom,
        ] {
            assert_eq!(enforce_cloud_sanitize(ProviderKind::Ollama, level), level);
        }
    }

    fn turn_img(mime: &str, data: &str) -> TurnImage {
        TurnImage {
            id: None,
            mime: mime.into(),
            name: None,
            data: data.into(),
        }
    }

    #[test]
    fn image_limits_reject_count_mime_and_size() {
        assert!(check_image_limits(&[]).is_ok(), "无图轮不该进这道闸门");
        assert!(check_image_limits(&[turn_img("image/png", "iVBORw0KGgo=")]).is_ok());
        // 超张数：这道闸门只看长度，字节内容是什么无关
        let many = vec![turn_img("image/png", "x"); attach::MAX_IMAGES_PER_TURN + 1];
        assert!(check_image_limits(&many).is_err(), "超张数即拒");
        // 白名单外 mime（前端已归一成 png/jpeg，走到这里就是异常）
        assert!(check_image_limits(&[turn_img("image/webp", "x")]).is_err());
        // 体积：base64 串长超过「4MB 字节的理论上界 + padding」即拒（不真解码）
        let huge = "A".repeat(attach::MAX_IMAGE_BYTES * 4 / 3 + 100);
        assert!(check_image_limits(&[turn_img("image/jpeg", &huge)]).is_err());
    }

    #[test]
    fn vision_gate_opens_only_on_confirmed_support() {
        // 「未探测」与「探测不支持」必须同拒：否则一个没测过的模型会收到它看不见的图。
        assert!(vision_allowed(Some(true)));
        assert!(!vision_allowed(Some(false)));
        assert!(!vision_allowed(None));
    }

    #[test]
    fn probe_message_carries_an_image_part() {
        // 探测的全部判定力在于「请求里确实带了图」：少了 image part，不支持视觉的端点
        // 也会回 OK，探出来就是一片假阴的「支持」。
        let Message::User { content } = probe_message() else {
            panic!("探测消息应为 user 角色");
        };
        assert_eq!(content.len(), 2, "文本 + 图");
        assert!(matches!(content[1], UserContent::Image(_)));
    }

    /// 探测图必须是一张**完整的 PNG**。
    ///
    /// 为什么单独钉这条：上面那条只能证明「请求里拼上了图」，证明不了图本身合法。
    /// 这个常量曾因手工续行少了一段，端点直接回 `400 invalid base64 format`，而
    /// `probe_vision` 把任何失败都归为「不支持」——结果所有模型都被误标成没有视觉能力，
    /// 单测却全绿。
    ///
    /// 不拉 `image` 依赖做真解码（为一个常量加一个解码器不值），改钉三个零成本不变量：
    /// base64 能原样往返（拦截断/非法字符）、头部是 PNG 签名 + 16×16 IHDR、
    /// 尾部必须是完整的 IEND chunk（长度域 + 类型 + 固定 CRC）——截断的图恰好丢的就是这段。
    #[test]
    fn probe_image_decodes_as_png() {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(PROBE_PNG)
            .expect("探测图必须是合法 base64");
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(&bytes),
            PROBE_PNG,
            "base64 往返不一致：常量被改坏了（多/少字符、含空白）"
        );
        assert_eq!(&bytes[..8], &[137, 80, 78, 71, 13, 10, 26, 10], "PNG 签名");
        assert_eq!(&bytes[12..16], *b"IHDR");
        // 16×16：避开「尺寸过小被判无效图」的那类端点
        assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 16);
        assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 16);
        // IEND chunk 全文固定不变：长度 0 + "IEND" + 其 CRC
        assert_eq!(
            &bytes[bytes.len() - 12..],
            &[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82],
            "缺完整 IEND chunk：图被截断了，端点会拒收"
        );
    }
}
