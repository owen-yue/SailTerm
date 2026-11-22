//! FTP/FTPS 后端：**独立的控制连接**，不复用 SSH。
//!
//! 与 SFTP 的三点关键差异，直接决定了本文件的形态：
//! - 一条控制连接同时只容一个数据连接，所以后端全程持锁串行化命令，
//!   传输进行时同会话的目录浏览会排队等待（这是协议限制，不是实现偷懒）；
//! - 协议没有 `realpath`/`stat`/`chmod`，只能用 `CWD`+`PWD`、`SIZE`+`MDTM` 拼凑，
//!   权限修改明确返回不支持（前端对 FTP 会话隐藏「权限…」入口）；
//! - 没有 actor 协程盯着连接存活，断线只能在命令返回连接级错误时**就地**判定。
//!
//! 安全红线：凭证只在 [`open`] 的登录路径出现，用后即随
//! [`Secret`] 释放；文件操作全程复用已认证的控制连接，绝不二次索取凭证。

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use suppaftp::list::{File, ListParser, PosixPexQuery};
use suppaftp::tokio::{
    AsyncFtpStream, AsyncRustlsConnector, AsyncRustlsFtpStream, ImplAsyncFtpStream, TokioTlsStream,
};
use suppaftp::tokio_rustls::TlsConnector;
use suppaftp::types::{FileType, FormatControl, Response};
use suppaftp::{FtpError, Mode, Status};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, SeekFrom};
use tokio::sync::Mutex;

use super::{
    file_name, ftps_trust, join, perms_string, sort_entries, CancelFlag, FileEntry, FileTransfer,
    Progress, CHUNK,
};
use crate::error::AppError;
use crate::session::{
    emit_stage, emit_state, keepalive_secs, AuthMaterial, ConnectSpec, FtpFeat, FtpHandle, FtpInfo,
    Reconnect, SessionEntry, SessionKind, SessionState, TerminalRing,
};
use crate::state::AppState;

/// 建立 FTP/FTPS 会话并注册到会话表，返回 `session_id`。
///
/// 与 SSH 会话不同：这里不 spawn actor，也没有 `tx`/`handle`——FTP 没有终端，
/// 断线感知下沉到文件命令的返回路径（见 [`FtpBackend::fail`]）。
pub async fn connect(
    app: AppHandle,
    spec: ConnectSpec,
    reconnect: Reconnect,
    connect_id: &str,
) -> Result<String, AppError> {
    let (ftp, ftp_info) = match open(&app, &spec, connect_id).await {
        Ok(v) => v,
        Err(e) => {
            emit_stage(
                &app,
                connect_id,
                &spec.host,
                "failed",
                "fail",
                &e.to_string(),
            );
            return Err(e);
        }
    };
    let sessions = &app.state::<AppState>().sessions;
    let id = sessions.next_id();
    // 连接代数从 1 起：心跳任务按代数认领自己押注的那条连接（见 spawn_keepalive），
    // 0 留给非 FTP 会话的占位值，两者永不混淆。
    let epoch: u64 = 1;
    sessions.insert(SessionEntry {
        id: id.clone(),
        label: spec.label(),
        host: spec.host.clone(),
        profile_id: reconnect.profile_id(),
        state: SessionState::Connected,
        kind: spec.kind,
        tx: None,
        handle: None,
        jumps: Vec::new(),
        ftp: Some(ftp),
        ftp_epoch: epoch,
        reconnect,
        // FTP 会话无终端：给个空环（能力表已把 get_terminal_snapshot 对 FTP 挡在下发之外，
        // 留着只是为了 `SessionEntry` 不区分协议也能统一拼 DTO）
        ring: TerminalRing::new(),
        port: spec.port,
        latency_ms: None,
        // FTP 无 SSH 协商，但有自己的一份快照（banner/FEAT/证书）
        negotiation: None,
        ftp_info: Some(ftp_info),
    });
    spawn_keepalive(&app, &id, epoch, spec.keepalive_secs);
    Ok(id)
}

/// 断线后沿用同一 `session_id` 重连：标签与文件面板的当前目录保持不变。
pub async fn reconnect(app: AppHandle, id: String, spec: ConnectSpec) -> Result<(), AppError> {
    // 重连的标签已存在（不是预加载占位），无 connect_id → 不发 stage 事件。
    let (ftp, ftp_info) = open(&app, &spec, "").await?;
    let epoch = app.state::<AppState>().sessions.update(&id, |e| {
        e.ftp = Some(ftp);
        e.ftp_info = Some(ftp_info);
        e.state = SessionState::Connected;
        // 代数自增：旧心跳按旧代数对不上号就退役，新心跳按新代数认领这条连接
        e.ftp_epoch = e.ftp_epoch.wrapping_add(1);
        e.ftp_epoch
    });
    let Some(epoch) = epoch else {
        return Err(AppError::NoSuchSession(id));
    };
    emit_state(&app, &id, SessionState::Connected, None);
    spawn_keepalive(&app, &id, epoch, spec.keepalive_secs);
    Ok(())
}

/// 取该会话的 FTP 文件后端：复用已认证的控制连接，不重新登录。
///
/// 同步函数——控制连接早就在注册表里了，这里只是按明文/TLS 分支实例化后端。
pub fn backend(app: &AppHandle, session_id: &str) -> Result<Arc<dyn FileTransfer>, AppError> {
    let st = app.state::<AppState>();
    let handle = match st.sessions.ftp(session_id) {
        Some(h) => h,
        None if st.sessions.contains(session_id) => {
            return Err(AppError::Biz {
                key: "err.conn.dropped",
                zh: "连接已断开，请重连",
            })
        }
        None => return Err(AppError::NoSuchSession(session_id.to_string())),
    };
    let (app, id) = (app.clone(), session_id.to_string());
    Ok(match handle {
        FtpHandle::Plain(conn) => Arc::new(FtpBackend::new(app, id, conn)),
        FtpHandle::Secure(conn) => Arc::new(FtpBackend::new(app, id, conn)),
    })
}

