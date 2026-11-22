//! 设置命令层：AI / 安全偏好（出网开关 / 审批模式 / 命令风险扫描层级）的类型化读写。
//!
//! 底层是通用键值表 `settings`（[`crate::db::settings`]，只存非敏感偏好，**绝不落凭证**）；
//! 本层负责「字符串 ↔ 枚举/布尔」的解析、默认值，以及把这些偏好装配成 orchestrator/policy 消费的形态。
//!
//! 当前覆盖 AI/安全子集（审批模式 / 出网开关 / 扫描层级）、AI 记忆偏好（总开关 / 自动提取 / 保留期）
//! 与终端外观（主题 / 字号 / 字体 + 自定义 JSON 主题导入）；其余设置分类沿用同一张表扩展。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use super::{blocking, state};
use crate::ai::context::{ChatMode, ReplyVerbosity};
use crate::ai::policy::{ApprovalMode, ScanLevel};
use crate::ai::providers::ProviderKind;
use crate::db::Db;
use crate::error::AppError;

/// `settings` 表里**本层管理的全部键名的唯一真源**（配置键注册表）。
///
/// 用枚举取代此前散落在各区块的裸 `&str` 常量：编译期杜绝打错键名、集中一处便于查阅与扩展；
/// 命令层其他模块经 [`load_ai_settings`] 等拿类型化视图，不直接碰键。
/// [`SettingKey::as_str`] 返回的字符串与历史落库值**逐字一致**——改这些串会让老用户配置失联，
/// 故此枚举是纯内部收敛，不改变任何持久化格式。
/// `pub(crate)`：锁屏空闲阈值键由 [`crate::commands::lock`] 读写（键名仍单源在此）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingKey {
    // ── AI / 安全 ──
    ApprovalMode,
    NetworkEnabled,
    ScanLevel,
    AllowPlaintextPassword,
    MemoryEnabled,
    MemoryAutoExtract,
    MemoryRetentionDays,
    /// 只读兼容：三合一根轴拆成 `TerminalAi` + `RailTab` 两个独立设置前的旧值来源。
    ConsoleMode,
    TerminalAi,
    RailTab,
    Mem0Url,
    Mem0Key,
    Mem0User,
    UiLanguage,
    ReplyVerbosity,
    /// 单轮最大工具往返数（达上限优雅收尾并留「继续」，见 orchestrator）。
    MaxToolRounds,
    /// 对话模式：智能体 / 问答（见 ai::context::ChatMode）。
    ChatMode,
    /// OS 系统通知横幅总开关（通用设置）：关=只走应用内 toast，不发系统横幅。
    OsNotifications,
    /// 只读兼容：`ai.exec_mode`（静默/当众执行通道）这根轴已删除——Agent 页签下 AI 命令
    /// 恒打进终端 PTY。键名保留只为让老库残留值仍被识别与清理，不再参与读写。
    ExecMode,
    /// 系统提示词模板（仅开发者模式可编辑）：只存 dev 的**覆盖版**，空/缺省即回落
    /// [`crate::ai::context::DEFAULT_TEMPLATE`]。故意**不进** [`AiSettings`]——它是大段自由文本、
    /// 非小偏好，且有独立的读/写/恢复/预览命令，不参与 settings_reset 的批量清键。
    AiPromptTemplate,
    // ── 外观 / 主题 ──
    Theme,
    FontSize,
    FontFamily,
    CustomTheme,
    // ── 终端 / Shell 偏好 ──
    Scrollback,
    CursorBlink,
    LocalShell,
    CopyOnSelect,
    CursorStyle,
    BellStyle,
    /// 关键字着色总开关（客户端语义着色）。
    KeywordHighlight,
    /// 小文件打包传输总开关（目录上传命中阈值时先 tar 再传单包）。默认开。
    BundleTransferEnabled,
    // ── 锁屏 ──
    /// 空闲自动锁屏阈值（分钟），0=关闭。**故意不进** [`settings_reset`] 的批量清键：
    /// 安全类配置不应被「恢复 AI 默认值」静默放宽（fail-safe 方向）；PIN 本体在保险库，与此无关。
    LockIdleMinutes,
}

impl SettingKey {
    /// 稳定键名（= 落库的 `settings.key`），与重构前的字面量完全一致，保证向后兼容。
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalMode => "approval.mode",
            Self::NetworkEnabled => "ai.network_enabled",
            Self::ScanLevel => "ai.scan_level",
            Self::AllowPlaintextPassword => "ai.allow_plaintext_password",
            Self::MemoryEnabled => "memory.enabled",
            Self::MemoryAutoExtract => "memory.auto_extract",
            Self::MemoryRetentionDays => "memory.retention_days",
            Self::ConsoleMode => "ai.console_mode",
            Self::TerminalAi => "ai.terminal_ai",
            Self::RailTab => "ai.rail_tab",
            Self::Mem0Url => "memory.mem0_url",
            Self::Mem0Key => "memory.mem0_key",
            Self::Mem0User => "memory.mem0_user",
            Self::UiLanguage => "ui.language",
            Self::ReplyVerbosity => "ai.reply_verbosity",
            Self::MaxToolRounds => "ai.max_tool_rounds",
            Self::ChatMode => "ai.chat_mode",
            Self::OsNotifications => "ui.os_notifications",
            Self::ExecMode => "ai.exec_mode",
            Self::AiPromptTemplate => "ai.system_prompt_template",
            Self::Theme => "appearance.theme",
            Self::FontSize => "appearance.font_size",
            Self::FontFamily => "appearance.font_family",
            Self::CustomTheme => "appearance.custom_theme",
            Self::Scrollback => "terminal.scrollback",
            Self::CursorBlink => "terminal.cursor_blink",
            Self::LocalShell => "terminal.local_shell",
            Self::CopyOnSelect => "terminal.copy_on_select",
            Self::CursorStyle => "terminal.cursor_style",
            Self::BellStyle => "terminal.bell_style",
            Self::KeywordHighlight => "terminal.keyword_highlight",
            Self::BundleTransferEnabled => "transfer.bundle_enabled",
            Self::LockIdleMinutes => "lock.idle_minutes",
        }
    }
}

/// 界面语言归一化：trim + 小写；空串回落 `"system"`（跟随系统）。
///
/// 不再是枚举：语言是**注册制**的（内置 zh/en，插件可经 manifest.locales 扩展任意 code），
/// 后端只存透传字符串；code 是否有效由前端语料注册表判定，未注册时前端自行回落。
fn normalize_ui_language(s: &str) -> String {
    let v = s.trim().to_ascii_lowercase();
    if v.is_empty() {
        "system".into()
    } else {
        v
    }
}

/// 终端上的 AI 开关（设置项 `ai.terminal_ai`）——**只管终端自己**，跟右侧栏无关：
/// 决定终端 Enter 是否接管自然语言、AI 是否在终端落笔（回复/确认条）。
///
/// 命令的执行通道与它是两根独立的轴：Agent 页签下 AI 命令恒打进终端 PTY（像手打一样
/// 回显并执行），聊天页签恒走后台 exec（见 `RailTab` 与 `tools.rs` 的逐轮快照）。
/// （旧名 `AiMode` 还把右栏的「聊天」视图也当成终端的一档，导致两个入口互相牵动。）
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAi {
    /// 关：终端纯 shell。Enter 永远直送 shell，AI 不在终端落笔，命令走 exec 通道。
    Off,
    /// 开（默认）：命令与问题都能直接敲进终端，AI 回复/待授权提示/确认条都落在终端里。
    #[default]
    On,
}

impl TerminalAi {
    /// 稳定字符串（= serde `snake_case` 形态），供设置项落库复用。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }

    /// [`TerminalAi::as_str`] 的逆。**兼容旧库**：这个概念在旧版库里先以布尔（`console_mode=true`=控制台）、
    /// 后以三档枚举（`agent`/`chat`/`shell`）存过，旧值都能映射过来，无法识别的值回落 `On`（= 升级前默认）。
    /// `shell`/`chat`/`false` 都代表「终端不被 AI 接管」→ `Off`；仅 `agent`/`true`/`on`（及脏值默认）为 `On`。
    pub fn parse(s: &str) -> Self {
        match s {
            "off" | "false" | "shell" | "chat" => Self::Off,
            _ => Self::On,
        }
    }
}

