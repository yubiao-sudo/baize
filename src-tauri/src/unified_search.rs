//! 统一搜索（一句话同时搜多源，不用分开查）
//!
//! 聚合的来源（sources 可选，缺省全部）：
//! - files     本地文件内容（工作空间/指定目录实时遍历，正则字面匹配）
//! - knowledge 知识库 RAG 索引（语义检索，未索引则自动为空）
//! - messages  白泽对话记录（跨会话全文检索）
//! - memories  工作记忆（remember 记下的偏好/事实）
//! - events    情景记忆（发生过的操作/决策事件）
//! - semantic  语义记忆（画像/项目知识）
//! - im        IM 消息总线日志（微信/飞书收发记录，内存环形缓冲）
//! - mail      邮件（复用 mail_fetch 的 IMAP 实现，需 vault 里配好 mail: 凭据）
//!
//! 设计原则：任何一个来源失败/未配置都不影响其它来源，返回里用 error 字段说明原因。

use std::sync::Arc;

use serde_json::{json, Value};

use crate::memory::MemoryStore;
use crate::tools::{decode_console_output, resolve_path, PermissionClass, Tool};

const DEFAULT_PER_SOURCE: usize = 5;

/// 命中行裁剪：太长的行截断到 ~400 字符
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let mut t: String = s.chars().take(n).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

/// 本地文件内容搜索：遍历目录，把包含关键词（忽略大小写）的行收集起来
fn search_files(root: &str, query: &str, max: usize) -> Result<Vec<Value>, String> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let path = resolve_path(root);
    let meta = std::fs::metadata(&path).map_err(|e| format!("路径不可访问: {e}"))?;
    let lower_q = query.to_lowercase();
    let mut results: Vec<Value> = Vec::new();

    let walk_root = if meta.is_file() {
        vec![std::path::PathBuf::from(&path)]
    } else {
        walkdir::WalkDir::new(&path)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .collect()
    };

    for file in walk_root {
        if results.len() >= max {
            break;
        }
        // 跳过明显的大文件与常见二进制扩展名
        let ext = file
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if matches!(
            ext.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "pdf" | "docx" | "xlsx" | "pptx"
                | "zip" | "7z" | "rar" | "exe" | "dll" | "so" | "dylib" | "bin" | "mp4"
                | "mp3" | "wav" | "flac" | "woff" | "woff2" | "ttf" | "db" | "sqlite"
        ) {
            continue;
        }
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes.len() > 4 * 1024 * 1024 {
            continue; // >4MB 按二进制/超大文件跳过
        }
        let content = decode_console_output(&bytes);
        let total_chars = content.chars().count().max(1);
        if content.matches('\u{FFFD}').count() * 10 > total_chars {
            continue; // 二进制文件
        }
        for (idx, line) in content.lines().enumerate() {
            if results.len() >= max {
                break;
            }
            if line.to_lowercase().contains(&lower_q) {
                results.push(json!({
                    "path": file.display().to_string(),
                    "line": idx + 1,
                    "text": clip(line, 400),
                }));
            }
        }
    }
    Ok(results)
}

pub struct UnifiedSearchTool {
    store: Arc<MemoryStore>,
}

