//! 应用级命令：启动信息与本地存储状态。

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use super::{blocking, state};
use crate::ai::policy::{ApprovalMode, ScanLevel};
use crate::ai::providers::ProviderKind;
use crate::db::ai::SanitizeLevel;
use crate::db::audit::AuditDecision;
use crate::error::AppError;

/// 「关于 / 安全状态」展示用；**不含任何凭证**。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    /// 当前生效的凭证后端：`os-keyring` 或 `aes-gcm-file`。
    pub vault_backend: &'static str,
    pub schema_version: i64,
    /// 数据目录（数据库与保险库回退文件所在处）。
    pub data_dir: String,
    /// 已打开/断线中的会话数。
    pub session_count: usize,
}

#[tauri::command]
pub async fn app_info(app: AppHandle) -> Result<AppInfo, AppError> {
    blocking(move || {
        let st = state(&app);
        Ok(AppInfo {
            version: app.package_info().version.to_string(),
            vault_backend: st.vault.backend(),
            schema_version: st.db.schema_version()?,
            data_dir: st.data_dir.display().to_string(),
            session_count: st.sessions.list().len(),
        })
    })
    .await
}

/// 数据目录即时迁移的进度事件载荷（前端进度条据此渲染）。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveProgress {
    /// `files`=拷贝文件，`database`=导出主库快照，`done`=完成。
    pub phase: &'static str,
    pub done: u64,
    pub total: u64,
    /// 0-100 整体百分比（files 段占 0–90，database 段 90–100）。
    pub percent: u8,
    /// 当前正在处理的相对文件名（供 UI 展示）。
    pub current: String,
}

/// 迁移目标目录的拒绝原因码（前端据此映射成双语轻提示，走 toast 而非重量级失败弹框）。
/// `Ok(())` 表示目标可用。只做只读判定，不创建目录。
fn validate_move_target(
    current: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), &'static str> {
    if target.as_os_str().is_empty() {
        return Err("empty");
    }
    // 以规范化路径比较，拒绝「同一目录」与「相互嵌套」（否则自拷贝/递归）
    let cur = current
        .canonicalize()
        .unwrap_or_else(|_| current.to_path_buf());
    let tgt = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    if tgt == cur {
        return Err("same");
    }
    if tgt.starts_with(&cur) || cur.starts_with(&tgt) {
        return Err("nested");
    }
    // 目标已是一个 SailTerm 数据目录则拒绝，避免误并入另一份数据
    if target.join("sailterm.db").exists() {
        return Err("hasData");
    }
    Ok(())
}

/// 原因码 → 中文兜底文案。仅用于 `data_dir_move` 内部的二次防御；正常路径前端已预检并给双语提示。
fn move_target_reason_msg(reason: &'static str) -> String {
    match reason {
        "same" => "目标目录与当前数据目录相同".into(),
        "nested" => "目标目录不能与当前数据目录相互嵌套".into(),
        "hasData" => "目标目录已存在一个 SailTerm 数据目录，请改选空目录".into(),
        _ => "未选择目标目录".into(),
    }
}

/// 迁移目标目录**预检**：只读判定，不创建目录、不搬运。
///
/// 返回 `None`=可用；`Some(reason)`=拒绝原因码（`same`/`nested`/`hasData`/`empty`）。
/// 前端在选择目录后立即调用，用轻量 toast 提示，避免把校验失败塞进重量级进度弹框。
#[tauri::command]
pub async fn data_dir_check(
    app: AppHandle,
    target: String,
) -> Result<Option<&'static str>, AppError> {
    blocking(move || {
        let current = state(&app).data_dir.clone();
        let target_path = PathBuf::from(target.trim());
        Ok(validate_move_target(&current, &target_path).err())
    })
    .await
}

