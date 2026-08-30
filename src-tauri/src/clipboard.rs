//! 剪贴板：读写系统剪贴板文本 + 历史记录管理
//!
//! 基于 arboard，跨平台统一 API。Windows 下剪贴板是全局对象，
//! arboard 仅在每次操作时打开，避免多实例抢占。
//! 历史记录在进程内存中维护（最新在前，去重限量），并有一个后台监听线程
//! 轮询剪贴板变化（外部复制也会被记录），变化时向前端推送 `clipboard-changed` 事件。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::tools::{PermissionClass, Tool};

/// 历史记录上限
const MAX_HISTORY: usize = 50;
/// 后台监听轮询间隔（毫秒）
const MONITOR_INTERVAL_MS: u64 = 800;

/// 剪贴板历史条目
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClipEntry {
    pub text: String,
    /// 记录时刻（毫秒时间戳）
    pub ts: i64,
    /// 字符数
    pub len: usize,
}

static HISTORY: OnceLock<Mutex<VecDeque<ClipEntry>>> = OnceLock::new();

fn history() -> &'static Mutex<VecDeque<ClipEntry>> {
    HISTORY.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 读取当前系统剪贴板文本；失败返回 None（供监听线程静默忽略）
fn read_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

/// 写入剪贴板文本
fn set_text(text: &str) -> Result<(), String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|e| format!("打开剪贴板失败: {e}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| format!("写入剪贴板失败: {e}"))
}

/// 记录一条剪贴板历史（去空、去重、限量，最新在前）
pub fn record(text: &str) {
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    let mut h = history().lock().unwrap();
    // 与最新一条相同则跳过
    if let Some(front) = h.front() {
        if front.text == t {
            return;
        }
    }
    // 移除历史中相同文本的旧条目
    h.retain(|e| e.text != t);
    h.push_front(ClipEntry {
        text: t.to_string(),
        ts: now_ms(),
        len: t.chars().count(),
    });
    if h.len() > MAX_HISTORY {
        h.truncate(MAX_HISTORY);
    }
}

/// 历史列表（最新在前）
pub fn list_entries() -> Vec<ClipEntry> {
    history().lock().unwrap().iter().cloned().collect()
}

/// 清空历史，返回清除条数
pub fn clear_history() -> usize {
    let mut h = history().lock().unwrap();
    let n = h.len();
    h.clear();
    n
}

/// 后台监听剪贴板变化（独立线程；读取失败静默跳过，不干扰用户）
pub fn start_monitor(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last: Option<String> = read_text();
        // 启动时把当前剪贴板内容纳入历史
        if let Some(t) = &last {
            record(t);
        }
        loop {
            std::thread::sleep(Duration::from_millis(MONITOR_INTERVAL_MS));
            let cur = read_text();
            if cur.is_some() && cur != last {
                last = cur.clone();
                if let Some(t) = &cur {
                    record(t);
                    let _ = app.emit("clipboard-changed", json!({ "text": t }));
                }
            }
        }
    });
}

/// 读取剪贴板文本
pub struct ClipboardGetTool;

impl Tool for ClipboardGetTool {
    fn name(&self) -> &str {
        "clipboard_get"
    }
    fn description(&self) -> &str {
        "读取系统剪贴板中的文本内容（只读；剪贴板为空或非文本时返回错误）"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let text = read_text().ok_or("读取剪贴板失败（可能无文本内容）")?;
        Ok(json!({ "text": text }))
    }
}

/// 写入剪贴板文本
pub struct ClipboardSetTool;

impl Tool for ClipboardSetTool {
    fn name(&self) -> &str {
        "clipboard_set"
    }
    fn description(&self) -> &str {
        "把指定文本写入系统剪贴板（写操作，需授权）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "要写入剪贴板的文本" }
            },
            "required": ["text"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let text = args["text"].as_str().ok_or("缺少参数 text")?;
        set_text(text)?;
        record(text);
        Ok(json!({ "ok": true, "bytes": text.len() }))
    }
}

/// 剪贴板历史管理（查看 / 清空 / 恢复）
pub struct ClipboardHistoryTool;

impl Tool for ClipboardHistoryTool {
    fn name(&self) -> &str {
        "clipboard_history"
    }
    fn description(&self) -> &str {
        "管理剪贴板历史：list 列出最近复制过的文本（最新在前）；clear 清空历史；restore 把第 index 条历史重新写回剪贴板（index 从 0 开始）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "clear", "restore"], "description": "操作类型，默认 list" },
                "index": { "type": "integer", "description": "restore 时使用的历史序号（从 0 开始）" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let action = args["action"].as_str().unwrap_or("list");
        match action {
            "clear" => {
                let n = clear_history();
                Ok(json!({ "ok": true, "cleared": n }))
            }
            "restore" => {
                let index = args["index"].as_u64().ok_or("restore 需要 index 参数")? as usize;
                let entries = list_entries();
                let entry = entries
                    .get(index)
                    .ok_or_else(|| format!("序号 {index} 超出历史范围（共 {} 条）", entries.len()))?;
                set_text(&entry.text)?;
                record(&entry.text);
                Ok(json!({ "ok": true, "text": entry.text, "len": entry.len }))
            }
            _ => {
                let entries = list_entries();
                Ok(json!({ "count": entries.len(), "entries": entries }))
            }
        }
    }
}

// ---------------- Tauri 命令（供前端面板调用） ----------------

/// 读取当前剪贴板文本
#[tauri::command]
pub fn clipboard_get_text() -> Result<String, String> {
    read_text().ok_or_else(|| "剪贴板为空或非文本内容".into())
}

/// 写入剪贴板文本（并记录历史）
#[tauri::command]
pub fn clipboard_set_text(text: String) -> Result<(), String> {
    set_text(&text)?;
    record(&text);
    Ok(())
}

/// 剪贴板历史（最新在前）
#[tauri::command]
pub fn clipboard_history() -> Vec<ClipEntry> {
    list_entries()
}

/// 清空剪贴板历史，返回清除条数
#[tauri::command]
pub fn clipboard_history_clear() -> usize {
    clear_history()
}