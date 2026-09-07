//! 后台任务队列（JobManager）：长操作（RAG 索引、批量处理等）后台执行，
//! 进度通过 "job-update" 事件实时广播，前端浮层显示活跃任务。
//!
//! 用法：
//! ```ignore
//! let job = jobs().start("rag", "索引工作区文档", total);
//! jobs().update(&job, done, "xxx.md");
//! jobs().finish(&job, true, "完成");
//! ```

use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize)]
pub struct JobInfo {
    pub id: String,
    /// 任务种类（"rag" / "batch" / ...），前端可据此显示图标
    pub kind: String,
    pub title: String,
    /// "running" | "done" | "failed"
    pub status: String,
    pub progress: u64,
    pub total: u64,
    pub detail: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

static JOBS: OnceLock<Mutex<HashMap<String, JobInfo>>> = OnceLock::new();
/// 事件句柄：setup 时注册；为 None 时（如单元测试）静默跳过事件广播
static APP: OnceLock<AppHandle> = OnceLock::new();

pub fn init(app: AppHandle) {
    let _ = APP.set(app);
}

fn slot() -> &'static Mutex<HashMap<String, JobInfo>> {
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn emit(job: &JobInfo) {
    if let Some(app) = APP.get() {
        let _ = app.emit("job-update", serde_json::to_value(job).unwrap_or_default());
    }
}

/// 创建并注册一个新任务，返回 id
pub fn start(kind: &str, title: &str, total: u64) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    let info = JobInfo {
        id: id.clone(),
        kind: kind.to_string(),
        title: title.to_string(),
        status: "running".into(),
        progress: 0,
        total,
        detail: String::new(),
        started_at: now_ms(),
        finished_at: None,
    };
    emit(&info);
    slot().lock().unwrap().insert(id.clone(), info);
    id
}

/// 设置总量（进度分母），创建时未知则后补
pub fn set_total(id: &str, total: u64) {
    let mut g = slot().lock().unwrap();
    if let Some(j) = g.get_mut(id) {
        j.total = total;
        emit(j);
    }
}

/// 更新进度（done/total）；detail 为空则保留原值
pub fn update(id: &str, progress: u64, detail: &str) {
    let mut g = slot().lock().unwrap();
    if let Some(j) = g.get_mut(id) {
        j.progress = progress;
        if !detail.is_empty() {
            j.detail = detail.to_string();
        }
        emit(j);
    }
}

/// 结束任务（ok=false 为失败）
pub fn finish(id: &str, ok: bool, detail: &str) {
    let mut g = slot().lock().unwrap();
    if let Some(j) = g.get_mut(id) {
        j.status = if ok { "done" } else { "failed" }.into();
        j.progress = if ok { j.total } else { j.progress };
        if !detail.is_empty() {
            j.detail = detail.to_string();
        }
        j.finished_at = Some(now_ms());
        emit(j);
        // 完成任务保留 60s 供前端看到结果，由事件接收端决定是否继续展示；
        // 这里开个线程延迟清理，避免内存积累
        let id = id.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(60));
            slot().lock().unwrap().remove(&id);
        });
    }
}

/// 当前活跃（running）任务列表
pub fn list_active() -> Vec<JobInfo> {
    let mut v: Vec<JobInfo> = slot()
        .lock()
        .unwrap()
        .values()
        .filter(|j| j.status == "running")
        .cloned()
        .collect();
    v.sort_by_key(|j| j.started_at);
    v
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
