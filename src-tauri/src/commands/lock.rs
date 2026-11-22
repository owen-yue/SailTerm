//! 锁屏 IPC 命令层：状态快照 / 手动锁 / 活动心跳 / 解锁校验 / PIN 设置与清除 / 空闲配置。
//!
//! 业务在 [`crate::lock`]（锁态单源 + PBKDF2 + 退避）；本层只做参数校验、保险库 IO
//! （统一 `blocking` 移出 async 执行器）与 `lock-changed` 全窗广播。
//!
//! 契约纪律：解锁的预期结果（错码 / 退避锁止）走**返回值** `UnlockOutcome` 而非错误通道——
//! 两者都是遮罩 UI 的正常分支，且 `retryAfterSecs` 需要前端带参本地化；只有「未设 PIN 就
//! 锁屏」这类误用才落 `AppError::Biz`。明文 PIN 只作入参瞬时进入，`Zeroizing` 即用完清零，
//! 绝不回显、绝不落事件/日志/store（同既有凭证红线）。

use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use zeroize::Zeroizing;

use super::settings::SettingKey;
use super::{blocking, state};
use crate::error::AppError;
use crate::lock::{self, LockState};
use crate::state::AppState;
use crate::vault::{keys, Secret};

/// 锁态变更广播事件（payload=是否锁定）：主窗 + `sess-*` 弹出窗 + 工具窗全部收到，
/// 各 WebView 在 `lib/lockClient` 订阅后渲染/撤下根级遮罩。
pub const EVENT_LOCK_CHANGED: &str = "lock-changed";

/// 锁屏快照：前端启动时拉一次（对齐 pinSet/idleMinutes 到设置页），锁定即时性靠事件推送。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LockStatus {
    pub locked: bool,
    pub pin_set: bool,
    pub idle_minutes: u64,
    /// 是否已设密保问答（忘记 PIN 的找回锚点）；true 才能走应用内重置。
    pub secq_set: bool,
}

/// 解锁判定回包（见模块头的通道取舍）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlockOutcome {
    pub ok: bool,
    /// true = 处于试错退避锁止期，本次未做校验（前端展示倒计时，勿再发重试）。
    pub blocked: bool,
    /// `blocked` 时的剩余秒数；错码但未达锁止门槛则为 0。
    pub retry_after_secs: u64,
}

/// 忘记 PIN 重置的判定回包（与 [`UnlockOutcome`] 同走返回值通道：错码 / 退避 / 无密保都是
/// 答题面板的正常分支，只有真 IPC/IO 异常才落 `AppError`）。`status` 恒带当前快照，
/// `ok` 时前端据此翻锁态（pinSet/secqSet/locked 转 false）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetOutcome {
    pub ok: bool,
    /// true = 处于试错退避锁止期，本次未做校验（前端展示倒计时）。
    pub blocked: bool,
    /// `blocked` 时的剩余秒数。
    pub retry_after_secs: u64,
    /// true = 答案错误且未达锁止门槛。
    pub wrong: bool,
    /// true = 未配置密保（遗留 PIN），无锚可验。
    pub no_secq: bool,
    pub status: LockStatus,
}

/// blocking 闭包内的解锁判决（异步侧只负责补发广播，不改判）。
enum Verdict {
    Unlocked,
    Already,
    Blocked(u64),
    WrongPin(u64),
}

fn snapshot(lk: &LockState) -> LockStatus {
    LockStatus {
        locked: lk.locked.load(Ordering::Relaxed),
        pin_set: lk.pin_set.load(Ordering::Relaxed),
        idle_minutes: lk.idle_minutes.load(Ordering::Relaxed),
        secq_set: lk.secq_set.load(Ordering::Relaxed),
    }
}

/// 当前锁屏状态快照（纯原子读，同步命令免 spawn_blocking）。
#[tauri::command]
pub fn lock_status(app: AppHandle) -> LockStatus {
    snapshot(&state(&app).lock)
}

/// 用户活动心跳（各窗口按 ≥10s 节流上报；空闲看门狗据此判定，见 [`start_watch`]）。
#[tauri::command]
pub fn lock_touch(app: AppHandle) {
    state(&app).lock.touch();
}

