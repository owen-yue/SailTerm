//! ZMODEM 传输：在既有 SSH 连接上跑远端 `sz`/`rz`，用 `zmodem2` 状态机收发文件。
//!
//! 复用已认证句柄开 **exec** 通道（不污染用户 PTY），二进制在通道上直接换手。
//! 安全：不经 AI、不落日志明文内容；路径由文件面板提供。

use std::path::{Path, PathBuf};

use russh::ChannelMsg;
use tauri::{AppHandle, Manager};
use zmodem2::{Action, Event, FileInfo, Position, Receiver, Sender};

use crate::error::AppError;
use crate::state::AppState;

/// 单次会话超时（秒）：大文件也应在合理时间内结束，卡死即中断。
const SESSION_SECS: u64 = 300;

/// 从远端 `sz` 下载到本地目录；返回落盘的文件名列表。
pub async fn download(
    app: AppHandle,
    session_id: String,
    remote_path: String,
    local_dir: String,
) -> Result<Vec<String>, AppError> {
    let handle = handle_of(&app, &session_id)?;
    let dest = PathBuf::from(&local_dir);
    std::fs::create_dir_all(&dest)?;

    let mut channel = handle.channel_open_session().await?;
    // -b binary；-- 后跟路径，避免被当成选项
    let cmd = format!("sz -b -- {}", shell_quote(&remote_path));
    channel.exec(true, cmd).await?;

    let mut rx = Receiver::new().map_err(zerr)?;
    let mut saved: Vec<String> = Vec::new();
    let mut current: Option<(Vec<u8>, PathBuf, std::fs::File)> = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(SESSION_SECS);

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                let _ = channel.close().await;
                return Err(AppError::Transfer("Zmodem 下载超时".into()));
            }
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    let _ = rx.submit_wire(&data).map_err(zerr)?;
                }
                Some(ChannelMsg::ExitStatus { .. }) | Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }

        // 泵协议：直到 Idle 再等下一块输入
        loop {
            match rx.poll() {
                Action::WriteWire(bytes) => {
                    let chunk = bytes.to_vec();
                    let n = chunk.len();
                    channel.data_bytes(chunk).await.map_err(zerr_io)?;
                    rx.wire_written(n);
                }
                Action::WriteFile(bytes) => {
                    let chunk = bytes.to_vec();
                    if let Some((_, _, f)) = current.as_mut() {
                        std::io::Write::write_all(f, &chunk)
                            .map_err(|e| AppError::Io(e.to_string()))?;
                    }
                    rx.file_written(chunk.len()).map_err(zerr)?;
                }
                Action::Event(ev) => match ev {
                    Event::FileStarted(info) => {
                        let name = String::from_utf8_lossy(info.name).trim().to_string();
                        let base = Path::new(&name)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "download.bin".into());
                        let path = dest.join(&base);
                        let f = std::fs::File::create(&path)?;
                        current = Some((info.name.to_vec(), path, f));
                    }
                    Event::FileCompleted => {
                        if let Some((_, name, f)) = current.take() {
                            drop(f);
                            saved.push(
                                name.file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                            );
                        }
                    }
                    Event::SessionCompleted | Event::Aborted => {
                        let _ = channel.close().await;
                        return Ok(saved);
                    }
                    _ => {}
                },
                Action::ReadFile { .. } | Action::Idle | _ => break,
            }
        }
    }
    let _ = channel.close().await;
    Ok(saved)
}

