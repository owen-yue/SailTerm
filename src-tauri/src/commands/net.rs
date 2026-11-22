//! 网络小工具（工具模块的后端能力，由工具独立窗口调用）：开放端口列表 / TCP 端口扫描 / Ping。
//!
//! **可分离纪律**（与 `src/tools/` 前端模块配套，见 doc 讨论）：本文件自包含，
//! 只依赖 `error`/`platform`/`commands::blocking` 这类系统级设施，**不碰 db / vault / session**；
//! 将来把工具能力摘出主程序 = 拷走本文件 + `platform.rs` + lib.rs 四行命令注册 + 删 state.rs 的
//! `scan_cancel` 一个字段，主代码零牵连。
//!
//! 安全边界：
//! - 三个命令都是**用户手动操作的内置工具**，不经 AI 调用，故不接入 `ai/policy` 审批网关；
//!   若将来要让 AI 使用网络能力，必须经能力网关接入（出网红线），不得直调本模块。
//! - 扫描只收 IPv4 字面量（不做 DNS 解析，防顺带放大）、并发 ≤256（洪水防护靠限并发，
//!   不限端口数，支持全端口 1-65535 扫描）、可经 `net_scan_cancel` 中止；列表只报端口与进程名，不含命令行等敏感信息。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::Emitter;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command as TokioCommand;

use super::blocking;
use crate::error::AppError;
use crate::platform;
use crate::state::AppState;

/// 一条本机监听/绑定端口记录。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ListeningPort {
    /// `tcp` | `udp`。
    pub proto: &'static str,
    pub local_addr: String,
    pub local_port: u16,
    /// 进程 ID（取不到为 0）。
    pub pid: u32,
    /// 进程名（取不到为空串）。
    pub process: String,
}

/// 一个被探测为开放的端口。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OpenPort {
    pub port: u16,
    /// TCP connect 成功即 `open`（防火墙过滤态无法区分，不造假）。
    pub state: &'static str,
    /// 建连耗时（毫秒）。
    pub latency_ms: u32,
}

/// 扫描进度事件负载（事件名 `tools-scan-progress`，200ms 节流）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgress {
    pub scanned: u32,
    pub total: u32,
    pub open: u32,
}

/// 单个 ping 应答包（后端结构化解析，前端本地化渲染，杜绝系统命令输出乱码）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PingReply {
    pub seq: u32,
    pub ip: String,
    /// 应答字节数；系统输出为中文时取不到（0 表示未知，前端隐藏）。
    pub bytes: u32,
    pub ttl: u32,
    pub time_ms: f64,
    /// 耗时是否形如 `<1ms`（Windows 常见）。
    pub time_less: bool,
}

/// ping 统计；解析失败时数值字段为 0，`ok` 退化为 exit-status 判定。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PingStats {
    pub ok: bool,
    pub sent: i64,
    pub received: i64,
    pub loss_pct: f64,
    pub min_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
    /// 结构化应答包列表（前端据此逐包实时渲染，避免系统输出乱码）。
    pub packets: Vec<PingReply>,
    /// 无应答时的失败原因 code（`permission` / `unknown_host`）；有应答或常规不可达为 `None`。
    /// 前端据此把误导性的「不可达」细化成可操作提示（如 macOS 本地网络权限未放行）。
    pub reason: Option<String>,
    /// 无应答时系统 `ping` 的原始输出（stderr 优先，回落 stdout 非应答行）；`reason` 未命中时的展示回退。
    pub detail: Option<String>,
}

/// 一条 DNS 解析记录（`net_dns_lookup`：系统解析器返回的每个地址一条）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DnsRecord {
    /// `A`（IPv4）| `AAAA`（IPv6）。
    pub kind: &'static str,
    /// 解析出的 IP 字面量。
    pub address: String,
    /// 该地址随附的服务端口（`to_socket_addrs` 按查询端口返回，通常为回显入参端口，前端不展示）。
    pub port: u16,
}

/// 一跳 traceroute 结果（`net_traceroute`，逐跳经 `tools-trace-hop` 事件流式推送）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TraceHop {
    /// 跳序号（从 1 起）。
    pub hop: u32,
    /// 该跳 IP；超时/取不到为空串（前端显示 `* * *`）。
    pub ip: String,
    /// 主机名（系统输出带括号名时有值，否则空串）。
    pub host: String,
    /// 各次探测往返时延（毫秒）；解析不到的次省略。
    pub times_ms: Vec<f64>,
}

/// 本机公网出口信息（`net_public_ip`，经公共回显服务查询）。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PublicInfo {
    /// 公网 IP（查询失败为空串）。
    pub ip: String,
    /// 国家/地区（尽力而为，取不到空串）。
    pub country: String,
    /// 城市（尽力而为）。
    pub city: String,
    /// ISP / 组织（尽力而为）。
    pub org: String,
}

/// 并发闸门与限量（防止扫描打成洪水请求；端口上限即 TCP 全空间，洪水防护由并发闸门兜底）。
const SCAN_CONCURRENCY: usize = 256;
const SCAN_MAX_PORTS: usize = 65535;

// =====================================================================
// 开放端口列表
// =====================================================================

/// 本机监听端口采集：编译期选定平台策略（见 [`PortLister`]）负责「取数」，
/// 本命令只跑统一后处理流水线（排序 + 去重）——职责分离，各平台可独立演进、单测。
#[tauri::command]
pub async fn net_listening_ports() -> Result<Vec<ListeningPort>, AppError> {
    blocking(move || {
        let ports = system_port_lister().collect()?;
        Ok(post_process(ports))
    })
    .await
}

// ---------------------------------------------------------------------
// 监听端口采集策略（Strategy）：每平台一个实现，net_listening_ports 统一后处理。
// ---------------------------------------------------------------------

/// 监听端口采集策略：把「跑哪些系统命令 + 如何解析」封装为平台无关契约，
/// 上层只对结果做统一后处理（排序/去重）。新增平台 = 新增一个实现 + 工厂分支，
/// 不动命令入口与既有平台逻辑（开闭原则）。
trait PortLister {
    /// 采集并解析本机监听端口；采集工具不可用时返回 Err（区别于「真的没有端口」的空成功）。
    fn collect(&self) -> Result<Vec<ListeningPort>, AppError>;
}

/// 编译期工厂：按目标平台选出具体策略。各实现是零尺寸单元结构，静态分派、无装箱。
#[cfg(windows)]
fn system_port_lister() -> impl PortLister {
    WindowsPortLister
}
#[cfg(target_os = "macos")]
fn system_port_lister() -> impl PortLister {
    MacosPortLister
}
#[cfg(all(unix, not(target_os = "macos")))]
fn system_port_lister() -> impl PortLister {
    LinuxPortLister
}

/// 统一后处理流水线（模板方法的固定骨架）：先排序使去重相邻，再按四元组去重
/// （ipv4/ipv6 同端口双栈监听 local_addr 不同，不会被合并——按地址语义保留）。
fn post_process(mut ports: Vec<ListeningPort>) -> Vec<ListeningPort> {
    ports.sort_by(|a, b| {
        (a.proto, a.local_port, &a.local_addr).cmp(&(b.proto, b.local_port, &b.local_addr))
    });
    ports.dedup_by(|a, b| {
        a.proto == b.proto
            && a.local_port == b.local_port
            && a.local_addr == b.local_addr
            && a.pid == b.pid
    });
    ports
}

