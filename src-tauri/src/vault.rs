//! 凭证保险库。
//!
//! 双层策略：**首选** OS 原生安全存储（Windows Credential Manager / macOS Keychain /
//! Linux Secret Service，经 `keyring`）；**回退** AES-256-GCM 加密文件（OS 存储不可用时）。
//!
//! 安全红线：明文凭证只在本模块与 SSH 握手路径出现，一律以 [`Secret`]
//! 包裹（`Debug` 恒为 `***`、`Drop` 时清零）；**本模块不实现 `Serialize`**，
//! 从类型层面杜绝凭证被序列化回前端。

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::error::AppError;

/// keyring service 名（与 tauri.conf.json 的 app identifier 一致，避免同机其他应用串号）。
const SERVICE: &str = "com.sailterm.desktop";
/// 合并模式（macOS/Linux）的单条目账号名：整张凭证表存进这一个 generic-password，
/// 使钥匙串鉴权只需一次即覆盖所有连接（避免「有几个连接就弹几次」）。
const VAULT_ACCOUNT: &str = "sail:vault:__all__";
/// AES-GCM 标准 nonce 长度；密文格式为 `nonce(12) || ciphertext || tag`。
const NONCE_LEN: usize = 12;
/// 主密钥长度（AES-256）。
const KEY_LEN: usize = 32;

/// 明文凭证包装：不可序列化、`Debug` 不泄漏内容、离开作用域即清零。
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(Zeroizing::new(s.into()))
    }

    /// 仅供 SSH 握手路径读取；禁止写入日志/错误信息/事件。
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// 前端表单直送（IPC 入参）；反序列化后即进入可信区。
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Self::new)
    }
}

/// 保险库：`key → 密文`。实例可跨线程共享（回退文件与 OS 合并单条目的读-改-写各有串行锁）。
pub struct Vault {
    /// OS 存储可用则走 keyring；否则用加密文件。
    use_os: bool,
    /// OS 模式下是否合并为单个钥匙串条目（macOS + Linux：避免逐条授权弹窗）。Windows 恒 false：
    /// 凭据管理器单条 generic 凭据 ≤2560B，装不下全部凭证+私钥 PEM，且本就不逐条弹窗。
    consolidated: bool,
    /// 合并模式（macOS/Linux）单条目读-改-写的串行锁，与 `Fallback::io` 同理：
    /// 整张凭证表存一个条目，`store/get(懒迁移)/delete` 都是 os_read_map → 改 → os_write_map
    /// （整表覆盖写），并发交错时后写者会把先写者的更新整表盖掉；懒迁移还会在读写窗口内
    /// 删除旧「逐条」条目，交错可致凭证**不可恢复地丢失**（旧条目已删、新表里没有）。
    /// 临界区内的 keyring 调用全为同步阻塞、不跨 `.await`，持 `std::sync::Mutex` 安全。
    os_io: Mutex<()>,
    fallback: Fallback,
}

/// AES-GCM 加密文件回退：`vault.enc` 存密文表，`vault.key` 存主密钥。
///
/// 主密钥当前由本机用户目录 ACL 保护；按平台接入 DPAPI / Keychain 派生
/// （「凭证后端分平台」）。
struct Fallback {
    store: PathBuf,
    key_file: PathBuf,
    cipher: Mutex<Option<Aes256Gcm>>,
    /// 读-改-写的串行锁（与 `cipher` 分开：它只护密钥缓存，本锁要覆盖整个 load→改→save）。
    io: Mutex<()>,
}

impl Vault {
    pub fn open(dir: &Path) -> Self {
        // dev-file-vault feature：强制走本机 AES-256-GCM 加密文件后端，绕开 macOS 自签名
        // （无 Team ID）dev 身份下钥匙串 partition_id 按 cdhash 分区、每次重编译反复弹授权的问题
        // （现代 macOS 以 partition_id 而非传统 ACL 放行钥匙串读取，自签名只能落到 cdhash 分区）。
        // 仅 scripts/dev-macos.sh 用 `--features dev-file-vault` 编入；发布构建不启用，仍走 OS 钥匙串。
        let use_os = if cfg!(feature = "dev-file-vault") {
            false
        } else {
            keyring::Entry::store_status().is_ok()
        };
        // 合并模式（macOS + Linux）：把全部凭证收进单个钥匙串条目。keyring 的 OS 分支本是「每条凭证
        // 一个 generic-password」，而 macOS 钥匙串 / Linux Secret Service 读钥匙串按条目鉴权 → 有几个
        // 连接就弹几次授权；合并成单条目后鉴权一次即覆盖全部。
        // Windows 不合并：① 凭据管理器单条 generic 凭据上限 ~2560B，多密码 + 私钥 PEM 合并必溢出写不进；
        // ② Windows 读同用户自己存的凭据本就静默、无逐条弹窗，合并只亏无益。故按「非 Windows」门控。
        let consolidated = use_os && !cfg!(windows);
        Self {
            use_os,
            consolidated,
            os_io: Mutex::new(()),
            fallback: Fallback::at(dir),
        }
    }

