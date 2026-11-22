//! 会话录制 / 回放：把终端输出经 VT 仿真还原后**原样**追加到磁盘文件，供事后查看。
//!
//! 录制挂在 [`TerminalRing`] 上（SSH 与本地会话同一路径），`push` 时还原「用户实际看到的
//! 最终行」再落盘（默认不脱敏，原样记录终端输出）。文件放在应用数据目录
//! `recordings/` 下，不引入 fs 插件。
//!
//! 落盘**默认明文**（随处可读、便于分发）。档案可勾选「加密
//! 落盘」启用 **AES-256-GCM**（数据密钥归 [`crate::vault::Vault`] 管理）：外部打开只见
//! 密文，程序内回放按文件头魔数 `SAILREC\x01` 嗅探自动解密；明文文件照旧直读。

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use super::{blocking, state};
use crate::error::AppError;
use crate::session::{ConnectSpec, TerminalRing};
use crate::vault::{keys, Secret, Vault};

/// 录制文件所在子目录（相对 app data dir）。
const DIR: &str = "recordings";
/// 回放读取上限（防超大文件撑爆前端）。
const READ_MAX_BYTES: u64 = 512 * 1024;
/// 加密录制文件的魔数头（8 字节）：`record_read` 据此区分加密/明文，明文旧文件照旧可读。
pub(crate) const RECORD_MAGIC: &[u8; 8] = b"SAILREC\x01";
/// AES-GCM 标准 nonce 长度；分帧格式 `[u32 LE 载荷长][nonce(12) ‖ 密文+tag]`。
const NONCE_LEN: usize = 12;
/// 录制数据密钥长度（AES-256）。
const KEY_LEN: usize = 32;

/// 取（或首次生成）录制数据密钥：base64 存进 Vault，密钥保护直接复用钥匙串/加密文件双后端。
/// 首次调用随机生成 32 字节并落库；之后各会话、录制与回放共用同一把。
pub fn record_data_key(vault: &Vault) -> Result<[u8; KEY_LEN], AppError> {
    let key = keys::record_data_key();
    if let Some(sec) = vault.get(&key)? {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(sec.expose().trim().as_bytes())
            .map_err(|_| AppError::Vault("录制数据密钥损坏".into()))?;
        if raw.len() != KEY_LEN {
            return Err(AppError::Vault("录制数据密钥长度非法".into()));
        }
        let mut k = [0u8; KEY_LEN];
        k.copy_from_slice(&raw);
        return Ok(k);
    }
    let gen = Aes256Gcm::generate_key(&mut OsRng);
    let mut k = [0u8; KEY_LEN];
    k.copy_from_slice(gen.as_slice());
    vault.store(
        &key,
        &Secret::new(base64::engine::general_purpose::STANDARD.encode(k)),
    )?;
    Ok(k)
}

/// 从 Vault 取/建录制数据密钥并构造 cipher（起录加密、回放解密共用同一路径）。
fn build_cipher(app: &AppHandle) -> Result<Aes256Gcm, AppError> {
    let st = state(app);
    let key = record_data_key(&st.vault)?;
    // 长度由 KEY_LEN 保证，new_from_slice 不会失败。
    Aes256Gcm::new_from_slice(&key).map_err(|e| AppError::Vault(e.to_string()))
}

/// 密封一行落盘分帧：`[u32 LE 载荷长][nonce ‖ 密文+tag]`。每行独立随机 nonce，追加写友好。
/// 加密失败（仅极端尺寸会触发）返回空 Vec，调用方据此跳过该行——与写盘失败同口径，绝不阻断终端。
pub(crate) fn seal_chunk(cipher: &Aes256Gcm, plain: &[u8]) -> Vec<u8> {
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let Ok(ct) = cipher.encrypt(&nonce, plain) else {
        return Vec::new();
    };
    let payload_len = NONCE_LEN + ct.len();
    let mut out = Vec::with_capacity(4 + payload_len);
    out.extend_from_slice(&(payload_len as u32).to_le_bytes());
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ct);
    out
}

