//! 主动唤醒（白龙马 TICK 常驻循环的简化版）
//!
//! 后台监听目录变化，检测到新增文件后「主动」向用户推送提醒卡片（ACUI 理念）。
//! 监听目录可用环境变量 BAIZE_WATCH_DIR 配置，默认 Downloads。

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use chrono::TimeZone;
use cron::Schedule;
use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::memory::MemoryStore;
use crate::tools::{PermissionClass, Tool};

/// 定时提醒工具：到点推送 proactive 提醒卡片（用于「X 分钟后提醒我」类请求）
pub struct ReminderTool {
    app: AppHandle,
}

impl ReminderTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for ReminderTool {
    fn name(&self) -> &str {
        "set_reminder"
    }
    fn description(&self) -> &str {
        "设定一个定时提醒：delay_seconds 秒后向用户推送提醒卡片（用于「X 分钟/小时后提醒我」类请求）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "delay_seconds": { "type": "number", "description": "多少秒后提醒" },
                "message": { "type": "string", "description": "提醒内容" }
            },
            "required": ["delay_seconds", "message"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let delay = (args["delay_seconds"].as_f64().unwrap_or(60.0).max(1.0)) as u64;
        let message = args["message"].as_str().unwrap_or("").to_string();
        if message.is_empty() {
            return Err("缺少参数 message".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let app = self.app.clone();
        let body = message.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(delay)).await;
            let _ = app.emit(
                "proactive",
                json!({
                    "id": id,
                    "title": "⏰ 定时提醒",
                    "body": body,
                    "files": [],
                    "action": format!("定时提醒已到：{}", message),
                }),
            );
        });
        Ok(json!({ "ok": true, "delay_seconds": delay }))
    }
}


/// 运行主动监听循环（阻塞，建议放独立线程）
pub fn run(app: AppHandle) -> Result<(), String> {
    let watch_dir = watch_dir();
    if !watch_dir.exists() {
        eprintln!("[主动唤醒] 监听目录不存在，跳过: {}", watch_dir.display());
        return Ok(());
    }

    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(tx).map_err(|e| e.to_string())?;
    watcher
        .watch(&watch_dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("监听目录失败: {e}"))?;

    println!("[主动唤醒] 监听目录: {}", watch_dir.display());

    let mut pending: Vec<String> = Vec::new();

    loop {
        // 等第一个事件
        let first = match rx.recv() {
            Ok(Ok(e)) => e,
            Ok(Err(e)) => {
                eprintln!("[主动唤醒] 监听错误: {e}");
                continue;
            }
            Err(_) => break,
        };
        collect_new_files(&first, &mut pending);

        // 去抖：800ms 内无新事件视为一批
        loop {
            match rx.recv_timeout(Duration::from_millis(800)) {
                Ok(Ok(e)) => collect_new_files(&e, &mut pending),
                Ok(Err(_)) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }

        if !pending.is_empty() {
            let files = pending.clone();
            pending.clear();
            let id = uuid::Uuid::new_v4().to_string();
            println!("[主动唤醒] 检测到新增文件: {}", files.join(", "));
            let _ = app.emit(
                "proactive",
                json!({
                    "id": id,
                    "title": "白泽提醒",
                    "body": format!("检测到「{}」新增 {} 个文件", watch_dir.display(), files.len()),
                    "files": files,
                }),
            );
        }
    }

    Ok(())
}

fn collect_new_files(event: &notify::Event, pending: &mut Vec<String>) {
    if matches!(event.kind, notify::EventKind::Create(_)) {
        for p in &event.paths {
            if let Some(name) = p.file_name() {
                let s = name.to_string_lossy().to_string();
                if !pending.contains(&s) {
                    pending.push(s);
                }
            }
        }
    }
}

/// 监听目录：BAIZE_WATCH_DIR 优先，否则 Downloads
fn watch_dir() -> PathBuf {
    if let Ok(d) = std::env::var("BAIZE_WATCH_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(up) = std::env::var("USERPROFILE") {
            return PathBuf::from(up).join("Downloads");
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Downloads");
        }
    }
    PathBuf::from(".")
}

