//! 远端目录列表缓存：秒开体验的关键，**不承担一致性保证**。
//!
//! 设计口径：
//! - TTL 20s：过期后自然回源，无需显式失效即可容忍外部改动（别人在服务器上 mv 一个文件，
//!   20s 后本端也能看到）；这个数字是"手感"与"陈旧度"的折中，不做配置项。
//! - 写类操作主动 invalidate：upload / mkdir / remove / rename / chmod / write_text
//!   成功分支清 path 与 parent 两条（重命名/删除都会改父目录列表）。
//! - 会话关闭整体 forget：`session.rs::teardown` 调 `forget_session(sid)`。
//! - 上限 500 目录：FIFO 淘汰（不做严格 LRU——TTL 已经很短，LRU 的额外命中率收益不值
//!   那点实现复杂度；`touch` 只维护 push 顺序，`evict` 从队首 pop）。
//! - 预取：file_list 成功后，对返回 entries 里的目录（≤32 个）后台 spawn 预列，
//!   `Semaphore(4)` 限流；预取失败静默忽略（用户可能没权限进那个目录，很正常）。
//!
//! 安全红线：缓存只存路径与元数据（`FileEntry`），绝不缓存文件内容；跨会话以
//! `session_id` 前缀隔离，A 会话的 path 命中不了 B 会话。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::FileEntry;

/// 目录条目的新鲜度窗口。
const TTL: Duration = Duration::from_secs(20);
/// 缓存目录数上限；超出 FIFO 淘汰队首。
const MAX_DIRS: usize = 500;
/// 单次预取最多触发的子目录数（一屏可见范围）。
const PREFETCH_LIMIT: usize = 32;
/// 预取并发上限，避免一次列目录把 SSH 通道打满。
pub const PREFETCH_CONCURRENCY: usize = 4;

#[derive(Clone)]
struct Slot {
    entries: Vec<FileEntry>,
    at: Instant,
}

/// map + FIFO 队列共用**同一把**锁，一次持锁改完两者。
///
/// 拆成两把锁会让 `evict` 与 `get`/`invalidate` 的加锁顺序相反，形成 ABBA 死锁；
/// 一旦卡死 `map` 被永占，`file_list` 的同步 `get` 在 tokio worker 上永久阻塞，
/// 文件栏卡「正在读取目录…」、worker 逐个耗尽后整进程冻结。
/// 单锁后不存在交叉等待，临界区全为 O(小常数) 操作，串行化开销可忽略。
#[derive(Default)]
struct Inner {
    /// key = `"{sid}\u{1}{canonical_path}"`；分隔符 0x01 不会出现在合法路径里。
    map: HashMap<String, Slot>,
    /// FIFO 队列：put 时 push_back；超限时 pop_front 淘汰。
    lru: VecDeque<String>,
}

#[derive(Default)]
pub struct DirCache {
    inner: Mutex<Inner>,
}

impl DirCache {
    fn key(sid: &str, path: &str) -> String {
        format!("{sid}\u{1}{path}")
    }

