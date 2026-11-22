//! 插件系统：manifest 声明式能力 + WASM 沙箱 + 能力网关 + 按 id 隔离存储。
//!
//! 安全边界：
//! - WASM 在 wasmi 解释器沙箱内跑，带 fuel 限额，无凭证；网络/会话写/字节桥走能力网关；
//! - 所有出沙箱操作走 [`host`] 能力网关，按 manifest `permissions` 放行；
//! - 会话命令经 [`crate::ai::policy`] 只读裁决；凭证永远不经插件。
//!
//! 两种形态：
//! 1. **命令包**（无 wasm）：`commands[]` 模板渲染后在会话上执行；
//! 2. **逻辑插件**（有 wasm）：导出 `on_output` / `on_command` 等钩子。

pub mod host;
pub mod manifest;
pub mod registry;

pub use manifest::{Permission, PluginSettingSpec};
pub use registry::PluginRegistry;
