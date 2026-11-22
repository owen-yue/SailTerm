//! known_hosts 持久化：TOFU 首次信任、后续严格比对。

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use super::{new_id, Db};
use crate::error::AppError;

/// 一条已信任的主机公钥记录。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyRecord {
    pub host: String,
    pub port: u16,
    pub key_type: String,
    /// base64 公钥 blob。
    pub key_blob: String,
    /// SHA256 指纹（展示用，如 `SHA256:xxxx`）。
    pub fingerprint: String,
    pub created_at: i64,
}

impl Db {
    pub fn get_host_key(
        &self,
        host: &str,
        port: u16,
        key_type: &str,
    ) -> Result<Option<HostKeyRecord>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT key_blob, fingerprint, created_at FROM known_hosts
                 WHERE host_key_hash = ?1 AND key_type = ?2",
            )?;
            Ok(st
                .query_row(params![ident(host, port), key_type], |r| {
                    Ok(HostKeyRecord {
                        host: host.to_string(),
                        port,
                        key_type: key_type.to_string(),
                        key_blob: r.get(0)?,
                        fingerprint: r.get(1)?,
                        created_at: r.get(2)?,
                    })
                })
                .optional()?)
        })
    }

    /// 记录首次信任的主机密钥（同 host+key_type 覆盖）。
    pub fn save_host_key(&self, rec: &HostKeyRecord) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO known_hosts (id, host_key_hash, key_type, key_blob, fingerprint, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(host_key_hash, key_type) DO UPDATE SET
                    key_blob=excluded.key_blob, fingerprint=excluded.fingerprint",
                params![new_id(), ident(&rec.host, rec.port), rec.key_type, rec.key_blob, rec.fingerprint, rec.created_at],
            )?;
            Ok(())
        })
    }

    /// 用户核对指纹后移除记录（例如服务器重装），返回删除条数。
    pub fn forget_host_keys(&self, host: &str, port: u16) -> Result<usize, AppError> {
        self.with(|c| {
            Ok(c.execute(
                "DELETE FROM known_hosts WHERE host_key_hash = ?1",
                params![ident(host, port)],
            )?)
        })
    }

    pub fn list_host_keys(&self) -> Result<Vec<HostKeyRecord>, AppError> {
        self.with(|c| {
            let mut st = c.prepare("SELECT host_key_hash, key_type, key_blob, fingerprint, created_at FROM known_hosts")?;
            let rows = st
                .query_map([], |r| {
                    let ident: String = r.get(0)?;
                    let (host, port) = split_ident(&ident);
                    Ok(HostKeyRecord {
                        host,
                        port,
                        key_type: r.get(1)?,
                        key_blob: r.get(2)?,
                        fingerprint: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }
}

fn ident(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

fn split_ident(ident: &str) -> (String, u16) {
    match ident.rsplit_once(':') {
        Some((host, port)) => (host.to_string(), port.parse().unwrap_or(22)),
        None => (ident.to_string(), 22),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::db::now_ms;

    fn record(host: &str, port: u16) -> HostKeyRecord {
        HostKeyRecord {
            host: host.into(),
            port,
            key_type: "ssh-ed25519".into(),
            key_blob: "blob".into(),
            fingerprint: "SHA256:abc".into(),
            created_at: now_ms(),
        }
    }

    #[test]
    fn save_lookup_forget() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        assert!(db.get_host_key("h", 22, "ssh-ed25519").unwrap().is_none());

        db.save_host_key(&record("h", 22)).unwrap();
        let got = db.get_host_key("h", 22, "ssh-ed25519").unwrap().unwrap();
        assert_eq!(got.fingerprint, "SHA256:abc");
        // 端口不同视为不同主机
        assert!(db.get_host_key("h", 2222, "ssh-ed25519").unwrap().is_none());

        assert_eq!(db.list_host_keys().unwrap().len(), 1);
        assert_eq!(db.forget_host_keys("h", 22).unwrap(), 1);
        assert!(db.get_host_key("h", 22, "ssh-ed25519").unwrap().is_none());
    }
}