/// Windows：`netstat -ano` 取端口/PID，`tasklist` 补进程名。
#[cfg(windows)]
struct WindowsPortLister;

#[cfg(windows)]
impl PortLister for WindowsPortLister {
    fn collect(&self) -> Result<Vec<ListeningPort>, AppError> {
        let mut out = Vec::new();
        let mut helper_missing = false;
        let mut names = HashMap::new();
        // tasklist 失败不致命：进程名列留空
        if let Ok(text) = run_cmd("tasklist", &["/fo", "csv", "/nh"]) {
            names = parse_tasklist(&text);
        }
        for args in [["-ano", "-p", "tcp"], ["-ano", "-p", "udp"]] {
            match run_cmd("netstat", &args) {
                Ok(text) => out.extend(parse_netstat_windows(&text, &names)),
                Err(_) => helper_missing = true,
            }
        }
        if helper_missing {
            return Err(AppError::Internal(
                "netstat 不可用，无法获取端口列表".into(),
            ));
        }
        Ok(out)
    }
}

/// Linux：优先 `ss -tunlp`，缺失时退化 `netstat -tunlp`（两者列形态接近，共用解析）。
#[cfg(all(unix, not(target_os = "macos")))]
struct LinuxPortLister;

#[cfg(all(unix, not(target_os = "macos")))]
impl PortLister for LinuxPortLister {
    fn collect(&self) -> Result<Vec<ListeningPort>, AppError> {
        match run_cmd("ss", &["-tunlp"]) {
            Ok(text) => Ok(parse_ss(&text)),
            Err(_) => match run_cmd("netstat", &["-tunlp"]) {
                Ok(text) => Ok(parse_ss(&text)),
                Err(_) => Err(AppError::Internal(
                    "netstat/ss 均不可用，无法获取端口列表".into(),
                )),
            },
        }
    }
}

/// macOS：无 `ss`，BSD `netstat` 又不认 `-tunlp`、列格式迥异（旧实现误用之 → 静默返回空 →
/// “未发现监听端口”假象）。改用 `lsof` 机器可读字段模式 `-F pcn` 采集：逐行单字母前缀 +
/// 值，进程名含空格也不会错位；TCP 与 UDP 各跑一次，协议由来源命令确定，无需解析协议列。
#[cfg(target_os = "macos")]
struct MacosPortLister;

#[cfg(target_os = "macos")]
impl PortLister for MacosPortLister {
    fn collect(&self) -> Result<Vec<ListeningPort>, AppError> {
        // lsof「无匹配进程」以退出码 1 + 空输出表达，run_cmd 已归一为空串 → 解析出空表，
        // 属合法「无监听端口」；仅 spawn 失败（lsof 缺失）才 Err，二者语义天然分开。
        let tcp = run_cmd("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-F", "pcn"]);
        let udp = run_cmd("lsof", &["-nP", "-iUDP", "-F", "pcn"]);
        match (tcp, udp) {
            (Err(_), Err(_)) => Err(AppError::Internal("lsof 不可用，无法获取端口列表".into())),
            (tcp, udp) => {
                let mut out = Vec::new();
                if let Ok(text) = tcp {
                    out.extend(parse_lsof(&text, "tcp"));
                }
                if let Ok(text) = udp {
                    out.extend(parse_lsof(&text, "udp"));
                }
                Ok(out)
            }
        }
    }
}

/// 解析 `lsof -F pcn` 输出（macOS）：`p<pid>` 设当前进程、`c<命令>` 设当前进程名，
/// `n<地址>` 为一条网络套接字。协议由调用方按来源命令传入（-iTCP / -iUDP）；
/// 未绑定具体端口的 `*:*` 由 split_host_port 自然丢弃。
#[cfg(target_os = "macos")]
pub fn parse_lsof(text: &str, proto: &'static str) -> Vec<ListeningPort> {
    let mut out = Vec::new();
    let mut pid = 0u32;
    let mut command = String::new();
    for line in text.lines() {
        let mut chars = line.chars();
        let (Some(tag), rest) = (chars.next(), chars.as_str()) else {
            continue;
        };
        match tag {
            'p' => pid = rest.parse().unwrap_or(0),
            'c' => command = rest.to_string(),
            'n' => {
                if let Some((addr, port)) = split_host_port(rest) {
                    out.push(ListeningPort {
                        proto,
                        local_addr: addr,
                        local_port: port,
                        pid,
                        process: command.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// 解析 `netstat -ano -p tcp|udp`（Windows）。
/// 行形态：`  TCP  0.0.0.0:22  0.0.0.0:0  LISTENING  1234`；UDP 无状态列。
/// 只报 TCP LISTENING 与全部 UDP（无连接语义）。
// 仅 Windows 采集分支与单测调用；非 Windows 下为死代码（解析逻辑与平台无关，保留供跨平台单测）。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_netstat_windows(text: &str, names: &HashMap<u32, String>) -> Vec<ListeningPort> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let proto = match f[0].to_ascii_lowercase().as_str() {
            "tcp" => "tcp",
            "udp" => "udp",
            _ => continue,
        };
        if proto == "tcp" && !line.to_ascii_uppercase().contains("LISTENING") {
            continue;
        }
        let Some((addr, port)) = split_host_port(f[1]) else {
            continue;
        };
        let pid = f.last().and_then(|s| s.parse().ok()).unwrap_or(0);
        out.push(ListeningPort {
            proto,
            local_addr: addr,
            local_port: port,
            pid,
            process: names.get(&pid).cloned().unwrap_or_default(),
        });
    }
    out
}

/// 解析 `tasklist /fo csv /nh`：`"imgname.exe","1234","Console","1","12,345 K"`。
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_tasklist(text: &str) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let cols: Vec<&str> = line
            .split(',')
            .map(|c| c.trim().trim_matches('"'))
            .collect();
        if cols.len() >= 2 {
            if let Ok(pid) = cols[1].parse::<u32>() {
                map.insert(pid, cols[0].to_string());
            }
        }
    }
    map
}

/// 解析 `ss -tunlp` / `netstat -tunlp`（Unix 两者列形态接近，共用）。
/// `tcp LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:(("sshd",pid=1,fd=3))`。
// 仅 Linux 采集分支与单测调用；macOS 走 lsof、Windows 走 netstat，两者下为死代码，故按平台抑制。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_ss(text: &str) -> Vec<ListeningPort> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 5 {
            continue;
        }
        let proto = match f[0] {
            "tcp" | "tcp6" => "tcp",
            "udp" | "udp6" => "udp",
            _ => continue,
        };
        if proto == "tcp" && f[1] != "LISTEN" && f[1] != "LISTENING" {
            continue;
        }
        let Some((addr, port)) = split_host_port(f[4]) else {
            continue;
        };
        out.push(ListeningPort {
            proto,
            local_addr: addr,
            local_port: port,
            pid: extract_field(line, "pid=").parse().unwrap_or(0),
            process: extract_quoted(line),
        });
    }
    out
}

