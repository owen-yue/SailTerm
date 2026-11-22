//! 进程列表与结束进程（PR-5 · 图形化进程管理器）。
//!
//! 采样与 [super::monitor] 同走 [crate::ai::exec::exec_command]（独立 exec 通道，不污染用户
//! PTY、不二次认证），并按 [super::system] 的做法**按远端 OS 选探测串**：
//! - Local：进程内 `sysinfo`（跨平台，无需 shell）。
//! - SSH/Linux：`ps aux`（procps-ng / BusyBox 通用，USER PID %CPU %MEM VSZ RSS STAT COMMAND）。
//! - SSH/macOS：`ps -ax -o`（BSD，`etime` 串 + `comm`）。
//! - SSH/Windows OpenSSH：`tasklist /fo csv`（无 uname → 探测回落 Windows 分支）。
//!
//! `process_list` 全是只读命令，与 monitor_sample 一样**不经策略审批**。`process_kill` 是写操作：
//! 前端的确认弹窗即人工审批（决策 D-kill A），后端再做**两道兜底**——① pid≤1 直接拒（系统祖进程）；
//! ② 组好的 kill 串过一遍 [crate::ai::policy::PolicyEngine]，命中 `Blocked`（黑名单）拒绝。
//! 复用同一条策略大脑（与 AI `run_command` 共享），但不牵动 AI-turn 审批卡的路由。
//!
//! 口径：CPU% 取 `ps`/sysinfo 的均值口径（`ps %cpu` 为衰减均值、sysinfo 为相邻刷新差值），
//! 与 top 的瞬时值略有差异，但对「谁在吃资源」的定位够用；解析为纯函数，可离线单测。

use serde::Serialize;
use tauri::AppHandle;

use super::{blocking, state};
use crate::ai::exec::exec_command;
use crate::ai::policy::{Decision, PolicyEngine};
use crate::error::AppError;
use crate::session::SessionKind;

/// 单个进程的视图。`cpu_pct` 在 Windows `tasklist` 通道拿不到时为 `None`（前端显 `—`）。
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProcInfo {
    pub pid: u32,
    pub user: String,
    /// 进程状态原始码（Linux/BSD：R/S/D/Z/T…；Windows tasklist 无此项时留空串）。
    pub state: String,
    pub cpu_pct: Option<f32>,
    /// 常驻内存 RSS（KB）。
    pub mem_kb: u64,
    /// 已运行秒数；拿不到（Windows tasklist / 本地 Windows）时留 0。
    pub elapsed_sec: u64,
    /// 命令/程序名。
    pub cmd: String,
}

/// 采样一次 exec 的耗时上限（进程列表可能很大，留够余量）。
const LIST_TIMEOUT_MS: u64 = 10_000;
/// kill 命令的超时（含 1s 核验等待，比采样上限宽）。
const KILL_TIMEOUT_MS: u64 = 15_000;
/// 远端 OS 探测超时。
const OS_PROBE_TIMEOUT_MS: u64 = 6_000;
/// 默认返回上限（防 IPC 载荷爆炸；按 sort_by 取前 N）。
const DEFAULT_LIMIT: usize = 200;

