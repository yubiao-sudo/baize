//! 后台长任务队列：提交任务后异步执行，不阻塞会话；支持状态查询、失败重试、取消与完成通知。
//!
//! 复用 `Supervisor` 作为执行内核，任务在 `tokio` 后台运行，通过 `task-update` 事件实时推送状态。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::agent::Supervisor;
use crate::AppState;

/// 后台任务记录
#[derive(Debug, Clone, Serialize)]
pub struct BackgroundTask {
    pub id: String,
    pub description: String,
    /// pending | running | retrying | done | failed | cancelled
    pub status: String,
    pub error: Option<String>,
    pub retries: usize,
    pub max_retries: usize,
}

/// 任务队列（进程内全局单例）
pub struct TaskQueue {
    inner: Mutex<Vec<Arc<Mutex<BackgroundTask>>>>,
}

impl TaskQueue {
    fn new() -> Self {
        Self { inner: Mutex::new(Vec::new()) }
    }
    fn push(&self, t: BackgroundTask) -> Arc<Mutex<BackgroundTask>> {
        let a = Arc::new(Mutex::new(t));
        self.inner.lock().unwrap().push(a.clone());
        a
    }
    fn list(&self) -> Vec<BackgroundTask> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|a| a.lock().unwrap().clone())
            .collect()
    }
}

static QUEUE: OnceLock<TaskQueue> = OnceLock::new();
fn queue() -> &'static TaskQueue {
    QUEUE.get_or_init(TaskQueue::new)
}
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn set_status(cell: &Arc<Mutex<BackgroundTask>>, status: &str, error: Option<String>) {
    let mut t = cell.lock().unwrap();
    t.status = status.to_string();
    t.error = error;
}

/// 提交一个后台任务，返回任务 id。
#[tauri::command]
pub fn submit_task(app: AppHandle, description: String) -> String {
    const MAX_RETRIES: usize = 2;
    let id = format!("task-{}", SEQ.fetch_add(1, Ordering::Relaxed));
    let cell = queue().push(BackgroundTask {
        id: id.clone(),
        description: description.clone(),
        status: "pending".into(),
        error: None,
        retries: 0,
        max_retries: MAX_RETRIES,
    });

    let task_id = id.clone();
    tauri::async_runtime::spawn(async move {
        for attempt in 0..=MAX_RETRIES {
            {
                let mut t = cell.lock().unwrap();
                t.retries = attempt;
                t.status = if attempt == 0 { "running".into() } else { "retrying".into() };
            }
            let _ = app.emit(
                "task-update",
                json!({ "id": task_id, "status": "running", "retries": attempt }),
            );

            match run_once(&app, &description).await {
                Ok(answer) => {
                    set_status(&cell, "done", None);
                    let _ = app.emit(
                        "task-update",
                        json!({
                            "id": task_id,
                            "status": "done",
                            "result_preview": answer.chars().take(200).collect::<String>(),
                        }),
                    );
                    let _ = app.emit(
                        "thought",
                        json!({ "kind": "task", "label": format!("后台任务完成 · {task_id}"), "detail": &description }),
                    );
                    return;
                }
                Err(e) => {
                    if attempt >= MAX_RETRIES {
                        set_status(&cell, "failed", Some(e.clone()));
                        let _ = app.emit(
                            "task-update",
                            json!({ "id": task_id, "status": "failed", "error": e }),
                        );
                        return;
                    }
                    set_status(&cell, "retrying", Some(e));
                }
            }
        }
    });

    id
}

/// 单次执行一个后台任务（复用 Agent 循环）
async fn run_once(app: &AppHandle, desc: &str) -> Result<String, String> {
    let state = app.state::<AppState>();
    Supervisor::new(app, &state).run(desc, vec![]).await
}

#[tauri::command]
pub fn list_tasks() -> Vec<BackgroundTask> {
    queue().list()
}

#[tauri::command]
pub fn get_task(id: String) -> Option<BackgroundTask> {
    queue()
        .inner
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.lock().unwrap().id == id)
        .map(|a| a.lock().unwrap().clone())
}

/// 取消后台任务：置全局取消标志（Agent 循环在下一个检查点返回），并把任务标记为 cancelled
#[tauri::command]
pub fn cancel_task(state: State<'_, AppState>, id: String) -> bool {
    let cell = queue()
        .inner
        .lock()
        .unwrap()
        .iter()
        .find(|a| a.lock().unwrap().id == id)
        .cloned();
    let Some(cell) = cell else {
        return false;
    };
    {
        let t = cell.lock().unwrap();
        if matches!(t.status.as_str(), "done" | "failed" | "cancelled") {
            return false;
        }
    }
    state.cancel.store(true, Ordering::SeqCst);
    // 同步置位全局工具取消标志：长跑子进程（ps_exec 等）感知后自行终止
    crate::tools::request_global_cancel();
    set_status(&cell, "cancelled", None);
    true
}