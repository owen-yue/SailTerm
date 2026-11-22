//! 主机密钥校验（TOFU）。
//!
//! 策略：**首次**连接把指纹抛给前端让用户裁决，接受则写入 known_hosts；
//! **之后**严格比对，密钥变更一律拒绝（防中间人），需用户显式 `host_key_forget` 后重连。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use russh::client;
use russh::keys::{HashAlg, PublicKeyOrCertificate};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;
use tokio::time::timeout;

use crate::db::host_key::HostKeyRecord;
use crate::db::now_ms;
use crate::error::AppError;
use crate::state::AppState;

/// 首次连接的主机密钥确认请求事件。
pub const EVENT_HOST_KEY: &str = "host-key-unknown";
/// 等待用户裁决上限；超时按拒绝处理（fail-safe）。
const DECISION_TIMEOUT: Duration = Duration::from_secs(120);

/// 校验未通过的原因：`check_server_key` 只能返回 bool，具体原因经此槽位回传给 `ssh` 层。
#[derive(Debug)]
pub enum Issue {
    Denied,
    Timeout,
    Mismatch {
        stored: String,
        offered: String,
    },
    /// 证书形式的主机密钥等暂不支持的情形。
    Unsupported(String),
    /// 本地 known_hosts 读写失败。
    Backend(String),
}

impl Issue {
    /// 翻译成面向用户的错误（不含主机名之外的敏感信息）。
    pub fn into_error(self) -> AppError {
        match self {
            Self::Denied => AppError::HostKey("你已拒绝该主机密钥".into()),
            Self::Timeout => AppError::HostKey("主机密钥确认超时".into()),
            Self::Mismatch { stored, offered } => AppError::HostKey(format!(
                "主机密钥与已记录的不一致（可能遭中间人攻击）。已记录 {stored}，本次 {offered}；\
                 确认服务器已重装后可移除旧记录再连"
            )),
            Self::Unsupported(what) => AppError::HostKey(what),
            Self::Backend(what) => AppError::Db(what),
        }
    }
}

/// 待裁决请求表：`request_id → 应答通道`。
#[derive(Default)]
pub struct Pending {
    map: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl Pending {
    pub fn register(&self, id: &str) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.map.lock().unwrap().insert(id.to_string(), tx);
        rx
    }

    /// 是否有正在等用户裁决的主机密钥请求：握手指看门狗（`ssh::mod::guard_handshake`）据此
    /// 暂停超时累计 —— 首连确认密钥要等人点弹窗，不能被「无进展」误杀。
    pub fn has_pending(&self) -> bool {
        !self.map.lock().unwrap().is_empty()
    }

    /// 前端应答；`false` 表示请求已失效（超时/重复应答）。
    pub fn respond(&self, id: &str, accept: bool) -> bool {
        self.map
            .lock()
            .unwrap()
            .remove(id)
            .is_some_and(|tx| tx.send(accept).is_ok())
    }
}

/// 抛给前端的确认请求负载。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    pub request_id: String,
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
}

/// 一次握手协商出的算法名（russh 各算法 Name 均以 `Debug` 打印内部字符串，如 `"aes256-ctr"`）。
trait AlgoName {
    fn name_str(&self) -> String;
}

macro_rules! impl_algo_name {
    ($($t:ty),+) => {$(
        impl AlgoName for $t {
            fn name_str(&self) -> String {
                format!("{self:?}")
                    .trim_matches('"')
                    .to_string()
            }
        }
    )+};
}

impl_algo_name!(russh::kex::Name, russh::cipher::Name, russh::mac::Name);

/// `Compression` 枚举无公开名字接口，按变体映射回算法名（与 russh 内部常量表一致）。
fn compression_name(c: &russh::compression::Compression) -> String {
    use russh::compression::Compression;
    match c {
        Compression::None => "none",
        Compression::Zlib => "zlib",
        Compression::ZlibOpenSSH => "zlib@openssh.com",
    }
    .to_string()
}

/// 会话协商快照：`kex_done` 捕获的算法名 + 服务器标识 + 主机密钥指纹。
/// 只存公钥指纹与算法名，**绝不含密钥材料**（`shared_secret` 一律不读）。
#[derive(Clone)]
pub struct Negotiated {
    pub kex: String,
    pub cipher: String,
    pub mac_client: String,
    pub mac_server: String,
    pub compression_client: String,
    pub compression_server: String,
    /// 服务器标识串（`SSH-2.0-OpenSSH_8.0` 这类）
    pub remote_sshid: String,
    /// 服务器主机密钥类型（`check_server_key` 时记录）
    pub host_key_type: String,
    /// 已信任记录中的 SHA256 指纹
    pub host_key_fingerprint: String,
}