/// 右侧栏页签（设置项 `ai.rail_tab`）——**AI 主画布总开关**：
/// - `Agent`：控制台是主场，终端是否接管/落笔再由 `ai.terminal_ai` 决定；有 PTY 的会话
///   （SSH/本地/Telnet…）AI 命令恒打进终端，像用户手打回显执行；
/// - `Chat`：聊天窗是主场，终端视同纯 shell（前端硬禁落笔/不显开关，后端命令恒静默 exec）。
/// 它与终端开关互不牵动的是**落库值**：切页签不改 `ai.terminal_ai`，反之亦然。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum RailTab {
    /// 会话坞：AgentPanel 展示本轮步骤与问答。
    Agent,
    /// 聊天（默认）：内嵌完整对话抽屉（AiDrawer），AI 在窗口里完成问答与审批。
    #[default]
    Chat,
}

impl RailTab {
    /// 稳定字符串（= serde `snake_case` 形态），供设置项落库复用。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Chat => "chat",
        }
    }

    /// [`RailTab::as_str`] 的逆。旧值显式列举（用户选过的不能丢）：布尔时代的 `true`、
    /// 三档时代的 `agent`/`shell` 归 `Agent`；`false`（控制台关=聊天主场）与 `chat` 归 `Chat`。
    /// 未识别的脏值回落默认档 [`RailTab::default`]。
    pub fn parse(s: &str) -> Self {
        match s {
            "agent" | "true" | "shell" => Self::Agent,
            "chat" | "false" => Self::Chat,
            _ => Self::default(),
        }
    }
}

/// 旧三档取值的字面量（只用于把两个新设置反向写回 `ai.console_mode`，读侧由
/// [`TerminalAi::parse`] / [`RailTab::parse`] 自己兼容）。不参与序列化，也不进任何结构体。
enum AiLegacyMode {
    Shell,
    Agent,
    Chat,
}

impl AiLegacyMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Agent => "agent",
            Self::Chat => "chat",
        }
    }
}

/// AI/安全设置的类型化视图（「安全/隐私」的相关项）。全部为**非敏感偏好**。
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    /// 审批模式：分级自动（只读自动）/ 全审批（只读也审批）。驱动 `PolicyEngine` 的 `approve_readonly`。
    pub approval_mode: ApprovalMode,
    /// AI 出网总开关：关闭后云端 provider 一律阻断，仅本地 Ollama 可用；SSH/终端/文件不受影响。
    pub network_enabled: bool,
    /// 命令风险扫描层级：宽松/标准/严格，决定 `READ_ONLY/WRITE/UNKNOWN` 的边界松紧。
    pub scan_level: ScanLevel,
    /// AI 记忆总开关：关闭后既不召回注入也不自动提取。
    pub memory_enabled: bool,
    /// 自动提取开关：从用户话语沉淀偏好/环境事实（仅在 `memory_enabled` 时生效）。
    pub memory_auto_extract: bool,
    /// 记忆保留天数：`0` = 永久；否则超期未命中即清理（如 90/180）。
    pub memory_retention_days: i64,
    /// Mem0 框架接入：服务地址（空 = 关闭，走本库规则记忆）。
    pub mem0_url: String,
    /// Mem0 API Key（本地自托管可空）。
    pub mem0_key: String,
    /// Mem0 user_id（默认 default）。
    pub mem0_user: String,
    /// 终端上的 AI 开关：关=终端纯 shell / 开=命令与问题都能直接敲进终端、AI 在终端落笔。
    /// 只影响终端呈现；命令执行通道由页签决定（Agent=打进终端 / Chat=后台 exec）。
    pub terminal_ai: TerminalAi,
    /// 右侧栏页签：Agent 会话坞 / 聊天抽屉。纯呈现层选择，不影响终端行为。
    pub rail: RailTab,
    /// 界面语言：已注册语料 code（如 zh/en，可由插件扩展）或 "system"=跟随系统；只影响前端文案。
    pub ui_language: String,
    /// 回复详略档（运维在终端只要重点）：同时决定系统提示词的【回答风格】额度
    /// 与终端落笔的行数上限（侧栏/历史不受影响，始终全文）。
    pub reply_verbosity: ReplyVerbosity,
    /// 明文凭证开关（默认宽松）：开=AI 可在命令里直接使用用户在对话中主动提供的
    /// 应用级密码（如 MySQL 密码）完成任务；关=AI 拒绝内联明文密码，改走免密。
    /// **不影响 SSH 登录凭证隔离**（主机/登录账号/私钥那条始终硬隔离）。
    pub allow_plaintext_password: bool,
    /// 单轮用户消息内允许的最大模型往返数（防失控兜底）。达上限不再硬停，而是优雅收尾、
    /// 保留已执行轨迹并出「继续」；可设区间见 `orchestrator::{MIN,MAX}_MAX_TOOL_ROUNDS`。
    pub max_tool_rounds: u32,
    /// 对话能力开关（设置项 `ai.chat_mode`，仅聊天页签内生效）：读写（可调全部工具、写命令弹审批）/
    /// 只读问答（只给只读工具，能查不能改，写命令执行期直接拒绝）。
    pub chat_mode: ChatMode,
    /// OS 系统通知横幅开关（设置项 `ui.os_notifications`）：关=通知只走应用内 toast，不发系统横幅。
    pub os_notifications: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            approval_mode: ApprovalMode::default(), // AutoReadonly：只读自动
            network_enabled: true,                  // 默认开：AI 开箱可用（缺键/脏值同此）
            scan_level: ScanLevel::default(),       // Standard：三级裁决基线
            memory_enabled: true,                   // 记忆默认开：越用越懂你
            memory_auto_extract: true,              // 自动提取默认开
            memory_retention_days: 0,               // 0 = 永久保留
            terminal_ai: TerminalAi::default(), // 默认开：终端里命令与问题都能敲（升级前的 console_mode=true）
            rail: RailTab::default(),           // 默认聊天页签（对话主场，命令静默 exec）
            ui_language: "system".into(),
            reply_verbosity: ReplyVerbosity::default(), // 精简：终端只打重点
            allow_plaintext_password: true, // 默认宽松：允许 AI 用用户主动提供的应用级密码
            max_tool_rounds: crate::ai::orchestrator::DEFAULT_MAX_TOOL_ROUNDS as u32, // 默认 100：多步任务少被打断，仍可被设置覆盖（8–200）
            chat_mode: ChatMode::default(), // 智能体：保持既有行为，问答需用户主动切
            os_notifications: true,         // OS 横幅默认开：失焦时的重大事件进系统通知中心
            mem0_url: String::new(),
            mem0_key: String::new(),
            mem0_user: "default".into(), // 默认跟随系统
        }
    }
}

impl AiSettings {
    /// 出网闸门（出网控制、完全离线模式；灯色「灰=AI 出网关闭(仅本地)」）：
    /// 出网关闭且 provider **非本地**（数据会离开本机）时返回 `Err`，阻止建 client / 发请求。
    /// 本地 provider（Ollama，数据不出本机）即便出网关闭也放行——这就是「仅本地」档。
    pub fn check_egress(&self, kind: ProviderKind) -> Result<(), AppError> {
        if !self.network_enabled && !kind.is_local() {
            return Err(AppError::Ai(
                "AI 出网已关闭（仅本地模型可用）：请在设置中开启出网，或改用本地 Ollama provider"
                    .into(),
            ));
        }
        Ok(())
    }
}