    /// 尝试拿锁。拿不到直接返回 None——缓存是「尽力而为」的加速器，
    /// **绝不阻塞调用方线程**：过去的卡死正是同步锁等待发生在 tokio worker 上，
    /// 一旦持锁者被钉住，worker 逐个耗尽、整进程冻结。try_lock 把「等待」降级成
    /// 「本次放弃缓存」：get 当 miss（多一次 SFTP 往返）、put/invalidate 丢弃（TTL 自愈）。
    fn lock(&self) -> Option<std::sync::MutexGuard<'_, Inner>> {
        self.inner.try_lock().ok()
    }

    /// 命中且未过期返回条目拷贝；过期即顺手删除并回 None。
    pub fn get(&self, sid: &str, path: &str) -> Option<Vec<FileEntry>> {
        let k = Self::key(sid, path);
        let mut inner = self.lock()?;
        if let Some(s) = inner.map.get(&k) {
            if s.at.elapsed() < TTL {
                return Some(s.entries.clone());
            }
            inner.map.remove(&k);
            if let Some(pos) = inner.lru.iter().position(|x| x == &k) {
                inner.lru.remove(pos);
            }
        }
        None
    }

    /// 写入/覆盖一条；顺带触发一次容量淘汰。拿不到锁则整条丢弃（下次回源）。
    pub fn put(&self, sid: &str, path: &str, entries: Vec<FileEntry>) {
        let k = Self::key(sid, path);
        let Some(mut inner) = self.lock() else { return };
        inner.map.insert(k.clone(), Slot { entries, at: Instant::now() });
        if let Some(pos) = inner.lru.iter().position(|x| x == &k) {
            inner.lru.remove(pos);
        }
        inner.lru.push_back(k);
        while inner.lru.len() > MAX_DIRS {
            if let Some(old) = inner.lru.pop_front() {
                inner.map.remove(&old);
            } else {
                break;
            }
        }
    }

    /// 主动失效：路径自身 + 父目录各清一条。拿不到锁则跳过（TTL 20s 后自愈）。
    /// 上传/写入改父；重命名改两侧父；删除改父——统一"父 + 自身"两条最保险。
    pub fn invalidate(&self, sid: &str, path: &str) {
        let parent = parent_of(path);
        let Some(mut inner) = self.lock() else { return };
        for p in [path, parent.as_str()] {
            let k = Self::key(sid, p);
            inner.map.remove(&k);
            if let Some(pos) = inner.lru.iter().position(|x| x == &k) {
                inner.lru.remove(pos);
            }
        }
    }

    /// 会话结束整体清理：所有 key 前缀匹配 `{sid}\u{1}` 的都删。
    /// 拿不到锁则跳过：残留条目 20s TTL 后自行过期，不影响正确性。
    pub fn forget_session(&self, sid: &str) {
        let prefix = format!("{sid}\u{1}");
        let Some(mut inner) = self.lock() else { return };
        inner.map.retain(|k, _| !k.starts_with(&prefix));
        inner.lru.retain(|k| !k.starts_with(&prefix));
    }
}

/// 全局单例：Tauri 命令层直接 `cache::global()` 拿。
pub fn global() -> &'static DirCache {
    static C: std::sync::OnceLock<DirCache> = std::sync::OnceLock::new();
    C.get_or_init(DirCache::default)
}

/// 取远端路径的父目录；根 `/` 或首段路径无父时回 `/` 或空串（与 `remote_parent` 语义一致，
/// 但缓存失效只关心"是不是同一个 key"，父算错最多导致少清一条、TTL 20s 后自愈）。
fn parent_of(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_string();
    }
    match trimmed.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
        None => "/".to_string(),
    }
}

