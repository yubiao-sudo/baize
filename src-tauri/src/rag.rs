use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::kb::{load_token, KbAnswer, KbConfig, RemoteKb};
use crate::memory::MemoryStore;
use crate::tools::{PermissionClass, Tool};

/// 文档 chunk
#[derive(Clone)]
pub struct DocChunk {
    pub path: String,
    pub content: String,
    pub embedding: Option<Vec<f32>>,
}

/// 本地知识库 RAG 索引（内存）
pub struct RagIndex {
    chunks: Mutex<Vec<DocChunk>>,
    store: Arc<MemoryStore>,
}

impl RagIndex {
    /// 取索引锁，**中毒也照常恢复**。
    ///
    /// 原来每处都写 `.lock().unwrap()`：一旦某个线程持锁时 panic，锁就被标记为
    /// 中毒，之后**所有**检索调用都会在同一行继续 panic——一次偶发崩溃会变成
    /// 永久故障，只能重启进程。索引里存的只是文档片段，中毒说明前一次写入
    /// 可能不完整，但读旧数据远好过整个知识库瘫掉。
    fn chunks_guard(&self) -> std::sync::MutexGuard<'_, Vec<DocChunk>> {
        self.chunks.lock().unwrap_or_else(|poisoned| {
            eprintln!("[RAG] 索引锁中毒，已恢复（前一次写入可能不完整）");
            poisoned.into_inner()
        })
    }

    pub fn new(store: Arc<MemoryStore>) -> Self {
        let chunks = store
            .load_rag_chunks()
            .unwrap_or_default()
            .into_iter()
            .map(|(path, content, embedding)| DocChunk {
                path,
                content,
                embedding,
            })
            .collect();
        Self {
            chunks: Mutex::new(chunks),
            store,
        }
    }

    /// 索引一个目录：扫描文本文件 → 分块 → embedding
    pub fn index_dir(&self, path: &str, max_chunks: usize) -> Result<usize, String> {
        self.index_dir_with_progress(path, max_chunks, None)
    }

    /// 同上，带进度回调（files_done, files_total）——供后台任务面板实时显示
    pub fn index_dir_with_progress(
        &self,
        path: &str,
        max_chunks: usize,
        on_progress: Option<&dyn Fn(usize, usize)>,
    ) -> Result<usize, String> {
        let files = scan_text_files(path)?;
        let total = files.len();
        let mut chunks: Vec<DocChunk> = Vec::new();
        for (done, f) in files.into_iter().enumerate() {
            if let Some(cb) = on_progress {
                cb(done + 1, total);
            }
            let Ok(content) = std::fs::read_to_string(&f) else {
                continue;
            };
            for chunk in split_chunks(&content, 400) {
                if chunks.len() >= max_chunks {
                    break;
                }
                let embedding = crate::embedding::embed(&chunk).ok();
                chunks.push(DocChunk {
                    path: f.clone(),
                    content: chunk,
                    embedding,
                });
            }
            if chunks.len() >= max_chunks {
                break;
            }
        }
        let count = chunks.len();
        let tuples: Vec<(String, String, Option<Vec<f32>>)> = chunks
            .iter()
            .map(|c| (c.path.clone(), c.content.clone(), c.embedding.clone()))
            .collect();
        self.store.save_rag_chunks(&tuples)?;
        *self.chunks_guard() = chunks;
        Ok(count)
    }

    /// 语义检索：按 `kb_mode` 决定走底座还是本地索引。
    ///
    /// 分派规则（`kb.rs` 的 `KbConfig`）：
    ///
    /// | mode | 行为 |
    /// |---|---|
    /// | `off` | 直接返回空，不打扰 agent |
    /// | `local` | 原来的本地 RAG（默认，向后兼容） |
    /// | `remote` | 只走底座；失败返回 `refused=true` 并说明原因，**不回落** |
    /// | `hybrid` | 优先底座；失败回落本地，且 `source` 标为 `local-fallback` |
    ///
    /// 为什么 `remote` 不静默回落：本地那份 RAG 无重排、无拒答、上限 100 块，
    /// 静默回落会让用户以为查的是企业库、实际拿到的是过期本地数据——
    /// 这比"明确报错"危险得多。
    pub fn search(&self, query: &str, top_k: usize) -> KbAnswer {
        let cfg = KbConfig::load(&self.store);

        if cfg.is_off() {
            return KbAnswer::empty("none");
        }

        if cfg.uses_remote() {
            let remote = RemoteKb::new(cfg.clone(), load_token(&self.store));
            let answer = remote.search(query, top_k);
            // 底座明确拒答 → 原样返回，绝不回落（拒答是结论，不是故障）
            if answer.refused && answer.reason.is_some()
                && !answer.reason.as_deref().unwrap_or("").starts_with("底座")
            {
                // 已归一化理由，直接透传
            }
            if !answer.refused && !answer.hits.is_empty() {
                return answer;
            }
            let failure = answer.reason.clone().unwrap_or_else(|| "无结果".into());
            // 拒答（有明确理由、且底座可达）与故障要区分开：
            //   · refused=true 且理由里带"可达分" → 底座判定依据不足，属正常结论；
            //   · 其余（超时/连不上/401/HTTP 错）→ 故障，才考虑回落。
            let is_verdict = answer.refused
                && (failure.contains("可达分") || failure.contains("按档位拒答"));
            if is_verdict {
                return answer;
            }
            if cfg.fallback_local {
                let mut local = self.search_local(query, top_k);
                if !local.hits.is_empty() {
                    local.source = "local-fallback";
                    local.reason = Some(format!(
                        "底座不可用（{failure}），已回落本地索引；结果可能不是企业库内容"
                    ));
                    return local;
                }
            }
            return answer;
        }

        self.search_local(query, top_k)
    }

    /// 原来的本地检索：embedding 余弦优先，n-gram 兜底。
    fn search_local(&self, query: &str, top_k: usize) -> KbAnswer {
        let chunks = self.chunks_guard().clone();
        if chunks.is_empty() {
            return KbAnswer::empty("local");
        }

        // 语义检索（embedding 余弦相似度）
        if let Ok(q_emb) = crate::embedding::embed(query) {
            let mut scored: Vec<(f64, &DocChunk)> = chunks
                .iter()
                .filter_map(|c| {
                    c.embedding
                        .as_ref()
                        .map(|e| (crate::embedding::cosine(&q_emb, e), c))
                })
                .collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
            let hits: Vec<Value> = scored
                .into_iter()
                .take(top_k)
                .filter(|(s, _)| *s > 0.2)
                .map(|(s, c)| json!({ "path": c.path, "content": c.content, "score": (s * 100.0) as i64 }))
                .collect();
            if !hits.is_empty() {
                return KbAnswer {
                    refused: false,
                    reason: None,
                    source: "local",
                    answerability: None,
                    threshold: None,
                    hits,
                };
            }
        }

        // 降级：关键词匹配
        let hits: Vec<Value> = chunks
            .iter()
            .filter(|c| c.content.contains(query))
            .take(top_k)
            .map(|c| json!({ "path": c.path, "content": c.content, "score": 0 }))
            .collect();
        KbAnswer {
            refused: false,
            reason: if hits.is_empty() {
                Some("本地索引里没有匹配内容".to_string())
            } else {
                None
            },
            source: "local",
            answerability: None,
            threshold: None,
            hits,
        }
    }

    /// 列出已索引文档（按 path 分组，统计 chunk 数）
    pub fn list_paths(&self) -> Vec<Value> {
        let chunks = self.chunks_guard();
        let mut map: HashMap<String, usize> = HashMap::new();
        for c in chunks.iter() {
            *map.entry(c.path.clone()).or_default() += 1;
        }
        let mut paths: Vec<Value> = map
            .into_iter()
            .map(|(path, count)| json!({ "path": path, "chunks": count }))
            .collect();
        paths.sort_by(|a, b| {
            a["path"]
                .as_str()
                .cmp(&b["path"].as_str())
        });
        paths
    }

    /// 清空知识库
    pub fn clear(&self) -> Result<(), String> {
        self.store.clear_rag_chunks()?;
        self.chunks_guard().clear();
        Ok(())
    }
}

