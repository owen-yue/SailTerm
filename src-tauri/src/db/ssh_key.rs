//! SSH 密钥管理持久化：导入的私钥材料只存保险库，
//! 本表只留非敏感元数据（算法 / 指纹 / 公钥 / 注释）+ 保险库键名 `key_ref`。
//!
//! **安全红线**：表中**无任何私钥 / 口令明文列**（回归护栏
//! `ssh_key_table_has_no_credential_column`）；私钥材料经 [`crate::vault::Vault`] 加密存储，
//! `key_ref` 只是保险库键名。公钥与指纹按定义即公开信息，可安全回前端展示 / 核对。
//!
//! 分层：本层只管元数据存取与指纹查重；解析私钥、出入保险库在 [`crate::commands::ssh_key`]。

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use super::{new_id, now_ms, Db};
use crate::error::AppError;

/// 一把已导入的 SSH 密钥（库内行模型 = 回前端 DTO；不含任何私钥材料）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SshKey {
    pub id: String,
    /// 用户命名（列表展示）。
    pub name: String,
    /// 算法名（`ssh-ed25519` / `ssh-rsa` / `ecdsa-sha2-nistp256` …）。
    pub algorithm: String,
    /// SHA256 指纹（`SHA256:…`），供与服务器端核对。
    pub fingerprint: String,
    /// authorized_keys 格式公钥串（公钥本就公开，可安全展示 / 复制）。
    pub public_key: String,
    /// 密钥自带注释（可空）。
    pub comment: String,
    /// 私钥是否带口令加密（前端据此提示连接时需口令）。
    pub encrypted: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// 保险库键名（私钥材料所在）；**不回前端**（`serde(skip)`），仅后端取私钥时用。
    #[serde(skip_serializing)]
    pub key_ref: String,
}

impl SshKey {
    /// 由解析出的密钥元数据新建一条记录：id / 时间戳已填，`key_ref` 按 id 生成（保险库命名空间）。
    pub fn new(
        name: impl Into<String>,
        algorithm: impl Into<String>,
        fingerprint: impl Into<String>,
        public_key: impl Into<String>,
        comment: impl Into<String>,
        encrypted: bool,
    ) -> Self {
        let id = new_id();
        let now = now_ms();
        let key_ref = crate::vault::keys::ssh_key(&id);
        Self {
            id,
            name: name.into(),
            algorithm: algorithm.into(),
            fingerprint: fingerprint.into(),
            public_key: public_key.into(),
            comment: comment.into(),
            encrypted,
            created_at: now,
            updated_at: now,
            key_ref,
        }
    }
}

/// 查询 / 回前端用的列顺序，必须与 [`map_ssh_key`] 的下标一一对应。
const SSH_KEY_COLS: &str =
    "id, name, algorithm, fingerprint, public_key, comment, encrypted, key_ref, created_at, updated_at";

