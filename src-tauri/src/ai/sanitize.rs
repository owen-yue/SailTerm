//! 输出脱敏：工具的**原始**结果在回填 LLM / 出网前必经此处，
//! 是安全路径上不可绕过的节点。
//!
//! # 双层脱敏定位
//! - **第一层·架构级隔离**：凭证/密码/私钥/API Key 从不进 AI 上下文——由 `db/ai.rs`
//!   的 `key_ref` 只走 Vault、工具签名不含凭证来保证，不在本文件。
//! - **第二层·内容级兜底**：命令输出/文件片段里**夹带**的 secret 与 PII，由本 [`Sanitizer`] 按
//!   规则脱敏（正则 + 键值识别 + 熵检测 + 一致化占位）。本文件即第二层。
//!
//! # 扫描层级（递进）
//! [`SanitizeLevel`] 决定脱敏强度，只增不减：
//! - `Loose`（基础）：仅凭证/密钥——PEM 私钥块、`password=`/`token=` 等键值、AWS Key、JWT、Bearer。
//! - `Standard`（标准，默认下限）：+ 连接串（`scheme://user:pass@`、`ssh user@host`）、`/home/<user>`、`*.internal` 等已知元数据。
//! - `Strict`（严格）：+ IP/邮箱/手机等 PII；**云 provider 强制 ≥ 此级**（自动升级）。
//! - `Custom`（偏执）：+ 高熵串兜底 + 用户自定义正则。可疑即脱敏。
//!
//! # 安全不变量
//! - **一致化占位**：同一 IP/邮箱/主机在多处替换为同一记号（`[IP_1]`），AI 仍能推理关联。
//! - **Fail-safe**：脱敏逻辑一旦 panic，[`scrub`] 兜底返回全脱敏占位，绝不 fail-open。
//! - **超长截断**：脱敏后按字节上限保留头尾、中间省略，防撑爆上下文与扩大泄露面。
//!
//! 说明：IPv6 仅实现「完整 8 段」与「带十六进制前缀的 `::` 压缩式」，刻意不匹配裸 `::`，
//! 以免误伤 `std::path` 之类代码路径、`hh:mm:ss` 时间与 MAC；会话已知主机/账号的**精确替换**
//! （双保险的第二保险）留待接入 `SanitizeContext` 时补。

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;

use regex::Regex;

use crate::ai::context::truncate_middle;
use crate::db::ai::SanitizeLevel;

/// 键值机密值被抹除后的占位符。
const REDACTED: &str = "[REDACTED]";
/// 独立令牌（AWS Key / JWT / Bearer）占位符。
const REDACTED_TOKEN: &str = "[REDACTED_TOKEN]";
/// PEM 私钥块占位符。
const REDACTED_PEM: &str = "[REDACTED_PRIVATE_KEY]";
/// 高熵串占位符（偏执级）。
const REDACTED_SECRET: &str = "[REDACTED_SECRET]";

/// 脱敏后单条输出的字节上限（超长保护）；工具各自已有上限，此处再统一收口。
const DEFAULT_MAX_BYTES: usize = 32 * 1024;

/// 层级 → 递进强度序号（`SanitizeLevel` 未派生 `Ord`，在此建立全序，避免改动 db 枚举）。
fn rank(level: SanitizeLevel) -> u8 {
    match level {
        SanitizeLevel::Loose => 0,
        SanitizeLevel::Standard => 1,
        SanitizeLevel::Strict => 2,
        SanitizeLevel::Custom => 3,
    }
}