/// 数据目录**即时迁移**：把当前数据目录内容实时拷到 `target`，全程发 `data-dir-progress` 事件。
///
/// 主库用 `VACUUM INTO` 生成一致快照（运行期持锁也安全），其余文件逐项拷贝（`uteke-mem.db`
/// 等按原文件连同 `-wal/-shm` 一并拷，下次启动由 SQLite 自行恢复）。拷完把引导指针的
/// `active` 改指 `target`——**下次启动**即从新目录读取（当前运行实例仍持旧目录，故需重启）。
/// 原目录不删除。凭证若在 OS 钥匙串则不随目录迁移。
#[tauri::command]
pub async fn data_dir_move(app: AppHandle, target: String) -> Result<(), AppError> {
    use tauri::Emitter;
    const EVENT: &str = "data-dir-progress";
    blocking(move || {
        let default_dir = app
            .path()
            .app_data_dir()
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let current = state(&app).data_dir.clone();
        let target_path = PathBuf::from(target.trim());
        // 前端选目录后已预检；此处二次防御（正常不触发），拒绝原因走轻量兜底文案。
        if let Err(reason) = validate_move_target(&current, &target_path) {
            return Err(AppError::Invalid(move_target_reason_msg(reason)));
        }
        // 通过预检后确保目标存在（一般已由选择器创建，这里兜底）
        std::fs::create_dir_all(&target_path)?;

        let emit = |phase: &'static str, done: u64, total: u64, percent: u8, current: String| {
            let _ = app.emit(
                EVENT,
                MoveProgress {
                    phase,
                    done,
                    total,
                    percent,
                    current,
                },
            );
        };

        // 1) 收集待拷文件（跳过引导指针与主库三件套——主库改由 VACUUM INTO 快照）。
        let mut rels: Vec<PathBuf> = Vec::new();
        crate::data_location::collect_files(&current, &current, &mut rels)?;
        rels.retain(|rel| {
            let top_level = rel.components().count() == 1;
            let name = rel.file_name().and_then(|s| s.to_str()).unwrap_or("");
            !(top_level
                && (name == crate::data_location::BOOTSTRAP_FILE
                    || crate::data_location::DB_FILES.contains(&name)))
        });
        let total = rels.len() as u64;
        emit("files", 0, total, 0, String::new());

        // 2) 逐项拷贝，按整数百分比变化节流（最多约 90 次事件）。
        let mut done: u64 = 0;
        let mut last_pct: i64 = -1;
        for rel in &rels {
            let from = current.join(rel);
            let to = target_path.join(rel);
            if let Some(p) = to.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(&from, &to)
                .map_err(|e| AppError::Io(format!("拷贝 {} 失败：{e}", rel.display())))?;
            done += 1;
            let pct = (done * 90) / total.max(1);
            if pct != last_pct as u64 || done == total {
                last_pct = pct as i64;
                emit(
                    "files",
                    done,
                    total,
                    pct.min(90) as u8,
                    rel.display().to_string(),
                );
            }
        }

        // 3) 主库一致快照：VACUUM INTO 目标（目标库若残留先清，VACUUM INTO 要求不存在）。
        emit("database", total, total, 92, String::new());
        let db_dst = target_path.join("sailterm.db");
        if db_dst.exists() {
            let _ = std::fs::remove_file(&db_dst);
        }
        let dst = db_dst.to_string_lossy().to_string();
        state(&app).db.with(move |conn| {
            conn.execute("VACUUM INTO ?1", rusqlite::params![dst])?;
            Ok(())
        })?;

        // 4) 落地：改指 active，下次启动从新目录读取。
        crate::data_location::set_active(&default_dir, target_path)?;
        emit("done", total, total, 100, String::new());
        Ok(())
    })
    .await
}

/// 在系统文件管理器中打开当前数据目录。
///
/// 走后端直接开目录，**绕开前端 opener 的 `$APPDATA/**` scope 限制**——数据目录可被
/// 迁移到任意路径，基于固定 scope 的 JS `openPath` 迁后必然 ForbiddenPath。
#[tauri::command]
pub async fn open_data_dir(app: AppHandle) -> Result<(), AppError> {
    blocking(move || {
        let dir = state(&app).data_dir.clone();
        if !dir.exists() {
            return Err(AppError::Invalid("数据目录不存在".into()));
        }
        reveal_dir(&dir)
    })
    .await
}

/// 各平台原生方式在文件管理器中打开目录（不经 opener 插件，故不受其前端 scope 约束）。
pub(crate) fn reveal_dir(path: &std::path::Path) -> Result<(), AppError> {
    let p = path.display().to_string();
    let spawned = if cfg!(target_os = "windows") {
        std::process::Command::new("explorer").arg(&p).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&p).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&p).spawn()
    };
    // explorer 成功时也常返回非零退出码，故只 spawn 不 wait，忽略退出状态。
    spawned
        .map(|_| ())
        .map_err(|e| AppError::Io(format!("打开目录失败：{e}")))
}