impl Db {
    /// 列全部密钥，按「名称 → 插入序」稳定排序。
    pub fn list_ssh_keys(&self) -> Result<Vec<SshKey>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {SSH_KEY_COLS} FROM ssh_key ORDER BY name ASC, rowid ASC"
            ))?;
            let rows = st
                .query_map([], map_ssh_key)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 按 id 取单条。
    pub fn get_ssh_key(&self, id: &str) -> Result<Option<SshKey>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!("SELECT {SSH_KEY_COLS} FROM ssh_key WHERE id = ?1"))?;
            Ok(st.query_row(params![id], map_ssh_key).optional()?)
        })
    }

    /// 按指纹查重（导入去重）：同一把密钥（指纹相同）不重复入库。
    pub fn find_ssh_key_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> Result<Option<SshKey>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {SSH_KEY_COLS} FROM ssh_key WHERE fingerprint = ?1"
            ))?;
            Ok(st.query_row(params![fingerprint], map_ssh_key).optional()?)
        })
    }

    /// 插入或按 id 更新（`created_at` / `key_ref` 保持首次写入值）。
    pub fn upsert_ssh_key(&self, k: &SshKey) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO ssh_key (id, name, algorithm, fingerprint, public_key, comment, encrypted, key_ref, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                 ON CONFLICT(id) DO UPDATE SET
                    name=excluded.name, algorithm=excluded.algorithm, fingerprint=excluded.fingerprint,
                    public_key=excluded.public_key, comment=excluded.comment, encrypted=excluded.encrypted,
                    updated_at=excluded.updated_at",
                params![
                    k.id,
                    k.name,
                    k.algorithm,
                    k.fingerprint,
                    k.public_key,
                    k.comment,
                    k.encrypted as i64,
                    k.key_ref,
                    k.created_at,
                    k.updated_at
                ],
            )?;
            Ok(())
        })
    }

    /// 删除单条，返回是否删到（存在与否）。
    pub fn delete_ssh_key(&self, id: &str) -> Result<bool, AppError> {
        self.with(|c| {
            let n = c.execute("DELETE FROM ssh_key WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
    }
}

fn map_ssh_key(r: &Row<'_>) -> rusqlite::Result<SshKey> {
    let encrypted: i64 = r.get(6)?;
    Ok(SshKey {
        id: r.get(0)?,
        name: r.get(1)?,
        algorithm: r.get(2)?,
        fingerprint: r.get(3)?,
        public_key: r.get(4)?,
        comment: r.get(5)?,
        encrypted: encrypted != 0,
        key_ref: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    /// 造一条测试密钥（元数据随意，db 层只管存取，不碰真实私钥）。
    fn key(name: &str, fp: &str) -> SshKey {
        SshKey::new(
            name,
            "ssh-ed25519",
            fp,
            format!("ssh-ed25519 AAAA{fp} {name}"),
            name,
            false,
        )
    }

    #[test]
    fn migrates_and_roundtrips_ssh_key() {
        let db = db();
        assert!(db.schema_version().unwrap() >= 1);
        assert!(db.list_ssh_keys().unwrap().is_empty());

        let k = key("生产 ed25519", "SHA256:aaaa");
        let id = k.id.clone();
        let created = k.created_at;
        db.upsert_ssh_key(&k).unwrap();

        let got = db.get_ssh_key(&id).unwrap().unwrap();
        assert_eq!(got.name, "生产 ed25519");
        assert_eq!(got.algorithm, "ssh-ed25519");
        assert_eq!(got.fingerprint, "SHA256:aaaa");
        assert_eq!(got.comment, "生产 ed25519");
        assert!(!got.encrypted);
        assert_eq!(got.key_ref, crate::vault::keys::ssh_key(&id));

        // 改名 / 标记加密：created_at 与 key_ref 不覆盖，updated_at 前进
        let mut edited = got.clone();
        edited.name = "生产环境密钥".into();
        edited.encrypted = true;
        edited.updated_at = now_ms() + 1;
        db.upsert_ssh_key(&edited).unwrap();
        let after = db.get_ssh_key(&id).unwrap().unwrap();
        assert_eq!(after.name, "生产环境密钥");
        assert!(after.encrypted);
        assert_eq!(after.created_at, created);
        assert_eq!(after.key_ref, crate::vault::keys::ssh_key(&id));

        // 删除
        assert!(db.delete_ssh_key(&id).unwrap());
        assert!(!db.delete_ssh_key(&id).unwrap());
        assert!(db.get_ssh_key(&id).unwrap().is_none());
    }

    #[test]
    fn list_sorts_by_name() {
        let db = db();
        for k in [
            key("zeta", "SHA256:z"),
            key("alpha", "SHA256:a"),
            key("mid", "SHA256:m"),
        ] {
            db.upsert_ssh_key(&k).unwrap();
        }
        let names: Vec<String> = db
            .list_ssh_keys()
            .unwrap()
            .into_iter()
            .map(|k| k.name)
            .collect();
        assert_eq!(
            names,
            vec!["alpha".to_string(), "mid".to_string(), "zeta".to_string()]
        );
    }

    #[test]
    fn dedup_by_fingerprint() {
        let db = db();
        let k = key("唯一键", "SHA256:unique");
        db.upsert_ssh_key(&k).unwrap();
        assert_eq!(
            db.find_ssh_key_by_fingerprint("SHA256:unique")
                .unwrap()
                .unwrap()
                .id,
            k.id
        );
        assert!(db
            .find_ssh_key_by_fingerprint("SHA256:nope")
            .unwrap()
            .is_none());
    }

    ///ssh_key 表只有非敏感元数据 + 公钥，无任何私钥 / 口令明文列
    /// （私钥材料只应在保险库，key_ref 仅存键名）。
    #[test]
    fn ssh_key_table_has_no_credential_column() {
        let db = db();
        let conn = crate::db::profile::raw(&db);
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('ssh_key')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(cols.iter().any(|c| c == "public_key"));
        assert!(cols.iter().any(|c| c == "key_ref"));
        assert!(!cols.iter().any(|c| c == "private_key"
            || c == "passphrase"
            || c == "password"
            || c == "secret"
            || c == "token"
            || c == "api_key"
            || c == "pem"));
    }
}