/// 从本地文件 `rz` 上传到远端目录。
pub async fn upload(
    app: AppHandle,
    session_id: String,
    local_path: String,
    remote_dir: String,
) -> Result<(), AppError> {
    let handle = handle_of(&app, &session_id)?;
    let path = PathBuf::from(&local_path);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .ok_or_else(|| AppError::Invalid("非法本地文件名".into()))?;
    let meta = std::fs::metadata(&path)?;
    let size = meta.len();
    let mut file = std::fs::File::open(&path)?;

    let mut channel = handle.channel_open_session().await?;
    let cmd = format!("cd {} && rz -b -y --", shell_quote(&remote_dir));
    channel.exec(true, cmd).await?;

    let mut tx = Sender::new().map_err(zerr)?;
    tx.start_file(FileInfo::new(
        name.as_bytes(),
        Some(Position::new(size as u32)),
    ))
    .map_err(zerr)?;
    tx.finish().map_err(zerr)?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(SESSION_SECS);

    loop {
        // 先把待写线数据送出去 / 补文件块，直到 Idle
        loop {
            match tx.poll() {
                Action::WriteWire(bytes) => {
                    let chunk = bytes.to_vec();
                    let n = chunk.len();
                    channel.data_bytes(chunk).await.map_err(zerr_io)?;
                    tx.wire_written(n);
                }
                Action::ReadFile { offset, max_len } => {
                    use std::io::{Read, Seek, SeekFrom};
                    file.seek(SeekFrom::Start(offset.get() as u64))
                        .map_err(|e| AppError::Io(e.to_string()))?;
                    let mut buf = vec![0u8; max_len.min(64 * 1024)];
                    let n = file
                        .read(&mut buf)
                        .map_err(|e| AppError::Io(e.to_string()))?;
                    tx.submit_file(&buf[..n]).map_err(zerr)?;
                }
                Action::Event(Event::SessionCompleted) | Action::Event(Event::Aborted) => {
                    let _ = channel.close().await;
                    return Ok(());
                }
                Action::Event(_) => {}
                Action::WriteFile(_) | Action::Idle => break,
                _ => break,
            }
        }

        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                let _ = channel.close().await;
                return Err(AppError::Transfer("Zmodem 上传超时".into()));
            }
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    tx.submit_wire(&data).map_err(zerr)?;
                }
                Some(ChannelMsg::ExitStatus { .. }) | Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                    let _ = channel.close().await;
                    return Ok(());
                }
                _ => {}
            }
        }
    }
}

fn zerr(e: zmodem2::Error) -> AppError {
    AppError::Transfer(format!("Zmodem 协议错误：{e:?}"))
}

fn zerr_io(e: russh::Error) -> AppError {
    AppError::Io(format!("Zmodem 通道写失败：{e}"))
}

// ---------- 终端内 ZMODEM（用户直接在 PTY 里敲 rz/sz，软件接管传输） ----------

/// 终端内 ZMODEM 方向：由握手帧的 hex 类型字节判定（抓包确认 lrzsz 3.x 格式）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ZmDir {
    /// `**\x18B01`（ZRINIT）：远端 `rz` 在等文件，本地选文件**上传**。
    Upload,
    /// `**\x18B00`（ZRQINIT）：远端 `sz` 要发文件，本地选目录**下载**。
    Download,
}

impl ZmDir {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Download => "download",
        }
    }
}

/// 在输出流里找 lrzsz 的 ZMODEM 握手头 `**\x18B`（ZPAD ZPAD ZDLE ZHEX），
/// 再按紧跟的 hex 类型字节判方向；找不到或类型不认识则 `None`（当普通文本处理）。
/// 返回 (帧起始位置, 方向)：调用方把 `pos` 之前的字节照常转发展示，`pos` 起进入 ZMODEM。
pub fn find_handshake(buf: &[u8]) -> Option<(usize, ZmDir)> {
    if buf.len() < 6 {
        return None;
    }
    for i in 0..=buf.len() - 6 {
        if &buf[i..i + 4] == b"**\x18B" {
            match (buf[i + 4], buf[i + 5]) {
                (b'0', b'1') => return Some((i, ZmDir::Upload)),
                (b'0', b'0') => return Some((i, ZmDir::Download)),
                _ => {}
            }
        }
    }
    None
}

/// 下载落盘句柄：`FileStarted` 时按对端给的文件名创建，`FileCompleted` 后换下一个。
pub(crate) struct RxSink {
    file: std::fs::File,
}

/// 终端内 ZMODEM 传输状态机：挂在会话 actor 的读写循环上，
/// 帧在**用户 PTY 通道**上双向换手（与面板右键版的开独立 exec 通道不同）。
/// 本地侧由 actor 每轮调 [`TermZm::pump`] 驱动，远端字节经 [`TermZm::feed`] 喂入。
pub enum TermZm {
    /// 上传（远端 `rz`）：`Sender` 读本地文件产帧。
    Tx {
        tx: Sender,
        file: std::fs::File,
        /// (文件名, 大小)：创建时即知，`take_file_info` 取出后供前端传输列表展示。
        file_info: Option<(String, u64)>,
    },
    /// 下载（远端 `sz`）：`Receiver` 收帧落盘到目标目录。
    Rx {
        rx: Receiver,
        dest: PathBuf,
        sink: Option<RxSink>,
        /// (文件名, 大小)：`FileStarted` 时才知（下载），供前端传输列表展示。
        file_info: Option<(String, u64)>,
        /// 状态机未消费的字节（CRC 一次只读 1 字节、帧被 SSH 分块时都会中途停）：
        /// 必须缓存到下一块数据到达后继续提交，丢弃会卡死握手。
        pending: Vec<u8>,
    },
}