/// 从库加载 AI/安全设置；缺键或脏值一律落 [`AiSettings::default`] 的对应默认（fail-open 到「开箱可用」，
/// 但审批/扫描层级的默认本就是安全基线，故不因缺配置而放宽管控）。
pub fn load_ai_settings(db: &Db) -> Result<AiSettings, AppError> {
    let d = AiSettings::default();
    Ok(AiSettings {
        approval_mode: db
            .get_setting(SettingKey::ApprovalMode.as_str())?
            .as_deref()
            .map(ApprovalMode::parse)
            .unwrap_or(d.approval_mode),
        network_enabled: db
            .get_setting(SettingKey::NetworkEnabled.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(d.network_enabled),
        scan_level: db
            .get_setting(SettingKey::ScanLevel.as_str())?
            .as_deref()
            .map(ScanLevel::parse)
            .unwrap_or(d.scan_level),
        memory_enabled: db
            .get_setting(SettingKey::MemoryEnabled.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(d.memory_enabled),
        memory_auto_extract: db
            .get_setting(SettingKey::MemoryAutoExtract.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(d.memory_auto_extract),
        memory_retention_days: db
            .get_setting(SettingKey::MemoryRetentionDays.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .map(|v| v.max(0))
            .unwrap_or(d.memory_retention_days),
        terminal_ai: match db.get_setting(SettingKey::TerminalAi.as_str())? {
            // 新键优先；缺键时回落到旧的三档键（布尔时代与三档时代的值都兼容）
            Some(v) => TerminalAi::parse(&v),
            None => db
                .get_setting(SettingKey::ConsoleMode.as_str())?
                .as_deref()
                .map(TerminalAi::parse)
                .unwrap_or(d.terminal_ai),
        },
        rail: match db.get_setting(SettingKey::RailTab.as_str())? {
            Some(v) => RailTab::parse(&v),
            None => db
                .get_setting(SettingKey::ConsoleMode.as_str())?
                .as_deref()
                .map(RailTab::parse)
                .unwrap_or(d.rail),
        },
        ui_language: db
            .get_setting(SettingKey::UiLanguage.as_str())?
            .as_deref()
            .map(normalize_ui_language)
            .unwrap_or(d.ui_language),
        reply_verbosity: db
            .get_setting(SettingKey::ReplyVerbosity.as_str())?
            .as_deref()
            .map(ReplyVerbosity::parse)
            .unwrap_or(d.reply_verbosity),
        allow_plaintext_password: db
            .get_setting(SettingKey::AllowPlaintextPassword.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(d.allow_plaintext_password),
        max_tool_rounds: db
            .get_setting(SettingKey::MaxToolRounds.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<u32>().ok())
            .map(|v| crate::ai::orchestrator::clamp_max_tool_rounds(v as usize) as u32)
            .unwrap_or(d.max_tool_rounds),
        chat_mode: db
            .get_setting(SettingKey::ChatMode.as_str())?
            .as_deref()
            .map(ChatMode::parse)
            .unwrap_or(d.chat_mode),
        os_notifications: db
            .get_setting(SettingKey::OsNotifications.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(d.os_notifications),
        mem0_url: db
            .get_setting(SettingKey::Mem0Url.as_str())?
            .unwrap_or_default(),
        mem0_key: db
            .get_setting(SettingKey::Mem0Key.as_str())?
            .unwrap_or_default(),
        mem0_user: db
            .get_setting(SettingKey::Mem0User.as_str())?
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| d.mem0_user.clone()),
    })
}

/// 把三项设置写回库（逐键 UPSERT）。
fn save_ai_settings(db: &Db, s: &AiSettings) -> Result<(), AppError> {
    db.set_setting(SettingKey::ApprovalMode.as_str(), s.approval_mode.as_str())?;
    db.set_setting(
        SettingKey::NetworkEnabled.as_str(),
        &s.network_enabled.to_string(),
    )?;
    db.set_setting(SettingKey::ScanLevel.as_str(), s.scan_level.as_str())?;
    db.set_setting(
        SettingKey::MemoryEnabled.as_str(),
        &s.memory_enabled.to_string(),
    )?;
    db.set_setting(
        SettingKey::MemoryAutoExtract.as_str(),
        &s.memory_auto_extract.to_string(),
    )?;
    db.set_setting(
        SettingKey::MemoryRetentionDays.as_str(),
        &s.memory_retention_days.to_string(),
    )?;
    db.set_setting(SettingKey::TerminalAi.as_str(), s.terminal_ai.as_str())?;
    db.set_setting(SettingKey::RailTab.as_str(), s.rail.as_str())?;
    // 旧键继续同步写一份（terminal_ai 为主）：万一回落到只读旧键的构建，语义不至于拧回去
    db.set_setting(
        SettingKey::ConsoleMode.as_str(),
        match (s.terminal_ai, s.rail) {
            (TerminalAi::On, _) => AiLegacyMode::Agent.as_str(),
            (TerminalAi::Off, RailTab::Chat) => AiLegacyMode::Chat.as_str(),
            (TerminalAi::Off, RailTab::Agent) => AiLegacyMode::Shell.as_str(),
        },
    )?;
    db.set_setting(SettingKey::UiLanguage.as_str(), &s.ui_language)?;
    db.set_setting(
        SettingKey::ReplyVerbosity.as_str(),
        s.reply_verbosity.as_str(),
    )?;
    db.set_setting(
        SettingKey::AllowPlaintextPassword.as_str(),
        &s.allow_plaintext_password.to_string(),
    )?;
    db.set_setting(
        SettingKey::MaxToolRounds.as_str(),
        &s.max_tool_rounds.to_string(),
    )?;
    db.set_setting(SettingKey::ChatMode.as_str(), s.chat_mode.as_str())?;
    db.set_setting(
        SettingKey::OsNotifications.as_str(),
        &s.os_notifications.to_string(),
    )?;
    db.set_setting(SettingKey::Mem0Url.as_str(), s.mem0_url.trim())?;
    db.set_setting(SettingKey::Mem0Key.as_str(), s.mem0_key.trim())?;
    db.set_setting(SettingKey::Mem0User.as_str(), s.mem0_user.trim())?;
    Ok(())
}

/// 读取当前 AI/安全设置（前端设置页 + 安全状态灯用；不含任何凭证）。
#[tauri::command]
pub async fn settings_get(app: AppHandle) -> Result<AiSettings, AppError> {
    blocking(move || load_ai_settings(&state(&app).db)).await
}

/// 设置补丁：仅覆盖显式给出的字段（`None` = 不动），便于前端做局部开关而不必回传整份。
/// `Default` 让调用方能用 `..Default::default()` 只填自己要改的那几个键（测试与内部调用都用得上）。
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsPatch {
    pub approval_mode: Option<ApprovalMode>,
    pub network_enabled: Option<bool>,
    pub scan_level: Option<ScanLevel>,
    pub memory_enabled: Option<bool>,
    pub memory_auto_extract: Option<bool>,
    pub memory_retention_days: Option<i64>,
    pub terminal_ai: Option<TerminalAi>,
    pub rail: Option<RailTab>,
    pub ui_language: Option<String>,
    pub reply_verbosity: Option<ReplyVerbosity>,
    pub allow_plaintext_password: Option<bool>,
    pub max_tool_rounds: Option<u32>,
    pub chat_mode: Option<ChatMode>,
    pub os_notifications: Option<bool>,
    pub mem0_url: Option<String>,
    pub mem0_key: Option<String>,
    pub mem0_user: Option<String>,
}

/// 补丁内核（非 tauri 命令，供命令与测试共用）：读当前设置 → 覆盖给出的字段 → 落库 → 返回完整视图。
/// `None` = 不动，所以局部开关不必回传整份。
pub fn settings_apply_patch(db: &Db, patch: &AiSettingsPatch) -> Result<AiSettings, AppError> {
    let mut s = load_ai_settings(db)?;
    if let Some(m) = patch.approval_mode {
        s.approval_mode = m;
    }
    if let Some(n) = patch.network_enabled {
        s.network_enabled = n;
    }
    if let Some(l) = patch.scan_level {
        s.scan_level = l;
    }
    if let Some(m) = patch.memory_enabled {
        s.memory_enabled = m;
    }
    if let Some(m) = patch.memory_auto_extract {
        s.memory_auto_extract = m;
    }
    if let Some(d) = patch.memory_retention_days {
        s.memory_retention_days = d.max(0);
    }
    if let Some(m) = patch.terminal_ai {
        s.terminal_ai = m;
    }
    if let Some(t) = patch.rail {
        s.rail = t;
    }
    if let Some(l) = patch.ui_language.as_deref() {
        s.ui_language = normalize_ui_language(l);
    }
    if let Some(v) = patch.reply_verbosity {
        s.reply_verbosity = v;
    }
    if let Some(a) = patch.allow_plaintext_password {
        s.allow_plaintext_password = a;
    }
    if let Some(r) = patch.max_tool_rounds {
        s.max_tool_rounds = crate::ai::orchestrator::clamp_max_tool_rounds(r as usize) as u32;
    }
    if let Some(m) = patch.chat_mode {
        s.chat_mode = m;
    }
    if let Some(n) = patch.os_notifications {
        s.os_notifications = n;
    }
    if let Some(v) = patch.mem0_url.as_deref() {
        s.mem0_url = v.to_string();
    }
    if let Some(v) = patch.mem0_key.as_deref() {
        s.mem0_key = v.to_string();
    }
    if let Some(v) = patch.mem0_user.as_deref() {
        s.mem0_user = v.to_string();
    }
    save_ai_settings(db, &s)?;
    Ok(s)
}

/// 更新 AI/安全设置，返回落库后的完整视图（前端据此回填 UI）。
#[tauri::command]
pub async fn settings_set(app: AppHandle, patch: AiSettingsPatch) -> Result<AiSettings, AppError> {
    blocking(move || settings_apply_patch(&state(&app).db, &patch)).await
}

/// 恢复默认：删除已存的三项 AI/安全设置键（回落到 [`AiSettings::default`]），返回默认视图。
/// 只删本层认识的键，不影响其他设置分类（终端/外观等留待后续阶段）。
#[tauri::command]
pub async fn settings_reset(app: AppHandle) -> Result<AiSettings, AppError> {
    blocking(move || {
        let db = &state(&app).db;
        db.delete_setting(SettingKey::ApprovalMode.as_str())?;
        db.delete_setting(SettingKey::NetworkEnabled.as_str())?;
        db.delete_setting(SettingKey::ScanLevel.as_str())?;
        db.delete_setting(SettingKey::MemoryEnabled.as_str())?;
        db.delete_setting(SettingKey::MemoryAutoExtract.as_str())?;
        db.delete_setting(SettingKey::MemoryRetentionDays.as_str())?;
        db.delete_setting(SettingKey::ConsoleMode.as_str())?;
        db.delete_setting(SettingKey::TerminalAi.as_str())?;
        db.delete_setting(SettingKey::RailTab.as_str())?;
        db.delete_setting(SettingKey::UiLanguage.as_str())?;
        db.delete_setting(SettingKey::ReplyVerbosity.as_str())?;
        db.delete_setting(SettingKey::AllowPlaintextPassword.as_str())?;
        db.delete_setting(SettingKey::MaxToolRounds.as_str())?;
        db.delete_setting(SettingKey::ChatMode.as_str())?;
        db.delete_setting(SettingKey::OsNotifications.as_str())?;
        db.delete_setting(SettingKey::ExecMode.as_str())?;
        // 删键后重新加载即得默认（load 对缺键回落 default）。
        load_ai_settings(db)
    })
    .await
}

// ─────────────── 系统提示词模板（开发者模式）───────────────

/// 读取 dev 覆盖的系统提示词模板原始值（`None` = 从未改过，走内置默认）。
pub fn get_prompt_template(db: &Db) -> Result<Option<String>, AppError> {
    db.get_setting(SettingKey::AiPromptTemplate.as_str())
}

/// 运行期应使用的模板：有非空覆盖用覆盖，否则回落内置 [`crate::ai::context::DEFAULT_TEMPLATE`]。
/// 这是「空即真相源」的唯一收敛点——prepare_turn 与预览都经它取模板，避免各处各写一套回落。
pub fn effective_prompt_template(db: &Db) -> Result<String, AppError> {
    Ok(get_prompt_template(db)?
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| crate::ai::context::DEFAULT_TEMPLATE.to_string()))
}

/// 写入 dev 覆盖模板；传空/纯空白等价于清除覆盖（回落内置默认）。
pub fn set_prompt_template(db: &Db, template: &str) -> Result<(), AppError> {
    if template.trim().is_empty() {
        db.delete_setting(SettingKey::AiPromptTemplate.as_str())
            .map(|_| ())
    } else {
        db.set_setting(SettingKey::AiPromptTemplate.as_str(), template)
    }
}

/// 恢复默认：删覆盖键（下次直接用内置 [`crate::ai::context::DEFAULT_TEMPLATE`]）。
pub fn reset_prompt_template(db: &Db) -> Result<(), AppError> {
    db.delete_setting(SettingKey::AiPromptTemplate.as_str())
        .map(|_| ())
}

// ─────────────────────── 外观 / 主题───────────────────────

// 外观相关键名收敛到文件顶部的 [`SettingKey`]（Theme / FontSize / FontFamily / CustomTheme）。

/// 默认终端配色主题 id（对应前端 `lib/themes.ts` 的内置预设，即 原硬编码配色）。
pub const DEFAULT_THEME: &str = "sailterm-dark";
/// 默认字体族（与 `Terminal.tsx` 原硬编码值一致）。
pub const DEFAULT_FONT_FAMILY: &str = "'Cascadia Code', 'JetBrains Mono', Consolas, monospace";
/// 默认字号（px）。
pub const DEFAULT_FONT_SIZE: f64 = 13.0;
/// 字号允许区间（超出即 clamp，防脏值把终端搞到不可读）。
const FONT_SIZE_MIN: f64 = 8.0;
const FONT_SIZE_MAX: f64 = 32.0;
/// 自定义主题 id：选中它时用 `custom_theme` 的颜色而非内置预设。
pub const CUSTOM_THEME_ID: &str = "custom";
/// 自定义主题色键数量上限（防超大/损坏 JSON 撑爆设置表）。
const MAX_THEME_COLORS: usize = 64;

/// 规范化的自定义终端配色（xterm `ITheme` 子集）：`colors` 是「色键 → CSS 颜色字符串」，
/// 色键沿用 xterm 命名（background/foreground/cursor/selection/black…brightWhite）。
/// 仅承载用户导入的 JSON 主题，持久化在 settings 表；**只含颜色，绝不含凭证**。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CustomTheme {
    /// 展示名（导入时取自 JSON `name`，缺省「自定义主题」）。
    pub name: String,
    /// 色键 → CSS 颜色（如 `background` → `#0a0e13`）。BTreeMap 保证序列化稳定有序。
    pub colors: BTreeMap<String, String>,
}

/// 外观设置的类型化视图（「外观/主题」）。全部为**非敏感偏好**。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppearanceSettings {
    /// 选中的主题 id：内置预设 id，或 `"custom"`（用 `custom_theme` 的颜色）。
    pub theme: String,
    /// 终端字号（px），clamp 到 `8..=32`。
    pub font_size: f64,
    /// 终端字体族（CSS font-family 值）。
    pub font_family: String,
    /// 用户导入的自定义主题（`theme == "custom"` 时生效）；未导入为 `None`。
    pub custom_theme: Option<CustomTheme>,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: DEFAULT_THEME.to_string(),
            font_size: DEFAULT_FONT_SIZE,
            font_family: DEFAULT_FONT_FAMILY.to_string(),
            custom_theme: None,
        }
    }
}

