//! 插件 IPC：列表 / 安装 / 卸载 / 配置读写 / 命令执行 / 输出钩子。
//!
//! **凭证不进插件**：会话命令走 exec + PolicyEngine 只读裁决；配置按 id 隔离。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use super::{blocking, state};
use crate::error::AppError;
use crate::plugin::host;
use crate::plugin::registry::{command_line, InstalledPlugin};
use crate::plugin::{Permission, PluginSettingSpec};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSettingsView {
    /// key → 当前生效值（默认 + 覆盖）。
    pub values: BTreeMap<String, String>,
    /// schema（前端动态渲染）。
    pub specs: Vec<PluginSettingSpec>,
}

/// 列出已安装插件。
#[tauri::command]
pub async fn plugin_list(app: AppHandle) -> Result<Vec<InstalledPlugin>, AppError> {
    blocking(move || {
        let st = state(&app);
        Ok(st.plugins.list())
    })
    .await
}

/// 重扫插件目录。
#[tauri::command]
pub async fn plugin_reload(app: AppHandle) -> Result<usize, AppError> {
    blocking(move || state(&app).plugins.reload()).await
}

/// 读插件贡献的语料包（manifest.locales 声明的 code → 平面「键 → 文案」字典）。
/// 前端启动时拉取并注册进 i18n，使插件语言与内置 zh/en 同权；路径限插件目录内（后端校验）。
#[tauri::command]
pub async fn plugin_locale_get(
    app: AppHandle,
    plugin_id: String,
    code: String,
) -> Result<BTreeMap<String, String>, AppError> {
    blocking(move || {
        let st = state(&app);
        let raw = st.plugins.locale_json(&plugin_id, &code)?;
        serde_json::from_str(&raw)
            .map_err(|e| AppError::Invalid(format!("语料 JSON 解析失败：{e}")))
    })
    .await
}

/// 从本地目录安装（含 `manifest.json` 与可选 wasm）。
#[tauri::command]
pub async fn plugin_install(app: AppHandle, path: String) -> Result<InstalledPlugin, AppError> {
    blocking(move || {
        let st = state(&app);
        st.plugins.install_from_dir(&PathBuf::from(path))
    })
    .await
}

/// 弹原生目录选择框挑插件目录（含 manifest.json 的那层）。
/// 回调式非阻塞 + `async` 命令（同 dialog.rs）：不卡主线程消息循环，避免整窗“无响应”
/// 与目录框内就地重命名卡住；用 `AppHandle` 开框保持非模态。取消返回 None。
#[tauri::command]
pub async fn plugin_pick_dir(app: AppHandle, title: Option<String>) -> Result<Option<String>, AppError> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    // 标题由前端按界面语言经 i18n 字典下发；未传时回落英文（开源默认语），不写死具体语言。
    app.dialog()
        .file()
        .set_title(title.unwrap_or_else(|| "Select plugin folder (with manifest.json)".into()))
        .pick_folder(move |p| {
            let _ = tx.send(p.map(|p| p.simplified().to_string()));
        });
    rx.await
        .map_err(|_| AppError::Internal("无法打开目录选择框".into()))
}

/// 卸载：删目录 + 清隔离配置。
#[tauri::command]
pub async fn plugin_uninstall(app: AppHandle, plugin_id: String) -> Result<bool, AppError> {
    blocking(move || {
        let st = state(&app);
        // 借 registry 需要 &self，state 是 State——先 clone 路径逻辑直接调
        let reg = &st.plugins;
        reg.uninstall(&app, &plugin_id)
    })
    .await
}

/// 读插件生效配置 + schema。
#[tauri::command]
pub async fn plugin_settings_get(
    app: AppHandle,
    plugin_id: String,
) -> Result<PluginSettingsView, AppError> {
    blocking(move || {
        let st = state(&app);
        let values = st.plugins.effective_settings(&app, &plugin_id)?;
        let specs = st
            .plugins
            .get(&plugin_id)
            .map(|p| p.manifest.settings)
            .unwrap_or_default();
        Ok(PluginSettingsView { values, specs })
    })
    .await
}

/// 写一条配置（命名空间 = plugin_id）。
#[tauri::command]
pub async fn plugin_setting_set(
    app: AppHandle,
    plugin_id: String,
    key: String,
    value: String,
) -> Result<(), AppError> {
    blocking(move || {
        let st = state(&app);
        st.db.set_plugin_setting(&plugin_id, &key, &value)
    })
    .await
}