    /// 测试专用：强制 AES-GCM 文件后端（不碰真机钥匙串），供跨模块单测构造保险库。
    #[cfg(test)]
    pub(crate) fn file_backend_for_test(dir: &Path) -> Self {
        Self {
            use_os: false,
            consolidated: false,
            os_io: Mutex::new(()),
            fallback: Fallback::at(dir),
        }
    }

    /// 当前生效的后端名（供设置页/安全状态灯展示）。
    pub fn backend(&self) -> &'static str {
        if self.use_os {
            "os-keyring"
        } else {
            "aes-gcm-file"
        }
    }

    /// 写入凭证（覆盖同 key）。
    pub fn store(&self, key: &str, secret: &Secret) -> Result<(), AppError> {
        if self.use_os {
            if self.consolidated {
                // 整段读-改-写独占（见 `os_io`）：并发 store 若不加锁，后写者会拿旧快照把
                // 先写者的 key 整表盖掉；调用点分布在 spawn_blocking 阻塞池，是真并行。
                let _guard = self.os_io.lock().unwrap();
                let mut map = self.os_read_map()?;
                map.insert(key.to_string(), secret.expose().to_string());
                self.os_write_map(&map)?;
                // 顺带清理该 key 可能残留的旧「逐条」条目（迁移后不再使用）。
                let _ = entry(key)?.delete_credential();
                return Ok(());
            }
            return entry(key)?
                .set_secret(secret.expose().as_bytes())
                .map_err(vault_err);
        }
        let blob = self.fallback.seal(secret.expose().as_bytes())?;
        self.fallback.update(|m| {
            m.insert(key.to_string(), blob);
            Ok(())
        })
    }

    /// 读取凭证；不存在返回 `None`。
    pub fn get(&self, key: &str) -> Result<Option<Secret>, AppError> {
        if self.use_os {
            if self.consolidated {
                // 懒迁移（读旧逐条 → 并入单条目 → 删旧条目）必须整段独占：若 A/B 并发迁移
                // 交错（A 写表含 X → A 删旧 X → B 用不含 X 的旧快照盖写），X 两头都不在，
                // 凭证不可恢复地丢失。纯读路径也统一在此锁内，保证与写者看到一致状态。
                let _guard = self.os_io.lock().unwrap();
                let mut map = self.os_read_map()?;
                if let Some(v) = map.get(key) {
                    return Ok(Some(Secret::new(v.clone())));
                }
                // 懒迁移：首次读到旧的「逐条」条目，并入单条目后删除老条目（免用户重录）。
                return match entry(key)?.get_secret() {
                    Ok(bytes) => {
                        let v = String::from_utf8_lossy(&bytes).into_owned();
                        map.insert(key.to_string(), v.clone());
                        self.os_write_map(&map)?;
                        let _ = entry(key)?.delete_credential();
                        Ok(Some(Secret::new(v)))
                    }
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(e) => Err(vault_err(e)),
                };
            } else {
                return match entry(key)?.get_secret() {
                    Ok(bytes) => Ok(Some(Secret::new(
                        String::from_utf8_lossy(&bytes).into_owned(),
                    ))),
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(e) => Err(vault_err(e)),
                };
            }
        }
        // 只读：不借道 `update`（那会为「读一条」把整张表重新加密写盘）。
        let blob = self.fallback.load()?.get(key).cloned();
        blob.map(|b| self.fallback.open(b.as_bytes()).map(Secret::new))
            .transpose()
    }

    /// 是否已存有该 key（用于前端展示「已保存凭证」而不回显内容）。
    pub fn contains(&self, key: &str) -> Result<bool, AppError> {
        if self.use_os {
            if self.consolidated {
                if self.os_read_map()?.contains_key(key) {
                    return Ok(true);
                }
                // 尚未迁移的旧条目也算存在（get 时会并入单条目）。
                return Ok(entry(key)?.get_secret().is_ok());
            }
            return Ok(entry(key)?.get_secret().is_ok());
        }
        // 同上：「问一句在不在」不该写盘。本函数在启动时被每个连接档案调一次（侧栏
        // 显「已保存凭证」小标），旧写法下就是 N 次并发整表重写。
        Ok(self.fallback.load()?.contains_key(key))
    }

    /// 批量删除；缺失的 key 视为已删除（幂等）。
    pub fn delete(&self, keys: &[String]) -> Result<(), AppError> {
        if self.use_os && self.consolidated {
            // 同 store：读-改-写整表覆盖，必须串行（见 `os_io`）。
            let _guard = self.os_io.lock().unwrap();
            let mut map = self.os_read_map()?;
            let mut changed = false;
            for key in keys {
                if map.remove(key).is_some() {
                    changed = true;
                }
            }
            if changed {
                self.os_write_map(&map)?;
            }
            // 一并删除可能存在的旧「逐条」条目（幂等）。
            for key in keys {
                match entry(key)?.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => {}
                    Err(e) => return Err(vault_err(e)),
                }
            }
            return Ok(());
        }
        for key in keys {
            if self.use_os {
                match entry(key)?.delete_credential() {
                    Ok(()) | Err(keyring::Error::NoEntry) => {}
                    Err(e) => return Err(vault_err(e)),
                }
            }
        }
        if !self.use_os {
            self.fallback.update(|m| {
                for key in keys {
                    m.remove(key);
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// 读单条目钥匙串里的 `{key: 明文凭证}` 表（macOS 合并模式）。缺条目视作空表。
    /// 单独调用可不加锁；参与读-改-写的调用方必须整体持 `os_io`。
    fn os_read_map(&self) -> Result<BTreeMap<String, String>, AppError> {
        match blob_entry()?.get_secret() {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| AppError::Vault(format!("凭据库损坏：{e}"))),
            Err(keyring::Error::NoEntry) => Ok(BTreeMap::new()),
            Err(e) => Err(vault_err(e)),
        }
    }

    /// 写回单条目（整表覆盖）。用 Zeroizing 收敛明文 JSON 的生命周期。
    fn os_write_map(&self, map: &BTreeMap<String, String>) -> Result<(), AppError> {
        let json =
            Zeroizing::new(serde_json::to_vec(map).map_err(|e| AppError::Vault(e.to_string()))?);
        blob_entry()?.set_secret(json.as_slice()).map_err(vault_err)
    }
}

fn entry(key: &str) -> Result<keyring::Entry, AppError> {
    keyring::Entry::new(SERVICE, key).map_err(vault_err)
}

/// 合并模式（macOS/Linux）的单条目（整张凭证表存于此）。与逐条模式的 `entry` 分开，互不干扰。
fn blob_entry() -> Result<keyring::Entry, AppError> {
    keyring::Entry::new(SERVICE, VAULT_ACCOUNT).map_err(vault_err)
}

fn vault_err(e: keyring::Error) -> AppError {
    AppError::Vault(format!("OS 凭据存储不可用：{e}"))
}

impl Fallback {
    /// 指向 `dir` 下 `vault.enc` / `vault.key` 的回退后端实例（字段集中在这一处，以
    /// 免 `Vault::open` 与测试因新增字段而各自漏初始化）。
    fn at(dir: &Path) -> Self {
        Self {
            store: dir.join("vault.enc"),
            key_file: dir.join("vault.key"),
            cipher: Mutex::new(None),
            io: Mutex::new(()),
        }
    }

    /// 读-改-写加密表（原子替换，避免中途崩溃损坏）。
    fn update<T>(
        &self,
        f: impl FnOnce(&mut BTreeMap<String, String>) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        // 整段读-改-写必须独占：否则两个并发写会互相把对方的临时文件 rename 走，输的一方
        // 拿到 ENOENT——真机上表现为启动时一条没有主语的「文件读写失败：No such file or
        // directory (os error 2)」；更隐蔽的是丢更新（后写的把先写的整表盖掉）。
        let _guard = self.io.lock().unwrap();
        let mut map = self.load()?;
        let out = f(&mut map)?;
        self.save(&map)?;
        Ok(out)
    }

    fn load(&self) -> Result<BTreeMap<String, String>, AppError> {
        let bytes = match fs::read(&self.store) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => {
                return Err(AppError::Vault(format!(
                    "读取凭据文件 {} 失败：{e}",
                    self.store.display()
                )))
            }
        };
        let json = self.open(&bytes)?;
        serde_json::from_str(&json).map_err(|e| AppError::Vault(format!("保险库文件损坏：{e}")))
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<(), AppError> {
        let json = serde_json::to_string(map).map_err(|e| AppError::Vault(e.to_string()))?;
        let blob = self.seal(json.as_bytes())?;
        let tmp = self.tmp_path();
        fs::write(&tmp, &blob).map_err(|e| {
            AppError::Vault(format!("写入凭据临时文件 {} 失败：{e}", tmp.display()))
        })?;
        fs::rename(&tmp, &self.store).map_err(|e| {
            AppError::Vault(format!("更新凭据文件 {} 失败：{e}", self.store.display()))
        })?;
        Ok(())
    }

    /// 原子替换用的临时名：带 pid 与自增序号。即使本进程的串行锁失效（或多进程共用
    /// 一个数据目录），也不会出现两个写者争同一个临时文件。
    fn tmp_path(&self) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        self.store
            .with_extension(format!("tmp-{}-{}", std::process::id(), n))
    }

    fn seal(&self, plain: &[u8]) -> Result<String, AppError> {
        let cipher = self.cipher()?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ct = cipher
            .encrypt(&nonce, plain)
            .map_err(|_| AppError::Vault("凭据加密失败".into()))?;
        Ok(b64().encode(nonce.iter().chain(ct.iter()).copied().collect::<Vec<u8>>()))
    }

    fn open(&self, encoded: &[u8]) -> Result<String, AppError> {
        let raw = b64()
            .decode(encoded)
            .map_err(|_| AppError::Vault("保险库数据非法".into()))?;
        if raw.len() <= NONCE_LEN {
            return Err(AppError::Vault("保险库数据被截断".into()));
        }
        let (nonce, ct) = raw.split_at(NONCE_LEN);
        let plain = self
            .cipher()?
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| AppError::Vault("凭据解密失败（主密钥或文件已变更）".into()))?;
        String::from_utf8(plain).map_err(|e| AppError::Vault(e.to_string()))
    }

    /// 取（或首次生成）主密钥；密钥文件仅在回退后端启用时创建。
    fn cipher(&self) -> Result<Aes256Gcm, AppError> {
        let mut slot = self.cipher.lock().unwrap();
        if let Some(c) = slot.as_ref() {
            return Ok(c.clone());
        }
        let key = match fs::read_to_string(&self.key_file) {
            Ok(text) => b64()
                .decode(text.trim().as_bytes())
                .map_err(|_| AppError::Vault("主密钥文件损坏".into()))?,
            Err(_) => {
                let key = Aes256Gcm::generate_key(&mut OsRng);
                let text = b64().encode(key.as_slice());
                if let Some(parent) = self.key_file.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        AppError::Vault(format!("创建凭据目录 {} 失败：{e}", parent.display()))
                    })?;
                }
                fs::write(&self.key_file, text).map_err(|e| {
                    AppError::Vault(format!(
                        "写入主密钥文件 {} 失败：{e}",
                        self.key_file.display()
                    ))
                })?;
                restrict(&self.key_file);
                key.to_vec()
            }
        };
        if key.len() != KEY_LEN {
            return Err(AppError::Vault("主密钥长度非法".into()));
        }
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| AppError::Vault(e.to_string()))?;
        *slot = Some(cipher.clone());
        Ok(cipher)
    }
}