/// 会话关闭时礼貌地发 `QUIT`；失败无所谓，连接本来就要丢弃。
pub async fn quit(handle: FtpHandle) {
    match handle {
        FtpHandle::Plain(conn) => {
            let _ = conn.lock().await.quit().await;
        }
        FtpHandle::Secure(conn) => {
            let _ = conn.lock().await.quit().await;
        }
    }
}

/// 建连 + 认证 + 切被动模式 + 采集协商快照。凭证只在这一个函数里出现。
///
/// 需要 `app`：FTPS 首次遇到不受系统信任的证书时，要把裁决请求归口到托管状态里的
/// 待应答表（与 SSH 主机密钥 TOFU 同一套机制）。
async fn open(
    app: &AppHandle,
    spec: &ConnectSpec,
    connect_id: &str,
) -> Result<(FtpHandle, FtpInfo), AppError> {
    let password = match &spec.auth {
        AuthMaterial::Password(pw) => pw.expose(),
        // 表单与档案层都已拦住私钥，这里只是不让类型系统留下未覆盖分支
        _ => return Err(AppError::Invalid("FTP/FTPS 只支持密码认证".into())),
    };
    let addr = (spec.host.as_str(), spec.port);
    match spec.kind {
        // 显式 TLS：`AUTH TLS` 在登录**之前**完成，凭证不上明文链路
        SessionKind::Ftps => {
            let slot = ftps_trust::Slot::default();
            // rustls 的校验回调是同步的，问不了人：首次遇到不受信任的证书时，它只能
            // 「记一笔 + 让本次握手失败」。真正的等人在这下面：用户接受 → 落库固定指纹 →
            // 重连一次（`into_secure` 会吃掉控制连接，故重试必须重开一条）。
            let mut approved = false;
            loop {
                let verifier =
                    ftps_trust::verifier(app.clone(), &spec.host, spec.port, slot.clone())?;
                let connector = AsyncRustlsConnector::from(TlsConnector::from(
                    ftps_trust::tls_config(verifier),
                ));
                emit_stage(
                    app,
                    connect_id,
                    &spec.host,
                    "tcp",
                    "start",
                    &format!("正在解析并连接 {}:{}", spec.host, spec.port),
                );
                let stream = AsyncRustlsFtpStream::connect(addr)
                    .await
                    .map_err(|e| connect_fail(spec, e))?;
                emit_stage(app, connect_id, &spec.host, "tcp", "ok", "TCP 连接已建立");
                emit_stage(
                    app,
                    connect_id,
                    &spec.host,
                    "tls",
                    "start",
                    "正在进行 TLS 握手与证书校验",
                );
                match stream.into_secure(connector, &spec.host).await {
                    Ok(mut s) => {
                        emit_stage(app, connect_id, &spec.host, "tls", "ok", "TLS 已建立");
                        prepare_stage(app, connect_id, spec);
                        prepare(&mut s, spec, password).await?;
                        emit_stage(app, connect_id, &spec.host, "login", "ok", "登录完成");
                        // 证书指纹从信任槽里拿：校验器在本次握手中已记过一笔，
                        // 连 CA 直通的合法证书也能在面板上显示出指纹（不靠已固定记录推）。
                        let info = snapshot(&mut s, slot.presented()).await;
                        return Ok((FtpHandle::Secure(Arc::new(Mutex::new(s))), info));
                    }
                    Err(e) => {
                        if let Some(ask) = slot.take_ask() {
                            if approved {
                                // 刚确认过却又递来另一张证书：不再打扰用户，直接中止
                                return Err(AppError::HostKey(
                                    "服务端证书与刚确认的不一致，连接已中止".into(),
                                ));
                            }
                            ftps_trust::resolve(app, ask).await?;
                            approved = true;
                            continue;
                        }
                        // 指纹与已记录不符（防中间人）：文案已由校验器组好
                        if let Some(msg) = slot.take_refusal() {
                            return Err(AppError::HostKey(msg));
                        }
                        return Err(connect_fail(spec, e));
                    }
                }
            }
        }
        // 明文 FTP：账号密码与文件内容都不加密，前端表单必须给出警告。
        // 非 FTP 会话不会走到这里（本函数仅由 `is_ftp()` 分派调用），列出只为穷尽匹配。
        SessionKind::Ftp
        | SessionKind::Ssh
        | SessionKind::Local
        | SessionKind::Telnet
        | SessionKind::Serial
        | SessionKind::Mosh => {
            emit_stage(
                app,
                connect_id,
                &spec.host,
                "tcp",
                "start",
                &format!("正在解析并连接 {}:{}", spec.host, spec.port),
            );
            let mut s = AsyncFtpStream::connect(addr)
                .await
                .map_err(|e| connect_fail(spec, e))?;
            emit_stage(app, connect_id, &spec.host, "tcp", "ok", "TCP 连接已建立");
            prepare_stage(app, connect_id, spec);
            prepare(&mut s, spec, password).await?;
            emit_stage(app, connect_id, &spec.host, "login", "ok", "登录完成");
            // 明文 FTP 无证书可报
            let info = snapshot(&mut s, None).await;
            Ok((FtpHandle::Plain(Arc::new(Mutex::new(s))), info))
        }
    }
}

/// 登录阶段开始埋点（供两个 FTP 分支复用）：FTP 登录含账号密码交换，无独立 TLS 时即此步。
fn prepare_stage(app: &AppHandle, connect_id: &str, spec: &ConnectSpec) {
    emit_stage(
        app,
        connect_id,
        &spec.host,
        "login",
        "start",
        &format!("正在登录用户 {}", spec.username),
    );
}

