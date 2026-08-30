//! 长期记忆层的记忆管理工具集。
//!
//! 对应《白泽自主进化》功能一：让白泽跨会话拥有「人格连续性」。
//! 提供记忆的主动记录 / 语义检索 / 列出 / 删除 / 合并整理 / 用户画像等能力，
//! 全部落地到 MemoryStore 的三张核心表（memories / events / semantic_memories）。

use std::sync::Arc;

use serde_json::{json, Value};

use crate::memory::MemoryStore;
use crate::tools::{PermissionClass, Tool};

// ───────────────── remember：主动记录一条记忆 ─────────────────

pub struct RememberTool {
    store: Arc<MemoryStore>,
}

impl RememberTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for RememberTool {
    fn name(&self) -> &str {
        "remember"
    }
    fn description(&self) -> &str {
        "主动记录一条值得长期记住的信息（偏好、事实、项目、习惯、经验等）。输入一句自然语言，白泽会过滤噪音、同话题自动合并强化，供以后对话自动召回。kind: preference偏好|fact事实|project项目|habit习惯|lesson经验"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "要记住的内容（自然语言一句话）" },
                "kind": { "type": "string", "description": "记忆类型，默认 fact" },
                "importance": { "type": "number", "description": "重要性 0~1，默认 0.5。高于 0.7 会额外记入情景事件表" }
            },
            "required": ["content"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let content = args["content"].as_str().ok_or("缺少参数 content")?;
        let kind = args["kind"].as_str().unwrap_or("fact");
        let importance = args["importance"].as_f64().unwrap_or(0.5);
        let outcome = self.store.smart_remember(content, kind)?;
        let state = match outcome {
            crate::memory::RememberOutcome::Created => "created",
            crate::memory::RememberOutcome::Reinforced => "reinforced",
            crate::memory::RememberOutcome::Filtered => "filtered",
        };
        // 高重要性内容额外作为情景事件打点，供后续合并为语义记忆
        if importance >= 0.7 && outcome != crate::memory::RememberOutcome::Filtered {
            let _ = self.store.record_event("decision", content, content, importance);
        }
        Ok(json!({ "ok": true, "state": state, "content": content }))
    }
}

// ───────────────── memory_search：语义/关键词跨界检索 ─────────────────

pub struct MemorySearchTool {
    store: Arc<MemoryStore>,
}

impl MemorySearchTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for MemorySearchTool {
    fn name(&self) -> &str {
        "memory_search"
    }
    fn description(&self) -> &str {
        "检索白泽的记忆（跨工作记忆/情景记忆/语义记忆三路召回）。按 query 的相关性返回 top_k 条，用于回答「我以前说过什么 / 用户喜欢什么 / 之前怎么决定的」等问题"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "检索关键词或问题" },
                "top_k": { "type": "number", "description": "返回条数，默认 6" }
            },
            "required": ["query"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let query = args["query"].as_str().ok_or("缺少参数 query")?;
        let top_k = args["top_k"].as_u64().unwrap_or(6).clamp(1, 30) as usize;

        let mut hits = Vec::new();
        // 1) 工作记忆（memories 表，语义相似度优先）
        for m in self.store.recall_related(query, top_k)? {
            hits.push(json!({
                "source": "memory",
                "kind": m.kind,
                "content": m.content,
                "salience": m.salience,
            }));
        }
        // 2) 情景记忆（events 表，关键词）
        for e in self.store.recall_events(query, top_k)? {
            hits.push(json!({
                "source": "event",
                "kind": e.event_type,
                "content": e.summary,
                "importance": e.importance,
                "ts": e.ts,
            }));
        }
        // 3) 语义记忆（semantic_memories 表，embedding 余弦相似度）
        for s in self.store.recall_semantic(query, top_k)? {
            hits.push(json!({
                "source": "semantic",
                "kind": s.category,
                "content": s.content,
                "confidence": s.confidence,
            }));
        }

        Ok(json!({ "query": query, "count": hits.len(), "results": hits }))
    }
}

// ───────────────── memory_list：列出记忆 ─────────────────

pub struct MemoryListTool {
    store: Arc<MemoryStore>,
}

impl MemoryListTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for MemoryListTool {
    fn name(&self) -> &str {
        "memory_list"
    }
    fn description(&self) -> &str {
        "列出白泽当前记住的内容：工作记忆（memories）、情景记忆（events）、语义记忆（semantic）。可指定 scope 只看某一类"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "scope": { "type": "string", "enum": ["all", "memory", "event", "semantic"], "description": "只看哪类，默认 all" },
                "limit": { "type": "number", "description": "每类条数上限，默认 20" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let scope = args["scope"].as_str().unwrap_or("all");
        let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 100) as usize;

        let mut out = json!({ "ok": true });
        if scope == "all" || scope == "memory" {
            let memories: Vec<Value> = self
                .store
                .recent_memories(limit)?
                .into_iter()
                .map(|m| json!({ "id": m.mem_id, "kind": m.kind, "content": m.content, "salience": m.salience }))
                .collect();
            out["memories"] = json!(memories);
        }
        if scope == "all" || scope == "event" {
            let events: Vec<Value> = self
                .store
                .list_events(limit)?
                .into_iter()
                .map(|e| json!({ "id": e.id, "type": e.event_type, "summary": e.summary, "importance": e.importance }))
                .collect();
            out["events"] = json!(events);
        }
        if scope == "all" || scope == "semantic" {
            let semantic: Vec<Value> = self
                .store
                .list_semantic("")?
                .into_iter()
                .map(|s| json!({ "id": s.id, "category": s.category, "content": s.content, "confidence": s.confidence }))
                .collect();
            out["semantic"] = json!(semantic);
        }
        Ok(out)
    }
}

