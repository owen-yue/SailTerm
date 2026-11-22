//! 连接配置与分组的持久化。
//!
//! **库内不存凭证**：密码/私钥口令/私钥内容一律以 `*_ref` 指向保险库。
//! `ref` 字段对前端不可见（`#[serde(skip)]`），前端只拿到 `has_credential` 之类的布尔标记。

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::{new_id, now_ms, Db};
use crate::error::AppError;
use crate::session::SessionKind;

/// 认证方式（与前端 `AuthType` 字面量一致）。
#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum AuthType {
    #[default]
    Password,
    PrivateKey,
    /// SSH keyboard-interactive（后端强制路由，UI 不再独立展示）。
    /// 保留作 DB 兼容：旧档案存了此值仍可连接（直接走 kbd 不探测）。
    KeyboardInteractive,
    /// 自动协商（SSH 新默认）：连接时先发 none 探测服务端可用方法，
    /// 按优先级 keyboard-interactive > password（档案已存）自动选路。
    Auto,
}

impl AuthType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::PrivateKey => "privateKey",
            Self::KeyboardInteractive => "keyboardInteractive",
            Self::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "privateKey" => Self::PrivateKey,
            "keyboardInteractive" => Self::KeyboardInteractive,
            "auto" => Self::Auto,
            _ => Self::Password,
        }
    }
}

/// `extra` 列的非敏感扩展。私钥**口令**不在此，走保险库。
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub struct ProfileExtra {
    pub private_key_path: Option<String>,
    /// 跳板机（ProxyJump）：指向另一条已保存连接档案的 id；目标经其 `direct-tcpip` 接入。
    pub jump_profile_id: Option<String>,
    /// 串口线参数（仅 `kind=serial`）。
    pub serial: Option<crate::serial::SerialParams>,
    /// 登录后自动执行的命令（仅 SSH 类，注入 PTY）；None/空 = 不执行。
    pub startup_command: Option<String>,
    /// 连接备注（自由文本，仅展示用，不参与连接）。
    pub notes: Option<String>,
    /// Agent 转发（-A）：允许远端经本连接使用本地 ssh-agent。默认关（按受信主机逐台开）。
    pub agent_forward: bool,
    /// X11 转发：远端 GUI 程序经本连接显示到本机 X server。默认关。
    pub x11_forward: bool,
    /// 心跳间隔（秒）；0 = 未配置，连接时折算成默认值（与 `ConnectSpec.keepalive_secs` 同口径）。
    pub keepalive_secs: u64,
    /// FTP 主动模式（PORT）开关；仅 FTP/FTPS 生效，默认关（= 被动）。存 `extra` 列。
    pub ftp_active_mode: bool,
    /// 建连超时（秒）；0 = 用默认。仅经网络的协议生效（串口无握手不适用）。
    pub connect_timeout_secs: u64,
    /// 压缩（zlib@openssh.com）优先开关；默认关（= none 优先，安全默认）。仅 SSH 生效。
    pub compression: bool,
    /// 连上即自动录制终端输出（原样落盘）；默认关。仅有终端输出环的协议生效。
    pub auto_record: bool,
    /// 录制存放目录（相对 app data dir 的子目录名）；None = `recordings/`。
    pub record_dir: Option<String>,
    /// 录制文件名模式（支持 `%n %Y %m %d %H %M %S`）；None = 内置 `{name}-{ts}.log`。
    pub record_file_pattern: Option<String>,
    /// 录制覆盖写（true）或追加写（false，默认）。
    pub record_overwrite: bool,
    /// 录制每行前加时间戳前缀 `[%Y-%m-%d %H:%M:%S] `；默认关。
    pub record_timestamp_prefix: bool,
    /// 录制文件加密落盘（AES-256-GCM，数据密钥归 Vault 保护）；默认关（明文）。
    /// 仅档案显式勾选后启用；旧 extra 缺此键按结构级默认 `false`。
    pub record_encrypt: bool,
    /// 初始终端列数；0 = 用实时尺寸。
    pub term_cols: u16,
    /// 初始终端行数；0 = 用实时尺寸。
    pub term_rows: u16,
    /// 响铃样式覆盖（`none`/`visual`/`sound`）；None = 跟随全局终端偏好。
    pub bell_style: Option<String>,
}

