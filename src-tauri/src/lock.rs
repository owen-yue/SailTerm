//! 锁屏（应用级）：全局锁态单源 + 空闲心跳 + PIN 口令的派生与校验。
//!
//! 威胁模型：防「离开座位时的临时窥屏与误操作」，不防已持有 OS 账户口令的攻击者。遮罩由前端各窗口渲染，但**锁态与校验一律在后端**：
//! 主窗 + `sess-*` 弹出窗 + 工具窗各有独立 JS 上下文，只有后端单源能保证「一锁全锁、
//! 一解全解」；状态变化经 `lock-changed` 事件广播到全部窗口（见 commands::lock）。
//!
//! 实现口径保持「遮罩盖住、不卸载任何东西」：锁定期间组件全部存活，传输/AI 轮次在
//! 后端继续跑，解锁即原位恢复、零丢失（这是与本功能配套的前端约定，勿改成条件渲染工作区）。
//!
//! PIN 模型（对齐全树 ring-only、零新增依赖的哲学，KDF 参数与 [`crate::sealed`] 同源）：
//! - PBKDF2-HMAC-SHA256，随机盐，迭代数随密文存储、钳位 [`ITER_MIN`]=..[`ITER_MAX`]，
//!   日后换 KDF 可按标识串平滑升级而不破坏旧值；
//! - 哈希存保险库 `sail:lock:pin`（见 vault::keys）——settings 表红线是「只存非敏感偏好」，
//!   PIN 哈希归凭证类，不落那里；
//! - 明文 PIN 只作为命令入参瞬时进入可信区，`Zeroizing` 即用完清零，绝不落日志/事件/store；
//! - 连续试错 [`FAIL_LIMIT`] 次起退避锁止（[`STEP_SECS`] 步进、封顶 [`MAX_BLOCK_SECS`]），
//!   防旁人现场暴力试。
//!
//! 忘记 PIN 的找回锚点：**密保问答**（对齐 Windows Hello / 手机应用锁的「用更强秘密重置」）。
//! PIN 不参与加密，遗忘不该销毁数据；但要让「忘记密码」对旁人失效，须让用户提前另设一个
//! 攻击者不知道的秘密。答案复用同一套 PBKDF2（归一化后派生），哈希进 vault；问题本身非密，
//! 明文存以便找回时展示。

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use ring::{pbkdf2, rand::SecureRandom, rand::SystemRandom};
use zeroize::Zeroizing;

use crate::error::AppError;

/// PBKDF2-HMAC-SHA256 迭代次数（OWASP 2023 量级，与 sealed.rs 一致）。
const ITERATIONS: u32 = 600_000;
/// 迭代次数下限/上限：防保险库条目被篡改填 0（校验形同虚设）或天文数字（解锁即 DoS 卡死）。
const ITER_MIN: u32 = 1_000;
const ITER_MAX: u32 = 5_000_000;
/// 随机盐长度（字节）。
const SALT_LEN: usize = 16;
/// 派生密钥长度（字节）。
const KEY_LEN: usize = 32;
/// 本实现写出的 KDF 标识（校验据此判定算法，预留平滑迁移）。
const KDF_ALG: &str = "pbkdf2-sha256";

/// PIN 格式：4-6 位纯数字（前端表单同口径预校验，这里是 fail-safe 的后端兜底）。
pub fn validate_pin(pin: &str) -> Result<(), AppError> {
    let ok = (4..=6).contains(&pin.chars().count()) && pin.chars().all(|c| c.is_ascii_digit());
    if !ok {
        return Err(AppError::Invalid("PIN 码须为 4-6 位数字".into()));
    }
    Ok(())
}

/// 从 PIN 生成可存储的哈希串：`pbkdf2-sha256:<iter>:<salt_b64>:<hash_b64>`（不含任何明文）。
pub fn hash_pin(pin: &str) -> String {
    hash_pin_with_iter(pin, ITERATIONS)
}

/// 迭代数可注入的内部实现（单测用小迭代提速；生产恒 [`ITERATIONS`]）。
fn hash_pin_with_iter(pin: &str, iter: u32) -> String {
    let mut salt = [0u8; SALT_LEN];
    SystemRandom::new()
        .fill(&mut salt)
        .expect("OS 随机源不可用（与 ssh/mod.rs 同前提）");
    let key = derive_key(pin, &salt, iter);
    let b64 = base64::engine::general_purpose::STANDARD;
    format!(
        "{KDF_ALG}:{iter}:{}:{}",
        b64.encode(salt),
        b64.encode(key.as_slice())
    )
}

