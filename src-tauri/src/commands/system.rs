//! 系统信息卡片（PR-1）：SSH / Local 会话一次采齐，卡片与会话生命周期同寿。
//!
//! 采集脚本与 AI 侧 `get_session_info` / `probe_session_env` **共用**同一份常量
//! [`crate::ai::tools::SESSION_PROBE`]（Win 走 `SESSION_PROBE_WIN`），脚本一份、执行两处
//! 各按需触发：AI 首问走短超时预探测填 `AppState::ai_env`；卡片按需触发填 `AppState::sys_info`。
//! 新加的 hostname/uptime/cpu/mem/disk/ip 只由 SystemInfo 消费，[`crate::ai::context::SessionEnv`]
//! 不解析这 9 个键，AI 侧「不泄露主机标识」的原约束保持不变。
//!
//! 无 exec 通道的会话（FTP/FTPS/Telnet/串口/Mosh）直接返 [`AppError::Invalid`]，前端按能力表
//! 过滤已隐去入口，这里是最后一道兜底。

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::ai::exec::exec_command;
use crate::ai::tools::{SESSION_PROBE, SESSION_PROBE_WIN};
use crate::error::AppError;
use crate::session::SessionKind;
use crate::state::AppState;

/// 单次采集耗时上限：与 `get_session_info` 同（10s）。远端机器负载高时留够余量。
const PROBE_TIMEOUT_MS: u64 = 10_000;

/// 系统信息卡片视图（一次采齐；UI 侧按需渲染，字段缺位显 "—"）。
///
/// `probed_at_ms` 是采集时刻（unix 毫秒），前端算"多久前"展示；单位与其它字段一致。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    // 与 SessionEnv 交集（AI 侧也用）
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub arch: Option<String>,
    pub distro: Option<String>,
    pub shell: Option<String>,
    pub cwd: Option<String>,
    // 卡片专属：AI 上下文不采（主机标识 / 资源汇总）
    pub hostname: Option<String>,
    pub uptime_sec: Option<u64>,
    pub cpu_model: Option<String>,
    pub cpu_cores: Option<u32>,
    pub mem_total_kb: Option<u64>,
    pub mem_avail_kb: Option<u64>,
    pub disk_total_kb: Option<u64>,
    pub disk_used_kb: Option<u64>,
    pub ip: Option<String>,
    /// 采集时间戳（unix 毫秒）。
    pub probed_at_ms: u64,
}

/// 解析探测回显。行式 `key=value`，未识别的键忽略，空值跳过。
///
/// 与 [`crate::ai::context::SessionEnv::parse`] 同源约定（同一脚本、同一分隔符）；差异只在于
/// 白名单加了 9 项卡片字段并做了数值型转换，其它保持"宽容解析"——脚本某行执行失败时那一行
/// 输出为空，这里跳过即可，绝不阻断整个采集。
pub fn parse_probe(raw: &str) -> SystemInfo {
    let mut info = SystemInfo::default();
    for line in raw.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        match k.trim() {
            "os" => info.os = Some(v.to_string()),
            "kernel" => info.kernel = Some(v.to_string()),
            "arch" => info.arch = Some(v.to_string()),
            "distro" => info.distro = Some(v.to_string()),
            "shell" => info.shell = Some(v.to_string()),
            "cwd" => info.cwd = Some(v.to_string()),
            "hostname" => info.hostname = Some(v.to_string()),
            "uptime_sec" => info.uptime_sec = v.parse().ok(),
            "cpu_model" => info.cpu_model = Some(v.to_string()),
            "cpu_cores" => info.cpu_cores = v.parse().ok(),
            "mem_total_kb" => info.mem_total_kb = v.parse().ok(),
            "mem_avail_kb" => info.mem_avail_kb = v.parse().ok(),
            "disk_total_kb" => info.disk_total_kb = v.parse().ok(),
            "disk_used_kb" => info.disk_used_kb = v.parse().ok(),
            "ip" => info.ip = Some(v.to_string()),
            // root/sudo/pkg/init 等 AI 侧键卡片不用，忽略。
            _ => continue,
        }
    }
    info.probed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    info
}