/// 在系统文件管理器中打开本地路径：目录直接打开，文件则定位到其所在目录并选中。
///
/// 走后端原生方式（与 [`open_data_dir`] 同源），绕开 opener 插件 `$APPDATA/**` 前端 scope——
/// 本地文件栏可浏览用户机器上任意目录，固定 scope 的 JS `openPath` 无法覆盖。
#[tauri::command]
pub async fn reveal_local_path(path: String) -> Result<(), AppError> {
    blocking(move || {
        let p = std::path::PathBuf::from(&path);
        if !p.exists() {
            return Err(AppError::Invalid("路径不存在或已移动".into()));
        }
        if p.is_dir() {
            return reveal_dir(&p);
        }
        select_in_dir(&p)
    })
    .await
}

/// 用系统默认程序打开本地文件/目录（图片→看图器、PDF→阅读器、目录→文件管理器）。
///
/// 与 [`reveal_local_path`] 区别：那个只在文件管理器里定位选中，这个是真正「打开」。
/// 复用 [`reveal_dir`] 的原生 open：Windows `explorer <文件>` / macOS `open` / Linux `xdg-open`
/// 对文件即按关联程序打开，对目录即进目录。同样走后端，绕开 opener 插件前端 scope。
#[tauri::command]
pub async fn open_local_path(path: String) -> Result<(), AppError> {
    blocking(move || {
        let p = std::path::PathBuf::from(&path);
        if !p.exists() {
            return Err(AppError::Invalid("路径不存在或已移动".into()));
        }
        reveal_dir(&p)
    })
    .await
}

/// 打开文件的所在目录并选中它（Windows explorer /select、macOS open -R、Linux 退化为开父目录）。
///
/// 平台差异走编译期 `#[cfg]`：Windows 分支用到 `std::os::windows` + `raw_arg`，
/// 若用运行期 `cfg!()` 布尔宏，非 Windows 平台仍会类型检查该分支而编译失败。
fn select_in_dir(path: &std::path::Path) -> Result<(), AppError> {
    #[cfg(target_os = "windows")]
    let spawned = {
        // explorer 的 /select 对含空格路径要求引号紧贴路径：用 raw_arg 精确控制，
        // 避免 Command 自动加引号把 `/select,"C:\a b"` 拆坏。
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("explorer");
        cmd.raw_arg(format!("/select,\"{}\"", path.display()));
        cmd.spawn()
    };
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open")
        .arg("-R")
        .arg(path.display().to_string())
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let spawned = {
        let dir = path.parent().unwrap_or(path);
        std::process::Command::new("xdg-open")
            .arg(dir.display().to_string())
            .spawn()
    };
    spawned
        .map(|_| ())
        .map_err(|e| AppError::Io(format!("打开目录失败：{e}")))
}

/// 重启应用：重新拉起当前可执行文件后退出本进程。
///
/// 迁移完成后用它一键切到新数据目录（新实例启动时按引导指针 `active` 打开新目录）。
/// 无单实例锁，且新旧实例读写不同数据目录、不抢同一 SQLite 文件锁，故直接 spawn + exit 安全。
#[tauri::command]
pub async fn restart_app(app: AppHandle) -> Result<(), AppError> {
    let exe = std::env::current_exe().map_err(|e| AppError::Internal(e.to_string()))?;
    std::process::Command::new(exe)
        .spawn()
        .map_err(|e| AppError::Io(format!("重启失败：{e}")))?;
    app.exit(0);
    Ok(())
}

/// 开发者模式：运行时开/关**调用窗口**的 devtools（浏览器「检查」）。
///
/// 前端在开发者模式关闭时调用 `open=false`，强制收起可能已打开的 devtools；开启时不自动弹出（由用户按 F12）。
/// devtools 能力仅在 debug 构建或启用 `devtools` feature 时存在——release 默认未编入，本命令即空操作，
/// 因为 release 下 devtools 本就不可用（这正是「非开发者模式」想要的效果）。
#[tauri::command]
pub async fn set_devtools(window: tauri::WebviewWindow, open: bool) -> Result<(), AppError> {
    #[cfg(any(debug_assertions, feature = "devtools"))]
    {
        if open {
            window.open_devtools();
        } else {
            window.close_devtools();
        }
    }
    #[cfg(not(any(debug_assertions, feature = "devtools")))]
    let _ = (window, open);
    Ok(())
}

/// 🛡 安全状态灯色：绿=就绪且脱敏开；灰=出网关闭(仅本地)/未配置；黄=降级/异常。
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SecurityLight {
    Green,
    Gray,
    Yellow,
}

