//! 统一进度上报通道（执行流进度条）。
//!
//! 目标：让**后台长任务**（办公文档解析/转换、文档生成、批量处理…）把真实阶段进度
//! 推到前端执行流，渲染成进度条，而不是只闪一个「执行中…」光标。
//!
//! 两个进度来源：
//!   ① Rust 侧已知的确定性阶段（如「已收集 12 个文件」）→ [`Progress::set`]
//!   ② Python sidecar 内部真实进度 → 子进程按行向 stderr 输出协议行
//!      `@@BAIZE_PROGRESS {"pct":42,"msg":"解析 报告.pdf (3/8)"}`
//!      由 [`drain_stderr`] 解析后转发（逐文件、逐页这种内部粒度只有脚本自己知道）
//!
//! 事件形态沿用执行流既有的 `tool_progress`（ExecutionFlow 已能渲染进度条/进度环），
//! 另外新增 `icon` 字段供非软件类任务显示类型图标。
//!
//! 设计要点：
//!   - 全局 `AppHandle` 单例（`OnceLock`），**不改变既有工具的签名**——
//!     任何函数（含深层 helper）都能 `Progress::new(..)` 直接上报；
//!   - 实时事件按 120ms 节流，避免刷爆前端 store；
//!   - 只有「首条」与「终态（done/failed）」写入 trace 日志，回看时不至于出现几十条进度条。

use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::AppState;

/// Python → Rust 的进度协议行前缀
pub const PROGRESS_TAG: &str = "@@BAIZE_PROGRESS";

/// 实时事件的节流间隔（毫秒）
const THROTTLE_MS: i64 = 120;

static APP: OnceLock<AppHandle> = OnceLock::new();