/// 连接档案。既是库内行模型，也是回前端的 DTO（凭证引用被 skip）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub group_id: Option<String>,
    /// 引用的密钥库私钥（ssh_key.id）；Some = 连接握手时从库取该私钥，None = 走内联路径/粘贴。
    pub private_key_id: Option<String>,
    /// 协议类型（库内 `kind` 列，v1 迁移就已存在，默认 `ssh`）。
    pub kind: SessionKind,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_type: AuthType,
    pub private_key_path: Option<String>,
    /// 跳板机档案 id（ProxyJump）；空 = 直连。
    pub jump_profile_id: Option<String>,
    /// 串口线参数（仅 `kind=serial`）。
    pub serial: Option<crate::serial::SerialParams>,
    pub startup_dir: Option<String>,
    /// 登录后自动执行的命令（仅 SSH 类）；空 = 不执行。
    pub startup_command: Option<String>,
    /// 连接备注（自由文本，仅展示用）。
    pub notes: Option<String>,
    /// Agent 转发（-A）开关；仅 SSH 类生效，默认关。
    pub agent_forward: bool,
    /// X11 转发开关；仅 SSH 类生效，默认关（显示端需本机跑 X server，如 VcXsrv）。
    pub x11_forward: bool,
    /// 心跳间隔（秒）；0 = 用默认。SSH 与 FTP 保活同源，两类协议均生效。
    pub keepalive_secs: u64,
    /// FTP 主动模式（PORT）开关；仅 FTP/FTPS 生效，默认关（= 被动）。
    pub ftp_active_mode: bool,
    /// 建连超时（秒）；0 = 用默认。仅经网络的协议生效。
    pub connect_timeout_secs: u64,
    /// 压缩优先开关；默认关。仅 SSH 生效。
    pub compression: bool,
    /// 连上即自动录制开关；默认关。仅有终端输出环的协议生效。
    pub auto_record: bool,
    /// 录制存放目录（相对 app data dir 子目录名）；None = `recordings/`。
    pub record_dir: Option<String>,
    /// 录制文件名模式；None = 内置默认。
    pub record_file_pattern: Option<String>,
    /// 录制覆盖写（true）/ 追加写（false）。
    pub record_overwrite: bool,
    /// 录制每行加时间戳前缀。
    pub record_timestamp_prefix: bool,
    /// 录制文件加密落盘；默认关（明文）。勾选后加密，密钥归 Vault，程序内回放透明解密。
    pub record_encrypt: bool,
    /// 初始终端列数；0 = 用实时尺寸。
    pub term_cols: u16,
    /// 初始终端行数；0 = 用实时尺寸。
    pub term_rows: u16,
    /// 响铃样式覆盖；None = 跟随全局终端偏好。
    pub bell_style: Option<String>,
    pub color_tag: Option<String>,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// 命令层依据保险库回填：是否已存密码 / 私钥内容（前端据此显示「已保存」）。
    pub has_credential: bool,
    pub has_private_key: bool,
    /// 保险库引用：仅可信区使用，绝不出 IPC。
    #[serde(skip)]
    pub credential_ref: Option<String>,
    #[serde(skip)]
    pub private_key_ref: Option<String>,
}