/// 解密整份加密录制（回放用）：校验魔数后逐帧还原、按序拼接为文本。
/// 尾部残缺帧（写到一半崩溃）静默丢弃，保留已完整解出的部分。
fn decrypt_recording(cipher: &Aes256Gcm, blob: &[u8]) -> Result<String, AppError> {
    let Some(mut rest) = blob.strip_prefix(RECORD_MAGIC) else {
        return Err(AppError::Io("录制文件格式非法".into()));
    };
    let mut out = String::new();
    while rest.len() >= 4 {
        let len = u32::from_le_bytes(rest[..4].try_into().unwrap()) as usize;
        rest = &rest[4..];
        if len <= NONCE_LEN || rest.len() < len {
            break; // 载荷不完整或非法，停在此处
        }
        let (frame, tail) = rest.split_at(len);
        rest = tail;
        let (nonce, ct) = frame.split_at(NONCE_LEN);
        let plain = cipher
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| AppError::Io("录制解密失败（数据密钥已变更或文件损坏）".into()))?;
        out.push_str(&String::from_utf8_lossy(&plain));
    }
    Ok(out)
}

/// 取末尾 `max` 字节（起点回退到 char 边界，避免切断多字节字符）。
fn tail_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// 一条录制文件的元数据（回前端）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RecordingInfo {
    /// 文件名（含扩展名，作为读取句柄）。
    pub name: String,
    /// 文件所在目录的绝对路径（全局库跨目录聚合后，回放/删除/打开都按此定位）。
    pub dir: String,
    /// 来源主机名（起录时登记）；`None` = 遗留/外部产生、索引查不到。
    pub host: Option<String>,
    pub size: u64,
    pub modified_ms: i64,
    /// 该文件是否正被某会话实时写入（录制中）。`true` = 正在录，前端标「录制中」，
    /// 回放为截至此刻的实时快照（不自动追新，需重开刷新）。
    pub active: bool,
}

/// 开始录制当前会话的终端输出（原样落盘）。
///
/// 命名与自动录制同源：取会话主机名 + 档案配置的文件名模式（覆盖/时间戳），
/// 避免手动产出 `s<会话id>-<毫秒>` 这种既不可读、又和自动录制对不上的文件名。
///
/// `dir/file_pattern/overwrite/timestamp_prefix` 为录制面板的临时覆盖：传入则优先于
/// 档案配置，未传（None）回退档案值，再缺省走内置默认（服务无档案的快速直连/本地会话）。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn record_start(
    app: AppHandle,
    session_id: String,
    dir: Option<String>,
    file_pattern: Option<String>,
    overwrite: Option<bool>,
    timestamp_prefix: Option<bool>,
) -> Result<String, AppError> {
    blocking(move || {
        let st = state(&app);
        let ring = st
            .sessions
            .ring(&session_id)
            .ok_or_else(|| AppError::NoSuchSession(session_id.clone()))?;
        if ring.is_recording() {
            return Err(AppError::Invalid("该会话已在录制中".into()));
        }
        // 会话主机名 + 所属档案的录制配置（快速直连无档案时走内置默认命名）。
        let (host, profile_id) = st
            .sessions
            .update(&session_id, |e| (e.host.clone(), e.profile_id.clone()))
            .unwrap_or_else(|| (String::new(), None));
        let profile = profile_id
            .as_deref()
            .and_then(|pid| st.db.get_profile(pid).ok().flatten());
        let t = RecordTrigger {
            host,
            // 入参 dir 优先（面板已按当前配置传入）；缺省回退档案 record_dir，再缺省用默认 recordings/。
            dir: dir.or_else(|| profile.as_ref().and_then(|p| p.record_dir.clone())),
            file_pattern: file_pattern.or_else(|| profile.as_ref().and_then(|p| p.record_file_pattern.clone())),
            overwrite: overwrite
                .unwrap_or_else(|| profile.as_ref().map(|p| p.record_overwrite).unwrap_or(false)),
            timestamp_prefix: timestamp_prefix
                .unwrap_or_else(|| profile.as_ref().map(|p| p.record_timestamp_prefix).unwrap_or(false)),
            // 加密仍只跟档案（面板暂不提供临时加密开关）；无档案默认明文。
            encrypt: profile.as_ref().map(|p| p.record_encrypt).unwrap_or(false),
        };
        apply(&app, &ring, &t)
    })
    .await
}

