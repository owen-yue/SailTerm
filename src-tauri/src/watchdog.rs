//! 冻结诊断看门狗（观测 + 死锁自救）。
//!
//! 背景：文件栏「正在读取目录…」曾偶发**永久卡死、只能强杀**。静态排查已排除：同步命令钉主
//! 线程（全 `async`）、std 锁跨 `await`、事件洪泛、全局遮罩、自定义运行时——真凶是目录缓存
//! 双锁 ABBA 死锁：`get` 持 map 等 lru，`evict` 持 lru 等 map，同步 lock 在 tokio worker 上
//! 交叉互等，worker 逐个耗尽后运行时冻结、定时器失效、所有超时失效。
//!
//! 双心跳把「到底谁卡住」变成运行时事实，而不是再猜：
//! - `RT_HB`：tokio 定时器心跳。若它停 → **tokio worker 线程被同步阻塞耗尽**（这会让
//!   `tokio::time::timeout` 也失效，正好解释「加了超时仍永久卡」）。
//! - `MAIN_HB`：主线程心跳（经 `run_on_main_thread` 投递，主线程下一次泵事件循环时才执行）。
//!   若它停而 `RT_HB` 仍在跳 → **tao 主线程消息泵被钉**（对应 Alt+F4 失效）。
//!
//! 独立 OS 线程每 2s 比对两心跳，任一落后超阈值即向 `<数据目录>/sailterm-hang.log` 追加一行
//! （每次卡住只在首次记录，恢复后再卡才重新记），并带当时时间戳，便于与复现时刻对齐。
//!
//! 第四层增强：随 `invoke_handler` 登记每条 IPC 命令的「最后派发时刻 + 累计次数」，命中冻结时
//! 把近 15s 内派发过的命令一并 dump 进日志，直接点名疑似钉死源（如 `file_rename`）。注意这不是
//! 严格的「在途集合」——tauri 的 async 命令在 `respond_async` 里 detached 执行，完成点拿不到；
//! 但对「同步阻塞型钉死」的定位足够。
//!
//! 第五层：死锁自救。死锁/worker 耗尽时主线程（消息泵）仍是健康的——投递 `MAIN_HB` 的
//! `run_on_main_thread` 走主队列 dispatch、不经过 tokio，故冻结时心跳照常更新，看门狗能据此
//! 区分「仅 tokio 停」（可自救）与「两者皆停」（主线程真被钉，只能强杀）。前者持续超过
//! `RECOVERY_ASK_MS` 后，经主线程弹 rfd 原生对话框（同步 show 只依赖 run loop，不依赖 tokio），
//! 用户确认后直接拉起新进程退出——不经过 tauri 命令分发（已冻结）和插件对话框（内部 block_on）。

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

/// 心跳落后多少毫秒判定为「卡住」。取 6s：远大于正常一次命令调度，又短于用户强杀前的忍耐。
const STALL_MS: u64 = 6_000;

/// 「仅 tokio 停、主线程健康」持续这么久（ms）后弹恢复对话框。
/// 死锁一旦发生即为永久——15s 足够滤掉瞬时抖动，又远早于用户失去耐心。
const RECOVERY_ASK_MS: u64 = 15_000;

/// tokio 运行时心跳（毫秒时间戳）。
static RT_HB: AtomicU64 = AtomicU64::new(0);
/// 主线程心跳（毫秒时间戳）。
static MAIN_HB: AtomicU64 = AtomicU64::new(0);
/// 边沿触发：同一场卡住只记一次，避免每秒刷日志。
static STALL_LOGGED: AtomicBool = AtomicBool::new(false);
/// 恢复对话框只弹一次（用户点「继续等待」后不骚扰）；tokio 恢复即复位，下次再卡可再弹。
static RECOVERY_ASKED: AtomicBool = AtomicBool::new(false);
/// 每条 IPC 命令的最后派发时刻（ms）与累计派发次数。键为命令名，规模有界（≈注册命令数）。
/// `HashMap::new` 非 const，故经 `OnceLock` 懒初始化（首次 `note_dispatch` 时落地）。
static DISPATCH: OnceLock<Mutex<HashMap<String, (u64, u64)>>> = OnceLock::new();

