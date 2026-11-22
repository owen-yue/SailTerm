//! 服务列表与启停操作（服务管理：Dock 面板 + AI 工具共用核心）。
//!
//! 采样与 [super::process] 同走 [crate::ai::exec::exec_command]（独立 exec 通道，不污染用户
//! PTY、不二次认证），并按远端 OS 选探测串：
//! - **Linux**：
//!   - systemd：`systemctl list-units --type=service --all` 三段采样（单元/自启态/MainPID），
//!     描述列可含空格且个别单元名也含空格，故只按空白切前 4 列、余部并入描述（同 parse_ps_aux 思路）。
//!   - 非 systemd 降级（老 SysVinit / BusyBox / 精简容器）：POSIX sh 遍历 `/etc/init.d/*`，
//!     运行态靠 pidfile（/var/run|/run + basename.pid）反查进程存活，产出 name+state 的最小记录。
//! - **macOS**：`launchctl list` 采样 launchd 作业（PID / Status / Label 三列）。自启列留空
//!   （RunAtLoad 需解析 plist，首版不做）；启停走 `launchctl start/stop`，restart=stop 后 start，
//!   reload 无对应语义（返回 `err.service.reloadUnsupported`）。
//! - **Windows**：暂不支持，返回明确的「暂不支持该 OS」错误（`err.service.unsupportedOs`）。
//!
//! `service_list` 全是只读命令，与 process_list 一样**不经策略审批**。`service_control` 是写操作：
//! 前端的确认弹窗即人工审批（与决策 D-kill A 同构），后端再做**三道兜底**——① 服务名字符白名单
//! （Linux 防注入并强制 `.service` 作用域；macOS 用 label 白名单，不强制后缀）；② action 白名单；
//! ③ 组好的命令串过 [crate::ai::policy::PolicyEngine]，命中 `Blocked` 拒绝。
//! Linux 上 SysV 机器 systemctl 会失败，按特征输出回退 `service <name> <action>` 再试一次。

use std::collections::HashMap;

use serde::Serialize;
use tauri::AppHandle;

use super::state;
use crate::ai::exec::exec_command;
use crate::ai::policy::{Decision, PolicyEngine};
use crate::error::AppError;
use crate::session::SessionKind;

/// 单个服务的视图。`MainPID` 拿不到（未运行 / 非 forking 单元 / SysV 降级）时 `pid=0`（前端显 `—`）。
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SvcInfo {
    /// 单元名（`nginx.service`；SysV 降级时为脚本名，不带后缀）。
    pub name: String,
    /// LOAD 列：loaded / not-found / masked…（SysV 降级恒 loaded）。
    pub loaded: String,
    /// ACTIVE 列原词：active / inactive / failed（SysV 降级：running→active，其余→inactive，判不了→空串）。
    pub state: String,
    /// SUB 列：running / exited / dead…（SysV 降级：running 或空串）。
    pub sub: String,
    /// 自启态 enabled / disabled / static / masked…（list-unit-files 合并；拿不到留空串）。
    pub enabled: String,
    /// MainPID，0 = 未知。
    pub pid: u32,
}

/// 采样一次 exec 的耗时上限（单元数可能很多 + 三段串行往返）。
const LIST_TIMEOUT_MS: u64 = 10_000;
/// 远端 OS 探测超时（与 process.rs 同口径）。
const OS_PROBE_TIMEOUT_MS: u64 = 6_000;
/// 单元总数上限：防 IPC 载荷爆炸（全机 service 单元通常 ≤ 几百，留足余量）。
const MAX_UNITS: usize = 2000;

/// systemd 单元列表：`--all` 含已停止，`--no-legend` 去表头/尾注，`--no-pager` 防分页卡死。
const PROBE_SYSTEMD_UNITS: &str =
    "systemctl list-units --type=service --all --no-legend --no-pager 2>/dev/null";
/// systemd 自启态列表（列 = UNIT FILE, STATE, [PRESET]；旧版无第三列，按前两列取值即可）。
const PROBE_SYSTEMD_ENABLEDS: &str =
    "systemctl list-unit-files --type=service --no-legend --no-pager 2>/dev/null";
/// systemd MainPID 采集：远端 shell 对每个 **active** 单元逐个跑 `systemctl show -p MainPID <unit>`
/// （该单属性查询跨 systemd 版本稳健，v219/CentOS 7 亦恒回 `MainPID=NNN`），用 `sed` 取纯数字，
/// 输出 `unit\037pid` 行。旧实现用 `systemctl show --all --type=service --property=MainPID` 批量拉，
/// 但 v219 不回 `id=` 行、单元块无干净分隔，无法把 MainPID 配到单元名（真机实测 PID 列全空）。
/// 只查 active 单元（inactive/failed 的 MainPID 本就为 0），把逐单元开销收敛到运行中的那几个。
const PROBE_SYSTEMD_PIDS: &str = r#"for u in $(systemctl list-units --type=service --all --no-legend --no-pager 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="active"){print $1;break}}'); do
  p=$(systemctl show -p MainPID "$u" 2>/dev/null | sed -n 's/^MainPID=//p')
  printf '%s\037%s\n' "$u" "$p"