impl UnifiedSearchTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for UnifiedSearchTool {
    fn name(&self) -> &str {
        "unified_search"
    }
    fn description(&self) -> &str {
        "跨应用统一搜索：一句话同时搜本地文件、知识库、对话记录、工作/情景/语义记忆、IM消息、邮件。\
         sources 可选（files/knowledge/messages/memories/events/semantic/im/mail），缺省全部；\
         path 可指定文件搜索根目录（缺省工作空间）。任一来源失败不影响其它来源"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "搜索关键词或短语" },
                "sources": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "要搜索的来源列表，缺省全部：files/knowledge/messages/memories/events/semantic/im/mail"
                },
                "path": { "type": "string", "description": "文件搜索根目录（缺省工作空间）" },
                "max_per_source": { "type": "integer", "description": "每个来源最多返回条数，默认 5" }
            },
            "required": ["query"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let query = args["query"]
            .as_str()
            .ok_or("缺少参数 query")?
            .trim()
            .to_string();
        if query.is_empty() {
            return Err("query 不能为空".into());
        }
        let max = args["max_per_source"]
            .as_u64()
            .unwrap_or(DEFAULT_PER_SOURCE as u64)
            .clamp(1, 50) as usize;
        let default_sources = [
            "files",
            "knowledge",
            "messages",
            "memories",
            "events",
            "semantic",
            "im",
            "mail",
        ];
        let wanted: Vec<String> = match args["sources"].as_array() {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                .filter(|s| default_sources.contains(&s.as_str()))
                .collect(),
            None => default_sources.iter().map(|s| s.to_string()).collect(),
        };
        let file_root = args["path"].as_str().unwrap_or(".").to_string();
        let want = |s: &str| wanted.iter().any(|x| x == s);

        let mut out: Value = json!({ "query": query, "sources": {} });

        // 1) 本地文件
        if want("files") {
            match search_files(&file_root, &query, max) {
                Ok(hits) => {
                    out["sources"]["files"] = json!({ "count": hits.len(), "hits": hits });
                }
                Err(e) => {
                    out["sources"]["files"] = json!({ "count": 0, "error": e });
                }
            }
        }

        // 2) 知识库 RAG
        if want("knowledge") {
            // RagIndex 在 AppState 里，工具层无法直接拿到；通过全局注册表桥接（lib 启动时注入）
            match crate::unified_search::rag_bridge_lookup() {
                Some(rag) => {
                    let hits = rag.search(&query, max);
                    out["sources"]["knowledge"] = json!({ "count": hits.len(), "hits": hits });
                }
                None => {
                    out["sources"]["knowledge"] =
                        json!({ "count": 0, "error": "知识库未就绪" });
                }
            }
        }

        // 3) 对话记录
        if want("messages") {
            match self.store.search_messages(&query, max) {
                Ok(rows) => {
                    let hits: Vec<Value> = rows
                        .iter()
                        .map(|(conv, title, role, content, ts)| {
                            json!({
                                "conversation": title,
                                "role": role,
                                "time": ts,
                                "text": clip(content, 400),
                                "conversation_id": conv,
                            })
                        })
                        .collect();
                    out["sources"]["messages"] = json!({ "count": hits.len(), "hits": hits });
                }
                Err(e) => {
                    out["sources"]["messages"] = json!({ "count": 0, "error": e });
                }
            }
        }

        // 4) 工作记忆
        if want("memories") {
            match self.store.recall(&query, max) {
                Ok(rows) => {
                    let hits: Vec<Value> = rows
                        .iter()
                        .map(|m| {
                            json!({
                                "content": m.content,
                                "kind": m.kind,
                                "salience": m.salience,
                            })
                        })
                        .collect();
                    out["sources"]["memories"] = json!({ "count": hits.len(), "hits": hits });
                }
                Err(e) => {
                    out["sources"]["memories"] = json!({ "count": 0, "error": e });
                }
            }
        }

        // 5) 情景记忆
        if want("events") {
            let rows = self.store.recall_events(&query, max).unwrap_or_default();
            let hits: Vec<Value> = rows
                .iter()
                .map(|e| {
                    json!({
                        "time": e.ts,
                        "type": e.event_type,
                        "summary": e.summary,
                    })
                })
                .collect();
            out["sources"]["events"] = json!({ "count": hits.len(), "hits": hits });
        }

        // 6) 语义记忆
        if want("semantic") {
            let rows = self.store.recall_semantic(&query, max).unwrap_or_default();
            let hits: Vec<Value> = rows
                .iter()
                .map(|s| {
                    json!({
                        "category": s.category,
                        "content": s.content,
                        "confidence": s.confidence,
                    })
                })
                .collect();
            out["sources"]["semantic"] = json!({ "count": hits.len(), "hits": hits });
        }

        // 7) IM 消息总线日志（内存环形缓冲）
        if want("im") {
            match crate::unified_search::im_log_bridge_lookup() {
                Some(entries) => {
                    let lower_q = query.to_lowercase();
                    let hits: Vec<Value> = entries
                        .into_iter()
                        .filter(|e| {
                            e["text"]
                                .as_str()
                                .map(|t| t.to_lowercase().contains(&lower_q))
                                .unwrap_or(false)
                        })
                        .take(max)
                        .collect();
                    out["sources"]["im"] = json!({ "count": hits.len(), "hits": hits });
                }
                None => {
                    out["sources"]["im"] = json!({ "count": 0, "error": "IM 日志未就绪" });
                }
            }
        }

        // 8) 邮件（需要 vault 里配好 IMAP 凭据；超时/无配置时静默跳过）
        if want("mail") {
            match crate::tools::fetch_mails_for_unified_search(&self.store, &query, max) {
                Some(mails) => {
                    out["sources"]["mail"] = json!({ "count": mails.len(), "hits": mails });
                }
                None => {
                    out["sources"]["mail"] =
                        json!({ "count": 0, "error": "未配置邮件凭据或无命中（vault_set 存 mail:xxx 配置后可用）" });
                }
            }
        }

        let total: usize = out["sources"]
            .as_object()
            .map(|m| {
                m.values()
                    .filter_map(|v| v["count"].as_u64())
                    .map(|c| c as usize)
                    .sum()
            })
            .unwrap_or(0);
        out["total"] = json!(total);
        Ok(out)
    }
}

// ---------------- 桥接：RagIndex / ImLog 由 lib 启动时注入（避免工具构造函数层层传参） ----------------

static RAG_BRIDGE: std::sync::OnceLock<Arc<crate::rag::RagIndex>> = std::sync::OnceLock::new();
static IM_LOG_BRIDGE: std::sync::OnceLock<Arc<crate::im::ImLog>> = std::sync::OnceLock::new();

/// lib 启动时注入 RAG 索引句柄
pub fn set_rag_bridge(rag: Arc<crate::rag::RagIndex>) {
    let _ = RAG_BRIDGE.set(rag);
}

/// lib 启动时注入 IM 消息日志句柄
pub fn set_im_log_bridge(log: Arc<crate::im::ImLog>) {
    let _ = IM_LOG_BRIDGE.set(log);
}

fn rag_bridge_lookup() -> Option<Arc<crate::rag::RagIndex>> {
    RAG_BRIDGE.get().cloned()
}

fn im_log_bridge_lookup() -> Option<Vec<Value>> {
    let log = IM_LOG_BRIDGE.get()?;
    Some(log.list())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_short() {
        assert_eq!(clip("hello", 10), "hello");
    }

    #[test]
    fn clip_long() {
        let s = "a".repeat(500);
        let c = clip(&s, 400);
        assert_eq!(c.chars().count(), 401); // 400 + '…'
    }
}