/// 把字号 clamp 到允许区间（NaN 落默认，避免 `f64::clamp` 对 NaN panic）。
fn clamp_font_size(v: f64) -> f64 {
    if v.is_nan() {
        DEFAULT_FONT_SIZE
    } else {
        v.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX)
    }
}

/// 从库加载外观设置；缺键/脏值一律落默认（与 `load_ai_settings` 同 fail-safe 风格）。
pub fn load_appearance(db: &Db) -> Result<AppearanceSettings, AppError> {
    let d = AppearanceSettings::default();
    let theme = db
        .get_setting(SettingKey::Theme.as_str())?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or(d.theme);
    let font_size = db
        .get_setting(SettingKey::FontSize.as_str())?
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .map(clamp_font_size)
        .unwrap_or(d.font_size);
    let font_family = db
        .get_setting(SettingKey::FontFamily.as_str())?
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or(d.font_family);
    let custom_theme = db
        .get_setting(SettingKey::CustomTheme.as_str())?
        .as_deref()
        .and_then(|s| serde_json::from_str::<CustomTheme>(s).ok());
    Ok(AppearanceSettings {
        theme,
        font_size,
        font_family,
        custom_theme,
    })
}

/// 写回外观设置（逐键 UPSERT；`custom_theme` 为 `None` 时删除该键，不留脏 JSON）。
fn save_appearance(db: &Db, s: &AppearanceSettings) -> Result<(), AppError> {
    db.set_setting(SettingKey::Theme.as_str(), &s.theme)?;
    db.set_setting(SettingKey::FontSize.as_str(), &s.font_size.to_string())?;
    db.set_setting(SettingKey::FontFamily.as_str(), &s.font_family)?;
    match &s.custom_theme {
        Some(ct) => {
            let json = serde_json::to_string(ct)
                .map_err(|e| AppError::Internal(format!("序列化自定义主题失败：{e}")))?;
            db.set_setting(SettingKey::CustomTheme.as_str(), &json)?;
        }
        None => {
            db.delete_setting(SettingKey::CustomTheme.as_str())?;
        }
    }
    Ok(())
}