impl TermZm {
    /// 上传：先喂首帧（ZRINIT）再建 `Sender`，准备推文件。
    pub fn upload(local_path: &Path, first: &[u8]) -> Result<Self, AppError> {
        let path = PathBuf::from(local_path);
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .ok_or_else(|| AppError::Invalid("非法本地文件名".into()))?;
        let meta = std::fs::metadata(&path)
            .map_err(|e| AppError::Io(format!("读取 {local_path:?} 失败：{e}")))?;
        let file = std::fs::File::open(&path)
            .map_err(|e| AppError::Io(format!("打开 {local_path:?} 失败：{e}")))?;
        let mut tx = Sender::new().map_err(zerr)?;
        tx.submit_wire(first).map_err(zerr)?;
        tx.start_file(FileInfo::new(
            name.as_bytes(),
            Some(Position::new(meta.len() as u32)),
        ))
        .map_err(zerr)?;
        tx.finish().map_err(zerr)?;
        Ok(Self::Tx {
            tx,
            file,
            file_info: Some((name, meta.len())),
        })
    }

    /// 下载：喂首帧（ZRQINIT）建 `Receiver`，文件名由对端 `FileStarted` 给出。
    pub fn download(dest_dir: &Path, first: &[u8]) -> Result<Self, AppError> {
        std::fs::create_dir_all(dest_dir)
            .map_err(|e| AppError::Io(format!("创建目录 {dest_dir:?} 失败：{e}")))?;
        let mut rx = Receiver::new().map_err(zerr)?;
        rx.submit_wire(first).map_err(zerr)?;
        Ok(Self::Rx {
            rx,
            dest: dest_dir.to_path_buf(),
            sink: None,
            file_info: None,
            pending: Vec::new(),
        })
    }

    /// 远端字节进来：交给状态机。返回 (需写回通道的字节, 是否完成, 状态机不消费的
    /// 剩余字节——只在完成后返回，是协议尾气 + shell 回显，调用方应转发展示)。
    ///
    /// 关键约束：`submit_wire` 返回 `used < len` 是**常态**（CRC 一次只读 1 字节、
    /// 事件/输出队列满都会中途停），剩余字节必须**缓存**到内部缓冲，下次数据到达
    /// 或本函数内循环继续提交，绝不能丢弃——否则 ZFILE/ZDATA 帧永远解析不完。
    pub fn feed(&mut self, data: &[u8]) -> Result<(Vec<u8>, bool, Vec<u8>), AppError> {
        match self {
            Self::Tx { tx, .. } => {
                let used = tx.submit_wire(data).map_err(zerr)?;
                Ok((Vec::new(), false, data[used..].to_vec()))
            }
            Self::Rx { .. } => {
                // 先并入缓冲，再与状态机交替：drain 发应答/落盘，submit 消费帧。
                // 借用只在局部作用域，避免与 `self.drain()` 的可变借用冲突。
                {
                    let Self::Rx { pending, .. } = self else {
                        unreachable!()
                    };
                    pending.extend_from_slice(data);
                }
                let mut out = Vec::new();
                let mut ended = false;
                loop {
                    let (wb, done) = self.drain()?;
                    out.extend_from_slice(&wb);
                    if done {
                        ended = true;
                        break;
                    }
                    let used = match self {
                        Self::Rx { rx, pending, .. } => {
                            if pending.is_empty() {
                                0
                            } else {
                                rx.submit_wire(pending).map_err(zerr)?
                            }
                        }
                        _ => 0,
                    };
                    if used == 0 {
                        break;
                    }
                    if let Self::Rx { pending, .. } = self {
                        pending.drain(..used);
                    }
                }
                let rest = if ended {
                    match self {
                        Self::Rx { pending, .. } => std::mem::take(pending),
                        _ => Vec::new(),
                    }
                } else {
                    Vec::new()
                };
                Ok((out, ended, rest))
            }
        }
    }

    /// 取出文件元信息（一次性）：上传创建时即知，下载 `FileStarted` 后知。
    /// 供调用方向前端广播 `zmodem-file`，把传输行补全（文件名/大小）。
    pub fn take_file_info(&mut self) -> Option<(String, u64)> {
        match self {
            Self::Tx { file_info, .. } | Self::Rx { file_info, .. } => file_info.take(),
        }
    }

