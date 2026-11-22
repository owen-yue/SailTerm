//! SSH 隧道 / 端口转发配置持久化。
//!
//! 保存的是**转发规则**（本地 ⇄ 远端 host:port 映射），全是非敏感地址与端口，
//! **绝不含任何凭证**（回归护栏 `tunnel_table_has_no_credential_column`）。
//! 运行时的本地监听与 `direct-tcpip` 转发在 [`crate::ssh::tunnel`]；本层只管配置 CRUD。
//!
//! `kind` 支持 `local` / `remote` / `dynamic`（见 `ssh/tunnel.rs` 运行时）。

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use super::{new_id, now_ms, Db};
use crate::error::AppError;

/// 隧道类型。v1 只实现本地转发；其余分支解析容错保留，便于后续扩展与旧数据兼容。
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum TunnelKind {
    /// 本地转发（`-L`）：本地监听 → 经 SSH 连接转发到远端 host:port。
    Local,
    /// 远程转发（`-R`）：远端监听 → 回连本地（v1 未实现，仅存配置）。
    Remote,
    /// 动态转发（`-D`，SOCKS）：本地 SOCKS 代理（v1 未实现，仅存配置）。
    Dynamic,
}

impl TunnelKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::Dynamic => "dynamic",
        }
    }

    /// 容错解析：库里出现未知值时退回本地转发，不让旧数据把列表带崩。
    pub fn parse(s: &str) -> Self {
        match s {
            "remote" => Self::Remote,
            "dynamic" => Self::Dynamic,
            _ => Self::Local,
        }
    }

    /// v1 是否已实现运行时转发（本地 / 远程 / SOCKS）。
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Local | Self::Remote | Self::Dynamic)
    }
}

/// 一条已保存的隧道转发规则（库内行模型 = 回前端 DTO；不含凭证）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TunnelConfig {
    pub id: String,
    /// 归属连接档案（松散引用，可空 = 不绑定，供临时会话手填目标）。
    pub profile_id: Option<String>,
    pub name: String,
    pub kind: TunnelKind,
    /// 本地监听地址（默认 `127.0.0.1`，只本机可达）。
    pub local_host: String,
    /// 本地监听端口（0 = 由系统分配随机可用端口）。
    pub local_port: u16,
    /// 远端目标主机（经该 SSH 连接可达，如 `127.0.0.1` / 内网 DB 主机）。
    pub remote_host: String,
    pub remote_port: u16,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TunnelConfig {
    /// 新建一条配置：id / 时间戳已填。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profile_id: Option<String>,
        name: impl Into<String>,
        kind: TunnelKind,
        local_host: impl Into<String>,
        local_port: u16,
        remote_host: impl Into<String>,
        remote_port: u16,
        enabled: bool,
    ) -> Self {
        let now = now_ms();
        Self {
            id: new_id(),
            profile_id,
            name: name.into(),
            kind,
            local_host: local_host.into(),
            local_port,
            remote_host: remote_host.into(),
            remote_port,
            enabled,
            created_at: now,
            updated_at: now,
        }
    }
}

/// 查询 / 回前端用的列顺序，必须与 [`map_tunnel`] 的下标一一对应。
const TUNNEL_COLS: &str = "id, profile_id, name, kind, local_host, local_port, remote_host, remote_port, enabled, created_at, updated_at";