/// 读取当前外观设置（前端启动 + 设置页用；不含任何凭证）。
#[tauri::command]
pub async fn appearance_get(app: AppHandle) -> Result<AppearanceSettings, AppError> {
    blocking(move || load_appearance(&state(&app).db)).await
}

/// 外观设置补丁：仅覆盖显式给出的字段（`None` = 不动），便于前端做即时热切换而不必回传整份。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppearancePatch {
    pub theme: Option<String>,
    pub font_size: Option<f64>,
    pub font_family: Option<String>,
}

/// 更新外观设置，返回落库后的完整视图。前端据此热切换运行中的 xterm（重建 options，不重建实例）。
#[tauri::command]
pub async fn appearance_set(
    app: AppHandle,
    patch: AppearancePatch,
) -> Result<AppearanceSettings, AppError> {
    blocking(move || {
        let db = &state(&app).db;
        let mut s = load_appearance(db)?;
        if let Some(t) = patch
            .theme
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
        {
            s.theme = t;
        }
        if let Some(fs) = patch.font_size {
            s.font_size = clamp_font_size(fs);
        }
        if let Some(ff) = patch
            .font_family
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty())
        {
            s.font_family = ff;
        }
        save_appearance(db, &s)?;
        Ok(s)
    })
    .await
}

/// 恢复默认外观：删除外观相关键，返回默认视图。只删本层认识的键，不影响 AI/安全设置。
#[tauri::command]
pub async fn appearance_reset(app: AppHandle) -> Result<AppearanceSettings, AppError> {
    blocking(move || {
        let db = &state(&app).db;
        db.delete_setting(SettingKey::Theme.as_str())?;
        db.delete_setting(SettingKey::FontSize.as_str())?;
        db.delete_setting(SettingKey::FontFamily.as_str())?;
        db.delete_setting(SettingKey::CustomTheme.as_str())?;
        load_appearance(db)
    })
    .await
}

/// 从 JSON 文件导入自定义主题（`~/.config/sailterm/themes/*.json`）：
/// 读取 → 校验 → 持久化为 `custom_theme` 并切到 `"custom"`，返回更新后的外观设置。
/// 走后端 `std::fs`（复用连接导入/导出与 `local_*` 的 IO 模式），**不引入 dialog/fs 插件**。
#[tauri::command]
pub async fn theme_import_file(
    app: AppHandle,
    path: String,
) -> Result<AppearanceSettings, AppError> {
    blocking(move || {
        let db = &state(&app).db;
        let path = path.trim();
        if path.is_empty() {
            return Err(AppError::Invalid("主题文件路径不能为空".into()));
        }
        let json = std::fs::read_to_string(path)
            .map_err(|e| AppError::Io(format!("读取 {} 失败：{e}", path)))?;
        let ct = parse_custom_theme(&json)?;
        let mut s = load_appearance(db)?;
        s.custom_theme = Some(ct);
        s.theme = CUSTOM_THEME_ID.to_string();
        save_appearance(db, &s)?;
        Ok(s)
    })
    .await
}

/// 目录扫描发现的一个自定义主题：只带**名称与文件路径**（颜色在点选时才走 [`theme_import_file`] 读取套用）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredTheme {
    pub name: String,
    pub path: String,
}

/// [`theme_scan_dir`] 返回：扫描的目录（供 UI 提示“把 *.json 放这里”）+ 其内校验通过的主题清单。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ThemeScanResult {
    pub dir: String,
    pub themes: Vec<DiscoveredTheme>,
}

/// 扫描 `dir` 里的 `*.json`，逐个用 [`parse_custom_theme`] 校验；解析失败的**静默跳过**
///（不让一个坏文件拖垮整个列表）。目录不存在 = 空清单（非错误）。按名大写不敏感排序。
///
/// 与命令解耦的纯函数（只接一个 `&Path`），便于离线单测（用临时目录灌合法/非法样本）。
pub fn scan_themes_dir(dir: &std::path::Path) -> Vec<DiscoveredTheme> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        let is_json = p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("json"))
            .unwrap_or(false);
        if !is_json {
            continue;
        }
        let Ok(json) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Ok(ct) = parse_custom_theme(&json) else {
            continue;
        };
        out.push(DiscoveredTheme {
            name: ct.name,
            path: p.to_string_lossy().to_string(),
        });
    }
    out.sort_by_key(|d| d.name.to_lowercase());
    out
}

/// 扫描应用数据目录下的 `themes/`，返回可点选的自定义主题清单。
/// 点选某项由前端回调 [`theme_import_file`]（传该文件路径）套用——复用单活跃 `custom_theme` 模型，不新增存储。
#[tauri::command]
pub async fn theme_scan_dir(app: AppHandle) -> Result<ThemeScanResult, AppError> {
    blocking(move || {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| AppError::Internal(format!("无法定位数据目录：{e}")))?
            .join("themes");
        let themes = scan_themes_dir(&dir);
        Ok(ThemeScanResult {
            dir: dir.to_string_lossy().to_string(),
            themes,
        })
    })
    .await
}

/// 解析并校验自定义主题 JSON：顶层必须是对象；`name` 取字符串（缺省「自定义主题」）；
/// 其余「纯 ASCII 字母键 + 非空字符串值」收进 colors（数量封顶 [`MAX_THEME_COLORS`]）；
/// 必须含 `background` + `foreground`，否则视为不可用主题拒绝（E_INVALID）。
fn parse_custom_theme(json: &str) -> Result<CustomTheme, AppError> {
    let v: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| AppError::Invalid(format!("主题 JSON 解析失败：{e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| AppError::Invalid("主题 JSON 顶层必须是对象".into()))?;
    let name = obj
        .get("name")
        .and_then(|n| n.as_str())
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "自定义主题".to_string());
    let mut colors = BTreeMap::new();
    for (k, val) in obj {
        // 只收 xterm ITheme 风格的色键：纯 ASCII 字母名（滤掉 version/bg-alt 等噪声）+ 非空字符串值
        if k == "name" || !k.chars().all(|c| c.is_ascii_alphabetic()) {
            continue;
        }
        if let Some(s) = val.as_str().map(str::trim).filter(|s| !s.is_empty()) {
            if colors.len() < MAX_THEME_COLORS {
                colors.insert(k.clone(), s.to_string());
            }
        }
    }
    for need in ["background", "foreground"] {
        if !colors.contains_key(need) {
            return Err(AppError::Invalid(format!("主题缺少必需颜色：{need}")));
        }
    }
    Ok(CustomTheme { name, colors })
}

// ─────────────────────── 终端 / Shell 偏好───────────────────────

// 终端偏好键名收敛到文件顶部的 [`SettingKey`]（Scrollback / CursorBlink / LocalShell / CopyOnSelect / CursorStyle / BellStyle）。

/// 光标样式（xterm `cursorStyle`）：块 / 竖线 / 下划线。
pub fn clamp_cursor_style(v: &str) -> &'static str {
    match v {
        "bar" => "bar",
        "underline" => "underline",
        _ => "block",
    }
}

