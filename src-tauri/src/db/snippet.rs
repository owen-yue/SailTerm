//! 命令片段 / 快捷指令持久化：常用命令模板，支持分组、
//! 变量占位（`{{name}}`），插入时填值渲染再写入终端。
//!
//! **安全红线**：`content` 是命令模板，**绝不存明文凭证**——保存前由命令层过
//! [`crate::ai::sanitize::Sanitizer`]，命中密码/密钥/令牌即拒存，提示改用 `{{变量}}` 占位
//! （插入时再填值，值不落库）。
//!
//! 分层：本层管存取与排序，以及**变量占位的纯逻辑**（[`parse_variables`] / [`render_snippet`]）；
//! CRUD 命令在 [`crate::commands::snippet`]。`variables` 是从 `content` 派生的只读字段，**不落库**。

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension, Row};
use serde::Serialize;

use super::{new_id, now_ms, Db};
use crate::error::AppError;

/// 一条命令片段（库内行模型 = 回前端 DTO）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    pub id: String,
    /// 片段名（列表展示）。
    pub name: String,
    /// 自由分组标签（空 = 未分组）。
    pub group_name: String,
    /// 命令模板（可含 `{{变量}}` 占位；绝不存明文凭证）。
    pub content: String,
    /// 备注说明（可空）。
    pub description: String,
    /// 从 `content` 解析出的变量名（去重、按首次出现序）；**不落库**，读时计算。
    pub variables: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Snippet {
    /// 新建一条片段（id/时间戳已填，`variables` 由 `content` 解析；分组/描述待调用方按需补）。
    pub fn new(name: impl Into<String>, content: impl Into<String>) -> Self {
        let content = content.into();
        let now = now_ms();
        Self {
            id: new_id(),
            name: name.into(),
            group_name: String::new(),
            variables: parse_variables(&content),
            content,
            description: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// 依 `content` 重算 `variables`（编辑内容后调用，保持派生字段一致）。
    pub fn sync_variables(&mut self) {
        self.variables = parse_variables(&self.content);
    }
}

/// 回前端 / 查询用的列顺序，必须与 [`map_snippet`] 的下标一一对应。
const SNIPPET_COLS: &str = "id, name, group_name, content, description, created_at, updated_at";

impl Db {
    /// 列片段：`group` 为空列全部，否则只列该分组。按「分组名 → 片段名 → 插入序」稳定排序。
    pub fn list_snippets(&self, group: Option<&str>) -> Result<Vec<Snippet>, AppError> {
        self.with(|c| {
            // `?1 IS NULL OR col = ?1` 让「传 None 即不过滤」无需动态拼 SQL（同 memory/audit 惯例）。
            let mut st = c.prepare(&format!(
                "SELECT {SNIPPET_COLS} FROM snippet \
                 WHERE (?1 IS NULL OR group_name = ?1) \
                 ORDER BY group_name ASC, name ASC, rowid ASC"
            ))?;
            let rows = st
                .query_map(params![group], map_snippet)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// 按 id 取单条片段（编辑回填用）。
    pub fn get_snippet(&self, id: &str) -> Result<Option<Snippet>, AppError> {
        self.with(|c| {
            let mut st = c.prepare(&format!("SELECT {SNIPPET_COLS} FROM snippet WHERE id = ?1"))?;
            Ok(st.query_row(params![id], map_snippet).optional()?)
        })
    }

    /// 插入或按 `id` 更新片段（`created_at` 保持首次写入值；`variables` 派生自 content，不落库）。
    pub fn upsert_snippet(&self, s: &Snippet) -> Result<(), AppError> {
        self.with(|c| {
            c.execute(
                "INSERT INTO snippet (id, name, group_name, content, description, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(id) DO UPDATE SET
                    name=excluded.name, group_name=excluded.group_name, content=excluded.content,
                    description=excluded.description, updated_at=excluded.updated_at",
                params![
                    s.id,
                    s.name,
                    s.group_name,
                    s.content,
                    s.description,
                    s.created_at,
                    s.updated_at
                ],
            )?;
            Ok(())
        })
    }

    /// 删除单条片段，返回是否删到（存在与否）。
    pub fn delete_snippet(&self, id: &str) -> Result<bool, AppError> {
        self.with(|c| {
            let n = c.execute("DELETE FROM snippet WHERE id = ?1", params![id])?;
            Ok(n > 0)
        })
    }
}

fn map_snippet(r: &Row<'_>) -> rusqlite::Result<Snippet> {
    let content: String = r.get(3)?;
    Ok(Snippet {
        id: r.get(0)?,
        name: r.get(1)?,
        group_name: r.get(2)?,
        variables: parse_variables(&content),
        content,
        description: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

/// 变量占位语法：`{{name}}`，`name` 限 ASCII 字母/数字/下划线且非空。选此语法是为**避免与 shell
/// 的 `${VAR}` / `$VAR` 冲突**（片段正文常含 shell 变量，不能被误当占位）。
fn is_var_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 从 `start` 起找最近的 `}}`，返回其相对字节偏移；找不到返回 `None`。
fn find_close(bytes: &[u8]) -> Option<usize> {
    let mut j = 0;
    while j + 1 < bytes.len() {
        if bytes[j] == b'}' && bytes[j + 1] == b'}' {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// 尝试把 `content[i..]` 处的 `{{...}}` 解析为合法变量名；成功返回 `(name, 占位总字节长)`。
/// 非法占位（非 `{{` 开头、未闭合、空名、名含非变量字符如中文）一律返回 `None`（按普通文本处理）。
fn placeholder_at(content: &str, i: usize) -> Option<(&str, usize)> {
    let bytes = content.as_bytes();
    if i + 1 >= bytes.len() || bytes[i] != b'{' || bytes[i + 1] != b'{' {
        return None;
    }
    let start = i + 2;
    let rel = find_close(&bytes[start..])?;
    let name = &content[start..start + rel];
    let nb = name.as_bytes();
    if nb.is_empty() || !nb.iter().all(|b| is_var_byte(*b)) {
        return None;
    }
    Some((name, rel + 4)) // 4 = 前导 `{{` + 收尾 `}}`
}

/// 解析正文里的变量名：去重、按首次出现顺序返回。非法/未闭合占位忽略。
pub fn parse_variables(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < content.len() {
        match placeholder_at(content, i) {
            Some((name, len)) => {
                if !out.iter().any(|v| v == name) {
                    out.push(name.to_string());
                }
                i += len;
            }
            None => {
                // 普通字符：按 char 推进，避免切进多字节中间（i < len 且总在 char 边界，必非空）
                let c = content[i..].chars().next().unwrap();
                i += c.len_utf8();
            }
        }
    }
    out
}

/// 用 `values` 渲染正文：把每个合法 `{{name}}` 占位替换为对应值（缺失则替空串）。
/// 非法占位（空名/未闭合/名含非变量字符）原样保留。与 [`parse_variables`] 共用同一语法。
pub fn render_snippet(content: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(content.len());
    let mut i = 0;
    while i < content.len() {
        match placeholder_at(content, i) {
            Some((name, len)) => {
                out.push_str(values.get(name).map(String::as_str).unwrap_or(""));
                i += len;
            }
            None => {
                let c = content[i..].chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn db() -> Db {
        Db::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn migrates_and_roundtrips_snippet() {
        let db = db();
        assert!(db.schema_version().unwrap() >= 1);
        assert!(db.list_snippets(None).unwrap().is_empty());

        let mut s = Snippet::new("重启服务", "sudo systemctl restart {{svc}}");
        s.group_name = "运维".into();
        s.description = "重启指定 systemd 服务".into();
        db.upsert_snippet(&s).unwrap();

        let got = db.get_snippet(&s.id).unwrap().unwrap();
        assert_eq!(got.name, "重启服务");
        assert_eq!(got.group_name, "运维");
        assert_eq!(got.description, "重启指定 systemd 服务");
        assert_eq!(got.content, "sudo systemctl restart {{svc}}");
        assert_eq!(got.variables, vec!["svc".to_string()]);

        // 编辑：改 content/name，created_at 不被覆盖，updated_at 前进，variables 随之更新
        let mut edited = got.clone();
        edited.name = "重启并查看状态".into();
        edited.content = "sudo systemctl restart {{svc}} && systemctl status {{svc}}".into();
        edited.sync_variables();
        edited.updated_at = now_ms() + 1;
        db.upsert_snippet(&edited).unwrap();
        let after = db.get_snippet(&s.id).unwrap().unwrap();
        assert_eq!(after.name, "重启并查看状态");
        assert_eq!(after.created_at, s.created_at);
        assert_eq!(after.variables, vec!["svc".to_string()]); // 去重后仍一个

        // 删除
        assert!(db.delete_snippet(&s.id).unwrap());
        assert!(!db.delete_snippet(&s.id).unwrap());
        assert!(db.get_snippet(&s.id).unwrap().is_none());
    }

    #[test]
    fn list_filters_by_group_and_sorts() {
        let db = db();
        let mut a = Snippet::new("b 片段", "echo b");
        a.group_name = "z 组".into();
        let mut b = Snippet::new("a 片段", "echo a");
        b.group_name = "z 组".into();
        let c = Snippet::new("未分组片段", "echo c"); // group_name 默认空
        for s in [&a, &b, &c] {
            db.upsert_snippet(s).unwrap();
        }
        // 全部：按 group_name('' 最先) → name 排序
        let all = db.list_snippets(None).unwrap();
        let names: Vec<&str> = all.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["未分组片段", "a 片段", "b 片段"]);
        // 按分组过滤
        let zg = db.list_snippets(Some("z 组")).unwrap();
        assert_eq!(zg.len(), 2);
        assert!(db.list_snippets(Some("无此组")).unwrap().is_empty());
    }

    #[test]
    fn parse_variables_dedup_order_and_guards() {
        // 去重 + 首次出现序
        assert_eq!(
            parse_variables("{{svc}} on {{host}}; restart {{svc}}"),
            vec!["svc".to_string(), "host".to_string()]
        );
        // 无占位
        assert!(parse_variables("ls -la /var/log").is_empty());
        // shell 变量不误判（$VAR / ${VAR} 不是占位）
        assert!(parse_variables("echo $HOME ${USER}").is_empty());
        // 空名 / 未闭合 / 含非变量字符（中文、连字符、空格）一律忽略
        assert!(parse_variables("{{}} {{x {{中文}} {{a-b}} {{a b}}").is_empty());
        // 合法：字母数字下划线
        assert_eq!(parse_variables("{{a1_B}}"), vec!["a1_B".to_string()]);
    }

    #[test]
    fn render_substitutes_values_and_guards() {
        let mut vals = BTreeMap::new();
        vals.insert("svc".to_string(), "nginx".to_string());
        vals.insert("host".to_string(), "web-01".to_string());
        assert_eq!(
            render_snippet("systemctl restart {{svc}} on {{host}}", &vals),
            "systemctl restart nginx on web-01"
        );
        // 缺失变量替空串
        assert_eq!(render_snippet("[{{gone}}]", &vals), "[]");
        // shell 变量与非法占位原样保留；多字节正文不破坏
        assert_eq!(
            render_snippet("echo $HOME {{}} {{中文}} 完成 {{svc}}", &vals),
            "echo $HOME {{}} {{中文}} 完成 nginx"
        );
        // 无占位原样返回
        assert_eq!(render_snippet("pwd", &vals), "pwd");
    }

    ///snippet 表没有任何明文凭证列（敏感值只应在 content 里以 {{占位}} 出现，
    /// 且保存前经 Sanitizer 拒绝硬凭证）。
    #[test]
    fn snippet_table_has_no_credential_column() {
        let db = db();
        let conn = crate::db::profile::raw(&db);
        let cols: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('snippet')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(cols.iter().any(|c| c == "content"));
        assert!(!cols.iter().any(|c| c == "password"
            || c == "api_key"
            || c == "secret"
            || c == "token"
            || c == "private_key"));
    }
}
