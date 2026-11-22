//! 轻量服务器监控采样：经既有 SSH `exec` 通道跑一组**只读**探测命令，
//! 解析出 CPU / 内存 / 磁盘 / 网络四元组，供底部「📈 监控」面板画卡片与曲线。
//!
//! 与 AI `run_command` 同走 [`crate::ai::exec::exec_command`]（独立 exec 通道，不污染用户 PTY、
//! 不二次认证）。探测串全是 `head`/`grep`/`df`/`awk` 只读命令，无需策略审批。
//! 输出解析为纯函数（[`parse_sample`]），可离线单测。
//!
//! 跨系统探测：先跑裸命令 `uname -s`（无 shell 语法，兼容登录 shell 为 csh/tcsh 的会话），
//! 按结果分流 Linux `/proc` 串 / macOS / FreeBSD 系串；分流结果按会话缓存（同 `ai_env` 纪律，
//! 会话断开随 `teardown` 失效）。BSD 串把输出整形回 Linux 同形状（cpu 时间片行 / MemTotal kB 行），
//! 解析器不分支；唯磁盘 IO 累计计数在 BSD 无 shell 可达源，探测显式打 `NA`，前端如实显示不支持。
//!
//! 安全：返回的只是资源数值（百分比/字节数），**不含主机名、IP、进程命令行**。

use tauri::AppHandle;

use super::state;
use crate::ai::exec::exec_command;
use crate::error::AppError;

/// 一次采样的四元组（单位：CPU 为内核 jiffies 累计、内存/磁盘为字节、网卡为累计字节）。
/// 百分比与速率由前端拿相邻两次样本做差——服务端只报事实，避免时钟/调度误差。
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MonitorSample {
    /// CPU 总时间片（user+nice+system+idle+iowait+irq+softirq+steal）。
    pub cpu_total: u64,
    /// CPU 空闲时间片（idle+iowait）。
    pub cpu_idle: u64,
    /// 物理内存总量（字节）。
    pub mem_total: u64,
    /// 可用内存（MemAvailable，字节）。
    pub mem_available: u64,
    /// 所有真实文件系统聚合总量（字节，按设备去重、排除伪文件系统）。
    pub disk_total: u64,
    /// 所有真实文件系统聚合已用（字节）。
    pub disk_used: u64,
    /// 各挂载点明细（供「磁盘分区」列表）；聚合值 = 本列表按设备去重后求和。
    pub disk_partitions: Vec<DiskPartition>,
    /// 网卡累计接收字节（所有非 lo 接口）。
    pub net_rx: u64,
    /// 网卡累计发送字节（所有非 lo 接口）。
    pub net_tx: u64,
    /// 1 分钟负载均值（本地 sysinfo / 远端 /proc/loadavg）。
    pub load1: f64,
    /// 进程数（约）。
    pub procs: u32,
    /// 磁盘累计读字节（本地 sysinfo / 远端 /proc/diskstats）。
    pub disk_read: u64,
    /// 磁盘累计写字节。
    pub disk_write: u64,
    /// 磁盘 IO 累计计数是否可得：BSD/macOS 无 shell 可达的累计字节计数（只有瞬时速率），
    /// 探测打 `NA` → 本字段 false，前端显示「暂不支持」而非假装 0 B/s。
    pub io_supported: bool,
    /// 每核 CPU（user+nice+system, idle+iowait）时间片，用于分核占用。
    pub cpu_cores: Vec<(u64, u64)>,
    /// 来源：`remote` | `local`。
    pub source: String,
}

/// 单个真实文件系统分区：`df` 一行 / sysinfo 一个 Disk 的规整结果。
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiskPartition {
    /// 挂载点，如 `/`、`/mysql/data`。
    pub mount: String,
    /// 设备名，如 `/dev/sda1`、`vg-root`。
    pub device: String,
    /// 容量（字节）。
    pub total: u64,
    /// 已用（字节）。
    pub used: u64,
}

