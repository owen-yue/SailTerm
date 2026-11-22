//! AI provider 配置、对话归档与对话消息的持久化。
//!
//! **库内不存明文 Key**：API Key 只在 Vault，本表仅以 `key_ref` 指向它；
//! `key_ref` 对前端不可见（`#[serde(skip)]`），前端只拿到 `has_key` 布尔标记。
//! `ai_chat_messages` 存的是**脱敏后**的文本与工具调用记录。

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::{new_id, now_ms, Db};
use crate::ai::providers::ProviderKind;
use crate::error::AppError;

/// 用量统计的一个分组条目（按模型 / 分时 / 按天 / 按月共用）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UsageByKey {
    /// 展示名/轴标签（模型「名称 · model」/ 小时 "00".."23" / 日 "YYYY-MM-DD" / 月 "YYYY-MM"）。
    pub key: String,
    /// 该桶内对话轮数。
    pub count: i64,
    /// token 合计。
    pub tokens: i64,
}

/// 时间桶内按模型拆分的段（堆叠柱的一段）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UsageSegment {
    /// 模型展示名（与 `by_model.key` 同口径：「名称 · model」）。
    pub model: String,
    /// 该桶内该模型的对话轮数。
    pub count: i64,
    /// 该桶内该模型的 token 合计。
    pub tokens: i64,
}

/// 时间序列的一个桶（按天/按月/按小时），内含按模型拆分的堆叠段。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    /// 轴标签（小时 "00".."23" / 日 "YYYY-MM-DD" / 月 "YYYY-MM"）。
    pub key: String,
    /// 桶内对话轮数（各段之和）。
    pub count: i64,
    /// 桶内 token 合计（各段之和）。
    pub tokens: i64,
    /// 按模型拆分的段（token 降序）。
    pub segments: Vec<UsageSegment>,
}

/// 用量统计汇总（`Db::usage_stats` 产物，随 `ai_usage_stats` 命令回前端）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    /// 历史累计 token（assistant 条合计）。
    pub total_tokens: i64,
    /// 历史累计对话轮数。
    pub total_turns: i64,
    /// 按模型（降序）。
    pub by_model: Vec<UsageByKey>,
    /// 近 30 天按小时（hour-of-day 0..23，只回有量的时点，前端补齐轴）。
    pub by_hour: Vec<UsageBucket>,
    /// 近 30 天逐日（只回有量的天，前端补齐轴）。
    pub by_day: Vec<UsageBucket>,
    /// 近 12 月逐月（只回有量的月，前端补齐轴）。
    pub by_month: Vec<UsageBucket>,
}

/// 脱敏级别；云模型默认 `strict`。
#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SanitizeLevel {
    #[default]
    Strict,
    Standard,
    Loose,
    Custom,
}

impl SanitizeLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Standard => "standard",
            Self::Loose => "loose",
            Self::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "standard" => Self::Standard,
            "loose" => Self::Loose,
            "custom" => Self::Custom,
            _ => Self::Strict,
        }
    }
}

/// 对话消息角色；与 rig 的 `Message` role 对应。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    Tool,
}

impl ChatRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "system" => Self::System,
            "assistant" => Self::Assistant,
            "tool" => Self::Tool,
            _ => Self::User,
        }
    }
}

/// AI provider 配置。既是库内行模型，也是回前端的 DTO（`key_ref` 被 skip）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiProvider {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    pub model: String,
    /// 是否支持工具调用；`None` = 未探测（能力探测）。
    pub supports_tools: Option<bool>,
    /// 是否支持图片输入（vision）；`None` = 未探测。由设置页「测试」按钮顺带探测写入，
    /// 发送带图时严格判 `Some(true)`，未探测/不支持一律拒发（见 `commands/ai.rs`）。
    pub supports_vision: Option<bool>,
    pub sanitize_level: SanitizeLevel,
    pub max_context_tokens: i64,
    pub temperature: Option<f64>,
    pub is_default: bool,
    /// 非敏感扩展的原始 JSON（自定义 header / 组织 id 等）。
    pub extra: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// 命令层依据 Vault 回填：是否已存 API Key（前端据此显示「已保存」）。
    pub has_key: bool,
    /// 保险库引用：仅可信区使用，绝不出 IPC。
    #[serde(skip)]
    pub key_ref: Option<String>,
}