/// 登录 + 协商数据连接模式 + 进入初始目录。
///
/// 默认被动（PASV）：数据连接由客户端主动外连，NAT/防火墙后面才通。
/// `spec.ftp_active_mode` 置真时改用主动（PORT）：客户端监听、服务端回连，
/// 仅在对端网络策略强制要求时用（本端在 NAT 后时通常连不通，属预期行为）。
async fn prepare<T: TokioTlsStream + Send>(
    s: &mut ImplAsyncFtpStream<T>,
    spec: &ConnectSpec,
    password: &str,
) -> Result<(), AppError> {
    // 匿名 FTP/FTPS：用户名留空按 RFC 惯例发 USER anonymous（密码可为空串）。
    let user = if spec.username.trim().is_empty() {
        "anonymous"
    } else {
        spec.username.as_str()
    };
    s.login(user, password).await.map_err(login_err)?;
    // RFC 2640：**登录后立刻**协商 UTF-8，必须早于任何路径/列目录命令。
    // 否则中文起始目录会先按服务端本地编码（GBK）解一次，CWD 直接失败；
    // 不少服务端不播报 FEAT 却认这条命令，失败只是回个 5xx，不影响已建好的连接，
    // 真不支持时靠 [`decode_text`] 的兜底解码。
    let _ = s.opts("UTF8", Some("ON")).await;
    s.set_mode(if spec.ftp_active_mode {
        Mode::Active
    } else {
        Mode::Passive
    });
    if let Some(dir) = &spec.startup_dir {
        s.cwd(dir).await.map_err(ftp_err)?;
    }
    Ok(())
}

/// 登录成功后采集一次协商快照（`FEAT` + `SYST` 各一条命令，只在建档时跑，不进热路径）。
///
/// 采集失败**不阻断连接**：FEAT 是 RFC 2389 可选项、SYST 内容各家不一，老设备可能直接回错误码；
/// 那种情况返回空值、面板显式说「服务端未应答」，比把刚认证好的连接拆掉有用得多。
async fn snapshot<T: TokioTlsStream + Send>(
    s: &mut ImplAsyncFtpStream<T>,
    cert_fingerprint: Option<String>,
) -> FtpInfo {
    // suppaftp 在 connect 时就把 220 welcome 存在了流上（`get_welcome_msg`），
    // 无需自己拆字节拼 banner；`into_secure` 也不弄丢它（结构体字段逐位搬过去）。
    let server_software = s
        .get_welcome_msg()
        .map(|w| {
            // 只按协议定义去掉状态码前缀（`220 ` / 多行的 `220-`），不再拆软件名
            w.strip_prefix("220 ")
                .or_else(|| w.strip_prefix("220-"))
                .unwrap_or(w)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let feat = match s.feat().await {
        Ok(map) => {
            let mut v: Vec<FtpFeat> = map
                .into_iter()
                .map(|(name, param)| FtpFeat { name, param })
                .collect();
            // HashMap 的迭代序每次不同，不排序会在面板上“跳动”
            v.sort_by(|a, b| a.name.to_uppercase().cmp(&b.name.to_uppercase()));
            v
        }
        Err(_) => Vec::new(),
    };
    // RFC 2640：服务端声明了 UTF8，也要客户端**显式**发 `OPTS UTF8 ON` 才真的按 UTF-8 传名字。
    // 这一步已在 [`prepare`] 登录后立刻做过（早于任何路径命令），这里不再重复发。
    let system = match s.custom_command("SYST", &[Status::Name]).await {
        Ok(r) => String::from_utf8_lossy(&r.body).trim().to_string(),
        Err(_) => String::new(),
    };
    FtpInfo {
        server_software,
        system,
        feat,
        cert_fingerprint,
    }
}

/// 文本解码：先按 UTF-8 严格解，失败再按 GB18030（GBK 的超集）解。
///
/// 为什么要自己解：FTP 的文件名编码不由协议统一——服务端声明 UTF8 时按 UTF-8，
/// 未声明时按本地编码（简体中文环境即 GBK）。字节就是字节，解错一次就是永久损坏。
fn decode_text(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        // 非法 UTF-8 才走本地编码：这是兜底，不是猜测——UTF-8 合法就没必要再试别的
        Err(_) => encoding_rs::GB18030.decode(raw).0.into_owned(),
    }
}

/// 读一次「数据连接上的文本行」（LIST / NLST 共用），由调用方拼命令字。
///
/// 不用 suppaftp 的 `list()`/`nlst()`：它内部是 `String::from_utf8_lossy`，
/// 非 UTF-8 服务端在那一刻就把文件名里的字节替换成 U+FFFD，之后再无还原。
/// 这里拿原始字节逐行解码（见 [`decode_text`]），与主流桌面客户端的做法一致。
async fn data_lines<T: TokioTlsStream + Send>(
    conn: &mut ImplAsyncFtpStream<T>,
    cmd: &str,
) -> Result<Vec<String>, FtpError> {
    // 命令必须单行：换行等于往控制连接里夹带第二条命令
    if cmd.contains('\r') || cmd.contains('\n') {
        return Err(FtpError::BadResponse);
    }
    let (_, stream) = conn
        .custom_data_command(cmd, &[Status::AboutToSend, Status::AlreadyOpen])
        .await?;
    let mut reader = BufReader::new(stream);
    let mut lines = Vec::new();
    loop {
        let mut raw = Vec::new();
        let n = reader
            .read_until(b'\n', &mut raw)
            .await
            .map_err(FtpError::ConnectionError)?;
        if n == 0 {
            break;
        }
        while matches!(raw.last(), Some(&b'\n') | Some(&b'\r')) {
            raw.pop();
        }
        if raw.is_empty() {
            continue;
        }
        lines.push(decode_text(&raw));
    }
    // 读完数据连接才能收尾（否则服务端不会发 226）
    reader.into_inner().finish().await?;
    Ok(lines)
}

/// 解析 `257 "/路径" [说明]` 的响应体：引号内是路径，按原始字节解码。
///
/// 与 [`decode_text`] 同源：服务端声明的 UTF8 不落实是常事，中文目录名按字节读才不会坏。
fn pwd_from_body(raw: &[u8]) -> Option<String> {
    let text = decode_text(raw);
    let begin = text.find('"')?;
    let end = text.rfind('"')?;
    (begin < end).then(|| text[begin + 1..end].to_string())
}

/// 拼一条 LIST/NLST 命令：路径为空则取当前目录。
fn list_cmd(verb: &str, path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        verb.to_string()
    } else {
        format!("{verb} {path}")
    }
}

