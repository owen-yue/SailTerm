//! 全局托管状态：本地存储 + 会话注册表 + 主机密钥/FTPS 证书裁决表 + SSH kbd 作答表
//! + SFTP 通道缓存 + 传输队列 + AI 命令审批表 + SSH 隧道运行时表。
//!
//! 由 `lib.rs` 的 `setup` 一次性构建并 `manage`；各字段内部自带并发保护，
//! 命令层只借不存，**绝不跨 `await` 持锁**。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::db::Db;
use crate::error::AppError;
use crate::files::sftp::Registry;
use crate::files::transfer::TransferManager;
use crate::plugin::PluginRegistry;
use crate::session::SessionManager;
use crate::ssh::host_key::Pending;
use crate::ssh::tunnel::TunnelManager;
use crate::vault::Vault;

/// 数据库文件名（位于应用数据目录）。
const DB_FILE: &str = "sailterm.db";

/// AI 在途轮次状态（[`AppState`] 的 `ai_cancel`/`ai_turn`/`ai_trace`）的**复合键**。
///
/// chat 与 Agent 是同一条连接（同一 `session_id`）上的两个独立线程：两轨可同时各挂一轮编排，
/// 故这三张表不能只按 `session_id` 单槽存（否则后起的一轨会覆盖前一条的取消端/轮次号/轨迹）。
/// 键取 `{track}\u{1}{session_id}`（`track` ∈ {"agent","chat"}，用不可打印分隔符防撞）。
#[inline]
pub fn ai_turn_key(track: &str, session_id: &str) -> String {
    format!("{track}\u{1}{session_id}")
}

/// 某会话两轨的全部键（连接销毁/断线时按会话一并终止两轨在途轮，见 [`crate::session::abort_ai_turn`]）。
#[inline]
pub fn ai_turn_keys_for_session(session_id: &str) -> [String; 2] {
    [ai_turn_key("agent", session_id), ai_turn_key("chat", session_id)]
}