impl AiProvider {
    /// 新建 provider 骨架（时间戳/ID 已填，`key_ref` 由命令层写入）。
    pub fn new(name: String, kind: ProviderKind, model: String) -> Self {
        let now = now_ms();
        Self {
            id: new_id(),
            name,
            kind,
            base_url: None,
            model,
            supports_tools: None,
            supports_vision: None,
            sanitize_level: SanitizeLevel::default(),
            max_context_tokens: kind.default_context_tokens(),
            temperature: None,
            is_default: false,
            extra: None,
            created_at: now,
            updated_at: now,
            has_key: false,
            key_ref: None,
        }
    }
}

/// 一段 AI 对话（按连接归档）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiConversation {
    pub id: String,
    /// 归档到哪个连接（profile_id）；`None` = 通用对话。
    pub connection_id: Option<String>,
    pub title: Option<String>,
    pub provider_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 一条对话消息（脱敏后落库）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiChatMessage {
    pub id: String,
    pub conversation_id: Option<String>,
    pub connection_id: Option<String>,
    pub session_id: Option<String>,
    pub provider_id: Option<String>,
    pub role: ChatRole,
    pub content: Option<String>,
    /// `role=tool` 时对应的调用 id。
    pub tool_call_id: Option<String>,
    /// `role=assistant` 的工具调用名。
    pub tool_name: Option<String>,
    /// 工具入参 JSON（不含凭证）。
    pub tool_args: Option<String>,
    /// 本轮过程轨迹（`AiStep` 数组的 JSON 串，已脱敏/截断）；仅 assistant 行有值，历史回放用。
    pub steps: Option<String>,
    /// 图片附件清单（`AttachmentRef` 数组的 JSON 串）；仅 user 行有值，字节在附件目录里。
    pub attachments: Option<String>,
    pub tokens: Option<i64>,
    /// 末次模型调用实际发送的上下文规模（仅 assistant 行有值）；回看历史回填工具栏 ctx 环。
    pub context_used: Option<i64>,
    /// 该轮 provider 上下文窗口总量（仅 assistant 行有值）；NULL/0 = 未知，前端隐藏占用环。
    pub context_window: Option<i64>,
    pub created_at: i64,
}

const PROVIDER_COLS: &str = "id, name, kind, base_url, model, key_ref, supports_tools, \
     sanitize_level, max_context_tokens, temperature, is_default, extra, created_at, updated_at, \
     supports_vision";
const CONV_COLS: &str = "id, connection_id, title, provider_id, created_at, updated_at";
const MSG_COLS: &str = "id, conversation_id, connection_id, session_id, provider_id, role, \
     content, tool_call_id, tool_name, tool_args, tokens, created_at, steps, context_used, \
     context_window, attachments";

impl Db {
    // ---- ai_providers ----

