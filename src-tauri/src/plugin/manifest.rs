//! 插件 manifest：id / 版本 / 权限 / 命令包 / AI 技能 / 设置 schema / wasm 入口。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 能力权限：出沙箱调用前由能力网关核对。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// 读写本插件隔离存储（settings 表）。
    Storage,
    /// 在绑定会话上执行**只读**命令（经 PolicyEngine 裁决）。
    SessionExec,
    /// 向 UI 推通知（toast）。
    Notify,
    /// 写诊断日志（不落敏感数据）。
    Log,
    /// 出网 HTTP(S)（受宿主超时与响应大小限制）。
    Network,
    /// 向绑定会话终端写入字节（注入提示/状态，不代替用户键入）。
    SessionWrite,
    /// 字节桥：与宿主之间开命名全双工通道（X11 / 自定义协议扩展点）。
    Bridge,
    /// 向 AI 贡献技能（`aiTools` 声明项，模型可在会话里调用本插件的 wasm 导出）。
    AiTool,
}

impl Permission {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "storage" => Self::Storage,
            "session_exec" => Self::SessionExec,
            "notify" => Self::Notify,
            "log" => Self::Log,
            "network" => Self::Network,
            "session_write" => Self::SessionWrite,
            "bridge" => Self::Bridge,
            "ai_tool" => Self::AiTool,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Storage => "storage",
            Self::SessionExec => "session_exec",
            Self::Notify => "notify",
            Self::Log => "log",
            Self::Network => "network",
            Self::SessionWrite => "session_write",
            Self::Bridge => "bridge",
            Self::AiTool => "ai_tool",
        }
    }
}

/// 设置字段声明：前端按 type 动态渲染表单。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PluginSettingSpec {
    pub key: String,
    /// `string` | `number` | `boolean` | `select`
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub default: String,
    /// `select` 的候选值。
    #[serde(default)]
    pub options: Vec<String>,
}

/// 命令包条目：模板里 `{{key}}` 用设置值替换后在会话上执行。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PluginCommand {
    pub id: String,
    pub title: String,
    /// shell 模板，支持 `{{setting_key}}` 与 `{{arg}}`。
    pub template: String,
    #[serde(default)]
    pub description: String,
    /// 执行该命令所需的会话能力档位（取值见 `SessionKind::caps` / 前端 `capabilities.ts`）。
    /// 缺省 `"exec"`：命令包统一经 exec 通道跑，故 FTP/FTPS（无 exec）上相关菜单/按钮不展示。
    /// 填 `"none"` 表示不依赖任何会话通路（宿主不再按能力门控）。
    #[serde(default = "default_command_requires")]
    pub requires: String,
}

fn default_command_requires() -> String {
    "exec".into()
}

/// 工具栏/顶栏按钮贡献。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PluginUiButton {
    pub id: String,
    pub title: String,
    /// 可选短图标字符（如 ⚡）。
    #[serde(default)]
    pub icon: String,
    /// `topbar` | `files` | `terminal`。
    #[serde(default = "default_target_topbar")]
    pub target: String,
    /// 点击后执行的命令包 id。
    pub command_id: String,
}

fn default_target_topbar() -> String {
    "topbar".into()
}

/// 右键菜单项贡献。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PluginUiMenu {
    pub id: String,
    pub title: String,
    /// `file` | `dir` | `terminal` | `always`。
    #[serde(default = "default_menu_when")]
    pub when: String,
    pub command_id: String,
}

fn default_menu_when() -> String {
    "always".into()
}

/// 插件贡献的 AI 技能（manifest.aiTools 条目）：模型在会话里看到的就是一个普通工具，
/// 执行落在本插件 wasm 的 `export` 导出上（见 `plugin::host::call_ai_tool`）。
///
/// schema 直接声明在 manifest（而非向 wasm 多要一次导出）：注册表每轮都要拿全量
/// schema 发给模型，从 manifest 读零成本；wasm 只在真被调用时才加载。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PluginAiTool {
    /// 包内唯一 id（宿主管理/启用开关用它）。
    pub id: String,
    /// LLM 可见的工具名：`[a-z0-9_]{1,64}`，各家 function-calling 规范的公约数格式。
    pub name: String,
    /// 进 schema 的一句话描述：教模型何时用、边界在哪。
    #[serde(default)]
    pub description: String,
    /// 所需会话能力档位（缺省 `"none"`：技能多数不依赖会话通路，与命令包的缺省 `exec` 相反）。
    #[serde(default = "default_ai_requires")]
    pub requires: String,
    /// 静态风险级：`readonly`（缺省，直通）| `confirm`（每次调用过审批卡）。
    #[serde(default = "default_ai_risk")]
    pub risk: String,
    /// wasm 导出名（缺省 `ai_tool_<name>`）。
    #[serde(default)]
    pub export: String,
    /// 参数 JSON Schema（OpenAI `function.parameters` 形态，必须是 `{"type":"object",...}`）。
    #[serde(default = "default_empty_params")]
    pub parameters: Value,
}