done"#;
/// SysV/BusyBox 兜底：遍历 `/etc/init.d/*`，字段以 \037(unit-sep) 分隔——
/// `name \037 state \037 pid`。运行态判据：pidfile（/var/run、/run 下 `<name>.pid`）里的数字
/// 进程仍存活；退回 `status` 输出关键字（running/active(running)）。两条都不满足记 inactive；
/// status 有输出但认不出来留空串（界面如实显「未知」，不猜）。纯 POSIX sh，BusyBox 亦可用。
const PROBE_SYSV: &str = r#"for f in /etc/init.d/*; do
  [ -f "$f" ] || continue
  n=${f##*/}
  pid=""
  for d in /var/run /run; do
    [ -n "$pid" ] && continue
    if [ -f "$d/$n.pid" ]; then
      p=$(head -n1 "$d/$n.pid" 2>/dev/null | tr -cd '0-9')
      if [ -n "$p" ] && [ -d "/proc/$p" ]; then pid=$p; fi
    fi
  done
  if [ -n "$pid" ]; then st=running
  else
    o=$("$f" status 2>&1 | head -n1 || true)
    case "$o" in
      *eading*|*ctive*|*unning*) st=running ;;
      "") st=stopped ;;
      *) st="" ;;
    esac
    if [ "$st" = running ]; then
      for w in $o; do case "$w" in
        *[1-9]*) [ -z "$pid" ] && pid=$(printf '%s' "$w" | tr -cd '0-9') ;;
      esac; done
    fi
  fi
  printf '%s\037%s\037%s\n' "$n" "$st" "$pid"
done"#;
/// macOS launchd 作业列表：`launchctl list` 输出 `PID\tStatus\tLabel` 三列（首行表头）。
/// PID 为 `-` 表示未运行；Status 是上次退出码（0=正常，非 0=异常）。反映当前 exec 用户可见的
/// launchd 域（SSH 非 GUI 会话可能看不全系统守护进程，属 launchd 固有限制，如实呈现）。
const PROBE_LAUNCHCTL: &str = "launchctl list 2>/dev/null";

/// 远端 OS 三分法（与 process.rs 同构：探测靠 `uname`，拿不到即 Windows）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Os {
    Linux,
    MacOS,
    Windows,
}

/// 从 `uname -s` 回显判 OS（宽容：空/异常一律回落 Windows 分支）。
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

/// 服务采样的共享核心（IPC 命令与 AI 工具 `list_services` 同入口）。
///
/// 本地会话按本机 OS 分派（Windows 本机走「不支持」分支）；FTP/Telnet/串口/Mosh 无 exec
/// 通道 → [`AppError::Invalid`]（前端已按 `hasCap(kind,"monitor")` 过滤入口，这里最后一道兜底）。
pub async fn sample(app: &AppHandle, session_id: &str) -> Result<Vec<SvcInfo>, AppError> {
    let kind = state(app)
        .sessions
        .kind(session_id)
        .ok_or_else(|| AppError::NoSuchSession(session_id.to_string()))?;
    match kind {
        SessionKind::Local | SessionKind::Ssh => {}
        _ => {
            return Err(AppError::Invalid(
                "该会话类型不支持服务管理（无 exec 通道）".into(),
            ))
        }
    }
    let os = remote_os(app, session_id).await?;
    let svcs = match os {
        Os::Linux => linux_sample(app, session_id).await?,
        Os::MacOS => macos_sample(app, session_id).await?,
        Os::Windows => {
            return Err(AppError::Biz {
                key: "err.service.unsupportedOs",
                zh: "暂不支持该操作系统的服务查看",
            })
        }
    };
    if svcs.is_empty() {
        let detail = match os {
            Os::MacOS => "远端服务采样失败：launchctl list 无有效输出",
            _ => "远端服务采样失败：systemctl 与 /etc/init.d 均无有效输出",
        };
        return Err(AppError::Exec(detail.into()));
    }
    Ok(svcs)
}