/// 立即锁屏（未设 PIN 拒绝——没有解锁门槛的「锁」只是自欺）。幂等：已锁则不重复广播。
#[tauri::command]
pub fn lock_now(app: AppHandle) -> Result<(), AppError> {
    let st = state(&app);
    let lk = &st.lock;
    if !lk.pin_set.load(Ordering::Relaxed) {
        return Err(AppError::Biz {
            key: "err.lock.noPin",
            zh: "尚未设置锁屏 PIN 码，请先在「设置 → 安全 → 屏幕锁定」中创建",
        });
    }
    if !lk.locked.swap(true, Ordering::SeqCst) {
        let _ = app.emit(EVENT_LOCK_CHANGED, true);
    }
    Ok(())
}

/// 用 PIN 解锁：退避锁止期直接回 blocked + 剩余秒数；校验通过则全窗广播撤罩。
#[tauri::command]
pub async fn lock_unlock(app: AppHandle, pin: String) -> Result<UnlockOutcome, AppError> {
    // AppHandle 不 Copy：clone 一份进 blocking，外面的 app 留着发广播。
    let app_in = app.clone();
    let verdict = blocking(move || {
        let st = state(&app_in);
        let lk = &st.lock;
        if !lk.locked.load(Ordering::Relaxed) {
            return Ok(Verdict::Already);
        }
        if let Some(secs) = lk.blocked_remaining() {
            return Ok(Verdict::Blocked(secs));
        }
        let pin = Zeroizing::new(pin);
        let stored = st.vault.get(&keys::lock_pin())?;
        // 保险库条目已被移除（锁定期间从其他路径清了 PIN）：视为无门槛，直接放行，
        // 绝不能把用户锁死在自己的应用里。
        let pass = match stored {
            None => true,
            Some(s) => lock::verify_pin(s.expose(), &pin),
        };
        if pass {
            lk.locked.store(false, Ordering::SeqCst);
            lk.reset_failures();
            Ok(Verdict::Unlocked)
        } else {
            Ok(Verdict::WrongPin(lk.note_failure()))
        }
    })
    .await?;
    match verdict {
        Verdict::Unlocked => {
            let _ = app.emit(EVENT_LOCK_CHANGED, false);
            Ok(UnlockOutcome {
                ok: true,
                blocked: false,
                retry_after_secs: 0,
            })
        }
        Verdict::Already => Ok(UnlockOutcome {
            ok: true,
            blocked: false,
            retry_after_secs: 0,
        }),
        Verdict::Blocked(secs) => Ok(UnlockOutcome {
            ok: false,
            blocked: true,
            retry_after_secs: secs,
        }),
        Verdict::WrongPin(secs) => Ok(UnlockOutcome {
            ok: false,
            blocked: secs > 0,
            retry_after_secs: secs,
        }),
    }
}

/// 设置/修改 PIN（4-6 位数字）。已存在时必须验旧；哈希（非明文）入保险库。
/// 首次建立 PIN 必须同时设密保问答（忘记 PIN 的找回锚点）；改 PIN 时密保为可选更新。
/// 校验旧 PIN 走保险库 IO，整体在 blocking 线程完成。
#[tauri::command]
pub async fn lock_pin_set(
    app: AppHandle,
    pin: String,
    old_pin: Option<String>,
    secq_question: Option<String>,
    secq_answer: Option<String>,
) -> Result<LockStatus, AppError> {
    let pin = Zeroizing::new(pin);
    lock::validate_pin(&pin)?;
    // 密保「成对且非空」才算要设置：只有问题没答案（或反之）视同未提供。
    let has_secq = secq_question
        .as_deref()
        .map(|q| !q.trim().is_empty())
        .unwrap_or(false)
        && secq_answer
            .as_deref()
            .map(|a| !lock::normalize_answer(a).is_empty())
            .unwrap_or(false);
    blocking(move || {
        let st = state(&app);
        let key = keys::lock_pin();
        let existed = st.vault.contains(&key)?;
        if existed {
            let old = Zeroizing::new(
                old_pin.filter(|s| !s.is_empty()).ok_or_else(|| {
                    AppError::Invalid("请输入当前 PIN 码".into())
                })?,
            );
            let stored = st.vault.get(&key)?;
            let pass = stored
                .as_ref()
                .map(|s| lock::verify_pin(s.expose(), &old))
                .unwrap_or(false);
            if !pass {
                return Err(AppError::Biz {
                    key: "err.lock.oldPin",
                    zh: "当前 PIN 码不正确",
                });
            }
        } else if !has_secq {
            // 首次建 PIN 必须同时设密保——否则日后「忘记密码」要么无锚可验形同虚设、
            // 要么把人彻底锁死。二者都不可接受，故在这一环就拒绝。
            return Err(AppError::Biz {
                key: "err.lock.needSecq",
                zh: "首次设置锁屏 PIN 须同时设置密保问题与答案，用于日后忘记 PIN 时找回",
            });
        }
        st.vault.store(&key, &Secret::new(lock::hash_pin(&pin)))?;
        st.lock.pin_set.store(true, Ordering::SeqCst);
        // 密保问答：提供了就（重）设并格式校验；未提供则维持原样（改 PIN 不必每次重填）。
        if has_secq {
            let q = secq_question.unwrap();
            let a = secq_answer.unwrap();
            lock::validate_question(&q)?;
            lock::validate_answer(&a)?;
            st.vault
                .store(&keys::lock_secq_question(), &Secret::new(q.trim().to_string()))?;
            st.vault
                .store(&keys::lock_secq_answer(), &Secret::new(lock::hash_answer(&a)))?;
            st.lock.secq_set.store(true, Ordering::SeqCst);
        }
        Ok(snapshot(&st.lock))
    })
    .await
}

