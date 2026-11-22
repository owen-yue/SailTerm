//! FTPS 服务端证书信任（TOFU）：首次由用户裁决、之后按指纹严格比对。
//!
//! 与 [`super::host_key`] 同构，只是对象从 SSH 主机公钥换成 TLS 叶子证书：
//! 比对项是证书 DER 的 SHA-256 指纹（公钥材料，非敏感），**绝不含凭证**。

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use super::{new_id, Db};
use crate::error::AppError;

/// 一条已信任的 FTPS 服务端证书记录。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FtpsCertRecord {
    pub host: String,
    pub port: u16,
    /// `SHA256:` + 冒号分隔十六进制，比对与展示同源。
    pub fingerprint: String,
    /// 证书主体 CN（展示用，可能为空串）。
    pub subject: String,
    /// 有效期区间（展示用，如 `2026-08-20 ~ 2027-08-20`）。
    pub validity: String,
    pub created_at: i64,
}

impl Db {
    pub fn get_ftps_cert(&self, host: &str, port: u16) -> Result<Option<FtpsCertRecord>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT fingerprint, subject, validity, created_at FROM ftps_certs WHERE ident = ?1",
            )?;
            Ok(st
                .query_row(params![ident(host, port)], |r| {
                    Ok(FtpsCertRecord {
                        host: host.to_string(),
                        port,
                        fingerprint: r.get(0)?,
                        subject: r.get(1)?,
                        validity: r.get(2)?,
                        created_at: r.get(3)?,
                    })
                })
                .optional()?)
        })
    }

    /// 记录首次信任的证书（同 host:port 覆盖 —— 服务器换证书后由用户重新裁决）。
    pub fn save_ftps_cert(&self, rec: &FtpsCertRecord) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO ftps_certs (id, ident, fingerprint, subject, validity, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(ident) DO UPDATE SET
                    fingerprint=excluded.fingerprint,
                    subject=excluded.subject,
                    validity=excluded.validity",
                params![
                    new_id(),
                    ident(&rec.host, rec.port),
                    rec.fingerprint,
                    rec.subject,
                    rec.validity,
                    rec.created_at
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_ftps_certs(&self) -> Result<Vec<FtpsCertRecord>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT ident, fingerprint, subject, validity, created_at FROM ftps_certs ORDER BY ident",
            )?;
            let rows = st
                .query_map([], |r| {
                    let ident: String = r.get(0)?;
                    let (host, port) = split_ident(&ident);
                    Ok(FtpsCertRecord {
                        host,
                        port,
                        fingerprint: r.get(1)?,
                        subject: r.get(2)?,
                        validity: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 用户核对后移除记录（服务器换证书 / 怀疑中间人时重来一次 TOFU），返回删除条数。
    pub fn forget_ftps_cert(&self, host: &str, port: u16) -> Result<usize, AppError> {
        self.with(|c| {
            Ok(c.execute(
                "DELETE FROM ftps_certs WHERE ident = ?1",
                params![ident(host, port)],
            )?)
        })
    }
}

fn ident(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

fn split_ident(ident: &str) -> (String, u16) {
    match ident.rsplit_once(':') {
        Some((host, port)) => (host.to_string(), port.parse().unwrap_or(21)),
        None => (ident.to_string(), 21),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::db::now_ms;

    fn record(host: &str, port: u16, fp: &str) -> FtpsCertRecord {
        FtpsCertRecord {
            host: host.into(),
            port,
            fingerprint: fp.into(),
            subject: "CN=UGREEN".into(),
            validity: "2026-08-20 ~ 2027-08-20".into(),
            created_at: now_ms(),
        }
    }

    #[test]
    fn save_lookup_overwrite_forget() {
        let db = Db::open(Path::new(":memory:")).unwrap();
        assert!(db.get_ftps_cert("nas", 21).unwrap().is_none());

        db.save_ftps_cert(&record("nas", 21, "SHA256:aa")).unwrap();
        let got = db.get_ftps_cert("nas", 21).unwrap().unwrap();
        assert_eq!(got.fingerprint, "SHA256:aa");
        assert_eq!(got.subject, "CN=UGREEN");
        // 端口不同视为不同主机
        assert!(db.get_ftps_cert("nas", 990).unwrap().is_none());

        // 同 ident 再存是覆盖（服务器换证书后重新裁决），不产生第二条
        db.save_ftps_cert(&record("nas", 21, "SHA256:bb")).unwrap();
        assert_eq!(db.list_ftps_certs().unwrap().len(), 1);
        assert_eq!(
            db.get_ftps_cert("nas", 21).unwrap().unwrap().fingerprint,
            "SHA256:bb"
        );

        assert_eq!(db.forget_ftps_cert("nas", 21).unwrap(), 1);
        assert!(db.get_ftps_cert("nas", 21).unwrap().is_none());
    }
}