/// Linux 服务采样：systemd 三段 → 空则 SysV 降级。
async fn linux_sample(app: &AppHandle, session_id: &str) -> Result<Vec<SvcInfo>, AppError> {
    let units = exec_command(app, session_id, PROBE_SYSTEMD_UNITS, LIST_TIMEOUT_MS).await?;
    let mut svcs = parse_systemctl_units(&units.stdout);
    if !svcs.is_empty() {
        // 自启态：一次 list-unit-files 合并（失败不致命——该列留空即可，不打断整次采样）。
        if let Ok(r) = exec_command(app, session_id, PROBE_SYSTEMD_ENABLEDS, LIST_TIMEOUT_MS).await
        {
            merge_enabled(&mut svcs, &parse_enabled(&r.stdout));
        }
        // MainPID：一次 show 拿全（同样允许失败，pid 列留 0）。
        if let Ok(r) = exec_command(app, session_id, PROBE_SYSTEMD_PIDS, LIST_TIMEOUT_MS).await {
            merge_pids(&mut svcs, &parse_main_pids(&r.stdout));
        }
        return Ok(trim(svcs));
    }
    // systemctl 不存在/无输出（老 SysVinit、BusyBox、精简容器）→ /etc/init.d 遍历降级
    let r = exec_command(app, session_id, PROBE_SYSV, LIST_TIMEOUT_MS).await?;
    Ok(trim(parse_sysv(&r.stdout)))
}

/// macOS 服务采样：`launchctl list` 一次拿全（无 systemd 式自启/PID 分段，enabled 列留空）。
async fn macos_sample(app: &AppHandle, session_id: &str) -> Result<Vec<SvcInfo>, AppError> {
    let r = exec_command(app, session_id, PROBE_LAUNCHCTL, LIST_TIMEOUT_MS).await?;
    Ok(trim(parse_launchctl(&r.stdout)))
}

/// `service_list`：只读采样，按名称升序返回（服务数远小于进程数，不做截断口径的排序下发）。
#[tauri::command]
pub async fn service_list(
    app: AppHandle,
    session_id: String,
) -> Result<Vec<SvcInfo>, AppError> {
    let mut svcs = sample(&app, &session_id).await?;
    svcs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(svcs)
}

/// `service_control` 的核验结果：命令退出码 0 只代表信号/请求投递成功，不代表达到预期状态。
///
/// macOS 最典型：`launchctl stop` 对 KeepAlive 作业「停完即被 launchd 拉起」，退出码仍是 0——
/// 之前前端据此直接弹「已停止」，用户看到服务还在。Linux systemd 的 stop/start 同步等收敛，
/// 退出码 0 即事实（不核验，settled=None）；Windows 不支持（直接报错）。
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SvcResult {
    /// `true`=达到预期状态；`false`=未达到（如 stop 后被 KeepAlive 拉起）；`None`=平台未核验。
    pub settled: Option<bool>,
    /// 核验时的实际状态描述（如 `running:25739` / `stopped`）；拿不到为 `None`。
    pub state: Option<String>,
}

/// `service_control`：启停服务。名称/动作白名单校验 → 组装串过 PolicyEngine（Blocked 拒）→
/// exec 执行；SysV 机器按特征输出回退 `service` 命令再试。前端确认弹窗即人工审批，
/// 这里的策略门是不可绕过的兜底。macOS 路径合并状态核验，返回 [`SvcResult`]。
#[tauri::command]
pub async fn service_control(
    app: AppHandle,
    session_id: String,
    name: String,
    action: String,
) -> Result<SvcResult, AppError> {
    if !matches!(action.as_str(), "start" | "stop" | "restart" | "reload") {
        return Err(AppError::Biz {
            key: "err.service.badAction",
            zh: "不支持的服务操作",
        });
    }
    let kind = state(&app)
        .sessions
        .kind(&session_id)
        .ok_or_else(|| AppError::NoSuchSession(session_id.clone()))?;
    match kind {
        SessionKind::Local | SessionKind::Ssh => {}
        _ => {
            return Err(AppError::Invalid(
                "该会话类型不支持服务管理（无 exec 通道）".into(),
            ))
        }
    }
    match remote_os(&app, &session_id).await? {
        Os::Linux => {
            linux_control(&app, &session_id, &name, &action).await?;
            Ok(SvcResult {
                settled: None,
                state: None,
            })
        }
        Os::MacOS => macos_control(&app, &session_id, &name, &action).await,
        Os::Windows => Err(AppError::Biz {
            key: "err.service.unsupportedOs",
            zh: "暂不支持该操作系统的服务管理",
        }),
    }
}