pub struct AppState {
    /// 本次启动真正生效的数据目录（可能是系统默认，或经 `data_location` 迁移后的自定义目录）；
    /// 供「关于」页展示与迁移校验用。
    pub data_dir: PathBuf,
    /// SQLite：连接档案 / 分组 / known_hosts / 命令历史。
    pub db: Db,
    /// 凭证保险库：OS keyring 优先，AES-GCM 文件回退。
    pub vault: Vault,
    /// 会话注册表：`session_id → 会话`。
    pub sessions: SessionManager,
    /// 首次连接待用户裁决的主机密钥请求。
    pub host_keys: Pending,
    /// FTPS 首次连接待用户裁决的证书请求。
    ///
    /// 与主机密钥是**两张独立的表**（同一套 `request_id → oneshot` 机制，故直接复用其实现：
    /// 一个 `request_id` 只可能被一个协议侧消费，混用会互相吞掉应答）。
    pub ftps_certs: Pending,
    /// SSH keyboard-interactive 认证：待用户作答的提示轮（`request_id → 答案应答通道`）。
    /// 独立成表（同 ftps_certs 的理由）；超时/放弃的残留在下次登记时回收。
    pub ssh_kbd: crate::ssh::kbd::KbdPending,
    /// AI 命令执行前待用户裁决的审批请求（与主机密钥用各自独立的表）。
    pub ai_approvals: crate::ai::approval::Pending,
    /// AI 澄清反问（`ask_user`）待用户作答的请求（与命令审批各自独立的表）。
    pub ai_questions: crate::ai::question::PendingQuestion,
    /// 进行中的编排轮次轨迹（`turn_key → 已 emit 的步骤`，见 [`ai_turn_key`]）：`run_turn` 开始时登记、
    /// 结束时取走随 assistant 消息归档，使回看历史能重现思考过程。按 (会话,轨) 分槽 → chat/agent 并发轮次互不串轨迹。仅进程内短暂持有，不跨 `await` 持锁。
    pub ai_trace: Mutex<HashMap<String, Vec<crate::ai::orchestrator::AiStep>>>,
    /// 在途 AI 编排轮次的取消发送端（`turn_key → oneshot::Sender<()>`，见 [`ai_turn_key`]）：
    /// `ai_send_message` 进入网络段前按轨登记，`ai_cancel_turn` 取出该轨的发送端并 `send(())` 触发 select
    /// 丢弃该轨 `run_turn` future（另一轨不受牵连）；轮次结束（正常/取消）即移除，绝不跨 `await` 持锁。
    pub ai_cancel: Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    /// 当前在途轮次的序号（`turn_key → turn`，见 [`ai_turn_key`]）：`ai_send_message` 进入网络段前按轨写入前端传来的
    /// 本轮 `turn`，`emit_step`/流式事件按该轨键读它回带 `turn` 字段，供前端丢弃「已终止轮次的迟到事件」
    /// 并定位归属轨道（chat/agent 各持一个在途 turn，互不覆盖）；轮次结束（正常/取消）即移除。
    pub ai_turn: Mutex<HashMap<String, u64>>,
    /// 首问懒预探测的目标机环境缓存（`session_id → SessionEnv`）：命中即喂进系统提示词的
    /// 【目标环境】块，省掉模型为摸环境浪费的工具往返。**每会话只探一次**（无论成败都登记，
    /// 避免慢/断会话每轮重复阻塞探测）；会话断开（teardown）随连接失效，重连是新 `session_id` 自然重探。
    pub ai_env: Mutex<HashMap<String, crate::ai::context::SessionEnv>>,
    /// 系统信息卡片缓存（`session_id → SystemInfo`，见 [`crate::commands::system`]）：
    /// 首次点开 ℹ️ 卡片时按需采集，同一会话内不重跑；用户手动「刷新」或点重置按钮传 `refresh=true` 强重采。
    /// 与 `ai_env` 各自独立的表——执行同、消费异：AI 侧要 root/sudo/pkg/init，卡片要 hostname/cpu/mem/disk/ip。
    /// 会话断开随 `teardown` 清理，绝不残留到重连后的新 `session_id`。
    pub sys_info: Mutex<HashMap<String, crate::commands::system::SystemInfo>>,
    /// 监控采样的远端系统分流缓存（`session_id → uname -s 原始串`，见 commands::monitor）：
    /// 每会话只探一次（无论成败/不支持都登记），探测串按系统族选择；会话断开随 `teardown` 失效。
    pub monitor_os: Mutex<HashMap<String, String>>,
    /// `session_id → SFTP 子系统通道`；连接一断即失效。
    pub sftp: Registry,
    /// 传输队列：活动任务表 + 并发闸门。
    pub transfers: TransferManager,
    /// 运行中的 SSH 隧道（本地端口转发）注册表。
    pub tunnels: TunnelManager,
    /// 插件注册表：manifest + wasm + 按 id 隔离配置。
    pub plugins: PluginRegistry,
    /// uteke 语义记忆（进程内）；打开失败则为 None，回落规则记忆。
    pub uteke: Option<crate::ai::uteke_mem::UtekeStore>,
    /// 在途端口扫描的取消标志（工具模块 `commands::net` 专用）：
    /// `net_port_scan` 开扫前清零、逐端口探测前检查；`net_scan_cancel` 置位即中止。
    /// 用 AtomicBool 而非会话表：扫描是短生命周期单例任务，不碰 db/session。
    pub scan_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// 密钥生成器（工具 keygen）短暂持有的「已生成、待保存」密钥对：`token → (私钥PEM, 公钥OpenSSH)`。
    /// 私钥只在后端进程内存中短暂停留，供「保存私钥到文件」写盘，**绝不经 IPC 回前端**；
    /// 不落保险库、不入 db（生成器与密钥库解耦，见 commands::ssh_key 的 ssh_keygen_*）。
    pub keygen: Mutex<HashMap<String, (zeroize::Zeroizing<String>, String)>>,
    /// 锁屏（应用级）全局态单源：锁标记 / PIN 就绪缓存 / 空闲阈值 / 活动心跳 / 试错退避。
    /// 见 [`crate::lock`]：多窗口各有独立 JS 上下文，锁态只能在这里，前端只跟随事件渲染遮罩。
    pub lock: crate::lock::LockState,
}