/// 移除 PIN（= 关闭锁屏功能）：须验旧；若正处于锁定则一并解锁（关闭即无门槛）。
#[tauri::command]
pub async fn lock_pin_clear(app: AppHandle, old_pin: String) -> Result<LockStatus, AppError> {
    let app2 = app.clone();
    let (status, was_locked) = blocking(move || {
        let st = state(&app2);
        let key = keys::lock_pin();
        if st.vault.contains(&key)? {
            let old = Zeroizing::new(old_pin);
            let stored = st.vault.get(&key)?;
            let pass = stored
                .as_ref()
                .map(|s| lock::verify_pin(s.expose(), &old))
                .unwrap_or(false);
            if !pass {
                return Err(AppError::Biz {
                    key: "err.lock.oldPin",
                    zh: "当前 PIN 码不正确",
                });
            }
            // 关闭锁屏一并拔掉找回锚点（PIN + 密保问题 + 答案哈希），不留孤儿条目。
            st.vault.delete(&keys::of_lock())?;
        }
        st.lock.pin_set.store(false, Ordering::SeqCst);
        st.lock.secq_set.store(false, Ordering::SeqCst);
        st.lock.reset_failures();
        let was_locked = st.lock.locked.swap(false, Ordering::SeqCst);
        Ok((snapshot(&st.lock), was_locked))
    })
    .await?;
    if was_locked {
        let _ = app.emit(EVENT_LOCK_CHANGED, false);
    }
    Ok(status)
}

/// 读取锁屏密保问题原文（找回流程展示用）。问题本身非秘密，可直接回前端；
/// 未设置时返回 `None`（前端据此判定「没有可验证的找回途径」）。答案哈希绝不从这里外泄。
#[tauri::command]
pub async fn lock_secq_question(app: AppHandle) -> Result<Option<String>, AppError> {
    blocking(move || {
        let st = state(&app);
        Ok(st
            .vault
            .get(&keys::lock_secq_question())?
            .map(|s| s.expose().to_string()))
    })
    .await
}