impl Profile {
    /// 新建档案骨架（时间戳/ID 已填，凭证引用由命令层写入）。
    pub fn new(
        name: String,
        host: String,
        port: u16,
        username: String,
        auth_type: AuthType,
    ) -> Self {
        let now = now_ms();
        Self {
            id: new_id(),
            name,
            group_id: None,
            private_key_id: None,
            kind: SessionKind::default(),
            host,
            port,
            username,
            auth_type,
            private_key_path: None,
            jump_profile_id: None,
            serial: None,
            startup_dir: None,
            startup_command: None,
            notes: None,
            agent_forward: false,
            x11_forward: false,
            keepalive_secs: 0,
            ftp_active_mode: false,
            connect_timeout_secs: 0,
            compression: false,
            auto_record: false,
            record_dir: None,
            record_file_pattern: None,
            record_overwrite: false,
            record_timestamp_prefix: false,
            record_encrypt: false,
            term_cols: 0,
            term_rows: 0,
            bell_style: None,
            color_tag: None,
            sort_order: 0,
            created_at: now,
            updated_at: now,
            has_credential: false,
            has_private_key: false,
            credential_ref: None,
            private_key_ref: None,
        }
    }

    fn extra_json(&self) -> String {
        let extra = ProfileExtra {
            private_key_path: self.private_key_path.clone(),
            jump_profile_id: self.jump_profile_id.clone(),
            serial: self.serial.clone(),
            startup_command: self.startup_command.clone(),
            notes: self.notes.clone(),
            agent_forward: self.agent_forward,
            x11_forward: self.x11_forward,
            keepalive_secs: self.keepalive_secs,
            ftp_active_mode: self.ftp_active_mode,
            connect_timeout_secs: self.connect_timeout_secs,
            compression: self.compression,
            auto_record: self.auto_record,
            record_dir: self.record_dir.clone(),
            record_file_pattern: self.record_file_pattern.clone(),
            record_overwrite: self.record_overwrite,
            record_timestamp_prefix: self.record_timestamp_prefix,
            record_encrypt: self.record_encrypt,
            term_cols: self.term_cols,
            term_rows: self.term_rows,
            bell_style: self.bell_style.clone(),
        };
        serde_json::to_string(&extra).unwrap_or_else(|_| "{}".into())
    }
}

/// 连接分组。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub sort_order: i64,
    pub created_at: i64,
}

const PROFILE_COLS: &str = "id, name, group_id, host, port, username, auth_type, \
     startup_dir, extra, color_tag, sort_order, created_at, updated_at, credential_ref, private_key_ref, kind, private_key_id";