/// 列目录（连接锁已持有的版本）：递归删复用同一条锁，供 [`remove_recursive`] 逐层下钻。
///
/// `fail` 取 `&dyn Fn`（而非泛型 `impl Fn`）：递归删会逐层向下传递它，若用泛型参数，
/// `F`/`&F`/`&&F`…… 会触发无界单态化（类型递归爆炸），编译器会无限膨胀。固定成 trait object 即打断这条链。
async fn list_with<T: TokioTlsStream + Send>(
    conn: &mut ImplAsyncFtpStream<T>,
    path: &str,
    fail: &(dyn Fn(FtpError) -> AppError + Send + Sync),
) -> Result<Vec<FileEntry>, AppError> {
    // 目录列表是文本：显式切回 ASCII，不受上一次 stat/传输留下的二进制态影响。
    let _ = conn
        .transfer_type(FileType::Ascii(FormatControl::Default))
        .await;
    let lines = data_lines(conn, &list_cmd("LIST", path))
        .await
        .map_err(fail)?;
    let mut out: Vec<FileEntry> = lines
        .iter()
        .filter_map(|line| entry_from_line(line, path))
        .collect();
    if out.is_empty() {
        // LIST 的格式五花八门，全都解析不出来时退回 NLST：
        // 至少给出名字让用户能点进去，属性留空好过编造
        let names = data_lines(conn, &list_cmd("NLST", path))
            .await
            .map_err(fail)?;
        out = names
            .iter()
            .filter(|n| !is_dot(n))
            .map(|n| skeleton(n, join(path, n), false))
            .collect();
    }
    sort_entries(&mut out);
    Ok(out)
}

/// RM 被拒后判「它其实是目录吗」：FTP 没有 stat，沿用 [`FtpBackend::stat`] 的同一口径——
/// SIZE 能出字节数就是文件；SIZE 被拒而 MDTM 能给时间戳则按目录处理。
/// 只在目录位拿不准的场合调（见 [`remove_recursive`]），多两条命令换一次正确删除。
async fn looks_like_dir<T: TokioTlsStream + Send>(
    conn: &mut ImplAsyncFtpStream<T>,
    path: &str,
) -> bool {
    // SIZE 在 ASCII 模式下会被多数服务端拒（同 stat 那里的处理），先切二进制才不会误判成目录。
    let _ = conn.transfer_type(FileType::Binary).await;
    if conn.size(path).await.is_ok() {
        return false;
    }
    conn.mdtm(path).await.is_ok()
}

/// 删除（连接锁已持有的版本）：目录先递归清空内容再 RMD。
///
/// FTP 的 RMD 与 SFTP 的 remove_dir 一样只删空目录，非空直接回 550；
/// 这里补上递归语义，与本地栏 `remove_dir_all` 及 SFTP 后端对齐。
/// 目录位拿不准（LIST 解析失败走 NLST 兜底，条目全按 `is_dir=false` 构造）时按文件先 RM，
/// 被拒再用 [`looks_like_dir`] 探一次，确认是目录才转递归删。
/// `fail` 用 `&dyn Fn` 而非泛型，理由同 [`list_with`]（避免递归层的无界单态化）。
async fn remove_recursive<T: TokioTlsStream + Send>(
    conn: &mut ImplAsyncFtpStream<T>,
    path: &str,
    is_dir: bool,
    fail: &(dyn Fn(FtpError) -> AppError + Send + Sync),
) -> Result<(), AppError> {
    if is_dir {
        for item in list_with(conn, path, fail).await? {
            // 软链接一律当文件删：跟进目标会把链接之外的内容一起删掉
            if item.is_dir && !item.is_symlink {
                Box::pin(remove_recursive(conn, &item.path, true, fail)).await?;
                continue;
            }
            if let Err(e) = conn.rm(&item.path).await {
                // RM 被拒：目录位拿不准时它其实是目录（RMD 只能删空目录），探到是目录就递归删；
                // 软链不参与兜底，否则会被跟进目标。探不出的服务端（无 MDTM）回原始 RM 错误。
                if item.is_symlink || !looks_like_dir(conn, &item.path).await {
                    return Err(fail(e));
                }
                Box::pin(remove_recursive(conn, &item.path, true, fail)).await?;
            }
        }
        conn.rmdir(path).await.map_err(fail)
    } else {
        conn.rm(path).await.map_err(fail)
    }
}

/// FTP 后端：把控制连接适配到统一的 [`FileTransfer`]。
///
/// 泛型参数 `T` 是底层流类型，明文与 TLS 各单态化一份；对外统一擦除成
/// `Arc<dyn FileTransfer>`，因此传输队列（[`super::transfer`]）对两者一视同仁。
pub struct FtpBackend<T>
where
    T: TokioTlsStream + Send,
{
    /// 判定断线时就地发 `session-state`，所以后端得握着 app 句柄。
    app: AppHandle,
    session_id: String,
    conn: Arc<Mutex<ImplAsyncFtpStream<T>>>,
}

