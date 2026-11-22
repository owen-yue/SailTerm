//! 本地终端会话：用 portable-pty 在本机开一个 shell，接入与 SSH
//! **完全相同**的会话 actor 模型——同一套 `session-output` 事件、[`SessionCmd`] 指令通道、
//! [`TerminalRing`] 回滚缓冲、前端 `Terminal.tsx`；前端只多了一种 `kind: "local"` 的标签。
//!
//! 与 SSH 会话的三点差异：
//! - 无握手 / 无凭证 / 无主机密钥裁决（纯本机子进程），`handle`/`ftp` 恒 `None`；
//! - portable-pty 的 reader/writer 是**阻塞** std::io：读循环跑在 `spawn_blocking` 线程池，
//!   写/改窗口/kill 由 async 指令循环驱动（键入是小而稀疏的管道写，同步执行即可）；
//! - “断线”= shell 进程退出：镜像 SSH 的断线语义（保留标签 + 转 `Disconnected` + 提供重连），
//!   “重连”= 用同一 shell 配置重开一个进程，沿用同一 `session_id` 与终端环形缓冲。
//!
//! 安全：本地终端不涉及任何凭证，天然满足 （无明文可泄漏）。

use std::io::{Read, Write};
use std::sync::Arc;

use base64::Engine;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot};

use crate::error::AppError;
use crate::session::{
    discard, emit_state, teardown, Reconnect, SessionCmd, SessionEntry, SessionKind, SessionState,
    TerminalRing,
};
use crate::ssh::EVENT_OUTPUT;
use crate::state::AppState;

/// 终端输出事件负载（与 SSH 会话同构，前端按 `sessionId` 路由到对应 xterm）。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Output<'a> {
    session_id: &'a str,
    data: &'a str,
}

/// 一次 `openpty` + `spawn shell` 的产物；四件套分属读循环与指令循环。
pub(crate) struct PtyBundle {
    /// 保留 master 以便 `resize`；drop 它即关闭 PTY。
    pub(crate) master: Box<dyn MasterPty + Send>,
    /// 子 shell 进程：用户关闭时 kill，退出时 wait 收尸。
    pub(crate) child: Box<dyn Child + Send>,
    pub(crate) reader: Box<dyn Read + Send>,
    pub(crate) writer: Box<dyn Write + Send>,
}

/// 建立本地终端会话：开 PTY + spawn 本机 shell，注册到会话表，返回 `session_id`。
///
/// `shell` 为 `None` 时用平台默认 shell（Windows: PowerShell；Unix: `$SHELL`/bash）。
pub async fn connect(
    app: AppHandle,
    cols: u32,
    rows: u32,
    shell: Option<String>,
) -> Result<String, AppError> {
    // ConPTY 偶发起不来（句柄未释放等）：短退避重试两次，减少「多开几次才连上」
    let mut last_err = None;
    let mut bundle = None;
    for i in 0..3 {
        match open_pty(cols, rows, shell.clone()).await {
            Ok(b) => {
                bundle = Some(b);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(80 * (i + 1) as u64)).await;
            }
        }
    }
    let bundle = bundle
        .ok_or_else(|| last_err.unwrap_or_else(|| AppError::Io("无法启动本地终端".into())))?;
    let sessions = &app.state::<AppState>().sessions;
    let id = sessions.next_id();
    let (tx, rx) = mpsc::channel(64);
    let kick = tx.clone();
    // 终端环形缓冲：一份入注册表（供 AI 快照读取），一份交给读循环持续 push
    let ring = TerminalRing::new();
    sessions.insert(SessionEntry {
        id: id.clone(),
        label: "本地终端".to_string(),
        host: "localhost".to_string(),
        profile_id: None,
        state: SessionState::Connected,
        kind: SessionKind::Local,
        tx: Some(tx),
        // 本地会话无 SSH 连接句柄、无 FTP 控制连接
        handle: None,
        jumps: Vec::new(),
        ftp: None,
        ftp_epoch: 0,
        reconnect: Reconnect::Local { shell },
        ring: ring.clone(),
        port: 0,
        latency_ms: None,
        // 非 SSH 协议无协商快照（FTP 有自己的一份，见 files::ftp 的 FtpInfo）
        negotiation: None,
        ftp_info: None,
    });
    // 初始建连不发 session-state：命令直接回传 DTO，前端 open() 已据此建标签（同 SSH/FTP）
    spawn_actor(app.clone(), id.clone(), bundle, rx, ring);
    debug!("[local] session {id} connected");
    // 冷启动怪癖：reader 起来后再踢一次 resize，首连才容易出提示符。
    // 不踢回车：\r 会执行空行多画一个提示符，裸 \n 又会被 PowerShell 当行内字符
    // 吃进 PSReadLine 多行编辑，显示成未闭合的 `>>` 续行提示。
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let _ = kick.try_send(SessionCmd::Resize(80, 24));
    });
    Ok(id)
}