/// Linux：用 `ps aux`（BSD 风格，procps-ng 与 BusyBox 都支持）——`-eo pid=,...` 那套
/// 自定义列在裁剪版 ps 上会被吐回格式串+孤立 PID（实测某 CentOS），故走最通用的 `aux`。
/// `ww` 放开命令列宽防截断。列 = USER PID %CPU %MEM VSZ RSS TTY STAT START TIME COMMAND。
const PROBE_LINUX: &str = "ps auxww 2>/dev/null";
/// BSD（macOS）：无表头，`etime` 为 `[DD-]HH:MM:SS` 串、`comm` 为整行余部（含参数）。
const PROBE_MACOS: &str = "ps -ax -o pid=,user=,stat=,%cpu=,rss=,etime=,comm= 2>/dev/null";
/// Windows：CSV、无表头（`/nh`），列 = 映像名,PID,会话名,会话#,内存使用。
const PROBE_WINDOWS: &str = "tasklist /fo csv /nh";
/// Linux `/proc` 兜底：当 `ps` 缺失或输出格式异常（`parse_ps_aux` 解析为空）时改走内核级
/// 稳定 ABI。遍历 `/proc/[0-9]*`，每条记录以 `\037`(0x1f, unit-sep) 分隔：
/// `pid \037 uid \037 state \037 rss_kb \037 cmd`。纯 POSIX sh + cat/awk/tr，BusyBox 亦可用；
/// 单次采样拿不到 CPU% → 该列留空；user 为数字 uid（兜底路径不做 passwd 反查）。
const PROBE_PROC_LINUX: &str = r#"for d in /proc/[0-9]*; do
  p=${d##*/}
  st=$(cat "$d/stat" 2>/dev/null) || continue
  rest=${st##*) }
  state=${rest%% *}
  set -- $(awk '/^Uid:/{u=$2}/^VmRSS:/{r=$2}END{print u" "r}' "$d/status" 2>/dev/null)
  cmd=$(tr '\0' ' ' < "$d/cmdline" 2>/dev/null)
  if [ -z "$cmd" ]; then t=${st#*(}; cmd="[${t%%)*}]"; fi
  printf '%s\037%s\037%s\037%s\037%s\n' "$p" "$1" "$state" "${2:-0}" "$cmd"
done"#;

/// 远端 OS 三分法。探测靠 `uname`：POSIX 有、Windows cmd/powershell 无 → 拿不到即 Windows。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Os {
    Linux,
    MacOS,
    Windows,
}

/// 从 `uname -s` 回显判 OS（宽容：空/异常一律回落 Windows，保守走最兼容分支）。
fn classify_os(uname: &str) -> Os {
    let u = uname.trim();
    if u.contains("Linux") {
        Os::Linux
    } else if u.contains("Darwin") {
        Os::MacOS
    } else {
        Os::Windows
    }
}

/// `process_list`：按会话能力分派采样源，服务端排序后截断到 `limit`。
///
/// `sort_by ∈ {"cpu","mem","pid"}`（默认 cpu）；`limit` 默认 200；`filter` 非空时先对**全量**
/// 样本做命令/用户/PID 子串匹配（不区分大小写），再排序截断——保证「搜什么就看到什么」
/// 是权威结果，不受 top-N 截断线的摆动影响（否则 0% CPU 的进程会随边界在列表里时隐时现）。
/// FTP/Telnet/串口/Mosh 无 exec 通道 → [`AppError::Invalid`]（前端已按 `hasCap(kind,"monitor")`
/// 过滤入口，这里最后一道兜底）。
#[tauri::command]
pub async fn process_list(
    app: AppHandle,
    session_id: String,
    sort_by: Option<String>,
    limit: Option<u32>,
    filter: Option<String>,
) -> Result<Vec<ProcInfo>, AppError> {
    let kind = state(&app)
        .sessions
        .kind(&session_id)
        .ok_or_else(|| AppError::NoSuchSession(session_id.clone()))?;
    let mut procs = match kind {
        SessionKind::Local => blocking(local_procs).await?,
        SessionKind::Ssh => {
            let os = remote_os(&app, &session_id).await?;
            let parsed = match os {
                Os::Linux => {
                    let r = exec_command(&app, &session_id, PROBE_LINUX, LIST_TIMEOUT_MS).await?;
                    let v = parse_ps_aux(&r.stdout);
                    if v.is_empty() {
                        // ps 缺失 / 格式异常 → 读内核级稳定的 /proc 兜底
                        let pr =
                            exec_command(&app, &session_id, PROBE_PROC_LINUX, LIST_TIMEOUT_MS)
                                .await?;
                        parse_proc_records(&pr.stdout)
                    } else {
                        v
                    }
                }
                Os::MacOS => {
                    let r = exec_command(&app, &session_id, PROBE_MACOS, LIST_TIMEOUT_MS).await?;
                    parse_ps_bsd(&r.stdout)
                }
                Os::Windows => {
                    let r = exec_command(&app, &session_id, PROBE_WINDOWS, LIST_TIMEOUT_MS).await?;
                    parse_tasklist(&r.stdout)
                }
            };
            // 仍为空：ps 与 /proc 都没能给出数据（极少数无 ps 且 /proc 不可读的机器），
            // 转成可读错误而非静默空列表，避免「出不来进程」的困惑。
            if parsed.is_empty() {
                return Err(AppError::Exec(
                    "远端进程采样失败：ps 与 /proc 均无有效输出".into(),
                ));
            }
            parsed
        }
        _ => {
            return Err(AppError::Invalid(
                "该会话类型不支持进程管理（无 exec 通道）".into(),
            ))
        }
    };

    // 过滤词：对全量样本匹配（在排序/截断之前），有词即权威搜索、无视 top-N 边界。
    if let Some(f) = filter.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        procs = filter_procs(procs, f);
    }

    let key = sort_by.unwrap_or_else(|| "cpu".into());
    // 降序：CPU/内存取大者在前；pid 取小者在前（稳定可读）。
    procs.sort_by(|a, b| match key.as_str() {
        "mem" => b.mem_kb.cmp(&a.mem_kb),
        "pid" => a.pid.cmp(&b.pid),
        _ => b
            .cpu_pct
            .unwrap_or(0.0)
            .partial_cmp(&a.cpu_pct.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal),
    });
    let n = limit.unwrap_or(DEFAULT_LIMIT as u32).max(1) as usize;
    procs.truncate(n);
    Ok(procs)
}

/// `process_kill` 的核验结果：信号已发出后，进程到底死没死。
///
/// 「kill 退出码 0」只证明信号投递成功，不证明进程退出（TERM 可被忽略/正在优雅退出/已成僵尸）。
/// 之前前端据此直接弹「已结束」，实际进程可能还在 → 列表反复出现，用户以为杀不掉。
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KillResult {
    /// `true`=已不在进程表；`false`=仍存在（含僵尸态，`state` 可区分）；`None`=平台不支持核验。
    pub gone: Option<bool>,
    /// 核验时的状态原词（如 `S` / `Z` / `R`）；拿不到为 `None`。
    pub state: Option<String>,
}

/// `process_kill`：pid≤1 直接拒；组 kill/taskkill 串 → 过 PolicyEngine（Blocked 拒）→ exec 执行。
///
/// `signal`（仅 POSIX 有意义）：`KILL`=强杀、其余（含缺省）=`TERM` 优雅终止。Windows 用 taskkill
/// （`KILL` → `/F` 强制，否则普通结束）。前端的确认弹窗即人工审批，这里的策略门是不可绕过的兜底。
/// POSIX 路径把「发信号 + 等 1s + 核验存活」合并进同一条命令，返回 [`KillResult`] 供前端如实提示。
#[tauri::command]
pub async fn process_kill(
    app: AppHandle,
    session_id: String,
    pid: u32,
    signal: Option<String>,
) -> Result<KillResult, AppError> {
    // 兜底 1：pid≤1 是系统祖进程 / 无效 pid，任何 OS 都禁（与 PID namespace 语义无关，绝对安全线）。
    if pid <= 1 {
        return Err(AppError::Biz {
            key: "err.process.forbidden",
            zh: "不允许操作系统关键进程（PID ≤ 1）",
        });
    }
    let kind = state(&app)
        .sessions
        .kind(&session_id)
        .ok_or_else(|| AppError::NoSuchSession(session_id.clone()))?;
    let force = matches!(
        signal.as_deref().map(str::to_ascii_uppercase).as_deref(),
        Some("KILL") | Some("9")
    );
    // POSIX 才能核验；Windows 的 taskkill 无统一核验串，返回 None（前端回退旧提示）。
    let (cmd, verify) = match kind {
        SessionKind::Local => {
            if cfg!(windows) {
                (taskkill_cmd(pid, force), false)
            } else {
                (kill_cmd(pid, force), true)
            }
        }
        SessionKind::Ssh => {
            if remote_os(&app, &session_id).await? == Os::Windows {
                (taskkill_cmd(pid, force), false)
            } else {
                (kill_cmd(pid, force), true)
            }
        }
        _ => {
            return Err(AppError::Invalid(
                "该会话类型不支持进程管理（无 exec 通道）".into(),
            ))
        }
    };

    // 兜底 2：与 AI run_command 共享的策略大脑。kill 本是写操作，故只拦黑名单命中的 Blocked
    // （如误把关键守护进程包进 pid 的命令、或用户在策略表里拉黑 kill 的模式串）。
    let verdict = PolicyEngine::default().classify(&cmd);
    if verdict.decision == Decision::Blocked {
        return Err(AppError::Biz {
            key: "err.process.forbidden",
            zh: "该进程操作被安全策略拦截（命中高危黑名单）",
        });
    }

    let r = exec_command(&app, &session_id, &cmd, KILL_TIMEOUT_MS).await?;
    // POSIX kill 成功退出 0；taskkill 成功也 0。非 0（如进程已不存在 / 权限不足）转可读错误。
    if r.exit_code != Some(0) {
        let err = r.stderr.trim();
        let out = r.stdout.trim();
        let detail = if !err.is_empty() {
            err
        } else if !out.is_empty() {
            out
        } else {
            "无输出"
        };
        return Err(AppError::Exec(format!(
            "结束进程失败（退出码 {:?}）：{}",
            r.exit_code, detail
        )));
    }
    Ok(if verify {
        parse_kill_verdict(r.stdout.trim())
    } else {
        KillResult {
            gone: None,
            state: None,
        }
    })
}

/// POSIX 终止串：`kill -TERM pid` / `kill -KILL pid`，并**在同一条 exec 里核验结果**：
/// 发完信号等 1s，再 `kill -0` 探存活（POSIX 标准做法，对僵尸态也准确）——
/// 无输出/失败即已不在进程表，回 `GONE`；仍在则回 `ALIVE:<ps 状态码>`（S/R/Z…）。
/// `kill` 本身失败（进程已不存在/无权限）走非零退出码，由调用方转成可读错误。
fn kill_cmd(pid: u32, force: bool) -> String {
    format!(
        "kill -{} {pid} 2>/dev/null || {{ echo SIGNAL_FAIL; exit 1; }}; \
         sleep 1; \
         if kill -0 {pid} 2>/dev/null; then \
           st=$(ps -p {pid} -o stat= 2>/dev/null); \
           echo \"ALIVE:${{st:-?}}\"; \
         else \
           echo GONE; \
         fi",
        if force { "KILL" } else { "TERM" }
    )
}

/// 解析核验串：`GONE` → 已退出；`ALIVE:Z` → 僵尸；`ALIVE:S` 等 → 仍存活；其余 → 未知。
fn parse_kill_verdict(out: &str) -> KillResult {
    if out == "GONE" {
        KillResult {
            gone: Some(true),
            state: None,
        }
    } else if let Some(st) = out.strip_prefix("ALIVE:") {
        KillResult {
            gone: Some(false),
            state: Some(st.trim().to_string()),
        }
    } else {
        KillResult {
            gone: None,
            state: None,
        }
    }
}

/// 全量样本上的子串过滤：命令/用户不区分大小写、PID 精确匹配；无匹配返回空（搜索语义，非错误）。
fn filter_procs(procs: Vec<ProcInfo>, f: &str) -> Vec<ProcInfo> {
    let fl = f.to_lowercase();
    procs
        .into_iter()
        .filter(|p| {
            p.cmd.to_lowercase().contains(&fl)
                || p.user.to_lowercase().contains(&fl)
                || p.pid.to_string() == f
        })
        .collect()
}

/// Windows 终止串：`taskkill /PID pid` / `taskkill /PID pid /F`。
fn taskkill_cmd(pid: u32, force: bool) -> String {
    if force {
        format!("taskkill /PID {pid} /F")
    } else {
        format!("taskkill /PID {pid}")
    }
}

/// SSH 会话远端 OS 探测：跑 `uname`，按回显判三分法。会话不存在/断开由 exec_command 直接抛错。
async fn remote_os(app: &AppHandle, session_id: &str) -> Result<Os, AppError> {
    let r = exec_command(
        app,
        session_id,
        "uname -s 2>/dev/null || echo NONE",
        OS_PROBE_TIMEOUT_MS,
    )
    .await?;
    Ok(classify_os(&r.stdout))
}

/// 本地进程列表（sysinfo）：CPU% 需相邻两次刷新才有值，取 200ms 间隔。
fn local_procs() -> Result<Vec<ProcInfo>, AppError> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
    let kind = RefreshKind::nothing()
        .with_processes(ProcessRefreshKind::everything());
    let mut sys = System::new_with_specifics(kind);
    sys.refresh_processes(ProcessesToUpdate::All, true);
    std::thread::sleep(std::time::Duration::from_millis(200));
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut out = Vec::with_capacity(sys.processes().len());
    for (pid, p) in sys.processes() {
        let start = p.start_time();
        let elapsed = if start > 0 && now >= start { now - start } else { 0 };
        out.push(ProcInfo {
            pid: pid.as_u32(),
            // sysinfo 0.37 的 Process 不暴露归属用户名；本地列表以 pid/名称/CPU/内存为主。
            user: String::new(),
            state: format!("{:?}", p.status()),
            cpu_pct: Some(p.cpu_usage()),
            mem_kb: p.memory() / 1024,
            elapsed_sec: elapsed,
            cmd: p.name().to_string_lossy().into_owned(),
        });
    }
    Ok(out)
}