fn default_ai_requires() -> String {
    "none".into()
}

fn default_ai_risk() -> String {
    "readonly".into()
}

fn default_empty_params() -> Value {
    serde_json::json!({ "type": "object", "properties": {} })
}

/// 语料包贡献（manifest.locales）：插件可为界面提供新语言（与内置 zh/en 同权）。
/// `path` 是插件目录内的相对 JSON 路径（平面「键 → 文案」对象），由后端限制在目录内防穿越。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PluginLocale {
    /// 语言码（BCP-47 主子标签，如 ja/ko；`"system"` 为保留值，parse 时拒收）。
    pub code: String,
    /// 展示名（用该语言自身书写，如「日本語」）。
    #[serde(default)]
    pub label: String,
    /// 语料 JSON 相对路径（如 `locales/ja.json`）。
    pub path: String,
}

/// 插件包描述（`manifest.json`）。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// 能力权限列表（空 = 仅 log）。
    #[serde(default)]
    pub permissions: Vec<String>,
    /// 可选 WASM 相对路径；无则为纯命令包。
    #[serde(default)]
    pub wasm: Option<String>,
    #[serde(default)]
    pub commands: Vec<PluginCommand>,
    #[serde(default)]
    pub settings: Vec<PluginSettingSpec>,
    /// 声明要挂的钩子：`on_output` / `on_command` / `on_timer` / `on_connect`。
    #[serde(default)]
    pub hooks: Vec<String>,
    /// `on_timer` 间隔秒；0/缺省 = 不定时。
    #[serde(default)]
    pub timer_secs: u32,
    /// 声明的字节桥名（X11 / 自定义流协议扩展点）。
    #[serde(default)]
    pub bridges: Vec<String>,
    /// UI 按钮（顶栏 / 文件栏 / 终端）。
    #[serde(default)]
    pub buttons: Vec<PluginUiButton>,
    /// 右键菜单项。
    #[serde(default)]
    pub menus: Vec<PluginUiMenu>,
    /// 插件贡献的 AI 技能（需同时声明 `ai_tool` 权限，安装时可见）。
    #[serde(default)]
    pub ai_tools: Vec<PluginAiTool>,
    /// 语料包贡献（可选）：前端启动时拉取 JSON 并注册进 i18n。
    #[serde(default)]
    pub locales: Vec<PluginLocale>,
}

