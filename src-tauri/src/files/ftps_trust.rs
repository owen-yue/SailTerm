//! FTPS 服务端证书信任（TOFU）：自定义 rustls 校验器 + 用户裁决回传。
//!
//! 为什么需要这一层：rustls 的链校验器（webpki）只接受 **X.509 v3** 且由受信根签发的证书，
//! 而内网/NAS 递出来的常常是自签、甚至 v1 且无任何扩展（连 SAN 都没有）的证书 —— 对这类目标
//! 「常规校验 + 不提供跳过开关」等于结构性不可用（实测：UGREEN NAS 的 FTPS 报
//! `invalid peer certificate: Other(OtherError(UnsupportedCertVersion))`）。
//!
//! 策略（与 [`crate::ssh::host_key`] 的 SSH 主机密钥同构，不是「跳过校验」）：
//! 1. **先按常规校验**：能过就什么都不发生，公网正规证书零打扰；
//! 2. 过不了才落到 **指纹固定**：首次连接把叶子证书指纹/主体/有效期交给用户裁决，
//!    接受即按 `host:port` 固定 SHA-256 指纹；之后严格比对，指纹变了直接拒（防证书重放/中间人），
//!    需用户在「安全设置 → 已信任的密钥与证书」里显式移除后重连；
//! 3. **CertificateVerify 签名照常校验**（见 [`verify_leaf_signature`]）—— 只固定指纹不验签
//!    会被「把真服务器的证书转发过来、自己没有私钥」的中间人绕过。也正因如此不能把这一步
//!    委托给常规校验器：它对 v1 证书根本解不开，故这里自己从 DER 里读公钥（见 [`der_read`]）。
//!
//! 并发纪律：rustls 的校验回调是**同步**的，不能 `await`，所以「要问用户」与「明确拒绝」
//! 都先记进 [`Slot`]，由 [`crate::files::ftp`] 的建连协程在 async 侧取走处理。

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, RootCertStore, SignatureScheme};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;
use tokio::time::timeout;

use crate::db::ftps_cert::FtpsCertRecord;
use crate::db::now_ms;
use crate::error::AppError;
use crate::state::AppState;

/// 首次遇到不受系统信任的 FTPS 证书时抛给前端的裁决事件。
pub const EVENT_FTPS_CERT: &str = "ftps-cert-unknown";
/// 等待用户裁决上限；超时按拒绝处理（fail-safe，与主机密钥一致）。
const DECISION_TIMEOUT: Duration = Duration::from_secs(120);

/// 抛给前端的确认请求负载。`reasons` 是稳定原因码，前端负责本地化成文案。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    pub request_id: String,
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    /// 证书主体 CN（可能为空串：自签设备经常只填组织名）。
    pub subject: String,
    /// 有效期区间，如 `2026-08-20 ~ 2027-08-20`。
    pub validity: String,
    /// `self_signed` / `legacy_version` / `expired` / `not_yet_valid` / `untrusted`
    pub reasons: Vec<String>,
}

/// 同步校验器 → 异步建连协程的回传槽。
#[derive(Clone, Default)]
pub struct Slot(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    /// 需要用户裁决：请求负载 + 应答通道。
    ask: Option<(Prompt, oneshot::Receiver<bool>)>,
    /// 已明确拒绝（指纹不符），文案即最终用户提示。
    refusal: Option<String>,
    /// 本次握手服务端**呈现**的叶子证书指纹。CA 直通的常规路径也要记：
    /// 否则「证书合法」的连接在会话面板上反而是盲的，用户看不到自己正在跟谁说话。
    presented: Option<String>,
}

impl Slot {
    fn put_ask(&self, prompt: Prompt, rx: oneshot::Receiver<bool>) {
        let mut g = self.0.lock().unwrap();
        g.ask = Some((prompt, rx));
        g.refusal = None;
    }

    fn put_refusal(&self, msg: String) {
        let mut g = self.0.lock().unwrap();
        g.refusal = Some(msg);
        g.ask = None;
    }

    fn put_presented(&self, fingerprint: String) {
        self.0.lock().unwrap().presented = Some(fingerprint);
    }

    /// 读本次握手呈现的证书指纹（`None` = 还没走到 TLS 校验，即明文 FTP）。
    /// **不消耗**：同一个 slot 会被控制连接与后续数据连接反复写，读一次不该弄掉。
    pub fn presented(&self) -> Option<String> {
        self.0.lock().unwrap().presented.clone()
    }