/// 主密钥文件收紧为仅当前用户可读（Unix）；Windows 依赖用户目录 ACL。
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// 保险库条目 key。
pub mod keys {
    pub fn password(profile_id: &str) -> String {
        format!("sail:conn:{profile_id}:password")
    }

    pub fn key_passphrase(profile_id: &str) -> String {
        format!("sail:conn:{profile_id}:key_passphrase")
    }

    pub fn private_key(profile_id: &str) -> String {
        format!("sail:conn:{profile_id}:private_key")
    }

    /// 删除连接时需一并清理的全部条目。
    pub fn of_profile(profile_id: &str) -> Vec<String> {
        vec![
            password(profile_id),
            key_passphrase(profile_id),
            private_key(profile_id),
        ]
    }

    /// AI Provider 的 API Key（BYOK）；只在 Rust 侧 HTTP header 使用，
    /// 永不进对话上下文/日志/事件。
    pub fn ai_provider(provider_id: &str) -> String {
        format!("sail:ai:{provider_id}:key")
    }

    /// 删除 AI Provider 时需一并清理的全部条目。
    pub fn of_ai_provider(provider_id: &str) -> Vec<String> {
        vec![ai_provider(provider_id)]
    }

    /// SSH 密钥管理：导入的私钥材料（OpenSSH / PEM 全文，只在保险库）。
    pub fn ssh_key(key_id: &str) -> String {
        format!("sail:ssh_key:{key_id}:private_key")
    }

