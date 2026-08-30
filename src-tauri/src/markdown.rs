use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::tools::{PermissionClass, Tool};

/// 文档标签页（与浏览器标签页一致的多标签结构）
#[derive(Clone, serde::Serialize)]
pub struct MarkdownDoc {
    pub id: String,
    pub title: String,
    pub content: String,
    /// 是否为当前激活的标签页
    pub active: bool,
}

/// 内置 Markdown 文档窗口的共享状态（白泽可控制/感知，多标签页）
#[derive(Default, Clone, serde::Serialize)]
pub struct MarkdownState {
    pub docs: Vec<MarkdownDoc>,
}

impl MarkdownState {
    pub fn snapshot(&self) -> Value {
        json!({ "docs": self.docs })
    }

    /// 写入一篇新文档（新建标签页，不覆盖已有文档），返回标签页 id
    pub fn push_document(&mut self, title: &str, content: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        for d in &mut self.docs {
            d.active = false;
        }
        self.docs.push(MarkdownDoc {
            id: id.clone(),
            title: title.to_string(),
            content: content.to_string(),
            active: true,
        });
        id
    }

    /// 在当前激活文档上追加内容；无文档时新建一个
    pub fn append(&mut self, content: &str) {
        if self.docs.is_empty() {
            self.push_document("白泽文档", content);
            return;
        }
        let idx = self
            .docs
            .iter()
            .position(|d| d.active)
            .unwrap_or(self.docs.len() - 1);
        let d = &mut self.docs[idx];
        d.content.push_str(content);
        if d.title.is_empty() {
            d.title = "白泽文档".to_string();
        }
    }

    /// 当前激活文档（无激活时回退到最后一个）
    pub fn active(&self) -> Option<&MarkdownDoc> {
        self.docs
            .iter()
            .find(|d| d.active)
            .or_else(|| self.docs.last())
    }

    pub fn set_active(&mut self, id: &str) {
        for d in &mut self.docs {
            d.active = d.id == id;
        }
    }

    pub fn close_doc(&mut self, id: &str) {
        let was_active = self.docs.iter().any(|d| d.id == id && d.active);
        self.docs.retain(|d| d.id != id);
        if was_active {
            if let Some(last) = self.docs.last_mut() {
                last.active = true;
            }
        }
    }
}

/// 把最新状态推送给文档窗口（按需调出窗口）
fn emit_update(app: &AppHandle, state: &Arc<Mutex<MarkdownState>>) {
    crate::windows::ensure_markdown_window(app);
    let snap = state.lock().unwrap().snapshot();
    let _ = app.emit_to("markdown", "markdown-update", &snap);
}

/// 写入一篇新文档（新建标签页）并推送到文档窗口，供其它模块复用
pub fn write_document(
    app: &AppHandle,
    state: &Arc<Mutex<MarkdownState>>,
    title: &str,
    content: &str,
) {
    {
        let mut s = state.lock().unwrap();
        s.push_document(title, content);
    }
    emit_update(app, state);
    // 主窗口「文档出现即朗读」：写入落笔的瞬间广播 doc-ready（含朗读用全文，截断 8000 字），
    // 前端立即开始分句朗读，与文档出现同步——不再等聊天回复生成落地后才开始
    let payload = json!({
        "title": title,
        "content": content.chars().take(8000).collect::<String>(),
    });
    let _ = app.emit_to("main", "doc-ready", payload);
}

// ───────────────────────── markdown_set ─────────────────────────

pub struct MarkdownSetTool {
    app: AppHandle,
    state: Arc<Mutex<MarkdownState>>,
}

impl MarkdownSetTool {
    pub fn new(app: AppHandle, state: Arc<Mutex<MarkdownState>>) -> Self {
        Self { app, state }
    }
}

impl Tool for MarkdownSetTool {
    fn name(&self) -> &str {
        "markdown_set"
    }
    fn description(&self) -> &str {
        "当用户要求写文档、写报告、写总结、写教程、生成 Markdown 时，调用此工具把完整内容写入右侧文档窗口（新建一个标签页，逐字显示，不覆盖已有文档）。不要只在对话里回复 Markdown 文本。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "Markdown 文本内容" },
                "title": { "type": "string", "description": "文档标题（可选）" }
            },
            "required": ["content"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let content = args["content"].as_str().ok_or("缺少参数 content")?.to_string();
        let title = args["title"].as_str().unwrap_or("白泽文档").to_string();
        write_document(&self.app, &self.state, &title, &content);
        Ok(json!({ "ok": true }))
    }
}

// ───────────────────────── markdown_append ─────────────────────────

pub struct MarkdownAppendTool {
    app: AppHandle,
    state: Arc<Mutex<MarkdownState>>,
}

impl MarkdownAppendTool {
    pub fn new(app: AppHandle, state: Arc<Mutex<MarkdownState>>) -> Self {
        Self { app, state }
    }
}

impl Tool for MarkdownAppendTool {
    fn name(&self) -> &str {
        "markdown_append"
    }
    fn description(&self) -> &str {
        "在右侧文档窗口当前打开的文档追加 Markdown 内容（续写当前标签页，逐字显示）。用于分多次写完一篇长文档。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": { "type": "string", "description": "要追加的 Markdown 文本" }
            },
            "required": ["content"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let content = args["content"].as_str().ok_or("缺少参数 content")?.to_string();
        {
            let mut s = self.state.lock().unwrap();
            s.append(&content);
        }
        emit_update(&self.app, &self.state);
        Ok(json!({ "ok": true }))
    }
}

// ───────────────────────── markdown_get（感知） ─────────────────────────

pub struct MarkdownGetTool {
    state: Arc<Mutex<MarkdownState>>,
}

impl MarkdownGetTool {
    pub fn new(state: Arc<Mutex<MarkdownState>>) -> Self {
        Self { state }
    }
}

impl Tool for MarkdownGetTool {
    fn name(&self) -> &str {
        "markdown_get"
    }
    fn description(&self) -> &str {
        "感知右侧文档窗口当前激活标签页的内容，用于读取/续写已有文档"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let s = self.state.lock().unwrap();
        match s.active() {
            Some(d) => {
                let total = d.content.chars().count();
                let preview: String = d.content.chars().take(4000).collect();
                Ok(json!({
                    "title": d.title,
                    "content": preview,
                    "content_len": total,
                    "docs": s.docs.len(),
                }))
            }
            None => Ok(json!({ "title": "", "content": "", "content_len": 0, "docs": 0 })),
        }
    }
}