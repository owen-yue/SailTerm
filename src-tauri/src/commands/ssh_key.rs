//! SSH 密钥管理 IPC：导入既有私钥、列密钥（只回非敏感
//! 元数据 + 公钥）、导出私钥到文件、删除。私钥解析复用 `russh::keys`（ssh-key），**未新增依赖**、
//! 全树 ring-only 不变。
//!
//! **安全红线**：私钥材料只进保险库（[`crate::vault::Vault`] 加密存储），
//! **绝不经 IPC 回前端**——[`ssh_key_list`] 只回元数据 + 公钥；导出私钥走后端 `std::fs` 写用户
//! 指定文件（复用连接导入导出的本地文件模式），不返回内容。db 表无任何私钥列（见 db/ssh_key.rs）。
//!
//! **生成**支持 Ed25519 / ECDSA(P-256/384/521) / RSA(2048/3072/4096)；格式转换仍顺延。

use russh::keys::{HashAlg, PrivateKey};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use zeroize::Zeroizing;

use super::{blocking, state};
use crate::db::ssh_key::SshKey;
use crate::error::AppError;
use crate::state::AppState;
use crate::vault::{keys, Secret};

/// 名称字符上限（超长截断）。
const MAX_NAME_CHARS: usize = 80;

/// 密钥导入入参：私钥来自粘贴内容或磁盘文件（二选一，内容优先）；口令可选（带口令私钥用）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInput {
    pub name: String,
    /// 用户粘贴的私钥内容（OpenSSH / PEM）。
    pub private_key_pem: Option<Secret>,
    /// 或指向磁盘私钥文件路径。
    pub private_key_path: Option<String>,
    /// 私钥口令（若私钥带口令加密，一并存保险库，连接时免再输）。
    pub passphrase: Option<Secret>,
}

/// 从私钥文本解析出的非敏感元数据（纯数据，便于离线单测）。
#[derive(Debug)]
struct KeyMeta {
    algorithm: String,
    fingerprint: String,
    public_key: String,
    comment: String,
    encrypted: bool,
}

/// 生成算法标识：表单下拉值，后端映射到 `ssh_key::Algorithm` / RSA 位数。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GenAlg {
    Ed25519,
    EcdsaP256,
    EcdsaP384,
    EcdsaP521,
    Rsa2048,
    Rsa3072,
    Rsa4096,
}

impl GenAlg {
    fn parse(s: &str) -> Result<Self, AppError> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "" | "ed25519" => Self::Ed25519,
            "ecdsa" | "ecdsa-p256" | "nistp256" => Self::EcdsaP256,
            "ecdsa-p384" | "nistp384" => Self::EcdsaP384,
            "ecdsa-p521" | "nistp521" => Self::EcdsaP521,
            "rsa" | "rsa-2048" => Self::Rsa2048,
            "rsa-3072" => Self::Rsa3072,
            "rsa-4096" => Self::Rsa4096,
            other => {
                return Err(AppError::Invalid(format!(
                    "不支持的密钥算法：{other}（可选 ed25519 / ecdsa / rsa）"
                )))
            }
        })
    }
}

/// **密钥生成**：按算法本地生成，私钥入保险库，元数据入库。
/// 生成的私钥**绝不回显**；公钥可复制到服务器 `authorized_keys`。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyGenInput {
    pub name: String,
    pub comment: Option<String>,
    /// `ed25519`（默认）/ `ecdsa` / `ecdsa-p384` / `ecdsa-p521` / `rsa` / `rsa-3072` / `rsa-4096`。
    pub algorithm: Option<String>,
}

#[tauri::command]
pub async fn ssh_key_generate(app: AppHandle, input: KeyGenInput) -> Result<SshKey, AppError> {
    blocking(move || generate_key(&state(&app), input)).await
}

