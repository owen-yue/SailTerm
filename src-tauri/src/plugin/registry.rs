//! 插件注册表：从应用数据目录 `plugins/<id>/` 加载 manifest + 可选 wasm。
//!
//! 安装 = 把 `manifest.json`（+ `plugin.wasm`）放进目录；卸载 = 删目录 + 清隔离配置。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use super::manifest::{PluginCommand, PluginManifest};
use crate::error::AppError;
use crate::state::AppState;

/// 插件根目录名（相对 app data dir）。
const ROOT: &str = "plugins";

/// 已安装插件（回前端）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    /// 是否含 wasm 逻辑。
    pub has_wasm: bool,
    /// 是否启用（禁用后不挂钩子、不列命令）。
    pub enabled: bool,
    pub path: String,
}

/// 注册表：进程内缓存 + 磁盘真相源。
pub struct PluginRegistry {
    dir: PathBuf,
    inner: Mutex<Vec<InstalledPlugin>>,
}

impl PluginRegistry {
    pub fn open(data_dir: &Path) -> Result<Self, AppError> {
        let dir = data_dir.join(ROOT);
        std::fs::create_dir_all(&dir)?;
        let reg = Self {
            dir,
            inner: Mutex::new(Vec::new()),
        };
        reg.reload()?;
        Ok(reg)
    }

    /// 重扫磁盘。
    pub fn reload(&self) -> Result<usize, AppError> {
        let mut list = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                if !e.path().is_dir() {
                    continue;
                }
                if let Ok(p) = self.load_one(&e.path()) {
                    list.push(p);
                }
            }
        }
        list.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        let n = list.len();
        *self.inner.lock().unwrap() = list;
        Ok(n)
    }

    fn load_one(&self, path: &Path) -> Result<InstalledPlugin, AppError> {
        let manifest_path = path.join("manifest.json");
        let raw = std::fs::read_to_string(&manifest_path)
            .map_err(|e| AppError::Io(format!("读取 {} 失败：{e}", manifest_path.display())))?;
        let manifest = PluginManifest::parse(&raw)
            .map_err(|e| AppError::Invalid(format!("{}：{e}", manifest_path.display())))?;
        let wasm = manifest
            .wasm
            .as_deref()
            .map(|w| path.join(w))
            .filter(|p| p.is_file());
        Ok(InstalledPlugin {
            has_wasm: wasm.is_some(),
            enabled: true,
            path: path.display().to_string(),
            manifest,
        })
    }

    pub fn list(&self) -> Vec<InstalledPlugin> {
        self.inner.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<InstalledPlugin> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.manifest.id == id)
            .cloned()
    }

    /// 插件目录（安装/卸载用）。
    pub fn plugin_dir(&self, id: &str) -> PathBuf {
        self.dir.join(sanitize_id(id))
    }

    /// 从目录安装（复制 manifest + wasm 进插件库）。
    pub fn install_from_dir(&self, src: &Path) -> Result<InstalledPlugin, AppError> {
        let src_manifest = src.join("manifest.json");
        let raw = std::fs::read_to_string(&src_manifest)
            .map_err(|e| AppError::Io(format!("读取 {} 失败：{e}", src_manifest.display())))?;
        let manifest = PluginManifest::parse(&raw).map_err(AppError::Invalid)?;
        let dest = self.plugin_dir(&manifest.id);
        std::fs::create_dir_all(&dest)?;
        std::fs::copy(&src_manifest, dest.join("manifest.json"))?;
        if let Some(w) = &manifest.wasm {
            let from = src.join(w);
            if from.is_file() {
                std::fs::copy(&from, dest.join(w))?;
            }
        }
        // 语料包随插件一起入库（缺失只跳过，不影响命令包能力）
        for l in &manifest.locales {
            let from = src.join(&l.path);
            if from.is_file() {
                let to = dest.join(&l.path);
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&from, to)?;
            }
        }
        self.reload()?;
        self.get(&manifest.id)
            .ok_or_else(|| AppError::Internal("安装后未能载入插件".into()))
    }

    /// 卸载：删目录 + 清配置；返回是否删到。
    pub fn uninstall(&self, app: &AppHandle, id: &str) -> Result<bool, AppError> {
        let dir = self.plugin_dir(id);
        let existed = dir.is_dir();
        if existed {
            std::fs::remove_dir_all(&dir)?;
        }
        // 清隔离配置
        app.state::<AppState>().db.clear_plugin_settings(id)?;
        self.reload()?;
        Ok(existed)
    }

    /// 取某插件生效配置（manifest 默认 + 库内覆盖）。
    pub fn effective_settings(
        &self,
        app: &AppHandle,
        plugin_id: &str,
    ) -> Result<BTreeMap<String, String>, AppError> {
        let mut vals = self
            .get(plugin_id)
            .map(|p| p.manifest.default_settings())
            .unwrap_or_default();
        let st = app.state::<AppState>();
        for s in st.db.plugin_settings(plugin_id)? {
            vals.insert(s.key, s.value);
        }
        Ok(vals)
    }

    /// 读插件贡献的语料包 JSON（manifest.locales 声明的 code → 平面字典）。
    ///
    /// 安全约束：路径取自 manifest 声明（parse 已归一），再拒收含 `..`/绝对路径的组件防目录穿越；
    /// 文件大小封顶 1MB，只接受「字符串 → 字符串」的平面对象，坏文件报 E_INVALID。
    pub fn locale_json(&self, plugin_id: &str, code: &str) -> Result<String, AppError> {
        let plugin = self
            .get(plugin_id)
            .ok_or_else(|| AppError::Invalid(format!("插件不存在：{plugin_id}")))?;
        let code = code.trim().to_ascii_lowercase();
        let decl = plugin
            .manifest
            .locales
            .iter()
            .find(|l| l.code == code)
            .ok_or_else(|| AppError::Invalid(format!("插件未声明语料：{code}")))?;
        if Path::new(&decl.path).is_absolute()
            || Path::new(&decl.path)
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(AppError::Invalid(format!(
                "语料路径非法（限插件目录内相对路径）：{}",
                decl.path
            )));
        }
        const MAX_LOCALE_BYTES: u64 = 1024 * 1024;
        let path = self.plugin_dir(plugin_id).join(&decl.path);
        let meta = std::fs::metadata(&path)
            .map_err(|e| AppError::Io(format!("读取 {} 失败：{e}", path.display())))?;
        if meta.len() > MAX_LOCALE_BYTES {
            return Err(AppError::Invalid("语料文件超过 1MB 上限".into()));
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| AppError::Io(format!("读取 {} 失败：{e}", path.display())))?;
        // 结构校验：顶层对象且值全为字符串（前端拿到即可直接入注册表）
        let v: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| AppError::Invalid(format!("语料 JSON 解析失败：{e}")))?;
        let obj = v
            .as_object()
            .ok_or_else(|| AppError::Invalid("语料 JSON 顶层必须是对象".into()))?;
        if !obj.values().all(|x| x.is_string()) {
            return Err(AppError::Invalid("语料字典的值必须全为字符串".into()));
        }
        Ok(raw)
    }
}

/// 命令包渲染：合并默认与已存设置。
pub fn command_line(
    plugin: &InstalledPlugin,
    cmd: &PluginCommand,
    stored: &BTreeMap<String, String>,
    arg: &str,
) -> String {
    let mut vals = plugin.manifest.default_settings();
    for (k, v) in stored {
        vals.insert(k.clone(), v.clone());
    }
    vals.insert("arg".into(), arg.to_string());
    plugin.manifest.render_template(&cmd.template, &vals)
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