/// 取 `key=` 后的数字串（`pid=123,` → `123`）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn extract_field(line: &str, key: &str) -> String {
    let Some(rest) = line.split(key).nth(1) else {
        return String::new();
    };
    rest.chars().take_while(|c| c.is_ascii_digit()).collect()
}

/// 取 `users:("name"` 中的进程名（第一个）。
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn extract_quoted(line: &str) -> String {
    let Some(rest) = line.split("(\"").nth(1) else {
        return String::new();
    };
    rest.chars().take_while(|c| *c != '"').collect()
}

/// `0.0.0.0:22` / `[::]:22` / `127.0.0.1%lo:53` → (addr, port)。按最后一个冒号切分。
fn split_host_port(s: &str) -> Option<(String, u16)> {
    let idx = s.rfind(':')?;
    let (host, port) = (&s[..idx], &s[idx + 1..]);
    let port: u16 = port.parse().ok()?;
    let host = host.trim_matches(|c| c == '[' || c == ']').to_string();
    Some((if host.is_empty() { "*".into() } else { host }, port))
}

// =====================================================================
// 端口扫描
// =====================================================================

/// 校验并展开端口区间 `"20-1024,3306,8000-8080"`（去重升序，最多 TCP 全空间 65535 个）。
pub fn parse_port_range(spec: &str) -> Result<Vec<u16>, AppError> {
    let mut ports = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('-') {
            Some((a, b)) => {
                let bad = || AppError::Invalid(format!("端口区间不合法：{part}"));
                let start: u32 = a.trim().parse().map_err(|_| bad())?;
                let end: u32 = b.trim().parse().map_err(|_| bad())?;
                let (lo, hi) = if start <= end {
                    (start, end)
                } else {
                    (end, start)
                };
                if lo == 0 || hi > 65535 {
                    return Err(AppError::Invalid(format!("端口区间越界：{part}")));
                }
                ports.extend(lo..=hi);
            }
            None => {
                let p: u32 = part
                    .parse()
                    .map_err(|_| AppError::Invalid(format!("端口不合法：{part}")))?;
                if p == 0 || p > 65535 {
                    return Err(AppError::Invalid(format!("端口不合法：{part}")));
                }
                ports.push(p);
            }
        }
    }
    ports.sort_unstable();
    ports.dedup();
    if ports.is_empty() || ports.len() > SCAN_MAX_PORTS {
        return Err(AppError::Invalid(format!(
            "端口数量需在 1–{SCAN_MAX_PORTS} 之间（当前 {}）",
            ports.len()
        )));
    }
    Ok(ports.into_iter().map(|p| p as u16).collect())
}

/// 目标必须是 IPv4 字面量（不解析主机名：避免扫描顺带变成 DNS 放大器）。
fn validate_ipv4(target: &str) -> Result<Ipv4Addr, AppError> {
    let target = target.trim();
    if target.is_empty() || target.len() > 64 {
        return Err(AppError::Invalid("目标地址为空或过长".into()));
    }
    match target.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => Ok(v4),
        _ => Err(AppError::Invalid(format!(
            "目标需为 IPv4 地址（如 192.168.1.1）：{target}"
        ))),
    }
}

/// TCP connect 端口扫描：并发 ≤256、逐端口超时；进度走 `tools-scan-progress` 事件；
/// 在途扫描可经 `net_scan_cancel` 置原子标志中止（`state.scan_cancel`）。
#[tauri::command]
pub async fn net_port_scan(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    target: String,
    port_range: String,
    timeout_ms: u64,
) -> Result<Vec<OpenPort>, AppError> {
    let ip = validate_ipv4(&target)?;
    let ports = parse_port_range(&port_range)?;
    let timeout = Duration::from_millis(timeout_ms.clamp(50, 3000));
    state
        .scan_cancel
        .store(false, std::sync::atomic::Ordering::Relaxed);

    use std::sync::atomic::{AtomicU32, Ordering};
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(SCAN_CONCURRENCY));
    let scanned = std::sync::Arc::new(AtomicU32::new(0));
    let open_cnt = std::sync::Arc::new(AtomicU32::new(0));
    let total = ports.len() as u32;
    let cancel = state.scan_cancel.clone();

    let mut set = tokio::task::JoinSet::new();
    for port in ports {
        let sem = sem.clone();
        let scanned = scanned.clone();
        let open_cnt = open_cnt.clone();
        let app = app.clone();
        let cancel = cancel.clone();
        set.spawn(async move {
            let Ok(_permit) = sem.acquire().await else {
                return Err(AppError::Canceled);
            };
            if cancel.load(Ordering::Relaxed) {
                return Err(AppError::Canceled);
            }
            let addr = SocketAddr::new(IpAddr::V4(ip), port);
            let started = Instant::now();
            let hit = tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr))
                .await
                .map(|r| r.is_ok())
                .unwrap_or(false);
            let n = scanned.fetch_add(1, Ordering::Relaxed) + 1;
            // 200ms 节流推进度；收尾那次必发
            if n % 10 == 0 || n == total {
                let _ = app.emit(
                    "tools-scan-progress",
                    ScanProgress {
                        scanned: n,
                        total,
                        open: open_cnt.load(Ordering::Relaxed),
                    },
                );
            }
            if hit {
                let ms = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
                open_cnt.fetch_add(1, Ordering::Relaxed);
                return Ok(Some(OpenPort {
                    port,
                    state: "open",
                    latency_ms: ms,
                }));
            }
            Ok(None)
        });
    }

    // 取消语义：置标志后在途任务逐个报 Canceled；驱动侧收编全部任务再退出，
    // 不靠 abort 硬杀（避免进度事件在命令返回后继续飞）。被 panic 拖垮的任务视同未开放。
    let mut found = Vec::new();
    let mut canceled = false;
    while let Some(r) = set.join_next().await {
        match r {
            Ok(Ok(Some(p))) => found.push(p),
            Ok(Ok(None)) => {} // 端口未开放
            Ok(Err(e)) => {
                // AppError 不实现 PartialEq：按错误码判定取消，其余错误只记标志
                if e.code() == "E_CANCELED" {
                    canceled = true;
                }
            }
            Err(e) if e.is_cancelled() => canceled = true,
            Err(_) => {} // 被 panic 拖垮的任务视同未开放
        }
    }
    if canceled {
        return Err(AppError::Canceled);
    }
    found.sort_by_key(|p| p.port);
    Ok(found)
}