fn dispatch() -> &'static Mutex<HashMap<String, (u64, u64)>> {
    DISPATCH.get_or_init(|| Mutex::new(HashMap::new()))
}

/// dump 时只保留最后派发距今不超过该毫秒数的命令，过滤掉早已收摊的历史噪音。
const DISPATCH_WINDOW_MS: u64 = 15_000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 登记一条 IPC 命令的派发。由 `invoke_handler` 包装层在每次分发前调用。
/// 临界区仅做一次 map 更新，绝不跨 `await`、绝不含阻塞调用——即便后续进程被钉，
/// 这把锁也不会被持有者带走（持有即释放）。
pub fn note_dispatch(cmd: &str) {
    if let Ok(mut m) = dispatch().lock() {
        let now = now_ms();
        let e = m.entry(cmd.to_string()).or_insert((now, 0));
        e.0 = now;
        e.1 += 1;
    }
}

/// 生成「近 `DISPATCH_WINDOW_MS` 内派发过的命令」清单文本；无则返回空串。
fn dispatch_snapshot(now: u64) -> String {
    let Ok(m) = dispatch().lock() else { return String::new() };
    let mut rows: Vec<(u64, &String, u64)> = m
        .iter()
        .map(|(cmd, &(last, count))| (now.saturating_sub(last), cmd, count))
        .filter(|(age, _, _)| *age <= DISPATCH_WINDOW_MS)
        .collect();
    if rows.is_empty() {
        return String::from("  近窗内无可登记的 IPC 命令派发记录。\n");
    }
    // 距今越近越靠前：冻结瞬间仍在被反复派发/刚派发的高频嫌疑对象优先曝光。
    rows.sort_by_key(|(age, _, _)| *age);
    let mut s = format!(
        "  近 {}s 派发过的 IPC 命令（按最后派发距今排序，非严格在途集合）：\n",
        DISPATCH_WINDOW_MS / 1000
    );
    for (age, cmd, count) in rows {
        s.push_str(&format!("    · {cmd}：最后派发于 {age}ms 前，累计 {count} 次\n"));
    }
    s
}