/// “重连”本地会话 = 用同一 shell 配置重开一个进程，沿用同一 `session_id` 与终端环形缓冲
/// （重连后终端内容保留）。
pub async fn reconnect(
    app: AppHandle,
    id: String,
    shell: Option<String>,
    cols: u32,
    rows: u32,
) -> Result<(), AppError> {
    // 先开 PTY 再改注册表：openpty 失败时会话仍停在 Disconnected，不留半初始化状态
    let bundle = open_pty(cols, rows, shell).await?;
    let (tx, rx) = mpsc::channel(64);
    let ring = app
        .state::<AppState>()
        .sessions
        .update(&id, |e| {
            e.tx = Some(tx);
            e.state = SessionState::Connected;
            e.ring.clone()
        })
        .ok_or_else(|| AppError::NoSuchSession(id.clone()))?;
    emit_state(&app, &id, SessionState::Connected, None);
    spawn_actor(app.clone(), id, bundle, rx, ring);
    Ok(())
}

/// 预热 Windows ConPTY：首次 openpty 往往最慢/最不稳，启动时先建一对 PTY 再丢弃。
///
/// **不再 spawn `cmd.exe`**：打包安装后从 GUI 进程拉起 cmd 会弹
/// `cmd.exe - 应用程序无法正常启动(0xc0000142)`（DLL 初始化失败），
/// 空跑 openpty 足够把 ConPTY 路径预热，且无副作用。
pub fn warmup_conpty() {
    #[cfg(windows)]
    {
        use portable_pty::{native_pty_system, PtySize};
        if let Ok(pair) = native_pty_system().openpty(PtySize {
            rows: 2,
            cols: 2,
            pixel_width: 16,
            pixel_height: 32,
        }) {
            drop(pair);
        }
    }
}

/// 屏蔽子进程的系统错误框（含 0xc0000142 这类 Application Error）。
/// GUI 应用 spawn console 子进程时 Windows 常弹这种框，对工具类应用是噪音。
#[cfg(windows)]
pub fn suppress_child_error_dialogs() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // kernel32!SetErrorMode：进程级，只影响本进程后续创建的子进程
        extern "system" {
            fn SetErrorMode(u_mode: u32) -> u32;
        }
        const SEM_FAILCRITICALERRORS: u32 = 0x0001;
        const SEM_NOGPFAULTERRORBOX: u32 = 0x0002;
        const SEM_NOOPENFILEERRORBOX: u32 = 0x8000;
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
    });
}

#[cfg(not(windows))]
pub fn suppress_child_error_dialogs() {}

/// 解析用户指定的 shell：空 → 默认；**路径不存在则忽略**（防设置里残留坏路径导致永远起不来）。
fn resolve_shell(shell: Option<&str>) -> Option<String> {
    let s = shell
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)?;
    let exists = std::path::Path::new(&s).is_file() || which_exists(&s);
    if !exists {
        debug!("[local] ignore invalid shell in settings: {s:?}");
        return None;
    }
    Some(s)
}