impl<T> FtpBackend<T>
where
    T: TokioTlsStream + Send,
{
    pub fn new(
        app: AppHandle,
        session_id: String,
        conn: Arc<Mutex<ImplAsyncFtpStream<T>>>,
    ) -> Self {
        Self {
            app,
            session_id,
            conn,
        }
    }

    /// 统一的错误翻译：连接级错误顺手把会话转成断线态，其余原样映射。
    /// 返回 `AppError` 而非 `Result`，这样才能直接喂给 `map_err`。
    fn fail(&self, e: FtpError) -> AppError {
        // 421 = 「服务不可用，正在关闭传输通道」（RFC 959）：服务端已主动收尾，这条控制
        // 连接不会再有下一次成功命令。它走 UnexpectedResponse 分支，原先只把原文交给用户，
        // 会话状态仍停在 connected → 顶栏指示灯一直绿着骗人。
        let closing = matches!(&e, FtpError::UnexpectedResponse(r) if r.status.code() == 421);
        if closing || matches!(&e, FtpError::ConnectionError(_) | FtpError::BadResponse) {
            mark_disconnected(&self.app, &self.session_id);
        }
        ftp_err(e)
    }
}

#[async_trait]
impl<T: TokioTlsStream + Send + Sync + 'static> FileTransfer for FtpBackend<T> {
    async fn canonicalize(&self, path: &str) -> Result<String, AppError> {
        // FTP 没有 realpath：只能 CWD 进去再 PWD 出来（顺带校验目录确实存在）
        let mut conn = self.conn.lock().await;
        conn.cwd(path).await.map_err(|e| self.fail(e))?;
        // 不走 suppaftp 的 `pwd()`：它内部是 `String::from_utf8_lossy`，GBK 服务端上的
        // 中文目录名在那一刻就变成 U+FFFD，之后面包屑与「进入目录」用的都是坏路径。
        // 这里自己发 PWD 拿原始响应字节，交给 [`decode_text`] 按同一套规则解码。
        let r = conn
            .custom_command("PWD", &[Status::PathCreated])
            .await
            .map_err(|e| self.fail(e))?;
        pwd_from_body(&r.body).ok_or_else(|| self.fail(FtpError::BadResponse))
    }

    async fn list(&self, path: &str) -> Result<Vec<FileEntry>, AppError> {
        let mut conn = self.conn.lock().await;
        let fail = |e| self.fail(e);
        self::list_with(&mut conn, path, &fail).await
    }

    async fn stat(&self, path: &str) -> Result<Option<FileEntry>, AppError> {
        // FTP 没有 stat：SIZE 与 MDTM 是两条独立命令，且 SIZE 对目录通常直接报错
        let mut conn = self.conn.lock().await;
        // SIZE 在 ASCII 模式下会被多数服务器拒绝（如 pyftpdlib 回 550），先切二进制再探测，
        // 避免 SIZE 失败后误走 MDTM 分支把文件当成目录。
        let _ = conn.transfer_type(FileType::Binary).await;
        // 先把结果落地再 match：scrutinee 里的 future 会一直借住 `conn` 到整个 match 结束，
        // 那样分支里就没法再发 MDTM
        let probed = conn.size(path).await;
        let size = match probed {
            Ok(n) => n as u64,
            // 服务端拒绝了 SIZE（多半因为是目录，或不支持该命令）：再用 MDTM 探一次，
            // 能出时间戳就当目录存在；还失败就是真不存在，返回 `None` 而非报错
            Err(FtpError::UnexpectedResponse(_)) | Err(FtpError::BadResponse) => {
                return Ok(conn.mdtm(path).await.ok().map(|t| {
                    let mut e = skeleton(&file_name(path), path.to_string(), true);
                    e.modified_ms = Some(t.and_utc().timestamp_millis());
                    e
                }));
            }
            Err(e) => return Err(self.fail(e)),
        };
        // MDTM 回的是不带时区的 UTC 时间；服务器不支持时只影响展示，不算失败
        let modified_ms = conn
            .mdtm(path)
            .await
            .ok()
            .map(|t| t.and_utc().timestamp_millis());
        let mut entry = skeleton(&file_name(path), path.to_string(), false);
        entry.size = size;
        entry.modified_ms = modified_ms;
        Ok(Some(entry))
    }

    async fn download(
        &self,
        remote: &str,
        local: &Path,
        offset: u64,
        cancel: &CancelFlag,
        progress: &Progress,
    ) -> Result<u64, AppError> {
        let mut dst = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            // 续传时不截断本地已有内容（断点续传）
            .truncate(offset == 0)
            .open(local)
            .await
            .map_err(|e| AppError::Io(format!("无法创建本地文件 {}：{e}", local.display())))?;
        if offset > 0 {
            dst.seek(SeekFrom::Start(offset))
                .await
                .map_err(|e| AppError::Io(format!("无法定位本地文件 {}：{e}", local.display())))?;
        }

        let mut conn = self.conn.lock().await;
        // 下载走二进制：ASCII 模式会转换换行/截断 NUL，破坏非文本文件。
        conn.transfer_type(FileType::Binary)
            .await
            .map_err(|e| self.fail(e))?;
        if offset > 0 {
            // REST 必须发在 RETR 之前
            conn.resume_transfer(offset as usize)
                .await
                .map_err(|e| self.fail(e))?;
        }
        let mut src = conn
            .retr_as_stream(remote)
            .await
            .map_err(|e| self.fail(e))?;

        let mut buf = vec![0u8; CHUNK];
        let mut total = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                // abort 负责关数据连接并吃掉服务端的 426/226，控制连接才能继续用
                let _ = conn.abort(src).await;
                return Err(AppError::Transfer("传输已取消".into()));
            }
            // 先落地再 match：把 `src.read(..)` 直接写在 match 头上会延长 `src` 的借用，
            // 错误分支里就没法把它 move 进 abort()
            let read = src.read(&mut buf).await;
            let n = match read {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = conn.abort(src).await;
                    // 数据连接读失败：链路问题（服务端关闭/网络中断），不是本地文件问题，
                    // 归到 FTP 类并顺手把会话转成断线态，避免前端收到误导性的「文件读写失败」。
                    mark_disconnected(&self.app, &self.session_id);
                    return Err(AppError::Ftp(format!("读取 FTP 数据连接失败：{e}")));
                }
            };
            if let Err(e) = dst.write_all(&buf[..n]).await {
                let _ = conn.abort(src).await;
                return Err(AppError::Io(format!(
                    "写入本地文件 {} 失败：{e}",
                    local.display()
                )));
            }
            total += n as u64;
            progress(n as u64);
        }
        // `finish` 才会关数据连接并读取服务端的完成响应；只 drop 的话本次结果就丢了
        src.finish().await.map_err(|e| self.fail(e))?;
        drop(conn);

        dst.flush()
            .await
            .map_err(|e| AppError::Io(format!("写入本地文件 {} 失败：{e}", local.display())))?;
        dst.sync_all()
            .await
            .map_err(|e| AppError::Io(format!("落盘本地文件 {} 失败：{e}", local.display())))?;
        Ok(total)
    }

    async fn upload(
        &self,
        local: &Path,
        remote: &str,
        offset: u64,
        cancel: &CancelFlag,
        progress: &Progress,
    ) -> Result<u64, AppError> {
        let mut src = tokio::fs::File::open(local)
            .await
            .map_err(|e| AppError::Io(format!("无法读取本地文件 {}：{e}", local.display())))?;
        if offset > 0 {
            src.seek(SeekFrom::Start(offset))
                .await
                .map_err(|e| AppError::Io(format!("无法定位本地文件 {}：{e}", local.display())))?;
        }

        let mut conn = self.conn.lock().await;
        // 上传走二进制：与下载同理，避免 ASCII 转换破坏文件内容。
        conn.transfer_type(FileType::Binary)
            .await
            .map_err(|e| self.fail(e))?;
        // 续传用 APPE 追加：REST+STOR 各家服务器实现差异太大，不赌
        let opened = if offset > 0 {
            conn.append_with_stream(remote).await
        } else {
            conn.put_with_stream(remote).await
        };
        let mut dst = opened.map_err(|e| self.fail(e))?;

        let mut buf = vec![0u8; CHUNK];
        let mut total = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = conn.abort(dst).await;
                return Err(AppError::Transfer("传输已取消".into()));
            }
            let read = src.read(&mut buf).await;
            let n = match read {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = conn.abort(dst).await;
                    return Err(AppError::Io(format!(
                        "读取本地文件 {} 失败：{e}",
                        local.display()
                    )));
                }
            };
            if let Err(e) = dst.write_all(&buf[..n]).await {
                let _ = conn.abort(dst).await;
                mark_disconnected(&self.app, &self.session_id);
                return Err(AppError::Ftp(format!("写入 FTP 数据连接失败：{e}")));
            }
            total += n as u64;
            progress(n as u64);
        }
        src.flush()
            .await
            .map_err(|e| AppError::Io(format!("读取本地文件 {} 失败：{e}", local.display())))?;
        dst.finish().await.map_err(|e| self.fail(e))?;
        Ok(total)
    }

    async fn mkdir(&self, path: &str) -> Result<(), AppError> {
        let mut conn = self.conn.lock().await;
        conn.mkdir(path).await.map_err(|e| self.fail(e))
    }

    async fn remove(&self, path: &str, is_dir: bool) -> Result<(), AppError> {
        // 锁只在这里取一次：递归删过程中每层重新加锁会自锁死（tokio Mutex 不可重入）
        let mut conn = self.conn.lock().await;
        let fail = |e| self.fail(e);
        self::remove_recursive(&mut conn, path, is_dir, &fail).await
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), AppError> {
        let mut conn = self.conn.lock().await;
        conn.rename(from, to).await.map_err(|e| self.fail(e))
    }

    async fn chmod(&self, _path: &str, _mode: u32) -> Result<(), AppError> {
        // `SITE CHMOD` 是各家私有扩展而非协议标准；与其发一条大概率被拒的命令，
        // 不如明确告知不支持（前端对 FTP 会话隐藏「权限…」入口）
        Err(AppError::Ftp("FTP 协议没有统一的权限修改命令".into()))
    }

    async fn read_text(&self, path: &str, max_bytes: u64) -> Result<(String, u64), AppError> {
        let mut conn = self.conn.lock().await;
        let size = conn.size(path).await.ok().map(|n| n as u64).unwrap_or(0);
        let mut src = conn.retr_as_stream(path).await.map_err(|e| self.fail(e))?;
        let want = max_bytes as usize;
        let mut buf = Vec::with_capacity(want.min(64 * 1024));
        let mut tmp = vec![0u8; CHUNK.min(want.max(1))];
        loop {
            if buf.len() >= want {
                break;
            }
            let read = src.read(&mut tmp).await;
            let n = match read {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    let _ = conn.abort(src).await;
                    mark_disconnected(&self.app, &self.session_id);
                    return Err(AppError::Ftp(format!("读取 FTP 数据连接失败：{e}")));
                }
            };
            let take = n.min(want - buf.len());
            buf.extend_from_slice(&tmp[..take]);
        }
        src.finish().await.map_err(|e| self.fail(e))?;
        Ok((decode_text(&buf), size))
    }

    async fn write_text(&self, path: &str, content: &str) -> Result<(), AppError> {
        let mut conn = self.conn.lock().await;
        let mut dst = conn.put_with_stream(path).await.map_err(|e| self.fail(e))?;
        if let Err(e) = dst.write_all(content.as_bytes()).await {
            let _ = conn.abort(dst).await;
            mark_disconnected(&self.app, &self.session_id);
            return Err(AppError::Ftp(format!("写入 FTP 数据连接失败：{e}")));
        }
        dst.finish().await.map_err(|e| self.fail(e))?;
        Ok(())
    }
}