// ───────────────── 定时任务调度（cron） ─────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub id: String,
    /// 显示名（自然语言任务摘录 / 命令摘要）
    #[serde(default)]
    pub title: String,
    pub cron_expr: String,
    /// "command"（PowerShell 直连）| "agent"（交给白泽 Agent 的自然语言任务）
    #[serde(default = "default_task_type")]
    pub task_type: String,
    /// 载荷：command 类型为 PowerShell 命令；agent 类型为自然语言任务描述
    pub command: String,
    pub created_at: i64,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub last_result: Option<String>,
}

/// 单次执行日志记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRun {
    pub id: String,
    pub job_id: String,
    pub job_title: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// "running" | "success" | "failed"
    pub status: String,
    pub result: String,
}

fn default_task_type() -> String {
    "command".to_string()
}

struct JobEntry {
    job: ScheduledJob,
    next_run_ms: i64,
}

/// 调度器状态：内存任务表 + 持久化 + 唤醒通道
pub struct SchedulerState {
    jobs: Mutex<HashMap<String, JobEntry>>,
    store: Arc<MemoryStore>,
    wake_tx: std::sync::mpsc::Sender<()>,
    wake_rx: Mutex<std::sync::mpsc::Receiver<()>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 解析 cron 表达式。用户按标准 5 段（分 时 日 月 周）书写时，自动补秒为 6 段；6/7 段原样解析。
fn parse_cron(expr: &str) -> Result<Schedule, String> {
    let field_count = expr.split_whitespace().count();
    let normalized = if field_count == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    };
    Schedule::from_str(&normalized).map_err(|e| format!("cron 表达式无效: {expr}（{e}）"))
}

/// 计算下一次触发时间（Unix 毫秒）。cron 按本地时区解释。
fn next_run_ms(expr: &str, after_ms: i64) -> Option<i64> {
    let schedule = parse_cron(expr).ok()?;
    let after = chrono::Local.timestamp_millis_opt(after_ms).single()?;
    schedule.after(&after).next().map(|d| d.timestamp_millis())
}