/// PATH 上是否能找到该可执行名。
fn which_exists(name: &str) -> bool {
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(name);
            if p.is_file() {
                return true;
            }
            #[cfg(windows)]
            {
                let mut e = dir.join(name);
                e.set_extension("exe");
                if e.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

/// 该候选是否为 zsh（按可执行文件名判定，兼容 `/bin/zsh`、`zsh`、`zsh.exe`）。
fn is_zsh(prog: &str) -> bool {
    std::path::Path::new(prog)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("zsh"))
        .unwrap_or(false)
}

/// 该 shell 需要的启动参数：仅 zsh 关掉 PROMPT_SP（否则首帧会在提示符前补打游离的行尾 `%`）。
/// 显式指定与 `$SHELL` 兜底两条路径共用，避免只对前者生效、macOS 默认 zsh 仍漏掉。
fn shell_args(prog: &str) -> Vec<String> {
    if is_zsh(prog) {
        vec!["-o".to_string(), "no_prompt_sp".to_string()]
    } else {
        Vec::new()
    }
}

/// Windows 上 `wsl.exe` 的绝对路径（System32），取不到 SystemRoot 时退回裸名靠 PATH。
#[cfg(windows)]
fn wsl_program() -> String {
    std::env::var("SystemRoot")
        .map(|r| {
            std::path::PathBuf::from(r)
                .join("System32")
                .join("wsl.exe")
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|_| "wsl.exe".to_string())
}

/// 判断某发行版是否为「可供交互登录」的 shell。
/// 排除 Docker Desktop 的后端数据发行版（`docker-desktop` / `docker-desktop-data`）：
/// 它们由 `wsl -l -q` 一并列出，但没有可供登录的用户 shell，选进终端只会得到空白/异常画面。
#[cfg(windows)]
fn is_interactive_distro(name: &str) -> bool {
    !name.eq_ignore_ascii_case("docker-desktop")
        && !name.eq_ignore_ascii_case("docker-desktop-data")
}

/// WSL 检测结果：`installed` = WSL 引擎存在（`wsl -l -q` 能成功跑起来）；
/// `distros` = 已装的**可交互**发行版名（已剔除 docker-desktop 这类非交互后端发行版）。
/// 前端据 `installed` 决定是否显示选择器（哪怕没有可交互发行版也让入口可见），据 `distros` 填充具体选项。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WslInfo {
    pub installed: bool,
    pub distros: Vec<String>,
}

/// 探测本机 WSL 引擎与可交互发行版。非 Windows / 未装 / 命令失败一律 installed=false、distros 空表。
pub fn wsl_info() -> WslInfo {
    #[cfg(windows)]
    {
        let out = std::process::Command::new(wsl_program())
            .args(["-l", "-q"])
            .output();
        match out {
            Ok(o) if o.status.success() => WslInfo {
                installed: true,
                distros: decode_wsl(&o.stdout)
                    .lines()
                    .map(|l| l.trim().trim_matches('\u{feff}'))
                    .filter(|l| !l.is_empty() && is_interactive_distro(l))
                    .map(|l| l.to_string())
                    .collect(),
            },
            _ => WslInfo {
                installed: false,
                distros: Vec::new(),
            },
        }
    }
    #[cfg(not(windows))]
    {
        WslInfo {
            installed: false,
            distros: Vec::new(),
        }
    }
}

/// `wsl --list` 系列在多数 Windows 版本输出 UTF-16LE（可能带 0xFF 0xFE BOM），少数走本地代码页。
/// 按 BOM / 0 字节占比判定 UTF-16LE，否则按 UTF-8 有损解码（避免乱码把发行版名读砸）。
#[cfg(windows)]
fn decode_wsl(bytes: &[u8]) -> String {
    let bom = bytes.starts_with(&[0xFF, 0xFE]);
    let zeros = bytes.iter().filter(|b| **b == 0).count();
    if bom || (bytes.len() >= 4 && zeros >= bytes.len() / 4) {
        let body = if bom { &bytes[2..] } else { bytes };
        let units: Vec<u16> = body
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// `openpty` + `spawn shell`（对齐 portable-pty 官方 example：先 reader、立刻 drop slave、resize 踢 ConPTY）。
async fn open_pty(cols: u32, rows: u32, shell: Option<String>) -> Result<PtyBundle, AppError> {
    tokio::task::spawn_blocking(move || {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: rows as u16,
                cols: cols as u16,
                // ConPTY 对 0 尺寸偶发异常，给一个与单元格无关的非零像素值
                pixel_width: (cols as u16).saturating_mul(8),
                pixel_height: (rows as u16).saturating_mul(16),
            })
            .map_err(|e| AppError::Io(format!("无法创建本地 PTY：{e}")))?;

        // 候选 (程序, 参数)。WSL 入口用 "wsl:<发行版>" 前缀编码在 shell 串里
        // （"wsl:" = 默认发行版；"wsl:Ubuntu" = 指定），转成 wsl.exe + ["-d", distro]。
        // token 不是文件路径，故在 resolve_shell 的存在性校验之前拦截。
        let mut candidates: Vec<(String, Vec<String>)> = Vec::new();
        let token = shell.as_deref().map(str::trim).filter(|s| !s.is_empty());
        #[cfg(windows)]
        if let Some(rest) = token.and_then(|t| t.strip_prefix("wsl:")) {
            let distro = rest.trim();
            let mut args: Vec<String> = Vec::new();
            if !distro.is_empty() {
                args.push("-d".to_string());
                args.push(distro.to_string());
            }
            candidates.push((wsl_program(), args));
        }

        if candidates.is_empty() {
            if let Some(s) = resolve_shell(token) {
                // zsh 默认开 PROMPT_SP：首帧会在提示符前补打行尾 `%`，仅对 zsh 关掉；其它 shell 无此选项。
                candidates.push((s.clone(), shell_args(&s)));
            } else {
                #[cfg(windows)]
                {
                    // 优先绝对路径（System32），避免 PATH 劫持 / 当前目录假 exe
                    let sys = std::env::var("SystemRoot")
                        .map(|r| std::path::PathBuf::from(r).join("System32"))
                        .unwrap_or_else(|_| std::path::PathBuf::from(r"C:\Windows\System32"));
                    candidates.push((
                        sys.join("WindowsPowerShell")
                            .join("v1.0")
                            .join("powershell.exe")
                            .to_string_lossy()
                            .into_owned(),
                        vec![],
                    ));
                    candidates.push((sys.join("cmd.exe").to_string_lossy().into_owned(), vec![]));
                    if let Ok(c) = std::env::var("COMSPEC") {
                        if !c.trim().is_empty() {
                            candidates.push((c, vec![]));
                        }
                    }
                    candidates.push(("cmd.exe".to_string(), vec![]));
                    candidates.push(("powershell.exe".to_string(), vec![]));
                }
                #[cfg(not(windows))]
                {
                    if let Ok(s) = std::env::var("SHELL") {
                        let s = s.trim().to_string();
                        if !s.is_empty() {
                            // macOS 默认 $SHELL=/bin/zsh：兜底路径同样要关 PROMPT_SP，否则首帧提示符前补打游离 `%`。
                            candidates.push((s.clone(), shell_args(&s)));
                        }
                    }
                    candidates.push(("/bin/bash".to_string(), vec![]));
                    candidates.push(("/bin/sh".to_string(), vec![]));
                }
            }
        }

        let mut last = None;
        let mut child = None;
        for (prog, args) in &candidates {
            // ConPTY 交互 shell 不要加 /K 等参数：wezterm 也是裸程序名（WSL 的 -d 是唯一例外）
            let mut cb = CommandBuilder::new(prog);
            for a in args {
                cb.arg(a);
            }
            debug!("[local] spawn try: {prog:?} {args:?}");
            match pair.slave.spawn_command(cb) {
                Ok(ch) => {
                    debug!("[local] spawn ok: {prog:?}");
                    child = Some(ch);
                    break;
                }
                Err(e) => {
                    debug!("[local] spawn fail: {prog:?} -> {e}");
                    last = Some(e);
                }
            }
        }
        let child = child.ok_or_else(|| {
            AppError::Io(format!(
                "无法启动本地 shell：{}",
                last.map(|e| e.to_string()).unwrap_or_default()
            ))
        })?;

        // 官方 example：spawn 后立刻释放 slave 句柄
        drop(pair.slave);

        // 先克隆 reader，再 take writer（官方顺序）；writer 必须持有否则可能死锁
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AppError::Io(format!("无法获取 PTY 输出流：{e}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| AppError::Io(format!("无法获取 PTY 输入流：{e}")))?;

        // 踢一次 resize：ConPTY 常见怪癖——不 resize/不写就可能迟迟不吐提示符
        let _ = pair.master.resize(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: (cols as u16).saturating_mul(8),
            pixel_height: (rows as u16).saturating_mul(16),
        });

        Ok(PtyBundle {
            master: pair.master,
            child,
            reader,
            writer,
        })
    })
    .await
    .map_err(|e| AppError::Internal(format!("PTY 任务异常终止：{e}")))?
}

/// 启动本地会话 actor：读循环（阻塞 IO，独立 blocking 线程）+ 指令循环（async）。
///
/// 结构镜像 [`crate::ssh`] 的 SSH actor，只是把 `rd.wait()` 换成 blocking 读线程，
/// 用 `oneshot` 把“shell 退出”这一事件递回 async 指令循环。
pub(crate) fn spawn_actor(
    app: AppHandle,
    id: String,
    bundle: PtyBundle,
    mut rx: mpsc::Receiver<SessionCmd>,
    ring: Arc<TerminalRing>,
) {
    let PtyBundle {
        master,
        child,
        reader,
        writer,
    } = bundle;
    let (eof_tx, mut eof_rx) = oneshot::channel::<()>();

    // 读循环：阻塞读 PTY 输出 → 先入环形缓冲（AI 快照数据源）再 base64 广播给前端 xterm。
    // EOF/读错误都视为 shell 退出，收束后通过 oneshot 通知指令循环。
    {
        let app = app.clone();
        let id = id.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        ring.push(&buf[..n]);
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
                        let _ = app.emit(
                            EVENT_OUTPUT,
                            Output {
                                session_id: &id,
                                data: &b64,
                            },
                        );
                    }
                }
            }
            // 空屏兜底：shell 秒退时给一句可见原因
            if ring.is_empty() {
                let msg =
                    b"[local] shell exited immediately - try Settings > Terminal > local shell\r\n";
                ring.push(msg);
                let b64 = base64::engine::general_purpose::STANDARD.encode(msg);
                let _ = app.emit(
                    EVENT_OUTPUT,
                    Output {
                        session_id: &id,
                        data: &b64,
                    },
                );
            }
            let _ = eof_tx.send(());
        });
    }

    // 指令循环：键入写入 / 改窗口尺寸 / 用户关闭；并感知 shell 退出（EOF）。
    tokio::spawn(async move {
        let mut child = child;
        let mut writer = writer;
        // true = 用户主动关闭；false = shell 自行退出（可重连）
        let closed_by_user = loop {
            tokio::select! {
                _ = &mut eof_rx => break false,
                cmd = rx.recv() => match cmd {
                    // 键入是小而稀疏的管道写，同步写入即可（阻塞窗口极短，且读线程持续排空输出）
                    Some(SessionCmd::Write(bytes)) => { let _ = writer.write_all(&bytes); }
                    Some(SessionCmd::Resize(cols, rows)) => {
                        let _ = master.resize(PtySize {
                            rows: rows as u16,
                            cols: cols as u16,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                    }
                    // 终端内 ZMODEM 仅 SSH 通道支持，这里忽略
                    Some(SessionCmd::ZmodemStart { .. }) | Some(SessionCmd::ZmodemCancel) => {}
                    Some(SessionCmd::Close) | None => break true,
                },
            }
        };

        if closed_by_user {
            let _ = child.kill();
            let _ = child.wait();
            // discard 内含 teardown（本地会话无 SFTP/传输/隧道，等价于直接摘除）+ closed 广播
            discard(&app, &id);
        } else {
            // shell 已退出：wait 立即返回（收尸），保留标签转 Disconnected，前端给“重连”
            let _ = child.wait();
            teardown(&app, &id);
            app.state::<AppState>().sessions.update(&id, |e| {
                e.tx = None;
                e.state = SessionState::Disconnected;
            });
            emit_state(
                &app,
                &id,
                SessionState::Disconnected,
                Some("本地 shell 已退出"),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::resolve_shell;

    #[test]
    fn blank_shell_falls_back_to_platform_default() {
        // None / 空串 / 纯空白都交给 portable-pty 选默认 shell
        assert_eq!(resolve_shell(None), None);
        assert_eq!(resolve_shell(Some("")), None);
        assert_eq!(resolve_shell(Some("   \t ")), None);
    }

    #[test]
    fn specified_shell_is_trimmed_and_kept() {
        // 用当前可执行文件自身做「一定存在」的样本，验证只 trim 不改内容（不依赖具体 shell 名，跨平台确定）。
        let exe = std::env::current_exe().unwrap();
        let exe = exe.to_str().unwrap();
        assert_eq!(resolve_shell(Some(&format!("  {exe}  "))).as_deref(), Some(exe));
        assert_eq!(resolve_shell(Some(exe)).as_deref(), Some(exe));
        // 不存在的命令 → 忽略回落默认（None）：存在性门控，不因坏配置去 spawn
        assert_eq!(resolve_shell(Some("definitely-not-a-real-shell-xyz")), None);
    }
}