impl Db {
    pub fn list_profiles(&self) -> Result<Vec<Profile>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {PROFILE_COLS} FROM connection_profiles ORDER BY sort_order, name COLLATE NOCASE"
            ))?;
            let rows = st.query_map([], row_to_profile)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn get_profile(&self, id: &str) -> Result<Option<Profile>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!(
                "SELECT {PROFILE_COLS} FROM connection_profiles WHERE id = ?1"
            ))?;
            Ok(st.query_row(params![id], row_to_profile).optional()?)
        })
    }

    /// 插入或整行更新（`created_at` 保持首次写入值）。
    pub fn upsert_profile(&self, p: &Profile) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO connection_profiles (id, name, group_id, host, port, username, auth_type,
                    credential_ref, private_key_ref, startup_dir, extra, color_tag, sort_order, created_at, updated_at, kind, private_key_id)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
                 ON CONFLICT(id) DO UPDATE SET
                    name=excluded.name, group_id=excluded.group_id, host=excluded.host, port=excluded.port,
                    username=excluded.username, auth_type=excluded.auth_type,
                    credential_ref=excluded.credential_ref, private_key_ref=excluded.private_key_ref,
                    startup_dir=excluded.startup_dir, extra=excluded.extra, color_tag=excluded.color_tag,
                    sort_order=excluded.sort_order, updated_at=excluded.updated_at, kind=excluded.kind,
                    private_key_id=excluded.private_key_id",
                params![
                    p.id,
                    p.name,
                    p.group_id,
                    p.host,
                    p.port,
                    p.username,
                    p.auth_type.as_str(),
                    p.credential_ref,
                    p.private_key_ref,
                    p.startup_dir,
                    p.extra_json(),
                    p.color_tag,
                    p.sort_order,
                    p.created_at,
                    p.updated_at,
                    p.kind.as_str(),
                    p.private_key_id
                ],
            )?;
            Ok(())
        })
    }

    /// 删除连接配置，并在同一事务里级联清掉归属这台服务器的全部历史：
    /// 传输任务 / 审计 / 命令历史 / 隧道 / 服务器级记忆 / AI 对话与消息。
    /// 注：旧库中 profile_id 为 NULL 的历史行无法回溯归属，不在此列。
    pub fn delete_profile(&self, id: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute("BEGIN IMMEDIATE", [])?;
            let result = (|| -> Result<(), AppError> {
                for sql in [
                    "DELETE FROM transfer_tasks WHERE profile_id = ?1",
                    "DELETE FROM audit_log WHERE profile_id = ?1",
                    "DELETE FROM command_history WHERE profile_id = ?1",
                    "DELETE FROM tunnel WHERE profile_id = ?1",
                    "DELETE FROM memory WHERE connection_id = ?1",
                    "DELETE FROM ai_chat_messages WHERE connection_id = ?1",
                    "DELETE FROM ai_conversations WHERE connection_id = ?1",
                    "DELETE FROM connection_profiles WHERE id = ?1",
                ] {
                    c.execute(sql, params![id])?;
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    c.execute("COMMIT", [])?;
                    Ok(())
                }
                Err(e) => {
                    let _ = c.execute("ROLLBACK", []);
                    Err(e)
                }
            }
        })
    }

    pub fn list_groups(&self) -> Result<Vec<Group>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(
                "SELECT id, name, parent_id, sort_order, created_at FROM connection_groups
                 ORDER BY sort_order, name COLLATE NOCASE",
            )?;
            let rows = st
                .query_map([], |r| {
                    Ok(Group {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        parent_id: r.get(2)?,
                        sort_order: r.get(3)?,
                        created_at: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 新建分组，返回带 ID/时间戳的完整记录。
    pub fn create_group(&self, name: &str, parent_id: Option<&str>) -> Result<Group, AppError> {
        let group = Group {
            id: new_id(),
            name: name.to_string(),
            parent_id: parent_id.map(str::to_string),
            sort_order: 0,
            created_at: now_ms(),
        };
        self.with(|c| {
            c.execute(
                "INSERT INTO connection_groups (id, name, parent_id, sort_order, created_at) VALUES (?1,?2,?3,?4,?5)",
                params![group.id, group.name, group.parent_id, group.sort_order, group.created_at],
            )?;
            Ok(group)
        })
    }

    /// 删除分组；组内连接不删除，仅置为未分组。
    pub fn delete_group(&self, id: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "UPDATE connection_profiles SET group_id = NULL WHERE group_id = ?1",
                params![id],
            )?;
            c.execute("DELETE FROM connection_groups WHERE id = ?1", params![id])?;
            Ok(())
        })
    }

    /// 重命名分组。
    pub fn rename_group(&self, id: &str, name: &str) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "UPDATE connection_groups SET name = ?2 WHERE id = ?1",
                params![id, name],
            )?;
            Ok(())
        })
    }

    /// 批量重排连接：按传入 id 的顺序把 `sort_order` 写成下标（0、1、2…）。
    /// 未出现在列表里的连接不动。整批走一个事务，避免拖拽落定时读到半成品顺序。
    /// 只改 `sort_order`，不触碰 `updated_at`（排序是视图操作，不算内容编辑）。
    pub fn reorder_profiles(&self, ids: &[String]) -> Result<(), AppError> {
        self.with(|c| {
            c.execute("BEGIN IMMEDIATE", [])?;
            let result = (|| -> Result<(), AppError> {
                for (i, id) in ids.iter().enumerate() {
                    c.execute(
                        "UPDATE connection_profiles SET sort_order = ?1 WHERE id = ?2",
                        params![i as i64, id],
                    )?;
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    c.execute("COMMIT", [])?;
                    Ok(())
                }
                Err(e) => {
                    let _ = c.execute("ROLLBACK", []);
                    Err(e)
                }
            }
        })
    }

    /// 批量重排分组：同 [`reorder_profiles`]，作用于 `connection_groups`。
    pub fn reorder_groups(&self, ids: &[String]) -> Result<(), AppError> {
        self.with(|c| {
            c.execute("BEGIN IMMEDIATE", [])?;
            let result = (|| -> Result<(), AppError> {
                for (i, id) in ids.iter().enumerate() {
                    c.execute(
                        "UPDATE connection_groups SET sort_order = ?1 WHERE id = ?2",
                        params![i as i64, id],
                    )?;
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    c.execute("COMMIT", [])?;
                    Ok(())
                }
                Err(e) => {
                    let _ = c.execute("ROLLBACK", []);
                    Err(e)
                }
            }
        })
    }
}

fn row_to_profile(r: &Row<'_>) -> rusqlite::Result<Profile> {
    let extra: Option<String> = r.get(8)?;
    let extra = extra
        .and_then(|json| serde_json::from_str::<ProfileExtra>(&json).ok())
        .unwrap_or_default();
    let auth_type: String = r.get(6)?;
    let kind: String = r.get(15)?;
    Ok(Profile {
        id: r.get(0)?,
        name: r.get(1)?,
        group_id: r.get(2)?,
        kind: SessionKind::parse(&kind),
        host: r.get(3)?,
        port: r.get(4)?,
        username: r.get(5)?,
        auth_type: AuthType::parse(&auth_type),
        startup_dir: r.get(7)?,
        color_tag: r.get(9)?,
        sort_order: r.get(10)?,
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
        has_credential: false,
        has_private_key: false,
        credential_ref: r.get(13)?,
        private_key_ref: r.get(14)?,
        private_key_id: r.get(16)?,
        private_key_path: extra.private_key_path,
        jump_profile_id: extra.jump_profile_id,
        serial: extra.serial,
        startup_command: extra.startup_command,
        notes: extra.notes,
        agent_forward: extra.agent_forward,
        x11_forward: extra.x11_forward,
        keepalive_secs: extra.keepalive_secs,
        ftp_active_mode: extra.ftp_active_mode,
        connect_timeout_secs: extra.connect_timeout_secs,
        compression: extra.compression,
        auto_record: extra.auto_record,
        record_dir: extra.record_dir,
        record_file_pattern: extra.record_file_pattern,
        record_overwrite: extra.record_overwrite,
        record_timestamp_prefix: extra.record_timestamp_prefix,
        record_encrypt: extra.record_encrypt,
        term_cols: extra.term_cols,
        term_rows: extra.term_rows,
        bell_style: extra.bell_style,
    })
}

/// 借出连接以便单测直接断言 SQL 结果。
#[cfg(test)]
pub(crate) fn raw(db: &Db) -> std::sync::MutexGuard<'_, rusqlite::Connection> {
    db.conn.lock().unwrap()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn profile_crud_roundtrip() {
        let db = db();
        assert!(db.list_profiles().unwrap().is_empty());

        let mut p = Profile::new(
            "web-01".into(),
            "10.0.0.1".into(),
            2222,
            "root".into(),
            AuthType::PrivateKey,
        );
        p.private_key_path = Some("/home/me/.ssh/id_ed25519".into());
        p.credential_ref = Some("sail:conn:x:key_passphrase".into());
        db.upsert_profile(&p).unwrap();

        let got = db.get_profile(&p.id).unwrap().unwrap();
        assert_eq!(got.name, "web-01");
        assert_eq!(got.port, 2222);
        assert_eq!(got.auth_type, AuthType::PrivateKey);
        assert_eq!(
            got.private_key_path.as_deref(),
            Some("/home/me/.ssh/id_ed25519")
        );
        assert_eq!(
            got.credential_ref.as_deref(),
            Some("sail:conn:x:key_passphrase")
        );

        // 更新走同一条 upsert：created_at 不变，name 变
        let mut edited = got.clone();
        edited.name = "web-02".into();
        edited.updated_at = now_ms() + 1000;
        db.upsert_profile(&edited).unwrap();
        let after = db.get_profile(&p.id).unwrap().unwrap();
        assert_eq!(after.name, "web-02");
        assert_eq!(after.created_at, p.created_at);

        db.delete_profile(&p.id).unwrap();
        assert!(db.get_profile(&p.id).unwrap().is_none());
    }

    /// 重排：按传入 id 顺序写 sort_order = 下标；list 随后按新顺序返回。
    /// 初始态显式铺 sort_order（0/1），不靠同名次下的名称序——CJK 在 `COLLATE NOCASE`
    /// 下不折叠 ASCII 大小写、回退比较不可靠，名称 ties 的先后本就不该被断言。
    #[test]
    fn reorder_profiles_and_groups_persist_order() {
        let db = db();
        let g1 = db.create_group("甲", None).unwrap();
        let g2 = db.create_group("乙", None).unwrap();
        db.reorder_groups(&[g1.id.clone(), g2.id.clone()]).unwrap();
        let mk = |name: &str| {
            let p = Profile::new(name.into(), "h".into(), 22, "u".into(), AuthType::Password);
            db.upsert_profile(&p).unwrap();
            p.id.clone()
        };
        let a = mk("a");
        let b = mk("b");
        let c = mk("c");

        // 连接默认 sort_order 全 0，同名次按名称排（ASCII 名称序可靠）：a b c
        let ids: Vec<String> = db
            .list_profiles()
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids, vec![a.clone(), b.clone(), c.clone()]);

        // 拖成 c a b
        db.reorder_profiles(&[c.clone(), a.clone(), b.clone()])
            .unwrap();
        let after: Vec<String> = db
            .list_profiles()
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(after, vec![c, a, b]);

        // 分组初始 甲(0) 乙(1) → 拖成 乙 甲
        db.reorder_groups(&[g2.id.clone(), g1.id.clone()]).unwrap();
        let gafter: Vec<String> = db
            .list_groups()
            .unwrap()
            .into_iter()
            .map(|g| g.id)
            .collect();
        assert_eq!(gafter, vec![g2.id, g1.id]);
    }

    #[test]
    fn group_delete_unsets_profiles() {
        let db = db();
        let g = db.create_group("生产", None).unwrap();
        let mut p = Profile::new("a".into(), "h".into(), 22, "u".into(), AuthType::Password);
        p.group_id = Some(g.id.clone());
        db.upsert_profile(&p).unwrap();

        db.delete_group(&g.id).unwrap();
        assert!(db.list_groups().unwrap().is_empty());
        assert!(db.get_profile(&p.id).unwrap().unwrap().group_id.is_none());
    }

    ///回前端的 JSON 里不得出现保险库引用（更不会有明文凭证）。
    #[test]
    fn dto_never_exposes_credential_refs() {
        let mut p = Profile::new("a".into(), "h".into(), 22, "u".into(), AuthType::Password);
        p.credential_ref = Some("sail:conn:a:password".into());
        p.private_key_ref = Some("sail:conn:a:private_key".into());
        let json = serde_json::to_value(&p).unwrap();
        assert!(!json.to_string().contains("sail:conn"));
        assert!(json.get("credentialRef").is_none() && json.get("privateKeyRef").is_none());
        assert!(json.get("hasCredential").is_some());
    }

    ///数据库文件里搜不到明文密码（此处验证 ref 机制不会把明文写进列）。
    #[test]
    fn stored_rows_contain_no_plaintext_secret() {
        let db = db();
        let p = Profile::new("a".into(), "h".into(), 22, "u".into(), AuthType::Password);
        db.upsert_profile(&p).unwrap();
        let conn = raw(&db);
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('connection_profiles')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(!cols
            .iter()
            .any(|c| c == "password" || c == "private_key" || c == "passphrase"));
    }

    /// extra JSON 同时携带私钥路径与跳板机 id；旧数据（仅 privateKeyPath）仍可解析。
    #[test]
    fn extra_json_carries_jump_profile_id() {
        let mut p = Profile::new("t".into(), "h".into(), 22, "u".into(), AuthType::Password);
        p.private_key_path = Some("/k".into());
        p.jump_profile_id = Some("jump-1".into());
        let e: ProfileExtra = serde_json::from_str(&p.extra_json()).unwrap();
        assert_eq!(e.private_key_path.as_deref(), Some("/k"));
        assert_eq!(e.jump_profile_id.as_deref(), Some("jump-1"));
        let old: ProfileExtra = serde_json::from_str(r#"{"privateKeyPath":"/a"}"#).unwrap();
        assert!(old.jump_profile_id.is_none());
    }

    /// FTP 主动模式经 `extra` 列往返；旧数据（无该字段）按 serde default 解为 false，向后兼容。
    #[test]
    fn extra_json_roundtrips_ftp_active_mode() {
        let mut p = Profile::new("t".into(), "h".into(), 21, "u".into(), AuthType::Password);
        p.ftp_active_mode = true;
        let e: ProfileExtra = serde_json::from_str(&p.extra_json()).unwrap();
        assert!(e.ftp_active_mode);
        // 迁移前的存量 extra 不含该键，必须仍能解析且默认被动
        let old: ProfileExtra = serde_json::from_str(r#"{"agentForward":false}"#).unwrap();
        assert!(!old.ftp_active_mode);
    }

    /// 新增的连接/录制/终端配置经 `extra` 列往返；旧数据（缺键）按 serde default 向后兼容。
    #[test]
    fn extra_json_roundtrips_conn_record_term() {
        let mut p = Profile::new("t".into(), "h".into(), 22, "u".into(), AuthType::Password);
        p.connect_timeout_secs = 15;
        p.compression = true;
        p.auto_record = true;
        p.record_dir = Some("logs".into());
        p.record_file_pattern = Some("%n-%Y%m%d".into());
        p.record_overwrite = true;
        p.record_timestamp_prefix = true;
        p.term_cols = 120;
        p.term_rows = 40;
        p.bell_style = Some("visual".into());
        let e: ProfileExtra = serde_json::from_str(&p.extra_json()).unwrap();
        assert_eq!(e.connect_timeout_secs, 15);
        assert!(e.compression);
        assert!(e.auto_record);
        assert_eq!(e.record_dir.as_deref(), Some("logs"));
        assert_eq!(e.record_file_pattern.as_deref(), Some("%n-%Y%m%d"));
        assert!(e.record_overwrite);
        assert!(e.record_timestamp_prefix);
        assert_eq!(e.term_cols, 120);
        assert_eq!(e.term_rows, 40);
        assert_eq!(e.bell_style.as_deref(), Some("visual"));
        // 迁移前的存量 extra 不含这些键，必须仍能解析且全部回落默认（0/false/None）。
        let old: ProfileExtra =
            serde_json::from_str(r#"{"ftpActiveMode":true}"#).unwrap();
        assert_eq!(old.connect_timeout_secs, 0);
        assert!(!old.compression);
        assert!(!old.auto_record);
        assert_eq!(old.record_dir, None);
        assert_eq!(old.bell_style, None);
        assert_eq!(old.term_cols, 0);
        assert_eq!(old.term_rows, 0);
    }
}
