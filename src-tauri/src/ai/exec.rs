//! AI 命令执行原语（`run_command` 的底座、`Executor` 节点）。
//!
//! 在**已认证**的 SSH 连接上另开一条 `exec` 通道执行单条命令，捕获 stdout/stderr/exit_code，
//! 带超时与输出上限。本地终端没有 SSH 通道：走 [`run_local`] 在本机 spawn
//! PowerShell/sh 收集输出，语义一致。与交互式 PTY 严格分离——绝不用 `SessionCmd::Write` 往
//! 用户正在看的终端里塞命令（那会污染会话、且无法回收输出），也绝不二次认证（复用会话
//! 句柄）。
//! 会话一断，句柄即失效，此时返回可读错误引导重连（对齐 [`crate::files::sftp::backend`]）。
//!
//! 安全边界：本函数只做「执行 + 捕获」，**不做**策略判断。调用方（`ai/tools.rs` 的
//! `run_command`）必须先过 [`crate::ai::policy::PolicyEngine`] 裁决与人工审批才落到这里；
//! 返回的是**原始未脱敏**输出，出网/回填 LLM 前仍须调用方过 Sanitizer。
//! 资源兜底在本层：超时中断 + 单流输出上限，防止 `cat 大文件`/`yes` 撑爆内存或长期占通道。

use std::time::{Duration, Instant};

use russh::ChannelMsg;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::error::AppError;
use crate::session::{SessionKind, SshHandle};
use crate::state::AppState;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// 未指定超时（`timeout_ms == 0`）时的默认值（默认 30s）。
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// 超时上限：AI 传入过大值也夹紧，防止一条命令长期占住通道（「可配」的兜底）。
const MAX_TIMEOUT_MS: u64 = 300_000;
/// 单条流（stdout/stderr）捕获上限。超出即停止追加并置 `truncated`；
/// 面向 LLM 上下文的进一步截断在 c4（token 预算）里做，这里只防内存失控。
const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

/// 一次命令执行的原始结果（未脱敏，调用方出网前须过 Sanitizer）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    /// 标准输出（UTF-8 有损转换；二进制输出里的非法字节替换为 U+FFFD）。
    pub stdout: String,
    /// 标准错误（SSH `ExtendedData` ext==1，RFC 4254）。
    pub stderr: String,
    /// 远端回传的退出码；命令未正常收尾（超时/连接中断）时为 `None`。
    pub exit_code: Option<i32>,
    /// 是否因超时被中断；此时 stdout/stderr 为已捕获的部分输出。
    pub timed_out: bool,
    /// 任一流是否因超过 [`MAX_CAPTURE_BYTES`] 被截断。
    pub truncated: bool,
    /// 执行耗时（毫秒）。
    pub duration_ms: u64,
}

/// 在会话上执行 `command`：本地会话在本机跑，其余走既有 SSH 连接的 `exec` 通道。
///
/// `timeout_ms` 为 0 时用 [`DEFAULT_TIMEOUT_MS`]，并夹紧到 [`MAX_TIMEOUT_MS`]。
/// 会话不存在 → [`AppError::NoSuchSession`]；已断开 → [`AppError::Ai`]（引导重连）。
pub async fn exec_command(
    app: &AppHandle,
    session_id: &str,
    command: &str,
    timeout_ms: u64,
) -> Result<ExecResult, AppError> {
    let timeout = Duration::from_millis(clamp_timeout(timeout_ms));
    // 本地终端没有 SSH 通道：在本机直接执行（Windows 走 powershell -Command）
    if app.state::<AppState>().sessions.kind(session_id) == Some(SessionKind::Local) {
        return run_local(command, timeout).await;
    }
    let handle = session_handle(app, session_id)?;
    run(&handle, command, timeout).await
}

/// 取会话的已认证句柄；区分「不存在」与「已断开/不支持」（后者可直接引导用户）。
fn session_handle(app: &AppHandle, session_id: &str) -> Result<SshHandle, AppError> {
    let st = app.state::<AppState>();
    match st.sessions.handle(session_id) {
        Some(h) => Ok(h),
        None if st.sessions.contains(session_id) => Err(match st.sessions.kind(session_id) {
            Some(SessionKind::Ftp) | Some(SessionKind::Ftps) => {
                AppError::Ai("FTP 会话不支持执行 shell 命令".into())
            }
            Some(SessionKind::Telnet) | Some(SessionKind::Serial) | Some(SessionKind::Mosh) => {
                AppError::Ai("该会话类型不支持后台执行命令，请在终端内直接输入".into())
            }
            _ => AppError::Ai("连接已断开，请先重连后再执行命令".into()),
        }),
        None => Err(AppError::NoSuchSession(session_id.to_string())),
    }
}