    /// SSH 密钥的口令（私钥带口令加密时一并存，连接时免再输）。
    pub fn ssh_key_passphrase(key_id: &str) -> String {
        format!("sail:ssh_key:{key_id}:passphrase")
    }

    /// 删除一把密钥时需一并清理的全部条目。
    pub fn of_ssh_key(key_id: &str) -> Vec<String> {
        vec![ssh_key(key_id), ssh_key_passphrase(key_id)]
    }

    /// 锁屏 PIN 的 PBKDF2 哈希串（见 crate::lock）：本机解锁凭证归保险库，
    /// 不落 settings 表（那张表的红线是「只存非敏感偏好」）。存的是派生物而非可重放凭证。
    pub fn lock_pin() -> String {
        "sail:lock:pin".into()
    }

    /// 锁屏密保**问题**原文（找回时展示用）。问题非秘密，但为与其他凭证同处密钥串保护域、
    /// 免另设存储通道，仍归 vault；真正机密的是下面的答案哈希。
    pub fn lock_secq_question() -> String {
        "sail:lock:secq:question".into()
    }

    /// 锁屏密保**答案**的 PBKDF2 哈希串（忘记 PIN 的找回锚点，见 crate::lock::hash_answer）。
    pub fn lock_secq_answer() -> String {
        "sail:lock:secq:answer".into()
    }