/// russh 客户端回调宿主：持有 AppHandle 以便查库 + 发事件。
pub struct ClientHandler {
    app: AppHandle,
    host: String,
    port: u16,
    issue: Arc<Mutex<Option<Issue>>>,
    /// 本连接是否开启 agent 转发（-A）；关时拒掉远端回连来的 agent 通道（fail-safe）。
    agent_forward: bool,
    /// 本连接是否开启 X11 转发；关时拒掉远端回连来的 x11 通道（fail-safe）。
    x11_forward: bool,
    /// 协商结果槽位：`kex_done` 写入，握手方（`ssh::connect`）取走给会话信息面板。
    /// 每次密钥交换（含 rekey）都会刷新，取走的是连接存续期的最终协商状态。
    negotiated: Arc<Mutex<Option<Negotiated>>>,
}

impl ClientHandler {
    pub fn new(
        app: AppHandle,
        host: String,
        port: u16,
        agent_forward: bool,
        x11_forward: bool,
    ) -> (
        Self,
        Arc<Mutex<Option<Issue>>>,
        Arc<Mutex<Option<Negotiated>>>,
    ) {
        let issue = Arc::new(Mutex::new(None));
        let negotiated = Arc::new(Mutex::new(None));
        (
            Self {
                app,
                host,
                port,
                issue: issue.clone(),
                agent_forward,
                x11_forward,
                negotiated: negotiated.clone(),
            },
            issue,
            negotiated,
        )
    }

    /// 远端回连、要取用本机 agent 的通道：接到本机 ssh-agent 后双向泵。
    /// 未开转发 / 本机无 agent → 拒绝，绝不把远端导向未知 socket。
    async fn bridge_agent_channel(
        &self,
        channel: russh::Channel<russh::client::Msg>,
        reply: russh::client::ChannelOpenHandle,
    ) {
        if !self.agent_forward {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return;
        }
        let Some(mut local) = local_agent_stream().await else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return;
        };
        reply.accept().await;
        let mut stream = channel.into_stream();
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut local, &mut stream).await;
        });
    }
}

/// 读写 + Unpin + Send 的组合超集：`dyn` 不能直接并列两个非 auto trait（`AsyncRead`
/// 与 `AsyncWrite`），故用一个组合 trait 收口，具体类型自动实现。
trait AgentIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> AgentIo for T {}

/// 本机 agent 传输的统一抽象（Unix domain socket / Windows 命名管，具体类型经 trait object 擦除）。
type AgentStream = Box<dyn AgentIo>;

/// 连本机 ssh-agent（Unix）：走 `$SSH_AUTH_SOCK`。缺变量或连不上即 `None`。
#[cfg(unix)]
async fn local_agent_stream() -> Option<AgentStream> {
    let path = std::env::var_os("SSH_AUTH_SOCK")?;
    let stream = tokio::net::UnixStream::connect(path).await.ok()?;
    Some(Box::new(stream))
}

/// 连本机 ssh-agent（Windows）：OpenSSH for Windows 的 agent 监听固定命名管。
/// agent 未运行 / 管不存在即 `None`（`open` 同步返回，无需 await）。
#[cfg(windows)]
async fn local_agent_stream() -> Option<AgentStream> {
    use tokio::net::windows::named_pipe::ClientOptions;
    let client = ClientOptions::new()
        .open(r"\\.\pipe\openssh-ssh-agent")
        .ok()?;
    Some(Box::new(client))
}

