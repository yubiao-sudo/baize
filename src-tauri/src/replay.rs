//! GUI 操作回放：关键帧留存 + 失败回放。
//!
//! GUI 自动化执行期间，每次「写操作」（点击/输入/拖拽/按键/窗口控制）成功后自动截屏留档，
//! 形成按执行顺序排列的关键帧序列。任务失败或结果异常时，模型可调用
//! [`ReplayKeyframesTool`]（`replay_keyframes` 工具）回看这些截图，定位是哪一步出了错。
//!
//! 关键帧日志为进程内全局滚动缓冲（上限 [`MAX_KEYFRAMES`] 帧），在新一轮任务开始时清空，
//! 避免混入上一轮的截屏。截屏为尽力而为：失败时静默忽略，不影响正常自动化流程。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use crate::capability::Capability;
use crate::tools::{PermissionClass, Tool};

/// 滚动缓冲上限：超过后丢弃最旧帧，防止长任务导致内存无限增长
const MAX_KEYFRAMES: usize = 300;

/// 一帧关键帧：seq = 顺序号，label = 触发该帧的操作（工具名），path = 截图本地路径
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct KeyframeEntry {
    pub seq: usize,
    pub ts: u128,
    pub label: String,
    pub path: String,
}

/// 关键帧日志（单次 GUI 任务的截屏序列）
struct KeyframeLog {
    frames: Mutex<Vec<KeyframeEntry>>,
}

impl KeyframeLog {
    fn new() -> Self {
        Self {
            frames: Mutex::new(Vec::new()),
        }
    }

    /// 追加一帧，返回写入的条目
    fn push(&self, ts: u128, label: String, path: String) -> KeyframeEntry {
        let mut f = self.frames.lock().unwrap();
        let entry = KeyframeEntry {
            seq: f.len() + 1,
            ts,
            label,
            path,
        };
        f.push(entry.clone());
        while f.len() > MAX_KEYFRAMES {
            f.remove(0);
        }
        entry
    }

    /// 按顺序返回全部关键帧
    fn list(&self) -> Vec<KeyframeEntry> {
        self.frames.lock().unwrap().clone()
    }

    /// 清空（新一轮 GUI 任务开始时调用）
    fn clear(&self) {
        self.frames.lock().unwrap().clear();
    }
}

/// 全局单例关键帧日志
fn keyframe_log() -> &'static KeyframeLog {
    static LOG: OnceLock<KeyframeLog> = OnceLock::new();
    LOG.get_or_init(KeyframeLog::new)
}

/// 清空关键帧日志（新一轮 GUI 任务开始时调用，避免混入上一轮的截屏）
pub fn clear_keyframes() {
    keyframe_log().clear();
}

// ─────────────── GUI 操作日志（gui_undo 的回退依据） ───────────────
//
// 每个成功的 GUI 写操作按顺序记录（工具名 + 参数摘要），供 gui_undo 工具：
//   - action=list  列出最近操作（透明留痕）
//   - action=last  回退最近一次「文本输入」类操作（全选删除清空）
// 点击/导航/按键类操作不可自动回退，gui_undo 会如实标注而非假装回滚。

/// 操作日志（进程内全局，上限 50 条，新一轮任务开始时随关键帧一起清空）
static OP_JOURNAL: OnceLock<Mutex<VecDeque<(String, String)>>> = OnceLock::new();

/// 记录一条成功的 GUI 写操作（detail 为参数摘要，自动截断）
pub fn record_gui_op(tool: &str, detail: &str) {
    let journal = OP_JOURNAL.get_or_init(|| Mutex::new(VecDeque::new()));
    if let Ok(mut q) = journal.lock() {
        if q.len() >= 50 {
            q.pop_front();
        }
        let brief: String = detail.chars().take(500).collect();
        q.push_back((tool.to_string(), brief));
    }
}

/// 最近 N 条 GUI 写操作（最新在前）：(工具名, 参数摘要)
pub fn last_gui_ops(n: usize) -> Vec<(String, String)> {
    let journal = OP_JOURNAL.get_or_init(|| Mutex::new(VecDeque::new()));
    match journal.lock() {
        Ok(q) => q.iter().rev().take(n).cloned().collect(),
        Err(_) => Vec::new(),
    }
}

/// 清空操作日志（新一轮任务开始时调用，与关键帧同生命周期）
pub fn clear_gui_ops() {
    if let Some(journal) = OP_JOURNAL.get() {
        if let Ok(mut q) = journal.lock() {
            q.clear();
        }
    }
}

/// 是否为会改变 GUI 状态的「写操作」工具（需要留关键帧）
pub fn is_gui_action_tool(name: &str) -> bool {
    matches!(
        name,
        "click_at"
            | "type_text"
            | "click_element"
            | "mouse_click"
            | "mouse_drag"
            | "key_press"
            | "key_down"
            | "key_up"
            | "paste_text"
            | "window_minimize_all"
            | "window_set_topmost"
            | "window_focus"
            | "window_prepare"
    )
}

/// 执行一次写操作后留档：截屏并记录一帧。失败静默忽略（回放是尽力而为）。
pub fn record_keyframe(capability: &dyn Capability, label: &str) {
    if let Ok(info) = capability.capture_screen() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let _ = keyframe_log().push(ts, label.to_string(), info.path);
    }
}

/// 回看关键帧工具（只读）：失败/异常时模型据此定位出错步骤
pub struct ReplayKeyframesTool;

impl ReplayKeyframesTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReplayKeyframesTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for ReplayKeyframesTool {
    fn name(&self) -> &str {
        "replay_keyframes"
    }
    fn description(&self) -> &str {
        "回看本次 GUI 自动化任务的关键帧（每个写操作后的屏幕截图），按执行顺序返回 seq / 步骤 / 截图路径 / 时间戳。\
         当 GUI 任务失败、报错或结果不符合预期时，调用本工具回看最近的关键帧，判断是哪一步操作出错，再做纠正。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "description": "只看最近 N 帧，默认返回全部" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let frames = keyframe_log().list();
        let start = match args["limit"].as_u64() {
            Some(l) => frames.len().saturating_sub(l as usize),
            None => 0,
        };
        let arr: Vec<Value> = frames[start..]
            .iter()
            .map(|f| {
                json!({
                    "seq": f.seq,
                    "label": f.label,
                    "path": f.path,
                    "ts": f.ts,
                })
            })
            .collect();
        Ok(json!({ "count": arr.len(), "frames": arr }))
    }
}