impl SchedulerState {
    pub fn new(store: Arc<MemoryStore>) -> Arc<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let state = Arc::new(Self {
            jobs: Mutex::new(HashMap::new()),
            store,
            wake_tx: tx,
            wake_rx: Mutex::new(rx),
        });
        state.load();
        state
    }

    /// 从持久化恢复任务
    fn load(&self) {
        let rows = match self.store.list_scheduled_jobs() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[定时任务] 加载失败: {e}");
                return;
            }
        };
        let now = now_ms();
        let mut jobs = self.jobs.lock().unwrap();
        for (id, data) in rows {
            if let Ok(job) = serde_json::from_str::<ScheduledJob>(&data) {
                let next = next_run_ms(&job.cron_expr, now).unwrap_or(i64::MAX);
                jobs.insert(id, JobEntry { job, next_run_ms: next });
            }
        }
    }

    /// 兼容旧接口：新增一条「command」类型定时任务
    pub fn add_job(&self, cron_expr: &str, command: &str) -> Result<ScheduledJob, String> {
        self.add_job_full(cron_expr, "", "command", command)
    }

    /// 通用新增：按任务类型（command / agent）创建定时任务
    pub fn add_job_full(
        &self,
        cron_expr: &str,
        title: &str,
        task_type: &str,
        task: &str,
    ) -> Result<ScheduledJob, String> {
        validate_cron(cron_expr)?;
        if task.trim().is_empty() {
            return Err("任务内容不能为空".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        let title = normalize_title(title, task);
        let job = ScheduledJob {
            id: id.clone(),
            title,
            cron_expr: cron_expr.to_string(),
            task_type: task_type.to_string(),
            command: task.to_string(),
            created_at: now,
            enabled: true,
            last_run_at: None,
            last_result: None,
        };
        let next_run = next_run_ms(cron_expr, now).unwrap_or(i64::MAX);
        self.persist(&job)?;
        self.jobs
            .lock()
            .unwrap()
            .insert(id, JobEntry { job: job.clone(), next_run_ms: next_run });
        let _ = self.wake_tx.send(());
        Ok(job)
    }

    /// 编辑一条任务（cron / 标题 / 类型 / 内容），重算下次触发
    pub fn update_job(
        &self,
        id: &str,
        cron_expr: &str,
        title: &str,
        task_type: &str,
        task: &str,
    ) -> Result<ScheduledJob, String> {
        validate_cron(cron_expr)?;
        if task.trim().is_empty() {
            return Err("任务内容不能为空".into());
        }
        let updated = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(entry) = jobs.get_mut(id) else {
                return Err("任务不存在".into());
            };
            entry.job.cron_expr = cron_expr.to_string();
            entry.job.title = normalize_title(title, task);
            entry.job.task_type = task_type.to_string();
            entry.job.command = task.to_string();
            entry.next_run_ms = next_run_ms(cron_expr, now_ms()).unwrap_or(i64::MAX);
            entry.job.clone()
        };
        self.persist(&updated)?;
        let _ = self.wake_tx.send(());
        Ok(updated)
    }

    pub fn list_jobs(&self) -> Vec<ScheduledJob> {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .map(|e| e.job.clone())
            .collect()
    }

    pub fn cancel_job(&self, id: &str) -> Result<bool, String> {
        let removed = self.jobs.lock().unwrap().remove(id).is_some();
        if removed {
            let _ = self.store.delete_scheduled_job(id);
        }
        let _ = self.wake_tx.send(());
        Ok(removed)
    }

    /// 暂停 / 恢复任务（enabled=false 暂停，true 恢复）
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, String> {
        let updated = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(entry) = jobs.get_mut(id) else {
                return Ok(false);
            };
            entry.job.enabled = enabled;
            if enabled {
                entry.next_run_ms = next_run_ms(&entry.job.cron_expr, now_ms()).unwrap_or(i64::MAX);
            }
            entry.job.clone()
        };
        let _ = self.persist(&updated);
        let _ = self.wake_tx.send(());
        Ok(true)
    }

    /// 查询执行日志（job_id 传空串查全部）
    pub fn list_runs(&self, job_id: &str, limit: usize) -> Vec<JobRun> {
        let limit = limit.clamp(1, 500);
        let rows = match self.store.list_job_runs(job_id, limit) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[定时任务] 读取执行日志失败: {e}");
                return Vec::new();
            }
        };
        rows.into_iter()
            .filter_map(|(_, _, _, data)| serde_json::from_str::<JobRun>(&data).ok())
            .collect()
    }

    /// 清空某任务的执行日志
    pub fn clear_runs(&self, job_id: &str) -> Result<usize, String> {
        self.store.clear_job_runs(job_id)
    }

    /// 最早的下一次触发时间（毫秒），无任务返回 None
    fn next_due_ms(&self) -> Option<i64> {
        self.jobs
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.job.enabled)
            .map(|e| e.next_run_ms)
            .min()
    }

    fn persist(&self, job: &ScheduledJob) -> Result<(), String> {
        let data = serde_json::to_string(job).map_err(|e| e.to_string())?;
        self.store.upsert_scheduled_job(&job.id, &data)
    }

    fn persist_run(&self, run: &JobRun) -> Result<(), String> {
        let data = serde_json::to_string(run).map_err(|e| e.to_string())?;
        self.store
            .upsert_job_run(&run.id, &run.job_id, run.started_at, &data)
    }

    /// 抽取当前到期的任务，并立即推进它们的下次触发时间（避免重复触发）
    fn take_due(&self, now_ms: i64) -> Vec<ScheduledJob> {
        let mut jobs = self.jobs.lock().unwrap();
        let mut due = Vec::new();
        for entry in jobs.values_mut() {
            if entry.job.enabled && entry.next_run_ms <= now_ms {
                entry.job.last_run_at = Some(now_ms);
                entry.next_run_ms = next_run_ms(&entry.job.cron_expr, now_ms).unwrap_or(i64::MAX);
                due.push(entry.job.clone());
            }
        }
        drop(jobs);
        for job in &due {
            let _ = self.persist(job);
        }
        due
    }

    /// 执行一条到期任务：按类型分发
    fn execute_job(self: &Arc<Self>, app: &AppHandle, job: &ScheduledJob) {
        if job.task_type == "agent" {
            self.execute_agent_job(app, job);
        } else {
            self.execute_command_job(app, job);
        }
    }

    /// 命令型任务：PowerShell 直连（同步）
    fn execute_command_job(self: &Arc<Self>, app: &AppHandle, job: &ScheduledJob) {
        let run_id = uuid::Uuid::new_v4().to_string();
        let started = now_ms();
        let _ = self.persist_run(&JobRun {
            id: run_id.clone(),
            job_id: job.id.clone(),
            job_title: job.title.clone(),
            started_at: started,
            finished_at: None,
            status: "running".into(),
            result: String::new(),
        });

        let (stdout, stderr, code) = run_powershell(&job.command);
        let summary = summarize_result(stdout, stderr, code);
        let status = if code == Some(0) { "success" } else { "failed" };
        let finished = now_ms();

        let _ = self.persist_run(&JobRun {
            id: run_id,
            job_id: job.id.clone(),
            job_title: job.title.clone(),
            started_at: started,
            finished_at: Some(finished),
            status: status.to_string(),
            result: summary.clone(),
        });
        self.set_last_result(&job.id, &summary);

        let _ = app.emit(
            "proactive",
            json!({
                "id": uuid::Uuid::new_v4().to_string(),
                "title": "⏰ 定时任务完成",
                "body": format!("{}：{}", display_title(&job.title), status_label(status)),
                "files": [],
                "action": summary.chars().take(300).collect::<String>(),
                "data": summary,
            }),
        );
    }

    /// Agent 型任务：交给白泽 Agent 的自然语言任务（异步）
    fn execute_agent_job(self: &Arc<Self>, app: &AppHandle, job: &ScheduledJob) {
        let run_id = uuid::Uuid::new_v4().to_string();
        let started = now_ms();
        let _ = self.persist_run(&JobRun {
            id: run_id.clone(),
            job_id: job.id.clone(),
            job_title: job.title.clone(),
            started_at: started,
            finished_at: None,
            status: "running".into(),
            result: String::new(),
        });

        let handle = app.clone();
        let task = job.command.clone();
        let title = job.title.clone();
        let job_id = job.id.clone();
        let self_arc = self.clone();
        tauri::async_runtime::spawn(async move {
            // 复位取消标志：用户停止主对话后残留的脏标志不应吞掉定时任务
            // （stop 的意图是终止当时正在跑的对话，不是终止之后才触发的任务）
            crate::tools::clear_global_cancel();
            let state = handle.state::<crate::AppState>();
            state.cancel.store(false, std::sync::atomic::Ordering::SeqCst);
            let result = crate::agent::Supervisor::new(&handle, state.inner())
                .run(&task, vec![])
                .await;
            let (status, result_text) = match result {
                Ok(s) => ("success".to_string(), truncate(s, 2000)),
                Err(e) => ("failed".to_string(), format!("执行失败：{e}")),
            };
            let finished = now_ms();
            let _ = self_arc.persist_run(&JobRun {
                id: run_id,
                job_id: job_id.clone(),
                job_title: title.clone(),
                started_at: started,
                finished_at: Some(finished),
                status: status.clone(),
                result: result_text.clone(),
            });
            self_arc.set_last_result(&job_id, &result_text);

            let _ = handle.emit(
                "proactive",
                json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "title": "⏰ 定时任务完成",
                    "body": format!("{}：{}", display_title(&title), status_label(&status)),
                    "files": [],
                    "action": result_text.chars().take(300).collect::<String>(),
                    "data": result_text,
                }),
            );
        });
    }

    /// 更新任务最近一次执行结果（内存 + 持久化）
    fn set_last_result(&self, job_id: &str, result: &str) {
        let updated = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(entry) = jobs.get_mut(job_id) else {
                return;
            };
            entry.job.last_result = Some(result.to_string());
            entry.job.clone()
        };
        let _ = self.persist(&updated);
    }
}