/// 响铃样式（xterm `bellStyle`）：无声 / 视觉闪烁 / 系统提示音。
pub fn clamp_bell_style(v: &str) -> &'static str {
    match v {
        "visual" => "visual",
        "sound" => "sound",
        _ => "none",
    }
}

/// 默认滚动缓冲行数（与 `Terminal.tsx` 原硬编码一致）。
pub const DEFAULT_SCROLLBACK: i64 = 5000;
/// 滚动缓冲允许区间（过小丢历史，过大吃内存）。
const SCROLLBACK_MIN: i64 = 100;
const SCROLLBACK_MAX: i64 = 100_000;

/// 终端行为偏好。全部为**非敏感偏好**，绝不含凭证。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSettings {
    /// 滚动缓冲行数，clamp 到 `100..=100000`。
    pub scrollback: i64,
    /// 光标闪烁（xterm `cursorBlink`）。
    pub cursor_blink: bool,
    /// 本地 shell 可执行路径；空串 = 平台默认（Windows ComSpec、Unix `$SHELL`）。
    pub local_shell: String,
    /// 选中文本即复制（xterm `onSelectionChange` + 剪贴板）。
    pub copy_on_select: bool,
    /// 光标样式：block / bar / underline。
    pub cursor_style: String,
    /// 响铃样式：none / visual / sound。
    pub bell_style: String,
    /// 关键字着色：按触发规则给终端输出里的 error/warn/IP 等着色。默认开。
    pub keyword_highlight: bool,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            scrollback: DEFAULT_SCROLLBACK,
            cursor_blink: true,
            local_shell: String::new(),
            copy_on_select: false,
            cursor_style: "block".into(),
            bell_style: "none".into(),
            keyword_highlight: true,
        }
    }
}

/// 把滚动缓冲 clamp 到允许区间。
fn clamp_scrollback(v: i64) -> i64 {
    v.clamp(SCROLLBACK_MIN, SCROLLBACK_MAX)
}