/// 忘记 PIN 时的应用内重置（从锁屏遮罩的「忘记密码」答题后进）。
///
/// 与 [`lock_pin_clear`] 的区别：不验旧 PIN（用户正因忘了才进来），改为**校验密保答案**——
/// 这是整个找回路径的安全支点：没有「攻击者不知道的秘密」兜底，重置就等于没锁。
/// 猜答案与猜 PIN 是同一威胁，共用同一套退避锁止。校验通过才删锁屏 PIN + 密保条目并解锁
/// （Windows 单条凭据、macOS/Linux 合并表里的单个 key，绝不误伤其他连接密码）。
#[tauri::command]
pub async fn lock_pin_reset(app: AppHandle, answer: String) -> Result<ResetOutcome, AppError> {
    let answer = Zeroizing::new(answer);
    let app2 = app.clone();
    let (outcome, was_locked) = blocking(move || {
        let st = state(&app2);
        let lk = &st.lock;
        // 退避锁止期：与 lock_unlock 同口径——窗口内一律不校验（含答对也须等窗口过去），
        // 保住「限制试错速率」这一唯一威胁模型价值。
        if let Some(secs) = lk.blocked_remaining() {
            return Ok((
                ResetOutcome {
                    ok: false,
                    blocked: true,
                    retry_after_secs: secs,
                    wrong: false,
                    no_secq: false,
                    status: snapshot(lk),
                },
                false,
            ));
        }
        let stored = st.vault.get(&keys::lock_secq_answer())?;
        // 未配密保（仅可能出现在本功能之前的遗留 PIN）：无答案可验，拒绝重置——宁可引导
        // 用户拿旧 PIN 去设置页关闭，也绝不开一个「谁点都能解」的后门。仍计入退避防连点。
        let Some(sec) = stored else {
            let secs = lk.note_failure();
            return Ok((
                ResetOutcome {
                    ok: false,
                    blocked: secs > 0,
                    retry_after_secs: secs,
                    wrong: false,
                    no_secq: secs == 0,
                    status: snapshot(lk),
                },
                false,
            ));
        };
        if !lock::verify_answer(sec.expose(), &answer) {
            // 关键修正：达到锁止门槛的那一次即刻改报 blocked（带倒计时），而非继续只报
            // 「答案错误」——否则用户被悄悄限流、下次答对又被开头 blocked 挡回，完全摸不着头脑。
            let secs = lk.note_failure();
            return Ok((
                ResetOutcome {
                    ok: false,
                    blocked: secs > 0,
                    retry_after_secs: secs,
                    wrong: secs == 0,
                    no_secq: false,
                    status: snapshot(lk),
                },
                false,
            ));
        }
        // 幂等：重置一并拔掉 PIN 与密保（消除后续任何残留门槛）。
        st.vault.delete(&keys::of_lock())?;
        lk.pin_set.store(false, Ordering::SeqCst);
        lk.secq_set.store(false, Ordering::SeqCst);
        lk.reset_failures();
        let was_locked = lk.locked.swap(false, Ordering::SeqCst);
        Ok((
            ResetOutcome {
                ok: true,
                blocked: false,
                retry_after_secs: 0,
                wrong: false,
                no_secq: false,
                status: snapshot(lk),
            },
            was_locked,
        ))
    })
    .await?;
    if was_locked {
        let _ = app.emit(EVENT_LOCK_CHANGED, false);
    }
    Ok(outcome)
}

/// 设置空闲自动锁屏阈值（分钟，0=关闭，≤240）。落 settings 表并同步原子缓存，
/// 并回拨活动时钟（刚改完配置不该立即触发锁定）。
#[tauri::command]
pub async fn lock_config_set(app: AppHandle, idle_minutes: u64) -> Result<LockStatus, AppError> {
    if idle_minutes > 240 {
        return Err(AppError::Invalid("空闲阈值不能超过 240 分钟".into()));
    }
    blocking(move || {
        let st = state(&app);
        st.db
            .set_setting(SettingKey::LockIdleMinutes.as_str(), &idle_minutes.to_string())?;
        st.lock.idle_minutes.store(idle_minutes, Ordering::Relaxed);
        st.lock.touch();
        Ok(snapshot(&st.lock))
    })
    .await
}

/// 启动空闲看门狗（进程单例，`lib.rs` setup 挂载）：5s 轮询后端侧的聚合心跳，
/// 达阈值即全窗锁定。
///
/// 不在各前端窗口做倒计时——每个 WebView 是独立 JS 上下文，窗口级计时会「在别的窗口
/// 忙着自己却数空闲」误锁；活动心跳经 `lock_touch` 汇聚到后端单一 `last_activity`，
/// 判定只在这里做。
pub fn start_watch(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let st = app.state::<AppState>();
            let lk = &st.lock;
            let mins = lk.idle_minutes.load(Ordering::Relaxed);
            // 关闭档 / 未设 PIN（锁了没意义反而可能困住用户）/ 已锁 → 静默等下一轮。
            if mins == 0 || !lk.pin_set.load(Ordering::Relaxed) || lk.locked.load(Ordering::Relaxed)
            {
                continue;
            }
            let due = lk.idle_due(mins);
            drop(st);
            if due {
                let st = app.state::<AppState>();
                if !st.lock.locked.swap(true, Ordering::SeqCst) {
                    let _ = app.emit(EVENT_LOCK_CHANGED, true);
                }
            }
        }
    });
}