/// setup 阶段注册事件句柄；未注册时（单元测试/无 GUI）所有上报静默丢弃
pub fn init(app: AppHandle) {
    let _ = APP.set(app);
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 一次长任务的进度句柄。`Clone` 廉价（内部原子量共享），可跨线程传递。
#[derive(Clone)]
pub struct Progress {
    label: String,
    icon: String,
    last_emit_ms: Arc<AtomicI64>,
    /// 最近一次进度值（十分之一为单位存整数，避免浮点原子）
    last_pct_x10: Arc<AtomicI64>,
    head_logged: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

impl Progress {
    /// 建立进度句柄（不立即发事件，首个 set 才会推送）
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            icon: "⏳".into(),
            last_emit_ms: Arc::new(AtomicI64::new(0)),
            last_pct_x10: Arc::new(AtomicI64::new(0)),
            head_logged: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 指定类型图标（📄 文档 / 📊 表格 / 🎞 演示 / 🔀 转换 / 📦 打包 …）
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    /// 任务是否已进入终态
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// 当前进度值（0-100）
    pub fn pct(&self) -> f64 {
        self.last_pct()
    }

    /// 距上次上报的间隔（毫秒）——供等待循环判断是否需要发心跳
    pub fn idle_ms(&self) -> i64 {
        let last = self.last_emit_ms.load(Ordering::Relaxed);
        if last == 0 {
            return i64::MAX / 2;
        }
        now_ms() - last
    }

    /// 上报一次进行中进度（0-100）。节流：120ms 内的重复调用只发最后一条。
    pub fn set(&self, pct: f64, message: &str) {
        if self.is_closed() {
            return;
        }
        let now = now_ms();
        let last = self.last_emit_ms.load(Ordering::Relaxed);
        let first = !self.head_logged.load(Ordering::Relaxed);
        if !first && now - last < THROTTLE_MS {
            return;
        }
        self.last_emit_ms.store(now, Ordering::Relaxed);
        // 首条写入 trace，保证回放时能看到任务起点
        let log_trace = first;
        self.head_logged.store(true, Ordering::Relaxed);
        self.emit(pct, "running", message, log_trace);
    }

    /// 完成（进度直接置 100，终态必然写入 trace）
    pub fn done(&self, message: &str) {
        if self.is_closed() {
            return;
        }
        self.closed.store(true, Ordering::Relaxed);
        self.emit(100.0, "done", message, true);
    }

    /// 失败（保留当前进度，终态写入 trace）
    pub fn fail(&self, message: &str) {
        if self.is_closed() {
            return;
        }
        self.closed.store(true, Ordering::Relaxed);
        let pct = self.last_pct();
        self.emit(pct, "failed", message, true);
    }

    fn last_pct(&self) -> f64 {
        self.last_pct_x10.load(Ordering::Relaxed) as f64 / 10.0
    }

    fn emit(&self, pct: f64, phase: &str, message: &str, log_trace: bool) {
        let pct = pct.clamp(0.0, 100.0);
        self.last_pct_x10.store((pct * 10.0).round() as i64, Ordering::Relaxed);
        let Some(app) = APP.get() else { return };
        let payload = json!({
            "ts": now_ms(),
            "kind": "tool_progress",
            "label": self.label.clone(),
            "detail": message,
            "progress": (pct * 10.0).round() / 10.0,
            "phase": phase,
            "icon": self.icon.clone(),
        });
        // 实时推送到执行流
        let _ = app.emit("thought", payload.clone());
        // 首条 / 终态固化到 trace，供总结回复与回放查看
        if log_trace {
            if let Some(state) = app.try_state::<AppState>() {
                state.log_thought_full(payload);
            }
        }
    }
}

/// 无内部信号时长任务的心跳百分比：把「已耗时」折算为缓慢逼近上限的进度。
/// 用于 COM 导出 PDF 这类外部引擎不给回调的场景（配合「已耗时 Xs」文案，不做假动画）。
pub fn heartbeat_pct(elapsed_ms: u128, floor: f64, cap: f64) -> f64 {
    let t = elapsed_ms as f64 / 1000.0;
    let p = cap * (1.0 - (-t / 12.0).exp());
    p.clamp(floor, cap)
}

/// 心跳计时器：存活期间每 500ms 上报一次「已用时 Xs」并把进度缓慢推向 cap。
/// 超出作用域（Drop）即停止 —— 用于包住一段拿不到内部回调的阻塞调用（如 COM 导出 PDF）。
pub struct Ticker {
    stop: Arc<AtomicBool>,
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Progress {
    /// 启动心跳（见 [`Ticker`]）
    pub fn spawn_ticker(&self, cap: f64, verb: &str) -> Ticker {
        let stop = Arc::new(AtomicBool::new(false));
        let pr = self.clone();
        let flag = stop.clone();
        let verb = verb.to_string();
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if flag.load(Ordering::Relaxed) || pr.is_closed() {
                    break;
                }
                let el = t0.elapsed().as_millis();
                let pct = heartbeat_pct(el, pr.pct().max(8.0), cap);
                pr.set(pct, &format!("{verb}… 已用时 {}s", el / 1000));
            }
        });
        Ticker { stop }
    }
}

/// 把 stderr 里的一行转成进度上报；非协议行返回 false（调用方继续按普通日志累积）。
pub fn handle_progress_line(line: &str, progress: Option<&Progress>) -> bool {
    let Some(rest) = line.trim().strip_prefix(PROGRESS_TAG) else {
        return false;
    };
    let Some(p) = progress else { return true };
    if let Ok(v) = serde_json::from_str::<Value>(rest.trim()) {
        let pct = v.get("pct").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let msg = v.get("msg").and_then(|x| x.as_str()).unwrap_or("");
        p.set(pct, msg);
    }
    true
}

/// 逐行消费子进程 stderr：识别进度协议行并转发，其余（含 traceback）原样累积。
/// 返回线程句柄，`join()` 得到完整 stderr 文本（失败诊断用）。
pub fn drain_stderr<R: Read + Send + 'static>(
    reader: R,
    progress: Option<Progress>,
) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut acc = String::new();
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            if handle_progress_line(&line, progress.as_ref()) {
                continue;
            }
            acc.push_str(&line);
            acc.push('\n');
        }
        acc
    })
}

/// 消费子进程 stdout（逐行读，避免大输出阻塞子进程）
pub fn drain_stdout<R: Read + Send + 'static>(reader: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut acc = String::new();
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else { break };
            acc.push_str(&line);
            acc.push('\n');
        }
        acc
    })
}