    /// 取走待裁决请求（建连协程用，取后即清空，避免重复弹）。
    pub fn take_ask(&self) -> Option<(Prompt, oneshot::Receiver<bool>)> {
        self.0.lock().unwrap().ask.take()
    }

    /// 取走拒绝原因。
    pub fn take_refusal(&self) -> Option<String> {
        self.0.lock().unwrap().refusal.take()
    }
}

/// 信任根与常规校验器：全进程共用一份（解析 Mozilla 根集合有成本，且内容不变）。
struct Trust {
    roots: Arc<RootCertStore>,
    /// 常规链校验器；万一根集合为空导致构造失败则退化为「一律 TOFU」，不 panic。
    strict: Option<Arc<WebPkiServerVerifier>>,
}

static TRUST: std::sync::OnceLock<Trust> = std::sync::OnceLock::new();

fn trust() -> &'static Trust {
    TRUST.get_or_init(|| {
        let mut store = RootCertStore::empty();
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let roots = Arc::new(store);
        // rustls 0.23 的 builder.build() 已返回 Arc<WebPkiServerVerifier>，无需再套一层 Arc
        let strict = WebPkiServerVerifier::builder(Arc::clone(&roots))
            .build()
            .ok();
        Trust { roots, strict }
    })
}

/// FTPS 客户端 TLS 配置：信任根照旧（供会话恢复等内部逻辑），校验规则换成 [`TofuVerifier`]。
/// 返回 `Arc` 是 [`suppaftp`](crate::files::ftp) 的 `TlsConnector` 所要求的形式，
/// 同一个 config 也得以被控制连接与其后的数据连接共享。
pub fn tls_config(verifier: Arc<dyn ServerCertVerifier>) -> Arc<rustls::ClientConfig> {
    let mut cfg = rustls::ClientConfig::builder()
        .with_root_certificates(Arc::clone(&trust().roots))
        .with_no_client_auth();
    cfg.dangerous().set_certificate_verifier(verifier);
    Arc::new(cfg)
}

/// 为一次建连构造校验器（控制连接与其后的数据连接共用同一个实例，故指纹键固定为
/// 用户填写的 `host:port`，不受 PASV 数据端口变化影响）。
pub fn verifier(
    app: AppHandle,
    host: &str,
    port: u16,
    slot: Slot,
) -> Result<Arc<dyn ServerCertVerifier>, AppError> {
    Ok(Arc::new(TofuVerifier {
        app,
        host: host.to_string(),
        port,
        strict: trust().strict.clone(),
        slot,
    }))
}

/// rustls 校验器：常规链校验优先，失败后按指纹固定 + TOFU。
struct TofuVerifier {
    app: AppHandle,
    host: String,
    port: u16,
    strict: Option<Arc<WebPkiServerVerifier>>,
    slot: Slot,
}

/// `AppHandle` 不保证 `Debug`，而 rustls 要求校验器 `Debug` —— 手搓一个够用的。
impl fmt::Debug for TofuVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TofuVerifier")
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        // 1) 常规校验能过：最理想路径，不弹任何窗
        // 指纹先算（叶子 DER 上一次 SHA-256，成本可忽略）：常规路径不弹窗、不入库，
        // 但会话信息面板要能拿出「本次到底见到了哪张证书」。
        let fingerprint = fingerprint_sha256(end_entity.as_ref());
        self.slot.put_presented(fingerprint.clone());
        if let Some(strict) = &self.strict {
            if strict
                .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
                .is_ok()
            {
                return Ok(ServerCertVerified::assertion());
            }
        }

        // 2) 过不了：自签 / v1 / 过期，改由用户裁决 + 指纹固定
        let der = end_entity.as_ref();
        let leaf = parse_leaf(der).ok_or_else(|| Error::General("服务端证书无法解析".into()))?;

        let state = self.app.state::<AppState>();
        let stored = state
            .db
            .get_ftps_cert(&self.host, self.port)
            .map_err(|e| Error::General(format!("读取证书信任记录失败：{e}")))?;

        if let Some(rec) = stored {
            if rec.fingerprint == fingerprint {
                return Ok(ServerCertVerified::assertion());
            }
            self.slot.put_refusal(format!(
                "证书指纹与已记录的不一致（可能遭中间人攻击）。已记录 {}，本次 {fingerprint}；\
                 确认服务器确实换过证书后，可在「安全设置 → 已信任的密钥与证书」移除记录再连",
                rec.fingerprint
            ));
            return Err(Error::General("FTPS 证书指纹与已记录不符".into()));
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let reply = state.ftps_certs.register(&request_id);
        let prompt = Prompt {
            request_id: request_id.clone(),
            host: self.host.clone(),
            port: self.port,
            fingerprint,
            subject: leaf.common_name.clone(),
            validity: leaf.validity.clone(),
            reasons: leaf.reasons(now),
        };
        let _ = self.app.emit(EVENT_FTPS_CERT, prompt.clone());
        self.slot.put_ask(prompt, reply);
        Err(Error::General("FTPS 证书待用户确认".into()))
    }

    /// TLS 1.2 / 1.3 的 CertificateVerify 校验：两条路径的 `message` 都由 rustls 组装好，
    /// 我们只负责用叶子证书里的公钥验签，故实现相同。
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_leaf_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_leaf_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        SUPPORTED_SCHEMES.to_vec()
    }
}