/// 停止录制，返回文件名（若未在录制则 `Err`）。
#[tauri::command]
pub async fn record_stop(app: AppHandle, session_id: String) -> Result<String, AppError> {
    blocking(move || {
        let st = state(&app);
        let ring = st
            .sessions
            .ring(&session_id)
            .ok_or_else(|| AppError::NoSuchSession(session_id.clone()))?;
        ring.stop_recording()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .ok_or_else(|| AppError::Invalid("该会话未在录制".into()))
    })
    .await
}

/// 查询会话是否正在录制。
#[tauri::command]
pub async fn record_status(app: AppHandle, session_id: String) -> Result<bool, AppError> {
    blocking(move || {
        Ok(state(&app)
            .sessions
            .ring(&session_id)
            .map(|r| r.is_recording())
            .unwrap_or(false))
    })
    .await
}

/// 列出全局录制文件（新→旧）：跨目录聚合。
///
/// 数据源两路合并去重：
/// 1. 索引（`record_index`）里的文件——应用录的，带真实主机名，直接按绝对路径 stat；
/// 2. 默认 `recordings/` + 各档案自定义目录扫出的 `.log`——补上索引外的遗留/外部文件（host 为空）。
/// 同时收敛索引：文件已不存在的行清掉。
#[tauri::command]
pub async fn record_list(app: AppHandle) -> Result<Vec<RecordingInfo>, AppError> {
    blocking(move || {
        let st = state(&app);
        let mut out: Vec<RecordingInfo> = Vec::new();
        let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        let mut existing: Vec<String> = Vec::new();

        // 1) 索引里的文件（带真实主机）：按登记的绝对路径直接 stat。
        for rec in st.db.record_index_list()? {
            let p = PathBuf::from(&rec.path);
            let Ok(md) = std::fs::metadata(&p) else { continue };
            if !md.is_file() {
                continue;
            }
            let name = match p.file_name() {
                Some(n) if n.to_string_lossy().ends_with(".log") => n.to_string_lossy().into_owned(),
                _ => continue,
            };
            if seen.insert(p.clone()) {
                existing.push(rec.path.clone());
                out.push(RecordingInfo {
                    dir: p.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default(),
                    name,
                    host: Some(rec.host.clone()),
                    size: md.len(),
                    modified_ms: mtime_ms(&md),
                    active: false,
                });
            }
        }

        // 2) 默认目录 + 各档案自定义目录：补上索引外的 `.log`（遗留/外部，host 为空）。
        let mut dirs: Vec<PathBuf> = vec![resolve_dir(&app, None)?];
        for p in st.db.list_profiles()? {
            if let Some(rd) = p.record_dir.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                if let Ok(d) = resolve_dir(&app, Some(rd)) {
                    dirs.push(d);
                }
            }
        }
        for d in dirs {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let Ok(md) = e.metadata() else { continue };
                if !md.is_file() {
                    continue;
                }
                let name = e.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".log") {
                    continue;
                }
                let abs = e.path();
                if seen.insert(abs.clone()) {
                    existing.push(abs.to_string_lossy().into_owned());
                    out.push(RecordingInfo {
                        dir: d.to_string_lossy().into_owned(),
                        name,
                        host: None,
                        size: md.len(),
                        modified_ms: mtime_ms(&md),
                        active: false,
                    });
                }
            }
        }

        // 3) 收敛索引：文件已不存在的行清掉（失败不阻断列表）。
        let _ = st.db.record_index_prune(&existing);

        // 4) 标记「录制中」：反查各会话正在写入的文件绝对路径，命中即置 active（供前端标红/提示实时快照）。
        let active: std::collections::HashSet<PathBuf> =
            st.sessions.active_record_paths().into_iter().collect();
        if !active.is_empty() {
            for info in out.iter_mut() {
                if active.contains(Path::new(&info.dir).join(&info.name).as_path()) {
                    info.active = true;
                }
            }
        }

        out.sort_by_key(|a| std::cmp::Reverse(a.modified_ms));
        Ok(out)
    })
    .await
}