/// Linux 启停：`.service` 单元加固 → systemctl 串过策略 → 执行；SysV 机器按特征回退 `service`。
async fn linux_control(
    app: &AppHandle,
    session_id: &str,
    name: &str,
    action: &str,
) -> Result<(), AppError> {
    let unit = validate_unit(name)?;
    // 兜底：与 AI run_command 共享的策略大脑（内置分级已把 systemctl 写子命令归为写操作，
    // 这里只拦黑名单命中的 Blocked；面板人工确认即 NeedsApproval 语义的审批）。
    let cmd = format!("systemctl {action} {unit}");
    let verdict = PolicyEngine::default().classify(&cmd);
    if verdict.decision == Decision::Blocked {
        return Err(AppError::Biz {
            key: "err.service.forbidden",
            zh: "该服务操作被安全策略拦截（命中高危黑名单）",
        });
    }

    let r = exec_command(app, session_id, &cmd, LIST_TIMEOUT_MS).await?;
    if r.exit_code == Some(0) {
        return Ok(());
    }
    // SysV 机器上 systemctl 必然失败（未以 systemd 启动 / 命令不存在）：按特征串回退一次。
    let err_low = r.stderr.to_lowercase();
    if err_low.contains("system has not been booted with systemd")
        || err_low.contains("systemd is not available")
        || err_low.contains("command not found")
    {
        // SysV 认 /etc/init.d 下的脚本名（如 sshd），不带 systemd 的 .service 后缀；否则回退命令必失败。
        let legacy = format!("service {} {action}", unit.strip_suffix(".service").unwrap_or(&unit));
        let v2 = PolicyEngine::default().classify(&legacy);
        if v2.decision != Decision::Blocked {
            let r2 = exec_command(app, session_id, &legacy, LIST_TIMEOUT_MS).await?;
            if r2.exit_code == Some(0) {
                return Ok(());
            }
            return Err(exec_fail(&r2));
        }
    }
    Err(exec_fail(&r))
}

/// macOS 启停：label 加固 → 合并「操作 + 1s 后核验状态」过策略。reload 无对应语义直接拒；
/// restart = 先 stop（未运行时失败忽略）再 start（以 start 的核验结果为准）。
async fn macos_control(
    app: &AppHandle,
    session_id: &str,
    name: &str,
    action: &str,
) -> Result<SvcResult, AppError> {
    let label = validate_label(name)?;
    match action {
        "start" => launchctl_verify(app, session_id, "start", &label, true).await,
        "stop" => launchctl_verify(app, session_id, "stop", &label, false).await,
        "restart" => {
            let _ = launchctl_run(app, session_id, "stop", &label).await;
            launchctl_verify(app, session_id, "start", &label, true).await
        }
        _ => Err(AppError::Biz {
            key: "err.service.reloadUnsupported",
            zh: "macOS 不支持服务重载，请使用重启",
        }),
    }
}

/// macOS 启停 + 核验合并命令：`launchctl <verb> <label>` 退出码 0 只代表信号投递成功，
/// 不代表作业达到预期（KeepAlive 作业 stop 后被 launchd 立即拉起、start 的作业可能立刻退出）——
/// 等 2s 后用**不带 label 的** `launchctl list` 全列表按第三列精确匹配查 PID：
/// `-`/空=未运行，数字=在跑。
/// 注意：macOS 26+ 的 `launchctl list <label>` 输出作业属性（plist 键值）而非列表行，
/// 不能再用它查 PID（否则把键名误当成 PID 判「在跑」）；全列表 + `$3==L` 才是跨版本稳定口径。
/// 失败时把 launchctl 真实报错并入 stdout 后退出（旧写法 `2>/dev/null || echo CTL_FAIL`
/// 吞掉了报错，用户只能看到内部标记 CTL_FAIL，无从判断是权限/域/禁用哪种失败）。
fn macos_ctl_cmd(verb: &str, label: &str, expect_running: bool) -> String {
    let check = if expect_running {
        r#"if [ -n "$p" ] && [ "$p" != "-" ]; then echo SETTLED; else echo UNSETTLED:stopped; fi"#
    } else {
        r#"if [ -n "$p" ] && [ "$p" != "-" ]; then echo "UNSETTLED:running:$p"; else echo SETTLED; fi"#
    };
    format!(
        "launchctl {verb} {label} 2>&1 || exit 1; \
         sleep 2; \
         p=$(launchctl list 2>/dev/null | awk -v L='{label}' '$3==L {{print $1}}'); \
         {check}",
        verb = verb,
        label = label,
        check = check,
    )
}

/// 跑 macOS 启停 + 核验：过策略（Blocked 拒）+ 退出码判定 + 解析 verdict。
async fn launchctl_verify(
    app: &AppHandle,
    session_id: &str,
    verb: &str,
    label: &str,
    expect_running: bool,
) -> Result<SvcResult, AppError> {
    let cmd = macos_ctl_cmd(verb, label, expect_running);
    let verdict = PolicyEngine::default().classify(&cmd);
    if verdict.decision == Decision::Blocked {
        return Err(AppError::Biz {
            key: "err.service.forbidden",
            zh: "该服务操作被安全策略拦截（命中高危黑名单）",
        });
    }
    let r = exec_command(app, session_id, &cmd, LIST_TIMEOUT_MS).await?;
    if r.exit_code != Some(0) {
        return Err(exec_fail(&r));
    }
    Ok(parse_svc_verdict(r.stdout.trim()))
}