/// 用户裁决落地：接受即固定指纹，拒绝/超时按拒绝处理（fail-safe）。
///
/// 返回 `Ok(true)` 表示**刚写入了一条新信任**，调用方应当重连一次（这次走指纹相符的快路径）。
pub async fn resolve(
    app: &AppHandle,
    ask: (Prompt, oneshot::Receiver<bool>),
) -> Result<bool, AppError> {
    let (prompt, reply) = ask;
    match timeout(DECISION_TIMEOUT, reply).await {
        Ok(Ok(true)) => {}
        Ok(_) => {
            return Err(AppError::HostKey(
                "你已拒绝该服务器的证书，连接已中止".into(),
            ))
        }
        Err(_) => return Err(AppError::HostKey("证书确认超时，连接已中止".into())),
    }
    let state = app.state::<AppState>();
    state.db.save_ftps_cert(&FtpsCertRecord {
        host: prompt.host,
        port: prompt.port,
        fingerprint: prompt.fingerprint,
        subject: prompt.subject,
        validity: prompt.validity,
        created_at: now_ms(),
    })?;
    Ok(true)
}

// ---------------------------------------------------------------- 验签

/// 支持的签名算法（与 ring 后端能力一致；不含 SHA-1、P-521 与 ML-DSA 等冷门项 ——
/// ring 没有 P-521 验签，声称支持却解不开等于给自己埋雷，故这里宁可不给）。
const SUPPORTED_SCHEMES: &[SignatureScheme] = &[
    SignatureScheme::RSA_PKCS1_SHA256,
    SignatureScheme::RSA_PKCS1_SHA384,
    SignatureScheme::RSA_PKCS1_SHA512,
    SignatureScheme::RSA_PSS_SHA256,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA512,
    SignatureScheme::ECDSA_NISTP256_SHA256,
    SignatureScheme::ECDSA_NISTP384_SHA384,
    SignatureScheme::ED25519,
];

