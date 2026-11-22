//! WASM 沙箱宿主（能力网关）：wasmi 解释器 + fuel 限额；导出钩子按 manifest 调用。
//!
//! 插件 ABI（i32 指针/长度）：
//! - 导入 `env`：`log(ptr,len)` · `notify(ptr,len)` · `get_setting(kptr,klen)->i64` ·
//!   `set_setting(kptr,klen,vptr,vlen)`
//! - 导出：`plugin_alloc(len)->i32` · `memory` ·
//!   `on_output(sid_ptr,sid_len,data_ptr,data_len)->i64`（高 32 ptr / 低 32 len）
//!
//! 仅 storage/notify/log 在沙箱内；`session_exec` 由 Rust 命令层经 PolicyEngine 执行。

use std::collections::BTreeMap;

use wasmi::{Caller, Engine, Linker, Module, Store};

use super::manifest::{Permission, PluginManifest};
use crate::error::AppError;

const FUEL: u64 = 20_000_000;
const MAX_STR: usize = 64 * 1024;

struct HostState {
    permissions: Vec<String>,
    settings: BTreeMap<String, String>,
    logs: Vec<String>,
    notices: Vec<String>,
    dirty: Vec<(String, String)>,
    /// session_write：要注入会话终端的字节（宿主层写入 PTY）。
    session_out: Vec<u8>,
    /// bridge：命名通道的待读缓冲。
    bridges: BTreeMap<String, Vec<u8>>,
}

/// 钩子一次执行的产物。
pub struct HookOutput {
    pub value: String,
    pub logs: Vec<String>,
    pub notices: Vec<String>,
    pub dirty_settings: Vec<(String, String)>,
    /// 要写入会话终端的字节。
    pub session_out: Vec<u8>,
    /// bridge 名 → 本钩子写入的数据（宿主可路由到 X11/自定义协议）。
    pub bridge_out: BTreeMap<String, Vec<u8>>,
}

/// 加载 wasm 并调用导出 `export`（两个字符串入参，返回字符串）。
pub fn call_hook(
    manifest: &PluginManifest,
    wasm_bytes: &[u8],
    export: &str,
    arg0: &str,
    arg1: &str,
    settings: BTreeMap<String, String>,
) -> Result<HookOutput, AppError> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm_bytes)
        .map_err(|e| AppError::Invalid(format!("插件 wasm 编译失败：{e}")))?;

    let mut store = Store::new(
        &engine,
        HostState {
            permissions: manifest.permissions.clone(),
            settings,
            logs: Vec::new(),
            notices: Vec::new(),
            dirty: Vec::new(),
            session_out: Vec::new(),
            bridges: BTreeMap::new(),
        },
    );
    store
        .set_fuel(FUEL)
        .map_err(|e| AppError::Internal(format!("fuel：{e}")))?;

    let mut linker: Linker<HostState> = Linker::new(&engine);
    define_imports(&mut linker)?;

    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| AppError::Invalid(format!("插件实例化失败：{e}")))?
        .start(&mut store)
        .map_err(|e| AppError::Invalid(format!("插件 _start 失败：{e}")))?;

    let memory = instance
        .get_export(&store, "memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| AppError::Invalid("插件未导出 memory".to_string()))?;
    let alloc = instance
        .get_func(&store, "plugin_alloc")
        .ok_or_else(|| AppError::Invalid("插件未导出 plugin_alloc".to_string()))?
        .typed::<i32, i32>(&store)
        .map_err(|e| AppError::Invalid(format!("plugin_alloc 签名：{e}")))?;

    let func = instance
        .get_func(&store, export)
        .ok_or_else(|| AppError::Invalid(format!("插件未导出 {export}")))?;
    let typed = func
        .typed::<(i32, i32, i32, i32), i64>(&store)
        .map_err(|e| AppError::Invalid(format!("{export} 签名须 (i32,i32,i32,i32)->i64：{e}")))?;

    let (p0, l0) = put_str(&mut store, &memory, &alloc, arg0)?;
    let (p1, l1) = put_str(&mut store, &memory, &alloc, arg1)?;
    let packed = typed
        .call(&mut store, (p0, l0, p1, l1))
        .map_err(|e| AppError::Invalid(format!("{export} 执行失败：{e}")))?;

    let value = if packed == 0 {
        String::new()
    } else {
        get_str(
            &mut store,
            &memory,
            (packed >> 32) as i32,
            (packed & 0xffff_ffff) as i32,
        )
        .unwrap_or_default()
    };

    let HostState {
        logs,
        notices,
        dirty,
        session_out,
        bridges,
        ..
    } = store.into_data();
    let bridge_out = bridges.into_iter().filter(|(_, v)| !v.is_empty()).collect();
    Ok(HookOutput {
        value,
        logs,
        notices,
        dirty_settings: dirty,
        session_out,
        bridge_out,
    })
}

