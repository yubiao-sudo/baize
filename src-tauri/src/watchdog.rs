//! 自主看护 Agent（Watchdog）
//!
//! 对应《白泽自主进化》功能二：设定任务后白泽可自主运行——监视目录/进程/网页内容/定时器，
//! 条件触发 → 自动处理 → 异常自愈（失败重试）→ 完成后通知。
//!
//! 架构：
//!   任务注册表（SQLite watchdog_tasks）→ 触发器轮询（cron/interval/fs/process/threshold）
//!   → 行动引擎（调用 L1 工具执行动作序列，支持 {var} 变量绑定）→ 失败重试（指数退避）
//!   → 通知通道（proactive 卡片 + 执行日志）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::memory::MemoryStore;
use crate::tools::{PermissionClass, Tool, ToolRegistry};

// ───────────────── 数据结构 ─────────────────

/// 触发器定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchdogTrigger {
    /// cron | interval | fs | process | threshold
    pub kind: String,
    /// 具体参数（JSON），由 kind 决定字段
    pub config: Value,
}

/// 动作定义（调用某个工具，或 notify/agent/sleep 内置动作）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchdogAction {
    pub tool: String,
    pub args: Value,
}

/// 看护任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchdogTask {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// 试运行模式：只列出执行计划，不真正执行写/高危动作
    pub dry_run: bool,
    pub triggers: Vec<WatchdogTrigger>,
    pub actions: Vec<WatchdogAction>,
    pub retry_max: u32,
    pub retry_backoff_secs: u64,
    /// always | failure | never
    pub notify_on: String,
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub last_result: Option<String>,
}

/// 单次执行日志
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchdogRun {
    pub id: String,
    pub task_id: String,
    pub task_name: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    /// success | failed | dry_run
    pub status: String,
    pub result: String,
}

/// 触发器运行时状态（用于边缘触发检测，避免重复触发）
struct TaskRuntime {
    cron_next_ms: Option<i64>,
    interval_last_ms: i64,
    fs_snapshot: String,
    fs_initialized: bool,
    state_armed: bool,
    state_initialized: bool,
}

impl Default for TaskRuntime {
    fn default() -> Self {
        Self {
            cron_next_ms: None,
            interval_last_ms: 0,
            fs_snapshot: String::new(),
            fs_initialized: false,
            state_armed: false,
            state_initialized: false,
        }
    }
}