/// 请求中止在途端口扫描（置原子标志；无在途扫描时幂等无副作用）。
#[tauri::command]
pub async fn net_scan_cancel(state: tauri::State<'_, AppState>) -> Result<(), AppError> {
    state
        .scan_cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// 一键打开 macOS「隐私与安全性 → 本地网络」设置面板。
///
/// 供 Ping 检测到权限拦截（`reason == "permission"`）时的跳转按钮调用。走后端原生 `open`
/// 系统 URL scheme，与 `open_data_dir` 同源做法（绕开 opener 插件对自定义 scheme 的前端 scope 限制）。
/// 该面板为 macOS 特有，非 mac 平台返回错误（前端仅在命中 permission 归类时展示此按钮）。
#[tauri::command]
pub async fn net_open_local_network_settings() -> Result<(), AppError> {
    blocking(|| -> Result<(), AppError> {
        if cfg!(target_os = "macos") {
            std::process::Command::new("open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_LocalNetwork")
                .spawn()
                .map(|_| ())
                .map_err(|e| AppError::Io(format!("打开系统设置失败：{e}")))
        } else {
            Err(AppError::Invalid("本地网络权限面板仅 macOS 提供".into()))
        }
    })
    .await
}

// =====================================================================
// Ping
// =====================================================================

/// 跨平台调用系统 `ping`（Windows `-n`，Unix `-c` + `-W`），**逐包流式**推送 `tools-ping-reply` 事件，
/// 命令结束时返回汇总统计。不引入 ping crate：ICMP 原始套接字在 Windows 需特殊权限，系统命令最稳。
/// 乱码治理：系统 ping 输出随控制台代码页（中文 Windows 为 GBK），直传会乱码——故只按
/// ASCII 令牌（`TTL=` / `ms` / 数字）结构化解析出应答包，回显文本由前端本地化渲染。
#[tauri::command]
pub async fn net_ping(
    app: tauri::AppHandle,
    host: String,
    count: Option<u32>,
) -> Result<PingStats, AppError> {
    let host = host.trim().to_string();
    if host.is_empty() || host.len() > 253 {
        return Err(AppError::Invalid("主机地址为空或过长".into()));
    }
    // 只放行主机名/IPv4 合法字符（防命令注入面，虽然 args 数组已隔离）
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
    {
        return Err(AppError::Invalid(format!("主机地址含非法字符：{host}")));
    }
    let n = count.unwrap_or(4).clamp(1, 10);
    // 平台 argv / 进程创建标志的差异收敛到 platform 模块（含 macOS 不加 `-W` 的坑，见其文档），
    // 本处只保留平台无关的编排。
    let args = platform::ping_args(n, &host);
    let mut cmd = TokioCommand::new("ping");
    // stderr 一并接管：ping 被 macOS「本地网络」隐私权限拦截时，真实原因（sendto: Operation not
    // permitted）只在 stderr——若丢弃就只剩误导性的「不可达 / 100% 丢包」，运维无从判断是权限没给
    // 还是网络真断了。ping 的 stderr 只有寥寥数行、不会填满管道缓冲，故读完 stdout 再排空无死锁风险。
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    platform::suppress_console_window(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::Io(format!("启动 ping 失败：{e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Internal("无法读取 ping 输出".into()))?;
    let mut reader = BufReader::new(stdout);
    let mut packets: Vec<PingReply> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    // 无应答时用于回显根因的原始行（macOS 有时把 sendto 错误打到 stdout 而非 stderr）。
    let mut diag: Vec<String> = Vec::new();
    // ping 自身汇总行/RTT 行的权威统计（逐包行可能因 macOS "out of wait time" 不打印，故以汇总行为准）。
    let mut summary: Option<(i64, i64, f64)> = None;
    let mut rtt: Option<(f64, f64, f64)> = None;
    // 逐行读原始字节（GBK 等非 UTF-8 输出用 lossy 解码，数字/ASCII 令牌不受影响），
    // 每解析出一个应答包即 emit，前端实时上屏。
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return Err(AppError::Io(e.to_string())),
        }
        let line = String::from_utf8_lossy(&buf).replace('\r', "");
        match parse_ping_reply(&line, packets.len() as u32 + 1) {
            Some(r) => {
                let _ = app.emit("tools-ping-reply", r.clone());
                packets.push(r);
            }
            // 非应答行：先尝试吸收为汇总/RTT 统计行（权威、不进 diag），其余（PING 头/Request
            // timeout/sendto 报错等）才留作诊断，仅在最终无应答时回显根因。
            None => {
                if let Some(s) = parse_ping_summary(&line) {
                    summary = Some(s);
                } else if let Some(r) = parse_ping_rtt(&line) {
                    rtt = Some(r);
                } else {
                    let t = line.trim();
                    if !t.is_empty() {
                        diag.push(t.to_string());
                    }
                }
            }
        }
    }
    // 排空 stderr（少量行，安全），无应答时据其内容归类失败原因。
    let mut err_buf: Vec<u8> = Vec::new();
    if let Some(stderr) = child.stderr.as_mut() {
        let _ = stderr.read_to_end(&mut err_buf).await;
    }
    let status = child.wait().await.ok();
    let stderr_text = String::from_utf8_lossy(&err_buf).trim().to_string();
    let mut stats = build_ping_stats(n as i64, packets);
    // 用 ping 自身的汇总/RTT 行校正：逐包应答行可能因时序（macOS "out of wait time"）未被逐行
    // 解析，但汇总行始终权威——据此判定可达，避免把「其实通了」误报成「不可达」。
    if let Some((s, r, loss)) = summary {
        stats.sent = s;
        stats.received = r;
        stats.loss_pct = loss;
        stats.ok = r > 0;
    }
    if let Some((mn, av, mx)) = rtt {
        stats.min_ms = mn;
        stats.avg_ms = av;
        stats.max_ms = mx;
    }
    if stats.received == 0 {
        // 归类命中的高频原因用友好文案；未命中则把系统 ping 真实输出（stderr 优先，回落 stdout 非应答行）
        // 原样透给前端作为回退展示，避免「不可达」把权限/解析等根因一并抹平（截断防超长）。
        let combined = format!("{stderr_text}\n{}", diag.join("\n"));
        stats.reason = classify_ping_failure(&combined, &host);
        let detail = if !stderr_text.is_empty() {
            stderr_text
        } else if !diag.is_empty() {
            diag.join("\n")
        } else {
            // 连输出都没有（ping 被系统硬拦截/静默丢弃）：至少回显退出码，便于判断根因。
            match status.and_then(|s| s.code()) {
                Some(c) => format!("（ping 无输出，退出码 {c}：可能被系统拦截或静默丢包）"),
                None => "（ping 无输出）".to_string(),
            }
        };
        stats.detail = Some(detail.chars().take(300).collect());
    }
    Ok(stats)
}

/// 由应答包列表算汇总统计（丢包按请求数 - 应答数，RTT 取各包耗时；`<1ms` 记 0）。
fn build_ping_stats(sent: i64, packets: Vec<PingReply>) -> PingStats {
    let received = packets.len() as i64;
    let mut s = PingStats {
        ok: received > 0,
        sent,
        received,
        packets,
        ..Default::default()
    };
    if received > 0 {
        let times: Vec<f64> = s
            .packets
            .iter()
            .map(|r| if r.time_less { 0.0 } else { r.time_ms })
            .collect();
        s.min_ms = times.iter().cloned().fold(f64::INFINITY, f64::min);
        s.max_ms = times.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        s.avg_ms = times.iter().sum::<f64>() / times.len() as f64;
    }
    if sent > 0 {
        s.loss_pct = ((sent - received) as f64 / sent as f64) * 100.0;
    }
    s
}

/// 无应答时按 ping 的输出文本 + 目标 host 归类失败原因（返回给前端的稳定 code，前端本地化成文案）。
///
/// 只识别几类**非网络本身**的高频原因，其余（真丢包 / 防火墙静默丢弃 / 超时）返回 `None`，
/// 前端仍显示常规「不可达」：
/// - `permission`：被 macOS「本地网络」隐私权限拦截（显式 `not permitted`，或 macOS 对私有网段的
///   `no route to host`——被拒时系统让局域网地址对该 app 表现为「不可达」，而同一台机器的「终端」能 ping 通）；
/// - `unknown_host`：主机名解析失败（`cannot resolve` / `No address` / `nodename nor servname` 等）。
fn classify_ping_failure(stderr: &str, host: &str) -> Option<String> {
    let low = stderr.to_ascii_lowercase();
    if low.contains("not permitted")
        || low.contains("permission denied")
        || low.contains("not authorized")
    {
        return Some("permission".into());
    }
    if low.contains("cannot resolve")
        || low.contains("no address")
        || low.contains("nodename nor servname")
        || low.contains("name or service not known")
        || low.contains("unknown host")
    {
        return Some("unknown_host".into());
    }
    // macOS「本地网络」隐私门：被拒时对该 app 访问局域网地址的发包返回 EHOSTUNREACH
    // （"no route to host"）而非 EPERM。仅对私有网段 + macOS 判定为权限，避免把真实的
    // 公网「主机不可达」误报成权限问题。
    if cfg!(target_os = "macos") && low.contains("no route to host") && is_private_addr(host) {
        return Some("permission".into());
    }
    None
}

/// host 是否为私有/链路本地地址（IPv4：10/8、172.16/12、192.168/16、169.254/16；IPv6：fc00::/7 等）。
/// 非 IP 字面量（主机名）返回 false。
fn is_private_addr(host: &str) -> bool {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_private(),
        // IPv6 的 `is_private` 在本工具链仍不稳（feature gate）：手动判 ULA fc00::/7。
        Ok(IpAddr::V6(v6)) => v6.segments()[0] & 0xfe00 == 0xfc00,
        Err(_) => false,
    }
}

/// 从一行 ping 回显解析单个应答包（locale 无关：只依赖 ASCII 数字与 `TTL=` / `ms` 令牌）。
/// 非应答行（无 `TTL=`）返回 None。seq 缺省用传入序号（Windows 回显不含 icmp_seq）。
fn parse_ping_reply(line: &str, fallback_seq: u32) -> Option<PingReply> {
    let up = line.to_ascii_uppercase();
    if !up.contains("TTL=") {
        return None;
    }
    let ip = extract_ipv4(line).unwrap_or_default();
    let ttl = after_eq_digits(&up, "TTL=").unwrap_or(0) as u32;
    // bytes 在不同平台/语系排布不同，依次尝试：
    //   Windows 英文 `bytes=32` → `BYTES=`；Unix/macOS `64 bytes from` → 数字在 `BYTES` 之前；
    //   非英文 Windows（GBK 经 lossy 后 `字节=` 变乱码）→ 首个 `=NN`。取不到记 0（仅影响回显字节数，不影响可达判定）。
    let bytes = after_eq_digits(&up, "BYTES=")
        .or_else(|| num_before(&up, "BYTES").map(|v| v as u64))
        .or_else(|| first_eq_digits(&up))
        .unwrap_or(0) as u32;
    let seq = after_eq_digits(&up, "SEQ=")
        .map(|v| v as u32)
        .unwrap_or(fallback_seq);
    let (time_ms, time_less) = extract_time_ms(&up);
    Some(PingReply {
        seq,
        ip,
        bytes,
        ttl,
        time_ms,
        time_less,
    })
}

/// 解析 ping 汇总统计行（权威计数，locale 无关）：
/// `4 packets transmitted, 4 packets received, 0.0% packet loss`（macOS 可能再缀 `, N packets out of wait time`）。
/// 返回 `(sent, received, loss_pct)`；非汇总行返回 None。（Windows 汇总行格式不同，不匹配则呷为逐包计数。）
fn parse_ping_summary(line: &str) -> Option<(i64, i64, f64)> {
    let low = line.to_ascii_lowercase();
    if !low.contains("packets received") || !low.contains("packet loss") {
        return None;
    }
    let sent = num_before(&low, "packets transmitted")? as i64;
    let received = num_before(&low, "packets received")? as i64;
    let loss = num_before(&low, "% packet loss").or_else(|| num_before(&low, "%"))?;
    Some((sent, received, loss))
}

/// 解析往返时延汇总行：`round-trip min/avg/max/stddev = 5.344/6.827/10.697/2.245 ms`。
/// 返回 `(min, avg, max)`；非该行返回 None。
fn parse_ping_rtt(line: &str) -> Option<(f64, f64, f64)> {
    let low = line.to_ascii_lowercase();
    if !low.contains("round-trip") {
        return None;
    }
    let eq = low.find('=')?;
    let nums: Vec<f64> = low[eq + 1..]
        .split([' ', '/'])
        .filter_map(|t| t.trim().parse::<f64>().ok())
        .collect();
    if nums.len() >= 3 {
        Some((nums[0], nums[1], nums[2]))
    } else {
        None
    }
}

/// 取 `needle` 之前紧邻的数字（跳过空白，支持小数）。用于从 "4 packets received" 取 4、
/// "0.0% packet loss" 取 0.0。找不到或前面无数字返回 None。
fn num_before(hay: &str, needle: &str) -> Option<f64> {
    let idx = hay.find(needle)?;
    let b = hay.as_bytes();
    let mut i = idx;
    while i > 0 && (b[i - 1] as char).is_ascii_whitespace() {
        i -= 1;
    }
    let end = i;
    while i > 0 && ((b[i - 1] as char).is_ascii_digit() || b[i - 1] == b'.') {
        i -= 1;
    }
    if i == end {
        return None;
    }
    hay[i..end].parse::<f64>().ok()
}

/// 取 `KEY=` 之后紧跟的整数（KEY 需为大写形式）。
fn after_eq_digits(up: &str, key: &str) -> Option<u64> {
    let idx = up.find(key)? + key.len();
    let digits: String = up[idx..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse::<u64>().ok()
}

/// 取行内第一个紧跟数字的 `=` 之后的整数。仅作为非英文 Windows 回显（`字节=` 被 lossy
/// 解码成乱码、无法按 `BYTES=` 定位）时提字节数的兵道；Unix/英文回显不会走到这里。
fn first_eq_digits(up: &str) -> Option<u64> {
    let b = up.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'=' {
            let digits: String = up[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                return digits.parse::<u64>().ok();
            }
        }
    }
    None
}

/// 从大写行里抽往返时延：定位 `MS`，回退收集数字/小数点，前一个字符为 `<` 则标记「小于」。
fn extract_time_ms(up: &str) -> (f64, bool) {
    let b = up.as_bytes();
    let ms = match up.rfind("MS") {
        Some(p) => p,
        None => return (0.0, false),
    };
    let mut i = ms;
    while i > 0 && (b[i - 1] as char).is_ascii_whitespace() {
        i -= 1;
    }
    let end = i;
    while i > 0 && ((b[i - 1] as char).is_ascii_digit() || b[i - 1] == b'.') {
        i -= 1;
    }
    let num = &up[i..end];
    let less = i > 0 && b[i - 1] == b'<';
    (num.parse::<f64>().unwrap_or(0.0), less)
}

/// 抽第一个 IPv4 字面量（四段 0-255）。
fn extract_ipv4(line: &str) -> Option<String> {
    let b = line.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut groups = 0usize;
        let mut ok = true;
        while groups < 4 {
            let ns = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i == ns || i - ns > 3 || line[ns..i].parse::<u32>().unwrap_or(999) > 255 {
                ok = false;
                break;
            }
            groups += 1;
            if groups < 4 {
                if i < b.len() && b[i] == b'.' {
                    i += 1;
                } else {
                    ok = false;
                    break;
                }
            }
        }
        if ok && groups == 4 {
            return Some(line[start..i].to_string());
        }
        if i == start {
            i += 1;
        }
    }
    None
}