/// 一次采样的输出上限（探测串本身很小，防异常环境刷屏）。
const PROBE_TIMEOUT_MS: u64 = 8_000;

/// 只读探测串（Linux）：分段标记 + 经典 Unix 工具，`df -Pk` 以 1024 字节块逐 fs 单行。
const PROBE_LINUX: &str = concat!(
    "echo '@CPU'; head -n1 /proc/stat; ",
    "echo '@MEM'; grep -E '^(MemTotal|MemAvailable):' /proc/meminfo; ",
    "echo '@DISK'; df -Pk 2>/dev/null; ",
    "echo '@NET'; awk 'NR>2 && $1!=\"lo:\" {rx+=$2; tx+=$10} END{print rx+0, tx+0}' /proc/net/dev; ",
    "echo '@LOAD'; cut -d' ' -f1 /proc/loadavg; echo P $(ps -e --no-headers | wc -l); ",
    "echo '@IO'; awk '/^[ ]*[0-9]+[ ]+[0-9]+[ ]+[0-9a-zA-Z]+/ {r+=$3*512; w+=$7*512} END{print r+0, w+0}' /proc/diskstats; ",
    "echo '@CORES'; grep '^cpu[0-9]' /proc/stat",
);

/// macOS 探测串：输出整形回 Linux 同形状（解析器不分支）。CPU 用 `top -l 1` 的瞬时占用合成
/// 「100 时间片」的 cpu 行（相邻样本 dTotal 恒 0，前端 derive 的回退分支直接用当前值，与本地
/// sysinfo 采样同一口径）；可用内存按 Apple 社区口径 free+inactive+speculative 页折算。
/// 全串只用 `;` 串联 + 单引号 awk，兼容登录 shell 为 csh/tcsh 的会话。
const PROBE_DARWIN: &str = concat!(
    "echo '@CPU'; top -l 1 | awk '/CPU usage/{gsub(/%/,\"\");printf \"cpu %.0f 0 0 %.0f\\n\", 100-$(NF-1), $(NF-1)}'; ",
    "echo '@MEM'; sysctl -n hw.memsize | awk '{printf \"MemTotal: %.0f kB\\n\", $1/1024}'; ",
    "vm_stat | awk '/page size of/{p=$(NF-1)} /^Pages free:/{f=$NF} /^Pages inactive:/{i=$NF} /^Pages speculative:/{s=$NF} END{printf \"MemAvailable: %.0f kB\\n\", (f+i+s)*p/1024}'; ",
    "echo '@DISK'; df -Pk; ",
    "echo '@NET'; netstat -ibn | awk '$3 ~ /^<Link/ && $1 !~ /^lo/ {rx+=$(NF-3); tx+=$(NF-1)} END{print rx+0, tx+0}'; ",
    "echo '@LOAD'; sysctl -n vm.loadavg | tr -d '{}'; ps -ax | awk 'END{print \"P\",NR}'; ",
    "echo '@IO'; echo NA",
);

/// FreeBSD 系（含 DragonFly/OpenBSD/NetBSD 同类 key）探测串：`kern.cp_time` 是真实累计时间片，
/// 直接投影成 cpu 行，差值口径与 Linux 一致；可用内存按 free+inactive 页折算。
const PROBE_FREEBSD: &str = concat!(
    "echo '@CPU'; sysctl -n kern.cp_time | awk '{print \"cpu\",$1,$2,$3,$4,$5}'; ",
    "echo '@MEM'; sysctl -n hw.physmem vm.stats.vm.v_page_size vm.stats.vm.v_free_count vm.stats.vm.v_inactive_count | awk 'NR==1{t=$1} NR==2{p=$1} NR==3{f=$1} NR==4{i=$1} END{printf \"MemTotal: %.0f kB\\nMemAvailable: %.0f kB\\n\", t/1024, (f+i)*p/1024}'; ",
    "echo '@DISK'; df -Pk; ",
    "echo '@NET'; netstat -ibn | awk '$3 ~ /^<Link/ && $1 !~ /^lo/ {rx+=$(NF-3); tx+=$(NF-1)} END{print rx+0, tx+0}'; ",
    "echo '@LOAD'; sysctl -n vm.loadavg | tr -d '{}'; ps -ax | awk 'END{print \"P\",NR}'; ",
    "echo '@IO'; echo NA",
);