/// 启动看门狗。在 `setup` 里、`app.manage(AppState)` 之后调用一次即可。
pub fn start(app: &AppHandle, log_dir: &Path) {
    let t0 = now_ms();
    RT_HB.store(t0, Ordering::Relaxed);
    MAIN_HB.store(t0, Ordering::Relaxed);
    let log_path = log_dir.join("sailterm-hang.log");

    // tokio 心跳：只驱动 RT_HB。主线程心跳改由巡检线程直接投递（见下），
    // 使「运行时冻结」与「主线程冻结」两路判定互不耦合。
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            RT_HB.store(now_ms(), Ordering::Relaxed);
        }
    });

    // 独立 OS 线程巡检（不依赖 tokio，故 tokio 被饿死时它仍能记日志、投递主线程心跳、触发自救）。
    let app_c = app.clone();
    std::thread::spawn(move || {
        // 「仅 tokio 停」的起始时刻（0 = 未在停），用于算持续时长、滤掉瞬时抖动。
        let mut rt_stall_since: u64 = 0;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let now = now_ms();
            // 主线程心跳：巡检线程直接投递。run_on_main_thread 走主队列 dispatch、不经过 tokio，
            // 运行时冻结与否不影响投递与执行——这正是死锁自救可依赖主线程的前提。
            if let Some(win) = app_c.get_webview_window("main") {
                let _ = win.run_on_main_thread(|| {
                    MAIN_HB.store(now_ms(), Ordering::Relaxed);
                });
            }
            let rt_stall = now.saturating_sub(RT_HB.load(Ordering::Relaxed));
            let main_stall = now.saturating_sub(MAIN_HB.load(Ordering::Relaxed));
            let stalled = rt_stall > STALL_MS || main_stall > STALL_MS;
            if stalled {
                // 只在进入卡住的瞬间记一次。
                if !STALL_LOGGED.swap(true, Ordering::Relaxed) {
                    let line = format!(
                        "[{}] 疑似冻结：tokio运行时落后 {}ms（>{}ms 即 worker 被同步阻塞/定时器失效），\
                         主线程落后 {}ms（>{}ms 即消息泵被钉→窗口控制失效）。判定：{}\n",
                        now,
                        rt_stall,
                        STALL_MS,
                        main_stall,
                        STALL_MS,
                        classify(rt_stall, main_stall),
                    );
                    append(&log_path, &line);
                    // 冻结定性之后，追加在途/近期派发命令清单，直接点名疑似钉死源。
                    let snap = dispatch_snapshot(now);
                    if !snap.is_empty() {
                        append(&log_path, &snap);
                    }
                }
            } else {
                // 两路都健康：复位边沿，下次再卡会重新记录。
                STALL_LOGGED.store(false, Ordering::Relaxed);
            }

            // 自救：tokio 停但主线程健康（死锁/worker 耗尽的特征）→ 持续超阈后经主线程弹恢复框。
            let tokio_dead = rt_stall > STALL_MS && main_stall <= STALL_MS;
            if tokio_dead {
                if rt_stall_since == 0 {
                    rt_stall_since = now;
                }
                if now.saturating_sub(rt_stall_since) >= RECOVERY_ASK_MS
                    && !RECOVERY_ASKED.swap(true, Ordering::Relaxed)
                {
                    ask_recovery(&app_c);
                }
            } else {
                // 恢复（或主线程也停、弹了也没用）：复位计时与弹窗边沿，下次再卡可重新弹。
                rt_stall_since = 0;
                RECOVERY_ASKED.store(false, Ordering::Relaxed);
            }
        }
    });
}

/// 死锁嫌疑确认：经主线程弹原生对话框，用户确认后拉起新进程并退出。
///
/// 两条路径都刻意绕开 tokio：`run_on_main_thread` 走主队列 dispatch，rfd 同步 `show`
/// 在主线程 runModal（自带嵌套 run loop，照常响应 UI）——冻结时仍可用；而 tauri 命令分发
/// （运行时已冻）和插件对话框（内部 `block_on`）都不可用。重启动作也不走 `app.exit`（其
/// 退出清理依赖运行时），直接 spawn 当前可执行文件后 `exit(0)`。
fn ask_recovery(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.run_on_main_thread(|| {
            let resp = rfd::MessageDialog::new()
                .set_title("SailTerm 响应异常")
                .set_description(
                    "后台任务长时间无响应（运行时疑似冻结）。\n继续等待可能无法恢复，建议立即重启应用。",
                )
                .set_buttons(rfd::MessageButtons::OkCancelCustom(
                    "立即重启".to_string(),
                    "继续等待".to_string(),
                ))
                .show();
            // OkCancelCustom：点第一个按钮（立即重启）返回 Custom(该文案)。
            if !matches!(resp, rfd::MessageDialogResult::Cancel) {
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).spawn();
                }
                std::process::exit(0);
            }
        });
    }
}

fn classify(rt_stall: u64, main_stall: u64) -> &'static str {
    match (rt_stall > STALL_MS, main_stall > STALL_MS) {
        (true, true) => "两者皆停 → 整个进程被钉（主线程 + tokio 同时不动）",
        (true, false) => "仅 tokio 停 → worker 线程被同步阻塞耗尽（超时/定时器一并失效）",
        (false, true) => "仅主线程停 → tao 消息泵被钉（窗口控制失效），tokio 仍在跑",
        (false, false) => "无",
    }
}

/// 追加一行到日志文件；任何 IO 失败都忽略（诊断件绝不因写日志而制造新问题）。
fn append(path: &Path, line: &str) {
    use std::fs::OpenOptions;
    use std::io::Write;
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}