/// 旧 bundle identifier 对应的数据目录名（`com.sailterm.app`）。
/// identifier 改为 `com.sailterm.desktop` 后 `app_data_dir` 换了路径，
/// 启动时若新目录尚无数据库，从旧目录一次性迁入，避免配置「消失」。
const LEGACY_DATA_DIR_NAME: &str = "com.sailterm.app";

fn migrate_legacy_data(data_dir: &Path) {
    let db_path = data_dir.join(DB_FILE);
    // 已有像样的库就别动
    if db_path.metadata().map(|m| m.len() > 4096).unwrap_or(false) {
        return;
    }
    let Some(parent) = data_dir.parent() else {
        return;
    };
    let legacy = parent.join(LEGACY_DATA_DIR_NAME);
    if !legacy.is_dir() || legacy == data_dir {
        return;
    }
    let legacy_db = legacy.join(DB_FILE);
    if !legacy_db
        .metadata()
        .map(|m| m.len() > 4096)
        .unwrap_or(false)
    {
        return;
    }
    for name in [
        DB_FILE,
        "sailterm.db-shm",
        "sailterm.db-wal",
        "plugins",
        "recordings",
    ] {
        let from = legacy.join(name);
        let to = data_dir.join(name);
        if !from.exists() || to.exists() {
            continue;
        }
        let _ = if from.is_dir() {
            copy_dir(&from, &to)
        } else {
            std::fs::copy(&from, &to).map(|_| ())
        };
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let t = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &t)?;
        } else {
            std::fs::copy(e.path(), t)?;
        }
    }
    Ok(())
}

impl AppState {
    /// 打开（必要时创建）数据目录下的数据库与保险库。
    pub fn open(data_dir: &Path) -> Result<Self, AppError> {
        std::fs::create_dir_all(data_dir)?;
        migrate_legacy_data(data_dir);
        let db = Db::open(&data_dir.join(DB_FILE))?;
        let vault = Vault::open(data_dir);
        // 锁屏态装载：空闲阈值从 settings 表读，PIN 就绪从保险库探测（都只在启动时装一次，
        // 后续由 commands::lock 的各命令同步维护）；读失败落默认值（锁未启用，不阻断启动）。
        let idle_minutes = db
            .get_setting(crate::commands::settings::SettingKey::LockIdleMinutes.as_str())
            .ok()
            .flatten()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let pin_set = vault
            .contains(&crate::vault::keys::lock_pin())
            .unwrap_or(false);
        // 密保问答就绪 = 答案哈希条目存在（问题单独存在不算，避免只删答案没删问题的脏态）。
        let secq_set = vault
            .contains(&crate::vault::keys::lock_secq_answer())
            .unwrap_or(false);
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            db,
            vault,
            sessions: SessionManager::default(),
            host_keys: Pending::default(),
            ftps_certs: Pending::default(),
            ssh_kbd: crate::ssh::kbd::KbdPending::default(),
            ai_approvals: crate::ai::approval::Pending::default(),
            ai_questions: crate::ai::question::PendingQuestion::default(),
            ai_trace: Mutex::new(HashMap::new()),
            ai_cancel: Mutex::new(HashMap::new()),
            ai_turn: Mutex::new(HashMap::new()),
            ai_env: Mutex::new(HashMap::new()),
            sys_info: Mutex::new(HashMap::new()),
            monitor_os: Mutex::new(HashMap::new()),
            sftp: Registry::default(),
            transfers: TransferManager::default(),
            tunnels: TunnelManager::default(),
            plugins: PluginRegistry::open(data_dir)?,
            uteke: crate::ai::uteke_mem::UtekeStore::open(data_dir).ok(),
            scan_cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            keygen: Mutex::new(HashMap::new()),
            lock: crate::lock::LockState::new(idle_minutes, pin_set, secq_set),
        })
    }
}
