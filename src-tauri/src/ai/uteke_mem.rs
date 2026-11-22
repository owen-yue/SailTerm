//! uteke-core 记忆引擎（进程内，替代规则抽取 + Mem0 HTTP）。
//!
//! 数据落在应用数据目录 `uteke-mem.db`；查询走语义 + 全文（vecq 后端）。
//! 失败时调用方回落旧的 `ai::memory` 规则记忆。

use std::path::Path;
use std::sync::Mutex;

use uteke_core::Uteke;

use crate::error::AppError;

pub struct UtekeStore {
    inner: Mutex<Uteke>,
}

impl UtekeStore {
    pub fn open(data_dir: &Path) -> Result<Self, AppError> {
        let path = data_dir.join("uteke-mem.db");
        let uteke = Uteke::open(path.to_str().unwrap_or("uteke-mem.db"))
            .map_err(|e| AppError::Ai(format!("打开 uteke 记忆库失败：{e}")))?;
        Ok(Self {
            inner: Mutex::new(uteke),
        })
    }

    /// 写入一条记忆（自动推断类型）；`ns` 为命名空间（如 session id，可空）。
    pub fn remember(
        &self,
        content: &str,
        tags: &[&str],
        ns: Option<&str>,
    ) -> Result<String, AppError> {
        if content.trim().is_empty() {
            return Err(AppError::Invalid("记忆内容不能为空".into()));
        }
        let db = self.inner.lock().unwrap();
        db.remember(content.trim(), tags, None, ns)
            .map_err(|e| AppError::Ai(format!("uteke 记忆写入失败：{e}")))
    }

    /// 语义 + 关键词召回，返回记忆正文。
    pub fn recall(
        &self,
        query: &str,
        limit: usize,
        ns: Option<&str>,
    ) -> Result<Vec<String>, AppError> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let db = self.inner.lock().unwrap();
        let hits = db
            .recall(q, limit.max(1), None, ns, 0.0, None, None)
            .map_err(|e| AppError::Ai(format!("uteke 召回失败：{e}")))?;
        Ok(hits
            .into_iter()
            .filter_map(|h| {
                // SearchResult 字段名以 crate 为准
                let text = h.memory.content;
                if text.trim().is_empty() {
                    None
                } else {
                    Some(text)
                }
            })
            .collect())
    }

    /// 列出（调试/设置页）。
    #[allow(dead_code)]
    pub fn list(&self, limit: usize, ns: Option<&str>) -> Result<Vec<(String, String)>, AppError> {
        let db = self.inner.lock().unwrap();
        let rows = db
            .list(None, limit, 0, ns)
            .map_err(|e| AppError::Ai(format!("uteke 列表失败：{e}")))?;
        Ok(rows.into_iter().map(|m| (m.id, m.content)).collect())
    }

    #[allow(dead_code)]
    pub fn forget(&self, id: &str) -> Result<(), AppError> {
        let db = self.inner.lock().unwrap();
        db.forget(id)
            .map_err(|e| AppError::Ai(format!("uteke 删除失败：{e}")))
    }
}
