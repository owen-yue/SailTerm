//! SSH 隧道 / 端口转发运行时。
//!
//! v1 只实现**本地转发**（`-L`）：在本机监听一个 TCP 端口，每来一个连接就在**既有 SSH
//! 会话**上开一条 `direct-tcpip` 通道，把两端字节对拷（`copy_bidirectional`）。复用会话
//! 已认证的 [`SshHandle`]，绝不二次索取凭证；隧道配置只存地址端口，
//! 不落任何敏感信息（见 [`crate::db::tunnel`]）。
//!
//! 三类转发均已落地：本地 `-L`、远程 `-R`（`tcpip-forward` + `forwarded-tcpip`）、
//! 动态 `-D`（本机 SOCKS5，CONNECT 后 `direct-tcpip`）。
//!
//! 生命周期：[`TunnelManager`] 挂在 [`crate::state::AppState`]，每条运行中的转发对应一个
//! 监听任务 + 一个取消信号；会话断开/关闭时 [`crate::session::teardown`] 调
//! [`TunnelManager::close_session`] 一并收掉，避免监听套接字泄漏。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tokio::io;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::error::AppError;
use crate::session::SshHandle;

/// 一条**正在运行**的转发（回前端展示用；不含凭证）。
///
/// 字段语义随 `kind` 变化：`local` 是本机监听→远端目标；`remote` 是远端监听→本地目标；
/// `dynamic` 只有本机 SOCKS 监听（`remote_*` 为占位）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTunnel {
    /// 运行时句柄 id（区别于配置 id：同一条配置可在不同会话上多次开启）。
    pub run_id: String,
    /// 承载该转发的 SSH 会话。
    pub session_id: String,
    pub name: String,
    /// `local` / `remote` / `dynamic`。
    pub kind: String,
    /// `local`/`dynamic`：本机监听地址；`remote`：本地回连目标主机。
    pub local_host: String,
    /// **实际**端口（配置填 0 时为系统分配值）。
    pub local_port: u16,
    /// `local`：远端目标主机；`remote`：远端监听主机；`dynamic`：`socks5`。
    pub remote_host: String,
    pub remote_port: u16,
}

/// 运行中的一条转发：展示元数据 + 取消信号发送端。
///
/// 取消信号被「监听任务」与「每条在转的连接」共同观察，`close` 时一次性收掉全部。
struct Running {
    meta: ActiveTunnel,
    cancel: watch::Sender<bool>,
}

/// bind 失败 → 可行动的文案（纯函数，占用者信息由调用方从注册表反查后带入）。
///
/// bind 本身就是端口预检：失败即真实占用，不存在探测与监听之间的竞态。
/// 这里只把两类最常被混为一谈的失败分开说清楚：端口被占 vs 特权端口无权限。
fn bind_fail_text(label: &str, host: &str, port: u16, e: &std::io::Error, occupier: Option<&str>) -> String {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::AddrInUse => match occupier {
            Some(name) => format!("{label}端口 {host}:{port} 已被本应用隧道「{name}」占用，先关闭那条再开启"),
            None => format!("{label}端口 {host}:{port} 已被其他程序占用，换一个端口或先停掉占用进程"),
        },
        // unix 下 bind <1024 非 root 报权限错；windows 无此限制不会走到这分支
        K::PermissionDenied if port < 1024 => {
            format!("端口 {port} 是系统保留端口（小于 1024），需要管理员权限才能监听，建议改用 1024 以上")
        }
        _ => format!("{label}端口 {host}:{port} 监听失败：{e}"),
    }
}

/// 监听地址归一：空/localhost → 回环；其余原样（不做 DNS 解析，冲突判定只防最常见的写法差异）。
fn norm_listen_host(h: &str) -> String {
    let t = h.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("localhost") {
        "127.0.0.1".into()
    } else {
        t.into()
    }
}

/// 在已知监听列表里找与 `want` 冲突的那条（占用者反查，纯函数便于单测）。
///
/// 列表项为 `(监听地址, 实际端口, 隧道名)`；通配监听（0.0.0.0 / ::）覆盖所有地址，同样算冲突。
fn find_local_conflict<'a>(
    listeners: impl IntoIterator<Item = &'a (String, u16, String)>,
    want: (&str, u16),
) -> Option<&'a str> {
    let (h, p) = want;
    listeners
        .into_iter()
        .find(|(host, port, _)| {
            *port == p && (host == h || host == "0.0.0.0" || (host == "::" && !h.contains('.')))
        })
        .map(|(_, _, name)| name.as_str())
}