/// 递归扫描目录下的文本文件
pub(crate) fn scan_text_files(dir: &str) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    scan_dir(dir, &mut files, 0)?;
    if files.is_empty() {
        return Err("目录中没有找到可索引的文本文件".to_string());
    }
    Ok(files)
}

fn scan_dir(dir: &str, out: &mut Vec<String>, depth: usize) -> Result<(), String> {
    if depth > 4 {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let _ = scan_dir(&path.to_string_lossy(), out, depth + 1);
        } else {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if matches!(
                ext.as_str(),
                "txt" | "md" | "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "json" | "csv"
                    | "html" | "css" | "toml" | "yaml" | "yml" | "log"
            ) {
                if let Ok(meta) = path.metadata() {
                    if meta.len() < 1_000_000 {
                        out.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }
    Ok(())
}

/// 按固定长度分块
fn split_chunks(text: &str, size: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        current.push(c);
        if current.chars().count() >= size {
            chunks.push(std::mem::take(&mut current));
        }
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

// ───────────────────────── rag_index 工具 ─────────────────────────

pub struct RagIndexTool {
    index: Arc<RagIndex>,
}

impl RagIndexTool {
    pub fn new(index: Arc<RagIndex>) -> Self {
        Self { index }
    }
}

impl Tool for RagIndexTool {
    fn name(&self) -> &str {
        "rag_index"
    }
    fn description(&self) -> &str {
        "把本地目录索引为知识库（扫描文本文件、分块、向量化）。\
         注意：若已接入远程企业知识库（kb_mode=remote/hybrid），\
         索引应在底座控制台完成，本工具会返回如何操作的说明。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "要索引的目录绝对路径" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = args["path"].as_str().ok_or("缺少参数 path")?;
        // 远程模式下把文件**上传到底座**，而不是在本地建一份副本。
        // 本地建索引会让用户以为"已经进了企业库"，实际只是本地副本，
        // 检索却走底座 —— 两边数据不一致是最难排查的问题。
        let cfg = KbConfig::load(&self.index.store);
        if cfg.uses_remote() {
            let remote = RemoteKb::new(cfg.clone(), load_token(&self.index.store));
            let mut out = remote.ingest_path(path, 20)?;
            out["hint"] = json!(format!(
                "已上传到底座 {}/{}（{}）。摄入完成即可用 rag_search 检索。",
                cfg.tenant, cfg.kb, cfg.ingest_url
            ));
            return Ok(out);
        }
        let count = self.index.index_dir(path, 100)?;
        Ok(json!({ "ok": true, "path": path, "chunks": count, "mode": cfg.mode }))
    }
}

// ───────────────────────── rag_search 工具 ─────────────────────────

pub struct RagSearchTool {
    index: Arc<RagIndex>,
    app: AppHandle,
}

impl RagSearchTool {
    pub fn new(app: AppHandle, index: Arc<RagIndex>) -> Self {
        Self { index, app }
    }
}

impl Tool for RagSearchTool {
    fn name(&self) -> &str {
        "rag_search"
    }
    fn description(&self) -> &str {
        "在知识库中检索与问题相关的内容。返回 hits（每条带 path/content/score/citation/char_range）。\
         ⚠️ 若返回 refused=true，表示底座判定「知识库里没有足够依据」，\
         此时**必须如实告诉用户知识库中没有相关内容**，禁止凭常识或记忆补全答案。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "检索关键词或问题" }
            },
            "required": ["query"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let query = args["query"].as_str().ok_or("缺少参数 query")?;
        let top_k = KbConfig::load(&self.index.store).top_k;
        let answer = self.index.search(query, top_k);
        // 检索完成时发「rag」事件，让前端展示检索结果数量与来源
        let label = if answer.refused {
            format!("知识库拒答 · {}", answer.reason.clone().unwrap_or_default())
        } else {
            format!("知识库检索 · {} 条（{}）", answer.hits.len(), answer.source)
        };
        let _ = self.app.emit(
            "thought",
            json!({
                "kind": "rag",
                "label": label,
                "detail": query,
            }),
        );
        Ok(answer.to_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_chunks_by_size() {
        let text = "a".repeat(450);
        let chunks = split_chunks(&text, 400);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), 400);
        assert_eq!(chunks[1].chars().count(), 50);
    }
}