/// `system_info(app, session_id, refresh)`：
/// - `refresh != Some(true)` 且缓存命中 → 直接回缓存（会话内点多少次都不重复跑 exec）
/// - `refresh == Some(true)` 或 miss → 跑探测、写缓存
/// 无 exec 通道的 kind 直接 [`AppError::Invalid`]；`probe_session_env` 保持独立路径不受影响。
#[tauri::command]
pub async fn system_info(
    app: AppHandle,
    session_id: String,
    refresh: Option<bool>,
) -> Result<SystemInfo, AppError> {
    let kind = app
        .state::<AppState>()
        .sessions
        .kind(&session_id)
        .ok_or_else(|| AppError::NoSuchSession(session_id.clone()))?;
    if !matches!(kind, SessionKind::Ssh | SessionKind::Local) {
        return Err(AppError::Invalid(
            "该会话类型无 exec 通道，不支持系统信息采集".into(),
        ));
    }
    if refresh != Some(true) {
        if let Some(hit) = app
            .state::<AppState>()
            .sys_info
            .lock()
            .unwrap()
            .get(&session_id)
        {
            return Ok(hit.clone());
        }
    }
    let script = match kind {
        SessionKind::Local if cfg!(windows) => SESSION_PROBE_WIN,
        _ => SESSION_PROBE,
    };
    let r = exec_command(&app, &session_id, script, PROBE_TIMEOUT_MS).await?;
    let info = parse_probe(r.stdout.trim());
    app.state::<AppState>()
        .sys_info
        .lock()
        .unwrap()
        .insert(session_id, info.clone());
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_probe_output_into_typed_fields() {
        let raw = "os=Linux\nkernel=6.1.0\narch=x86_64\ndistro=Debian 12\nshell=/bin/bash\ncwd=/root\n\
                   root=yes
sudo=no
pkg=apt-get
init=systemd
\
                   hostname=srv-01
uptime_sec=86400
cpu_model=Intel(R) Xeon(R) Platinum 8369HB
cpu_cores=8
\
                   mem_total_kb=16384000
mem_avail_kb=8192000
disk_total_kb=52428800
disk_used_kb=26214400
\
                   ip=10.0.0.5";
        let info = parse_probe(raw);
        assert_eq!(info.os.as_deref(), Some("Linux"));
        assert_eq!(info.hostname.as_deref(), Some("srv-01"));
        assert_eq!(info.uptime_sec, Some(86_400));
        assert_eq!(info.cpu_cores, Some(8));
        assert_eq!(info.mem_total_kb, Some(16_384_000));
        assert_eq!(info.disk_used_kb, Some(26_214_400));
        assert_eq!(info.ip.as_deref(), Some("10.0.0.5"));
        // AI 侧字段被忽略（不进 SystemInfo）
        assert!(info.cpu_model.as_deref().unwrap().starts_with("Intel"));
        assert!(info.probed_at_ms > 0);
    }

    #[test]
    fn skips_empty_values_and_unknown_keys_without_error() {
        // 脚本某行执行失败会留空值（如 hostname= ），或非白名单键（root=）——都不该 panic。
        let raw = "os=Linux\nhostname=\nroot=yes\nsome_future_key=xyz\n";
        let info = parse_probe(raw);
        assert_eq!(info.os.as_deref(), Some("Linux"));
        assert!(info.hostname.is_none(), "空值应跳过");
    }

    #[test]
    fn numeric_parse_failure_leaves_field_none() {
        // 某些发行版 awk 输出可能带单位或异常串，parse 失败应保持 None 而非 panic。
        let raw = "cpu_cores=eight\nmem_total_kb=not-a-number\n";
        let info = parse_probe(raw);
        assert!(info.cpu_cores.is_none());
        assert!(info.mem_total_kb.is_none());
    }

    #[test]
    fn empty_input_yields_default_shell_with_now_timestamp() {
        let info = parse_probe("");
        assert!(info.os.is_none());
        assert!(info.ip.is_none());
        // 时间戳仍要落，避免 UI 显「1970」
        assert!(info.probed_at_ms > 0);
    }
}