/// 远端可监控的系统族（按 `uname -s` 分流）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RemoteOs {
    Linux,
    Darwin,
    Bsd,
}

impl RemoteOs {
    /// `uname -s` 输出 → 系统族；不认识的系统返 `None`（调用侧给可读错误，不盲探）。
    fn from_uname(u: &str) -> Option<Self> {
        match u.trim() {
            "Linux" => Some(Self::Linux),
            "Darwin" => Some(Self::Darwin),
            "FreeBSD" | "DragonFly" | "OpenBSD" | "NetBSD" => Some(Self::Bsd),
            _ => None,
        }
    }

    fn probe(self) -> &'static str {
        match self {
            Self::Linux => PROBE_LINUX,
            Self::Darwin => PROBE_DARWIN,
            Self::Bsd => PROBE_FREEBSD,
        }
    }
}

/// 识别远端系统族：裸命令 `uname -s`（无变量/控制流，任意登录 shell 都能执行），
/// 结果按会话缓存——成败都登记（同 `ai_env` 纪律），不支持的系统也不每次重探。
async fn detect_remote_os(app: &AppHandle, session_id: &str) -> Result<RemoteOs, AppError> {
    let cached = state(app)
        .monitor_os
        .lock()
        .unwrap()
        .get(session_id)
        .cloned();
    let raw = match cached {
        Some(s) => s,
        None => {
            let r = exec_command(app, session_id, "uname -s", PROBE_TIMEOUT_MS).await?;
            r.stdout.trim().to_string()
        }
    };
    match RemoteOs::from_uname(&raw) {
        Some(os) => {
            state(app)
                .monitor_os
                .lock()
                .unwrap()
                .insert(session_id.to_string(), raw);
            Ok(os)
        }
        None => {
            state(app)
                .monitor_os
                .lock()
                .unwrap()
                .insert(session_id.to_string(), raw.clone());
            Err(AppError::Exec(format!(
                "暂不支持监控该系统（{}）",
                if raw.is_empty() { "未知" } else { &raw }
            )))
        }
    }
}

/// 在 SSH 会话上跑一次只读探测并解析（Linux/macOS/FreeBSD 系）。
/// 本地会话走进程内 sysinfo；FTP 等无 exec 通道的会话返回可读错误。
#[tauri::command]
pub async fn monitor_sample(app: AppHandle, session_id: String) -> Result<MonitorSample, AppError> {
    // 本地会话：进程内 sysinfo；不走 SSH
    if state(&app).sessions.kind(&session_id) == Some(crate::session::SessionKind::Local) {
        return local_sample();
    }
    let os = detect_remote_os(&app, &session_id).await?;
    let r = exec_command(&app, &session_id, os.probe(), PROBE_TIMEOUT_MS).await?;
    if r.exit_code != Some(0) && r.stdout.trim().is_empty() {
        return Err(AppError::Exec(format!(
            "监控采样失败：{}",
            r.stderr.trim()
        )));
    }
    parse_sample(&r.stdout)
        .ok_or_else(|| AppError::Exec("该机器缺少可识别的监控数据来源，暂不支持采样".into()))
}