/// 解析核验串：`SETTLED` → 达到预期；`UNSETTLED:<detail>` → 未达到（detail 供前端展示）；
/// 其余（空/异常）→ 未知（前端回退旧提示）。
fn parse_svc_verdict(out: &str) -> SvcResult {
    if out == "SETTLED" {
        SvcResult {
            settled: Some(true),
            state: None,
        }
    } else if let Some(detail) = out.strip_prefix("UNSETTLED:") {
        SvcResult {
            settled: Some(false),
            state: Some(detail.trim().to_string()),
        }
    } else {
        SvcResult {
            settled: None,
            state: None,
        }
    }
}

/// 跑单条 `launchctl <verb> <label>`：过策略（Blocked 拒）+ 退出码判定。
async fn launchctl_run(
    app: &AppHandle,
    session_id: &str,
    verb: &str,
    label: &str,
) -> Result<(), AppError> {
    let cmd = format!("launchctl {verb} {label}");
    let verdict = PolicyEngine::default().classify(&cmd);
    if verdict.decision == Decision::Blocked {
        return Err(AppError::Biz {
            key: "err.service.forbidden",
            zh: "该服务操作被安全策略拦截（命中高危黑名单）",
        });
    }
    let r = exec_command(app, session_id, &cmd, LIST_TIMEOUT_MS).await?;
    if r.exit_code == Some(0) {
        return Ok(());
    }
    Err(exec_fail(&r))
}

/// 服务名加固：字符白名单（防 shell 注入）+ 强制 `.service` 单元作用域（不碰 mount/target/timer）。
/// 空名 / 非法字符 / 非 .service 后缀（如 `foo.mount`）一律拒绝。
pub fn validate_unit(name: &str) -> Result<String, AppError> {
    let n = name.trim();
    if n.is_empty() || !n.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | ':' | '+' | '-')
    }) {
        return Err(AppError::Biz {
            key: "err.service.badName",
            zh: "服务名不合法",
        });
    }
    // 作用域锁死在 .service：判据取「补后缀之前」的原始名最后一个点后的段——显式带其他单元类型
    // 后缀（.mount/.timer/.socket/.target…）一律拒绝，不碰非 service 单元；无点的裸名（nginx）
    // 才在下面补 .service。以 . 开头的裸后缀（.service/.mount）同样死在这条上。
    // （切勿先补 .service 再判后缀：那样 `foo.mount` 会变成 `foo.mount.service` 而蒙混过关。）
    if n.chars().next() == Some('.') {
        return Err(AppError::Biz {
            key: "err.service.badName",
            zh: "服务名不合法",
        });
    }
    if let Some(dot) = n.rfind('.') {
        if &n[dot + 1..] != "service" {
            return Err(AppError::Biz {
                key: "err.service.badName",
                zh: "服务名不合法",
            });
        }
    }
    let unit = if n.ends_with(".service") {
        n.to_string()
    } else {
        format!("{n}.service")
    };
    Ok(unit)
}

/// macOS launchd label 加固：字符白名单（防 shell 注入）+ 禁以 `-`/`.` 开头（防被当作选项/隐藏名）。
/// label 是反向 DNS 风格（`com.apple.foo`），不强制任何后缀。空名/非法字符一律拒绝。
pub fn validate_label(name: &str) -> Result<String, AppError> {
    let n = name.trim();
    if n.is_empty()
        || !n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        || n.starts_with('-')
        || n.starts_with('.')
    {
        return Err(AppError::Biz {
            key: "err.service.badName",
            zh: "服务名不合法",
        });
    }
    Ok(n.to_string())
}

fn exec_fail(r: &crate::ai::exec::ExecResult) -> AppError {
    let err = r.stderr.trim();
    let out = r.stdout.trim();
    let detail = if !err.is_empty() {
        err
    } else if !out.is_empty() {
        out
    } else {
        "无输出"
    };
    AppError::Exec(format!("服务操作失败（退出码 {:?}）：{}", r.exit_code, detail))
}

fn trim(mut svcs: Vec<SvcInfo>) -> Vec<SvcInfo> {
    svcs.truncate(MAX_UNITS);
    svcs
}

