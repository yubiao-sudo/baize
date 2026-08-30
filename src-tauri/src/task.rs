use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::tools::{PermissionClass, Tool};

/// 任务步骤（todo 项）
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Todo {
    pub id: usize,
    pub title: String,
    /// pending | in_progress | completed
    pub status: String,
}

/// 解析模型输出的 JSON 步骤数组（容忍模型加的前后缀文字）
pub fn parse_todos(text: &str) -> Result<Vec<Todo>, String> {
    let start = text.find('[').ok_or("无 JSON 数组")?;
    let end = text.rfind(']').ok_or("无 JSON 数组")?;
    let json = &text[start..=end];
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let arr = v.as_array().ok_or("非数组")?;
    let mut out = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
        if !title.is_empty() {
            out.push(Todo {
                id: i,
                title,
                status: "pending".to_string(),
            });
        }
    }
    Ok(out)
}

/// 推送完整 todo 列表到前端（任务拆解完成时）
pub fn emit_todo_list(app: &AppHandle, todos: &[Todo]) {
    let _ = app.emit("todo-list", json!({ "todos": todos }));
}

/// 推送 todo 状态更新到前端（执行过程中）
pub fn emit_todo_update(app: &AppHandle, todos: &[Todo]) {
    let _ = app.emit("todo-update", json!({ "todos": todos }));
}

/// 检查点持久化键名
const CHECKPOINT_KEY: &str = "checkpoint:todos";

/// 持久化任务步骤（断点续跑：重启后恢复未完成步骤）
pub fn save_task_checkpoint(store: &crate::memory::MemoryStore, todos: &[Todo]) {
    match serde_json::to_string(todos) {
        Ok(json) => {
            let _ = store.set_setting(CHECKPOINT_KEY, &json);
        }
        Err(e) => eprintln!("[检查点] 序列化 todo 失败: {e}"),
    }
}

/// 恢复任务步骤（无检查点或解析失败返回空表）
pub fn load_task_checkpoint(store: &crate::memory::MemoryStore) -> Vec<Todo> {
    match store.get_setting(CHECKPOINT_KEY) {
        Ok(Some(json)) => match serde_json::from_str::<Vec<Todo>>(&json) {
            Ok(todos) => todos,
            Err(e) => {
                eprintln!("[检查点] 解析 todo 失败: {e}");
                Vec::new()
            }
        },
        _ => Vec::new(),
    }
}

/// todo_update 工具：让模型自主维护任务步骤状态，前端据此展示执行流程
pub struct TodoUpdateTool {
    app: AppHandle,
    todos: Arc<Mutex<Vec<Todo>>>,
    store: Arc<crate::memory::MemoryStore>,
}

impl TodoUpdateTool {
    pub fn new(
        app: AppHandle,
        todos: Arc<Mutex<Vec<Todo>>>,
        store: Arc<crate::memory::MemoryStore>,
    ) -> Self {
        Self { app, todos, store }
    }
}

impl Tool for TodoUpdateTool {
    fn name(&self) -> &str {
        "todo_update"
    }
    fn description(&self) -> &str {
        "更新任务计划的步骤状态，用于展示执行进度。执行多步骤任务时，每开始或完成一个步骤就调用一次，\
         传入完整步骤列表（每项含 title 和 status：pending/in_progress/completed）。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "完整步骤列表",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string", "description": "步骤描述" },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"]
                            }
                        },
                        "required": ["title", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let arr = args["todos"].as_array().ok_or("缺少参数 todos")?;
        let mut new_todos: Vec<Todo> = Vec::new();
        for (i, item) in arr.iter().enumerate() {
            let title = item["title"].as_str().unwrap_or("").to_string();
            let status = item["status"].as_str().unwrap_or("pending").to_string();
            let status = match status.as_str() {
                "in_progress" | "completed" => status,
                _ => "pending".to_string(),
            };
            if !title.is_empty() {
                new_todos.push(Todo { id: i, title, status });
            }
        }
        {
            let mut t = self.todos.lock().unwrap();
            *t = new_todos.clone();
        }
        save_task_checkpoint(&self.store, &new_todos);
        emit_todo_update(&self.app, &new_todos);
        Ok(json!({ "ok": true, "count": new_todos.len() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_todos_extracts_steps() {
        let text = "[\n  {\"title\": \"步骤一\"},\n  {\"title\": \"步骤二\"}\n]";
        let todos = parse_todos(text).unwrap();
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0].title, "步骤一");
        assert_eq!(todos[1].title, "步骤二");
        assert_eq!(todos[0].status, "pending");
    }

    #[test]
    fn parse_todos_handles_prefix_text() {
        let text = "好的，以下是步骤：[\n{\"title\":\"a\"},{\"title\":\"b\"}]";
        let todos = parse_todos(text).unwrap();
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[0].title, "a");
    }

    #[test]
    fn parse_todos_empty_array() {
        let todos = parse_todos("[]").unwrap();
        assert!(todos.is_empty());
    }
}
