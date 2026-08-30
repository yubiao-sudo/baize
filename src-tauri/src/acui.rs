//! ACUI 受控卡片：Agent 只能通过 render_card 工具渲染「白名单类型」的卡片，
//! 保证 UI 安全可预测（不会让模型自由注入任意 HTML/脚本）。

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::tools::{PermissionClass, Tool};

/// 允许的卡片类型
const ALLOWED_KINDS: &[&str] = &["text", "progress", "confirm", "data"];

pub struct RenderCardTool {
    app: AppHandle,
}

impl RenderCardTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for RenderCardTool {
    fn name(&self) -> &str {
        "render_card"
    }
    fn description(&self) -> &str {
        "在界面上渲染一张卡片（白名单类型：text 文本 / progress 进度 / confirm 确认 / data 数据展示）。data 类型用于展示结构化数据（天气、股票、统计等），自动按 JSON/markdown/表格 等形式美化渲染，可任意编辑美化。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "enum": ["text", "progress", "confirm", "data"], "description": "卡片类型" },
                "title": { "type": "string", "description": "卡片标题" },
                "body": { "type": "string", "description": "卡片正文" },
                "progress": { "type": "number", "description": "进度 0-100（仅 progress 类型）" },
                "data": { "type": "string", "description": "结构化数据内容（仅 data 类型）：可为 JSON 字符串、markdown、或表格 markdown" }
            },
            "required": ["kind", "title"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let kind = args["kind"].as_str().unwrap_or("text").to_string();
        if !ALLOWED_KINDS.contains(&kind.as_str()) {
            return Err(format!("不允许的卡片类型: {kind}（仅支持 text/progress/confirm/data）"));
        }
        let title = args["title"].as_str().unwrap_or("").to_string();
        let body = args["body"].as_str().unwrap_or("").to_string();
        let progress = args["progress"].as_f64().unwrap_or(0.0).clamp(0.0, 100.0);
        let data = args["data"].as_str().unwrap_or("").to_string();
        let id = uuid::Uuid::new_v4().to_string();

        self.app
            .emit(
                "acui-card",
                json!({ "id": id, "kind": kind, "title": title, "body": body, "progress": progress, "data": data }),
            )
            .map_err(|e| e.to_string())?;

        Ok(json!({ "ok": true, "id": id }))
    }
}