/// 解析 `ps aux`（BSD 风格）输出。不同实现（procps-ng / BusyBox）列顺序不同，
/// 故**按表头列名定位**再逐行取值；最后一列（COMMAND）吃掉行内所有余部（可含空格）。
/// 表头缺失 / 非 ps 输出 → 返回空（交由上层报错）。`ps aux` 无「已运行秒数」→ elapsed_sec 恒 0。
pub fn parse_ps_aux(text: &str) -> Vec<ProcInfo> {
    let mut header: Option<Vec<String>> = None;
    let mut out = Vec::new();
    for line in text.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.is_empty() {
            continue;
        }
        // 首行判定为表头（含 PID + USER/COMMAND 类列名）；否则视为非 ps 输出，返回空。
        if header.is_none() {
            let up: Vec<String> = toks.iter().map(|s| s.to_uppercase()).collect();
            let is_hdr = up.iter().any(|c| c.as_str() == "PID")
                && up.iter().any(|c| matches!(c.as_str(), "USER" | "COMMAND" | "CMD" | "ARGS"));
            if is_hdr {
                header = Some(up);
            }
            continue;
        }
        let h = header.as_ref().unwrap();
        if toks.len() < h.len() {
            continue;
        }
        // 前 n-1 列各占一个 token，最后一列（COMMAND）并入所有余部。
        let mut cols: Vec<String> = toks[..h.len() - 1].iter().map(|s| s.to_string()).collect();
        cols.push(toks[h.len() - 1..].join(" "));
        let val = |names: &[&str]| -> Option<&str> {
            names
                .iter()
                .find_map(|n| h.iter().position(|c| c.as_str() == *n))
                .and_then(|i| cols.get(i).map(|s| s.as_str()))
        };
        let pid: u32 = match val(&["PID"]).and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        out.push(ProcInfo {
            pid,
            user: val(&["USER", "UID"]).unwrap_or("").to_string(),
            state: val(&["STAT", "STATE"]).unwrap_or("").to_string(),
            cpu_pct: val(&["%CPU", "CPU"]).and_then(|s| s.parse().ok()),
            mem_kb: val(&["RSS"]).and_then(|s| s.parse().ok()).unwrap_or(0),
            elapsed_sec: val(&["ELAPSED", "ETIMES"]).and_then(|s| s.parse().ok()).unwrap_or(0),
            cmd: val(&["COMMAND", "CMD", "ARGS"]).unwrap_or("").to_string(),
        });
    }
    out
}