/// 看护状态：内存任务表 + 持久化
pub struct WatchdogState {
    tasks: Mutex<HashMap<String, WatchdogTask>>,
    runtime: Mutex<HashMap<String, TaskRuntime>>,
    store: Arc<MemoryStore>,
    tools: Arc<ToolRegistry>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl WatchdogState {
    pub fn new(store: Arc<MemoryStore>, tools: Arc<ToolRegistry>) -> Arc<Self> {
        let state = Arc::new(Self {
            tasks: Mutex::new(HashMap::new()),
            runtime: Mutex::new(HashMap::new()),
            store,
            tools,
        });
        state.load();
        state
    }

    fn load(&self) {
        let rows = match self.store.list_watchdog_tasks() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[看护] 加载任务失败: {e}");
                return;
            }
        };
        let mut tasks = self.tasks.lock().unwrap();
        let mut runtime = self.runtime.lock().unwrap();
        for (id, data) in rows {
            if let Ok(task) = serde_json::from_str::<WatchdogTask>(&data) {
                tasks.insert(id, task);
            }
        }
        for id in tasks.keys() {
            runtime.entry(id.clone()).or_default();
        }
        println!("[看护] 已加载 {} 个任务", tasks.len());
    }

    fn persist(&self, task: &WatchdogTask) -> Result<(), String> {
        let data = serde_json::to_string(task).map_err(|e| e.to_string())?;
        self.store.upsert_watchdog_task(&task.id, &data)
    }

    fn persist_run(&self, run: &WatchdogRun) -> Result<(), String> {
        let data = serde_json::to_string(run).map_err(|e| e.to_string())?;
        self.store
            .upsert_watchdog_run(&run.id, &run.task_id, run.started_at, &data)
    }

    pub fn register(&self, task: WatchdogTask) -> Result<WatchdogTask, String> {
        if task.name.trim().is_empty() {
            return Err("任务名不能为空".into());
        }
        if task.triggers.is_empty() {
            return Err("至少需要一个触发器".into());
        }
        if task.actions.is_empty() {
            return Err("至少需要一个动作".into());
        }
        self.persist(&task)?;
        self.tasks.lock().unwrap().insert(task.id.clone(), task.clone());
        self.runtime.lock().unwrap().entry(task.id.clone()).or_default();
        Ok(task)
    }

    pub fn delete(&self, id: &str) -> Result<bool, String> {
        let removed = self.tasks.lock().unwrap().remove(id).is_some();
        if removed {
            let _ = self.store.delete_watchdog_task(id);
        }
        self.runtime.lock().unwrap().remove(id);
        Ok(removed)
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, String> {
        let updated = {
            let mut tasks = self.tasks.lock().unwrap();
            let Some(task) = tasks.get_mut(id) else {
                return Ok(false);
            };
            task.enabled = enabled;
            task.clone()
        };
        let _ = self.persist(&updated);
        Ok(true)
    }

    pub fn list(&self) -> Vec<WatchdogTask> {
        self.tasks.lock().unwrap().values().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<WatchdogTask> {
        self.tasks.lock().unwrap().get(id).cloned()
    }

    pub fn list_runs(&self, task_id: &str, limit: usize) -> Vec<WatchdogRun> {
        let limit = limit.clamp(1, 500);
        let rows = match self.store.list_watchdog_runs(task_id, limit) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[看护] 读取执行日志失败: {e}");
                return Vec::new();
            }
        };
        rows.into_iter()
            .filter_map(|(_, _, _, data)| serde_json::from_str::<WatchdogRun>(&data).ok())
            .collect()
    }

    // ---------- 触发器评估 ----------

    /// 评估某任务的所有触发器，返回是否命中 + 触发上下文变量
    fn evaluate(&self, task: &WatchdogTask, rt: &mut TaskRuntime) -> (bool, HashMap<String, String>) {
        for trig in &task.triggers {
            if let Some(vars) = self.eval_trigger(trig, rt) {
                return (true, vars);
            }
        }
        (false, HashMap::new())
    }

    fn eval_trigger(
        &self,
        trig: &WatchdogTrigger,
        rt: &mut TaskRuntime,
    ) -> Option<HashMap<String, String>> {
        let now = now_ms();
        match trig.kind.as_str() {
            "interval" => {
                let secs = trig.config["seconds"]
                    .as_u64()
                    .or_else(|| trig.config["interval"].as_u64())
                    .unwrap_or(60) as i64;
                if now - rt.interval_last_ms >= secs * 1000 {
                    rt.interval_last_ms = now;
                    let mut v = HashMap::new();
                    v.insert("now".into(), now.to_string());
                    return Some(v);
                }
                None
            }
            "cron" => {
                let expr = trig.config["cron"]
                    .as_str()
                    .or_else(|| trig.config["expr"].as_str())
                    .unwrap_or("*/5 * * * *")
                    .to_string();
                if rt.cron_next_ms.is_none() {
                    rt.cron_next_ms = next_cron_ms(&expr, now);
                }
                if let Some(next) = rt.cron_next_ms {
                    if now >= next {
                        rt.cron_next_ms = next_cron_ms(&expr, now);
                        let mut v = HashMap::new();
                        v.insert("now".into(), now.to_string());
                        return Some(v);
                    }
                }
                None
            }
            "fs" => {
                let path = trig.config["path"].as_str().unwrap_or(".").to_string();
                let snapshot = dir_snapshot(&path);
                if !rt.fs_initialized {
                    rt.fs_snapshot = snapshot;
                    rt.fs_initialized = true;
                    return None; // 首次不触发，只建立基线
                }
                if snapshot != rt.fs_snapshot {
                    let old = std::mem::replace(&mut rt.fs_snapshot, snapshot);
                    let mut v = HashMap::new();
                    v.insert("path".into(), path.clone());
                    v.insert("changed".into(), changed_hint(&old, &rt.fs_snapshot));
                    return Some(v);
                }
                None
            }
            "process" => {
                let name = trig.config["name"].as_str().unwrap_or("").to_string();
                let expect = trig.config["expect"].as_str().unwrap_or("running");
                let exists = process_exists(&name);
                let armed = match expect {
                    "stopped" | "not_running" | "absent" => !exists,
                    _ => exists,
                };
                if !rt.state_initialized {
                    rt.state_armed = armed;
                    rt.state_initialized = true;
                    return None;
                }
                if armed && !rt.state_armed {
                    rt.state_armed = true;
                    let mut v = HashMap::new();
                    v.insert("name".into(), name.clone());
                    v.insert("status".into(), expect.to_string());
                    return Some(v);
                }
                rt.state_armed = armed;
                None
            }
            "threshold" => {
                let metric = trig.config["metric"].as_str().unwrap_or("disk_free_pct");
                let op = trig.config["op"].as_str().unwrap_or("<");
                let value = trig.config["value"].as_f64().unwrap_or(10.0);
                let actual = read_metric(metric);
                let armed = match actual {
                    Some(v) if op == "<" => v < value,
                    Some(v) if op == "<=" => v <= value,
                    Some(v) if op == ">" => v > value,
                    Some(v) if op == ">=" => v >= value,
                    Some(v) if op == "==" || op == "=" => (v - value).abs() < f64::EPSILON,
                    _ => false,
                };
                if !rt.state_initialized {
                    rt.state_armed = armed;
                    rt.state_initialized = true;
                    return None;
                }
                if armed && !rt.state_armed {
                    rt.state_armed = true;
                    let mut v = HashMap::new();
                    v.insert("metric".into(), metric.to_string());
                    v.insert("value".into(), actual.map(|x| x.to_string()).unwrap_or_default());
                    return Some(v);
                }
                rt.state_armed = armed;
                None
            }
            _ => None,
        }
    }

    // ---------- 行动引擎 ----------

    /// 执行任务动作序列（带失败重试），返回 (成功与否, 结果摘要)
    fn execute(&self, app: &AppHandle, task: &WatchdogTask, vars: &HashMap<String, String>) -> (bool, String) {
        if task.dry_run {
            let plan: Vec<String> = task.actions.iter().map(|a| a.tool.clone()).collect();
            return (true, format!("试运行（未执行）：{}", plan.join(" → ")));
        }

        let mut results: Vec<String> = Vec::new();
        let mut failed = false;
        for (i, action) in task.actions.iter().enumerate() {
            let bound = bind_value(&action.args, vars);
            let mut last_err = String::new();
            let mut ok = false;
            // 失败重试（指数退避）
            let attempts = (task.retry_max.max(0) + 1) as usize;
            for attempt in 0..attempts {
                if attempt > 0 {
                    let shift = (attempt - 1).min(6);
                    let backoff = (task.retry_backoff_secs.max(1) as u64) * (1u64 << shift);
                    std::thread::sleep(Duration::from_secs(backoff));
                }
                match self.execute_action(app, action, &bound) {
                    Ok(msg) => {
                        ok = true;
                        results.push(format!("[{}] {}", i + 1, msg));
                        break;
                    }
                    Err(e) => last_err = e,
                }
            }
            if !ok {
                failed = true;
                results.push(format!("[{}] 失败: {}", i + 1, last_err));
            }
        }
        (!failed, results.join("\n"))
    }

    fn execute_action(
        &self,
        app: &AppHandle,
        action: &WatchdogAction,
        bound: &Value,
    ) -> Result<String, String> {
        match action.tool.as_str() {
            "notify" => {
                let title = bound["title"].as_str().unwrap_or("白泽看护");
                let body = bound["body"].as_str().or_else(|| bound["msg"].as_str()).unwrap_or("");
                let _ = app.emit(
                    "proactive",
                    json!({
                        "id": uuid::Uuid::new_v4().to_string(),
                        "title": title,
                        "body": body,
                        "files": [],
                        "action": body,
                    }),
                );
                Ok(format!("已通知：{body}"))
            }
            "sleep" => {
                let secs = bound["seconds"].as_u64().or_else(|| bound["secs"].as_u64()).unwrap_or(1);
                std::thread::sleep(Duration::from_secs(secs));
                Ok(format!("等待 {secs}s"))
            }
            "agent" => {
                let task_text = bound["task"]
                    .as_str()
                    .or_else(|| bound["prompt"].as_str())
                    .ok_or("agent 动作缺少 task")?
                    .to_string();
                let handle = app.clone();
                let task_clone = task_text.clone();
                tauri::async_runtime::spawn(async move {
                    let state = handle.state::<crate::AppState>();
                    let _ = crate::agent::Supervisor::new(&handle, state.inner())
                        .run(&task_clone, vec![])
                        .await;
                });
                Ok(format!(
                    "已派发 Agent 任务：{}",
                    task_text.chars().take(40).collect::<String>()
                ))
            }
            tool_name => {
                let tool = self
                    .tools
                    .get(tool_name)
                    .ok_or_else(|| format!("未知工具: {tool_name}"))?;
                let result = tool.run(bound.clone())?;
                let summary = result
                    .get("result")
                    .and_then(|v| v.as_str())
                    .or_else(|| result.get("message").and_then(|v| v.as_str()))
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        let s = result.to_string();
                        s.chars().take(200).collect::<String>()
                    });
                Ok(summary)
            }
        }
    }

    /// 执行一次任务（供轮询与手动 run_now 复用）
    pub fn run_one(&self, app: &AppHandle, task: &WatchdogTask, vars: &HashMap<String, String>) {
        let run_id = uuid::Uuid::new_v4().to_string();
        let started = now_ms();
        let mut run = WatchdogRun {
            id: run_id.clone(),
            task_id: task.id.clone(),
            task_name: task.name.clone(),
            started_at: started,
            finished_at: None,
            status: "running".into(),
            result: String::new(),
        };
        let _ = self.persist_run(&run);

        let (success, summary) = self.execute(app, task, vars);
        run.status = if task.dry_run {
            "dry_run".into()
        } else if success {
            "success".into()
        } else {
            "failed".into()
        };
        run.result = summary.clone();
        run.finished_at = Some(now_ms());
        let _ = self.persist_run(&run);

        if let Some(t) = self.tasks.lock().unwrap().get_mut(&task.id) {
            t.last_run_at = Some(started);
            t.last_result = Some(summary.clone());
            let clone = t.clone();
            let _ = self.persist(&clone);
        }

        let should_notify = match task.notify_on.as_str() {
            "failure" => !success && !task.dry_run,
            "never" | "none" => false,
            _ => true,
        };
        if should_notify {
            let status_label = if task.dry_run {
                "试运行完成"
            } else if success {
                "已完成"
            } else {
                "失败"
            };
            let _ = app.emit(
                "proactive",
                json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "title": "🛡️ 看护任务".to_string(),
                    "body": format!("{}：{}", task.name, status_label),
                    "files": [],
                    "action": summary.chars().take(300).collect::<String>(),
                    "data": summary,
                }),
            );
        }
    }
}