/// 解析探测输出；结构不完整则 `None`。
/// 每段格式：
/// - `@CPU` → `cpu  user nice system idle iowait irq softirq steal ...`
/// - `@MEM` → `MemTotal: N kB` / `MemAvailable: N kB`
/// - `@DISK` → `fs blocks used avail% mount`（`df -B1` 的 used/size）
/// - `@NET` → `rx tx`（字节）
pub fn parse_sample(text: &str) -> Option<MonitorSample> {
    let section = |tag: &str| {
        text.split_once(&format!("@{tag}\n"))
            .map(|(_, rest)| rest)
            .map(|rest| {
                ["@CPU", "@MEM", "@DISK", "@NET", "@LOAD", "@IO", "@CORES"]
                    .iter()
                    .find_map(|t| rest.split_once(&format!("{t}\n")).map(|(a, _)| a))
                    .unwrap_or(rest)
            })
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };

    let (cpu_total, cpu_idle) = parse_cpu(section("CPU")?)?;
    let (mem_total, mem_available) = parse_mem(section("MEM")?)?;
    let (disk_total, disk_used, disk_partitions) = parse_disk(section("DISK")?);
    let (net_rx, net_tx) = parse_net(section("NET")?)?;

    let (disk_read, disk_write) = parse_io(section("IO"));
    // BSD/macOS 探测段只打 `NA`：累计计数不可得，前端据此显示「暂不支持」而非 0 B/s。
    let io_supported = section("IO").map(|s| s.trim() != "NA").unwrap_or(true);
    let cpu_cores = parse_cores(section("CORES"));
    let mut procs = 0u32;
    if let Some(l) = section("LOAD") {
        for line in l.lines() {
            if let Some(rest) = line.strip_prefix("P ") {
                procs = rest.trim().parse().unwrap_or(0);
            }
        }
    }
    Some(MonitorSample {
        cpu_total,
        cpu_idle,
        mem_total,
        mem_available,
        disk_total,
        disk_used,
        disk_partitions,
        net_rx,
        net_tx,
        load1: parse_load(section("LOAD")),
        procs,
        disk_read,
        disk_write,
        io_supported,
        cpu_cores,
        source: "remote".into(),
    })
}

/// `/proc/stat` 首行：`cpu user nice system idle iowait irq softirq steal ...`。
/// 最少需 4 段（user nice system idle）；`iowait` 可缺（下方 `n.get(4)` 已兜底 0），
/// 故不能要求满 5 段——否则某些只给到 idle 的源（或精简采样）会把整行判空、CPU 显示消失。
fn parse_cpu(line: &str) -> Option<(u64, u64)> {
    let mut it = line.split_whitespace();
    if it.next()? != "cpu" {
        return None;
    }
    let n: Vec<u64> = it.filter_map(|s| s.parse().ok()).collect();
    if n.len() < 4 {
        return None;
    }
    let total: u64 = n.iter().sum();
    let idle = n[3] + n.get(4).copied().unwrap_or(0);
    Some((total, idle))
}

/// `/proc/meminfo`：`MemTotal: N kB` 两行（顺序不限）。
fn parse_mem(block: &str) -> Option<(u64, u64)> {
    let mut total = None;
    let mut avail = None;
    for line in block.lines() {
        let (k, v) = line.split_once(':')?;
        let bytes: u64 = v
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|kb| kb * 1024)?;
        match k {
            "MemTotal" => total = Some(bytes),
            "MemAvailable" => avail = Some(bytes),
            _ => {}
        }
    }
    Some((total?, avail?))
}

/// 判断是否伪文件系统 / 无需监控的挂载（按设备名或挂载点前缀）。
/// 排除 tmpfs、devtmpfs、cgroup、proc、sysfs、overlay、squashfs、/dev/loop*（snap），
/// 以及挂在 /proc /sys /dev /run /snap /System/Volumes（macOS APFS 卷内部）下的内核虚拟 fs——
/// 这些不是「磁盘」，纳入会虚报容量或重复计数。
fn is_pseudo_fs(device: &str, mount: &str) -> bool {
    const PSEUDO: &[&str] = &[
        "tmpfs", "devtmpfs", "udev", "none", "shm", "overlay", "squashfs", "sysfs", "proc",
        "procfs", "cgroup", "cgroup2", "securityfs", "pstore", "debugfs", "tracefs", "mqueue",
        "hugetlbfs", "configfs", "fusectl", "efivarfs", "bpf", "autofs", "binfmt_misc", "ramfs",
        "devpts", "fuse.gvfsd-fuse", "fuse.portal", "nsfs", "rpc_pipefs",
    ];
    let dev = device.to_ascii_lowercase();
    if PSEUDO.contains(&dev.as_str()) || dev.starts_with("/dev/loop") {
        return true;
    }
    ["/proc", "/sys", "/dev", "/run", "/snap", "/System/Volumes"]
        .iter()
        .any(|p| mount == *p || mount.starts_with(&format!("{p}/")))
}