/// 恒定时间校验 PIN；存储串非法（算法未知/迭代越界/base64 损坏）一律 false（不区分原因，避免预言机）。
pub fn verify_pin(stored: &str, pin: &str) -> bool {
    let mut parts = stored.split(':');
    let (Some(alg), Some(iter), Some(salt), Some(hash)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return false;
    };
    if alg != KDF_ALG {
        return false;
    }
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(hash) else {
        return false;
    };
    if raw.len() != KEY_LEN {
        return false;
    }
    let iter = match iter.parse::<u32>() {
        Ok(v) => v.clamp(ITER_MIN, ITER_MAX),
        Err(_) => return false,
    };
    let Ok(salt) = base64::engine::general_purpose::STANDARD.decode(salt) else {
        return false;
    };
    let rounds = match NonZeroU32::new(iter) {
        Some(v) => v,
        None => return false,
    };
    pbkdf2::verify(
        pbkdf2::PBKDF2_HMAC_SHA256,
        rounds,
        &salt,
        pin.as_bytes(),
        &raw,
    )
    .is_ok()
}

/// PBKDF2 派生 32 字节密钥；结果 `Zeroizing` 包裹，离开作用域即清零。
fn derive_key(pin: &str, salt: &[u8], iter: u32) -> Zeroizing<[u8; KEY_LEN]> {
    let mut key = [0u8; KEY_LEN];
    let rounds = NonZeroU32::new(iter.clamp(ITER_MIN, ITER_MAX)).expect("iter clamped to >= ITER_MIN");
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        rounds,
        salt,
        pin.as_bytes(),
        &mut key,
    );
    Zeroizing::new(key)
}

// ── 密保问答（忘记 PIN 的找回锚点）──

/// 答案归一化：去首尾空白 + 转小写。降低「大小写/尾空格」导致的答案对不上；代价是缩小
/// 答案有效字符集，可接受——本锁防的是旁人临时窥屏，不抗持有机器的高熵离线爆破。
pub fn normalize_answer(ans: &str) -> String {
    ans.trim().to_lowercase()
}

/// 问题格式（明文存储，仅用于找回时展示）：trim 后非空、≤100 字符。
pub fn validate_question(q: &str) -> Result<(), AppError> {
    let n = q.trim().chars().count();
    if n == 0 {
        return Err(AppError::Invalid("请填写密保问题".into()));
    }
    if n > 100 {
        return Err(AppError::Invalid("密保问题过长（不超过 100 字符）".into()));
    }
    Ok(())
}

/// 答案格式：归一化后非空、≤100 字符。
pub fn validate_answer(ans: &str) -> Result<(), AppError> {
    let n = normalize_answer(ans).chars().count();
    if n == 0 {
        return Err(AppError::Invalid("请填写密保答案".into()));
    }
    if n > 100 {
        return Err(AppError::Invalid("密保答案过长（不超过 100 字符）".into()));
    }
    Ok(())
}

/// 密保答案哈希：复用 PIN 的 PBKDF2 机制（[`hash_pin`] 只把入参当字符串派生，不做数字校验），
/// 入参先 [`normalize_answer`]。产物串格式与 PIN 一致，同样是 `pbkdf2-sha256:...`，绝不含明文。
pub fn hash_answer(ans: &str) -> String {
    hash_pin(&normalize_answer(ans))
}

/// 恒定时间校验密保答案（入参先归一化）。
pub fn verify_answer(stored: &str, ans: &str) -> bool {
    verify_pin(stored, &normalize_answer(ans))
}

/// 进入退避锁止的连续失败次数（第 N 次失败起锁 `（N-FAIL_LIMIT+1）* STEP_SECS` 秒）。
const FAIL_LIMIT: u32 = 3;
const STEP_SECS: u64 = 15;
const MAX_BLOCK_SECS: u64 = 300;

