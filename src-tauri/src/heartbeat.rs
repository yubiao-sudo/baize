//! 统一心跳中心：收集各子系统的心跳打点，聚合成「活跃度 + 脉冲」广播给前端银河背景。
//!
//! 设计：
//! - 任意子系统调 `heartbeat::beat("来源")` 即接入（注册式，一行代码加一个心跳源）
//! - 后台线程 5Hz 采样：聚合为 activity(0~1 活跃度 EMA) 与 pulse(窗口内有新打点)，
//!   有变化时 emit `baize:vital` {a, p}——payload 极小，前端 Galaxy 消费驱动星光明灭
//! - 静默时（无打点且活跃度归零）不发包，让银河自然落回深呼吸基线

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter};

/// 采样/广播周期（5Hz）
const TICK_MS: u64 = 200;
/// 活跃度统计窗口（秒）：窗口内打点越多越活跃
const WINDOW_SECS: f64 = 15.0;
/// 每秒打点达到该次数视为活跃度满格
const BEATS_PER_SEC_FULL: f64 = 2.0;
/// 打点环形缓冲上限（防极端频闪撑爆内存）
const MAX_BEATS: usize = 256;

struct Center {
    app: Option<AppHandle>,
    beats: VecDeque<Instant>,
    activity: f64,
    last_emit_activity: f64,
    beats_since_emit: usize,
}

static CENTER: OnceLock<Mutex<Center>> = OnceLock::new();

fn center() -> &'static Mutex<Center> {
    CENTER.get_or_init(|| {
        Mutex::new(Center {
            app: None,
            beats: VecDeque::new(),
            activity: 0.0,
            last_emit_activity: 0.0,
            beats_since_emit: 0,
        })
    })
}

/// 启动心跳采样/广播线程（lib.rs setup 时调用一次）
pub fn init(app: AppHandle) {
    {
        let mut c = center().lock().unwrap();
        c.app = Some(app.clone());
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(TICK_MS));
        tick();
    });
}

/// 心跳打点：任意子系统在「活着/干了活」的节点调用（来源名仅用于将来排查，当前不参与聚合）
pub fn beat(_source: &str) {
    if let Ok(mut c) = center().lock() {
        c.beats.push_back(Instant::now());
        c.beats_since_emit += 1;
        if c.beats.len() > MAX_BEATS {
            c.beats.pop_front();
        }
    }
}

/// 采样一轮：活跃度 EMA + 脉冲检测 + 变化时广播
fn tick() {
    let mut c = match center().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let now = Instant::now();
    let window = Duration::from_secs_f64(WINDOW_SECS);
    while let Some(front) = c.beats.front() {
        if now.duration_since(*front) > window {
            c.beats.pop_front();
        } else {
            break;
        }
    }

    // 窗口内打点频率 → 归一化活跃度，EMA 平滑（0.08 ≈ 1s 时间常数）
    let raw = (c.beats.len() as f64 / (WINDOW_SECS * BEATS_PER_SEC_FULL)).min(1.0);
    c.activity += (raw - c.activity) * 0.08;

    let pulse = c.beats_since_emit > 0;
    let changed = pulse || (c.activity - c.last_emit_activity).abs() > 0.02;
    if !changed {
        return;
    }

    // 静默期静音：活跃度几乎为零且无脉冲时不发包
    if !pulse && c.activity < 0.01 && c.last_emit_activity < 0.01 {
        c.last_emit_activity = c.activity;
        return;
    }

    if let Some(app) = &c.app {
        let _ = app.emit(
            "baize:vital",
            json!({ "a": (c.activity * 1000.0).round() / 1000.0, "p": pulse }),
        );
    }
    c.last_emit_activity = c.activity;
    c.beats_since_emit = 0;
}
