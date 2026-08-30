//! 主动意识：对话结束后的空闲整理（白龙马「TICK 常驻循环」的轻量简化版）。
//!
//! 不做每 N 秒的常驻 tick，改为「事件驱动 + 空闲触发」：
//! - 每次对话结束后跑一次整理（记忆衰减 + 检查未完成任务）；
//! - 发现未完成的任务时，主动推送「续跑提醒」卡片，让用户决定是否继续。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::TimeZone;
use chrono::Timelike;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::memory::MemoryStore;
use crate::task::Todo;

/// 对话结束后的主动整理（异步执行，不阻塞对话返回）。
pub fn on_chat_idle(app: AppHandle, todos: Arc<Mutex<Vec<Todo>>>, store: Arc<MemoryStore>) {
    tauri::async_runtime::spawn(async move {
        // 1) 记忆衰减整理（超过 7 天未访问的记忆降权/清理）
        match store.decay_memories() {
            Ok(0) => {}
            Ok(n) => println!("[主动意识] 记忆衰减: {n} 条"),
            Err(e) => eprintln!("[主动意识] 记忆衰减失败: {e}"),
        }

        // 2) 情景→语义巩固：把重要性较高的事件归纳为可复用的语义记忆
        match store.consolidate_events_to_semantic() {
            Ok(0) => {}
            Ok(n) => println!("[主动意识] 情景→语义巩固: {n} 条"),
            Err(e) => eprintln!("[主动意识] 情景→语义巩固失败: {e}"),
        }

        // 3) 检查未完成任务 → 续跑提醒
        let unfinished: Vec<String> = {
            let t = todos.lock().unwrap();
            t.iter()
                .filter(|x| x.status != "completed")
                .map(|x| x.title.clone())
                .collect()
        };

        if !unfinished.is_empty() {
            let id = uuid::Uuid::new_v4().to_string();
            println!("[主动意识] 发现 {} 个未完成任务，推送续跑提醒", unfinished.len());
            let _ = app.emit(
                "proactive",
                json!({
                    "id": id,
                    "title": "有任务未完成",
                    "body": format!(
                        "上次有 {} 个步骤还没做完：{}。需要我接着完成吗？",
                        unfinished.len(),
                        unfinished.join("、")
                    ),
                    "files": [],
                    "action": "继续完成上次未完成的任务（根据当前 todo 列表里的 pending/in_progress 步骤依次执行）",
                }),
            );
        }
    });
}

/// 后台心跳循环：常驻线程，定期（默认 60s）检测「用户长期未互动」，
/// 在合理时段主动问候一次（每日限流）。与 `on_chat_idle` 互补——
/// 后者是对话结束后的事件驱动整理，这里是跨会话的定时主动行为。
pub fn run_heartbeat(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(60));
        heartbeat_tick(app.clone());
    });
}

fn heartbeat_tick(app: AppHandle) {
    let state = app.state::<crate::AppState>();
    let store = state.store.clone();
    let now = now_ms();

    // 仅在「合理时段」（早 8 点 ~ 晚 22 点）主动打扰，夜间静默
    let hour = chrono::Local
        .timestamp_millis_opt(now)
        .single()
        .map(|d| d.hour())
        .unwrap_or(12);
    if !(8..=22).contains(&hour) {
        return;
    }

    // 用户多久没互动了？以最近一条用户消息为准
    let Some(last) = store.last_user_message_time().unwrap_or(None) else {
        return; // 还没有任何交互，无需问候
    };
    let idle_days = (now - last).max(0) / (24 * 3600 * 1000);
    const IDLE_DAYS_THRESHOLD: i64 = 3;
    if idle_days < IDLE_DAYS_THRESHOLD {
        return;
    }

    // 每日限流：同一天只主动问候一次
    const IDLE_GREET_KEY: &str = "proactive_greet_last";
    let last_greet = store
        .get_setting(IDLE_GREET_KEY)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let today = now / (24 * 3600 * 1000);
    if last_greet / (24 * 3600 * 1000) == today {
        return;
    }

    // 用最近的用户画像生成一段「我记得……」式的主动问候
    let memory_hint = store
        .recall_profile(3)
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.content)
        .collect::<Vec<_>>()
        .join("；");
    let body = if memory_hint.is_empty() {
        format!("你已经 {idle_days} 天没上线了，有点想你。有什么我可以帮忙的吗？")
    } else {
        format!(
            "好久不见，距上次互动已 {idle_days} 天。我还记得：{memory_hint}。有什么想让我接着做的吗？"
        )
    };

    let _ = app.emit(
        "proactive",
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "title": "白泽的想念",
            "body": body,
            "files": [],
            "action": "继续未完成的任务，或告诉我你想做什么",
        }),
    );
    let _ = store.set_setting(IDLE_GREET_KEY, &now.to_string());
    println!("[主动意识] 长期未互动（{} 天），已主动问候", idle_days);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