/// 拉起 FTP 控制连接的空闲保活任务。
///
/// NAS / 家用服务端普遍在几分钟无命令后回 `421 Timeout` 并关掉控制连接，而 FTP 会话
/// 没有 actor 盯着存活（见模块头）——不保活就是「灯是绿的、一动就报错」。
/// 周期与 SSH 侧同源（`keepalive_secs` 在那边喂 russh 的 keepalive_interval），
/// 档案没填就走默认。
fn spawn_keepalive(app: &AppHandle, id: &str, mine: u64, configured: u64) {
    // 下限 5s：防止一个被填错的档案把控制连接刷爆
    let every = Duration::from_secs(keepalive_secs(configured).max(5));
    let (app, id) = (app.clone(), id.to_string());
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            // 只认自己押注的那一代连接：重连会自增代数、断开/关闭会清空连接，两种情况本任务
            // 都该退役，否则多次重连会攒出一堆心跳一起往同一条连接发 NOOP。身份用代数而非句柄
            // 堆地址——旧连接释放后地址可能被新连接复用（ABA），旧任务会把新连接误认成自己那条。
            let handle = match app.state::<AppState>().sessions.ftp_at(&id, mine) {
                Some(h) => h,
                _ => return,
            };
            let r = match handle {
                FtpHandle::Plain(c) => c.lock().await.noop().await,
                FtpHandle::Secure(c) => c.lock().await.noop().await,
            };
            if let Err(e) = r {
                match e {
                    // 服务端还活着、只是不认 NOOP（有的回 200 而不是 250，有的直接 500）：
                    // 不能据此判成断开；但保活对它无效，就此收队，判定交给命令失败路径。
                    FtpError::UnexpectedResponse(_) => return,
                    _ => {
                        mark_disconnected(&app, &id);
                        return;
                    }
                }
            }
        }
    });
}