/// 试错退避 + 活动心跳（需互斥保护的少量状态）。
struct Inner {
    /// 最近一次「任一窗口有用户活动」的时刻（`lock_touch` 心跳刷新）。
    last_activity: Instant,
    /// 连续校验失败次数（成功解锁即清零）。
    fails: u32,
    /// 在此之前拒绝再试（退避锁止的到期时刻）。
    blocked_until: Option<Instant>,
}

/// 锁屏全局态。原子字段供异步看门狗/命令层免锁轮询；`inner` 只在极短临界区内持有。
pub struct LockState {
    /// 当前是否锁定（单源在此；前端只跟随渲染遮罩）。
    pub locked: AtomicBool,
    /// 是否已设置 PIN（保险库存在该条目的缓存；pin_set/pin_clear 命令维护，启动时装载）。
    pub pin_set: AtomicBool,
    /// 是否已设置密保问答（忘记 PIN 的找回锚点；缓存 vault 是否存在答案哈希条目）。
    pub secq_set: AtomicBool,
    /// 空闲自动锁屏阈值（分钟），0 = 关闭。启动时从 settings 表装载，config_set 更新。
    pub idle_minutes: AtomicU64,
    inner: Mutex<Inner>,
}

impl LockState {
    pub fn new(idle_minutes: u64, pin_set: bool, secq_set: bool) -> Self {
        Self {
            locked: AtomicBool::new(false),
            pin_set: AtomicBool::new(pin_set),
            secq_set: AtomicBool::new(secq_set),
            idle_minutes: AtomicU64::new(idle_minutes),
            inner: Mutex::new(Inner {
                // 启动即视为刚有活动：避免「带着旧心跳一开机就被空闲锁」。
                last_activity: Instant::now(),
                fails: 0,
                blocked_until: None,
            }),
        }
    }

    /// 用户活动心跳（任一窗口节流上报；只回拨时钟，无其他副作用）。
    pub fn touch(&self) {
        self.inner.lock().unwrap().last_activity = Instant::now();
    }

    /// 距最后心跳是否已超出 `minutes` 分钟空闲（看门狗轮询用；minutes=0 恒 false）。
    pub fn idle_due(&self, minutes: u64) -> bool {
        if minutes == 0 {
            return false;
        }
        let elapsed = self.inner.lock().unwrap().last_activity.elapsed();
        elapsed >= Duration::from_secs(minutes * 60)
    }

    /// 当前是否处于退避锁止期；是则返回剩余秒数（向上取整）。
    pub fn blocked_remaining(&self) -> Option<u64> {
        let until = self.inner.lock().unwrap().blocked_until?;
        let now = Instant::now();
        if now >= until {
            return None;
        }
        Some(until.duration_since(now).as_secs_f64().ceil() as u64)
    }

    /// 记一次校验失败；返回本次判定出的退避剩余秒数（未达门槛为 0）。
    pub fn note_failure(&self) -> u64 {
        let mut it = self.inner.lock().unwrap();
        it.fails += 1;
        let delay = backoff_secs(it.fails);
        if delay > 0 {
            it.blocked_until = Some(Instant::now() + Duration::from_secs(delay));
        }
        delay
    }

    /// 校验成功（或 PIN 被重设/清除）：清零失败计数与锁止，并回拨活动时钟
    /// （否则刚解锁就被同一轮空闲立即再锁）。
    pub fn reset_failures(&self) {
        let mut it = self.inner.lock().unwrap();
        it.fails = 0;
        it.blocked_until = None;
        it.last_activity = Instant::now();
    }
}