/// 用叶子证书自带的公钥验证握手签名，证明对面真的持有私钥。
///
/// 必须自己做：常规校验器解不开 v1 证书，一委托就在这类设备上误判通过或误判失败。
fn verify_leaf_signature(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, Error> {
    let leaf =
        parse_leaf(cert.as_ref()).ok_or_else(|| Error::General("服务端证书无法解析".into()))?;
    let algorithm = ring_algorithm(leaf.key_alg, dss.scheme).ok_or_else(|| {
        Error::General(format!(
            "服务端证书使用了不受支持的签名算法：{}",
            scheme_label(dss.scheme)
        ))
    })?;
    ring::signature::UnparsedPublicKey::new(algorithm, &leaf.public_key)
        .verify(message, dss.signature())
        .map_err(|_| Error::General("服务端证书签名校验失败（握手可能被拦截）".into()))?;
    Ok(HandshakeSignatureValid::assertion())
}

fn scheme_label(scheme: SignatureScheme) -> String {
    format!("{scheme:?}")
}

/// 按「证书公钥类型 × 握手签名算法」挑出 ring 的验签算法。
///
/// 两者必须自洽：RSA 公钥配 ECDSA 方案（或反过来）一律 `None`，不给交叉验证留缝。
fn ring_algorithm(
    key_alg: KeyAlg,
    scheme: SignatureScheme,
) -> Option<&'static dyn ring::signature::VerificationAlgorithm> {
    use ring::signature as s;
    use SignatureScheme::*;
    let rsa = |alg: &'static s::RsaParameters| match key_alg {
        KeyAlg::Rsa => Some(alg as &dyn s::VerificationAlgorithm),
        _ => None,
    };
    let ec = |alg: &'static s::EcdsaVerificationAlgorithm| match key_alg {
        KeyAlg::Ec => Some(alg as &dyn s::VerificationAlgorithm),
        _ => None,
    };
    match scheme {
        RSA_PKCS1_SHA256 => rsa(&s::RSA_PKCS1_2048_8192_SHA256),
        RSA_PKCS1_SHA384 => rsa(&s::RSA_PKCS1_2048_8192_SHA384),
        RSA_PKCS1_SHA512 => rsa(&s::RSA_PKCS1_2048_8192_SHA512),
        RSA_PSS_SHA256 => rsa(&s::RSA_PSS_2048_8192_SHA256),
        RSA_PSS_SHA384 => rsa(&s::RSA_PSS_2048_8192_SHA384),
        RSA_PSS_SHA512 => rsa(&s::RSA_PSS_2048_8192_SHA512),
        ECDSA_NISTP256_SHA256 => ec(&s::ECDSA_P256_SHA256_ASN1),
        ECDSA_NISTP384_SHA384 => ec(&s::ECDSA_P384_SHA384_ASN1),
        ED25519 if key_alg == KeyAlg::Ed25519 => Some(&s::ED25519),
        _ => None,
    }
}

// ---------------------------------------------------------------- 证书解析

/// 证书公钥类型（决定用哪个 ring 验签算法）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyAlg {
    Rsa,
    Ec,
    Ed25519,
    Other,
}

/// 从证书里读出的、我们需要的几样东西（全是公钥材料，不含任何敏感数据）。
struct Leaf {
    /// X.509 版本号（1/2/3）。v3 才是 TLS 端实体证书的合规形态。
    version: u8,
    key_alg: KeyAlg,
    /// RSA 为 PKCS#1 `RSAPublicKey` DER，EC 为未压缩点，Ed25519 为 32 字节 —— 正是 ring 要的形状。
    public_key: Vec<u8>,
    common_name: String,
    validity: String,
    not_before: Option<i64>,
    not_after: Option<i64>,
    self_signed: bool,
}

impl Leaf {
    /// 归纳「为什么常规校验不放行」，给裁决弹窗当理由（稳定码，前端本地化）。
    fn reasons(&self, now: UnixTime) -> Vec<String> {
        let now = now.as_secs() as i64;
        let mut out = Vec::new();
        if self.version != 3 {
            out.push("legacy_version".into());
        }
        if self.self_signed {
            out.push("self_signed".into());
        }
        if self.not_after.is_some_and(|t| now > t) {
            out.push("expired".into());
        }
        if self.not_before.is_some_and(|t| now < t) {
            out.push("not_yet_valid".into());
        }
        if out.is_empty() {
            out.push("untrusted".into());
        }
        out
    }
}

/// 证书 DER 的 SHA-256 指纹（`SHA256:` + 冒号十六进制，与 `openssl x509 -fingerprint` 同形）。
pub fn fingerprint_sha256(der: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, der);
    let hex = digest
        .as_ref()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    format!("SHA256:{hex}")
}