impl PluginManifest {
    /// 解析并校验：id/名称/版本必填，id 限安全字符，权限归一化去重。
    pub fn parse(json: &str) -> Result<Self, String> {
        let mut m: Self =
            serde_json::from_str(json).map_err(|e| format!("manifest JSON 非法：{e}"))?;
        m.id = m.id.trim().to_string();
        m.name = m.name.trim().to_string();
        m.version = m.version.trim().to_string();
        if m.id.is_empty()
            || !m
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err("manifest.id 非法（仅字母数字 . - _）".into());
        }
        if m.name.is_empty() {
            return Err("manifest.name 不能为空".into());
        }
        if m.version.is_empty() {
            return Err("manifest.version 不能为空".into());
        }
        // 权限归一：未知项丢弃，去重
        let mut perms: Vec<String> = Vec::new();
        for p in &m.permissions {
            if let Some(ok) = Permission::parse(p) {
                let s = ok.as_str().to_string();
                if !perms.contains(&s) {
                    perms.push(s);
                }
            }
        }
        m.permissions = perms;
        // 命令 id 唯一
        let mut seen = std::collections::BTreeSet::new();
        for c in &mut m.commands {
            if c.id.trim().is_empty() || c.template.trim().is_empty() {
                return Err("commands[].id/template 不能为空".into());
            }
            // requires 归一：小写 trim；空按缺省 exec。非 "none" 且非已知档位视为非法，早失败防拼错。
            let mut req = c.requires.trim().to_ascii_lowercase();
            if req.is_empty() {
                req = "exec".into();
            }
            if req != "none" && crate::session::Caps::from_name(&req).is_none() {
                return Err(format!("commands[].requires 非法档位：{}", c.requires));
            }
            c.requires = req;
            if !seen.insert(c.id.clone()) {
                return Err(format!("commands[].id 重复：{}", c.id));
            }
        }
        // AI 技能归一：LLM 可见名限子串安全格式；requires/risk 白名单早失败防拼错；
        // export 缺省按约定派生；id/name/export 均验唯一（同一 wasm 导出不该挂两个名字）。
        {
            let mut seen_id = std::collections::BTreeSet::new();
            let mut seen_name = std::collections::BTreeSet::new();
            for t in &mut m.ai_tools {
                t.id = t.id.trim().to_string();
                t.name = t.name.trim().to_ascii_lowercase();
                t.export = t.export.trim().to_string();
                if t.id.is_empty() || t.name.is_empty() {
                    return Err("aiTools[].id/name 不能为空".into());
                }
                if t.name.len() > 64
                    || !t
                        .name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                {
                    return Err(format!("aiTools[].name 非法（仅小写字母数字下划线，≤ 64）：{}", t.name));
                }
                if t.requires.trim().is_empty() {
                    t.requires = "none".into();
                }
                t.requires = t.requires.trim().to_ascii_lowercase();
                if t.requires != "none" && crate::session::Caps::from_name(&t.requires).is_none() {
                    return Err(format!("aiTools[].requires 非法档位：{}", t.requires));
                }
                t.risk = t.risk.trim().to_ascii_lowercase();
                if !matches!(t.risk.as_str(), "readonly" | "confirm") {
                    return Err(format!("aiTools[].risk 非法（readonly/confirm）：{}", t.risk));
                }
                if t.export.is_empty() {
                    t.export = format!("ai_tool_{}", t.name);
                }
                if t.parameters.get("type").and_then(Value::as_str) != Some("object") {
                    return Err(format!("aiTools[].parameters 必须是 type=object 的 JSON Schema：{}", t.name));
                }
                if !seen_id.insert(t.id.clone()) {
                    return Err(format!("aiTools[].id 重复：{}", t.id));
                }
                if !seen_name.insert(t.name.clone()) {
                    return Err(format!("aiTools[].name 重复：{}", t.name));
                }
            }
            // 声明了技能就必须同时授予 `ai_tool` 权限：下发处（`tools::plugin_tools`）按权限筛，
            // 不在这里早失败，插件作者看到的就只是「技能凭空消失」的无日志哑谜。
            // 放在逐项校验之后：先报技能本身写错了，而不是权限没给。
            // 只对真声明了技能的包生效：纯命令包（无 aiTools）不需要 ai_tool 权限。
            if !m.ai_tools.is_empty() && !m.has_permission(Permission::AiTool) {
                return Err("声明了 aiTools 必须同时在 permissions 里授予 ai_tool".into());
            }
        }
        // 语料声明归一：code trim+小写且非空、非保留值 "system"，path 非空；同 code 去重（首条胜出）
        let mut locales: Vec<PluginLocale> = Vec::new();
        for mut l in m.locales {
            l.code = l.code.trim().to_ascii_lowercase();
            l.label = l.label.trim().to_string();
            l.path = l.path.trim().to_string();
            if l.code.is_empty() || l.code == "system" || l.path.is_empty() {
                return Err(
                    "locales[].code/path 非法（code 非空且不能为保留值 system，path 非空）".into(),
                );
            }
            if !locales.iter().any(|e| e.code == l.code) {
                locales.push(l);
            }
        }
        m.locales = locales;
        Ok(m)
    }

    pub fn has_permission(&self, p: Permission) -> bool {
        self.permissions.iter().any(|s| s == p.as_str())
    }

    /// 设置默认值表（key → default）。
    pub fn default_settings(&self) -> BTreeMap<String, String> {
        self.settings
            .iter()
            .map(|s| (s.key.clone(), s.default.clone()))
            .collect()
    }

    /// 渲染命令模板：`{{key}}` 替换为设置值，未知键留空。
    pub fn render_template(&self, template: &str, values: &BTreeMap<String, String>) -> String {
        let mut out = template.to_string();
        for (k, v) in values {
            out = out.replace(&format!("{{{{{k}}}}}"), v);
        }
        // 未绑定的占位清空，避免把字面量 `{{x}}` 交给 shell
        while let Some(start) = out.find("{{") {
            let Some(end) = out[start..].find("}}") else {
                out.truncate(start);
                break;
            };
            out.replace_range(start..start + end + 2, "");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"{
        "id": "com.demo.tools",
        "name": "Demo Tools",
        "version": "1.0.0",
        "permissions": ["storage", "session_exec", "log", "bogus"],
        "commands": [
            {"id": "t1", "title": "Tail", "template": "tail -n {{lines}} {{path}}"}
        ],
        "settings": [
            {"key": "lines", "type": "number", "label": "Lines", "default": "50"},
            {"key": "path", "type": "string", "label": "Path", "default": "/var/log/syslog"}
        ],
        "hooks": ["on_output"]
    }"#;

    #[test]
    fn parses_manifest_and_filters_permissions() {
        let m = PluginManifest::parse(OK).unwrap();
        assert_eq!(m.id, "com.demo.tools");
        assert!(m.has_permission(Permission::Storage));
        assert!(m.has_permission(Permission::SessionExec));
        assert!(!m.permissions.contains(&"bogus".to_string()));
        assert_eq!(m.commands.len(), 1);
    }

    #[test]
    fn rejects_bad_id() {
        assert!(PluginManifest::parse(r#"{"id":"a/b","name":"x","version":"1"}"#).is_err());
        assert!(PluginManifest::parse(r#"{"id":"","name":"x","version":"1"}"#).is_err());
    }

    /// 语料声明：code 归一小写、保留值 system 拒收、同 code 去重；缺省为空。
    #[test]
    fn locales_normalized_and_guarded() {
        let m = PluginManifest::parse(
            r#"{"id":"p","name":"x","version":"1","locales":[
                {"code":" JA ","label":"日本語","path":"locales/ja.json"},
                {"code":"ja","label":"dup","path":"locales/ja2.json"}]}"#,
        )
        .unwrap();
        assert_eq!(m.locales.len(), 1);
        assert_eq!(m.locales[0].code, "ja");
        assert_eq!(m.locales[0].label, "日本語");
        assert!(PluginManifest::parse(
            r#"{"id":"p","name":"x","version":"1","locales":[{"code":"system","path":"a.json"}]}"#
        )
        .is_err());
        assert!(PluginManifest::parse(OK).unwrap().locales.is_empty());
    }

    #[test]
    fn renders_template_placeholders() {
        let m = PluginManifest::parse(OK).unwrap();
        let mut vals = m.default_settings();
        vals.insert("path".into(), "/var/log/nginx/error.log".into());
        let cmd = m.render_template("tail -n {{lines}} {{path}} {{missing}}", &vals);
        assert_eq!(cmd, "tail -n 50 /var/log/nginx/error.log ");
    }

    /// AI 技能声明（测试）：合法项归一 + 非法项早失败。
    #[test]
    fn ai_tools_normalized_and_guarded() {
        let m = PluginManifest::parse(
            r#"{"id":"p","name":"x","version":"1","permissions":["ai_tool"],"wasm":"a.wasm","aiTools":[
                {"id":"cmdb","name":"CMDB_Lookup","description":"d","risk":"confirm",
                 "parameters":{"type":"object","properties":{}}}]}"#,
        )
        .unwrap();
        assert_eq!(m.ai_tools.len(), 1);
        let t = &m.ai_tools[0];
        assert_eq!(t.name, "cmdb_lookup");
        assert_eq!(t.export, "ai_tool_cmdb_lookup");
        assert_eq!(t.requires, "none");
        assert!(m.has_permission(Permission::AiTool));

        // 非法 name（含连字符）
        assert!(PluginManifest::parse(
            r#"{"id":"p","name":"x","version":"1","aiTools":[{"id":"a","name":"bad-name","parameters":{"type":"object"}}]}"#
        )
        .is_err());
        // 非法 risk
        assert!(PluginManifest::parse(
            r#"{"id":"p","name":"x","version":"1","aiTools":[{"id":"a","name":"ok","risk":"yolo","parameters":{"type":"object"}}]}"#
        )
        .is_err());
        // 非法 requires 档位
        assert!(PluginManifest::parse(
            r#"{"id":"p","name":"x","version":"1","aiTools":[{"id":"a","name":"ok","requires":"teleport","parameters":{"type":"object"}}]}"#
        )
        .is_err());
        // parameters 非 object 形状
        assert!(PluginManifest::parse(
            r#"{"id":"p","name":"x","version":"1","aiTools":[{"id":"a","name":"ok","parameters":{"type":"array"}}]}"#
        )
        .is_err());
        // name 重复
        assert!(PluginManifest::parse(
            r#"{"id":"p","name":"x","version":"1","aiTools":[
                {"id":"a","name":"ok","parameters":{"type":"object"}},
                {"id":"b","name":"ok","parameters":{"type":"object"}}]}"#
        )
        .is_err());
        // 缺省：无 aiTools 时为空表
        assert!(PluginManifest::parse(OK).unwrap().ai_tools.is_empty());
        // 声明了技能却没授予 ai_tool：安装时就早失败，不能静默不下发
        assert!(PluginManifest::parse(
            r#"{"id":"p","name":"x","version":"1","aiTools":[{"id":"a","name":"ok","parameters":{"type":"object"}}]}"#
        )
        .is_err());
    }
}
