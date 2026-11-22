//! 统一 IPC 错误：序列化为 `{ code, message, key, detail }`。
//!
//! - `code`：稳定错误码，供前端逻辑分支（如 `E_HOST_KEY`），不随语言变；
//! - `key`：`err.*` 本地化归类键，前端据此查词典渲染当前语言的类别文案（中文前缀不再烤进后端）；
//! - `detail`：不可翻译的动态细节（原始技术串 / 端点自报文本），拼在归类之后；
//! - `message`：中文兜底整串（词典未命中 / 旧版客户端 / 日志）。
//!
//! 后端绝不泄漏凭证；归类文案交给前端词典，故界面语言切换能覆盖全站后端错误。

use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("SSH 连接失败：{0}")]
    Ssh(String),
    #[error("认证失败：{0}")]
    Auth(String),
    #[error("会话不存在或已关闭：{0}")]
    NoSuchSession(String),
    #[error("连接配置不存在：{0}")]
    ProfileNotFound(String),
    #[error("私钥不可用：{0}")]
    Key(String),
    #[error("主机密钥校验未通过：{0}")]
    HostKey(String),
    #[error("文件操作失败：{0}")]
    Sftp(String),
    #[error("FTP 连接失败：{0}")]
    Ftp(String),
    #[error("传输任务异常：{0}")]
    Transfer(String),
    /// 传输任务行不存在（本地视图有、后端 DB 已无的「幽灵行」）。必须是独立变体：
    /// 前端据稳定 `code` 摘掉本地行，不能靠中文文案子串匹配（文案一改，清理出路就静默失效）。
    #[error("传输任务不存在：{0}")]
    NoSuchTask(String),
    #[error("AI 编排失败：{0}")]
    Ai(String),
    /// 在已连会话上远程执行命令的采样/操作失败（与 AI 无关：进程列表、监控等只读采样走
    /// 独立 exec 通道，失败属「远程命令执行」类，不应被归为 `Ai`「AI 编排失败」）。
    #[error("远程命令执行失败：{0}")]
    Exec(String),
    /// AI provider 调用失败（网络 / 鉴权 / 限流 / 端点错误等）：`key` 是 `err.ai.*` 归类稳定
    /// i18n 键（前端据此渲染当前语言的类别文案），`detail` 是不可翻译的动态部分（HTTP 状态 +
    /// 端点自报原文）。与通用 [`AppError::Ai`]（编排内部错误）分开，使 provider 归类能按 HTTP 语义细分。
    #[error("{detail}")]
    AiProvider { key: &'static str, detail: String },
    #[error("AI Provider 配置错误：{0}")]
    Provider(String),
    #[error("本地数据库错误：{0}")]
    Db(String),
    #[error("凭据保险库错误：{0}")]
    Vault(String),
    #[error("文件读写失败：{0}")]
    Io(String),
    #[error("参数不合法：{0}")]
    Invalid(String),
    /// 名称冲突一类的正常业务结果：不加「XX失败：」前缀，message 即最终文案。
    #[error("{0}")]
    Conflict(String),
    /// 面向普通用户、需前端本地化的高频错误：`key` 是稳定的 i18n 键（同时作为 IPC `code`
    /// 下发，前端据此查 `err.*` 词典渲染当前语言）；`zh` 是中文兜底文案（前端无该词条/旧版
    /// 时原样显示，也用于日志）。
    #[error("{zh}")]
    Biz { key: &'static str, zh: &'static str },
    #[error("已终止")]
    Canceled,
    #[error("内部错误：{0}")]
    Internal(String),
}