/// 解析 `df -Pk` 整段（表头 + 每 fs 一行）→ (聚合 total, 聚合 used, 按设备去重的分区明细)。
/// POSIX `-P` 保证每 fs 单行不折行，`-k` 固定 1024 字节块（各 df 实现通用；避免 `-B1` 被
/// GNU df 误读成 `-B 1P`=1PB 块导致容量塌成 1）。size/used 从右侧数更稳（挂载点可能含空格），
/// 再 ×1024 还原字节。同一设备多挂载点（bind mount）按设备去重只留首个，防聚合翻倍。
fn parse_disk(block: &str) -> (u64, u64, Vec<DiskPartition>) {
    let mut parts: Vec<DiskPartition> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in block.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        // 至少 6 列且倒数第 5 列（1024-块 size）为数字才算一条真 fs（跳过表头 / 异常行）。
        if f.len() < 6 {
            continue;
        }
        let Ok(total_kb) = f[f.len() - 5].parse::<u64>() else { continue };
        let used_kb = f[f.len() - 4].parse::<u64>().unwrap_or(0);
        let mount = f[f.len() - 1].to_string();
        let device = f[0].to_string();
        // APFS：同一物理容器的多个卷（/dev/disk3s1s1、disk3s5…）设备名不同但都报整份
        // 容器容量，按全名去重拦不住、聚合会翻倍——按「第几块盘」归并（与 local_disk
        // 的根卷优先同一口径，见其 APFS 注释）；非 APFS 设备名原样，Linux 同盘多分区不受害。
        let key = apfs_volume_base(&device).to_string();
        if is_pseudo_fs(&device, &mount) || !seen.insert(key) {
            continue;
        }
        parts.push(DiskPartition {
            mount,
            device,
            total: total_kb * 1024,
            used: used_kb * 1024,
        });
    }
    let total = parts.iter().map(|p| p.total).sum();
    let used = parts.iter().map(|p| p.used).sum();
    (total, used, parts)
}

/// APFS 卷设备名归并到容器盘基：`/dev/disk3s1s1` → `/dev/disk3`；其余设备名原样返回。
/// 只认 `/dev/disk<数字>` 形态（macOS 整份容器重复报数的卷族），不碰 `/dev/disk/by-uuid/…`
/// 这类 Linux udev 符号链（disk 后非数字），也不碰 `/dev/sda1` 这类同盘多分区真独立 fs。
fn apfs_volume_base(device: &str) -> &str {
    if let Some(rest) = device.strip_prefix("/dev/disk") {
        let n = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if n > 0 {
            return &device[.."/dev/disk".len() + n];
        }
    }
    device
}

/// `awk` 汇总：`rx tx`。
fn parse_net(line: &str) -> Option<(u64, u64)> {
    let mut it = line.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// 本机采样（Windows / Unix 均可）。
fn local_sample() -> Result<MonitorSample, AppError> {
    use std::time::Duration;
    use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
    let mut sys = System::new();
    sys.refresh_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::everything())
            .with_memory(MemoryRefreshKind::everything()),
    );
    std::thread::sleep(Duration::from_millis(200));
    sys.refresh_cpu_all();
    let cpu_pct = f64::from(sys.global_cpu_usage()).clamp(0.0, 100.0);
    let cpu_total: u64 = 10_000;
    let cpu_idle: u64 = ((100.0 - cpu_pct) * 100.0) as u64;
    let mem_total = sys.total_memory();
    let mem_available = sys.available_memory();
    let load1 = System::load_average().one;
    let procs = sys.processes().len() as u32;
    let (disk_total, disk_used, disk_partitions) = local_disk();
    let (net_rx, net_tx) = local_net();
    let (disk_read, disk_write) = local_disk_io();
    let cpu_cores: Vec<(u64, u64)> = Vec::new();
    Ok(MonitorSample {
        cpu_total,
        cpu_idle,
        mem_total,
        mem_available,
        disk_total,
        disk_used,
        disk_partitions,
        net_rx,
        net_tx,
        load1,
        procs,
        disk_read,
        disk_write,
        io_supported: true,
        cpu_cores,
        source: "local".into(),
    })
}