// ---------- 后台轮询循环 ----------

/// 启动看护后台循环（阻塞，放独立线程）
pub fn run_watchdog(state: Arc<WatchdogState>, app: AppHandle) {
    std::thread::spawn(move || loop {
        let due: Vec<WatchdogTask> = {
            let tasks = state.tasks.lock().unwrap();
            tasks.values().filter(|t| t.enabled).cloned().collect()
        };

        for task in due {
            let mut rt = {
                let mut map = state.runtime.lock().unwrap();
                map.entry(task.id.clone()).or_default();
                map.remove(&task.id).unwrap_or_default()
            };
            let (fire, vars) = state.evaluate(&task, &mut rt);
            state.runtime.lock().unwrap().insert(task.id.clone(), rt);

            if fire {
                state.run_one(&app, &task, &vars);
            }
        }

        std::thread::sleep(Duration::from_secs(1));
    });
}

// ---------- 触发器辅助函数 ----------

fn next_cron_ms(expr: &str, after_ms: i64) -> Option<i64> {
    use std::str::FromStr;
    use chrono::TimeZone;
    let field_count = expr.split_whitespace().count();
    let normalized = if field_count == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    };
    let schedule = cron::Schedule::from_str(&normalized).ok()?;
    let after = chrono::Local.timestamp_millis_opt(after_ms).single()?;
    schedule.after(&after).next().map(|d| d.timestamp_millis())
}