// =====================================================================
// 网络诊断：DNS 解析 / Traceroute / 公网 IP
// =====================================================================

/// 从用户输入中提取纯主机名：容忍粘贴完整 URL（`https://host:port/path`）。
/// 依次剔掉：协议、路径/查询/锚点、userinfo（`user@`）、端口（IPv4/域名后）、IPv6 方括号、行末点。
fn normalize_host(raw: &str) -> String {
    let mut s = raw.trim();
    if let Some(idx) = s.find("://") {
        s = &s[idx + 3..];
    }
    s = s.split(['/', '?', '#']).next().unwrap_or("");
    if let Some(at) = s.rfind('@') {
        s = &s[at + 1..];
    }
    // 去端口：方括号形式 `[ipv6]:port` 取括号内；否则恰好一个冒号视为 host:port。
    if s.starts_with('[') {
        s = match s.find(']') {
            Some(close) => &s[1..close],
            None => &s[1..],
        };
    } else if s.matches(':').count() == 1 {
        s = s.split(':').next().unwrap_or(s);
    }
    s.trim_end_matches('.').to_string()
}

/// 校验主机名/域名合法性（字母数字 + `.-_:`，长度 1–253）；先归一化 URL，返回主机名。
fn validate_host(host: &str) -> Result<String, AppError> {
    let h = normalize_host(host);
    if h.is_empty() || h.len() > 253 {
        return Err(AppError::Invalid(
            "主机名/域名不能为空，长度不能超过 253 字符".into(),
        ));
    }
    if !h
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
    {
        return Err(AppError::Invalid(format!("主机名/域名含非法字符：{h}")));
    }
    Ok(h)
}