    pub fn list_providers(&self) -> Result<Vec<AiProvider>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {PROVIDER_COLS} FROM ai_providers \
                 ORDER BY is_default DESC, name COLLATE NOCASE"
            ))?;
            let rows = st
                .query_map([], map_provider)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_provider(&self, id: &str) -> Result<Option<AiProvider>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {PROVIDER_COLS} FROM ai_providers WHERE id = ?1"
            ))?;
            Ok(st.query_row(params![id], map_provider).optional()?)
        })
    }

    /// 当前默认 provider：优先 `is_default=1`；无人勾选默认时**回退最近更新的一条**
    /// （避免「明明配了却报尚未配置」），全表空才 `None`。
    pub fn get_default_provider(&self) -> Result<Option<AiProvider>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {PROVIDER_COLS} FROM ai_providers WHERE is_default = 1 \
                 ORDER BY updated_at DESC LIMIT 1"
            ))?;
            if let Some(p) = st.query_row([], map_provider).optional()? {
                return Ok(Some(p));
            }
            let mut st = c.prepare(&format!(
                "SELECT {PROVIDER_COLS} FROM ai_providers \
                 ORDER BY updated_at DESC LIMIT 1"
            ))?;
            Ok(st.query_row([], map_provider).optional()?)
        })
    }

    /// 插入或整行更新（`created_at` 保持首次写入值）。若本行为默认，先清掉其它行的默认标记。
    pub fn upsert_provider(&self, p: &AiProvider) -> Result<(), AppError> {
        self.with(|c| {
            if p.is_default {
                c.execute(
                    "UPDATE ai_providers SET is_default = 0 WHERE id <> ?1 AND is_default <> 0",
                    params![p.id],
                )?;
            }
            c.execute(
                "INSERT INTO ai_providers (id, name, kind, base_url, model, key_ref, supports_tools,
                    sanitize_level, max_context_tokens, temperature, is_default, extra, created_at, updated_at,
                    supports_vision)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
                 ON CONFLICT(id) DO UPDATE SET
                    name=excluded.name, kind=excluded.kind, base_url=excluded.base_url, model=excluded.model,
                    key_ref=excluded.key_ref, supports_tools=excluded.supports_tools,
                    sanitize_level=excluded.sanitize_level, max_context_tokens=excluded.max_context_tokens,
                    temperature=excluded.temperature, is_default=excluded.is_default,
                    extra=excluded.extra, updated_at=excluded.updated_at,
                    supports_vision=excluded.supports_vision",
                params![
                    p.id,
                    p.name,
                    p.kind.as_str(),
                    p.base_url,
                    p.model,
                    p.key_ref,
                    p.supports_tools.map(i64::from),
                    p.sanitize_level.as_str(),
                    p.max_context_tokens,
                    p.temperature,
                    i64::from(p.is_default),
                    p.extra,
                    p.created_at,
                    p.updated_at,
                    p.supports_vision.map(i64::from),
                ],
            )?;
            Ok(())
        })
    }

    /// 设为默认（先清全部再置本行）。
    pub fn set_default_provider(&self, id: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "UPDATE ai_providers SET is_default = 0 WHERE is_default <> 0",
                [],
            )?;
            c.execute(
                "UPDATE ai_providers SET is_default = 1, updated_at = ?1 WHERE id = ?2",
                params![now_ms(), id],
            )?;
            Ok(())
        })
    }

    /// 删除 provider 行（Vault 里的 Key 由命令层用 `keys::of_ai_provider` 一并清理）。
    pub fn delete_provider(&self, id: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute("DELETE FROM ai_providers WHERE id = ?1", params![id])?;
            Ok(())
        })
    }

    // ---- ai_conversations ----

    /// 新建对话，返回带 ID/时间戳的完整记录。
    pub fn create_conversation(
        &self,
        connection_id: Option<&str>,
        title: Option<&str>,
        provider_id: Option<&str>,
    ) -> Result<AiConversation, AppError> {
        let now = now_ms();
        let conv = AiConversation {
            id: new_id(),
            connection_id: connection_id.map(str::to_string),
            title: title.map(str::to_string),
            provider_id: provider_id.map(str::to_string),
            created_at: now,
            updated_at: now,
        };
        self.with(|c| {
            c.execute(
                "INSERT INTO ai_conversations (id, connection_id, title, provider_id, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    conv.id,
                    conv.connection_id,
                    conv.title,
                    conv.provider_id,
                    conv.created_at,
                    conv.updated_at
                ],
            )?;
            Ok(conv)
        })
    }

    /// 按连接（或全局）列对话，最近更新在前。
    pub fn list_conversations(
        &self,
        connection_id: Option<&str>,
    ) -> Result<Vec<AiConversation>, AppError> {
        self.with(|c| {
            let (sql, use_conn) = match connection_id {
                Some(_) => (
                    format!(
                        "SELECT {CONV_COLS} FROM ai_conversations WHERE connection_id = ?1 \
                         ORDER BY updated_at DESC, rowid DESC"
                    ),
                    true,
                ),
                None => (
                    format!(
                        "SELECT {CONV_COLS} FROM ai_conversations \
                         ORDER BY updated_at DESC, rowid DESC"
                    ),
                    false,
                ),
            };
            let mut st = c.prepare(&sql)?;
            let rows = if use_conn {
                st.query_map(params![connection_id], map_conversation)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                st.query_map([], map_conversation)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            Ok(rows)
        })
    }

    pub fn get_conversation(&self, id: &str) -> Result<Option<AiConversation>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {CONV_COLS} FROM ai_conversations WHERE id = ?1"
            ))?;
            Ok(st.query_row(params![id], map_conversation).optional()?)
        })
    }

    pub fn rename_conversation(&self, id: &str, title: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "UPDATE ai_conversations SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![title, now_ms(), id],
            )?;
            Ok(())
        })
    }

    /// 删除对话及其全部消息（消息表无外键，手动级联，避免遗留孤儿行）。
    pub fn delete_conversation(&self, id: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "DELETE FROM ai_chat_messages WHERE conversation_id = ?1",
                params![id],
            )?;
            c.execute("DELETE FROM ai_conversations WHERE id = ?1", params![id])?;
            Ok(())
        })
    }

    /// 某对话各条消息的附件清单原始 JSON（只回非空条）。删对话前由命令层取走，
    /// 据此级联删除附件目录里的图片文件（db 层不认得附件结构，故只回原始串）。
    pub fn list_attachment_manifests(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<String>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT attachments FROM ai_chat_messages \
                 WHERE conversation_id = ?1 AND attachments IS NOT NULL",
            )?;
            let rows = st
                .query_map(params![conversation_id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    // ---- ai_chat_messages ----

    /// 追加一条消息，并刷新所属对话的 `updated_at`（对话列表按此排序）。
    pub fn append_message(&self, m: &AiChatMessage) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO ai_chat_messages (id, conversation_id, connection_id, session_id, provider_id,
                    role, content, tool_call_id, tool_name, tool_args, tokens, created_at, steps, context_used, context_window, attachments)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![
                    m.id,
                    m.conversation_id,
                    m.connection_id,
                    m.session_id,
                    m.provider_id,
                    m.role.as_str(),
                    m.content,
                    m.tool_call_id,
                    m.tool_name,
                    m.tool_args,
                    m.tokens,
                    m.created_at,
                    m.steps,
                    m.context_used,
                    m.context_window,
                    m.attachments,
                ],
            )?;
            if let Some(conv) = &m.conversation_id {
                c.execute(
                    "UPDATE ai_conversations SET updated_at = ?1 WHERE id = ?2",
                    params![m.created_at, conv],
                )?;
            }
            Ok(())
        })
    }

    /// 取一个对话的最近 `limit` 条消息，按时间**正序**返回（重建上下文用）。
    pub fn list_messages(
        &self,
        conversation_id: &str,
        limit: i64,
    ) -> Result<Vec<AiChatMessage>, AppError> {
        let limit = limit.clamp(1, 2000);
        self.with(|c| {
            // 先按时间倒序取最近 limit 条，再翻回正序（对话轮次有限，内存翻转成本可忽略）。
            let mut st = c.prepare(&format!(
                "SELECT {MSG_COLS} FROM ai_chat_messages WHERE conversation_id = ?1 \
                 ORDER BY created_at DESC, rowid DESC LIMIT ?2"
            ))?;
            let mut rows = st
                .query_map(params![conversation_id, limit], map_message)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.reverse();
            Ok(rows)
        })
    }

    /// 用量统计汇总（一次拉齐，供「用量统计」弹窗）：总计 + 按模型 + 按对话 + 近 14 天。
    ///
    /// 口径：tokens 只记在 assistant 消息上 = 本轮全部模型调用的真值合计（含工具往返；
    /// 本轮能力上线前的旧数据为字符估算）。user 条的单条估算不计入，避免与合计重复；
    /// 已删 provider 的 id 取不到名字时回退显示 id 前缀。
    /// `tz_offset_secs` 由前端传（JS 自带本地时区），避免为拼日期引入 chrono。
    pub fn usage_stats(&self, tz_offset_secs: i64) -> Result<UsageStats, AppError> {
        self.with(|c| {
            let total: (i64, i64) = c.query_row(
                "SELECT COALESCE(SUM(tokens), 0), COUNT(*) \
                 FROM ai_chat_messages WHERE role = 'assistant'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;

            let mut st = c.prepare(
                "SELECT COALESCE(p.name, '未知模型') || ' · ' || COALESCE(p.model, ''), \
                        COUNT(*), COALESCE(SUM(m.tokens), 0) \
                 FROM ai_chat_messages m LEFT JOIN ai_providers p ON p.id = m.provider_id \
                 WHERE m.role = 'assistant' \
                 GROUP BY m.provider_id ORDER BY SUM(m.tokens) DESC",
            )?;
            let by_model = st
                .query_map([], |r| {
                    Ok(UsageByKey {
                        key: r.get(0)?,
                        count: r.get(1)?,
                        tokens: r.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            // 时间聚合统一向前端传入的时区偏移换算（(created_at + tz*1000)/1000 转本地秒）。
            let tz = tz_offset_secs * 1000;
            let day30 = now_ms() - 29 * 86_400_000;
            let mon365 = now_ms() - 365 * 86_400_000;

            // 时间桶 × 模型 明细查询：同一模板只换桶表达式与时间窗，结果按 (桶, token 降序) 排。
            let bucket_query =
                |sql: &str, cutoff: i64| -> rusqlite::Result<Vec<(String, String, i64, i64)>> {
                    let mut st = c.prepare(sql)?;
                    let rows = st
                        .query_map(params![tz, cutoff], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(rows)
                };

            // 近 30 天按小时分布（hour-of-day），按模型堆叠。
            let by_hour = fold_usage_buckets(bucket_query(
                "SELECT strftime('%H', (m.created_at + ?1) / 1000, 'unixepoch') AS k, \
                        COALESCE(p.name, '未知模型') || ' · ' || COALESCE(p.model, ''), \
                        COUNT(*), COALESCE(SUM(m.tokens), 0) \
                 FROM ai_chat_messages m LEFT JOIN ai_providers p ON p.id = m.provider_id \
                 WHERE m.role = 'assistant' AND m.created_at >= ?2 \
                 GROUP BY k, m.provider_id ORDER BY k, SUM(m.tokens) DESC",
                day30,
            )?);

            // 近 30 天逐日，按模型堆叠。
            let by_day = fold_usage_buckets(bucket_query(
                "SELECT date((m.created_at + ?1) / 1000, 'unixepoch') AS k, \
                        COALESCE(p.name, '未知模型') || ' · ' || COALESCE(p.model, ''), \
                        COUNT(*), COALESCE(SUM(m.tokens), 0) \
                 FROM ai_chat_messages m LEFT JOIN ai_providers p ON p.id = m.provider_id \
                 WHERE m.role = 'assistant' AND m.created_at >= ?2 \
                 GROUP BY k, m.provider_id ORDER BY k, SUM(m.tokens) DESC",
                day30,
            )?);

            // 近 12 月逐月，按模型堆叠。
            let by_month = fold_usage_buckets(bucket_query(
                "SELECT strftime('%Y-%m', (m.created_at + ?1) / 1000, 'unixepoch') AS k, \
                        COALESCE(p.name, '未知模型') || ' · ' || COALESCE(p.model, ''), \
                        COUNT(*), COALESCE(SUM(m.tokens), 0) \
                 FROM ai_chat_messages m LEFT JOIN ai_providers p ON p.id = m.provider_id \
                 WHERE m.role = 'assistant' AND m.created_at >= ?2 \
                 GROUP BY k, m.provider_id ORDER BY k, SUM(m.tokens) DESC",
                mon365,
            )?);

            Ok(UsageStats {
                total_tokens: total.0,
                total_turns: total.1,
                by_model,
                by_hour,
                by_day,
                by_month,
            })
        })
    }
}

/// 把「(桶, 模型, 轮数, token)」明细按桶折叠成堆叠序列。
/// 依赖入参已按 (桶, token 降序) 排序，故同桶行连续，用 last 合并即可。
fn fold_usage_buckets(rows: Vec<(String, String, i64, i64)>) -> Vec<UsageBucket> {
    let mut out: Vec<UsageBucket> = Vec::new();
    for (key, model, count, tokens) in rows {
        match out.last_mut() {
            Some(b) if b.key == key => {
                b.count += count;
                b.tokens += tokens;
                b.segments.push(UsageSegment {
                    model,
                    count,
                    tokens,
                });
            }
            _ => out.push(UsageBucket {
                key,
                count,
                tokens,
                segments: vec![UsageSegment {
                    model,
                    count,
                    tokens,
                }],
            }),
        }
    }
    out
}

fn map_provider(r: &Row<'_>) -> rusqlite::Result<AiProvider> {
    let kind: String = r.get(2)?;
    let supports: Option<i64> = r.get(6)?;
    let sanitize: String = r.get(7)?;
    let is_default: i64 = r.get(10)?;
    let vision: Option<i64> = r.get(14)?;
    Ok(AiProvider {
        id: r.get(0)?,
        name: r.get(1)?,
        // 库内取值受 CHECK 约束；万一被外部改动也不该让整表读不出来
        kind: ProviderKind::parse(&kind),
        base_url: r.get(3)?,
        model: r.get(4)?,
        supports_tools: supports.map(|v| v != 0),
        supports_vision: vision.map(|v| v != 0),
        sanitize_level: SanitizeLevel::parse(&sanitize),
        max_context_tokens: r.get(8)?,
        temperature: r.get(9)?,
        is_default: is_default != 0,
        extra: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
        has_key: false,
        key_ref: r.get(5)?,
    })
}

fn map_conversation(r: &Row<'_>) -> rusqlite::Result<AiConversation> {
    Ok(AiConversation {
        id: r.get(0)?,
        connection_id: r.get(1)?,
        title: r.get(2)?,
        provider_id: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

fn map_message(r: &Row<'_>) -> rusqlite::Result<AiChatMessage> {
    let role: String = r.get(5)?;
    Ok(AiChatMessage {
        id: r.get(0)?,
        conversation_id: r.get(1)?,
        connection_id: r.get(2)?,
        session_id: r.get(3)?,
        provider_id: r.get(4)?,
        role: ChatRole::parse(&role),
        content: r.get(6)?,
        tool_call_id: r.get(7)?,
        tool_name: r.get(8)?,
        tool_args: r.get(9)?,
        tokens: r.get(10)?,
        created_at: r.get(11)?,
        steps: r.get(12)?,
        context_used: r.get(13)?,
        context_window: r.get(14)?,
        attachments: r.get(15)?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    fn provider(id: &str, name: &str, kind: ProviderKind, is_default: bool) -> AiProvider {
        let now = now_ms();
        AiProvider {
            id: id.to_string(),
            name: name.to_string(),
            kind,
            model: "gpt-4o-mini".to_string(),
            is_default,
            created_at: now,
            updated_at: now,
            ..AiProvider::new(name.to_string(), kind, "gpt-4o-mini".to_string())
        }
    }

    fn msg(conv: &str, role: ChatRole, content: &str, created_at: i64) -> AiChatMessage {
        AiChatMessage {
            id: new_id(),
            conversation_id: Some(conv.to_string()),
            connection_id: Some("conn-1".to_string()),
            session_id: Some("s1".to_string()),
            provider_id: Some("p1".to_string()),
            role,
            content: Some(content.to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_args: None,
            steps: None,
            attachments: None,
            tokens: Some(10),
            context_used: None,
            context_window: None,
            created_at,
        }
    }

    #[test]
    fn migrates_and_roundtrips_provider() {
        let db = db();
        assert!(db.schema_version().unwrap() >= 1);
        assert!(db.list_providers().unwrap().is_empty());

        let mut p = provider("p1", "OpenAI", ProviderKind::Openai, true);
        p.key_ref = Some("sail:ai:p1:key".into());
        p.supports_tools = Some(true);
        p.temperature = Some(0.3);
        db.upsert_provider(&p).unwrap();

        let got = db.get_provider("p1").unwrap().unwrap();
        assert_eq!(got.name, "OpenAI");
        assert_eq!(got.kind, ProviderKind::Openai);
        assert_eq!(got.supports_tools, Some(true));
        assert_eq!(got.temperature, Some(0.3));
        assert!(got.is_default);
        assert_eq!(got.sanitize_level, SanitizeLevel::Strict);
        assert_eq!(got.key_ref.as_deref(), Some("sail:ai:p1:key"));
        assert_eq!(db.get_default_provider().unwrap().unwrap().id, "p1");

        // 更新走同一条 upsert：created_at 不变，name 变
        let mut edited = got.clone();
        edited.name = "GPT-4o".into();
        edited.updated_at = now_ms() + 1000;
        db.upsert_provider(&edited).unwrap();
        let after = db.get_provider("p1").unwrap().unwrap();
        assert_eq!(after.name, "GPT-4o");
        assert_eq!(after.created_at, p.created_at);
    }

    #[test]
    fn only_one_default_provider() {
        let db = db();
        db.upsert_provider(&provider("p1", "A", ProviderKind::Openai, true))
            .unwrap();
        db.upsert_provider(&provider("p2", "B", ProviderKind::Ollama, true))
            .unwrap();
        // p2 落库为默认后，p1 的默认标记被清掉
        assert!(!db.get_provider("p1").unwrap().unwrap().is_default);
        assert!(db.get_provider("p2").unwrap().unwrap().is_default);
        assert_eq!(db.get_default_provider().unwrap().unwrap().id, "p2");

        db.set_default_provider("p1").unwrap();
        assert!(db.get_provider("p1").unwrap().unwrap().is_default);
        assert!(!db.get_provider("p2").unwrap().unwrap().is_default);

        db.delete_provider("p1").unwrap();
        assert!(db.get_provider("p1").unwrap().is_none());
    }

    /// kind 经 CHECK 约束落库为文档口径字符串；claude 与 openai-compatible 往返一致。
    #[test]
    fn provider_kind_persists_documented_strings() {
        let db = db();
        db.upsert_provider(&provider("c1", "Claude", ProviderKind::Anthropic, false))
            .unwrap();
        db.upsert_provider(&provider(
            "q1",
            "Qwen",
            ProviderKind::OpenaiCompatible,
            false,
        ))
        .unwrap();
        assert_eq!(
            db.get_provider("c1").unwrap().unwrap().kind,
            ProviderKind::Anthropic
        );
        assert_eq!(
            db.get_provider("q1").unwrap().unwrap().kind,
            ProviderKind::OpenaiCompatible
        );
    }

    #[test]
    fn conversation_and_message_roundtrip() {
        let db = db();
        let conv = db
            .create_conversation(Some("conn-1"), Some("排查磁盘"), Some("p1"))
            .unwrap();
        let now = now_ms();
        db.append_message(&msg(&conv.id, ChatRole::User, "磁盘满了怎么办", now))
            .unwrap();
        let mut a = msg(&conv.id, ChatRole::Assistant, "先看 df -h", now + 1);
        a.tool_name = Some("run_command".into());
        a.tool_args = Some(r#"{"session_id":"s1","command":"df -h"}"#.into());
        db.append_message(&a).unwrap();

        // 按连接归档能查到该对话
        let convs = db.list_conversations(Some("conn-1")).unwrap();
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].title.as_deref(), Some("排查磁盘"));
        // 其它连接查不到
        assert!(db.list_conversations(Some("conn-x")).unwrap().is_empty());

        // 消息按时间正序返回
        let msgs = db.list_messages(&conv.id, 100).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, ChatRole::User);
        assert_eq!(msgs[1].role, ChatRole::Assistant);
        assert_eq!(msgs[1].tool_name.as_deref(), Some("run_command"));

        // 追加消息刷新了对话 updated_at
        let after = db.get_conversation(&conv.id).unwrap().unwrap();
        assert!(after.updated_at >= conv.updated_at);

        // 重命名生效
        db.rename_conversation(&conv.id, "磁盘告急").unwrap();
        assert_eq!(
            db.get_conversation(&conv.id)
                .unwrap()
                .unwrap()
                .title
                .as_deref(),
            Some("磁盘告急")
        );

        // 删除对话级联删消息
        db.delete_conversation(&conv.id).unwrap();
        assert!(db.get_conversation(&conv.id).unwrap().is_none());
        assert!(db.list_messages(&conv.id, 100).unwrap().is_empty());
    }

    ///回前端的 provider JSON 里不得出现保险库引用（更不会有明文 Key）。
    #[test]
    fn provider_dto_never_exposes_key_ref() {
        let mut p = provider("p1", "A", ProviderKind::Openai, false);
        p.key_ref = Some("sail:ai:p1:key".into());
        let json = serde_json::to_value(&p).unwrap();
        assert!(!json.to_string().contains("sail:ai"));
        assert!(json.get("keyRef").is_none());
        assert!(json.get("hasKey").is_some());
    }

    ///ai_providers 表没有明文 Key 列，只有指向保险库的 key_ref。
    #[test]
    fn provider_table_has_no_plaintext_key_column() {
        let db = db();
        let conn = crate::db::profile::raw(&db);
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('ai_providers')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(cols.iter().any(|c| c == "key_ref"));
        assert!(!cols
            .iter()
            .any(|c| c == "api_key" || c == "key" || c == "token" || c == "secret"));
    }

    /// `supports_vision` 三态（NULL/1/0）往返：与 `supports_tools` 同口径，NULL 必须能读出
    /// `None`（未探测）而不能被当成 `Some(false)`——两者在发送链里同构拒发，但设置页徽标
    /// 要能区分「未探测」与「探过且不支持」。
    #[test]
    fn provider_supports_vision_three_state_roundtrip() {
        let db = db();
        // 新建默认即未探测
        db.upsert_provider(&provider("p1", "A", ProviderKind::Openai, true))
            .unwrap();
        assert_eq!(
            db.get_provider("p1").unwrap().unwrap().supports_vision,
            None
        );

        for (state, want) in [(Some(true), "支持"), (Some(false), "不支持")] {
            let mut p = db.get_provider("p1").unwrap().unwrap();
            p.supports_vision = state;
            db.upsert_provider(&p).unwrap();
            assert_eq!(
                db.get_provider("p1").unwrap().unwrap().supports_vision,
                state,
                "三态里的 {state:?}（{want}）应原样存回"
            );
        }

        // 改回 None 也要真的落回 NULL（不能因为 `map(i64::from)` 遇 None 就保留旧值）。
        let mut p = db.get_provider("p1").unwrap().unwrap();
        p.supports_vision = None;
        db.upsert_provider(&p).unwrap();
        assert_eq!(
            db.get_provider("p1").unwrap().unwrap().supports_vision,
            None
        );
    }

    /// user 行的附件清单往返：没存过就是 NULL（旧数据不能报错），存了就能原样读回；
    /// `list_attachment_manifests` 只回非空条（删对话时据此级联删文件）。
    #[test]
    fn message_attachments_column_roundtrip_and_manifest_query() {
        let db = db();
        let conv = db
            .create_conversation(Some("conn-1"), Some("看图"), Some("p1"))
            .unwrap();
        let now = now_ms();

        let text_only = msg(&conv.id, ChatRole::User, "纯文本轮", now);
        db.append_message(&text_only).unwrap();
        assert_eq!(
            db.list_messages(&conv.id, 100).unwrap()[0]
                .attachments
                .as_deref(),
            None,
            "不传附件就是 NULL，不是空串"
        );

        let mut with_img = msg(&conv.id, ChatRole::User, "看这张", now + 1);
        with_img.attachments =
            Some(r#"[{"id":"a1","mime":"image/png","name":"s.png","bytes":42}]"#.into());
        db.append_message(&with_img).unwrap();
        db.append_message(&msg(&conv.id, ChatRole::Assistant, "图里是 OOM", now + 2))
            .unwrap();

        let msgs = db.list_messages(&conv.id, 100).unwrap();
        assert_eq!(
            msgs[1].attachments.as_deref(),
            with_img.attachments.as_deref()
        );
        assert!(msgs[2].attachments.is_none(), "assistant 行不携带附件");

        let manifests = db.list_attachment_manifests(&conv.id).unwrap();
        assert_eq!(manifests.len(), 1, "只回非空条");
        assert!(manifests[0].contains("\"a1\""));

        db.delete_conversation(&conv.id).unwrap();
        assert!(db.list_attachment_manifests(&conv.id).unwrap().is_empty());
    }
}