/// 全局隧道注册表：`run_id → 运行中的转发`。
///
/// `Mutex` 只在增删查的瞬间持有，**绝不跨 `await`**（转发泵在独立任务里跑）。
#[derive(Default)]
pub struct TunnelManager {
    inner: Mutex<HashMap<String, Running>>,
    seq: AtomicU64,
    /// 远程转发映射：`addr:port`（远端监听）→ 本地目标 `host:port`。
    /// [`ClientHandler`] 收到 `forwarded-tcpip` 时查此表决定连到哪里。
    remote_map: Mutex<HashMap<String, (String, u16)>>,
}

impl TunnelManager {
    fn next_run_id(&self) -> String {
        format!("t{}", self.seq.fetch_add(1, Ordering::Relaxed))
    }

    /// 本机监听套接字（-L / -D 共用）：失败时把错误翻译成可行动的文案。
    ///
    /// bind 失败即真实占用（无 TOCTOU）；占用者若是我们自己另一条在跑的隧道，
    /// 从注册表反查出名字带进提示，用户才知道该关哪条。
    async fn bind_local(
        &self,
        label: &str,
        local_host: &str,
        local_port: u16,
    ) -> Result<TcpListener, AppError> {
        match TcpListener::bind((local_host, local_port)).await {
            Ok(l) => Ok(l),
            Err(e) => {
                let occupier = if e.kind() == std::io::ErrorKind::AddrInUse && local_port != 0 {
                    let want = (norm_listen_host(local_host), local_port);
                    let listeners: Vec<(String, u16, String)> = self
                        .inner
                        .lock()
                        .unwrap()
                        .values()
                        .filter(|r| r.meta.kind == "local" || r.meta.kind == "dynamic")
                        .map(|r| (r.meta.local_host.clone(), r.meta.local_port, r.meta.name.clone()))
                        .collect();
                    find_local_conflict(&listeners, (&want.0, want.1)).map(str::to_string)
                } else {
                    None
                };
                Err(AppError::Io(bind_fail_text(
                    label,
                    local_host,
                    local_port,
                    &e,
                    occupier.as_deref(),
                )))
            }
        }
    }

