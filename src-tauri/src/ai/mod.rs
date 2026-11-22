//! AI 编排层：provider 接入（rig）+ 轻量状态机 + 工具 + 脱敏。
//!
//! **架构决策**（经 `probe/ai` spike 实测）：官方 `langchain-rust` 4.6.0
//! 没有图编排能力（无 `StateGraph`/`Checkpointer`/`interrupt`），故编排层改用**自己的轻量
//! 状态机**，provider/模型层用 **rig**。安全核心
//! （Policy/Sanitize/Audit/Executor）内建于本层、不可绕过；凭证与 API Key 永不进模型上下文。
//!
//! 加密后端与 russh/suppaftp 一致，全树只用 **ring**：rig 配成 `default-features=false`
//! + reqwest `rustls-no-provider`，运行期由 [`install_crypto_provider`] 装 ring 全局 provider。

pub mod approval;
pub mod attach;
pub mod context;
pub mod exec;
pub mod knowledge;
pub mod mem0;
pub mod memory;
pub mod orchestrator;
pub mod policy;
pub mod providers;
pub mod question;
pub mod sanitize;
pub mod tools;
pub mod uteke_mem;

/// 把 ring 装成 rustls 的全局默认 crypto provider。
///
/// reqwest 以 `rustls-no-provider` 编译（不带任何 crypto 后端），握手时取这个全局 provider；
/// 必须在建任何 HTTP client（即任何 rig provider client）之前调用一次。`install_default()`
/// 幂等——已被装过则返回 `Err`，忽略即可（全树只编进 ring 一种 provider）。
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