// ───────────────── memory_delete：删除记忆 ─────────────────

pub struct MemoryDeleteTool {
    store: Arc<MemoryStore>,
}

impl MemoryDeleteTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for MemoryDeleteTool {
    fn name(&self) -> &str {
        "memory_delete"
    }
    fn description(&self) -> &str {
        "删除一条记忆。id 来自 memory_list 返回的记忆 id；传 \"all\" 清空全部记忆。用于用户明确表示「忘掉/别记这个」的场景"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "记忆 id，或 \"all\" 清空全部" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        if id == "all" {
            let n = self.store.clear_memories()?;
            return Ok(json!({ "ok": true, "cleared": n }));
        }
        let removed = self.store.delete_memory(id)?;
        Ok(json!({ "ok": true, "removed": removed }))
    }
}

// ───────────────── memory_consolidate：合并/遗忘整理 ─────────────────

pub struct MemoryConsolidateTool {
    store: Arc<MemoryStore>,
}

impl MemoryConsolidateTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for MemoryConsolidateTool {
    fn name(&self) -> &str {
        "memory_consolidate"
    }
    fn description(&self) -> &str {
        "整理记忆：对超期未命中的工作记忆做衰减遗忘，按遗忘策略清理过期情景事件。可选设置某类记忆的保留策略（max_age_days 最长保留天数 / min_hits 最小命中次数）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "max_age_days": { "type": "number", "description": "可选：设置 events 类记忆最长保留天数" },
                "min_hits": { "type": "number", "description": "可选：低于该命中次数的过期事件将被删除" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        if let (Some(max_age), min_hits) = (
            args["max_age_days"].as_f64(),
            args["min_hits"].as_f64().unwrap_or(0.0),
        ) {
            self.store
                .set_forget_policy("events", max_age as i64, min_hits as i64)?;
        }
        let decayed = self.store.decay_memories()?;
        let purged = self.store.consolidate_events()?;
        let policy = self.store.get_forget_policy("events")?;
        Ok(json!({
            "ok": true,
            "decayed": decayed,
            "purged_events": purged,
            "event_policy": { "max_age_days": policy.0, "min_hits": policy.1 },
        }))
    }
}

// ───────────────── memory_profile：用户画像 ─────────────────

pub struct MemoryProfileTool {
    store: Arc<MemoryStore>,
}

impl MemoryProfileTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for MemoryProfileTool {
    fn name(&self) -> &str {
        "memory_profile"
    }
    fn description(&self) -> &str {
        "查看白泽目前对用户的长期画像/偏好（高显著性 + 偏好关键词过滤的记忆，以及 user_profile 类语义记忆），回答「你还记得我什么」时使用"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let profile: Vec<String> = self
            .store
            .recall_profile(20)?
            .into_iter()
            .map(|m| m.content)
            .collect();
        let semantic: Vec<String> = self
            .store
            .list_semantic("user_profile")?
            .into_iter()
            .map(|s| s.content)
            .collect();
        Ok(json!({
            "ok": true,
            "preferences": profile,
            "profile_semantic": semantic,
            "count": profile.len() + semantic.len(),
        }))
    }
}

// ───────────────── memory_semantic_add：写入语义记忆 ─────────────────

pub struct MemorySemanticAddTool {
    store: Arc<MemoryStore>,
}

impl MemorySemanticAddTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for MemorySemanticAddTool {
    fn name(&self) -> &str {
        "memory_semantic_add"
    }
    fn description(&self) -> &str {
        "把一条提炼后的语义记忆（稳定的画像/事实/项目知识）写入语义记忆库。category: user_profile用户画像|project项目|preference偏好|lesson经验教训"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "category": { "type": "string", "description": "记忆分类" },
                "content": { "type": "string", "description": "语义记忆内容" },
                "confidence": { "type": "number", "description": "置信度 0~1，默认 0.6" }
            },
            "required": ["category", "content"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let category = args["category"].as_str().ok_or("缺少参数 category")?;
        let content = args["content"].as_str().ok_or("缺少参数 content")?;
        let confidence = args["confidence"].as_f64().unwrap_or(0.6);
        self.store.upsert_semantic(category, content, confidence, None)?;
        Ok(json!({ "ok": true, "category": category }))
    }
}

// ───────────────── memory_forget：按关键词遗忘 ─────────────────

pub struct MemoryForgetTool {
    store: Arc<MemoryStore>,
}

impl MemoryForgetTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for MemoryForgetTool {
    fn name(&self) -> &str {
        "memory_forget"
    }
    fn description(&self) -> &str {
        "忘记与某关键词相关的记忆（工作记忆、语义记忆、情景事件里命中该词的都会清除）。用于用户明确说「忘记我说过 XX / 别再记这个」时，删除后相关内容不再出现在简报和检索里"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "keyword": { "type": "string", "description": "要遗忘的关键词/短语" }
            },
            "required": ["keyword"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let keyword = args["keyword"].as_str().ok_or("缺少参数 keyword")?;
        let removed = self.store.forget_matching(keyword)?;
        Ok(json!({ "ok": true, "removed": removed, "keyword": keyword }))
    }
}