/// 后台调度循环：阻塞运行，放独立线程
pub fn run_schedule(state: Arc<SchedulerState>, app: AppHandle) {
    std::thread::spawn(move || {
        loop {
            let now = now_ms();
            let due = state.take_due(now);
            for job in due {
                state.execute_job(&app, &job);
            }

            let wait_ms = match state.next_due_ms() {
                Some(next) => (next - now_ms()).clamp(1000, 24 * 3600 * 1000),
                None => 60 * 1000,
            };
            let _ = state
                .wake_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(wait_ms as u64));
        }
    });
}

fn summarize_result(stdout: String, stderr: String, code: Option<i32>) -> String {
    let mut out = format!("exit_code={}\n", code.unwrap_or(-1));
    if !stdout.is_empty() {
        out.push_str("[stdout]\n");
        out.push_str(&truncate(stdout, 2000));
        out.push('\n');
    }
    if !stderr.is_empty() {
        out.push_str("[stderr]\n");
        out.push_str(&truncate(stderr, 2000));
    }
    out
}

fn truncate(s: String, max: usize) -> String {
    if s.chars().count() > max {
        let t: String = s.chars().take(max).collect();
        format!("{t}\n...(已截断)")
    } else {
        s
    }
}

fn validate_cron(expr: &str) -> Result<(), String> {
    parse_cron(expr).map(|_| ())
}