/// 按算法生成一个未加密密钥对（Ed25519 / ECDSA(P-256/384/521) / RSA(2048/3072/4096)）。
/// 供密钥库生成（[`generate_key`]）与工具生成器（[`keygen_generate`]）共用。
fn build_keypair(which: GenAlg) -> Result<PrivateKey, AppError> {
    use ssh_key::getrandom::SysRng;
    use ssh_key::private::RsaKeypair;
    use ssh_key::rand_core::UnwrapErr;
    use ssh_key::{Algorithm, EcdsaCurve, PrivateKey};

    let mut rng = UnwrapErr(SysRng);
    let key = match which {
        GenAlg::Ed25519 => PrivateKey::random(&mut rng, Algorithm::Ed25519),
        GenAlg::EcdsaP256 => PrivateKey::random(
            &mut rng,
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
        ),
        GenAlg::EcdsaP384 => PrivateKey::random(
            &mut rng,
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP384,
            },
        ),
        GenAlg::EcdsaP521 => PrivateKey::random(
            &mut rng,
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP521,
            },
        ),
        // RSA 位数走 RsaKeypair（PrivateKey::random 固定 4096）
        GenAlg::Rsa2048 | GenAlg::Rsa3072 | GenAlg::Rsa4096 => {
            let bits = match which {
                GenAlg::Rsa2048 => 2048,
                GenAlg::Rsa3072 => 3072,
                _ => 4096,
            };
            RsaKeypair::random(&mut rng, bits).map(PrivateKey::from)
        }
    }
    .map_err(|e| AppError::Key(format!("生成密钥失败：{e}")))?;
    Ok(key)
}

fn generate_key(st: &AppState, input: KeyGenInput) -> Result<SshKey, AppError> {
    use ssh_key::LineEnding;

    let name = clip_chars(input.name.trim(), MAX_NAME_CHARS);
    if name.is_empty() {
        return Err(AppError::Invalid("请填写密钥名称".into()));
    }
    let which = GenAlg::parse(input.algorithm.as_deref().unwrap_or("ed25519"))?;
    let key = build_keypair(which)?;
    let pem = Zeroizing::new(
        key.to_openssh(LineEnding::LF)
            .map_err(|e| AppError::Key(format!("导出私钥失败：{e}")))?
            .to_string(),
    );
    let meta = parse_key_meta(pem.as_bytes())?;
    if let Some(existing) = st.db.find_ssh_key_by_fingerprint(&meta.fingerprint)? {
        return Err(AppError::Invalid(format!(
            "该密钥已存在（名为「{}」）",
            existing.name
        )));
    }
    let comment = input
        .comment
        .map(|c| clip_chars(c.trim(), 120))
        .unwrap_or_default();
    let k = SshKey::new(
        name,
        meta.algorithm,
        meta.fingerprint,
        meta.public_key,
        if comment.is_empty() {
            meta.comment
        } else {
            comment
        },
        meta.encrypted,
    );
    st.vault.store(&k.key_ref, &Secret::new(pem.as_str()))?;
    st.db.upsert_ssh_key(&k)?;
    Ok(k)
}

// =====================================================================
// 密钥生成器（工具 keygen）：生成 → 展示公钥 → 保存公/私钥到文件。**不入库、不入保险库**，
// 与上面的密钥库管理完全解耦。私钥只在后端进程内存（AppState.keygen）短暂停留供写盘，绝不出 IPC。
// =====================================================================

/// 生成器入参：算法（同密钥库下拉值）+ 可选私钥口令（非空则私钥以 OpenSSH 加密）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeygenInput {
    /// `ed25519`（默认）/ `ecdsa` / `ecdsa-p384` / `ecdsa-p521` / `rsa` / `rsa-3072` / `rsa-4096`。
    pub algorithm: Option<String>,
    /// 私钥口令（可选）；留空则生成无口令私钥。
    pub passphrase: Option<Secret>,
}

/// 生成器预览结果（公钥非敏感可回前端；私钥只留后端缓冲，凭 `token` 保存）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeygenPreview {
    /// 取回缓存私钥的凭据（用指纹作 key，唯一且非敏感）。
    pub token: String,
    pub algorithm: String,
    pub fingerprint: String,
    pub public_key: String,
    /// 私钥是否已用口令加密（前端据此标注）。
    pub encrypted: bool,
}

#[tauri::command]
pub async fn ssh_keygen_generate(
    app: AppHandle,
    input: KeygenInput,
) -> Result<KeygenPreview, AppError> {
    blocking(move || keygen_generate(&state(&app), input)).await
}