/// 从库加载终端偏好；缺键/脏值一律落默认（与 `load_ai_settings` 同 fail-safe 风格）。
pub fn load_terminal(db: &Db) -> Result<TerminalSettings, AppError> {
    let d = TerminalSettings::default();
    Ok(TerminalSettings {
        scrollback: db
            .get_setting(SettingKey::Scrollback.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok())
            .map(clamp_scrollback)
            .unwrap_or(d.scrollback),
        cursor_blink: db
            .get_setting(SettingKey::CursorBlink.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(d.cursor_blink),
        local_shell: db
            .get_setting(SettingKey::LocalShell.as_str())?
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        copy_on_select: db
            .get_setting(SettingKey::CopyOnSelect.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(d.copy_on_select),
        cursor_style: db
            .get_setting(SettingKey::CursorStyle.as_str())?
            .as_deref()
            .map(clamp_cursor_style)
            .unwrap_or(d.cursor_style.as_str())
            .to_string(),
        bell_style: db
            .get_setting(SettingKey::BellStyle.as_str())?
            .as_deref()
            .map(clamp_bell_style)
            .unwrap_or(d.bell_style.as_str())
            .to_string(),
        keyword_highlight: db
            .get_setting(SettingKey::KeywordHighlight.as_str())?
            .as_deref()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or(d.keyword_highlight),
    })
}

/// 写回终端偏好（逐键 UPSERT）。
fn save_terminal(db: &Db, s: &TerminalSettings) -> Result<(), AppError> {
    db.set_setting(
        SettingKey::Scrollback.as_str(),
        &clamp_scrollback(s.scrollback).to_string(),
    )?;
    db.set_setting(
        SettingKey::CursorBlink.as_str(),
        &s.cursor_blink.to_string(),
    )?;
    db.set_setting(SettingKey::LocalShell.as_str(), s.local_shell.trim())?;
    db.set_setting(
        SettingKey::CopyOnSelect.as_str(),
        &s.copy_on_select.to_string(),
    )?;
    db.set_setting(
        SettingKey::CursorStyle.as_str(),
        clamp_cursor_style(&s.cursor_style),
    )?;
    db.set_setting(
        SettingKey::BellStyle.as_str(),
        clamp_bell_style(&s.bell_style),
    )?;
    db.set_setting(
        SettingKey::KeywordHighlight.as_str(),
        &s.keyword_highlight.to_string(),
    )
}

/// 读取当前终端偏好。
#[tauri::command]
pub async fn terminal_get(app: AppHandle) -> Result<TerminalSettings, AppError> {
    blocking(move || load_terminal(&state(&app).db)).await
}

/// 终端偏好补丁：仅覆盖显式给出的字段（`None` = 不动）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalPatch {
    pub scrollback: Option<i64>,
    pub cursor_blink: Option<bool>,
    pub local_shell: Option<String>,
    pub copy_on_select: Option<bool>,
    pub cursor_style: Option<String>,
    pub bell_style: Option<String>,
    pub keyword_highlight: Option<bool>,
}

/// 更新终端偏好，返回落库后的完整视图（前端据此热切换 xterm options）。
#[tauri::command]
pub async fn terminal_set(
    app: AppHandle,
    patch: TerminalPatch,
) -> Result<TerminalSettings, AppError> {
    blocking(move || {
        let db = &state(&app).db;
        let mut s = load_terminal(db)?;
        if let Some(v) = patch.scrollback {
            s.scrollback = clamp_scrollback(v);
        }
        if let Some(v) = patch.cursor_blink {
            s.cursor_blink = v;
        }
        if let Some(v) = patch.local_shell {
            s.local_shell = v.trim().to_string();
        }
        if let Some(v) = patch.copy_on_select {
            s.copy_on_select = v;
        }
        if let Some(v) = patch.cursor_style {
            s.cursor_style = clamp_cursor_style(&v).to_string();
        }
        if let Some(v) = patch.bell_style {
            s.bell_style = clamp_bell_style(&v).to_string();
        }
        if let Some(v) = patch.keyword_highlight {
            s.keyword_highlight = v;
        }
        save_terminal(db, &s)?;
        Ok(s)
    })
    .await
}

/// 恢复默认终端偏好：删除终端相关键，返回默认视图。
#[tauri::command]
pub async fn terminal_reset(app: AppHandle) -> Result<TerminalSettings, AppError> {
    blocking(move || {
        let db = &state(&app).db;
        db.delete_setting(SettingKey::Scrollback.as_str())?;
        db.delete_setting(SettingKey::CursorBlink.as_str())?;
        db.delete_setting(SettingKey::LocalShell.as_str())?;
        db.delete_setting(SettingKey::CopyOnSelect.as_str())?;
        db.delete_setting(SettingKey::CursorStyle.as_str())?;
        db.delete_setting(SettingKey::BellStyle.as_str())?;
        db.delete_setting(SettingKey::KeywordHighlight.as_str())?;
        load_terminal(db)
    })
    .await
}

// ─────────────────────── 传输 / 小文件打包───────────────────────

/// 读取打包传输总开关（缺键/脏值→默认关）。供命令层 `run_upload_dir` 判定打包是否启用。
/// 功能暂不稳定，已雪藏：默认关，且用户设置页不再提供开关（GeneralSettings 已撤下该 Toggle）；
/// 将来放开时把此默认值翻回 true 并把设置项加回即可（IPC 命令与 bundle 本体代码均保留）。
pub fn bundle_transfer_enabled(db: &Db) -> Result<bool, AppError> {
    Ok(db
        .get_setting(SettingKey::BundleTransferEnabled.as_str())?
        .as_deref()
        .and_then(|s| s.parse::<bool>().ok())
        .unwrap_or(false))
}

/// 读当前打包开关（前端设置页用）。
#[tauri::command]
pub async fn transfer_bundle_get(app: AppHandle) -> Result<bool, AppError> {
    blocking(move || bundle_transfer_enabled(&state(&app).db)).await
}

/// 设置打包开关，回写后的值。
#[tauri::command]
pub async fn transfer_bundle_set(app: AppHandle, enabled: bool) -> Result<bool, AppError> {
    blocking(move || {
        let db = &state(&app).db;
        db.set_setting(SettingKey::BundleTransferEnabled.as_str(), &enabled.to_string())?;
        Ok(enabled)
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    /// 默认即安全基线：只读自动放行、出网开、标准扫描。
    #[test]
    fn default_is_safe_baseline() {
        let d = AiSettings::default();
        assert_eq!(d.approval_mode, ApprovalMode::AutoReadonly);
        assert!(d.network_enabled);
        assert_eq!(d.scan_level, ScanLevel::Standard);
        // 回复默认精简：运维在终端只要重点
        assert_eq!(d.reply_verbosity, ReplyVerbosity::Concise);
        // 终端 AI 默认开（命令与问题都能敲）；页签默认聊天
        assert_eq!(d.terminal_ai, TerminalAi::On);
        // 页签默认聊天；执行通道不再是独立设置：Agent 页签恒把命令打进 PTY，Chat 恒静默 exec
        assert_eq!(d.rail, RailTab::Chat);
        // 审批模式→PolicyEngine 的 approve_readonly 映射：仅 All 为 true
        assert!(!ApprovalMode::AutoReadonly.approve_readonly());
        assert!(ApprovalMode::All.approve_readonly());
    }

    ///出网关闭时云端 provider 被闸门拦下；本地 Ollama 仍放行（数据不出本机）。
    #[test]
    fn egress_gate_blocks_cloud_only_when_disabled() {
        let off = AiSettings {
            network_enabled: false,
            ..AiSettings::default()
        };
        for cloud in [
            ProviderKind::Openai,
            ProviderKind::Anthropic,
            ProviderKind::OpenaiCompatible,
        ] {
            assert!(
                matches!(off.check_egress(cloud), Err(AppError::Ai(_))),
                "出网关闭时云端 {cloud:?} 应被拦"
            );
        }
        // 本地模型不受出网开关影响（即「灰=仅本地」档）
        assert!(off.check_egress(ProviderKind::Ollama).is_ok());

        // 出网开启：四类 provider 全部放行
        let on = AiSettings::default();
        for kind in [
            ProviderKind::Openai,
            ProviderKind::Anthropic,
            ProviderKind::OpenaiCompatible,
            ProviderKind::Ollama,
        ] {
            assert!(on.check_egress(kind).is_ok(), "出网开启时 {kind:?} 应放行");
        }
    }

    /// 旧库兼容：这个概念先以布尔（`ai.console_mode`）、后以三档枚举存过，新键缺位时必须
    /// 从旧键映射出两个独立设置，不能回落成默认（用户选过的不能丢）。
    #[test]
    fn legacy_console_mode_values_map_to_both_new_settings() {
        let db = db();
        db.set_setting(SettingKey::ConsoleMode.as_str(), "false")
            .unwrap();
        let s = load_ai_settings(&db).unwrap();
        assert_eq!((s.terminal_ai, s.rail), (TerminalAi::Off, RailTab::Chat));

        db.set_setting(SettingKey::ConsoleMode.as_str(), "true")
            .unwrap();
        let s = load_ai_settings(&db).unwrap();
        assert_eq!((s.terminal_ai, s.rail), (TerminalAi::On, RailTab::Agent));

        db.set_setting(SettingKey::ConsoleMode.as_str(), "shell")
            .unwrap();
        let s = load_ai_settings(&db).unwrap();
        assert_eq!((s.terminal_ai, s.rail), (TerminalAi::Off, RailTab::Agent));

        db.set_setting(SettingKey::ConsoleMode.as_str(), "chat")
            .unwrap();
        let s = load_ai_settings(&db).unwrap();
        assert_eq!((s.terminal_ai, s.rail), (TerminalAi::Off, RailTab::Chat));
    }

    /// 两个设置互不牵动（用户纠正的核心点）：改页签不影响终端开关，反之亦然；
    /// 新键存在时以新键为准，不再受旧键残值影响。
    #[test]
    fn terminal_ai_and_rail_are_independent() {
        let db = db();
        save_ai_settings(
            &db,
            &AiSettings {
                terminal_ai: TerminalAi::On,
                rail: RailTab::Chat,
                ..AiSettings::default()
            },
        )
        .unwrap();
        // 只在聊天与 Agent 页签之间来回切：终端开关必须恒为「开」
        for rail in [RailTab::Chat, RailTab::Agent, RailTab::Chat] {
            settings_apply_patch(
                &db,
                &AiSettingsPatch {
                    rail: Some(rail),
                    ..Default::default()
                },
            )
            .unwrap();
            let s = load_ai_settings(&db).unwrap();
            assert_eq!(s.terminal_ai, TerminalAi::On, "切页签不该动终端开关");
            assert_eq!(s.rail, rail);
        }
        // 再把终端关掉：页签必须仍在聊天
        settings_apply_patch(
            &db,
            &AiSettingsPatch {
                terminal_ai: Some(TerminalAi::Off),
                ..Default::default()
            },
        )
        .unwrap();
        let s = load_ai_settings(&db).unwrap();
        assert_eq!(s.terminal_ai, TerminalAi::Off);
        assert_eq!(s.rail, RailTab::Chat, "改终端开关不该动页签");
        // 旧键同步写入，供只读旧键的路径回落
        assert_eq!(
            db.get_setting(SettingKey::ConsoleMode.as_str())
                .unwrap()
                .as_deref(),
            Some("chat")
        );
    }

    /// 空库 → 全部落默认（缺键不放宽管控）。
    #[test]
    fn load_defaults_on_empty_db() {
        assert_eq!(load_ai_settings(&db()).unwrap(), AiSettings::default());
    }

    /// 存→读往返一致（settings_set / settings_get 的内核）。
    #[test]
    fn save_load_roundtrip() {
        let db = db();
        let s = AiSettings {
            approval_mode: ApprovalMode::All,
            network_enabled: false,
            scan_level: ScanLevel::Strict,
            memory_enabled: false,
            memory_auto_extract: false,
            memory_retention_days: 90,
            reply_verbosity: ReplyVerbosity::Detailed,
            mem0_url: String::new(),
            mem0_key: String::new(),
            mem0_user: "default".into(),
            terminal_ai: TerminalAi::Off,
            rail: RailTab::Chat,
            ..Default::default()
        };
        save_ai_settings(&db, &s).unwrap();
        assert_eq!(load_ai_settings(&db).unwrap(), s);
    }

    /// 脏值 / 无法解析一律回落默认，绝不因坏配置放宽（fail-safe 到基线）。
    #[test]
    fn dirty_values_fall_back_to_default() {
        let db = db();
        db.set_setting(SettingKey::ApprovalMode.as_str(), "not-a-mode")
            .unwrap();
        db.set_setting(SettingKey::NetworkEnabled.as_str(), "maybe")
            .unwrap();
        db.set_setting(SettingKey::ScanLevel.as_str(), "ultra")
            .unwrap();
        db.set_setting(SettingKey::MemoryEnabled.as_str(), "yep")
            .unwrap();
        db.set_setting(SettingKey::MemoryAutoExtract.as_str(), "nope")
            .unwrap();
        db.set_setting(SettingKey::MemoryRetentionDays.as_str(), "soon")
            .unwrap();
        db.set_setting(SettingKey::ConsoleMode.as_str(), "sideways")
            .unwrap();
        db.set_setting(SettingKey::TerminalAi.as_str(), "sideways")
            .unwrap();
        db.set_setting(SettingKey::ExecMode.as_str(), "sideways")
            .unwrap();
        db.set_setting(SettingKey::RailTab.as_str(), "sideways")
            .unwrap();
        db.set_setting(SettingKey::ReplyVerbosity.as_str(), "chatty")
            .unwrap();
        assert_eq!(load_ai_settings(&db).unwrap(), AiSettings::default());
    }

    /// 恢复默认：删键后重载即得默认（settings_reset 的内核）。
    #[test]
    fn reset_restores_defaults() {
        let db = db();
        save_ai_settings(
            &db,
            &AiSettings {
                approval_mode: ApprovalMode::All,
                network_enabled: false,
                scan_level: ScanLevel::Strict,
                memory_enabled: false,
                memory_auto_extract: false,
                memory_retention_days: 90,
                terminal_ai: TerminalAi::Off,
                rail: RailTab::Chat,
                reply_verbosity: ReplyVerbosity::Detailed,
                ..Default::default()
            },
        )
        .unwrap();
        // 非默认已落库
        assert_ne!(load_ai_settings(&db).unwrap(), AiSettings::default());
        db.delete_setting(SettingKey::ApprovalMode.as_str())
            .unwrap();
        db.delete_setting(SettingKey::NetworkEnabled.as_str())
            .unwrap();
        db.delete_setting(SettingKey::ScanLevel.as_str()).unwrap();
        db.delete_setting(SettingKey::MemoryEnabled.as_str())
            .unwrap();
        db.delete_setting(SettingKey::MemoryAutoExtract.as_str())
            .unwrap();
        db.delete_setting(SettingKey::MemoryRetentionDays.as_str())
            .unwrap();
        db.delete_setting(SettingKey::ConsoleMode.as_str()).unwrap();
        db.delete_setting(SettingKey::TerminalAi.as_str()).unwrap();
        db.delete_setting(SettingKey::RailTab.as_str()).unwrap();
        db.delete_setting(SettingKey::ReplyVerbosity.as_str())
            .unwrap();
        assert_eq!(load_ai_settings(&db).unwrap(), AiSettings::default());
    }

    /// 外观默认值 + 存读往返（appearance_get/set 的内核）。
    #[test]
    fn appearance_defaults_and_roundtrip() {
        let db = db();
        let d = load_appearance(&db).unwrap();
        assert_eq!(d, AppearanceSettings::default());
        assert_eq!(d.theme, DEFAULT_THEME);
        assert_eq!(d.font_size, DEFAULT_FONT_SIZE);
        assert!(d.custom_theme.is_none());
        // 存非默认 → 读回一致
        let s = AppearanceSettings {
            theme: "dracula".to_string(),
            font_size: 16.0,
            font_family: "Menlo".to_string(),
            custom_theme: None,
        };
        save_appearance(&db, &s).unwrap();
        assert_eq!(load_appearance(&db).unwrap(), s);
    }

    /// 字号 clamp：超区间收敛、NaN 落默认、库内脏值落默认（防终端不可读）。
    #[test]
    fn appearance_font_size_clamped() {
        assert_eq!(clamp_font_size(99.0), FONT_SIZE_MAX);
        assert_eq!(clamp_font_size(1.0), FONT_SIZE_MIN);
        assert_eq!(clamp_font_size(f64::NAN), DEFAULT_FONT_SIZE);
        assert_eq!(clamp_font_size(15.0), 15.0);
        let db = db();
        db.set_setting(SettingKey::FontSize.as_str(), "abc")
            .unwrap();
        assert_eq!(load_appearance(&db).unwrap().font_size, DEFAULT_FONT_SIZE);
    }

    /// 自定义主题解析：合法通过（name 缺省、跳过非色键/非字符串值）；缺色/非对象/非法 JSON 即拒。
    #[test]
    fn parse_custom_theme_guards() {
        let ok = parse_custom_theme(
            r##"{"name":"My","background":"#000","foreground":"#fff","cursor":"#0f0","version":3,"bg-alt":"#111"}"##,
        )
        .unwrap();
        assert_eq!(ok.name, "My");
        assert_eq!(
            ok.colors.get("background").map(String::as_str),
            Some("#000")
        );
        assert_eq!(ok.colors.get("cursor").map(String::as_str), Some("#0f0"));
        assert!(!ok.colors.contains_key("version")); // 非字符串值跳过
        assert!(!ok.colors.contains_key("bg-alt")); // 非纯字母键跳过
        assert!(!ok.colors.contains_key("name")); // name 不入 colors
        assert!(matches!(
            parse_custom_theme(r##"{"background":"#000"}"##),
            Err(AppError::Invalid(_))
        ));
        assert!(matches!(
            parse_custom_theme("[1,2]"),
            Err(AppError::Invalid(_))
        ));
        assert!(matches!(
            parse_custom_theme("{oops"),
            Err(AppError::Invalid(_))
        ));
    }

    /// 主题目录扫描：合法 json 入选并按名不敏感排序；非法 json 与非 json 文件静默跳过；目录不存在返回空。
    #[test]
    fn scan_themes_dir_lists_valid_skips_bad() {
        use std::fs;
        let base =
            std::env::temp_dir().join(format!("sailterm-themes-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        fs::write(
            base.join("b.json"),
            r##"{"name":"Bravo","background":"#000","foreground":"#fff"}"##,
        )
        .unwrap();
        fs::write(
            base.join("a.json"),
            r##"{"name":"alpha","background":"#111","foreground":"#eee"}"##,
        )
        .unwrap();
        // 非法：缺 foreground，parse_custom_theme 会拒 → 跳过
        fs::write(base.join("bad.json"), r##"{"background":"#000"}"##).unwrap();
        // 非 json 后缀：忽略
        fs::write(base.join("note.txt"), "hello").unwrap();

        let got = scan_themes_dir(&base);
        let _ = fs::remove_dir_all(&base);

        let names: Vec<&str> = got.iter().map(|d| d.name.as_str()).collect();
        // 大写不敏感排序：alpha < Bravo
        assert_eq!(names, vec!["alpha", "Bravo"]);
        assert!(got.iter().all(|d| d.path.ends_with(".json")));
        // 目录不存在 → 空清单（非错误）
        assert!(scan_themes_dir(Path::new("/no/such/themes-dir-xyz")).is_empty());
    }

    /// 自定义主题持久化往返；置 None 时删键（theme_import_file/appearance_reset 的内核）。
    #[test]
    fn custom_theme_persist_roundtrip() {
        let db = db();
        let ct = CustomTheme {
            name: "Night".to_string(),
            colors: [
                ("background".to_string(), "#0a0e13".to_string()),
                ("foreground".to_string(), "#d7e2ec".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let s = AppearanceSettings {
            theme: CUSTOM_THEME_ID.to_string(),
            custom_theme: Some(ct.clone()),
            ..AppearanceSettings::default()
        };
        save_appearance(&db, &s).unwrap();
        let loaded = load_appearance(&db).unwrap();
        assert_eq!(loaded.theme, CUSTOM_THEME_ID);
        assert_eq!(loaded.custom_theme, Some(ct));
        // 置 None → 删键
        save_appearance(
            &db,
            &AppearanceSettings {
                custom_theme: None,
                ..loaded
            },
        )
        .unwrap();
        assert!(load_appearance(&db).unwrap().custom_theme.is_none());
        assert!(db
            .get_setting(SettingKey::CustomTheme.as_str())
            .unwrap()
            .is_none());
    }

    /// 终端偏好默认值 + 存读往返 + 滚动缓冲 clamp。
    #[test]
    fn terminal_defaults_roundtrip_and_clamp() {
        let db = db();
        assert_eq!(load_terminal(&db).unwrap(), TerminalSettings::default());
        let s = TerminalSettings {
            scrollback: 20_000,
            cursor_blink: false,
            local_shell: "  pwsh  ".to_string(),
            keyword_highlight: false,
            ..Default::default()
        };
        save_terminal(&db, &s).unwrap();
        let loaded = load_terminal(&db).unwrap();
        assert_eq!(loaded.scrollback, 20_000);
        assert!(!loaded.cursor_blink);
        assert_eq!(loaded.local_shell, "pwsh"); // trim 后落库
        assert!(!loaded.keyword_highlight); // 默认开，显式关要存得住
        assert_eq!(clamp_scrollback(1), SCROLLBACK_MIN);
        assert_eq!(clamp_scrollback(10_000_000), SCROLLBACK_MAX);
        db.set_setting(SettingKey::Scrollback.as_str(), "abc")
            .unwrap();
        assert_eq!(load_terminal(&db).unwrap().scrollback, DEFAULT_SCROLLBACK);
    }
}