fn normalize_title(title: &str, task: &str) -> String {
    let t = title.trim();
    if !t.is_empty() {
        t.to_string()
    } else {
        task.chars().take(30).collect()
    }
}

fn display_title(title: &str) -> String {
    if title.trim().is_empty() {
        "定时任务".to_string()
    } else {
        title.chars().take(40).collect()
    }
}

fn status_label(status: &str) -> &'static str {
    if status == "success" {
        "已完成"
    } else {
        "失败"
    }
}

fn run_powershell(command: &str) -> (String, String, Option<i32>) {
    use std::io::Read;
    use std::process::Stdio;

    let mut child = match crate::tools::silent_command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (String::new(), format!("启动 PowerShell 失败: {e}"), None),
    };

    let stdout_reader = child.stdout.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_reader = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    let status = loop {
        if let Ok(Some(st)) = child.try_wait() {
            break st;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            let out = stdout_reader.and_then(|h| h.join().ok()).unwrap_or_default();
            return (out, "命令超时（300s），已终止".to_string(), None);
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    let stdout = stdout_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = stderr_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    (stdout, stderr, status.code())
}

// ───────────────── 定时任务工具 ─────────────────

pub struct ScheduleTool {
    state: Arc<SchedulerState>,
}

impl ScheduleTool {
    pub fn new(state: Arc<SchedulerState>) -> Self {
        Self { state }
    }
}

impl Tool for ScheduleTool {
    fn name(&self) -> &str {
        "schedule"
    }
    fn description(&self) -> &str {
        "创建一条定时任务：按 cron 表达式（5 段：分 时 日 月 周，本地时间，如 \"0 9 * * *\" 每天 9 点）到点自动执行。给 task 传自然语言任务（交给白泽 Agent），或给 command 传 PowerShell 命令直接执行"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "cron_expr": { "type": "string", "description": "cron 表达式，5 段：分 时 日 月 周" },
                "task": { "type": "string", "description": "到点执行的自然语言任务（交给白泽 Agent，优先于 command）" },
                "command": { "type": "string", "description": "到点直接执行的 PowerShell 命令（未提供 task 时使用）" }
            },
            "required": ["cron_expr"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let expr = args["cron_expr"].as_str().ok_or("缺少参数 cron_expr")?;
        if let Some(task) = args["task"].as_str().filter(|s| !s.trim().is_empty()) {
            let job = self.state.add_job_full(expr, "", "agent", task)?;
            return Ok(json!({ "ok": true, "id": job.id, "cron_expr": expr, "task_type": "agent" }));
        }
        let command = args["command"].as_str().ok_or("缺少参数 task 或 command")?;
        let job = self.state.add_job(expr, command)?;
        Ok(json!({ "ok": true, "id": job.id, "cron_expr": expr, "task_type": "command" }))
    }
}

pub struct ScheduleListTool {
    state: Arc<SchedulerState>,
}

impl ScheduleListTool {
    pub fn new(state: Arc<SchedulerState>) -> Self {
        Self { state }
    }
}

impl Tool for ScheduleListTool {
    fn name(&self) -> &str {
        "schedule_list"
    }
    fn description(&self) -> &str {
        "列出所有定时任务及上次执行结果"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let jobs: Vec<Value> = self
            .state
            .list_jobs()
            .into_iter()
            .map(|j| {
                json!({
                    "id": j.id,
                    "title": j.title,
                    "cron_expr": j.cron_expr,
                    "task_type": j.task_type,
                    "command": j.command,
                    "enabled": j.enabled,
                    "last_run_at": j.last_run_at,
                    "last_result": j.last_result,
                })
            })
            .collect();
        Ok(json!(jobs))
    }
}

pub struct ScheduleCancelTool {
    state: Arc<SchedulerState>,
}

impl ScheduleCancelTool {
    pub fn new(state: Arc<SchedulerState>) -> Self {
        Self { state }
    }
}

impl Tool for ScheduleCancelTool {
    fn name(&self) -> &str {
        "schedule_cancel"
    }
    fn description(&self) -> &str {
        "取消（删除）一条定时任务"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 id（见 schedule_list）" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        let removed = self.state.cancel_job(id)?;
        Ok(json!({ "ok": true, "id": id, "removed": removed }))
    }
}

/// 暂停 / 恢复定时任务（调度开关）
pub enum SetEnabledKind {
    Pause,
    Resume,
}

pub struct ScheduleSetEnabledTool {
    state: Arc<SchedulerState>,
    kind: SetEnabledKind,
}

impl ScheduleSetEnabledTool {
    pub fn new(state: Arc<SchedulerState>, kind: SetEnabledKind) -> Self {
        Self { state, kind }
    }
}

impl Tool for ScheduleSetEnabledTool {
    fn name(&self) -> &str {
        match self.kind {
            SetEnabledKind::Pause => "schedule_pause",
            SetEnabledKind::Resume => "schedule_resume",
        }
    }
    fn description(&self) -> &str {
        match self.kind {
            SetEnabledKind::Pause => "暂停一条定时任务（暂停后不再触发）",
            SetEnabledKind::Resume => "恢复一条已暂停的定时任务",
        }
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 id（见 schedule_list）" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        let enabled = matches!(self.kind, SetEnabledKind::Resume);
        let updated = self.state.set_enabled(id, enabled)?;
        Ok(json!({ "ok": true, "id": id, "updated": updated, "enabled": enabled }))
    }
}

pub struct ScheduleLogsTool {
    state: Arc<SchedulerState>,
}

impl ScheduleLogsTool {
    pub fn new(state: Arc<SchedulerState>) -> Self {
        Self { state }
    }
}

impl Tool for ScheduleLogsTool {
    fn name(&self) -> &str {
        "schedule_logs"
    }
    fn description(&self) -> &str {
        "查看定时任务的执行日志（历史记录），不传 id 则返回全部任务最近日志"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 id（缺省查全部）" },
                "limit": { "type": "number", "description": "返回条数上限，默认 20" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let job_id = args["id"].as_str().unwrap_or("");
        let limit = args["limit"].as_u64().unwrap_or(20) as usize;
        let runs: Vec<Value> = self
            .state
            .list_runs(job_id, limit)
            .into_iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "job_id": r.job_id,
                    "job_title": r.job_title,
                    "started_at": r.started_at,
                    "finished_at": r.finished_at,
                    "status": r.status,
                    "result": r.result,
                })
            })
            .collect();
        Ok(json!(runs))
    }
}