/// 文件 mtime → unix 毫秒（取不到记 0）。
fn mtime_ms(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 读取一条录制（回放预览）：文件带 `SAILREC\x01` 头则用 Vault 数据密钥解密，否则按明文读。
/// 超过上限取**末尾**（更接近收尾状态）。
#[tauri::command]
pub async fn record_read(app: AppHandle, name: String, dir: Option<String>) -> Result<String, AppError> {
    blocking(move || {
        let path = safe_path(&resolve_dir(&app, dir.as_deref())?, &name)?;
        let bytes =
            std::fs::read(&path).map_err(|e| AppError::Io(format!("读取录制失败：{e}")))?;
        let text = if bytes.starts_with(RECORD_MAGIC) {
            decrypt_recording(&build_cipher(&app)?, &bytes)?
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        Ok(tail_str(&text, READ_MAX_BYTES as usize))
    })
    .await
}

/// 删除一条录制。
#[tauri::command]
pub async fn record_delete(app: AppHandle, name: String, dir: Option<String>) -> Result<(), AppError> {
    blocking(move || {
        let path = safe_path(&resolve_dir(&app, dir.as_deref())?, &name)?;
        std::fs::remove_file(&path).map_err(|e| AppError::Io(format!("删除录制失败：{e}")))?;
        // 同步清理索引，避免残留。
        let _ = state(&app).db.record_index_forget(&path.to_string_lossy());
        Ok(())
    })
    .await
}

/// 在系统文件管理器中打开录制存放目录（与 resolve_dir 同源：空→默认 recordings/，
/// 相对名→数据目录下子目录，绝对路径→直接采用）。目录不存在时先建，保证能打开。
#[tauri::command]
pub async fn record_open_dir(app: AppHandle, dir: Option<String>) -> Result<(), AppError> {
    blocking(move || {
        let d = resolve_dir(&app, dir.as_deref())?;
        std::fs::create_dir_all(&d)?;
        super::app::reveal_dir(&d)
    })
    .await
}

/// 校验文件名落在 recordings 目录内（拒绝 `..` 与绝对路径）。
fn safe_path(dir: &Path, name: &str) -> Result<PathBuf, AppError> {
    if name.is_empty()
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || !name.ends_with(".log")
    {
        return Err(AppError::Invalid("非法录制文件名".into()));
    }
    Ok(dir.join(name))
}

// ─────────────────────── 自动录制（连上即录）───────────────────────

/// 从 [`ConnectSpec`] 抽取的自动录制意图（建连前取出，避免为触发录制而克隆含凭证的 spec）。
pub struct RecordTrigger {
    /// 主机名（`%n` 取值；档案未存名时的可读锚点）。
    pub host: String,
    pub dir: Option<String>,
    pub file_pattern: Option<String>,
    pub overwrite: bool,
    pub timestamp_prefix: bool,
    /// 加密落盘（数据密钥归 Vault）；true = 默认。
    pub encrypt: bool,
}

impl RecordTrigger {
    pub fn from_spec(spec: &ConnectSpec) -> Self {
        Self {
            host: spec.host.clone(),
            dir: spec.record_dir.clone(),
            file_pattern: spec.record_file_pattern.clone(),
            overwrite: spec.record_overwrite,
            timestamp_prefix: spec.record_timestamp_prefix,
            encrypt: spec.record_encrypt,
        }
    }
}

/// 会话建立后开启自动录制：定位目录、渲染文件名、在会话的输出环上起录；返回文件名。
/// 目录限定在 app data dir 之下（子目录名白名单，拒绝穿越/绝对路径）。
pub fn apply(app: &AppHandle, ring: &TerminalRing, t: &RecordTrigger) -> Result<String, AppError> {
    let dir = resolve_dir(app, t.dir.as_deref())?;
    std::fs::create_dir_all(&dir)?;
    let name = render_name(t.file_pattern.as_deref(), &t.host, crate::db::now_ms());
    let path = dir.join(&name);
    // 明确要加密但数据密钥拿不到（钥匙串被拒/损坏）：降级明文并记日志，
    // 绝不因加密失败而丢录制（无密钥的密文事后也无法解密，不如退回明文）。
    let cipher = if t.encrypt {
        match build_cipher(app) {
            Ok(c) => Some(c),
            Err(e) => {
                debug!("[record] 数据密钥不可用，本次改明文录制：{e}");
                None
            }
        }
    } else {
        None
    };
    ring.start_recording(path.clone(), t.overwrite, t.timestamp_prefix, cipher)
        .map_err(|e| AppError::Io(format!("录制开启失败：{e}")))?;
    // 登记路径→主机（全局库靠此标注来源）；失败不阻断录制，只影响标注。
    let _ = state(app).db.record_index_put(&path.to_string_lossy(), &t.host);
    Ok(name)
}

/// 录制存放目录：`sub` 为空用默认 `recordings/`；否则 app data dir 下的安全子目录名。
fn resolve_dir(app: &AppHandle, sub: Option<&str>) -> Result<PathBuf, AppError> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Internal(format!("无法定位数据目录：{e}")))?;
    match sub.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(base.join(DIR)),
        Some(s) => {
            // 绝对路径：用户通过原生文件对话框选定，直接采用。
            let p = Path::new(s);
            if p.is_absolute() {
                return Ok(p.to_path_buf());
            }
            // 相对路径：只允许纯目录名（不含 .. 或路径分隔符），拼到 app data dir 下。
            if s.contains("..") || s.contains('/') || s.contains('\\') {
                return Err(AppError::Invalid("非法录制目录".into()));
            }
            Ok(base.join(sanitize_name(s)))
        }
    }
}