/// 便捷：终端输出钩子。
pub fn on_output(
    manifest: &PluginManifest,
    wasm: &[u8],
    session_id: &str,
    output: &str,
    settings: BTreeMap<String, String>,
) -> Result<HookOutput, AppError> {
    call_hook(manifest, wasm, "on_output", session_id, output, settings)
}

/// AI 技能调用（插件贡献的 `aiTools` 条目）：走与钩子同一条 wasm ABI，入参给导出
/// `(tool_name_json, args_json)` 两段——单 wasm 多技能时导出内部靠首参分派，不必编译多个入口。
///
/// 返回导出回传的字符串（约定为 `{"ok":true,"text":...,"data":...}` 或 `{"error":...}` 的 JSON，
/// 解释与兜底在调用方 `PluginTool`，宿主不担模型协议形状）。`ai_tool` 权限在此硬校：
/// 注册表下发处已筛过一道，这是防「绕 UI 直调」的兵家必争之地（同 `plugin_run_command` 口径）。
pub fn call_ai_tool(
    manifest: &PluginManifest,
    wasm: &[u8],
    export: &str,
    tool_name: &str,
    args_json: &str,
    settings: BTreeMap<String, String>,
) -> Result<String, AppError> {
    if !manifest.has_permission(Permission::AiTool) {
        return Err(AppError::Conflict(
            "插件未声明 ai_tool 权限，不能响应 AI 技能调用".into(),
        ));
    }
    let out = call_hook(manifest, wasm, export, tool_name, args_json, settings)?;
    Ok(out.value)
}

fn define_imports(linker: &mut Linker<HostState>) -> Result<(), AppError> {
    linker
        .func_wrap(
            "env",
            "log",
            |mut c: Caller<'_, HostState>, ptr: i32, len: i32| {
                if !has(&c, Permission::Log) {
                    return;
                }
                if let Some(s) = mem_str(&mut c, ptr, len) {
                    c.data_mut().logs.push(s);
                }
            },
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;
    linker
        .func_wrap(
            "env",
            "notify",
            |mut c: Caller<'_, HostState>, ptr: i32, len: i32| {
                if !has(&c, Permission::Notify) {
                    return;
                }
                if let Some(s) = mem_str(&mut c, ptr, len) {
                    c.data_mut().notices.push(s);
                }
            },
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;
    linker
        .func_wrap(
            "env",
            "get_setting",
            |mut c: Caller<'_, HostState>, kptr: i32, klen: i32| -> i64 {
                if !has(&c, Permission::Storage) {
                    return 0;
                }
                let Some(key) = mem_str(&mut c, kptr, klen) else {
                    return 0;
                };
                let val = c.data().settings.get(&key).cloned().unwrap_or_default();
                // 调用插件 plugin_alloc 造缓冲，写入值，打包 (ptr<<32)|len
                let Some(func) = c.get_export("plugin_alloc").and_then(|e| e.into_func()) else {
                    return 0;
                };
                let Ok(typed) = func.typed::<i32, i32>(&c) else {
                    return 0;
                };
                let bytes = val.as_bytes();
                let Ok(ptr) = typed.call(&mut c, bytes.len() as i32) else {
                    return 0;
                };
                if ptr < 0 || bytes.is_empty() {
                    return 0;
                }
                let Some(mem) = c.get_export("memory").and_then(|e| e.into_memory()) else {
                    return 0;
                };
                if mem.write(&mut c, ptr as usize, bytes).is_err() {
                    return 0;
                }
                ((ptr as i64) << 32) | bytes.len() as i64
            },
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;
    // set_setting 保持可用（存储）
    linker
        .func_wrap(
            "env",
            "set_setting",
            |mut c: Caller<'_, HostState>, kptr: i32, klen: i32, vptr: i32, vlen: i32| {
                if !has(&c, Permission::Storage) {
                    return;
                }
                if let (Some(k), Some(v)) =
                    (mem_str(&mut c, kptr, klen), mem_str(&mut c, vptr, vlen))
                {
                    let st = c.data_mut();
                    st.settings.insert(k.clone(), v.clone());
                    st.dirty.push((k, v));
                }
            },
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;
    // ── 扩展能力（X11/自定义协议前置）──
    linker
        .func_wrap(
            "env",
            "session_write",
            |mut c: Caller<'_, HostState>, ptr: i32, len: i32| {
                if !has(&c, Permission::SessionWrite) {
                    return;
                }
                if let Some(s) = mem_str(&mut c, ptr, len) {
                    c.data_mut().session_out.extend_from_slice(s.as_bytes());
                }
            },
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;
    // http_get：同步短超时 GET，返回 (ptr<<32)|len；失败返回 0
    linker
        .func_wrap(
            "env",
            "http_get",
            |mut c: Caller<'_, HostState>, ptr: i32, len: i32| -> i64 {
                if !has(&c, Permission::Network) {
                    return 0;
                }
                let Some(url) = mem_str(&mut c, ptr, len) else {
                    return 0;
                };
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return 0;
                }
                let body = match http_get_sync(&url) {
                    Ok(b) => b,
                    Err(_) => return 0,
                };
                let Some(func) = c.get_export("plugin_alloc").and_then(|e| e.into_func()) else {
                    return 0;
                };
                let Ok(typed) = func.typed::<i32, i32>(&c) else {
                    return 0;
                };
                let Ok(p) = typed.call(&mut c, body.len() as i32) else {
                    return 0;
                };
                if p < 0 || body.is_empty() {
                    return 0;
                }
                let Some(mem) = c.get_export("memory").and_then(|e| e.into_memory()) else {
                    return 0;
                };
                if mem.write(&mut c, p as usize, &body).is_err() {
                    return 0;
                }
                ((p as i64) << 32) | body.len() as i64
            },
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;
    // bridge_send：往命名通道追加数据（宿主可接到 X11 / 自定义字节流）
    linker
        .func_wrap(
            "env",
            "bridge_send",
            |mut c: Caller<'_, HostState>, nptr: i32, nlen: i32, dptr: i32, dlen: i32| {
                if !has(&c, Permission::Bridge) {
                    return;
                }
                let (Some(name), Some(data)) =
                    (mem_str(&mut c, nptr, nlen), mem_str(&mut c, dptr, dlen))
                else {
                    return;
                };
                if name.len() > 64 {
                    return;
                }
                c.data_mut()
                    .bridges
                    .entry(name)
                    .or_default()
                    .extend_from_slice(data.as_bytes());
            },
        )
        .map_err(|e| AppError::Internal(e.to_string()))?;
    Ok(())
}