/// 连续第 `fails` 次失败对应的退避秒数（前 [`FAIL_LIMIT`] 次为 0，纯函数便于单测）。
fn backoff_secs(fails: u32) -> u64 {
    if fails < FAIL_LIMIT {
        return 0;
    }
    ((fails - FAIL_LIMIT + 1) as u64 * STEP_SECS).min(MAX_BLOCK_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单测用小迭代（生产 60 万次会让测试套件变慢且无意义）。
    const TEST_ITER: u32 = 2_000;

    #[test]
    fn hash_verify_roundtrip() {
        let stored = hash_pin_with_iter("1234", TEST_ITER);
        assert!(stored.starts_with("pbkdf2-sha256:"));
        assert!(!stored.contains("1234"), "存储串不得含明文");
        assert!(verify_pin(&stored, "1234"));
        assert!(!verify_pin(&stored, "1235"));
        assert!(!verify_pin(&stored, ""));
    }

    #[test]
    fn salted_hash_differs_and_both_verify() {
        let a = hash_pin_with_iter("0000", TEST_ITER);
        let b = hash_pin_with_iter("0000", TEST_ITER);
        assert_ne!(a, b, "每次须随机盐");
        assert!(verify_pin(&a, "0000") && verify_pin(&b, "0000"));
    }

    #[test]
    fn verify_rejects_malformed_stored() {
        // 算法未知 / 迭代非数字 / 段数不足 / base64 损坏 / 长度不符 → 一律 false，不得 panic。
        for bad in [
            "",
            "sha1:1000:c2FsdA:aaaa",
            "pbkdf2-sha256:abc:c2FsdA:aaaa",
            "pbkdf2-sha256:1000:c2FsdA",
            "pbkdf2-sha256:1000:c2FsdA:!!!not-base64!!!",
            // 迭代天文数字会被钳到上限后照常校验（防 DoS 靠钳位而非拒绝解析）。
            "pbkdf2-sha256:99999999999:c2FsdA:aaaa",
        ] {
            assert!(!verify_pin(bad, "1234"), "非法存储串应判 false: {bad:?}");
        }
    }

    #[test]
    fn iter_clamped_both_ways() {
        // 低于下限钳到 ITER_MIN：按钳位值复算，不因恶意迭代而放行或死循环。
        let low = format!(
            "pbkdf2-sha256:1:{}:{}",
            base64::engine::general_purpose::STANDARD.encode([0u8; SALT_LEN]),
            base64::engine::general_purpose::STANDARD.encode([0u8; KEY_LEN]),
        );
        assert!(!verify_pin(&low, "1234")); // 密钥对不上照样 false，但关键是没 panic 且可判定
    }

    #[test]
    fn validate_pin_shape() {
        assert!(validate_pin("1234").is_ok());
        assert!(validate_pin("123456").is_ok());
        assert!(validate_pin("123").is_err(), "不足 4 位");
        assert!(validate_pin("1234567").is_err(), "超过 6 位");
        assert!(validate_pin("12a4").is_err(), "非数字");
        assert!(validate_pin("").is_err());
    }

    #[test]
    fn answer_hash_verify_normalizes_case_and_space() {
        let stored = hash_answer("  BeiJing  ");
        assert!(stored.starts_with("pbkdf2-sha256:"));
        assert!(!stored.to_lowercase().contains("beijing"), "存储串不得含明文");
        // 归一化：大小写 / 首尾空格不影响判定。
        assert!(verify_answer(&stored, "beijing"));
        assert!(verify_answer(&stored, "  BEIJING "));
        assert!(!verify_answer(&stored, "shanghai"));
    }

    #[test]
    fn secq_question_answer_validation() {
        assert!(validate_question("你出生的城市？").is_ok());
        assert!(validate_question("   ").is_err(), "空白问题非法");
        assert!(validate_answer("abc").is_ok());
        assert!(validate_answer("   ").is_err(), "归一化后空答案非法");
    }

    #[test]
    fn backoff_step_function() {
        assert_eq!(backoff_secs(1), 0);
        assert_eq!(backoff_secs(2), 0);
        assert_eq!(backoff_secs(3), 15);
        assert_eq!(backoff_secs(4), 30);
        assert_eq!(backoff_secs(100), MAX_BLOCK_SECS, "封顶");
    }

    #[test]
    fn failure_backoff_and_reset() {
        let st = LockState::new(0, false, false);
        assert_eq!(st.note_failure(), 0);
        assert_eq!(st.note_failure(), 0);
        assert_eq!(st.note_failure(), 15);
        assert!(st.blocked_remaining().is_some(), "第 3 次失败后须处于锁止期");
        st.reset_failures();
        assert_eq!(st.blocked_remaining(), None);
    }

    #[test]
    fn idle_due_respects_minutes_and_touch() {
        let st = LockState::new(0, false, false);
        assert!(!st.idle_due(0), "关闭档恒不触发");
        assert!(!st.idle_due(1), "刚创建不超时");
        // touch 只回拨时钟；阈值判定用分钟粒度，这里仅验证不 panic 且现值方向正确。
        st.touch();
        assert!(!st.idle_due(1));
    }
}