/// 解析 `systemctl list-units` 输出：列 = UNIT LOAD ACTIVE SUB DESCRIPTION。
/// UNIT 一般不含空格（printer spooler 类单元存在但罕见），DESCRIPTION 必含空格——
/// 故前 4 列各占一个空白 token，余部并入描述（描述不进视图，直接丢弃）。
/// 兼容旧版 systemd 缺 LOAD 列的四列形态（UNIT ACTIVE SUB DESCRIPTION）。
pub fn parse_systemctl_units(text: &str) -> Vec<SvcInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let tk: Vec<&str> = line.split_whitespace().collect();
        if tk.len() < 4 || !tk[0].ends_with(".service") {
            continue;
        }
        // 5 列（含 LOAD）或 4 列（旧版无 LOAD）：第 2 个 token 是 loaded/not-found/… 即含 LOAD。
        let (loaded, state, sub) = if is_load_word(tk[1]) {
            (tk[1].to_string(), tk[2].to_string(), tk[3].to_string())
        } else {
            ("loaded".to_string(), tk[1].to_string(), tk[2].to_string())
        };
        out.push(SvcInfo {
            name: tk[0].to_string(),
            loaded,
            state,
            sub,
            enabled: String::new(),
            pid: 0,
        });
    }
    out
}

/// systemd LOAD 列取值域（list-units 第二列；ACTIVE 的 active/inactive/failed 不在此列，
/// 注意 `active` 不是 LOAD 词 → 4 列形态不会误判）。
fn is_load_word(w: &str) -> bool {
    matches!(
        w,
        "loaded"
            | "not-found"
            | "bad-setting"
            | "error"
            | "masked"
            | "stub"
            | "reserved"
    )
}

/// 解析 `systemctl list-unit-files`：`UNIT FILE STATE [PRESET]`，取前两列。
pub fn parse_enabled(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let tk: Vec<&str> = line.split_whitespace().collect();
        if tk.len() >= 2 && tk[0].ends_with(".service") {
            map.insert(tk[0].to_string(), tk[1].to_string());
        }
    }
    map
}

/// 解析 [`PROBE_SYSTEMD_PIDS`] 输出：每行 `unit\037MainPID`（远端 shell 已取好纯数字）；
/// pid 段为空/非数字记 0。key 为带 `.service` 后缀的完整单元名，与 SvcInfo.name 对齐。
pub fn parse_main_pids(text: &str) -> HashMap<String, u32> {
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut f = line.split('\u{1f}');
        let Some(name) = f.next().map(str::trim).filter(|n| !n.is_empty()) else {
            continue;
        };
        let pid = f
            .next()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);
        map.insert(name.to_string(), pid);
    }
    map
}

fn merge_enabled(svcs: &mut [SvcInfo], map: &HashMap<String, String>) {
    for s in svcs.iter_mut() {
        if let Some(v) = map.get(&s.name) {
            s.enabled = v.clone();
        }
    }
}

fn merge_pids(svcs: &mut [SvcInfo], map: &HashMap<String, u32>) {
    for s in svcs.iter_mut() {
        if let Some(v) = map.get(&s.name) {
            s.pid = *v;
        }
    }
}

/// 解析 [`PROBE_SYSV`] 输出：每行 `name \037 state \037 pid`（state：running/stopped/空=未知）。
pub fn parse_sysv(text: &str) -> Vec<SvcInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut f = line.split('\u{1f}');
        let Some(name) = f.next().map(str::trim).filter(|n| !n.is_empty()) else {
            continue;
        };
        let st = f.next().unwrap_or("").trim();
        let pid = f
            .next()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let (state, sub) = match st {
            "running" => ("active", "running"),
            "stopped" => ("inactive", ""),
            _ => ("", ""),
        };
        out.push(SvcInfo {
            name: format!("{name}.service"),
            loaded: "loaded".to_string(),
            state: state.to_string(),
            sub: sub.to_string(),
            enabled: String::new(),
            pid,
        });
    }
    out
}

/// 解析 `launchctl list` 输出：列 = PID Status Label（首行表头跳过）。
/// PID 非数字（`-`）记 0=未运行；Status 非数字记 0。状态映射：PID>0→active/running；
/// 否则 Status≠0→failed；否则 inactive。loaded 恒 "loaded"（list 只列已加载作业），enabled 留空。
/// label 可含空格（罕见），取第二个空白后的余部并入。
pub fn parse_launchctl(text: &str) -> Vec<SvcInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let tk: Vec<&str> = line.split_whitespace().collect();
        // 三列 + 首列必须是纯数字 PID 或 `-`（launchctl 恒如此）；据此跳过表头行与任何畸形/噪声行，
        // 免得把非数字行误判成「已停止的作业」。
        if tk.len() < 3 || !(tk[0] == "-" || tk[0].bytes().all(|b| b.is_ascii_digit())) {
            continue;
        }
        let pid = tk[0].parse::<u32>().unwrap_or(0);
        let status = tk[1].parse::<i64>().unwrap_or(0);
        let (state, sub) = if pid > 0 {
            ("active", "running")
        } else if status != 0 {
            ("failed", "")
        } else {
            ("inactive", "")
        };
        out.push(SvcInfo {
            name: tk[2..].join(" "),
            loaded: "loaded".to_string(),
            state: state.to_string(),
            sub: sub.to_string(),
            enabled: String::new(),
            pid,
        });
    }
    out
}