/// 执行命令包里的一条命令（模板渲染后在会话上只读 exec）。
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginRunArgs {
    pub plugin_id: String,
    pub command_id: String,
    pub session_id: String,
    #[serde(default)]
    pub arg: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRunResult {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[tauri::command]
pub async fn plugin_run_command(
    app: AppHandle,
    args: PluginRunArgs,
) -> Result<PluginRunResult, AppError> {
    let (plugin, cmd, stored) = blocking({
        let app = app.clone();
        let a = args.clone();
        move || {
            let st = state(&app);
            let plugin = st
                .plugins
                .get(&a.plugin_id)
                .ok_or_else(|| AppError::Invalid("插件不存在".into()))?;
            let cmd = plugin
                .manifest
                .commands
                .iter()
                .find(|c| c.id == a.command_id)
                .cloned()
                .ok_or_else(|| AppError::Invalid("命令不存在".into()))?;
            if !plugin.manifest.has_permission(Permission::SessionExec) {
                // 权限拒绝不是「参数不合法」：走 Conflict 免前缀，文案即所见
                return Err(AppError::Conflict(
                    "插件未声明 session_exec 权限，不能执行命令".into(),
                ));
            }
            let stored = st.plugins.effective_settings(&app, &a.plugin_id)?;
            Ok((plugin, cmd, stored))
        }
    })
    .await?;

    let line = command_line(&plugin, &cmd, &stored, &args.arg);
    // 只读执行：经 AI exec（独立通道），再过 PolicyEngine 裁决——插件不得写系统
    use crate::ai::policy::{Decision, PolicyEngine, RiskLevel};
    let verdict = PolicyEngine::default().classify(&line);
    if verdict.risk != RiskLevel::ReadOnly || verdict.decision == Decision::Blocked {
        return Err(AppError::Invalid(
            "插件命令被策略拦下（仅允许只读命令）".into(),
        ));
    }
    // 能力门控：命令声明所需档位（缺省 exec）。会话没有这条通路（FTP/FTPS 无 exec、
    // 本地文件栏无关联会话）时给清晰引导，而非把底层 exec 错误抛给用户。前端已按同一张
    // 能力表隐藏入口，这里是防绕过 UI 直接调 IPC 的后端兵家必争。
    if cmd.requires != "none" {
        use crate::session::Caps;
        let have = state(&app)
            .sessions
            .kind(&args.session_id)
            .map(|k| k.caps())
            .unwrap_or(Caps::NONE);
        let ok = Caps::from_name(&cmd.requires)
            .map(|c| have.contains(c))
            .unwrap_or(false);
        if !ok {
            return Err(AppError::Ai(format!(
                "该插件命令需要「{}」能力，当前会话不支持",
                cmd.requires
            )));
        }
    }
    let r = crate::ai::exec::exec_command(&app, &args.session_id, &line, 30_000).await?;
    Ok(PluginRunResult {
        command: line,
        stdout: r.stdout,
        stderr: r.stderr,
        exit_code: r.exit_code,
    })
}

/// 对一段输出跑全部启用插件的 `on_output` 钩子（有 wasm 的才跑）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginHookArgs {
    pub session_id: String,
    pub output: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginHookResult {
    pub values: Vec<String>,
    pub notices: Vec<String>,
    pub logs: Vec<String>,
    /// 插件请求写入会话终端的字节（宿主层负责注入）。
    pub session_out: Vec<u8>,
    /// 字节桥通道名 → 数据（X11/自定义协议扩展点）。
    pub bridge_out: std::collections::BTreeMap<String, Vec<u8>>,
}

#[tauri::command]
pub async fn plugin_on_output(
    app: AppHandle,
    args: PluginHookArgs,
) -> Result<PluginHookResult, AppError> {
    blocking(move || {
        let st = state(&app);
        let mut values = Vec::new();
        let mut notices = Vec::new();
        let mut logs = Vec::new();
        let mut session_out = Vec::new();
        let mut bridge_out: std::collections::BTreeMap<String, Vec<u8>> = Default::default();
        for p in st.plugins.list() {
            if !p.has_wasm || !p.manifest.hooks.iter().any(|h| h == "on_output") {
                continue;
            }
            let dir = st.plugins.plugin_dir(&p.manifest.id);
            let wasm_path = dir.join(p.manifest.wasm.as_deref().unwrap_or("plugin.wasm"));
            let Ok(bytes) = std::fs::read(&wasm_path) else {
                continue;
            };
            let Ok(settings) = st.plugins.effective_settings(&app, &p.manifest.id) else {
                continue;
            };
            if let Ok(out) = host::on_output(
                &p.manifest,
                &bytes,
                &args.session_id,
                &args.output,
                settings,
            ) {
                if !out.value.is_empty() {
                    values.push(out.value);
                }
                notices.extend(out.notices);
                logs.extend(out.logs);
                session_out.extend_from_slice(&out.session_out);
                for (name, data) in out.bridge_out {
                    bridge_out.entry(name).or_default().extend_from_slice(&data);
                }
                for (k, v) in out.dirty_settings {
                    let _ = st.db.set_plugin_setting(&p.manifest.id, &k, &v);
                }
            }
        }
        Ok(PluginHookResult {
            values,
            notices,
            logs,
            session_out,
            bridge_out,
        })
    })
    .await
}