/// 解析叶子证书：取公钥、主体 CN、有效期、版本与是否自签。
fn parse_leaf(der: &[u8]) -> Option<Leaf> {
    let mut top = der;
    let cert = der_read::next(&mut top)?;
    if cert.tag != TAG_SEQ {
        return None;
    }
    // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signatureValue }
    let tbs = der_read::children(cert.val).into_iter().next()?;
    if tbs.tag != TAG_SEQ {
        return None;
    }
    // TBSCertificate 的字段顺序固定，只有开头的 version 是可选的 [0] EXPLICIT
    let items = der_read::children(tbs.val);
    let mut i = 0usize;
    let mut version = 1u8;
    if items.get(i)?.tag == 0xA0 {
        let raw = der_read::children(items[i].val).into_iter().next()?;
        version = raw.val.first().copied().unwrap_or(2) + 1;
        i += 1;
    }
    // serialNumber, signature, issuer, validity, subject, subjectPublicKeyInfo
    if items.len() < i + 6 {
        return None;
    }
    let (issuer, validity, subject, spki) =
        (items[i + 2], items[i + 3], items[i + 4], items[i + 5]);

    let times = der_read::children(validity.val);
    if times.len() < 2 {
        return None;
    }
    let not_before = as_epoch(&times[0]);
    let not_after = as_epoch(&times[1]);
    let validity_text = match (fmt_date(not_before), fmt_date(not_after)) {
        (Some(a), Some(b)) => format!("{a} ~ {b}"),
        _ => "未知".into(),
    };

    // SubjectPublicKeyInfo ::= SEQUENCE { AlgorithmIdentifier, subjectPublicKey BIT STRING }
    let parts = der_read::children(spki.val);
    if parts.len() < 2 || parts[1].tag != TAG_BITS {
        return None;
    }
    let alg_oid = der_read::children(parts[0].val).into_iter().next()?;
    let key_alg = match alg_oid.val {
        OID_RSA => KeyAlg::Rsa,
        OID_EC => KeyAlg::Ec,
        OID_ED25519 => KeyAlg::Ed25519,
        _ => KeyAlg::Other,
    };
    // BIT STRING 首字节是「未使用位数」，公钥必须整字节对齐
    let unused = *parts[1].val.first()?;
    if unused != 0 {
        return None;
    }
    let public_key = parts[1].val.get(1..)?.to_vec();

    Some(Leaf {
        version,
        key_alg,
        public_key,
        common_name: common_name(&subject),
        validity: validity_text,
        not_before,
        not_after,
        self_signed: issuer.val == subject.val,
    })
}

/// 从 X.509 `Name`（RDNSequence）里挑出全部 CN，多个则以逗号连接。
fn common_name(name: &der_read::Tlv<'_>) -> String {
    const CN: &[u8] = &[0x55, 0x04, 0x03]; // 2.5.4.3
    let mut out = Vec::new();
    for rdn in der_read::children(name.val) {
        for atv in der_read::children(rdn.val) {
            let parts = der_read::children(atv.val);
            if parts.len() >= 2 && parts[0].val == CN {
                let value = String::from_utf8_lossy(parts[1].val).into_owned();
                if !value.is_empty() {
                    out.push(value);
                }
            }
        }
    }
    out.join(",")
}

const TAG_SEQ: u8 = 0x30;
const TAG_BITS: u8 = 0x03;
/// rsaEncryption 1.2.840.113549.1.1.1
const OID_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
/// ecPublicKey 1.2.840.10045.2.1
const OID_EC: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
/// Ed25519 1.3.101.112
const OID_ED25519: &[u8] = &[0x2B, 0x65, 0x70];

/// 极简 DER（ASN.1 TLV）游标。
///
/// 只为「从证书里取公钥 / 主体 / 有效期」这一件事服务：只实现证书真正用到的构造与原子类型，
/// 不做通用 ASN.1，也不引入 x509 解析 crate（见 Cargo.toml 的极简依赖纪律）。
mod der_read {
    /// 一个 TLV：tag 字节 + 已按长度截好的内容切片。
    #[derive(Clone, Copy)]
    pub struct Tlv<'a> {
        pub tag: u8,
        pub val: &'a [u8],
    }

    /// 读一个 TLV。长度只支持确定型短/长形式（证书里不会出现不定长）。
    pub fn next<'a>(buf: &mut &'a [u8]) -> Option<Tlv<'a>> {
        let tag = *buf.first()?;
        *buf = &buf[1..];
        let first = *buf.first()?;
        *buf = &buf[1..];
        let len = if first < 0x80 {
            usize::from(first)
        } else {
            let n = usize::from(first & 0x7F);
            if n == 0 || n > 3 {
                return None;
            }
            let bytes = buf.get(..n)?;
            *buf = &buf[n..];
            bytes
                .iter()
                .fold(0usize, |acc, b| (acc << 8) | usize::from(*b))
        };
        let val = buf.get(..len)?;
        *buf = &buf[len..];
        Some(Tlv { tag, val })
    }

    /// 构造类型（SEQUENCE / SET / `[0]`）的子元素序列；遇到解析不上的尾巴即停止，
    /// 由调用方按「字段够不够」判断证书是否可信。
    pub fn children(val: &[u8]) -> Vec<Tlv<'_>> {
        let mut rest = val;
        let mut out = Vec::new();
        while let Some(t) = next(&mut rest) {
            out.push(t);
        }
        out
    }
}