impl AppError {
    /// 稳定错误码（前端按 code 映射文案；也供命令层内部判定，如扫描取消）。
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Ssh(_) => "E_SSH",
            Self::Auth(_) => "E_AUTH",
            Self::NoSuchSession(_) => "E_NO_SESSION",
            Self::ProfileNotFound(_) => "E_PROFILE",
            Self::Key(_) => "E_KEY",
            Self::HostKey(_) => "E_HOST_KEY",
            Self::Sftp(_) => "E_SFTP",
            Self::Ftp(_) => "E_FTP",
            Self::Transfer(_) => "E_TRANSFER",
            Self::NoSuchTask(_) => "E_TRANSFER_NOT_FOUND",
            Self::Ai(_) => "E_AI",
            Self::Exec(_) => "E_EXEC",
            Self::AiProvider { .. } => "E_AI",
            Self::Provider(_) => "E_PROVIDER",
            Self::Db(_) => "E_DB",
            Self::Vault(_) => "E_VAULT",
            Self::Io(_) => "E_IO",
            Self::Invalid(_) => "E_INVALID",
            Self::Conflict(_) => "E_CONFLICT",
            Self::Biz { key, .. } => key,
            Self::Canceled => "E_CANCELED",
            Self::Internal(_) => "E_INTERNAL",
        }
    }

    /// 面向本地化的归类键：前端据此查 `err.*` 词典渲染当前语言的「类别」文案。与 [`code`](Self::code)
    /// 分离——`code` 供逻辑分支（稳定、不随语言变），`key` 供文案渲染（各语言词典）。把「中文前缀」
    /// 从 Display 里抽出来交给前端，从而【一处改动即让全站后端错误的类别文案跟随界面语言】。
    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::Ssh(_) => "err.ssh",
            Self::Auth(_) => "err.sshAuth",
            Self::NoSuchSession(_) => "err.noSession",
            Self::ProfileNotFound(_) => "err.profile",
            Self::Key(_) => "err.key",
            Self::HostKey(_) => "err.hostKey",
            Self::Sftp(_) => "err.sftp",
            Self::Ftp(_) => "err.ftp",
            Self::Transfer(_) => "err.transfer",
            Self::NoSuchTask(_) => "err.transfer.notFound",
            Self::Ai(_) => "err.ai",
            Self::Exec(_) => "err.exec",
            Self::AiProvider { key, .. } => key,
            Self::Provider(_) => "err.provider",
            Self::Db(_) => "err.db",
            Self::Vault(_) => "err.vault",
            Self::Io(_) => "err.io",
            Self::Invalid(_) => "err.invalid",
            Self::Conflict(_) => "err.conflict",
            Self::Biz { key, .. } => key,
            Self::Canceled => "err.canceled",
            Self::Internal(_) => "err.internal",
        }
    }

    /// 不可翻译的动态细节（原始技术串 / 端点自报文本）。前端把它拼在本地化归类之后；`Biz` /
    /// `Conflict` / `Canceled` 这类「文案即全部信息」的错误无独立细节 → `None`。
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Ssh(s)
            | Self::Auth(s)
            | Self::NoSuchSession(s)
            | Self::ProfileNotFound(s)
            | Self::Key(s)
            | Self::HostKey(s)
            | Self::Sftp(s)
            | Self::Ftp(s)
            | Self::Transfer(s)
            | Self::NoSuchTask(s)
            | Self::Ai(s)
            | Self::Exec(s)
            | Self::Provider(s)
            | Self::Db(s)
            | Self::Vault(s)
            | Self::Io(s)
            | Self::Invalid(s)
            | Self::Internal(s) => Some(s),
            Self::AiProvider { detail, .. } => Some(detail),
            Self::Conflict(_) | Self::Biz { .. } | Self::Canceled => None,
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // IPC 形态 `{ code, message, key, detail }`：
        // - `code` 稳定错误码（前端分支用）；`message` 中文兜底整串（词典未命中 / 旧版 / 日志）；
        // - `key` = `err.*` 本地化归类键；`detail` = 不可翻译动态部分（可为 null）。
        let mut st = s.serialize_struct("AppError", 4)?;
        st.serialize_field("code", self.code())?;
        st.serialize_field("message", &self.to_string())?;
        st.serialize_field("key", self.i18n_key())?;
        st.serialize_field("detail", &self.detail())?;
        st.end()
    }
}

/// russh 的连接/通道错误统一归入 `Ssh`。
impl From<russh::Error> for AppError {
    fn from(e: russh::Error) -> Self {
        Self::Ssh(e.to_string())
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<russh::keys::ssh_key::Error> for AppError {
    fn from(e: russh::keys::ssh_key::Error) -> Self {
        Self::Key(e.to_string())
    }
}

impl From<keyring::Error> for AppError {
    fn from(e: keyring::Error) -> Self {
        Self::Vault(e.to_string())
    }
}