/// 解析 [`PROBE_PROC_LINUX`] 的 `/proc` 兜底输出：每条记录一行，字段以 `\u{1f}` 分隔——
/// `pid \u{1f} uid \u{1f} state \u{1f} rss_kb \u{1f} cmd`。单次采样拿不到 CPU% → `cpu_pct=None`；
/// user 直接给数字 uid（兜底路径不做 passwd 反查）。
pub fn parse_proc_records(text: &str) -> Vec<ProcInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut f = line.split('\u{1f}');
        let pid = match f.next().and_then(|s| s.trim().parse::<u32>().ok()) {
            Some(p) => p,
            None => continue,
        };
        let user = f.next().unwrap_or("").trim().to_string();
        let state = f.next().unwrap_or("").trim().to_string();
        let mem_kb = f
            .next()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let cmd = f.next().unwrap_or("").trim().to_string();
        out.push(ProcInfo {
            pid,
            user,
            state,
            cpu_pct: None,
            mem_kb,
            elapsed_sec: 0,
            cmd,
        });
    }
    out
}

/// 解析 BSD（macOS）输出：列 = pid/user/stat/%cpu/rss/etime/comm（**无 pmem**，比 procps 少一列）；
/// 前 6 列定长字段，余部 join 回 comm；`etime` 串换算秒。
pub fn parse_ps_bsd(text: &str) -> Vec<ProcInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let tk: Vec<&str> = line.split_whitespace().collect();
        if tk.len() < 6 {
            continue;
        }
        let Ok(pid) = tk[0].parse::<u32>() else {
            continue;
        };
        out.push(ProcInfo {
            pid,
            user: tk[1].to_string(),
            state: tk[2].to_string(),
            cpu_pct: Some(tk[3].parse::<f32>().unwrap_or(0.0)),
            mem_kb: tk[4].parse::<u64>().unwrap_or(0),
            elapsed_sec: parse_etime(tk[5]).unwrap_or(0),
            cmd: tk[6..].join(" "),
        });
    }
    out
}