/// 同步 HTTP GET（插件钩子内）：独立 runtime + 5s 超时 + 1MB 上限。
fn http_get_sync(url: &str) -> Result<Vec<u8>, String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.get(url).send().await.map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
        Ok(bytes.iter().take(1024 * 1024).copied().collect())
    })
}

fn has(c: &Caller<'_, HostState>, p: Permission) -> bool {
    c.data().permissions.iter().any(|s| s == p.as_str())
}

fn mem_str(c: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> Option<String> {
    if ptr < 0 || len < 0 || len as usize > MAX_STR {
        return None;
    }
    let mem = c.get_export("memory")?.into_memory()?;
    let mut buf = vec![0u8; len as usize];
    mem.read(&mut *c, ptr as usize, &mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

fn put_str(
    store: &mut Store<HostState>,
    memory: &wasmi::Memory,
    alloc: &wasmi::TypedFunc<i32, i32>,
    s: &str,
) -> Result<(i32, i32), AppError> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Ok((0, 0));
    }
    let ptr = alloc
        .call(&mut *store, bytes.len() as i32)
        .map_err(|e| AppError::Invalid(format!("plugin_alloc：{e}")))?;
    if ptr < 0 {
        return Ok((0, 0));
    }
    memory
        .write(&mut *store, ptr as usize, bytes)
        .map_err(|e| AppError::Invalid(format!("写入插件内存失败：{e}")))?;
    Ok((ptr, bytes.len() as i32))
}

fn get_str(
    store: &mut Store<HostState>,
    memory: &wasmi::Memory,
    ptr: i32,
    len: i32,
) -> Option<String> {
    if ptr < 0 || len < 0 || len as usize > MAX_STR {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    memory.read(&mut *store, ptr as usize, &mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_wasm_fails_closed() {
        let m = PluginManifest::parse(r#"{"id":"x","name":"x","version":"1"}"#).unwrap();
        assert!(call_hook(&m, b"\0asm garbage", "on_output", "a", "b", BTreeMap::new()).is_err());
    }
}