/// 文件名模式渲染：`%n`→主机、`%Y %m %d %H %M %S`→UTC 时间；无模式用 `{host}-{ms}.log`。
/// 结果过滤掉文件系统非法字符并保证 `.log` 结尾。
fn render_name(pattern: Option<&str>, name: &str, ms: i64) -> String {
    let raw = match pattern {
        None => format!("{name}-{ms}"),
        Some(p) => {
            let (y, mo, d, h, mi, s) = split_utc(ms);
            let mut out = String::new();
            let mut it = p.chars();
            while let Some(c) = it.next() {
                if c != '%' {
                    out.push(c);
                    continue;
                }
                match it.next() {
                    Some('n') => out.push_str(name),
                    Some('Y') => out.push_str(&format!("{y:04}")),
                    Some('m') => out.push_str(&format!("{mo:02}")),
                    Some('d') => out.push_str(&format!("{d:02}")),
                    Some('H') => out.push_str(&format!("{h:02}")),
                    Some('M') => out.push_str(&format!("{mi:02}")),
                    Some('S') => out.push_str(&format!("{s:02}")),
                    Some('%') => out.push('%'),
                    Some(other) => {
                        out.push('%');
                        out.push(other);
                    }
                    None => out.push('%'),
                }
            }
            out
        }
    };
    let mut safe = sanitize_name(&raw);
    if !safe.ends_with(".log") {
        safe.push_str(".log");
    }
    safe
}

/// 文件名安全化：保留字母数字（含 CJK）与 `-_ .`，把路径分隔符/冒号/控制符等换成 `-`。
fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '?' | '*' | '"' | '<' | '>' | '|' => '-',
            c if (c as u32) < 0x20 => '-',
            c => c,
        })
        .collect()
}