fn local_disk() -> (u64, u64, Vec<DiskPartition>) {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    // 与远端 df 口径一致：排除伪文件系统，按「设备名 + 容量」去重（macOS APFS 下
    // /、/System/Volumes/Data 等共享同一容器、各自报整份容量，去重防总容量翻数倍；
    // Windows 各盘符为独立设备，天然各成一行）。聚合值 = 去重后各分区求和。
    let mut parts: Vec<DiskPartition> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for d in disks.list() {
        let device = d.name().to_string_lossy().into_owned();
        let mount = d.mount_point().to_string_lossy().into_owned();
        if is_pseudo_fs(&device, &mount) || !seen.insert((device.clone(), d.total_space())) {
            continue;
        }
        let total = d.total_space();
        parts.push(DiskPartition {
            mount,
            device,
            total,
            used: total.saturating_sub(d.available_space()),
        });
    }
    let total = parts.iter().map(|p| p.total).sum();
    let used = parts.iter().map(|p| p.used).sum();
    (total, used, parts)
}

fn local_net() -> (u64, u64) {
    use sysinfo::Networks;
    let nets = Networks::new_with_refreshed_list();
    let mut rx = 0u64;
    let mut tx = 0u64;
    for data in nets.list().values() {
        rx += data.total_received();
        tx += data.total_transmitted();
    }
    (rx, tx)
}