/// 连接级失败的就地收尾：FTP 没有 actor 去感知断线，只能在命令失败时把会话转成
/// 断线态并广播，让前端标签切灰、给出重连按钮。
///
/// 只处理「从 connected 掉下去」这一次，避免重试期间反复发事件。
fn mark_disconnected(app: &AppHandle, id: &str) {
    let st = app.state::<AppState>();
    let changed = st
        .sessions
        .update(id, |e| {
            if e.state == SessionState::Connected {
                e.state = SessionState::Disconnected;
                e.ftp = None;
                true
            } else {
                false
            }
        })
        .unwrap_or(false);
    if changed {
        // 尚在跑的传输也没意义了：置取消位，worker 在下一块边界收尾
        st.transfers.cancel_session(id);
        emit_state(app, id, SessionState::Disconnected, Some("FTP 连接已中断"));
    }
}

/// LIST 的一行 → 目录项。服务端可能是 Unix 风格也可能是 DOS/Windows 风格，两种都试；
/// 都解析不出来就丢弃这一行（宁可少显示一行，也不要给出错误的属性）。
fn entry_from_line(line: &str, dir: &str) -> Option<FileEntry> {
    // 记住是哪套语法解析成功的：DOS 风格没有权限位，此时不该编一个 `rwxrwxrwx` 出来
    let (file, posix) = match ListParser::parse_posix(line) {
        Ok(f) => (f, true),
        Err(_) => (ListParser::parse_dos(line).ok()?, false),
    };
    // 有的服务端在给了绝对路径参数后，LIST 会回全路径而不是文件名
    let name = file_name(file.name());
    if is_dot(&name) {
        return None;
    }
    let is_dir = file.is_directory();
    let is_symlink = file.is_symlink();
    let mut entry = skeleton(&name, join(dir, &name), is_dir);
    entry.is_symlink = is_symlink;
    entry.size = file.size() as u64;
    entry.modified_ms = modified_ms(&file);
    entry.uid = file.uid();
    entry.gid = file.gid();
    // suppaftp 的 LIST 解析只给数字 uid/gid，拿不到属主/属组名，留空由前端显示 `-`
    if posix {
        let mode = posix_mode(&file);
        entry.mode = Some(mode);
        entry.perms = Some(perms_string(is_dir, is_symlink, mode));
    }
    Some(entry)
}

/// suppaftp 的 `PosixPex` 字段全私有，只能按 can_read/can_write/can_execute 反推 9 个权限位。
fn posix_mode(file: &File) -> u32 {
    use PosixPexQuery as Q;
    let triplet = |who| {
        (u32::from(file.can_read(who)) << 2)
            | (u32::from(file.can_write(who)) << 1)
            | u32::from(file.can_execute(who))
    };
    (triplet(Q::Owner) << 6) | (triplet(Q::Group) << 3) | triplet(Q::Others)
}