    /// 泵本地侧：上传读文件产帧；下载把握手应答发出去。返回 (写回字节, 是否完成)。
    /// 注意：`SessionCompleted` 出现时状态机可能还排队着收尾字节（发送方对 ZFIN 的
    /// `OO` 应答），**必须继续泵到 Idle 把它们发出去**，否则远端 rz/sz 一直等收尾不退出。
    pub fn pump(&mut self) -> Result<(Vec<u8>, bool), AppError> {
        match self {
            Self::Tx { tx, file, .. } => {
                let mut out = Vec::new();
                let mut ended = false;
                loop {
                    match tx.poll() {
                        Action::WriteWire(bytes) => {
                            let n = bytes.len();
                            out.extend_from_slice(bytes);
                            tx.wire_written(n);
                        }
                        Action::ReadFile { offset, max_len } => {
                            use std::io::{Read, Seek, SeekFrom};
                            file.seek(SeekFrom::Start(offset.get() as u64))
                                .map_err(|e| AppError::Io(e.to_string()))?;
                            let mut buf = vec![0u8; max_len.min(64 * 1024)];
                            let n = file
                                .read(&mut buf)
                                .map_err(|e| AppError::Io(e.to_string()))?;
                            tx.submit_file(&buf[..n]).map_err(zerr)?;
                        }
                        Action::Event(Event::SessionCompleted) | Action::Event(Event::Aborted) => {
                            // 只做标记：继续泵，把排队的收尾字节发完再报完成
                            ended = true;
                        }
                        Action::Event(_) => {}
                        _ => break,
                    }
                }
                Ok((out, ended))
            }
            Self::Rx { .. } => self.drain(),
        }
    }

    /// 泵 `Receiver`：发应答帧 + 落盘文件数据，直到 Idle。
    fn drain(&mut self) -> Result<(Vec<u8>, bool), AppError> {
        let Self::Rx {
            rx,
            dest,
            sink,
            file_info,
            ..
        } = self
        else {
            return Ok((Vec::new(), false));
        };
        let mut out = Vec::new();
        let mut ended = false;
        loop {
            match rx.poll() {
                Action::WriteWire(bytes) => {
                    debug!("[zm] poll WriteWire {}B", bytes.len());
                    let n = bytes.len();
                    out.extend_from_slice(bytes);
                    rx.wire_written(n);
                }
                Action::WriteFile(bytes) => {
                    debug!(
                        "[zm] poll WriteFile {}B sink={}",
                        bytes.len(),
                        sink.is_some()
                    );
                    // poll() 的 bytes 借用 receiver，先复制出来再还回可变借用
                    let data = bytes.to_vec();
                    if let Some(s) = sink.as_mut() {
                        std::io::Write::write_all(&mut s.file, &data)
                            .map_err(|e| AppError::Io(e.to_string()))?;
                    }
                    rx.file_written(data.len()).map_err(zerr)?;
                }
                Action::Event(ev) => {
                    debug!("[zm] poll Event {ev:?}");
                    match ev {
                        Event::FileStarted(info) => {
                            let name = String::from_utf8_lossy(info.name).trim().to_string();
                            let base = Path::new(&name)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "zmodem.bin".into());
                            let path = dest.join(&base);
                            let file = std::fs::File::create(&path)
                                .map_err(|e| AppError::Io(e.to_string()))?;
                            *sink = Some(RxSink { file });
                            // 供前端传输列表补全文件名/大小
                            let size = info.size.map(|p| p.get() as u64).unwrap_or(0);
                            *file_info = Some((name, size));
                        }
                        Event::FileCompleted => {
                            *sink = None;
                        }
                        Event::SessionCompleted | Event::Aborted => {
                            // 只做标记：继续泵，把排队的收尾应答（ZFIN）发完再报完成
                            ended = true;
                        }
                        _ => {}
                    }
                }
                Action::ReadFile { .. } | Action::Idle | _ => {
                    debug!("[zm] poll Idle/ReadFile -> drain end");
                    break;
                }
            }
        }
        Ok((out, ended))
    }
}

/// 极简 shell 单引号包裹（远端 Linux/macOS）。
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 供命令层取句柄（与 exec 同源，避免重复逻辑）。
pub(crate) fn handle_of(
    app: &AppHandle,
    session_id: &str,
) -> Result<crate::session::SshHandle, AppError> {
    let st = app.state::<AppState>();
    match st.sessions.handle(session_id) {
        Some(h) => Ok(h),
        None if st.sessions.contains(session_id) => {
            Err(AppError::Ai("连接已断开，请先重连".into()))
        }
        None => Err(AppError::NoSuchSession(session_id.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::shell_quote;

    #[test]
    fn quotes_shell_path() {
        assert_eq!(shell_quote("/var/log/a"), "'/var/log/a'");
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }
}