/// 其余平台暂无本机 agent 桥接实现。
#[cfg(not(any(unix, windows)))]
async fn local_agent_stream() -> Option<AgentStream> {
    None
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        match verify(&self.app, &self.host, self.port, key).await {
            Ok(()) => {
                // 顺手记录密钥类型 + 指纹（公钥材料，非敏感），供会话信息面板展示
                if let PublicKeyOrCertificate::PublicKey { key, .. } = key {
                    *self.negotiated.lock().unwrap() = Some(Negotiated {
                        kex: String::new(),
                        cipher: String::new(),
                        mac_client: String::new(),
                        mac_server: String::new(),
                        compression_client: String::new(),
                        compression_server: String::new(),
                        remote_sshid: String::new(),
                        host_key_type: key.algorithm().as_str().to_string(),
                        host_key_fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                    });
                }
                Ok(true)
            }
            Err(issue) => {
                *self.issue.lock().unwrap() = Some(issue);
                Ok(false)
            }
        }
    }

    /// 密钥交换完成：捕获协商出的算法名与服务器标识（会话信息面板数据源）。
    /// 只读算法名与版本串；`shared_secret` 是敏感密钥材料，一律不读不存。
    async fn kex_done(
        &mut self,
        _shared_secret: Option<&[u8]>,
        names: &russh::Names,
        session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        let sshid = String::from_utf8_lossy(session.remote_sshid()).into_owned();
        let (kex, cipher, mac_c, mac_s, comp_c, comp_s) = (
            names.kex.name_str(),
            names.cipher.name_str(),
            names.client_mac.name_str(),
            names.server_mac.name_str(),
            compression_name(&names.client_compression),
            compression_name(&names.server_compression),
        );
        let mut slot = self.negotiated.lock().unwrap();
        // 两臂各消费一次局部绑定，避免同一值被移动两次
        match slot.as_mut() {
            Some(rec) => {
                rec.kex = kex;
                rec.cipher = cipher;
                rec.mac_client = mac_c;
                rec.mac_server = mac_s;
                rec.compression_client = comp_c;
                rec.compression_server = comp_s;
                rec.remote_sshid = sshid;
            }
            // 理论上 `check_server_key` 先于 `kex_done`；兜底建快照，指纹留空
            None => {
                *slot = Some(Negotiated {
                    kex,
                    cipher,
                    mac_client: mac_c,
                    mac_server: mac_s,
                    compression_client: comp_c,
                    compression_server: comp_s,
                    remote_sshid: sshid,
                    host_key_type: String::new(),
                    host_key_fingerprint: String::new(),
                })
            }
        }
        Ok(())
    }

    /// 远程转发（-R）：服务器回连一条 forwarded-tcpip，接到本地目标后双向泵。
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        use tauri::Manager;
        let target = {
            let st = self.app.state::<crate::state::AppState>();
            st.tunnels.remote_target(connected_address, connected_port)
        };
        let Some((host, port)) = target else {
            reply
                .reject(russh::ChannelOpenFailure::UnknownChannelType)
                .await;
            return Ok(());
        };
        let Ok(mut tcp) = tokio::net::TcpStream::connect((host.as_str(), port)).await else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        reply.accept().await;
        let mut stream = channel.into_stream();
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
        });
        Ok(())
    }

    /// 现代 OpenSSH 的 agent 转发：远端经 forwarded-streamlocal（socket_path 固定为
    /// 字面量 "SSH_AUTH_SOCK"）回连取用本机 agent。其它 streamlocal 转发不支持 → 拒绝。
    async fn server_channel_open_forwarded_streamlocal(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        socket_path: &str,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        if socket_path != "SSH_AUTH_SOCK" {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
        self.bridge_agent_channel(channel, reply).await;
        Ok(())
    }

    /// 旧式 agent 转发请求（auth-agent@openssh.com）：与现代路径共用同一桥接。
    async fn server_channel_open_agent_forward(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        self.bridge_agent_channel(channel, reply).await;
        Ok(())
    }

    /// X11 转发：远端 GUI 程序连上 sshd 的 X11 端口后，服务端开 x11 通道回连。
    /// 桥接到本机 X server（Windows 上常见 VcXsrv/Xming，默认监听 6000 即 display :0）；
    /// 本连接未开 X11 转发时拒绝（fail-safe）。字节转发与 agent 通道同构。
    async fn server_channel_open_x11(
        &mut self,
        channel: russh::Channel<russh::client::Msg>,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::client::ChannelOpenHandle,
        _session: &mut russh::client::Session,
    ) -> Result<(), Self::Error> {
        if !self.x11_forward {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
        // 本地 X server 地址：display :0 → 6000。VcXsrv 默认即此；
        // 后续若需支持自定义 display，可提为设置项（偏移 = 6000 + display 号）。
        let Ok(mut xsrv) = tokio::net::TcpStream::connect(("127.0.0.1", 6000u16)).await else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        reply.accept().await;
        let mut stream = channel.into_stream();
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut xsrv, &mut stream).await;
        });
        Ok(())
    }
}

/// 已信任 → `Ok`；未记录 → 问用户；记录不符/被拒/超时 → `Err`。
async fn verify(
    app: &AppHandle,
    host: &str,
    port: u16,
    key: &PublicKeyOrCertificate,
) -> Result<(), Issue> {
    let public = match key {
        PublicKeyOrCertificate::PublicKey { key, .. } => key,
        PublicKeyOrCertificate::Certificate(_) => {
            return Err(Issue::Unsupported("暂不支持 SSH 证书形式的主机密钥".into()))
        }
    };
    let key_type = public.algorithm().as_str().to_string();
    let blob = base64::engine::general_purpose::STANDARD.encode(
        public
            .to_bytes()
            .map_err(|e| Issue::Unsupported(e.to_string()))?,
    );
    let fingerprint = public.fingerprint(HashAlg::Sha256).to_string();

    let state = app.state::<AppState>();
    let stored = state
        .db
        .get_host_key(host, port, &key_type)
        .map_err(|e| Issue::Backend(e.to_string()))?;
    match stored {
        Some(rec) if rec.key_blob == blob => return Ok(()),
        Some(rec) => {
            return Err(Issue::Mismatch {
                stored: rec.fingerprint,
                offered: fingerprint,
            })
        }
        None => {}
    }

    let request_id = uuid::Uuid::new_v4().to_string();
    let reply = state.host_keys.register(&request_id);
    let _ = app.emit(
        EVENT_HOST_KEY,
        Prompt {
            request_id: request_id.clone(),
            host: host.to_string(),
            port,
            key_type: key_type.clone(),
            fingerprint: fingerprint.clone(),
        },
    );

    match timeout(DECISION_TIMEOUT, reply).await {
        Ok(Ok(true)) => {}
        Ok(_) => return Err(Issue::Denied),
        Err(_) => return Err(Issue::Timeout),
    }
    state
        .db
        .save_host_key(&HostKeyRecord {
            host: host.to_string(),
            port,
            key_type,
            key_blob: blob,
            fingerprint,
            created_at: now_ms(),
        })
        .map_err(|e| Issue::Backend(e.to_string()))?;
    Ok(())
}