/// 供命令层过滤"值得预取"的目录条目：只取目录、跳过 `.`/`..`（后端已过滤但双保险）、上限 32。
pub fn prefetch_targets(entries: &[FileEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.is_dir && e.name != "." && e.name != "..")
        .take(PREFETCH_LIMIT)
        .map(|e| e.path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, path: &str, is_dir: bool) -> FileEntry {
        FileEntry {
            name: name.into(),
            path: path.into(),
            is_dir,
            is_symlink: false,
            size: 0,
            modified_ms: None,
            mode: None,
            perms: None,
            uid: None,
            gid: None,
            user: None,
            group: None,
        }
    }

    #[test]
    fn put_then_get_hits() {
        let c = DirCache::default();
        c.put("s1", "/tmp", vec![entry("a", "/tmp/a", false)]);
        assert!(c.get("s1", "/tmp").is_some());
    }

    #[test]
    fn different_session_ids_are_isolated() {
        let c = DirCache::default();
        c.put("s1", "/tmp", vec![]);
        assert!(c.get("s1", "/tmp").is_some());
        assert!(c.get("s2", "/tmp").is_none());
    }

    #[test]
    fn invalidate_clears_self_and_parent() {
        let c = DirCache::default();
        c.put("s1", "/var/log", vec![]);
        c.put("s1", "/var", vec![]);
        c.put("s1", "/etc", vec![]);
        c.invalidate("s1", "/var/log");
        assert!(c.get("s1", "/var/log").is_none());
        assert!(c.get("s1", "/var").is_none());
        assert!(c.get("s1", "/etc").is_some(), "unrelated entry must survive");
    }

    #[test]
    fn forget_session_drops_all_under_sid() {
        let c = DirCache::default();
        c.put("s1", "/a", vec![]);
        c.put("s1", "/b", vec![]);
        c.put("s2", "/a", vec![]);
        c.forget_session("s1");
        assert!(c.get("s1", "/a").is_none());
        assert!(c.get("s1", "/b").is_none());
        assert!(c.get("s2", "/a").is_some());
    }

    #[test]
    fn expired_entry_returns_none() {
        let c = DirCache::default();
        c.put("s1", "/tmp", vec![]);
        // 手动把 at 拨回 TTL+1 秒前，模拟过期
        {
            let mut inner = c.inner.lock().unwrap();
            if let Some(s) = inner.map.get_mut(&DirCache::key("s1", "/tmp")) {
                s.at = Instant::now() - TTL - Duration::from_secs(1);
            }
        }
        assert!(c.get("s1", "/tmp").is_none());
    }

    #[test]
    fn fifo_evicts_oldest_when_over_capacity() {
        let c = DirCache::default();
        for i in 0..=MAX_DIRS {
            c.put("s1", &format!("/p{i}"), vec![]);
        }
        // 第 501 次插入应把队首 /p0 淘汰；后续仍在
        assert!(c.get("s1", "/p0").is_none());
        assert!(c.get("s1", &format!("/p{MAX_DIRS}")).is_some());
    }

    /// 回归看门：旧实现（map/lru 双锁）下 `get` 过期分支与 `put→evict` 锁序相反，
    /// 多线程交叉会 ABBA 死锁 → `file_list` 永久卡「正在读取目录」。
    /// 这里填满容量 + 全部置过期 + 8 线程交叉 get/put/invalidate，5s 未收摊即判死锁。
    #[test]
    fn concurrent_get_put_invalidate_does_not_deadlock() {
        use std::sync::mpsc;
        use std::sync::Arc;
        use std::time::Duration as StdDuration;

        let c = Arc::new(DirCache::default());
        for i in 0..MAX_DIRS {
            c.put("s", &format!("/pre{i}"), vec![]);
        }
        {
            let mut inner = c.inner.lock().unwrap();
            for s in inner.map.values_mut() {
                s.at = Instant::now() - TTL - Duration::from_secs(1);
            }
        }

        let (done_tx, done_rx) = mpsc::channel();
        let mut handles = Vec::new();
        for t in 0..8u32 {
            let c = c.clone();
            let done_tx = done_tx.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..2000 {
                    if t % 2 == 0 {
                        // 写侧：持续触发容量淘汰
                        c.put("s", &format!("/n{t}-{i}"), vec![]);
                    } else {
                        // 读侧：命中过期的条目，走进「删 map 改 lru」分支
                        let _ = c.get("s", &format!("/pre{}", i % MAX_DIRS));
                        c.invalidate("s", &format!("/n{t}-{i}"));
                    }
                }
                drop(done_tx);
            }));
        }
        drop(done_tx);
        // 有线程收摊时继续等其余；全部 sender 销毁（Disconnected）才退出；反复超时（Timeout）计次判死锁。
        let mut ticks = 0;
        loop {
            match done_rx.recv_timeout(StdDuration::from_millis(500)) {
                Ok(()) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    ticks += 1;
                    assert!(ticks < 10, "疑似死锁：并发 get/put/invalidate 8 线程 5s 未收摊");
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        for h in handles {
            h.join().expect("工作线程 panic");
        }
    }

    #[test]
    fn parent_of_handles_root_and_relative() {
        assert_eq!(parent_of("/var/log"), "/var");
        assert_eq!(parent_of("/var"), "/");
        assert_eq!(parent_of("/var/"), "/");
        assert_eq!(parent_of("/"), "/");
        // 相对路径取首段：父算错最多少清一条，TTL 20s 后自愈（见 parent_of 注释）
        assert_eq!(parent_of("relative/path"), "relative");
    }

    #[test]
    fn prefetch_targets_only_picks_dirs_capped() {
        let mut entries = vec![entry("f", "/x/f", false)];
        for i in 0..50 {
            entries.push(entry(&format!("d{i}"), &format!("/x/d{i}"), true));
        }
        let t = prefetch_targets(&entries);
        assert_eq!(t.len(), PREFETCH_LIMIT);
        assert!(t.iter().all(|p| p.starts_with("/x/d")));
    }
}