/// OS 探测：跑 `uname`，按回显判三分法（与 process.rs 同串同口径）。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_os_maps_uname() {
        assert_eq!(classify_os("Linux\n"), Os::Linux);
        assert_eq!(classify_os("Darwin"), Os::MacOS);
        assert_eq!(classify_os("NONE"), Os::Windows);
        assert_eq!(classify_os("'uname' is not recognized"), Os::Windows);
    }

    #[test]
    fn units_parse_five_column_layout() {
        let text = "  sshd.service loaded active running OpenSSH daemon\n\
                     nginx.service  loaded active running nginx - high performance web server\n\
                     firewalld.service loaded inactive dead firewalld - dynamic firewall daemon\n";
        let v = parse_systemctl_units(text);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].name, "sshd.service");
        assert_eq!(v[0].loaded, "loaded");
        assert_eq!(v[0].state, "active");
        assert_eq!(v[0].sub, "running");
        assert_eq!(v[2].state, "inactive");
        assert_eq!(v[2].sub, "dead");
    }

    #[test]
    fn units_parse_legacy_four_column() {
        // 老 systemd 无 LOAD 列：第二列直接是 ACTIVE。
        let text = "crond.service active running Command Scheduler\n";
        let v = parse_systemctl_units(text);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].loaded, "loaded");
        assert_eq!(v[0].state, "active");
        assert_eq!(v[0].sub, "running");
    }

    #[test]
    fn units_skips_non_service_and_short_lines() {
        let text = "tmp.mount loaded active waiting /tmp\nnoise\n";
        assert!(parse_systemctl_units(text).is_empty());
    }

    #[test]
    fn enabled_and_pids_merge_by_name() {
        let mut svcs = parse_systemctl_units("sshd.service loaded active running OpenSSH daemon\n");
        let en = parse_enabled("sshd.service enabled enabled\ngunk not-a-unit whatever\n");
        merge_enabled(&mut svcs, &en);
        // pid 采集为远端 shell 产出的 `unit\037MainPID` 行，按单元名配对。
        let pids = parse_main_pids("sshd.service\u{1f}999\n");
        merge_pids(&mut svcs, &pids);
        assert_eq!(svcs[0].enabled, "enabled");
        assert_eq!(svcs[0].pid, 999);
        let mut s2 = parse_systemctl_units("crond.service loaded active running scheduler\n");
        merge_pids(&mut s2, &parse_main_pids("crond.service\u{1f}4242\n"));
        assert_eq!(s2[0].pid, 4242);
        // pid 段为空的单元记 0。
        let mut s3 = parse_systemctl_units("a.service loaded active running x\n");
        merge_pids(&mut s3, &parse_main_pids("a.service\u{1f}\n"));
        assert_eq!(s3[0].pid, 0);
    }

    #[test]
    fn sysv_parses_unit_separated() {
        let s = '\u{1f}';
        let text = format!(
            "sshd{s}running{s}1234\nnetwork{s}stopped{s}0\ncustom{s}{s}5\n",
            s = s
        );
        let v = parse_sysv(&text);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].name, "sshd.service");
        assert_eq!(v[0].state, "active");
        assert_eq!(v[0].sub, "running");
        assert_eq!(v[0].pid, 1234);
        assert_eq!(v[1].state, "inactive");
        assert_eq!(v[2].state, ""); // 未知如实留空
        assert_eq!(v[2].pid, 5);
    }

    #[test]
    fn validate_unit_whitelists_and_scopes() {
        assert_eq!(validate_unit("nginx").unwrap(), "nginx.service");
        assert_eq!(validate_unit("sshd.service").unwrap(), "sshd.service");
        assert_eq!(validate_unit("  dbus@1.service ").unwrap(), "dbus@1.service");
        assert_eq!(validate_unit("a+b-c:d.service").unwrap(), "a+b-c:d.service");
        // 注入与跨单元类型一律拒绝
        assert!(validate_unit("nginx; rm -rf /").is_err());
        assert!(validate_unit("nginx && whoami").is_err());
        assert!(validate_unit("$(reboot)").is_err());
        assert!(validate_unit("").is_err());
        assert!(validate_unit("foo.mount").is_err());
        assert!(validate_unit("foo.timer").is_err());
        assert!(validate_unit("foo.socket").is_err());
        // 先补 .service 再判后缀的老写法会把 `foo.mount` 变成 `foo.mount.service` 蒙混过关；
        // 现在判据取补后缀前的原始名，非 service 后缀必拒。
        assert!(validate_unit(".service").is_err());
        assert!(validate_unit(".mount").is_err());
        assert_eq!(validate_unit("foo.bar.service").unwrap(), "foo.bar.service");
    }

    #[test]
    fn control_commands_stay_in_policy_semantics() {
        // 组装出的写命令必须落在内置分级里（非 Blocked 才允许执行；回归策略表改动）。
        for cmd in [
            "systemctl start nginx.service",
            "systemctl stop nginx.service",
            "systemctl restart sshd.service",
            "service nginx.service restart",
            "launchctl start com.apple.foo",
            "launchctl stop com.apple.foo",
        ] {
            let v = PolicyEngine::default().classify(cmd);
            assert_ne!(v.decision, Decision::Blocked, "{cmd} 不应被内置黑名单拦截");
        }
        // macOS 合并核验串同样不得被策略误拦。
        for cmd in [
            macos_ctl_cmd("stop", "homebrew.mxcl.redis", false),
            macos_ctl_cmd("start", "homebrew.mxcl.redis", true),
        ] {
            let v = PolicyEngine::default().classify(&cmd);
            assert_ne!(v.decision, Decision::Blocked, "{cmd} 不应被内置黑名单拦截");
        }
    }

    #[test]
    fn macos_ctl_cmd_embeds_launchctl_and_verification() {
        let stop = macos_ctl_cmd("stop", "homebrew.mxcl.redis", false);
        assert!(stop.starts_with("launchctl stop homebrew.mxcl.redis"));
        assert!(stop.contains("sleep 2"));
        // 核验必须用不带 label 的全列表 + 第三列精确匹配（macOS 26+ 的 `launchctl list <label>`
        // 输出属性而非列表行，按 NR>1 取第一列会把键名误当 PID）。
        assert!(stop.contains("launchctl list 2>/dev/null | awk"));
        assert!(stop.contains("$3==L"));
        assert!(stop.contains("UNSETTLED:running:$p"));
        let start = macos_ctl_cmd("start", "homebrew.mxcl.redis", true);
        assert!(start.starts_with("launchctl start homebrew.mxcl.redis"));
        assert!(start.contains("UNSETTLED:stopped"));
    }

    #[test]
    fn svc_verdict_parses_settled_unsettled_unknown() {
        assert_eq!(
            parse_svc_verdict("SETTLED"),
            SvcResult { settled: Some(true), state: None }
        );
        assert_eq!(
            parse_svc_verdict("UNSETTLED:running:25739"),
            SvcResult { settled: Some(false), state: Some("running:25739".into()) }
        );
        assert_eq!(
            parse_svc_verdict("UNSETTLED:stopped"),
            SvcResult { settled: Some(false), state: Some("stopped".into()) }
        );
        assert_eq!(
            parse_svc_verdict(""),
            SvcResult { settled: None, state: None }
        );
    }

    #[test]
    fn launchctl_parses_pid_status_label() {
        let text = "PID\tStatus\tLabel\n\
                    -\t0\tcom.apple.notrunning\n\
                    123\t0\tcom.apple.running\n\
                    -\t1\tcom.apple.crashed\n";
        let v = parse_launchctl(text);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].name, "com.apple.notrunning");
        assert_eq!(v[0].state, "inactive");
        assert_eq!(v[0].pid, 0);
        assert_eq!(v[1].state, "active");
        assert_eq!(v[1].sub, "running");
        assert_eq!(v[1].pid, 123);
        assert_eq!(v[2].state, "failed");
        assert_eq!(v[2].enabled, ""); // 自启列首版留空
        // 畸形/噪声行（首列非数字且非 `-`）与表头行都不产幽灵记录。
        assert!(parse_launchctl("noise\nonly two fields\nPID\tStatus\tLabel\n").is_empty());
    }

    #[test]
    fn validate_label_scopes_launchd_names() {
        assert_eq!(
            validate_label("com.apple.foo").unwrap(),
            "com.apple.foo"
        );
        assert_eq!(
            validate_label(" homebrew.mxcl.nginx ").unwrap(),
            "homebrew.mxcl.nginx"
        );
        assert_eq!(validate_label("a-b_c").unwrap(), "a-b_c");
        // 注入 / 选项伪装 / 非法字符一律拒绝
        assert!(validate_label("foo; rm -rf /").is_err());
        assert!(validate_label("$(reboot)").is_err());
        assert!(validate_label("--help").is_err());
        assert!(validate_label(".hidden").is_err());
        assert!(validate_label("").is_err());
        assert!(validate_label("foo/bar").is_err());
    }
}