fn keygen_generate(st: &AppState, input: KeygenInput) -> Result<KeygenPreview, AppError> {
    use ssh_key::getrandom::SysRng;
    use ssh_key::LineEnding;

    let which = GenAlg::parse(input.algorithm.as_deref().unwrap_or("ed25519"))?;
    let key = build_keypair(which)?;
    // 口令非空 → 用 bcrypt-pbkdf + AES256-CBC 加密私钥（OpenSSH 格式，与 ssh-keygen -p 兼容）。
    let pass = input
        .passphrase
        .filter(|s| !s.is_empty())
        .map(|s| s.expose().to_string());
    let key = match &pass {
        Some(p) => {
            let mut rng = SysRng;
            key.encrypt(&mut rng, p.as_bytes())
                .map_err(|e| AppError::Key(format!("加密私钥失败：{e}")))?
        }
        None => key,
    };
    let pem = Zeroizing::new(
        key.to_openssh(LineEnding::LF)
            .map_err(|e| AppError::Key(format!("导出私钥失败：{e}")))?
            .to_string(),
    );
    let meta = parse_key_meta(pem.as_bytes())?;
    let token = meta.fingerprint.clone();
    st.keygen
        .lock()
        .map_err(|_| AppError::Internal("密钥缓冲锁定".into()))?
        .insert(token.clone(), (pem.clone(), meta.public_key.clone()));
    Ok(KeygenPreview {
        token,
        algorithm: meta.algorithm,
        fingerprint: meta.fingerprint,
        public_key: meta.public_key,
        encrypted: meta.encrypted,
    })
}

/// 保存生成器缓存的私钥到用户指定文件（后端 `std::fs` 写，**不经 IPC 返回内容**）；Unix 收紧 0600。
#[tauri::command]
pub async fn ssh_keygen_save_private(
    app: AppHandle,
    token: String,
    path: String,
) -> Result<bool, AppError> {
    blocking(move || keygen_save(&state(&app), &token, &path, true)).await
}

/// 保存生成器缓存的公钥（OpenSSH 单行）到用户指定文件。
#[tauri::command]
pub async fn ssh_keygen_save_public(
    app: AppHandle,
    token: String,
    path: String,
) -> Result<bool, AppError> {
    blocking(move || keygen_save(&state(&app), &token, &path, false)).await
}

/// 写盘共享逻辑：`private=true` 取缓冲元组.0（私钥，收紧权限），否则取 .1（公钥 + 换行）。
fn keygen_save(st: &AppState, token: &str, path: &str, private: bool) -> Result<bool, AppError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(AppError::Invalid("保存路径不能为空".into()));
    }
    let map = st
        .keygen
        .lock()
        .map_err(|_| AppError::Internal("密钥缓冲锁定".into()))?;
    let (pem, pubk) = map
        .get(token)
        .ok_or_else(|| AppError::Invalid("密钥已失效，请重新生成".into()))?;
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if private {
        std::fs::write(p, pem.as_bytes())?;
        restrict_private_file(p);
    } else {
        std::fs::write(p, format!("{pubk}\n").as_bytes())?;
    }
    Ok(true)
}

/// 列全部密钥（只回非敏感元数据 + 公钥；私钥材料不出保险库）。
#[tauri::command]
pub async fn ssh_key_list(app: AppHandle) -> Result<Vec<SshKey>, AppError> {
    blocking(move || state(&app).db.list_ssh_keys()).await
}

/// 导入一把既有私钥：解析取元数据（算法 / 指纹 / 公钥 / 注释）、私钥材料入保险库、元数据入库。
/// 指纹重复（同一把密钥）即拒绝，避免重复导入。
#[tauri::command]
pub async fn ssh_key_import(app: AppHandle, input: KeyInput) -> Result<SshKey, AppError> {
    blocking(move || import_key(&state(&app), input)).await
}

/// 导出私钥到用户指定文件（后端 `std::fs` 写，**不经 IPC 返回内容**）；Unix 下收紧 0600。
#[tauri::command]
pub async fn ssh_key_export_file(
    app: AppHandle,
    id: String,
    path: String,
) -> Result<bool, AppError> {
    blocking(move || export_private_file(&state(&app), &id, &path)).await
}

/// 删除一把密钥：先清保险库条目（私钥 + 口令），再删 db 行。
#[tauri::command]
pub async fn ssh_key_delete(app: AppHandle, id: String) -> Result<bool, AppError> {
    blocking(move || {
        let st = state(&app);
        st.vault.delete(&keys::of_ssh_key(&id))?;
        st.db.delete_ssh_key(&id)
    })
    .await
}

/// 解析私钥文本 → 非敏感元数据（纯函数，不触保险库 / db，便于单测）。
/// OpenSSH 格式优先，退回通用 PEM（PKCS#1/#8）；解析失败归 `E_KEY`。
fn parse_key_meta(pem: &[u8]) -> Result<KeyMeta, AppError> {
    let parsed = PrivateKey::from_openssh(pem)
        .or_else(|_| PrivateKey::from_bytes(pem))
        .map_err(|e| AppError::Key(format!("私钥解析失败：{e}")))?;
    Ok(KeyMeta {
        algorithm: parsed.algorithm().as_str().to_string(),
        fingerprint: parsed.fingerprint(HashAlg::Sha256).to_string(),
        // 加密私钥的公钥部分在 OpenSSH 格式里是明文，无需口令即可导出（to_openssh 的错误经 From 归 E_KEY）
        public_key: parsed.public_key().to_openssh()?,
        comment: parsed.comment().to_string(),
        encrypted: parsed.is_encrypted(),
    })
}