// ---------------------------------------------------------------- 时间

/// `UTCTime` / `GeneralizedTime` → unix 秒（均按 UTC 解释，与 X.509 一致）。
fn as_epoch(tlv: &der_read::Tlv<'_>) -> Option<i64> {
    let digits: Vec<u8> = tlv.val.iter().copied().filter(u8::is_ascii_digit).collect();
    let yy = match digits.len() {
        12 => 2,
        14 => 4,
        _ => return None,
    };
    let num = |at: usize| -> Option<i64> {
        digits[at..at + 2].iter().try_fold(0i64, |acc, d| {
            acc.checked_mul(10)?.checked_add(i64::from(d - b'0'))
        })
    };
    // 年份位数是两者唯一的结构差异：UTCTime 两位、GeneralizedTime 四位，其后字段对齐
    let year = if yy == 4 {
        num(0)? * 100 + num(2)?
    } else {
        // RFC 5280：UTCTime 的两位年份以 50 为界分属 20xx / 19xx
        let y = num(0)?;
        y + if y < 50 { 2000 } else { 1900 }
    };
    let (month, day) = (num(yy)?, num(yy + 2)?);
    let (hour, min, sec) = (num(yy + 4)?, num(yy + 6)?, num(yy + 8)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + min * 60 + sec)
}