/// DNS 解析：走系统解析器（`std::net::ToSocketAddrs`），返回解析出的 A/AAAA 地址列表。
/// 不引 trust-dns：系统解析器已覆盖 hosts/缓存/DHCDNS，行为与 `ping 域名` 一致、跨平台最稳。
#[tauri::command]
pub async fn net_dns_lookup(domain: String) -> Result<Vec<DnsRecord>, AppError> {
    let host = validate_host(&domain)?;
    blocking(move || {
        use std::net::ToSocketAddrs;
        // 端口 0：只关心地址，端口无意义（解析器对任意端口返回同一批地址）。
        let addrs = (host.as_str(), 0u16)
            .to_socket_addrs()
            .map_err(|e| AppError::Io(format!("DNS 解析失败：{e}")))?;
        let mut out: Vec<DnsRecord> = addrs
            .map(|a| DnsRecord {
                kind: if a.ip().is_ipv4() { "A" } else { "AAAA" },
                address: a.ip().to_string(),
                port: a.port(),
            })
            .collect();
        // A 在前、AAAA 在后，各自按地址字符串稳定排序，便于前端展示。
        out.sort_by(|a, b| a.kind.cmp(b.kind).then_with(|| a.address.cmp(&b.address)));
        out.dedup_by(|a, b| a.address == b.address && a.kind == b.kind);
        Ok(out)
    })
    .await
}

/// Traceroute：跨平台调系统 `tracert`(Win)/`traceroute`(Unix)，逐跳解析后经
/// `tools-trace-hop` 事件流式上屏，命令结束返回全部跳。同 ping—不自己实现 ICMP。
#[tauri::command]
pub async fn net_traceroute(
    app: tauri::AppHandle,
    host: String,
    max_hops: Option<u32>,
) -> Result<Vec<TraceHop>, AppError> {
    let host = validate_host(&host)?;
    let max = max_hops.unwrap_or(20).clamp(1, 30);
    // 可执行名 + argv + 进程创建标志的平台差异收敛到 platform 模块，本处保持平台无关。
    let (program, args) = platform::traceroute_cmd(max, &host);
    let mut cmd = TokioCommand::new(program);
    cmd.args(&args).stdout(Stdio::piped()).stderr(Stdio::null());
    platform::suppress_console_window(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::Io(format!("启动 traceroute 失败：{e}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Internal("无法读取 traceroute 输出".into()))?;
    let mut reader = BufReader::new(stdout);
    let mut hops: Vec<TraceHop> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return Err(AppError::Io(e.to_string())),
        }
        let line = String::from_utf8_lossy(&buf).replace('\r', "");
        if let Some(h) = parse_trace_hop(&line, hops.len() as u32 + 1) {
            let _ = app.emit("tools-trace-hop", h.clone());
            hops.push(h);
        }
    }
    let _ = child.wait().await;
    Ok(hops)
}

/// 从一行 traceroute 回显解析一跳（locale 无关：只依赖行首跳号 + IPv4 + `ms` 前数字）。
/// 非跳行（如表头、`over the maximum`）返回 None。超时行（无 IP）也返回一条 ip 为空的记录。
fn parse_trace_hop(line: &str, _fallback_hop: u32) -> Option<TraceHop> {
    let trimmed = line.trim_start();
    // 行首必须是整数跳号
    let num: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
    if num.is_empty() {
        return None;
    }
    let hop: u32 = num.parse().ok()?;
    // 去掉跳号后的剩余：提主机名（括号内）与 IPv4
    let rest = trimmed[num.len()..].trim_start();
    // 主机名：Unix `traceroute` 为 `name (ip)`，取 `(` 前最后一个非空 token；Windows `tracert -d` 无括号→留空。
    let ip = extract_ipv4(rest).unwrap_or_default();
    // 全 `*` 行（超时）：无 IP 且剩余只剩 `*`/空白。
    if ip.is_empty() && rest.replace(['*', ' '], "").is_empty() {
        return Some(TraceHop {
            hop,
            ip: String::new(),
            host: String::new(),
            times_ms: vec![],
        });
    }
    let host = match rest.rfind('(') {
        Some(p) => {
            // `split_whitespace` 自带首尾空白忽略，无需先 `trim_end`
            let name = rest[..p].split_whitespace().last().unwrap_or("");
            // 只接受像主机名的串（非 IP、非纯数字/点）
            if !name.is_empty()
                && name != ip
                && !name.chars().all(|c| c.is_ascii_digit() || c == '.')
            {
                name.to_string()
            } else {
                String::new()
            }
        }
        None => String::new(),
    };
    // 各次时延：收集所有形如 `N ms` 的浮点数（Windows `  2 ms`，Unix `2.345 ms`）。
    let times_ms = extract_trace_times(rest);
    Some(TraceHop {
        hop,
        ip,
        host,
        times_ms,
    })
}

/// 从剩余串里抽所有 `N ms` 的数值（大写后逐个定位 `MS`、回退收数字/小数点）。
fn extract_trace_times(rest: &str) -> Vec<f64> {
    let up = rest.to_ascii_uppercase();
    let b = up.as_bytes();
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = up[from..].find("MS") {
        let ms = from + rel;
        // 从 MS 向前跳空白，再回收数字/小数点
        let mut i = ms;
        while i > 0 && (b[i - 1] as char).is_ascii_whitespace() {
            i -= 1;
        }
        let end = i;
        while i > 0 && ((b[i - 1] as char).is_ascii_digit() || b[i - 1] == b'.') {
            i -= 1;
        }
        if let Ok(v) = up[i..end].parse::<f64>() {
            out.push(v);
        }
        from = ms + 2;
    }
    out
}

/// 本机公网出口 IP：先经 ipify 拿纯 IP（主，备 ifconfig.me），再尽力从 ipinfo.io 补地理/ISP。
/// 经 reqwest（ring 全局 provider 已在 lib.rs setup 装好，故 HTTPS 可用）。
#[tauri::command]
pub async fn net_public_ip() -> Result<PublicInfo, AppError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .map_err(|e| AppError::Internal(format!("构建 HTTP 客户端失败：{e}")))?;
    // IP：ipify 主、ifconfig.me 备（均为纯文本回显）。
    let ip_raw = match fetch_text(&client, "https://api.ipify.org").await {
        Some(s) => s,
        None => fetch_text(&client, "https://ifconfig.me/ip")
            .await
            .unwrap_or_default(),
    };
    let ip = ip_raw.trim().to_string();
    if ip.is_empty() {
        return Err(AppError::Io(
            "公网 IP 查询失败（可能离线或回显服务不可达）".into(),
        ));
    }
    // 地理/ISP：尽力从 ipinfo.io 的 JSON 补全，失败不影响 IP 展示。
    let mut info = PublicInfo {
        ip,
        ..Default::default()
    };
    if let Ok(resp) = client.get("https://ipinfo.io/json").send().await {
        if let Ok(json) = resp.json::<serde_json::Value>().await {
            info.country = json
                .get("country")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            info.city = json
                .get("city")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            info.org = json
                .get("org")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
        }
    }
    Ok(info)
}

