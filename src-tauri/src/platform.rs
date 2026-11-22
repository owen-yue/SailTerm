//! 跨平台 leaf 差异的单一收敛点（后端侧，与前端 `lib/platform.ts` 对称）。
//!
//! 只放**无状态的纯映射**：按目标平台选可执行名 / argv / 子进程创建标志。业务命令文件
//! （如 `commands/net.rs`）因此保持平台无关的可读逻辑，不再散落 `if cfg!(...)`；加平台只改这里。
//!
//! 属系统级设施：不依赖 db / vault / session，可随 `commands/net.rs` 一并摘出（见其文件头
//! 「可分离纪律」）。注意与 `PortLister`（策略 + 编译期工厂）的分工：那边的差异是「采集机制」
//! 本身多态（不同命令 + 不同解析），故用 trait；这边只是「给定语义 → 该平台 argv/程序名」的
//! 一次性映射，用普通函数最贴切，不为了统一形式而硬套 trait。

use tokio::process::Command;

/// Windows：给子进程挂 `CREATE_NO_WINDOW`，避免 GUI 进程 spawn 控制台程序闪黑框；其余平台空操作。
/// （`creation_flags` 是 tokio::process::Command 在 Windows 上的原生方法，无需额外 trait。）
pub fn suppress_console_window(cmd: &mut Command) {
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    #[cfg(not(windows))]
    let _ = cmd;
}

/// 系统 `ping` 的 argv（不含程序名，host 追加末尾）：
/// - Windows：`-n`；
/// - macOS/BSD：只 `-c`，**不加 `-W`**——实测 `-W` 会把按时到达的应答也判成 "out of wait time"
///   而不逐行回显、甚至虚报丢包（`-c 4 -W 2 8.8.8.8` 谎报 25% loss），去掉 `-W` 即正常；
/// - Linux/其他 Unix：`-c -W 2` 限制等待，避免静默主机长时间挂起。
pub fn ping_args(count: u32, host: &str) -> Vec<String> {
    if cfg!(windows) {
        vec!["-n".into(), count.to_string(), host.into()]
    } else if cfg!(target_os = "macos") {
        vec!["-c".into(), count.to_string(), host.into()]
    } else {
        vec![
            "-c".into(),
            count.to_string(),
            "-W".into(),
            "2".into(),
            host.into(),
        ]
    }
}

/// 路由追踪的（可执行名, argv）：Windows `tracert -h <max> -d`；其余 `traceroute -m <max> -q 1`
/// （`-q 1` 每跳单查询、`-d` 禁 DNS 反解，令逐跳解析 locale 无关）。
pub fn traceroute_cmd(max_hops: u32, host: &str) -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        (
            "tracert",
            vec!["-h".into(), max_hops.to_string(), "-d".into(), host.into()],
        )
    } else {
        (
            "traceroute",
            vec![
                "-m".into(),
                max_hops.to_string(),
                "-q".into(),
                "1".into(),
                host.into(),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_args_put_host_last_and_respect_count() {
        let a = ping_args(4, "10.0.0.1");
        assert_eq!(a.last().map(String::as_str), Some("10.0.0.1"));
        assert!(a.iter().any(|x| x == "4"));
        // mac 不加 -W（有坑）；Windows 用 -n 而非 -c
        if cfg!(target_os = "macos") {
            assert!(!a.contains(&"-W".to_string()));
            assert!(a.contains(&"-c".to_string()));
        } else if cfg!(windows) {
            assert!(a.contains(&"-n".to_string()));
        } else {
            assert!(a.contains(&"-W".to_string()));
        }
    }

    #[test]
    fn traceroute_cmd_picks_program_per_platform() {
        let (prog, args) = traceroute_cmd(20, "example.com");
        assert_eq!(
            prog,
            if cfg!(windows) {
                "tracert"
            } else {
                "traceroute"
            }
        );
        assert_eq!(args.last().map(String::as_str), Some("example.com"));
        assert!(args.iter().any(|x| x == "20"));
    }
}