fn dir_snapshot(path: &str) -> String {
    let mut items: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let (len, mtime) = match e.metadata() {
                Ok(m) => (
                    m.len(),
                    m.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                ),
                Err(_) => (0, 0),
            };
            items.push(format!("{name}|{len}|{mtime}"));
        }
    }
    items.sort();
    items.join("\n")
}

fn changed_hint(old: &str, new: &str) -> String {
    let old_set: std::collections::HashSet<&str> = old.lines().collect();
    let new_set: std::collections::HashSet<&str> = new.lines().collect();
    let added: Vec<&str> = new_set.difference(&old_set).cloned().collect();
    let removed: Vec<&str> = old_set.difference(&new_set).cloned().collect();
    if !added.is_empty() {
        format!("新增 {} 项", added.len())
    } else if !removed.is_empty() {
        format!("移除 {} 项", removed.len())
    } else {
        "内容变更".to_string()
    }
}

fn process_exists(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let (stdout, _, code) = run_ps(&format!(
        "if (Get-Process -Name '{name}' -ErrorAction SilentlyContinue) {{ 'FOUND' }} else {{ 'NONE' }}"
    ));
    code == Some(0) && stdout.contains("FOUND")
}

fn read_metric(metric: &str) -> Option<f64> {
    let (stdout, _, _) = match metric {
        "disk_free_pct" | "disk_free_percent" => run_ps(
            "$d = Get-PSDrive C; if(($d.Used+$d.Free) -gt 0){ [math]::Round(($d.Free/($d.Used+$d.Free))*100, 2) } else { '0' }",
        ),
        "disk_free_gb" => run_ps("$d = Get-PSDrive C; [math]::Round($d.Free/1GB, 2)"),
        "mem_available_mb" => run_ps(
            "$os = Get-CimInstance Win32_OperatingSystem; [math]::Round($os.FreePhysicalMemory/1KB, 0)",
        ),
        _ => return None,
    };
    stdout.trim().parse::<f64>().ok()
}

