use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

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
        let files = scan_text_files(path)?;
        let mut chunks: Vec<DocChunk> = Vec::new();
        for f in files {
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
        *self.chunks.lock().unwrap() = chunks;
        Ok(count)
    }

    /// 语义检索：query embedding 相似度（不可用时降级为关键词匹配）
    pub fn search(&self, query: &str, top_k: usize) -> Vec<Value> {
        let chunks = self.chunks.lock().unwrap().clone();
        if chunks.is_empty() {
            return vec![];
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
                return hits;
            }
        }

        // 降级：关键词匹配
        chunks
            .iter()
            .filter(|c| c.content.contains(query))
            .take(top_k)
            .map(|c| json!({ "path": c.path, "content": c.content, "score": 0 }))
            .collect()
    }

    /// 列出已索引文档（按 path 分组，统计 chunk 数）
    pub fn list_paths(&self) -> Vec<Value> {
        let chunks = self.chunks.lock().unwrap();
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
        self.chunks.lock().unwrap().clear();
        Ok(())
    }
}

/// 递归扫描目录下的文本文件
fn scan_text_files(dir: &str) -> Result<Vec<String>, String> {
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
        "索引一个本地目录为知识库（扫描文本文件、分块、向量化），之后可用 rag_search 语义检索其中的内容"
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
        let count = self.index.index_dir(path, 100)?;
        Ok(json!({ "ok": true, "path": path, "chunks": count }))
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
        "在已索引的本地知识库中语义检索相关内容（需先 rag_index 索引目录）"
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
        let hits = self.index.search(query, 5);
        // 检索完成时发「rag」事件，让前端展示检索结果数量
        let _ = self.app.emit(
            "thought",
            json!({
                "kind": "rag",
                "label": format!("知识库检索 · {} 条", hits.len()),
                "detail": query,
            }),
        );
        Ok(json!({ "count": hits.len(), "hits": hits }))
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
