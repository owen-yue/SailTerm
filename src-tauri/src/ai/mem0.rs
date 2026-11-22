//! Mem0 框架接入（HTTP）：抽取 + 语义召回。
//!
//! 本地自托管示例：
//! `docker run -p 8000:8000 mem0oss/mem0-rest-api`
//! 或官方平台 `https://api.mem0.ai`（需 API Key）。
//!
//! 未配置 URL 时完全走本库规则记忆（`ai::memory`），互不影响。

use serde::Deserialize;
use serde_json::json;

use crate::error::AppError;

#[derive(Clone, Debug)]
pub struct Mem0Client {
    pub base_url: String,
    pub api_key: Option<String>,
    /// 用户命名空间（多用户隔离；桌面单人可固定 `default`）。
    pub user_id: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Mem0Hit {
    #[serde(default)]
    #[allow(dead_code)]
    pub id: String,
    #[serde(default)]
    pub memory: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

impl Mem0Client {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        user_id: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.filter(|s| !s.trim().is_empty()),
            user_id: user_id.into(),
        }
    }

    fn http(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .unwrap_or_default()
    }

    fn auth(&self, b: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(k) => b.bearer_auth(k),
            None => b,
        }
    }

    /// 把用户消息交给 Mem0 做抽取入库（框架内部 LLM 抽事实）。
    pub async fn add(&self, session_id: &str, text: &str) -> Result<(), AppError> {
        let url = format!("{}/v1/memories/", self.base_url);
        let body = json!({
            "user_id": self.user_id,
            "agent_id": session_id,
            "messages": [{ "role": "user", "content": text }],
        });
        let resp = self
            .auth(self.http().post(&url).json(&body))
            .send()
            .await
            .map_err(|e| AppError::Ai(format!("Mem0 add 失败：{e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Ai(format!("Mem0 add HTTP {}", resp.status())));
        }
        Ok(())
    }

    /// 语义召回；返回记忆文本。
    pub async fn search(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<String>, AppError> {
        let url = format!("{}/v1/memories/search/", self.base_url);
        let body = json!({
            "user_id": self.user_id,
            "agent_id": session_id,
            "query": query,
            "limit": limit,
        });
        let resp = self
            .auth(self.http().post(&url).json(&body))
            .send()
            .await
            .map_err(|e| AppError::Ai(format!("Mem0 search 失败：{e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::Ai(format!("Mem0 search HTTP {}", resp.status())));
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum SearchBody {
            List(Vec<Mem0Hit>),
            Wrapped { results: Vec<Mem0Hit> },
            Memories(Vec<Mem0Hit>),
        }
        let body: SearchBody = resp
            .json()
            .await
            .map_err(|e| AppError::Ai(format!("Mem0 响应解析失败：{e}")))?;
        let hits = match body {
            SearchBody::List(v) | SearchBody::Memories(v) => v,
            SearchBody::Wrapped { results } => results,
        };
        Ok(hits
            .into_iter()
            .filter_map(|h| h.memory.or(h.text))
            .filter(|s| !s.trim().is_empty())
            .take(limit)
            .collect())
    }
}

/// 从设置解析客户端；未配置 URL 返回 `None`。
pub fn from_settings(base_url: &str, api_key: &str, user_id: &str) -> Option<Mem0Client> {
    let url = base_url.trim();
    if url.is_empty() {
        return None;
    }
    let uid = if user_id.trim().is_empty() {
        "default".to_string()
    } else {
        user_id.trim().to_string()
    };
    Some(Mem0Client::new(url, Some(api_key.to_string()), uid))
}
