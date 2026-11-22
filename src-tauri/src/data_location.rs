//! 数据目录迁移：在「系统默认应用数据目录」放一个**不随迁移搬走**的引导指针（bootstrap），
//! 记录当前真正生效的数据目录（`active`）与下次启动待执行的迁移目标（`pending`）。
//!
//! 为什么走「暂存 + 重启」而非即时搬迁：运行期 SQLite 持有文件锁、还带 `-wal/-shm`，热拷贝会拷坏。
//! 故迁移在**下次启动、开库之前**由 [`resolve`] 落地拷贝（与 `state::migrate_legacy_data` 同思路）。
//!
//! 注意：凭证若存于 OS 钥匙串（`vault.use_os=true`），与数据目录无关，**不会**随目录搬迁；
//! 只有降级用的 `vault.enc/key` 文件才会被拷贝。跨机器迁移密钥类凭证本就不可随行。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 引导指针文件名（始终位于系统默认 `app_data_dir`，迁移时跳过、不拷进目标）。
pub const BOOTSTRAP_FILE: &str = "data_location.json";

/// 主库三件套：即时迁移时跳过原始拷贝，改由 `VACUUM INTO` 生成一致快照（运行期持锁也安全）。
pub const DB_FILES: &[&str] = &["sailterm.db", "sailterm.db-shm", "sailterm.db-wal"];

#[derive(Default, Serialize, Deserialize)]
struct DataLocation {
    /// 当前生效的自定义数据目录；`None` = 用系统默认目录。
    #[serde(default)]
    active: Option<PathBuf>,
    /// 下次启动待搬迁到的目标目录；`None` = 无待办。
    #[serde(default)]
    pending: Option<PathBuf>,
}

fn bootstrap_path(default_dir: &Path) -> PathBuf {
    default_dir.join(BOOTSTRAP_FILE)
}

fn load(default_dir: &Path) -> DataLocation {
    std::fs::read(bootstrap_path(default_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save(default_dir: &Path, loc: &DataLocation) -> std::io::Result<()> {
    std::fs::create_dir_all(default_dir)?;
    let json = serde_json::to_vec_pretty(loc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(bootstrap_path(default_dir), json)
}

/// 递归拷贝 `from` → `to`；顶层 `skip` 命中的文件名跳过（避免把引导指针 itself 拷进目标）。
fn copy_tree(from: &Path, to: &Path, skip: Option<&str>) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name_os = entry.file_name();
        let name = name_os.to_string_lossy();
        if skip == Some(name.as_ref()) {
            continue;
        }
        let src = entry.path();
        let dst = to.join(name_os);
        if src.is_dir() {
            copy_tree(&src, &dst, None)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// 解析本次启动应使用的数据目录：
/// 若有 `pending`，在开库前把当前数据目录（`active`，或系统默认）整体拷到目标；
/// 成功后 `active` 指向目标、清除 `pending`；失败则放弃本次迁移、维持现状（清除 `pending` 不反复重试）。
/// 返回最终生效目录（`active` 或系统默认）。
pub fn resolve(default_dir: &Path) -> PathBuf {
    let mut loc = load(default_dir);
    if let Some(target) = loc.pending.take() {
        let source = loc
            .active
            .clone()
            .unwrap_or_else(|| default_dir.to_path_buf());
        let migrated =
            source == target || copy_tree(&source, &target, Some(BOOTSTRAP_FILE)).is_ok();
        if migrated {
            loc.active = Some(target);
        } else {
            debug!("数据目录迁移失败，已放弃本次迁移并维持原目录");
        }
        if let Err(e) = save(default_dir, &loc) {
            debug!("写入数据目录引导指针失败：{e}");
        }
    }
    loc.active.unwrap_or_else(|| default_dir.to_path_buf())
}

/// 递归收集 `dir` 下所有文件的**相对 `root` 路径**（供即时迁移逐项拷贝与进度统计）。
pub fn collect_files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(root, &path, out)?;
        } else {
            out.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        }
    }
    Ok(())
}

/// 即时迁移成功后直接把生效目录改指 `target`（并清 `pending`）；
/// 下次启动 [`resolve`] 直接用它、不再重拷（数据已在本命令里实时拷完）。
pub fn set_active(default_dir: &Path, target: PathBuf) -> std::io::Result<()> {
    let mut loc = load(default_dir);
    loc.active = Some(target);
    loc.pending = None;
    save(default_dir, &loc)
}