fn parse_load(section: Option<&str>) -> f64 {
    section
        .and_then(|s| s.lines().next())
        .and_then(|l| l.split_whitespace().next())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn parse_io(section: Option<&str>) -> (u64, u64) {
    let Some(s) = section else { return (0, 0) };
    let mut it = s.split_whitespace();
    let r = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let w = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (r, w)
}

/// `/proc/stat` 的 `cpuN` 行 → (busy 片, idle 片)。
fn parse_cores(section: Option<&str>) -> Vec<(u64, u64)> {
    let Some(s) = section else { return Vec::new() };
    s.lines()
        .filter(|l| l.trim_start().starts_with("cpu") && !l.trim_start().starts_with("cpu "))
        .filter_map(parse_cpu)
        .collect()
}

fn local_disk_io() -> (u64, u64) {
    use std::path::Path;
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    // 同 local_disk：APFS 多卷会各自报告同一物理设备的累计读写量，累加会翻数倍。
    // 有根卷时只取根卷（其计数即整盘 IO）；否则按「设备名」去重累加。
    let mut r = 0u64;
    let mut w = 0u64;
    if let Some(root) = disks
        .list()
        .iter()
        .find(|d| d.mount_point() == Path::new("/"))
    {
        let u = root.usage();
        r += u.total_read_bytes;
        w += u.total_written_bytes;
    } else {
        let mut seen = std::collections::HashSet::new();
        for d in disks.list() {
            if !seen.insert(d.name().to_string_lossy().into_owned()) {
                continue;
            }
            let u = d.usage();
            r += u.total_read_bytes;
            w += u.total_written_bytes;
        }
    }
    (r, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
@CPU
cpu  100 0 50 800 50 0 0 0 0 0
@MEM
MemTotal:       16384000 kB
MemAvailable:    8192000 kB
@DISK
Filesystem     1024-blocks      Used Available Capacity Mounted on
/dev/sda1 100000 40000 60000 40% /
/dev/sdb1 500000 100000 400000 20% /data
tmpfs 64000 1000 63000 1% /run
@NET
123456 7890
";

    #[test]
    fn parses_all_sections() {
        let s = parse_sample(FIXTURE).unwrap();
        assert_eq!(s.cpu_total, 100 + 50 + 800 + 50);
        assert_eq!(s.cpu_idle, 800 + 50);
        assert_eq!(s.mem_total, 16_384_000 * 1024);
        assert_eq!(s.mem_available, 8_192_000 * 1024);
        assert_eq!(s.disk_total, (100_000 + 500_000) * 1024); // tmpfs 排除；1K 块 ×1024
        assert_eq!(s.disk_used, (40_000 + 100_000) * 1024);
        assert_eq!(s.disk_partitions.len(), 2);
        assert_eq!(s.disk_partitions[0].mount, "/");
        assert_eq!(s.disk_partitions[1].mount, "/data");
        assert_eq!(s.net_rx, 123_456);
        assert_eq!(s.net_tx, 7_890);
    }

    #[test]
    fn missing_section_is_none() {
        assert!(parse_sample("@CPU\ncpu  1 0 0 1 0\n").is_none());
        assert!(parse_sample("").is_none());
        assert!(parse_sample("@MEM\nMemTotal: 1 kB\n").is_none()); // 缺 MemAvailable
    }

    #[test]
    fn cpu_without_iowait_still_works() {
        let (t, i) = parse_cpu("cpu  10 0 5 85").unwrap();
        assert_eq!(t, 100);
        assert_eq!(i, 85);
    }

    #[test]
    fn disk_aggregates_lists_and_dedups() {
        let block = "/dev/sda1 200 80 120 40% /\n/dev/sdb1 500 100 400 20% /data\n\
                     tmpfs 64 1 63 1% /run\n/dev/sda1 200 80 120 40% /bind";
        let (t, u, parts) = parse_disk(block);
        // tmpfs 排除；/bind 与 / 同为 /dev/sda1（bind mount）按设备去重，只计一次
        assert_eq!((t, u), (700 * 1024, 180 * 1024));
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].mount, "/");
        assert_eq!(parts[1].mount, "/data");
    }

    #[test]
    fn apfs_base_merges_only_disk_volumes() {
        assert_eq!(apfs_volume_base("/dev/disk3s1s1"), "/dev/disk3");
        assert_eq!(apfs_volume_base("/dev/disk3s5"), "/dev/disk3");
        // 另一块物理盘（外接盘）不被误并
        assert_eq!(apfs_volume_base("/dev/disk4s2"), "/dev/disk4");
        // Linux 同盘多分区是真独立 fs，不并
        assert_eq!(apfs_volume_base("/dev/sda1"), "/dev/sda1");
        assert_eq!(apfs_volume_base("/dev/nvme0n1p2"), "/dev/nvme0n1p2");
        // udev 符号链形态（disk 后非数字）原样
        assert_eq!(apfs_volume_base("/dev/disk/by-uuid/ab12"), "/dev/disk/by-uuid/ab12");
    }

    #[test]
    fn apfs_volumes_merge_into_one_container() {
        // 本机实采 df -Pk 的形态：同一容器多卷各自报整份容量，聚合只能计一次
        let block = "Filesystem     1024-blocks      Used Available Capacity  Mounted on\n\
                     /dev/disk3s1s1 482797652 13335948 8974528 60% /\n\
                     devfs 203 203 0 100% /dev\n\
                     /dev/disk3s5 482797652 446073408 8974528 99% /System/Volumes/Data\n\
                     /dev/disk3s3 482797652 1503120 8974528 15% /Volumes/Recovery\n\
                     /dev/disk4s1 976760576 500000000 476760576 52% /Volumes/USB";
        let (t, _u, parts) = parse_disk(block);
        // 根卷保留（首见）；Data 卷被 /System/Volumes 前缀排除；Recovery 卷并入 disk3；
        // 外接盘 disk4 独立保留；devfs 被 /dev 前缀排除。
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].device, "/dev/disk3s1s1");
        assert_eq!(parts[1].device, "/dev/disk4s1");
        assert_eq!(t, (482797652 + 976760576) * 1024);
    }

    #[test]
    fn uname_maps_to_os_family() {
        assert_eq!(RemoteOs::from_uname("Linux"), Some(RemoteOs::Linux));
        assert_eq!(RemoteOs::from_uname("Darwin\n"), Some(RemoteOs::Darwin));
        assert_eq!(RemoteOs::from_uname("FreeBSD"), Some(RemoteOs::Bsd));
        assert_eq!(RemoteOs::from_uname("SunOS"), None);
        assert_eq!(RemoteOs::from_uname(""), None);
    }

    #[test]
    fn darwin_sample_parses_from_real_output() {
        // 下面全部是本机（macOS）实采输出整形后的探测串（top 的 cpu 行为合成值）
        let text = "\
@CPU
cpu 29 0 0 71
@MEM
MemTotal: 16777216 kB
MemAvailable: 5000656 kB
@DISK
Filesystem     1024-blocks      Used Available Capacity  Mounted on
/dev/disk3s1s1   482797652  13335948   8974528    60%    /
devfs                  203       203         0   100%    /dev
/dev/disk3s5     482797652 446073408   8974528    99%    /System/Volumes/Data
/dev/disk3s3     482797652   1503120   8974528    15%    /Volumes/Recovery
@NET
25352713 8915430122
@LOAD
 3.31 2.85 2.55
P 829
@IO
NA
";
        let s = parse_sample(text).unwrap();
        // 合成 cpu 行：busy 29 + idle 71 → 占用 29%（前端拿 dTotal==0 回退用当前值）
        assert_eq!((s.cpu_total, s.cpu_idle), (100, 71));
        assert_eq!(s.mem_total, 16_777_216 * 1024);
        assert_eq!(s.mem_available, 5_000_656 * 1024);
        // APFS 多卷合并：只剩根卷一条（Recovery 同容器，Data 被前缀排除）
        assert_eq!(s.disk_partitions.len(), 1);
        assert_eq!(s.disk_total, 482_797_652 * 1024);
        assert_eq!((s.net_rx, s.net_tx), (25_352_713, 8_915_430_122));
        assert_eq!(s.load1, 3.31);
        assert_eq!(s.procs, 829);
        // BSD 没有累计 IO 计数：显式 NA，不得静默成「支持但为 0」
        assert!(!s.io_supported);
        assert_eq!((s.disk_read, s.disk_write), (0, 0));
    }

    #[test]
    fn freebsd_sample_parses_from_probe_shape() {
        // kern.cp_time 是真实累计时间片；形态出自 sysctl/top/man 文档，未真机采集（FreeBSD 环镜不可得）
        let text = "\
@CPU
cpu 631848 443 1177571 7974315 0
@MEM
MemTotal: 4194304 kB
MemAvailable: 2097152 kB
@DISK
Filesystem 1024-blocks Used Available Capacity Mounted on
/dev/gids/root 20480000 8000000 11468800 41% /
@NET
1000 2000
@LOAD
 0.05 0.10 0.20
P 210
@IO
NA
";
        let s = parse_sample(text).unwrap();
        assert_eq!(s.cpu_total, 631848 + 443 + 1177571 + 7974315);
        assert_eq!(s.cpu_idle, 7974315);
        assert_eq!(s.mem_total, 4194304 * 1024);
        assert_eq!(s.disk_partitions.len(), 1);
        assert_eq!(s.load1, 0.05);
        assert_eq!(s.procs, 210);
        assert!(!s.io_supported);
    }

    #[test]
    fn linux_probe_keeps_io_supported() {
        // 旧 Linux 路径（FIXTURE）无 NA 段：io 可得性必须保持 true，前端不退化
        let s = parse_sample(FIXTURE).unwrap();
        assert!(s.io_supported);
    }
}