/// LIST 的时间戳。DOS 风格解析不出时间时 suppaftp 给的是 UNIX 纪元，
/// 与其在列表里显示 1970-01-01，不如留空。
fn modified_ms(file: &File) -> Option<i64> {
    let t = file.modified();
    if t <= UNIX_EPOCH {
        return None;
    }
    t.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// 只有名字与类型的骨架条目：属性未知就留空，前端显示 `-`，
/// 好过给一个编造的 `0 字节 / 1970-01-01`。NLST 兜底与 `stat` 都从这里起步。
fn skeleton(name: &str, path: String, is_dir: bool) -> FileEntry {
    FileEntry {
        name: name.to_string(),
        path,
        is_dir,
        is_symlink: false,
        size: 0,
        modified_ms: None,
        mode: None,
        perms: None,
        uid: None,
        gid: None,
        user: None,
        group: None,
    }
}

/// `.` 与 `..` 在 SFTP 后端由 `ReadDir` 迭代器过滤，FTP 这边得自己筛。
fn is_dot(name: &str) -> bool {
    name == "." || name == ".."
}

/// 建连阶段的错误：带上目标地址，用户才知道是哪个连接没通。
fn connect_fail(spec: &ConnectSpec, e: FtpError) -> AppError {
    match e {
        FtpError::ConnectionError(io) => {
            AppError::Ftp(format!("无法连接 {}:{}：{io}", spec.host, spec.port))
        }
        FtpError::InvalidAddress(_) => AppError::Ftp(format!("主机地址无法解析：{}", spec.host)),
        e => ftp_err(e),
    }
}

/// 登录失败单独归因：530 是「用户名或密码不对」，该走 `E_AUTH` 让前端提示重填。
fn login_err(e: FtpError) -> AppError {
    if let FtpError::UnexpectedResponse(r) = &e {
        if r.status.code() == 530 {
            return AppError::Auth("用户名或密码错误".into());
        }
    }
    ftp_err(e)
}

/// FTP 协议错误 → 可读中文（错误码统一 `E_FTP`，见）。
///
/// 注意 `FtpError` **没有 IO 变体**：数据流读写返回的是 `io::Error`，
/// 由调用点各自映射到 `AppError::Io`。
fn ftp_err(e: FtpError) -> AppError {
    match e {
        FtpError::ConnectionError(io) => AppError::Ftp(format!("FTP 连接异常：{io}")),
        FtpError::SecureError(msg) => AppError::Ftp(format!("TLS 握手失败：{msg}")),
        // 服务器明确拒绝了命令，550/553 之类的说明文字就是给用户看的最好信息
        FtpError::UnexpectedResponse(r) => AppError::Ftp(server_message(&r)),
        FtpError::BadResponse => AppError::Ftp("服务端返回了无法解析的响应".into()),
        FtpError::InvalidAddress(_) => AppError::Ftp("主机地址无法解析".into()),
        FtpError::DataConnectionAlreadyOpen => AppError::Biz {
            key: "err.transfer.busy",
            zh: "上一传输未结束，请稍后重试",
        },
    }
}

/// 把服务端响应正文原样交给用户（如 `550 No such file`）；空正文时退回状态码。
fn server_message(r: &Response) -> String {
    match r.as_string() {
        Ok(text) if !text.is_empty() => text,
        _ => format!("服务端拒绝了该操作（{}）", r.status.code()),
    }
}

#[cfg(test)]
mod tests {
    use super::{entry_from_line, is_dot, posix_mode, server_message};
    use suppaftp::list::ListParser;
    use suppaftp::types::Response;
    use suppaftp::Status;

    #[test]
    fn parses_unix_listing_line() {
        let e = entry_from_line(
            "-rw-r--r--   1 1000     1000         2048 Jan 01 12:00 app.log",
            "/var/log",
        )
        .unwrap();
        assert_eq!(e.name, "app.log");
        assert_eq!(e.path, "/var/log/app.log");
        assert!(!e.is_dir);
        assert_eq!(e.size, 2048);
        assert_eq!(e.mode, Some(0o644));
        assert_eq!(e.perms.as_deref(), Some("-rw-r--r--"));
        // LIST 只有数字 uid/gid，属主名拿不到
        assert_eq!(e.uid, Some(1000));
        assert!(e.user.is_none() && e.group.is_none());
    }

    #[test]
    fn parses_directory_and_filters_dot_entries() {
        let d = entry_from_line(
            "drwxr-xr-x   2 1000     1000         4096 Jan 01 12:00 conf",
            "/etc",
        )
        .unwrap();
        assert!(d.is_dir && d.mode == Some(0o755));

        assert!(entry_from_line(
            "drwxr-xr-x   2 1000     1000         4096 Jan 01 12:00 ..",
            "/etc"
        )
        .is_none());
        assert!(is_dot(".") && is_dot("..") && !is_dot("..."));
    }

    /// 全路径形式的 LIST（部分服务端在收到绝对路径参数后会这么回）也要能取出文件名。
    #[test]
    fn takes_last_segment_when_server_returns_full_path() {
        let e = entry_from_line(
            "-rw-r--r--   1 1000     1000            0 Jan 01 12:00 /var/log/app.log",
            "/var/log",
        )
        .unwrap();
        assert_eq!(e.name, "app.log");
        assert_eq!(e.path, "/var/log/app.log");
    }

    /// DOS 风格没有权限位：不能编一个 `rwxrwxrwx` 出来冒充。
    #[test]
    fn dos_listing_leaves_permissions_empty() {
        let e = entry_from_line("01-01-26  12:00PM       <DIR>          conf", "/srv").unwrap();
        assert!(e.is_dir);
        assert_eq!(e.name, "conf");
        assert!(e.mode.is_none() && e.perms.is_none());
    }

    /// 解析不出来的行直接丢弃，不让一行脏数据毁掉整个列表。
    #[test]
    fn drops_unparsable_lines() {
        assert!(entry_from_line("total 24", "/var").is_none());
    }

    #[test]
    fn reconstructs_posix_mode_from_pex_queries() {
        let f = ListParser::parse_posix("-rwxr-x---  1 0 0 0 Jan 01 12:00 run.sh").unwrap();
        assert_eq!(posix_mode(&f), 0o750);
    }

    #[test]
    fn falls_back_to_status_code_when_body_is_empty() {
        let empty = Response::new(Status::FileUnavailable, Vec::new());
        assert!(server_message(&empty).contains("550"));
        let text = Response::new(Status::FileUnavailable, b"550 No such file".to_vec());
        assert_eq!(server_message(&text), "550 No such file");
    }
}