    /// 关闭/重置锁屏时需一并清理的全部条目（PIN + 密保问答）。
    pub fn of_lock() -> Vec<String> {
        vec![lock_pin(), lock_secq_question(), lock_secq_answer()]
    }

    /// 录制文件的数据密钥（AES-256，base64 存）：把录制加密的保护委托给 Vault 的
    /// 钥匙串/加密文件双后端，密钥本身绝不写进录制文件（程序内回放才能解密）。
    pub fn record_data_key() -> String {
        "sail:record:data_key".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回退后端往返：写入 → 读取一致 → 落盘内容不含明文 → 删除后不可读。
    #[test]
    fn fallback_roundtrip_and_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault {
            use_os: false,
            consolidated: false,
            os_io: Mutex::new(()),
            fallback: Fallback::at(dir.path()),
        };
        let key = keys::password("p1");
        assert!(!vault.contains(&key).unwrap());

        vault
            .store(&key, &Secret::new("s3cr3t-passphrase"))
            .unwrap();
        assert!(vault.contains(&key).unwrap());
        assert_eq!(
            vault.get(&key).unwrap().unwrap().expose(),
            "s3cr3t-passphrase"
        );

        //加密文件与主密钥文件里都搜不到明文
        let plain = b"s3cr3t";
        assert!(!fs::read(dir.path().join("vault.enc"))
            .unwrap()
            .windows(plain.len())
            .any(|w| w == plain));
        assert!(!fs::read_to_string(dir.path().join("vault.key"))
            .unwrap()
            .contains("s3cr3t"));

        vault.delete(std::slice::from_ref(&key)).unwrap();
        assert!(vault.get(&key).unwrap().is_none());
    }

    /// `Secret` 的 `Debug` 不得泄漏内容。
    #[test]
    fn secret_debug_is_redacted() {
        assert_eq!(format!("{:?}", Secret::new("hunter2")), "Secret(***)");
    }
}