impl Db {
    /// 列隧道配置：给了 `profile_id` 只列该连接的，否则列全部；按「名称 → 插入序」稳定排序。
    pub fn list_tunnels(&self, profile_id: Option<&str>) -> Result<Vec<TunnelConfig>, AppError> {
        self.with(|c| {
            // `?1 IS NULL OR profile_id = ?1`：None → 列全部，Some → 只列该连接（单查询免分支）
            let mut st = c.prepare(&format!(
                "SELECT {TUNNEL_COLS} FROM tunnel WHERE (?1 IS NULL OR profile_id = ?1) ORDER BY name ASC, rowid ASC"
            ))?;
            let rows = st
                .query_map(params![profile_id], map_tunnel)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 按 id 取单条。
    pub fn get_tunnel(&self, id: &str) -> Result<Option<TunnelConfig>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!("SELECT {TUNNEL_COLS} FROM tunnel WHERE id = ?1"))?;
            Ok(st.query_row(params![id], map_tunnel).optional()?)
        })
    }

    /// 插入或按 id 更新（`created_at` 保持首次写入值）。
    pub fn upsert_tunnel(&self, t: &TunnelConfig) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO tunnel (id, profile_id, name, kind, local_host, local_port, remote_host, remote_port, enabled, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
                 ON CONFLICT(id) DO UPDATE SET
                    profile_id=excluded.profile_id, name=excluded.name, kind=excluded.kind,
                    local_host=excluded.local_host, local_port=excluded.local_port,
                    remote_host=excluded.remote_host, remote_port=excluded.remote_port,
                    enabled=excluded.enabled, updated_at=excluded.updated_at",
                params![
                    t.id,
                    t.profile_id,
                    t.name,
                    t.kind.as_str(),
                    t.local_host,
                    t.local_port as i64,
                    t.remote_host,
                    t.remote_port as i64,
                    t.enabled as i64,
                    t.created_at,
                    t.updated_at
                ],
            )?;
            Ok(())
        })
    }

    /// 删除单条，返回是否删到（存在与否）。
    pub fn delete_tunnel(&self, id: &str) -> Result<bool, AppError> {
        self.with(|c| {
            let n = c.execute("DELETE FROM tunnel WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
    }
}

fn map_tunnel(r: &Row<'_>) -> rusqlite::Result<TunnelConfig> {
    let kind: String = r.get(3)?;
    let local_port: i64 = r.get(5)?;
    let remote_port: i64 = r.get(7)?;
    let enabled: i64 = r.get(8)?;
    Ok(TunnelConfig {
        id: r.get(0)?,
        profile_id: r.get(1)?,
        name: r.get(2)?,
        kind: TunnelKind::parse(&kind),
        local_host: r.get(4)?,
        local_port: local_port.clamp(0, 65535) as u16,
        remote_host: r.get(6)?,
        remote_port: remote_port.clamp(0, 65535) as u16,
        enabled: enabled != 0,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    /// 造一条本地转发配置（db 层只管存取，不碰真实连接）。
    fn local(profile: Option<&str>, name: &str, lp: u16, rp: u16) -> TunnelConfig {
        TunnelConfig::new(
            profile.map(str::to_string),
            name,
            TunnelKind::Local,
            "127.0.0.1",
            lp,
            "127.0.0.1",
            rp,
            true,
        )
    }

    #[test]
    fn migrates_and_roundtrips_tunnel() {
        let db = db();
        assert!(db.schema_version().unwrap() >= 1);
        assert!(db.list_tunnels(None).unwrap().is_empty());

        let t = local(Some("prof-1"), "本地 PG", 15432, 5432);
        let id = t.id.clone();
        let created = t.created_at;
        db.upsert_tunnel(&t).unwrap();

        let got = db.get_tunnel(&id).unwrap().unwrap();
        assert_eq!(got.name, "本地 PG");
        assert_eq!(got.profile_id.as_deref(), Some("prof-1"));
        assert_eq!(got.kind, TunnelKind::Local);
        assert_eq!(got.local_host, "127.0.0.1");
        assert_eq!(got.local_port, 15432);
        assert_eq!(got.remote_port, 5432);
        assert!(got.enabled);

        // 改名 / 停用：created_at 不覆盖，updated_at 前进
        let mut edited = got.clone();
        edited.name = "生产 PG 转发".into();
        edited.enabled = false;
        edited.updated_at = now_ms() + 1;
        db.upsert_tunnel(&edited).unwrap();
        let after = db.get_tunnel(&id).unwrap().unwrap();
        assert_eq!(after.name, "生产 PG 转发");
        assert!(!after.enabled);
        assert_eq!(after.created_at, created);

        // 删除
        assert!(db.delete_tunnel(&id).unwrap());
        assert!(!db.delete_tunnel(&id).unwrap());
        assert!(db.get_tunnel(&id).unwrap().is_none());
    }

    #[test]
    fn list_filters_by_profile_and_sorts() {
        let db = db();
        for t in [
            local(Some("A"), "zeta", 20001, 1),
            local(Some("A"), "alpha", 20002, 2),
            local(Some("B"), "beta", 20003, 3),
        ] {
            db.upsert_tunnel(&t).unwrap();
        }
        // 按 profile 过滤 + 名称排序
        let a: Vec<String> = db
            .list_tunnels(Some("A"))
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(a, vec!["alpha".to_string(), "zeta".to_string()]);
        // 不指定 profile = 全部
        assert_eq!(db.list_tunnels(None).unwrap().len(), 3);
        assert!(db.list_tunnels(Some("C")).unwrap().is_empty());
    }

    /// 库内端口列被写成越界值（脏数据 / 旧版本）时，读回应夹进 u16 合法区间而不 panic。
    #[test]
    fn clamps_out_of_range_ports() {
        let db = db();
        let t = local(None, "越界", 0, 0);
        let id = t.id.clone();
        db.upsert_tunnel(&t).unwrap();
        // 绕过 u16 结构体，直接把库里的 INTEGER 列改成越界值
        {
            let conn = crate::db::profile::raw(&db);
            conn.execute(
                "UPDATE tunnel SET local_port = 70000, remote_port = 99999 WHERE id = ?1",
                params![id],
            )
            .unwrap();
        } // conn（连接锁）在此释放，下面 get_tunnel 才能再取锁
        let got = db.get_tunnel(&id).unwrap().unwrap();
        assert_eq!(got.local_port, 65535);
        assert_eq!(got.remote_port, 65535);
    }

    ///tunnel 表只存转发地址端口，无任何凭证列。
    #[test]
    fn tunnel_table_has_no_credential_column() {
        let db = db();
        let conn = crate::db::profile::raw(&db);
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('tunnel')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(cols.iter().any(|c| c == "local_port"));
        assert!(cols.iter().any(|c| c == "remote_host"));
        assert!(!cols.iter().any(|c| c == "password"
            || c == "passphrase"
            || c == "private_key"
            || c == "secret"
            || c == "token"
            || c == "api_key"
            || c == "credential_ref"));
    }
}
