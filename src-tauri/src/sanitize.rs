//! 命令脱敏（种子实现）：入库/进上下文前抹掉命令行里的内联凭证。
//!
//! 将在此基础上扩展为完整 Sanitizer（输出脱敏 + 会话上下文精确替换，
//!）。本模块只做**命令行**级别的保守处理：宁可多遮一点，
//! 也不让密码/token 落到磁盘或出网。

/// `key=value` 形式里视为敏感的 key 片段（小写包含匹配），覆盖 `--password=x`、`TOKEN=x` 等。
const SENSITIVE_KEY_HINTS: &[&str] = &[
    "pass",
    "pwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "credential",
];

/// 「程序名 → (粘连值选项, 分离值选项)」：仅对这些程序生效，避免误伤 `grep -pattern` 之类。
/// 粘连 = `-pMyPass`；分离 = `-u admin:pw`（下一 token 不以 `-` 开头才遮，避免吃掉真实参数）。
const SENSITIVE_FLAGS: &[(&str, &[&str], &[&str])] = &[
    ("mysql", &["-p"], &[]),
    ("mysqldump", &["-p"], &[]),
    ("mariadb", &["-p"], &[]),
    ("redis-cli", &["-a"], &["-a"]),
    ("curl", &["-u"], &["-u"]),
    ("wget", &["--password"], &["--password"]),
    ("sshpass", &["-p"], &["-p"]),
];

/// 抹掉命令中的内联凭证；返回可直接入库/展示的字符串。
pub fn redact_command(cmd: &str) -> String {
    let program = cmd
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    let (inline, separated) = match SENSITIVE_FLAGS.iter().find(|(name, _, _)| *name == program) {
        Some((_, inline, separated)) => (*inline, *separated),
        None => (&[][..], &[][..]),
    };

    let mut out: Vec<String> = Vec::new();
    let mut mask_next = false;
    for token in cmd.split_whitespace() {
        if mask_next && !token.starts_with('-') {
            out.push("***".into());
            mask_next = false;
            continue;
        }
        mask_next = separated.contains(&token);
        out.push(redact_token(token, inline));
    }
    out.join(" ")
}

fn redact_token(token: &str, inline: &[&str]) -> String {
    if let Some((key, _)) = token.split_once('=') {
        if is_sensitive_key(key) {
            return format!("{key}=***");
        }
    }
    match inline
        .iter()
        .find(|f| token.starts_with(**f) && token.len() > f.len())
    {
        Some(flag) => format!("{flag}***"),
        None => token.to_string(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let k = key.trim_start_matches('-').to_ascii_lowercase();
    SENSITIVE_KEY_HINTS.iter().any(|hint| k.contains(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_inline_credentials() {
        assert_eq!(
            redact_command("mysql -uroot -pS3cret -e 'show dbs'"),
            "mysql -uroot -p*** -e 'show dbs'"
        );
        assert_eq!(
            redact_command("curl -u admin:pw https://x.io"),
            "curl -u *** https://x.io"
        );
        assert_eq!(
            redact_command("sshpass -p hunter2 ssh host"),
            "sshpass -p *** ssh host"
        );
        assert_eq!(
            redact_command("export API_TOKEN=abc123"),
            "export API_TOKEN=***"
        );
        assert_eq!(
            redact_command("PGPASSWORD=pw psql -h db -U admin"),
            "PGPASSWORD=*** psql -h db -U admin"
        );
    }

    #[test]
    fn keeps_ordinary_commands_intact() {
        assert_eq!(redact_command("ls -la /var/log"), "ls -la /var/log");
        assert_eq!(
            redact_command("grep -pattern foo bar.txt"),
            "grep -pattern foo bar.txt"
        );
        // `-p` 后跟的是选项（提示输入密码），不吃掉后面的真实参数
        assert_eq!(
            redact_command("mysql -p -e 'show dbs'"),
            "mysql -p -e 'show dbs'"
        );
        assert_eq!(redact_command("ps -p 1234"), "ps -p 1234");
        assert_eq!(redact_command(""), "");
    }
}
