//! 解析 OpenSSH `~/.ssh/config`，映射为连接档案草稿（EXT：迁移友好）。
//!
//! 只读：Host / HostName / User / Port / IdentityFile / ProxyJump / Compression。
//! 不读私钥内容、不写 known_hosts；导入走既有 `profile_save` 语义由前端批量调用。

use serde::Serialize;

use crate::error::AppError;

/// 一条可导入的连接草稿（不含凭证；IdentityFile 路径非敏感）。
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SshConfigEntry {
    /// `Host` 别名（作默认连接名）。
    pub name: String,
    /// 真实主机（`HostName`，缺省用别名）。
    pub host: String,
    pub port: u16,
    pub username: String,
    pub identity_file: Option<String>,
    /// ProxyJump 目标（原样字符串，如 `bastion`）。
    pub proxy_jump: Option<String>,
}

/// 解析 OpenSSH config 文本：`Host` 块累加；`Host *` 作默认值不单独成条。
pub fn parse_ssh_config(text: &str) -> Vec<SshConfigEntry> {
    #[derive(Default)]
    struct Cur {
        name: String,
        host: String,
        port: u16,
        user: String,
        identity: Option<String>,
        jump: Option<String>,
    }
    fn flush(cur: Cur, out: &mut Vec<SshConfigEntry>) {
        if cur.name.is_empty() || cur.name == "*" {
            return;
        }
        out.push(SshConfigEntry {
            name: cur.name.clone(),
            host: if cur.host.is_empty() {
                cur.name.clone()
            } else {
                cur.host
            },
            port: if cur.port == 0 { 22 } else { cur.port },
            username: cur.user,
            identity_file: cur.identity,
            proxy_jump: cur.jump,
        });
    }

    let mut out = Vec::new();
    let mut cur = Cur::default();
    let mut in_host = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once([' ', '=', '\t']) else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().trim_matches('"'));
        match k.as_str() {
            "host" => {
                if in_host {
                    flush(std::mem::take(&mut cur), &mut out);
                }
                in_host = true;
                // 只取第一个别名（多别名时其余忽略）
                cur.name = v.split_whitespace().next().unwrap_or("").to_string();
            }
            "hostname" if in_host => cur.host = v.to_string(),
            "port" if in_host => cur.port = v.parse().unwrap_or(0),
            "user" if in_host => cur.user = v.to_string(),
            "identityfile" if in_host => cur.identity = Some(v.to_string()),
            "proxyjump" if in_host => cur.jump = Some(v.to_string()),
            _ => {}
        }
    }
    if in_host {
        flush(cur, &mut out);
    }
    out
}

/// 读默认 `~/.ssh/config` 并解析。
pub fn load_default() -> Result<Vec<SshConfigEntry>, AppError> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| AppError::Invalid("无法定位用户目录".into()))?;
    let path = std::path::PathBuf::from(home).join(".ssh").join("config");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| AppError::Io(format!("读取 {} 失败：{e}", path.display())))?;
    Ok(parse_ssh_config(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_blocks() {
        let cfg = r#"
Host web1
    HostName 10.0.0.1
    User root
    Port 2222
    IdentityFile ~/.ssh/id_ed25519

Host *
    User admin

Host db
    HostName db.internal
    ProxyJump web1
"#;
        let list = parse_ssh_config(cfg);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].name, "web1");
        assert_eq!(list[0].port, 2222);
        assert_eq!(list[0].username, "root");
        assert_eq!(list[1].proxy_jump.as_deref(), Some("web1"));
    }
}