/// BSD `ps etime` → 秒：格式 `[[DD-]HH:]MM:SS`。
fn parse_etime(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // 天：以 `-` 分隔（如 `1-02:03:04`）。
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<u64> = rest.split(':').map(str::to_string).filter_map(|x| x.parse().ok()).collect();
    let (hms, mins, secs) = match parts.len() {
        3 => (parts[0], parts[1], parts[2]),
        2 => (0, parts[0], parts[1]),
        1 => (0, 0, parts[0]),
        _ => return None,
    };
    Some(((days * 24 + hms) * 3600) + mins * 60 + secs)
}

/// 解析 `tasklist /fo csv /nh`：每行 `"映像名","PID","会话名","会话#","内存使用"`。
/// 引号内含逗号（映像名可含空格）——走 [`split_csv_line`]。CPU/状态/运行时长 tasklist 不提供。
pub fn parse_tasklist(text: &str) -> Vec<ProcInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let cols = split_csv_line(line.trim());
        if cols.len() < 5 {
            continue;
        }
        let Ok(pid) = cols[1].parse::<u32>() else { continue };
        let mem_kb = parse_kb_with_unit(&cols[4]);
        out.push(ProcInfo {
            pid,
            user: String::new(),
            state: String::new(),
            cpu_pct: None,
            mem_kb,
            elapsed_sec: 0,
            cmd: cols[0].clone(),
        });
    }
    out
}