pub(crate) fn import_key(st: &AppState, mut input: KeyInput) -> Result<SshKey, AppError> {
    let name = clip_chars(input.name.trim(), MAX_NAME_CHARS);
    if name.is_empty() {
        return Err(AppError::Invalid("密钥名称不能为空".into()));
    }

    // 私钥文本：粘贴内容优先，否则读文件（复用连接导入导出的 std::fs 模式）；用后即清零。
    let pem: Zeroizing<String> = match input.private_key_pem.take().filter(|s| !s.is_empty()) {
        Some(s) => Zeroizing::new(s.expose().to_string()),
        None => {
            let path = input
                .private_key_path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AppError::Invalid("请提供私钥内容或私钥文件路径".into()))?;
            std::fs::read_to_string(path)
                .map(Zeroizing::new)
                .map_err(|e| AppError::Io(format!("读取私钥文件失败：{e}")))?
        }
    };

    let meta = parse_key_meta(pem.as_bytes())?;

    // 指纹查重：同一把密钥不重复导入。
    if let Some(existing) = st.db.find_ssh_key_by_fingerprint(&meta.fingerprint)? {
        return Err(AppError::Invalid(format!(
            "该密钥已导入（名为「{}」，指纹 {}）",
            existing.name, meta.fingerprint
        )));
    }

    // 私钥材料入保险库（只此一处存私钥，db 只存 key_ref 键名，IPC 只回元数据）。
    let k = SshKey::new(
        name,
        meta.algorithm,
        meta.fingerprint,
        meta.public_key,
        meta.comment,
        meta.encrypted,
    );
    st.vault.store(&k.key_ref, &Secret::new(pem.as_str()))?;
    if let Some(pass) = input.passphrase.take().filter(|s| !s.is_empty()) {
        st.vault.store(&keys::ssh_key_passphrase(&k.id), &pass)?;
    }
    st.db.upsert_ssh_key(&k)?;
    Ok(k)
}

/// 从保险库取私钥写到用户指定文件（不经 IPC 返回内容）；Unix 收紧 0600。
fn export_private_file(st: &AppState, id: &str, path: &str) -> Result<bool, AppError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(AppError::Invalid("导出路径不能为空".into()));
    }
    let k = st
        .db
        .get_ssh_key(id)?
        .ok_or_else(|| AppError::Invalid(format!("密钥不存在：{id}")))?;
    let secret = st
        .vault
        .get(&k.key_ref)?
        .ok_or_else(|| AppError::Key("保险库中未找到该私钥".into()))?;
    let p = std::path::Path::new(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, secret.expose().as_bytes())?;
    restrict_private_file(p);
    Ok(true)
}

/// 私钥导出文件收紧为仅当前用户可读（Unix 0600）；Windows 依赖用户目录 ACL。
#[cfg(unix)]
fn restrict_private_file(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_private_file(_path: &std::path::Path) {}

/// 按字符数截断（多字节安全），超长加省略号。
fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用项目自身的生成路径（[`build_keypair`]）运行时生成一把 ed25519 测试私钥：
    /// 仓库内不硬编码任何密钥字面量，避免密钥扫描误报。
    fn test_ed25519_pem() -> String {
        use ssh_key::LineEnding;
        build_keypair(GenAlg::Ed25519)
            .unwrap()
            .to_openssh(LineEnding::LF)
            .unwrap()
            .to_string()
    }

    #[test]
    fn parses_ed25519_metadata() {
        let m = parse_key_meta(test_ed25519_pem().as_bytes()).unwrap();
        assert_eq!(m.algorithm, "ssh-ed25519");
        assert!(m.fingerprint.starts_with("SHA256:"), "{}", m.fingerprint);
        assert!(m.public_key.starts_with("ssh-ed25519 "), "{}", m.public_key);
        assert!(!m.encrypted);
    }

    #[test]
    fn rejects_non_key_text() {
        // 非密钥文本解析失败归 E_KEY（不落库、不 panic）
        let err = parse_key_meta(b"definitely not an ssh key").unwrap_err();
        assert!(matches!(err, AppError::Key(_)));
    }
}