/// 预编译正则集（「预编译 regex 集合」）；进程内只编译一次。
struct Patterns {
    /// `key=value` / `key: value` 形式的口令/令牌（含 `MYSQL_PWD` 等前后缀）。
    kv_secret: Regex,
    /// 中文口令赋值：`密码/口令/密钥/令牌` 等词 + 系词（是/为/:/：/=/＝）+ 值。`kv_secret` 只识 ASCII
    /// 键名与 `=:` 分隔，中文「数据库密码是 hunter2secret」会漏网——此处补齐，凭证层地板（所有级都走）。
    kv_secret_cjk: Regex,
    /// AWS Access Key Id（`AKIA…` 等前缀 + 16 位大写）。
    aws_key: Regex,
    /// JWT（`eyJ….….…` 三段 base64url）。
    jwt: Regex,
    /// `Authorization: Bearer <token>` 里的 token。
    bearer: Regex,
    /// `Authorization:` 头整行值（覆盖 Bearer/Basic 等多 token 方案，避免只抹首词泄露真值）。
    auth_header: Regex,
    /// 带凭证的 URL：`scheme://user:pass@`。
    url_creds: Regex,
    /// `ssh/scp/sftp user@host`。
    ssh_conn: Regex,
    /// `/home/<user>`、`/Users/<user>`。
    home_user: Regex,
    /// 内网域名 `*.internal/.local/.lan/.corp/.intranet`。
    internal_host: Regex,
    /// IPv4（四段十进制，代码侧再校验 0–255）。
    ipv4: Regex,
    /// IPv6（完整 8 段 / 带前缀的 `::` 压缩式；best-effort）。
    ipv6: Regex,
    /// 邮箱。
    email: Regex,
    /// 中国大陆手机号。
    phone: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        kv_secret: Regex::new(
            r#"(?i)([a-z0-9_]*(?:password|passwd|pwd|secret|token|api[_-]?key|apikey|access[_-]?key|accesskey|private[_-]?key|privatekey|credential|client[_-]?secret|session[_-]?key|auth[_-]?token)[a-z0-9_]*)\s*([=:])\s*([^\s,;]+)"#,
        ).expect("kv_secret 正则应有效"),
        // 三组与 `redact_kv` 契约一致（1=键、2=分隔、3=值）；键必须以中文/英文凭证词收尾，
        // 故「答案是」「目标是」等非凭证句不会被误抹（宁缺勿滥，方向偏保守脱敏）。
        kv_secret_cjk: Regex::new(
            r#"([^\s,;，。；]*?(?:密码|口令|密钥|秘钥|令牌|passphrase))\s*(是|为|:|：|=|＝)\s*([^\s,;，。；]+)"#,
        )
        .expect("kv_secret_cjk 正则应有效"),
        aws_key: Regex::new(r#"\b(?:AKIA|ASIA|ABIA|ACCA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ASCA)[0-9A-Z]{16}\b"#)
            .expect("aws_key 正则应有效"),
        jwt: Regex::new(r#"\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\b"#)
            .expect("jwt 正则应有效"),
        bearer: Regex::new(r#"(?i)\b(bearer)\s+[A-Za-z0-9._~+/=-]{8,}"#).expect("bearer 正则应有效"),
        auth_header: Regex::new(r#"(?i)(\bauthorization\b\s*[:=]\s*)([^\r\n]+)"#).expect("auth_header 正则应有效"),
        url_creds: Regex::new(r#"\b([a-z][a-z0-9+.-]*://)([^\s/:@]+):([^\s@]+)@"#)
            .expect("url_creds 正则应有效"),
        ssh_conn: Regex::new(r#"(?i)\b(ssh|scp|sftp)\s+([^\s@]+)@([^\s]+)"#).expect("ssh_conn 正则应有效"),
        home_user: Regex::new(r#"(/home/|/Users/)([A-Za-z_][A-Za-z0-9_.-]*)"#).expect("home_user 正则应有效"),
        internal_host: Regex::new(r#"(?i)\b[a-z0-9][a-z0-9.-]*\.(?:internal|local|lan|corp|intranet)\b"#)
            .expect("internal_host 正则应有效"),
        ipv4: Regex::new(r#"\b(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})\b"#).expect("ipv4 正则应有效"),
        ipv6: Regex::new(r#"(?i)(?:[0-9a-f]{1,4}:){7}[0-9a-f]{1,4}|(?:[0-9a-f]{2,4}:){1,6}:[0-9a-f]{1,4}"#)
            .expect("ipv6 正则应有效"),
        email: Regex::new(r#"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"#).expect("email 正则应有效"),
        phone: Regex::new(r#"\b1[3-9]\d{9}\b"#).expect("phone 正则应有效"),
    })
}

/// 输出脱敏器。持层级 + 自定义规则 + 字节上限；`Send + Sync`，可跨 await 持有。
#[derive(Clone, Debug)]
pub struct Sanitizer {
    level: SanitizeLevel,
    custom: Vec<Regex>,
    max_bytes: usize,
}

impl Default for Sanitizer {
    /// 默认取 `SanitizeLevel::default()`（`Strict`）——未显式配置时按最稳妥的云模型口径脱敏。
    fn default() -> Self {
        Self::new(SanitizeLevel::default())
    }
}

impl Sanitizer {
    /// 按层级建脱敏器（无自定义规则）。
    pub fn new(level: SanitizeLevel) -> Self {
        Self {
            level,
            custom: Vec::new(),
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// 叠加用户自定义正则规则；**无效模式静默跳过**——绝不因自定义规则失效而放宽基线脱敏。
    // 自定义规则由「设置」功能配置后注入（推迟）；这是填充 self.custom 的唯一入口。
    #[allow(dead_code)]
    pub fn with_custom(level: SanitizeLevel, patterns: &[String]) -> Self {
        let custom = patterns.iter().filter_map(|p| Regex::new(p).ok()).collect();
        Self {
            level,
            custom,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// 覆盖脱敏后的字节上限（链式）；单测与后续「设置」用。
    #[allow(dead_code)]
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes.max(256);
        self
    }

    /// 抹除文本中的明文机密与（按层级的）PII，返回可安全回填模型 / 出网的副本。
    ///
    /// 出网前统一入口：所有终端快照、命令结果、文件片段都必须过此函数。
    pub fn scrub(&self, input: &str) -> String {
        // Fail-safe：脱敏逻辑异常/panic → 全脱敏，绝不把原文漏出去。
        let scrubbed = catch_unwind(AssertUnwindSafe(|| self.scrub_inner(input)))
            .unwrap_or_else(|_| REDACTED.to_string());
        truncate_middle(&scrubbed, self.max_bytes)
    }

    fn scrub_inner(&self, input: &str) -> String {
        let p = patterns();
        let r = rank(self.level);
        let mut ctx = RedactCtx::default();

        // PEM 私钥块：所有层级都抹（危害最大）。
        let mut s = redact_pem(input);

        // ── 凭证类：所有层级（≥ 基础）──
        // Authorization 头先整行脱敏，避免 kv 只抹「Bearer」首词而泄露其后的真 token。
        s = redact_auth_header(&s, &p.auth_header);
        s = redact_bearer(&s, &p.bearer);
        s = redact_kv(&s, &p.kv_secret);
        s = redact_kv(&s, &p.kv_secret_cjk);
        s = p.aws_key.replace_all(&s, REDACTED_TOKEN).into_owned();
        s = p.jwt.replace_all(&s, REDACTED_TOKEN).into_owned();

        // ── 元数据类：标准+ ──
        if r >= rank(SanitizeLevel::Standard) {
            s = redact_url_creds(&s, &p.url_creds, &mut ctx);
            s = redact_ssh_conn(&s, &p.ssh_conn, &mut ctx);
            s = redact_home_user(&s, &p.home_user, &mut ctx);
            s = replace_consistent(&s, &p.internal_host, "HOST", &mut ctx);
        }

        // ── PII 类：严格+ ──
        if r >= rank(SanitizeLevel::Strict) {
            s = replace_ipv4(&s, &p.ipv4, &mut ctx);
            s = replace_consistent(&s, &p.ipv6, "IP", &mut ctx);
            s = replace_consistent(&s, &p.email, "EMAIL", &mut ctx);
            s = replace_consistent(&s, &p.phone, "PHONE", &mut ctx);
        }

        // ── 高熵串兜底：偏执（Custom）──
        if r >= rank(SanitizeLevel::Custom) {
            s = redact_entropy(&s);
        }

        // 用户自定义规则：叠加在所有层级之上。
        for re in &self.custom {
            s = re.replace_all(&s, REDACTED).into_owned();
        }

        s
    }
}

/// 一次 `scrub` 内的一致化占位状态：同一原值 → 同一记号，各类别独立计数。
#[derive(Default)]
struct RedactCtx {
    seen: HashMap<String, String>,
    counters: HashMap<&'static str, usize>,
}

impl RedactCtx {
    /// 取 `original` 的占位符；首次出现按类别分配 `[CAT_n]`，其后复用。
    fn placeholder(&mut self, cat: &'static str, original: &str) -> String {
        if let Some(p) = self.seen.get(original) {
            return p.clone();
        }
        let n = {
            let c = self.counters.entry(cat).or_insert(0);
            *c += 1;
            *c
        };
        let p = format!("[{cat}_{n}]");
        self.seen.insert(original.to_string(), p.clone());
        p
    }
}

/// `key<sep>value` → 保留 `key<sep>`，值替换为 `[REDACTED]`。
fn redact_kv(s: &str, re: &Regex) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for caps in re.captures_iter(s) {
        let whole = caps.get(0).expect("group 0 必存在");
        out.push_str(&s[last..whole.start()]);
        out.push_str(&caps[1]); // key（保留原样，便于 AI 理解字段名）
        out.push_str(&caps[2]); // 分隔符 = 或 :
        out.push_str(REDACTED);
        last = whole.end();
    }
    out.push_str(&s[last..]);
    out
}

/// `Bearer <token>` → 保留 `Bearer`，token 替换。
fn redact_bearer(s: &str, re: &Regex) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for caps in re.captures_iter(s) {
        let whole = caps.get(0).expect("group 0 必存在");
        out.push_str(&s[last..whole.start()]);
        out.push_str(&caps[1]); // "bearer"/"Bearer"
        out.push(' ');
        out.push_str(REDACTED_TOKEN);
        last = whole.end();
    }
    out.push_str(&s[last..]);
    out
}

/// `Authorization: <value>` → 保留 header 名与分隔符，值（到行尾）整体替换为 `[REDACTED]`。
/// 覆盖 `Bearer <token>`/`Basic <base64>` 等多 token 方案——避免只抹首词而泄露真值。
fn redact_auth_header(s: &str, re: &Regex) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for caps in re.captures_iter(s) {
        let whole = caps.get(0).expect("group 0 必存在");
        out.push_str(&s[last..whole.start()]);
        out.push_str(&caps[1]); // "Authorization: "
        out.push_str(REDACTED);
        last = whole.end();
    }
    out.push_str(&s[last..]);
    out
}

/// `scheme://user:pass@` → 保留 `scheme://`，user 用一致化 `[USER_n]`、pass 用 `[REDACTED]`。
fn redact_url_creds(s: &str, re: &Regex, ctx: &mut RedactCtx) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for caps in re.captures_iter(s) {
        let whole = caps.get(0).expect("group 0 必存在");
        out.push_str(&s[last..whole.start()]);
        out.push_str(&caps[1]); // scheme://
        out.push_str(&ctx.placeholder("USER", &caps[2]));
        out.push(':');
        out.push_str(REDACTED);
        out.push('@');
        last = whole.end();
    }
    out.push_str(&s[last..]);
    out
}

/// `ssh user@host` → `ssh [USER_n]@[HOST_n]`。
fn redact_ssh_conn(s: &str, re: &Regex, ctx: &mut RedactCtx) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for caps in re.captures_iter(s) {
        let whole = caps.get(0).expect("group 0 必存在");
        out.push_str(&s[last..whole.start()]);
        out.push_str(&caps[1]); // ssh/scp/sftp
        out.push(' ');
        out.push_str(&ctx.placeholder("USER", &caps[2]));
        out.push('@');
        out.push_str(&ctx.placeholder("HOST", &caps[3]));
        last = whole.end();
    }
    out.push_str(&s[last..]);
    out
}

/// `/home/<user>` → `/home/[USER_n]`（保留前缀路径）。
fn redact_home_user(s: &str, re: &Regex, ctx: &mut RedactCtx) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for caps in re.captures_iter(s) {
        let whole = caps.get(0).expect("group 0 必存在");
        out.push_str(&s[last..whole.start()]);
        out.push_str(&caps[1]); // /home/ 或 /Users/
        out.push_str(&ctx.placeholder("USER", &caps[2]));
        last = whole.end();
    }
    out.push_str(&s[last..]);
    out
}

/// 整段匹配 → 一致化占位 `[CAT_n]`（用于内网域名/IPv6/邮箱/手机）。
fn replace_consistent(s: &str, re: &Regex, cat: &'static str, ctx: &mut RedactCtx) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for m in re.find_iter(s) {
        out.push_str(&s[last..m.start()]);
        out.push_str(&ctx.placeholder(cat, m.as_str()));
        last = m.end();
    }
    out.push_str(&s[last..]);
    out
}

/// IPv4：四段均在 0–255 才算 IP（避免误伤 `1.2.3.999` 之类版本号），命中 → 一致化 `[IP_n]`。
fn replace_ipv4(s: &str, re: &Regex, ctx: &mut RedactCtx) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for caps in re.captures_iter(s) {
        let whole = caps.get(0).expect("group 0 必存在");
        out.push_str(&s[last..whole.start()]);
        let valid = (1..=4).all(|i| caps[i].parse::<u16>().is_ok_and(|v| v <= 255));
        if valid {
            out.push_str(&ctx.placeholder("IP", whole.as_str()));
        } else {
            out.push_str(whole.as_str()); // 非合法 IP：原样保留
        }
        last = whole.end();
    }
    out.push_str(&s[last..]);
    out
}

/// 抹除所有 PEM 私钥块（`-----BEGIN … PRIVATE KEY-----` 到对应 `-----END …-----`）。
/// 非私钥的 PEM（如证书）保留。
fn redact_pem(input: &str) -> String {
    if !input.contains("PRIVATE KEY-----") {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(b) = rest.find("-----BEGIN ") {
        let line_end = rest[b..].find('\n').map(|i| b + i).unwrap_or(rest.len());
        if !rest[b..line_end].contains("PRIVATE KEY") {
            // 非私钥块：原样保留到本行末尾，继续扫描其后。
            out.push_str(&rest[..line_end]);
            rest = &rest[line_end..];
            continue;
        }
        // 私钥块：保留 BEGIN 之前的内容，整块（含 END 行）替换为占位符。
        out.push_str(&rest[..b]);
        out.push_str(REDACTED_PEM);
        rest = match rest[b..].find("-----END ") {
            Some(e) => {
                let abs = b + e;
                let end_line = rest[abs..]
                    .find('\n')
                    .map(|i| abs + i)
                    .unwrap_or(rest.len());
                &rest[end_line..]
            }
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// 偏执级：把高熵长串（疑似密钥/令牌）整体抹除。逐空白分词，仅对满足严格条件的 token 动手，
/// 尽量少误伤正常文本（英文词熵低、无数字，不会命中）。
fn redact_entropy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut tok = String::new();
    for c in s.chars() {
        if c.is_whitespace() {
            push_token(&mut out, &tok);
            tok.clear();
            out.push(c);
        } else {
            tok.push(c);
        }
    }
    push_token(&mut out, &tok);
    out
}

fn push_token(out: &mut String, tok: &str) {
    if tok.is_empty() {
        return;
    }
    out.push_str(if is_high_entropy_secret(tok) {
        REDACTED_SECRET
    } else {
        tok
    });
}

/// 高熵机密判定：长度 ≥ 20、字符集限于 base64/base64url、同时含字母与数字、香农熵 ≥ 3.5。
fn is_high_entropy_secret(tok: &str) -> bool {
    let b = tok.as_bytes();
    if b.len() < 20 {
        return false;
    }
    let allowed =
        |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'=' | b'_' | b'-');
    if !b.iter().copied().all(allowed) {
        return false;
    }
    let has_digit = b.iter().any(|c| c.is_ascii_digit());
    let has_alpha = b.iter().any(|c| c.is_ascii_alphabetic());
    if !(has_digit && has_alpha) {
        return false;
    }
    shannon_entropy(b) >= 3.5
}

/// 香农熵（bit/字节）：随机密钥接近上界，自然语言/路径显著更低。
fn shannon_entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let n = bytes.len() as f64;
    counts.iter().filter(|&&c| c > 0).fold(0.0, |h, &c| {
        let p = c as f64 / n;
        h - p * p.log2()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 凭证类（所有层级）──

    #[test]
    fn redacts_key_value_secrets() {
        let s = Sanitizer::new(SanitizeLevel::Loose);
        assert_eq!(s.scrub("PASSWORD=hunter2"), "PASSWORD=[REDACTED]");
        assert_eq!(s.scrub("api_key: sk-abc123"), "api_key:[REDACTED]");
        // 带前后缀的环境变量名也命中
        assert_eq!(s.scrub("MYSQL_PWD=s3cr3t"), "MYSQL_PWD=[REDACTED]");
        assert_eq!(
            s.scrub("DB_PASSWORD=x host=h"),
            "DB_PASSWORD=[REDACTED] host=h"
        );
    }

    #[test]
    fn redacts_aws_jwt_bearer() {
        let s = Sanitizer::new(SanitizeLevel::Loose);
        let out = s.scrub("aws key AKIAIOSFODNN7EXAMPLE here");
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"), "{out}");
        assert!(out.contains(REDACTED_TOKEN));
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghij1234567890";
        let o2 = s.scrub(jwt);
        assert!(!o2.contains("eyJhbGci"), "{o2}");
        let o3 = s.scrub("Authorization: Bearer abcdefghijklmnop1234");
        assert!(o3.contains("Authorization"), "保留 header 名：{o3}");
        assert!(!o3.contains("abcdefghijklmnop1234"), "token 必须抹除：{o3}");
        // 裸 Bearer（无 Authorization 前缀）也要抹 token
        let o4 = s.scrub("use Bearer abcdefghijklmnop1234 now");
        assert!(!o4.contains("abcdefghijklmnop1234"), "{o4}");
        assert!(o4.contains(REDACTED_TOKEN), "{o4}");
    }

    #[test]
    fn keeps_ordinary_output() {
        let s = Sanitizer::new(SanitizeLevel::Strict);
        let plain = "total 12\ndrwxr-xr-x 2 root root 4096 .";
        assert_eq!(s.scrub(plain), plain);
        assert_eq!(
            s.scrub("this line mentions token but no value"),
            "this line mentions token but no value"
        );
    }

    #[test]
    fn redacts_pem_private_key_block() {
        let s = Sanitizer::new(SanitizeLevel::Loose);
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXkAAAA\nAAABBBB\n\
                   -----END OPENSSH PRIVATE KEY-----\ntail";
        let out = s.scrub(pem);
        assert!(out.contains(REDACTED_PEM), "{out}");
        assert!(!out.contains("b3BlbnNzaC1rZXk"), "{out}");
        assert!(out.contains("tail"), "{out}");
    }

    // ── 层级递进 ──

    #[test]
    fn loose_does_not_redact_ip_but_strict_does() {
        let text = "connected to 10.0.0.5 from 192.168.1.9";
        let loose = Sanitizer::new(SanitizeLevel::Loose).scrub(text);
        assert!(loose.contains("10.0.0.5"), "基础级不动 IP：{loose}");
        let strict = Sanitizer::new(SanitizeLevel::Strict).scrub(text);
        assert!(!strict.contains("10.0.0.5"), "严格级脱敏 IP：{strict}");
        assert!(strict.contains("[IP_"), "{strict}");
    }

    #[test]
    fn standard_redacts_conn_string_and_home() {
        let s = Sanitizer::new(SanitizeLevel::Standard);
        let out = s.scrub("run: ssh deploy@bastion and cd /home/deploy/app");
        assert!(!out.contains("deploy@bastion"), "{out}");
        assert!(out.contains("[USER_"), "{out}");
        assert!(!out.contains("/home/deploy"), "{out}");
        // URL 凭证
        let u = s.scrub("postgres://admin:s3cr3t@db.corp:5432/app");
        assert!(!u.contains("s3cr3t"), "{u}");
        assert!(!u.contains("admin"), "{u}");
        assert!(u.contains("postgres://"), "保留 scheme：{u}");
    }

    #[test]
    fn strict_redacts_email_and_phone() {
        let s = Sanitizer::new(SanitizeLevel::Strict);
        let out = s.scrub("mail alice@example.com or call 13800138000");
        assert!(!out.contains("alice@example.com"), "{out}");
        assert!(!out.contains("13800138000"), "{out}");
        assert!(out.contains("[EMAIL_"), "{out}");
        assert!(out.contains("[PHONE_"), "{out}");
        // 基础级不动 PII
        let loose = Sanitizer::new(SanitizeLevel::Loose).scrub("mail alice@example.com");
        assert!(loose.contains("alice@example.com"), "{loose}");
    }

    // ── 一致化占位──

    #[test]
    fn consistent_placeholders_for_same_value() {
        let s = Sanitizer::new(SanitizeLevel::Strict);
        let out = s.scrub("src 10.0.0.1 -> dst 10.0.0.2, then 10.0.0.1 again");
        // 两个不同 IP 得到不同记号，同一 IP 复用同一记号
        assert!(out.contains("[IP_1]") && out.contains("[IP_2]"), "{out}");
        assert_eq!(out.matches("[IP_1]").count(), 2, "同一 IP 复用：{out}");
        assert!(!out.contains("10.0.0."), "{out}");
    }

    // ── 高熵（偏执级）──

    #[test]
    fn custom_level_redacts_high_entropy_secret() {
        let secret = "aB3xK9mQ7pL2nR8vT5wY1zC4eF6gH0jU"; // 32 位混合，高熵
        let strict = Sanitizer::new(SanitizeLevel::Strict).scrub(secret);
        assert!(
            strict.contains(secret),
            "严格级不识别无键名的高熵串：{strict}"
        );
        let custom = Sanitizer::new(SanitizeLevel::Custom).scrub(secret);
        assert!(
            custom.contains(REDACTED_SECRET),
            "偏执级兜底高熵串：{custom}"
        );
        // 正常英文句子不被误伤
        let prose = "the quick brown fox jumps over the lazy dog again";
        assert_eq!(Sanitizer::new(SanitizeLevel::Custom).scrub(prose), prose);
    }

    // ── 自定义规则 ──

    #[test]
    fn custom_rules_layered_and_invalid_skipped() {
        let s = Sanitizer::with_custom(
            SanitizeLevel::Loose,
            &["PROJ-[0-9]{4}".to_string(), "([invalid".to_string()], // 第二个非法，应被跳过而非报错
        );
        let out = s.scrub("ticket PROJ-1234 and password=x");
        assert!(!out.contains("PROJ-1234"), "自定义规则生效：{out}");
        assert!(
            out.contains("password=[REDACTED]"),
            "基线规则不受影响：{out}"
        );
    }

    // ── 截断 & 边界 ──

    #[test]
    fn truncates_overlong_output_middle() {
        let s = Sanitizer::new(SanitizeLevel::Strict).with_max_bytes(200);
        let big = "line\n".repeat(500);
        let out = s.scrub(&big);
        assert!(out.len() <= 200 + 64, "应被截断到上限附近：{}", out.len());
        assert!(out.contains("中间已省略"), "{out}");
    }

    #[test]
    fn preserves_trailing_newline_shape() {
        let s = Sanitizer::new(SanitizeLevel::Strict);
        assert_eq!(s.scrub("a\n"), "a\n");
        assert_eq!(s.scrub("a"), "a");
        assert_eq!(s.scrub(""), "");
    }

    #[test]
    fn redacts_first_then_truncates() {
        // 头部含机密、整体超长：机密应先被抹掉再截断，头尾都不得残留原文
        let s = Sanitizer::new(SanitizeLevel::Strict).with_max_bytes(300);
        let big = format!("password=supersecretvalue\n{}", "x".repeat(2000));
        let out = s.scrub(&big);
        assert!(!out.contains("supersecretvalue"), "{out}");
    }

    #[test]
    fn entropy_helpers_are_sane() {
        assert!(!is_high_entropy_secret("short"));
        assert!(!is_high_entropy_secret(&"a".repeat(40))); // 无数字
        assert!(shannon_entropy(b"aaaa") < 0.01);
        assert!(shannon_entropy(b"abcd1234!@#$") > 2.0);
    }
}