/// 本地会话（[`SessionKind::Local`]）没有 SSH 通道：在本机直接执行命令。
///
/// Windows 走 `powershell -NoProfile -NonInteractive -Command`（先强制 UTF-8 输出，避免中文
/// 系统 GBK 输出乱码进 LLM），Unix 走 `/bin/sh -c`。超时/输出上限/退出码语义与 SSH exec 对齐；
/// stdout/stderr 用双线程排空（防写满管道缓冲死锁），主线程轮询 `try_wait`，超时 `kill` 收尸。
async fn run_local(command: &str, timeout: Duration) -> Result<ExecResult, AppError> {
    let started = Instant::now();
    let command = command.to_string();
    let (stdout, stderr, exit_code, timed_out, spawn_err) =
        tokio::task::spawn_blocking(move || {
            #[cfg(windows)]
            let mut cmd = {
                let sys =
                    std::env::var("SystemRoot").unwrap_or_else(|_| String::from(r"C:\Windows"));
                let ps = std::path::Path::new(&sys)
                    .join("System32")
                    .join("WindowsPowerShell")
                    .join("v1.0")
                    .join("powershell.exe");
                let wrapped = wrap_local_command(&command);
                let mut c = std::process::Command::new(ps);
                c.args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    wrapped.as_str(),
                ]);
                c
            };
            #[cfg(not(windows))]
            let mut cmd = {
                let mut c = std::process::Command::new("/bin/sh");
                c.args(["-c", command.as_str()]);
                c
            };
            cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            #[cfg(windows)]
            {
                // GUI 进程拉起控制台程序：不弹黑窗
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    return (
                        String::new(),
                        String::new(),
                        None,
                        false,
                        Some(format!("无法在本机执行命令：{e}")),
                    )
                }
            };
            // 双线程排空管道，防输出写满 64K 管道缓冲死锁；主线程轮询超时 → kill
            let (tx_out, rx_out) = std::sync::mpsc::channel::<Vec<u8>>();
            let (tx_err, rx_err) = std::sync::mpsc::channel::<Vec<u8>>();
            if let Some(mut so) = child.stdout.take() {
                std::thread::spawn(move || {
                    use std::io::Read;
                    let mut buf = Vec::new();
                    let _ = so.read_to_end(&mut buf);
                    let _ = tx_out.send(buf);
                });
            } else {
                let _ = tx_out.send(Vec::new());
            }
            if let Some(mut se) = child.stderr.take() {
                std::thread::spawn(move || {
                    use std::io::Read;
                    let mut buf = Vec::new();
                    let _ = se.read_to_end(&mut buf);
                    let _ = tx_err.send(buf);
                });
            } else {
                let _ = tx_err.send(Vec::new());
            }
            let deadline = Instant::now() + timeout;
            let mut exit_code = None;
            let mut timed_out = false;
            loop {
                match child.try_wait() {
                    Ok(Some(st)) => {
                        exit_code = st.code();
                        break;
                    }
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            timed_out = true;
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(e) => {
                        return (
                            String::new(),
                            String::new(),
                            None,
                            false,
                            Some(format!("本机执行失败：{e}")),
                        )
                    }
                }
            }
            // 收尾：等读线程把最后一段交回来（kill 后管道立即 EOF，短暂阻塞可接受）
            let out = rx_out.recv().unwrap_or_default();
            let err = rx_err.recv().unwrap_or_default();
            (
                String::from_utf8_lossy(&out).into_owned(),
                String::from_utf8_lossy(&err).into_owned(),
                exit_code,
                timed_out,
                None,
            )
        })
        .await
        .map_err(|e| AppError::Internal(format!("本机执行任务异常终止：{e}")))?;

    if let Some(e) = spawn_err {
        return Err(AppError::Ai(e));
    }
    let mut out = CappedBuf::default();
    out.push(stdout.as_bytes());
    let mut err = CappedBuf::default();
    err.push(stderr.as_bytes());
    let truncated = out.truncated || err.truncated;
    Ok(ExecResult {
        stdout: out.into_string(),
        stderr: err.into_string(),
        exit_code,
        timed_out,
        truncated,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// Windows 下强制 PowerShell 以 UTF-8 输出（外部命令经 `chcp 65001` 也走 UTF-8），
/// 中文系统默认 GBK 输出会乱码进 LLM。
/// 注意 `chcp 65001>$null` 在 PS 5.1 会把 `65001>$null` 当参数传给 chcp（不支持数字流
/// 重定向），产生 stderr 告警；用 `| Out-Null` 抑制。
#[cfg(windows)]
fn wrap_local_command(command: &str) -> String {
    format!(
        "$ProgressPreference='SilentlyContinue';[Console]::OutputEncoding=[Text.UTF8Encoding]::new();chcp 65001 | Out-Null;{command}"
    )
}

/// 0 → 默认；其余夹紧到上限，避免异常大的超时把通道长期占住。
const fn clamp_timeout(ms: u64) -> u64 {
    if ms == 0 {
        DEFAULT_TIMEOUT_MS
    } else if ms > MAX_TIMEOUT_MS {
        MAX_TIMEOUT_MS
    } else {
        ms
    }
}

/// 开通道 → `exec` → 收流直到 `ExitStatus`/`Close`/`Eof`/通道关闭或超时。
async fn run(handle: &SshHandle, command: &str, timeout: Duration) -> Result<ExecResult, AppError> {
    let started = Instant::now();
    // 开通道 + exec 请求也纳入超时：russh 的 channel_open_session / exec 在半死连接上会无限
    // await（与 SFTP 建通道同一类坑）。不兜住，卡死的连接会把 AI run_command、目录打包的远端
    // 探测/解包（都走 exec_command）一起拖死且永不报错、永不回退。
    let mut channel = tokio::time::timeout(timeout, async {
        let channel = handle.channel_open_session().await?;
        // want_reply=true：等服务端确认受理 exec 请求（命令不存在/被拒会在此返回 Err）。
        channel.exec(true, command).await?;
        Ok::<_, AppError>(channel)
    })
    .await
    .map_err(|_| AppError::Ai("命令打开通道超时，连接可能已失效，请重连后再试".into()))??;

    let mut out = CappedBuf::default();
    let mut err = CappedBuf::default();
    let mut exit_code: Option<i32> = None;

    // 采集循环借可变引用捕获 channel/out/err/exit_code；timeout 消费该 async 块，
    // await 结束即释放借用，故超时后仍可 channel.close() 并读出部分输出。
    let drained = tokio::time::timeout(timeout, async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => out.push(&data),
                // ext==1 是 stderr；其余扩展数据类型本项目不关心
                Some(ChannelMsg::ExtendedData { data, ext: 1 }) => err.push(&data),
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    exit_code = Some(exit_status as i32)
                }
                // Close / 通道彻底关闭（None）是终态：exit-status 按 RFC 一定先于 Close，到此必已收全。
                Some(ChannelMsg::Close) | None => break,
                // Eof 仅表示对端不再发数据，不保证 exit-status 已送达——EOF 与 ExitStatus 的先后
                // 顺序各服务端/负载下不固定（tar 解包这类输出多、耗时长的命令更易 EOF 抢先）。
                // 已拿到退出码即结束，否则继续等 Close，避免把 exit_code 误判成 None（打包回退根因）。
                Some(ChannelMsg::Eof) => {
                    if exit_code.is_some() {
                        break;
                    }
                }
                _ => {}
            }
        }
    })
    .await
    .is_ok();

    // 超时：主动关通道，避免远端命令继续跑、占用服务端资源（关闭错误无需上抛）。
    if !drained {
        let _ = channel.close().await;
    }

    let truncated = out.truncated || err.truncated;
    Ok(ExecResult {
        stdout: out.into_string(),
        stderr: err.into_string(),
        exit_code,
        timed_out: !drained,
        truncated,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

/// 带上限的字节累加器：超过 [`MAX_CAPTURE_BYTES`] 后停止追加并置 `truncated`。
#[derive(Default)]
struct CappedBuf {
    buf: Vec<u8>,
    truncated: bool,
}

impl CappedBuf {
    fn push(&mut self, data: &[u8]) {
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(self.buf.len());
        if data.len() <= remaining {
            self.buf.extend_from_slice(data);
        } else {
            self.buf.extend_from_slice(&data[..remaining]);
            self.truncated = true;
        }
    }

    fn into_string(self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_buf_accumulates_under_limit() {
        let mut b = CappedBuf::default();
        b.push(b"hello ");
        b.push(b"world");
        assert!(!b.truncated);
        assert_eq!(b.into_string(), "hello world");
    }

    #[test]
    fn capped_buf_stops_at_limit_and_flags_truncation() {
        let mut b = CappedBuf::default();
        b.push(&vec![b'x'; MAX_CAPTURE_BYTES + 100]);
        assert!(b.truncated);
        assert_eq!(b.into_string().len(), MAX_CAPTURE_BYTES);
    }

    #[test]
    fn capped_buf_exact_fill_is_not_truncated() {
        let mut b = CappedBuf::default();
        b.push(&vec![b'y'; MAX_CAPTURE_BYTES]);
        assert!(!b.truncated);
        // 再推一字节即触发截断
        b.push(b"z");
        assert!(b.truncated);
    }

    #[test]
    fn capped_buf_decodes_utf8_lossily() {
        let mut b = CappedBuf::default();
        // 非法 UTF-8 前缀 + 合法尾部：有损解码不应 panic，尾部可读
        b.push(&[0xff, 0xfe, b'o', b'k']);
        assert!(b.into_string().ends_with("ok"));
    }

    #[test]
    fn clamp_timeout_defaults_and_caps() {
        assert_eq!(clamp_timeout(0), DEFAULT_TIMEOUT_MS);
        assert_eq!(clamp_timeout(5_000), 5_000);
        assert_eq!(clamp_timeout(MAX_TIMEOUT_MS), MAX_TIMEOUT_MS);
        assert_eq!(clamp_timeout(MAX_TIMEOUT_MS + 1), MAX_TIMEOUT_MS);
    }
}