/// tasklist 内存列形如 `1,234 K`：去千分位逗号、去单位、按 KB 取整。
fn parse_kb_with_unit(s: &str) -> u64 {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    cleaned.parse().unwrap_or(0)
}

/// 拆一行 CSV（双引号包裹、`""` 转义内部引号），返回去引号的字段列表。
fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ',' if !in_quotes => {
                fields.push(std::mem::take(&mut cur));
            }
            other => cur.push(other),
        }
    }
    fields.push(cur);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_os_maps_uname() {
        assert_eq!(classify_os("Linux\n"), Os::Linux);
        assert_eq!(classify_os("Darwin"), Os::MacOS);
        assert_eq!(classify_os("NONE"), Os::Windows);
        assert_eq!(classify_os(""), Os::Windows);
        assert_eq!(classify_os("'uname' is not recognized"), Os::Windows);
    }

    #[test]
    fn ps_aux_skips_header_and_keeps_args_with_spaces() {
        let text = "USER         PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND\n\
                    root           1  0.0  0.1 100000   200 ?        Ss   Jan01   0:01 /sbin/init\n\
                    alice         42 12.5  3.0 999999  1200 ?        R    Jan01   0:12 python train.py --epochs 3\n";
        let v = parse_ps_aux(text);
        assert_eq!(v.len(), 2); // 表头行被跳过
        assert_eq!(v[0].pid, 1);
        assert_eq!(v[1].pid, 42);
        assert_eq!(v[1].user, "alice");
        assert_eq!(v[1].state, "R");
        assert_eq!(v[1].cpu_pct, Some(12.5));
        assert_eq!(v[1].mem_kb, 1200);
        assert_eq!(v[1].elapsed_sec, 0);
        assert_eq!(v[1].cmd, "python train.py --epochs 3");
    }

    #[test]
    fn ps_aux_maps_columns_by_header_regardless_of_order() {
        // BusyBox 风格：列顺序与 procps 不同（PID 在前，无 %CPU/RSS/START/TIME）。
        let text = "PID USER STAT VSZ %MEM COMMAND\n\
                    1 root S 100000 0.1 /sbin/init\n\
                    42 alice R 999999 3.0 python train.py --epochs 3\n";
        let v = parse_ps_aux(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].pid, 42);
        assert_eq!(v[1].user, "alice");
        assert_eq!(v[1].state, "R");
        assert_eq!(v[1].cmd, "python train.py --epochs 3");
        assert_eq!(v[1].cpu_pct, None); // 无 %CPU 列
        assert_eq!(v[1].mem_kb, 0); // 无 RSS 列
    }

    #[test]
    fn proc_records_parse_unit_separated() {
        let s = '\u{1f}';
        let text = format!(
            "1{s}0{s}S{s}9876{s}/sbin/init --quiet\n42{s}1000{s}R{s}54321{s}python train.py --epochs 3\n",
            s = s
        );
        let v = parse_proc_records(&text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].pid, 1);
        assert_eq!(v[1].pid, 42);
        assert_eq!(v[1].user, "1000");
        assert_eq!(v[1].state, "R");
        assert_eq!(v[1].mem_kb, 54321);
        assert_eq!(v[1].cpu_pct, None);
        assert_eq!(v[1].cmd, "python train.py --epochs 3");
    }

    #[test]
    fn bsd_parses_etime_to_seconds() {
        let text = "  1 root  Ss   0.0  1234    01:02 /sbin/launchd\n\
                     99 bob   R    33.3  5678  2-03:04:05 /usr/bin/app arg\n";
        let v = parse_ps_bsd(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].elapsed_sec, 62);
        assert_eq!(v[1].elapsed_sec, (2 * 24 + 3) * 3600 + 4 * 60 + 5);
        assert_eq!(v[1].cmd, "/usr/bin/app arg");
    }

    #[test]
    fn parse_etime_variants() {
        assert_eq!(parse_etime("05:00"), Some(300));
        assert_eq!(parse_etime("1:30:00"), Some(5400));
        assert_eq!(parse_etime("3-00:00:00"), Some(3 * 86400));
        assert_eq!(parse_etime("junk"), None);
    }

    #[test]
    fn tasklist_parses_csv_with_quoted_comma_name() {
        let text = "\"chrome.exe\",\"1234\",\"Console\",\"1\",\"123,456 K\"\n\
                     \"svchost.exe\",\"567\",\"Services\",\"0\",\"8,192 K\"\n";
        let v = parse_tasklist(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].pid, 1234);
        assert_eq!(v[0].cmd, "chrome.exe");
        assert_eq!(v[0].mem_kb, 123456);
        assert_eq!(v[0].cpu_pct, None);
    }

    #[test]
    fn kill_cmd_respects_signal_and_verifies() {
        let ter = kill_cmd(4242, false);
        assert!(ter.starts_with("kill -TERM 4242"));
        assert!(ter.contains("sleep 1"));
        assert!(ter.contains("kill -0 4242"));
        assert!(ter.contains("echo GONE"));
        let kill = kill_cmd(4242, true);
        assert!(kill.starts_with("kill -KILL 4242"));
        assert_eq!(taskkill_cmd(4242, false), "taskkill /PID 4242");
        assert_eq!(taskkill_cmd(4242, true), "taskkill /PID 4242 /F");
    }

    #[test]
    fn kill_verdict_parses_gone_alive_unknown() {
        assert_eq!(
            parse_kill_verdict("GONE"),
            KillResult { gone: Some(true), state: None }
        );
        assert_eq!(
            parse_kill_verdict("ALIVE:Z"),
            KillResult { gone: Some(false), state: Some("Z".into()) }
        );
        assert_eq!(
            parse_kill_verdict("ALIVE:Ss"),
            KillResult { gone: Some(false), state: Some("Ss".into()) }
        );
        assert_eq!(
            parse_kill_verdict(""),
            KillResult { gone: None, state: None }
        );
        assert_eq!(
            parse_kill_verdict("garbage"),
            KillResult { gone: None, state: None }
        );
    }

    #[test]
    fn filter_procs_matches_cmd_user_pid() {
        let mk = |pid: u32, user: &str, cmd: &str| ProcInfo {
            pid,
            user: user.into(),
            state: "S".into(),
            cpu_pct: Some(0.0),
            mem_kb: 0,
            elapsed_sec: 0,
            cmd: cmd.into(),
        };
        let v = vec![
            mk(101, "mysql", "/usr/sbin/mysqld --daemonize"),
            mk(102, "root", "/usr/sbin/sshd"),
            mk(103, "root", "/usr/bin/mysql-client"),
        ];
        assert_eq!(filter_procs(v.clone(), "mysql").len(), 2); // 命令 + 用户
        assert_eq!(filter_procs(v.clone(), "MYSQLD").len(), 1); // 命令不区分大小写
        assert_eq!(filter_procs(v.clone(), "102").len(), 1); // PID 精确
        assert_eq!(filter_procs(v, "nginx").len(), 0); // 无匹配 → 空（搜索语义）
    }
}