fn run_ps(command: &str) -> (String, String, Option<i32>) {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = match std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", command])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (String::new(), format!("启动 PowerShell 失败: {e}"), None),
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_string(&mut out);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut err);
    }
    let code = child.wait().ok().and_then(|st| st.code());
    (out, err, code)
}

/// 递归变量绑定：把字符串中的 {var} 替换为对应值
fn bind_value(v: &Value, vars: &HashMap<String, String>) -> Value {
    match v {
        Value::String(s) => {
            let mut out = s.clone();
            for (k, val) in vars {
                out = out.replace(&format!("{{{k}}}"), val);
            }
            Value::String(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| bind_value(x, vars)).collect()),
        Value::Object(o) => {
            let mut m = serde_json::Map::new();
            for (k, val) in o {
                m.insert(k.clone(), bind_value(val, vars));
            }
            Value::Object(m)
        }
        other => other.clone(),
    }
}

fn uid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn parse_triggers(v: &Value) -> Result<Vec<WatchdogTrigger>, String> {
    let arr = v.as_array().ok_or("triggers 必须为数组")?;
    let mut out = Vec::new();
    for item in arr {
        let kind = item["type"]
            .as_str()
            .or_else(|| item["kind"].as_str())
            .ok_or_else(|| format!("触发器缺少 type: {item}"))?
            .to_string();
        let mut config = item.clone();
        let obj = config.as_object_mut().ok_or("触发器必须为对象")?;
        obj.remove("type");
        obj.remove("kind");
        out.push(WatchdogTrigger { kind, config });
    }
    Ok(out)
}

