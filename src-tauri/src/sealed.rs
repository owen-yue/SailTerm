//! 备份凭据包的密封加密（主密码 → 加解密一小段 JSON 凭据负载）。
//!
//! 用途：仅在「导出完整备份（含密钥）」时启用。把明文的 [`crate::commands::profiles::CredentialBundle`]
//! 用用户主密码加密成一个可移植的密文块，塞进备份 JSON 的 `sealed` 字段。**默认导出不走这里**
//! （仍是不含凭据的明文 JSON），确保随手分享配置不会泄密。
//!
//! 设计约束（对齐全树 ring-only、零新增依赖的哲学，见 Cargo.toml）：
//! - KDF：**PBKDF2-HMAC-SHA256**（`ring::pbkdf2`，已在依赖树）。未选 Argon2 是为不引入新 crate；
//!   迭代次数（默认 60 万）随密文一并存储，日后要换 KDF 可按 `kdf` 字段平滑升级而不破坏旧备份。
//! - AEAD：**AES-256-GCM**（`aes-gcm`，与保险库回退后端同源）。每次导出随机 salt + nonce。
//! - 主密码只在可信区内参与派生，用后即 `Zeroizing` 清零；明文凭据负载绝不经 IPC 回前端。
//!
//! 安全提醒（固有取舍）：忘主密码 = 该备份永久无法解开，无任何找回途径。

use std::num::NonZeroU32;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm};
use base64::Engine;
use ring::pbkdf2;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::AppError;

/// PBKDF2-HMAC-SHA256 迭代次数（OWASP 2023 推荐量级）；导出时写入密文，导入按存储值复算。
const ITERATIONS: u32 = 600_000;
/// 迭代次数下限/上限：防旧/恶意文件填 0（解不开）或天文数字（导入即 DoS 卡死）。
const ITER_MIN: u32 = 1_000;
const ITER_MAX: u32 = 5_000_000;
/// 随机盐长度（字节）。
const SALT_LEN: usize = 16;
/// AES-GCM 标准 nonce 长度；密文格式 `nonce(12) || ciphertext || tag`。
const NONCE_LEN: usize = 12;
/// 派生的 AES-256 密钥长度（字节）。
const KEY_LEN: usize = 32;
/// 本实现写出的 KDF 标识（导入据此判定算法）。
const KDF_ALG: &str = "pbkdf2-sha256";

/// 密封后的凭据块——序列化进备份 JSON 的 `sealed` 字段，字段全为 base64/标量，**不含任何明文**。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Sealed {
    /// KDF 算法标识（当前恒为 `pbkdf2-sha256`；预留给未来 Argon2 平滑迁移）。
    pub kdf: String,
    /// PBKDF2 迭代次数（读取时钳到 [`ITER_MIN`]=..[`ITER_MAX`]）。
    pub iter: u32,
    /// base64 随机盐。
    pub salt: String,
    /// base64 `nonce || ciphertext || tag`。
    pub data: String,
}

/// 用主密码加密明文字节 → [`Sealed`]。salt/nonce 每次随机。
pub fn seal(master: &str, plain: &[u8]) -> Result<Sealed, AppError> {
    if master.is_empty() {
        return Err(AppError::Invalid("主密码不能为空".into()));
    }
    let mut salt = [0u8; SALT_LEN];
    aes_gcm::aead::rand_core::RngCore::fill_bytes(&mut OsRng, &mut salt);

    let key = derive_key(master, &salt, ITERATIONS);
    let cipher = Aes256Gcm::new_from_slice(key.as_slice())
        .map_err(|e| AppError::Internal(format!("凭据包加密初始化失败：{e}")))?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plain)
        .map_err(|_| AppError::Internal("凭据包加密失败".into()))?;

    let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
    blob.extend_from_slice(nonce.as_slice());
    blob.extend_from_slice(&ct);
    Ok(Sealed {
        kdf: KDF_ALG.into(),
        iter: ITERATIONS,
        salt: b64().encode(salt),
        data: b64().encode(blob),
    })
}

/// 用主密码解开 [`Sealed`] → 明文字节。密码错误 / 数据损坏一律归为解密失败（不区分，避免预言机）。
pub fn open(sealed: &Sealed, master: &str) -> Result<Vec<u8>, AppError> {
    if sealed.kdf != KDF_ALG {
        return Err(AppError::Invalid(format!(
            "不支持的凭据包算法：{}（请升级 SailTerm）",
            sealed.kdf
        )));
    }
    let salt = b64()
        .decode(sealed.salt.as_bytes())
        .map_err(|_| AppError::Invalid("凭据包数据非法".into()))?;
    let raw = b64()
        .decode(sealed.data.as_bytes())
        .map_err(|_| AppError::Invalid("凭据包数据非法".into()))?;
    if raw.len() <= NONCE_LEN {
        return Err(AppError::Invalid("凭据包数据被截断".into()));
    }
    let iter = sealed.iter.clamp(ITER_MIN, ITER_MAX);
    let key = derive_key(master, &salt, iter);
    let cipher = Aes256Gcm::new_from_slice(key.as_slice())
        .map_err(|_| AppError::Invalid("凭据包数据非法".into()))?;
    let (nonce_bytes, ct) = raw.split_at(NONCE_LEN);
    let nonce = GenericArray::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ct).map_err(|_| AppError::Biz {
        key: "err.backup.wrongMaster",
        zh: "主密码不正确，或备份凭据已损坏",
    })
}

/// PBKDF2-HMAC-SHA256 主密码派生 32 字节密钥；结果用 `Zeroizing` 包裹，离开作用域即清零。
fn derive_key(master: &str, salt: &[u8], iter: u32) -> Zeroizing<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    let rounds = NonZeroU32::new(iter).expect("iter clamped to >= ITER_MIN");
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        rounds,
        salt,
        master.as_bytes(),
        &mut key,
    );
    Zeroizing::new(key)
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let plain = b"{\"hello\":\"world\",\"pw\":\"s3cr3t\"}";
        let s = seal("correct horse", plain).unwrap();
        // 密文里绝不应出现明文片段。
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("s3cr3t"));
        assert!(!json.contains("hello"));
        let out = open(&s, "correct horse").unwrap();
        assert_eq!(out, plain);
    }

    #[test]
    fn wrong_password_fails() {
        let s = seal("right", b"payload").unwrap();
        assert!(open(&s, "wrong").is_err());
    }

    #[test]
    fn empty_master_rejected() {
        assert!(seal("", b"x").is_err());
    }

    #[test]
    fn nonce_is_random_each_time() {
        let a = seal("m", b"x").unwrap();
        let b = seal("m", b"x").unwrap();
        assert_ne!(a.salt, b.salt);
        assert_ne!(a.data, b.data);
    }

    #[test]
    fn tampered_data_fails() {
        let mut s = seal("m", b"payload").unwrap();
        // 翻一个字符制造损坏。
        let mut raw = b64().decode(s.data.as_bytes()).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        s.data = b64().encode(raw);
        assert!(open(&s, "m").is_err());
    }

    #[test]
    fn unknown_kdf_rejected() {
        let s = Sealed {
            kdf: "argon2id".into(),
            iter: 3,
            salt: b64().encode([0u8; SALT_LEN]),
            data: b64().encode([0u8; NONCE_LEN + 1]),
        };
        assert!(open(&s, "m").is_err());
    }
}