    /// 开启一条本地转发：在 `local_host:local_port` 监听（`local_port=0` 由系统分配随机端口），
    /// 每个接入连接都经 `handle` 在既有 SSH 会话上开 `direct-tcpip` 到 `remote_host:remote_port`。
    ///
    /// 失败：端口被占用 / 特权端口无权限 / 地址非法 → [`AppError::Io`]（已按占用来源分级措辞）。
    #[allow(clippy::too_many_arguments)]
    pub async fn open_local(
        &self,
        session_id: &str,
        name: &str,
        local_host: &str,
        local_port: u16,
        remote_host: &str,
        remote_port: u16,
        handle: SshHandle,
    ) -> Result<ActiveTunnel, AppError> {
        let listener = self.bind_local("本地", local_host, local_port).await?;
        let actual_port = listener.local_addr()?.port();

        let meta = ActiveTunnel {
            run_id: self.next_run_id(),
            session_id: session_id.to_string(),
            name: name.to_string(),
            kind: "local".into(),
            local_host: local_host.to_string(),
            local_port: actual_port,
            remote_host: remote_host.to_string(),
            remote_port,
        };

        // 取消信号：close/close_session 时 send(true)，监听任务与所有在转连接同时收尾。
        // 两个 receiver 分工，避开 select! 对同一变量的可变/不可变借用冲突。
        let (cancel, cancel_rx) = watch::channel(false);
        let mut accept_cancel = cancel_rx.clone();
        let remote_host = remote_host.to_string();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((mut tcp, peer)) => {
                            let h = handle.clone();
                            let rh = remote_host.clone();
                            let mut conn_cancel = cancel_rx.clone();
                            tokio::spawn(async move {
                                // 在既有连接上开 direct-tcpip；originator 填真实来源，便于远端审计
                                let channel = match h
                                    .channel_open_direct_tcpip(
                                        rh,
                                        remote_port as u32,
                                        peer.ip().to_string(),
                                        peer.port() as u32,
                                    )
                                    .await
                                {
                                    Ok(c) => c,
                                    Err(_) => return,
                                };
                                let mut stream = channel.into_stream();
                                tokio::select! {
                                    // 双向泵：任一端 EOF/出错即结束，随后 stream 丢弃自动关通道
                                    _ = io::copy_bidirectional(&mut tcp, &mut stream) => {}
                                    _ = conn_cancel.changed() => {}
                                }
                            });
                        }
                        // 监听套接字出错（如被系统关闭）：结束监听任务
                        Err(_) => break,
                    },
                    _ = accept_cancel.changed() => break,
                }
            }
        });

        self.inner.lock().unwrap().insert(
            meta.run_id.clone(),
            Running {
                meta: meta.clone(),
                cancel,
            },
        );
        Ok(meta)
    }

    /// 开启远程转发（`-R`）：在**远端** `listen_host:listen_port` 监听，
    /// 每个接入连接回连到本地 `local_host:local_port`。
    /// 依赖服务端 `AllowTcpForwarding`；端口 0 = 服务器随机分配（返回实际端口）。
    #[allow(clippy::too_many_arguments)]
    pub async fn open_remote(
        &self,
        session_id: &str,
        name: &str,
        listen_host: &str,
        listen_port: u16,
        local_host: &str,
        local_port: u16,
        handle: SshHandle,
    ) -> Result<ActiveTunnel, AppError> {
        let actual = handle
            .tcpip_forward(listen_host, listen_port as u32)
            .await
            .map_err(|e| {
                AppError::Ai(format!(
                    "远程转发失败（服务器可能未开 AllowTcpForwarding）：{e}"
                ))
            })?;
        let actual_port = if actual == 0 {
            listen_port
        } else {
            actual as u16
        };
        let key = format!("{listen_host}:{actual_port}");
        self.remote_map
            .lock()
            .unwrap()
            .insert(key.clone(), (local_host.to_string(), local_port));

        // 展示语义：local_* = 本地回连目标，remote_* = 远端实际监听（端口 0 时为服务器分配值）。
        let meta = ActiveTunnel {
            run_id: self.next_run_id(),
            session_id: session_id.to_string(),
            name: name.to_string(),
            kind: "remote".into(),
            local_host: local_host.to_string(),
            local_port,
            remote_host: listen_host.to_string(),
            remote_port: actual_port,
        };
        let (cancel, cancel_rx) = watch::channel(false);
        {
            let key = key.clone();
            let h = handle.clone();
            let host = listen_host.to_string();
            tokio::spawn(async move {
                let mut rx = cancel_rx;
                let _ = rx.changed().await;
                let _ = h.cancel_tcpip_forward(host, actual_port as u32).await;
                let _ = key;
            });
        }
        self.inner.lock().unwrap().insert(
            meta.run_id.clone(),
            Running {
                meta: meta.clone(),
                cancel,
            },
        );
        Ok(meta)
    }

    /// 动态转发（`-D`，SOCKS5）：本机 `local_host:local_port` 起 SOCKS 代理，
    /// 每个 CONNECT 经 SSH `direct-tcpip` 到目标。
    pub async fn open_socks(
        &self,
        session_id: &str,
        name: &str,
        local_host: &str,
        local_port: u16,
        handle: SshHandle,
    ) -> Result<ActiveTunnel, AppError> {
        let listener = self.bind_local("SOCKS", local_host, local_port).await?;
        let actual_port = listener.local_addr()?.port();
        let meta = ActiveTunnel {
            run_id: self.next_run_id(),
            session_id: session_id.to_string(),
            name: name.to_string(),
            kind: "dynamic".into(),
            local_host: local_host.to_string(),
            local_port: actual_port,
            remote_host: "socks5".to_string(),
            remote_port: 0,
        };
        let (cancel, cancel_rx) = watch::channel(false);
        let mut accept_cancel = cancel_rx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((tcp, _)) => {
                            let h = handle.clone();
                            let mut conn_cancel = cancel_rx.clone();
                            tokio::spawn(async move {
                                let _ = Self::socks5_connect_and_pump(tcp, h, &mut conn_cancel).await;
                            });
                        }
                        Err(_) => break,
                    },
                    _ = accept_cancel.changed() => break,
                }
            }
        });
        self.inner.lock().unwrap().insert(
            meta.run_id.clone(),
            Running {
                meta: meta.clone(),
                cancel,
            },
        );
        Ok(meta)
    }

    /// SOCKS5（仅 CONNECT、无认证）：握手 → 取目标 → SSH direct-tcpip → 双向泵。
    async fn socks5_connect_and_pump(
        mut tcp: tokio::net::TcpStream,
        handle: SshHandle,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), AppError> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // 方法协商：05 01 00
        let mut buf = [0u8; 3];
        tcp.read_exact(&mut buf)
            .await
            .map_err(|e| AppError::Io(e.to_string()))?;
        if buf[0] != 5 {
            return Err(AppError::Io("非 SOCKS5".into()));
        }
        tcp.write_all(&[5, 0]).await.ok();
        // 请求：05 01 00 ATYP ...
        let mut hdr = [0u8; 4];
        tcp.read_exact(&mut hdr)
            .await
            .map_err(|e| AppError::Io(e.to_string()))?;
        if hdr[0] != 5 || hdr[1] != 1 {
            tcp.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await.ok();
            return Err(AppError::Io("仅支持 SOCKS5 CONNECT".into()));
        }
        let (host, port) = match hdr[3] {
            1 => {
                let mut ip = [0u8; 4];
                tcp.read_exact(&mut ip)
                    .await
                    .map_err(|e| AppError::Io(e.to_string()))?;
                let mut p = [0u8; 2];
                tcp.read_exact(&mut p)
                    .await
                    .map_err(|e| AppError::Io(e.to_string()))?;
                (
                    std::net::IpAddr::from(ip).to_string(),
                    u16::from_be_bytes(p),
                )
            }
            3 => {
                let mut n = [0u8; 1];
                tcp.read_exact(&mut n)
                    .await
                    .map_err(|e| AppError::Io(e.to_string()))?;
                let mut name = vec![0u8; n[0] as usize];
                tcp.read_exact(&mut name)
                    .await
                    .map_err(|e| AppError::Io(e.to_string()))?;
                let mut p = [0u8; 2];
                tcp.read_exact(&mut p)
                    .await
                    .map_err(|e| AppError::Io(e.to_string()))?;
                (
                    String::from_utf8_lossy(&name).into_owned(),
                    u16::from_be_bytes(p),
                )
            }
            4 => {
                let mut ip = [0u8; 16];
                tcp.read_exact(&mut ip)
                    .await
                    .map_err(|e| AppError::Io(e.to_string()))?;
                let mut p = [0u8; 2];
                tcp.read_exact(&mut p)
                    .await
                    .map_err(|e| AppError::Io(e.to_string()))?;
                (
                    std::net::IpAddr::from(ip).to_string(),
                    u16::from_be_bytes(p),
                )
            }
            _ => return Err(AppError::Io("不支持的 SOCKS 地址类型".into())),
        };
        let channel = handle
            .channel_open_direct_tcpip(host, port as u32, "127.0.0.1".to_string(), 0u32)
            .await
            .map_err(|e| AppError::Ai(format!("SOCKS 转发失败：{e}")))?;
        // 成功应答
        tcp.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.ok();
        let mut stream = channel.into_stream();
        tokio::select! {
            _ = io::copy_bidirectional(&mut tcp, &mut stream) => {}
            _ = cancel.changed() => {}
        }
        Ok(())
    }

    /// 远端来了一个 forwarded-tcpip：查本地目标。
    pub fn remote_target(&self, listen_host: &str, listen_port: u32) -> Option<(String, u16)> {
        self.remote_map
            .lock()
            .unwrap()
            .get(&format!("{listen_host}:{listen_port}"))
            .cloned()
            .or_else(|| {
                // 服务器可能回 0.0.0.0 / :: 等，按端口兜底匹配
                self.remote_map
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(k, _)| k.ends_with(&format!(":{listen_port}")))
                    .map(|(_, v)| v.clone())
            })
    }

    /// 关闭一条运行中的转发（按 `run_id`）；返回是否确实关掉了。
    pub fn close(&self, run_id: &str) -> bool {
        match self.inner.lock().unwrap().remove(run_id) {
            Some(r) => {
                let _ = r.cancel.send(true);
                true
            }
            None => false,
        }
    }

    /// 列运行中的转发：给了 `session_id` 只列该会话的，否则列全部；按本地端口稳定排序。
    pub fn list(&self, session_id: Option<&str>) -> Vec<ActiveTunnel> {
        let mut v: Vec<ActiveTunnel> = self
            .inner
            .lock()
            .unwrap()
            .values()
            .filter(|r| match session_id {
                Some(s) => r.meta.session_id == s,
                None => true,
            })
            .map(|r| r.meta.clone())
            .collect();
        v.sort_by(|a, b| {
            a.local_port
                .cmp(&b.local_port)
                .then(a.remote_port.cmp(&b.remote_port))
                .then(a.name.cmp(&b.name))
        });
        v
    }

    /// 会话断开/关闭时收掉它名下所有转发（[`crate::session::teardown`] 调用）。
    pub fn close_session(&self, session_id: &str) {
        let mut map = self.inner.lock().unwrap();
        let doomed: Vec<String> = map
            .iter()
            .filter(|(_, r)| r.meta.session_id == session_id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in doomed {
            if let Some(r) = map.remove(&k) {
                let _ = r.cancel.send(true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listeners() -> Vec<(String, u16, String)> {
        vec![
            ("127.0.0.1".into(), 8080, "web".into()),
            ("0.0.0.0".into(), 9000, "any".into()),
        ]
    }

    #[test]
    fn conflict_finds_same_host_and_wildcard() {
        let l = listeners();
        assert_eq!(find_local_conflict(&l, ("127.0.0.1", 8080)), Some("web"));
        // 通配监听占住了同端口的任意地址
        assert_eq!(find_local_conflict(&l, ("127.0.0.1", 9000)), Some("any"));
        assert_eq!(find_local_conflict(&l, ("127.0.0.1", 7070)), None);
        assert_eq!(find_local_conflict(&l, ("192.168.1.5", 8080)), None);
    }

    #[test]
    fn listen_host_normalized() {
        assert_eq!(norm_listen_host(""), "127.0.0.1");
        assert_eq!(norm_listen_host("Localhost"), "127.0.0.1");
        assert_eq!(norm_listen_host(" 0.0.0.0 "), "0.0.0.0");
        assert_eq!(norm_listen_host("192.168.1.5"), "192.168.1.5");
    }

    #[test]
    fn bind_fail_text_distinguishes_occupier_and_privileged() {
        let in_use = std::io::Error::from(std::io::ErrorKind::AddrInUse);
        assert!(
            bind_fail_text("本地", "127.0.0.1", 8080, &in_use, Some("web")).contains("隧道「web」")
        );
        let other = bind_fail_text("本地", "127.0.0.1", 8080, &in_use, None);
        assert!(other.contains("其他程序") && other.contains("8080"));
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(bind_fail_text("本地", "0.0.0.0", 80, &denied, None).contains("系统保留端口"));
        // 高端口上的权限错（如 SELinux/沙盒拦截）不归因特权端口
        assert!(bind_fail_text("本地", "0.0.0.0", 8080, &denied, None).contains("监听失败"));
    }

    #[tokio::test]
    async fn bind_local_reports_our_own_tunnel_as_occupier() {
        // 先占住一个真实端口，再走 bind_local：应反查出注册表里那条隧道的名字
        let held = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = held.local_addr().unwrap().port();

        let mgr = TunnelManager::default();
        let (cancel, _rx) = watch::channel(false);
        mgr.inner.lock().unwrap().insert(
            "r1".into(),
            Running {
                meta: ActiveTunnel {
                    run_id: "r1".into(),
                    session_id: "s1".into(),
                    name: "测试隧道".into(),
                    kind: "local".into(),
                    local_host: "127.0.0.1".into(),
                    local_port: port,
                    remote_host: "example".into(),
                    remote_port: 22,
                },
                cancel,
            },
        );

        let err = mgr
            .bind_local("本地", "127.0.0.1", port)
            .await
            .expect_err("端口已被占用，应失败");
        let AppError::Io(text) = err else {
            panic!("预期 Io 错误，实际：{err:?}");
        };
        assert!(text.contains("测试隧道"), "提示应点名占用者：{text}");
    }

    #[tokio::test]
    async fn bind_local_foreign_occupier_says_other_program() {
        let held = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = held.local_addr().unwrap().port();

        let mgr = TunnelManager::default();
        let err = mgr
            .bind_local("本地", "127.0.0.1", port)
            .await
            .expect_err("端口已被占用，应失败");
        let AppError::Io(text) = err else {
            panic!("预期 Io 错误，实际：{err:?}");
        };
        assert!(text.contains("其他程序"), "注册表无匹配时应指向外部占用：{text}");
    }
}