/// 取纯文本回显（去首尾空白）；任何失败（网络/非 2xx）都返回 None，供多源回退。
async fn fetch_text(client: &reqwest::Client, url: &str) -> Option<String> {
    client
        .get(url)
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()
        .map(|s| s.trim().to_string())
}

// =====================================================================
// 命令执行helper（Windows 隐藏控制台窗口）
// =====================================================================

/// 运行命令并取 UTF-8 输出（stderr 合并进来，解析器按行过滤不受影响）。
fn run_cmd(program: &str, args: &[&str]) -> Result<String, AppError> {
    run_cmd_raw(
        program,
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    )
}

/// 同上，args 为 String 版；失败（启动不了）返回 Err::Io。
fn run_cmd_raw(program: &str, args: &[String]) -> Result<String, AppError> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：别闪黑框
    }
    let out = cmd.output()?;
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&out.stderr).to_string();
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn lsof_macos_field_output() {
        // 复刻 lsof -F pcn 输出：p=pid、c=命令名（可含空格）、n=地址。
        let text = "p997\ncrapportd\nn*:54453\np1371\ncGoogle Chrome\nn127.0.0.1:9222\np2\ncnode\nn[::1]:3000\n";
        let rows = parse_lsof(text, "tcp");
        assert_eq!(rows.len(), 3);
        assert_eq!(
            (rows[0].proto, rows[0].local_port, rows[0].pid),
            ("tcp", 54453, 997)
        );
        // 进程名含空格不错位：整段作为命令名
        assert_eq!(rows[1].process, "Google Chrome");
        // IPv6 方括号地址正确剥壳
        assert_eq!(
            (rows[2].local_addr.as_str(), rows[2].local_port),
            ("::1", 3000)
        );
    }

    #[test]
    fn netstat_windows_listening_only() {
        let text = "
  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:22             0.0.0.0:0              LISTENING       1234
  TCP    1.2.3.4:52100          5.6.7.8:443            ESTABLISHED     1234
  UDP    0.0.0.0:5353           *:*                                    99
";
        let names = HashMap::from([(1234u32, "ssh.exe".to_string()), (99, "mdns".to_string())]);
        let rows = parse_netstat_windows(text, &names);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].local_port, 22);
        assert_eq!(rows[0].process, "ssh.exe");
        assert_eq!(rows[1].proto, "udp");
    }

    #[test]
    fn ss_parses_users_pid_and_name() {
        let text = "Netid State Recv-Q Send-Q Local Address:Port Peer Address:Port Process
tcp LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=811,fd=3))
udp UNCONN 0 0 0.0.0.0:5353 0.0.0.0:* users:((\"systemd-resolve\",pid=500,fd=12))
tcp ESTAB 0 0 10.0.0.2:22 10.0.0.1:5555 -
";
        let rows = parse_ss(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].pid, 811);
        assert_eq!(rows[0].process, "sshd");
        assert_eq!(rows[1].proto, "udp");
    }

    #[test]
    fn host_port_split_v6() {
        // 只剖空主机才写 `*`；`[::]`（IPv6 任意地址）去括号后保留 `::`，与 `0.0.0.0` 保留原样同口径。
        assert_eq!(split_host_port("[::]:22"), Some(("::".into(), 22u16)));
        assert_eq!(split_host_port(":22"), Some(("*".into(), 22u16)));
        assert_eq!(split_host_port("0.0.0.0%lo:53").unwrap().1, 53);
        assert_eq!(split_host_port("bad"), None);
    }

    #[test]
    fn classifies_ping_failure_reasons() {
        // 显式权限错误（任意平台）
        assert_eq!(
            classify_ping_failure("ping: sendto: Operation not permitted\n", "192.168.1.1")
                .as_deref(),
            Some("permission")
        );
        // 主机名解析失败
        assert_eq!(
            classify_ping_failure("ping: cannot resolve foo: Unknown host", "foo").as_deref(),
            Some("unknown_host")
        );
        // 真实不可达（无 stderr，只有 stdout 超时行）→ None，前端仍显「不可达」
        assert_eq!(classify_ping_failure("", "8.8.8.8").as_deref(), None);
        // 私有网段判定
        assert!(is_private_addr("192.168.31.183"));
        assert!(is_private_addr("10.0.0.5"));
        assert!(!is_private_addr("8.8.8.8"));
        assert!(!is_private_addr("localhost"));
        // macOS「本地网络」门：对局域网地址的 no route to host 视为权限拦截；公网地址则不误判
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                classify_ping_failure("ping: sendto: No route to host", "192.168.31.183")
                    .as_deref(),
                Some("permission")
            );
            assert_eq!(
                classify_ping_failure("ping: sendto: No route to host", "8.8.8.8").as_deref(),
                None
            );
        }
    }

    #[test]
    fn parses_ping_summary_and_rtt() {
        // 标准 Unix/BSD 汇总行
        assert_eq!(
            parse_ping_summary("4 packets transmitted, 4 packets received, 0.0% packet loss"),
            Some((4, 4, 0.0))
        );
        // macOS「out of wait time」变体：逐包行可能缺失，汇总行仍权威
        assert_eq!(
            parse_ping_summary("4 packets transmitted, 4 packets received, 0.0% packet loss, 4 packets out of wait time"),
            Some((4, 4, 0.0))
        );
        // 有丢包
        assert_eq!(
            parse_ping_summary("4 packets transmitted, 2 packets received, 50.0% packet loss"),
            Some((4, 2, 50.0))
        );
        // 非汇总行（应答行/PING 头）→ None，不误吃
        assert_eq!(
            parse_ping_summary("64 bytes from 1.2.3.4: icmp_seq=0 ttl=64 time=1 ms"),
            None
        );
        assert_eq!(
            parse_ping_summary("PING 1.2.3.4 (1.2.3.4): 56 data bytes"),
            None
        );

        // RTT 行：取 min/avg/max（忽略 stddev）
        assert_eq!(
            parse_ping_rtt("round-trip min/avg/max/stddev = 5.344/6.827/10.697/2.245 ms"),
            Some((5.344, 6.827, 10.697))
        );
        assert_eq!(
            parse_ping_rtt("4 packets transmitted, 4 packets received, 0.0% packet loss"),
            None
        );
    }

    #[test]
    fn port_range_validated() {
        assert_eq!(
            parse_port_range("80,443,8000-8002").unwrap(),
            vec![80, 443, 8000, 8001, 8002]
        );
        assert!(parse_port_range("").is_err());
        assert!(parse_port_range("0").is_err());
        assert!(parse_port_range("70000").is_err());
        assert!(parse_port_range("1-70000").is_err()); // 越界（区间哨兵在限量之前）
    }

    #[test]
    fn ping_reply_english_unix() {
        let r = parse_ping_reply(
            "64 bytes from 127.0.0.1: icmp_seq=1 ttl=64 time=0.043 ms",
            9,
        )
        .unwrap();
        assert_eq!(r.ip, "127.0.0.1");
        assert_eq!(r.ttl, 64);
        assert_eq!(r.bytes, 64);
        assert_eq!(r.seq, 1);
        assert!((r.time_ms - 0.043).abs() < 1e-6);
        assert!(!r.time_less);
    }

    #[test]
    fn ping_reply_chinese_windows() {
        // 中文 GBK 回显经 lossy 解码后中文变替换符，但 ASCII 令牌（TTL=/32=32/<1ms）完好
        let line = "\u{fffd}\u{fffd}\u{fffd} 127.0.0.1 \u{fffd}\u{fffd}\u{fffd}\u{fffd}: \u{fffd}\u{fffd}=32 \u{fffd}\u{fffd}<1ms TTL=128";
        let r = parse_ping_reply(line, 2).unwrap();
        assert_eq!(r.ip, "127.0.0.1");
        assert_eq!(r.ttl, 128);
        assert_eq!(r.bytes, 32);
        assert_eq!(r.seq, 2); // Windows 回显无 SEQ=，用 fallback 序号
        assert!(r.time_less); // “<1ms”
        assert_eq!(r.time_ms, 1.0);
    }

    #[test]
    fn ping_reply_non_reply_line() {
        assert!(parse_ping_reply("Pinging 127.0.0.1 with 32 bytes of data:", 1).is_none());
        assert!(parse_ping_reply("Request timeout for icmp_seq 0", 1).is_none());
    }

    #[test]
    fn ping_stats_from_packets() {
        let pkts = vec![
            PingReply {
                seq: 1,
                ip: "1.1.1.1".into(),
                bytes: 32,
                ttl: 128,
                time_ms: 2.0,
                time_less: false,
            },
            PingReply {
                seq: 2,
                ip: "1.1.1.1".into(),
                bytes: 32,
                ttl: 128,
                time_ms: 1.0,
                time_less: true,
            },
            PingReply {
                seq: 3,
                ip: "1.1.1.1".into(),
                bytes: 32,
                ttl: 128,
                time_ms: 4.0,
                time_less: false,
            },
        ];
        let s = build_ping_stats(4, pkts);
        assert!(s.ok);
        assert_eq!((s.sent, s.received), (4, 3));
        assert!((s.loss_pct - 25.0).abs() < 1e-6);
        assert_eq!(s.min_ms, 0.0); // <1ms 计 0
        assert_eq!(s.max_ms, 4.0);
        // 与 min/max 同源：time_less 包也按 0 计入均值 → (2.0 + 0.0 + 4.0) / 3 = 2.0
        assert!((s.avg_ms - 2.0).abs() < 1e-6);
    }

    #[test]
    fn trace_hop_windows_and_unix() {
        // Windows tracert：`  1     2 ms     1 ms     1 ms  192.168.1.1`（无主机名，-d）
        let h = parse_trace_hop("   1     2 ms     1 ms     1 ms  192.168.1.1", 1).unwrap();
        assert_eq!(h.hop, 1);
        assert_eq!(h.ip, "192.168.1.1");
        assert_eq!(h.host, "");
        assert_eq!(h.times_ms, vec![2.0, 1.0, 1.0]);
        // Unix traceroute：` 1  gw.local (10.0.0.1)  1.234 ms  1.1 ms  1.0 ms`
        let h = parse_trace_hop(" 1  gw.local (10.0.0.1)  1.234 ms  1.1 ms  1.0 ms", 9).unwrap();
        assert_eq!(h.hop, 1);
        assert_eq!(h.ip, "10.0.0.1");
        assert_eq!(h.host, "gw.local");
        assert_eq!(h.times_ms.len(), 3);
    }

    #[test]
    fn trace_hop_timeout_and_header() {
        // 超时行：只剩 `*`，ip 为空、无时延
        let h = parse_trace_hop("  5  * * *", 5).unwrap();
        assert_eq!(h.hop, 5);
        assert_eq!(h.ip, "");
        assert!(h.times_ms.is_empty());
        // 表头/非跳行：行首无数字 → None
        assert!(parse_trace_hop("Tracing route to example.com ...", 1).is_none());
    }

    #[test]
    fn host_validation_rejects_bad_chars() {
        assert_eq!(validate_host(" example.com. ").unwrap(), "example.com");
        assert!(validate_host("").is_err());
        assert!(validate_host("bad; rm -rf").is_err());
    }

    #[test]
    fn host_normalization_strips_url_parts() {
        // 粘贴完整 URL → 只留主机名
        assert_eq!(
            validate_host("https://www.baidu.com").unwrap(),
            "www.baidu.com"
        );
        assert_eq!(
            validate_host("http://example.com:8080/a/b?x=1#y").unwrap(),
            "example.com"
        );
        assert_eq!(validate_host("user@example.com").unwrap(), "example.com");
        // IPv6 保留（多冒号 + 方括号）
        assert_eq!(validate_host("[::1]:8080").unwrap(), "::1");
        assert_eq!(validate_host("fe80::1").unwrap(), "fe80::1");
    }
}