/// 公历日期 → 距 unix 纪元的天数（Howard Hinnant 的 days_from_civil，免引 chrono）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 展示用日期（`2026-08-20`）；无法解析时 `None`。
fn fmt_date(secs: Option<i64>) -> Option<String> {
    let secs = secs?;
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实样本：某 UGREEN NAS 递出的 FTPS 叶子证书（X.509 **v1**、自签、无任何扩展、RSA-2048）。
    /// 就是它把 rustls 的链校验顶到了 `UnsupportedCertVersion` —— 公钥材料，非敏感。
    const NAS_V1: &str = "MIIDOzCCAiMCFA3zGFeXps+bJ7hFlssQu5t4DGRJMA0GCSqGSIb3DQEBCwUAMFoxCzAJBgNVBAYTAkNOMQswCQYDVQQIDAJHRDEL\
MAkGA1UEBwwCU1oxDzANBgNVBAoMBlVHUkVFTjEPMA0GA1UECwwGVUdSRUVOMQ8wDQYDVQQDDAZVR1JFRU4wHhcNMjYwODIwMDIw\
MDAyWhcNMjcwODIwMDIwMDAyWjBaMQswCQYDVQQGEwJDTjELMAkGA1UECAwCR0QxCzAJBgNVBAcMAlNaMQ8wDQYDVQQKDAZVR1JF\
RU4xDzANBgNVBAsMBlVHUkVFTjEPMA0GA1UEAwwGVUdSRUVOMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAqIIj0PIS\
o6pb+LnhOmLmaLGOfZE7zukevl/oAJf644sokVh1n2UjeC4IZHQLOu2w5biYfZr0eCif8c1Whe7i8z9ruKJeeAKKrnXiglYVBTKv\
4LbP41PWP2KqaZu3bwxGaRPabmHdns4gt4C6X73eRFSzlZldjkWVf4WYrzm7nGGIlai4FGyrEWUVr/xfYtPTw/FxVrRbvRp17Qs4\
Jq3dNG2qgY9AS3PtU1wTb0lI6NxumU4k28s0jRDJ9+bz/qhlYPZv9RFNmsv8ivwaWaETaTk89UVcuF/KbiZmLzOGnE7cPJlhB1qS\
EnD+FyPL3ACFfKumdQ+OS09MGhlRYL/vNQIDAQABMA0GCSqGSIb3DQEBCwUAA4IBAQBCqA+rdMjZolL75TWAEO/iJeYB/Q0uyVtj\
mKPfZoV2K6nD++UD/icH9cp3qxADBoMeSeMqRfhlbp3m8rMX9HSwjJ1JVQ31mh64FY3zNQP7WDGB53JS45lb8JuHCA/y4ebQSr7O\
GD0F5VmFfijZZ1Qi/hHJD/GvAIPGsd1PooGM44QoWftaeN1UOgBlaVX5Pdb6gXHX0CfTgyZlSCE78UcIoIbgM6hkTDl3F1SXUe0Z\
F8T+f3kGbqvwz37x5e2ixv+nL/ARq79IS0TIoIB7IlceIpJ0yx/jA54MGrwvjCCZQj4kckB5XK7uIJVj5sl7T4MIbADXmpHETpaa\
fpYdxy5P";

    fn nas_cert() -> Vec<u8> {
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, NAS_V1).unwrap()
    }

    #[test]
    fn reads_v1_self_signed_cert() {
        let der = nas_cert();
        let leaf = parse_leaf(&der).expect("v1 证书也该解析得出来");
        assert_eq!(leaf.version, 1, "NAS 这张证书是 X.509 v1");
        assert_eq!(leaf.key_alg, KeyAlg::Rsa);
        assert!(leaf.self_signed);
        assert_eq!(leaf.common_name, "UGREEN");
        assert_eq!(leaf.validity, "2026-08-20 ~ 2027-08-20");
        // RSA 公钥取的是 PKCS#1 RSAPublicKey DER（ring 要的形状），开头应是 SEQUENCE + 2048bit 模数
        assert_eq!(leaf.public_key[0], TAG_SEQ);
        assert!(leaf.public_key.len() > 250);
    }

    #[test]
    fn fingerprint_matches_openssl() {
        // 与 `openssl x509 -fingerprint -sha256` 对同一张证书的输出一字不差
        assert_eq!(
            fingerprint_sha256(&nas_cert()),
            "SHA256:52:57:9F:45:6E:3B:C5:42:62:7E:42:15:90:8D:87:17:12:F1:E2:FC:78:7F:D1:36:E4:A5:59:F0:AA:D2:2F:A8"
        );
    }

    #[test]
    fn reasons_flag_legacy_and_self_signed() {
        let leaf = parse_leaf(&nas_cert()).unwrap();
        let now = UnixTime::since_unix_epoch(std::time::Duration::from_secs(
            (leaf.not_after.unwrap() - 3600) as u64,
        ));
        let reasons = leaf.reasons(now);
        assert!(reasons.contains(&"legacy_version".to_string()));
        assert!(reasons.contains(&"self_signed".to_string()));
        assert!(!reasons.contains(&"expired".to_string()));
        // 过期后应给出 expired 理由
        let late = UnixTime::since_unix_epoch(std::time::Duration::from_secs(
            (leaf.not_after.unwrap() + 3600) as u64,
        ));
        assert!(leaf.reasons(late).contains(&"expired".to_string()));
    }

    #[test]
    fn rejects_mismatched_key_scheme() {
        // EC 方案配 RSA 公钥：不给交叉验证留缝
        assert!(ring_algorithm(KeyAlg::Rsa, SignatureScheme::ECDSA_NISTP256_SHA256).is_none());
        assert!(ring_algorithm(KeyAlg::Ec, SignatureScheme::RSA_PKCS1_SHA256).is_none());
        assert!(ring_algorithm(KeyAlg::Rsa, SignatureScheme::RSA_PKCS1_SHA256).is_some());
        assert!(ring_algorithm(KeyAlg::Rsa, SignatureScheme::RSA_PSS_SHA384).is_some());
    }

    #[test]
    fn civil_date_roundtrip() {
        // 2026-08-20 02:00:02 UTC 的 unix 秒
        let secs = days_from_civil(2026, 8, 20) * 86_400 + 2 * 3600 + 2;
        assert_eq!(fmt_date(Some(secs)).as_deref(), Some("2026-08-20"));
        assert_eq!(fmt_date(Some(0)).as_deref(), Some("1970-01-01"));
        // 闰日与跨年边界
        assert_eq!(fmt_date(Some(-1)).as_deref(), Some("1969-12-31"));
        assert_eq!(
            fmt_date(Some(days_from_civil(2024, 2, 29) * 86_400)).as_deref(),
            Some("2024-02-29")
        );
    }

    #[test]
    fn parses_generalized_time_too() {
        // GeneralizedTime（14 位）与 UTCTime（12 位）都要能吃下
        let g = der_read::Tlv {
            tag: 0x18,
            val: b"20260820020002Z",
        };
        let u = der_read::Tlv {
            tag: 0x17,
            val: b"260820020002Z",
        };
        assert_eq!(as_epoch(&g), as_epoch(&u));
        assert_eq!(fmt_date(as_epoch(&u)).as_deref(), Some("2026-08-20"));
    }
}