fn parse_actions(v: &Value) -> Result<Vec<WatchdogAction>, String> {
    let arr = v.as_array().ok_or("actions 必须为数组")?;
    let mut out = Vec::new();
    for item in arr {
        let tool = item["tool"].as_str().ok_or_else(|| format!("动作缺少 tool: {item}"))?.to_string();
        let args = item.get("args").cloned().unwrap_or_else(|| json!({}));
        out.push(WatchdogAction { tool, args });
    }
    Ok(out)
}

// ───────────────── 工具 ─────────────────

/// watchdog_register：注册一个看护任务
pub struct WatchdogRegisterTool {
    state: Arc<WatchdogState>,
}

impl WatchdogRegisterTool {
    pub fn new(state: Arc<WatchdogState>) -> Self {
        Self { state }
    }
}

impl Tool for WatchdogRegisterTool {
    fn name(&self) -> &str {
        "watchdog_register"
    }
    fn description(&self) -> &str {
        "注册一个自主看护任务，白泽会在后台持续监视并在条件触发时自动执行动作。触发器 type 支持：cron(cron 表达式，5 段本地时间)、interval(seconds)、fs(path 目录变化)、process(name 进程名, expect running/stopped)、threshold(metric 如 disk_free_pct/mem_available_mb, op 如 < / >)。动作 tool 为任意工具名或内置 notify/agent/sleep。动作 args 支持 {var} 变量绑定（如 {path} {name} {value} {now}）。notify_on: always|failure|never；dry_run=true 只列计划不真正执行。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "任务名" },
                "triggers": {
                    "type": "array",
                    "description": "触发器列表（至少一个）。每项含 type 及对应字段，如 {\"type\":\"cron\",\"cron\":\"*/10 * * * *\"}、{\"type\":\"interval\",\"seconds\":60}、{\"type\":\"fs\",\"path\":\"D:/Downloads\"}、{\"type\":\"process\",\"name\":\"chrome\",\"expect\":\"running\"}、{\"type\":\"threshold\",\"metric\":\"disk_free_pct\",\"op\":\"<\",\"value\":10}",
                    "items": { "type": "object" }
                },
                "actions": {
                    "type": "array",
                    "description": "动作列表（至少一个）。每项 {tool, args}，如 {\"tool\":\"notify\",\"args\":{\"title\":\"x\",\"body\":\"y\"}} 或 {\"tool\":\"ps_exec\",\"args\":{\"command\":\"...\"}}",
                    "items": { "type": "object" }
                },
                "retry_max": { "type": "number", "description": "失败重试次数，默认 0" },
                "retry_backoff_secs": { "type": "number", "description": "重试退避秒数，默认 5" },
                "notify_on": { "type": "string", "enum": ["always", "failure", "never"], "description": "通知时机，默认 always" },
                "dry_run": { "type": "boolean", "description": "试运行，默认 false" }
            },
            "required": ["name", "triggers", "actions"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?.to_string();
        let triggers = parse_triggers(&args["triggers"])?;
        let actions = parse_actions(&args["actions"])?;
        let id = uid();
        let task = WatchdogTask {
            id: id.clone(),
            name,
            enabled: true,
            dry_run: args["dry_run"].as_bool().unwrap_or(false),
            triggers,
            actions,
            retry_max: args["retry_max"].as_u64().unwrap_or(0) as u32,
            retry_backoff_secs: args["retry_backoff_secs"].as_u64().unwrap_or(5),
            notify_on: args["notify_on"].as_str().unwrap_or("always").to_string(),
            created_at: now_ms(),
            last_run_at: None,
            last_result: None,
        };
        let task = self.state.register(task)?;
        Ok(json!({ "ok": true, "id": id, "name": task.name }))
    }
}

/// watchdog_list：列出看护任务
pub struct WatchdogListTool {
    state: Arc<WatchdogState>,
}

impl WatchdogListTool {
    pub fn new(state: Arc<WatchdogState>) -> Self {
        Self { state }
    }
}