/// 录制内容行前缀（UTC，`YYYY-MM-DD HH:MM:SS`）。供 [`crate::session::TerminalRing`] 落盘时调用。
pub fn ts_prefix_utc() -> String {
    let (y, mo, d, h, mi, s) = split_utc(crate::db::now_ms());
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// epoch 毫秒 → UTC 年月日时分秒。用 Howard Hinnant 的 days→civil（免引 chrono）。
fn split_utc(ms: i64) -> (i64, u32, u32, u32, u32, u32) {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86400);
    let sod = secs.rem_euclid(86400);
    let (y, mo, d) = civil_from_days(days);
    (
        y,
        mo,
        d,
        (sod / 3600) as u32,
        ((sod % 3600) / 60) as u32,
        (sod % 60) as u32,
    )
}

/// days_from_civil 的逆运算：距 1970-01-01 的天数 → (年, 月, 日)。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_path_guard() {
        let dir = Path::new("/tmp/rec");
        assert!(safe_path(dir, "ok.log").is_ok());
        assert!(safe_path(dir, "../x.log").is_err());
        assert!(safe_path(dir, "a/b.log").is_err());
        assert!(safe_path(dir, "x.txt").is_err());
        assert!(safe_path(dir, "").is_err());
    }

    /// 文件名模板渲染：无模式用 `{host}-{ms}.log`；`%n`/`%Y%m%d%H%M%S` 按 UTC 展开；
    /// 结果经 sanitize 并保证 `.log` 结尾。
    #[test]
    fn render_name_expands_pattern_in_utc() {
        // 无模式：主机名 + epoch 毫秒
        assert_eq!(render_name(None, "srv", 1704164645000), "srv-1704164645000.log");
        // 2024-01-02T03:04:05Z 对应毫秒
        let ms = 1_704_164_645_000;
        assert_eq!(
            render_name(Some("%n-%Y-%m-%d_%H-%M-%S"), "srv", ms),
            "srv-2024-01-02_03-04-05.log"
        );
        // 已是 .log 结尾不重复追加；模式中的路径分隔符经 sanitize 换成 `-`。
        assert_eq!(render_name(Some("%n.log"), "srv", ms), "srv.log");
        assert_eq!(render_name(Some("%n/..%n"), "srv", ms), "srv-..srv.log");
    }

    /// sanitize_name：路径分隔符/冒号/星号/控制符换成 `-`，保留字母数字、`-_.` 与 CJK。
    #[test]
    fn sanitize_name_strips_unsafe_chars() {
        assert_eq!(sanitize_name("a/b:c*d?e"), "a-b-c-d-e");
        assert_eq!(sanitize_name("日志_2024.01"), "日志_2024.01");
    }

    /// 加密录制往返：逐行 seal_chunk + 魔数头拼成文件，decrypt_recording 还原与原文一致；
    /// 密文里不得出现明文；换一把密钥解密必须报错。
    #[test]
    fn record_crypto_seal_decrypt_roundtrip() {
        let cipher = Aes256Gcm::new_from_slice(&[7u8; KEY_LEN]).unwrap();
        let lines = ["hello\n", "中文 行\n", "\n"];
        let mut blob: Vec<u8> = RECORD_MAGIC.to_vec();
        for l in lines {
            blob.extend_from_slice(&seal_chunk(&cipher, l.as_bytes()));
        }
        assert!(blob.starts_with(RECORD_MAGIC));
        // 落盘字节里不得含明文 "hello"
        assert!(!blob.windows(5).any(|w| w == b"hello"));
        assert_eq!(decrypt_recording(&cipher, &blob).unwrap(), "hello\n中文 行\n\n");
        // 另一把密钥 → 认证标签校验失败
        let other = Aes256Gcm::new_from_slice(&[9u8; KEY_LEN]).unwrap();
        assert!(decrypt_recording(&other, &blob).is_err());
    }

    /// tail_str：末尾截断起点回退到 char 边界，不把多字节字符切成半个。
    #[test]
    fn tail_str_keeps_char_boundary() {
        assert_eq!(tail_str("abcdef", 3), "def");
        assert_eq!(tail_str("短abc", 100), "短abc");
        // "中文abc" = 3+3+1+1+1 字节；取末尾 4 字节 → 回退到 'a' 边界
        assert_eq!(tail_str("中文abc", 4), "abc");
    }
}