/// 灯色判定（纯函数，便于回归 语义）：
/// 黄（凭证未 OS 隔离=降级）> 灰（出网关闭或未配置 provider=仅本地/不可用）> 绿（就绪且脱敏开）。
fn light_for(vault_isolated: bool, network_enabled: bool, has_provider: bool) -> SecurityLight {
    if !vault_isolated {
        SecurityLight::Yellow
    } else if !network_enabled || !has_provider {
        SecurityLight::Gray
    } else {
        SecurityLight::Green
    }
}

/// 默认 provider 概览（**不含 key_ref / API Key**）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBrief {
    pub name: String,
    pub model: String,
    pub kind: ProviderKind,
    /// 是否本地模型（数据不出机）。
    pub is_local: bool,
}

/// 🛡 安全状态灯负载：AI 数据流向的实时仪表盘。
/// 只暴露**布尔/枚举/计数**级别的安全态势，绝不含任何凭证或 API Key。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityStatus {
    pub light: SecurityLight,
    /// 默认 provider 概览；未配置则 `None`。
    pub provider: Option<ProviderBrief>,
    /// AI 出网总开关（关=仅本地模型可用）。
    pub network_enabled: bool,
    /// 生效脱敏层级（云端已强制 ≥ Strict）。
    pub sanitize_level: SanitizeLevel,
    /// 凭证后端：`os-keyring` | `aes-gcm-file`。
    pub vault_backend: &'static str,
    /// 凭证是否 OS 级隔离（keyring=true；加密文件回退=false=降级）。
    pub vault_isolated: bool,
    /// 审批模式（只读自动/全审批）。
    pub approval_mode: ApprovalMode,
    /// 命令风险扫描层级。
    pub scan_level: ScanLevel,
    /// 活动会话数。
    pub session_count: usize,
    /// 审计派生：累计拦截高危次数。
    pub audit_blocked: usize,
    /// 审计派生：AI 命令提议总数。
    pub audit_total: usize,
}

/// 🛡 顶栏安全状态灯：聚合出网/脱敏/凭证隔离/会话统计四维态势。
#[tauri::command]
pub async fn security_status(app: AppHandle) -> Result<SecurityStatus, AppError> {
    blocking(move || {
        let st = state(&app);
        let settings = super::settings::load_ai_settings(&st.db)?;
        let provider = st.db.get_default_provider()?;

        let vault_backend = st.vault.backend();
        let vault_isolated = vault_backend == "os-keyring";
        // 云端 provider 脱敏层级强制 ≥ Strict；未配置 provider 时用默认（Strict）。
        let sanitize_level = match &provider {
            Some(p) => super::ai::enforce_cloud_sanitize(p.kind, p.sanitize_level),
            None => SanitizeLevel::default(),
        };
        let brief = provider.as_ref().map(|p| ProviderBrief {
            name: p.name.clone(),
            model: p.model.clone(),
            kind: p.kind,
            is_local: p.kind.is_local(),
        });

        // 审计派生的安全事件计数（只读，最多回看 1000 条）。
        let recent = st.db.list_audit(None, None, 1000)?;
        let audit_total = recent.len();
        let audit_blocked = recent
            .iter()
            .filter(|a| a.decision == Some(AuditDecision::Blocked))
            .count();

        // 灯色优先级：黄（凭证隔离降级）> 灰（出网关闭/未配置）> 绿（就绪且脱敏开）。
        let light = light_for(vault_isolated, settings.network_enabled, brief.is_some());

        Ok(SecurityStatus {
            light,
            provider: brief,
            network_enabled: settings.network_enabled,
            sanitize_level,
            vault_backend,
            vault_isolated,
            approval_mode: settings.approval_mode,
            scan_level: settings.scan_level,
            session_count: st.sessions.list().len(),
            audit_blocked,
            audit_total,
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 灯色语义：黄（凭证降级）优先于灰（出网关/未配置）优先于绿（就绪）。
    #[test]
    fn light_priority_yellow_over_gray_over_green() {
        // 凭证未 OS 隔离 → 黄，无论出网/配置如何（降级最刺眼）
        assert_eq!(light_for(false, true, true), SecurityLight::Yellow);
        assert_eq!(light_for(false, false, false), SecurityLight::Yellow);

        // 已隔离但出网关闭 → 灰（仅本地）
        assert_eq!(light_for(true, false, true), SecurityLight::Gray);
        // 已隔离、出网开但未配置 provider → 灰（AI 不可用）
        assert_eq!(light_for(true, true, false), SecurityLight::Gray);

        // 全绿：已隔离 + 出网开 + 有 provider
        assert_eq!(light_for(true, true, true), SecurityLight::Green);
    }
}