impl Tool for WatchdogListTool {
    fn name(&self) -> &str {
        "watchdog_list"
    }
    fn description(&self) -> &str {
        "列出所有自主看护任务及其状态（是否启用、上次执行结果）"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let tasks: Vec<Value> = self
            .state
            .list()
            .into_iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "name": t.name,
                    "enabled": t.enabled,
                    "dry_run": t.dry_run,
                    "triggers": t.triggers.iter().map(|tr| json!({"type": tr.kind, "config": tr.config})).collect::<Vec<_>>(),
                    "actions_count": t.actions.len(),
                    "notify_on": t.notify_on,
                    "last_run_at": t.last_run_at,
                    "last_result": t.last_result,
                })
            })
            .collect();
        Ok(json!(tasks))
    }
}

/// watchdog_run：手动触发一次
pub struct WatchdogRunTool {
    app: AppHandle,
    state: Arc<WatchdogState>,
}

impl WatchdogRunTool {
    pub fn new(app: AppHandle, state: Arc<WatchdogState>) -> Self {
        Self { app, state }
    }
}

impl Tool for WatchdogRunTool {
    fn name(&self) -> &str {
        "watchdog_run"
    }
    fn description(&self) -> &str {
        "立即手动触发一次看护任务（按 id）。用于测试或临时执行"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 id（见 watchdog_list）" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        let task = self.state.get(id).ok_or("任务不存在")?;
        let vars = HashMap::new();
        self.state.run_one(&self.app, &task, &vars);
        Ok(json!({ "ok": true, "id": id, "triggered": true }))
    }
}

/// watchdog_pause / resume / delete 组合工具
pub enum WatchdogToggle {
    Pause,
    Resume,
}

pub struct WatchdogToggleTool {
    state: Arc<WatchdogState>,
    kind: WatchdogToggle,
}

impl WatchdogToggleTool {
    pub fn new(state: Arc<WatchdogState>, kind: WatchdogToggle) -> Self {
        Self { state, kind }
    }
}

impl Tool for WatchdogToggleTool {
    fn name(&self) -> &str {
        match self.kind {
            WatchdogToggle::Pause => "watchdog_pause",
            WatchdogToggle::Resume => "watchdog_resume",
        }
    }
    fn description(&self) -> &str {
        match self.kind {
            WatchdogToggle::Pause => "暂停一个看护任务（暂停后不再触发）",
            WatchdogToggle::Resume => "恢复一个已暂停的看护任务",
        }
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 id" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        let enabled = matches!(self.kind, WatchdogToggle::Resume);
        let updated = self.state.set_enabled(id, enabled)?;
        Ok(json!({ "ok": true, "id": id, "updated": updated, "enabled": enabled }))
    }
}

/// watchdog_delete：删除看护任务
pub struct WatchdogDeleteTool {
    state: Arc<WatchdogState>,
}

impl WatchdogDeleteTool {
    pub fn new(state: Arc<WatchdogState>) -> Self {
        Self { state }
    }
}

impl Tool for WatchdogDeleteTool {
    fn name(&self) -> &str {
        "watchdog_delete"
    }
    fn description(&self) -> &str {
        "删除一个看护任务"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "任务 id" }
            },
            "required": ["id"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let id = args["id"].as_str().ok_or("缺少参数 id")?;
        let removed = self.state.delete(id)?;
        Ok(json!({ "ok": true, "id": id, "removed": removed }))
    }
}

/// watchdog_logs：查看执行日志
pub struct WatchdogLogsTool {
    state: Arc<WatchdogState>,
}

impl WatchdogLogsTool {
    pub fn new(state: Arc<WatchdogState>) -> Self {
        Self { state }
    }
}

impl Tool for WatchdogLogsTool {
    fn name(&self) -> &str {
        "watchdog_logs"
    }
    fn description(&self) -> &str {
        "查看看护任务的执行日志（历史记录），不传 id 则返回全部"
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
        let task_id = args["id"].as_str().unwrap_or("");
        let limit = args["limit"].as_u64().unwrap_or(20) as usize;
        let runs: Vec<Value> = self
            .state
            .list_runs(task_id, limit)
            .into_iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "task_id": r.task_id,
                    "task_name": r.task_name,
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