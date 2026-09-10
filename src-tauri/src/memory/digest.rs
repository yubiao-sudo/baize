//! 主动记忆整理：每晚固定时间把当天的对话沉淀成结构化笔记
//!
//! 流程：
//! 1. 后台线程每晚 `nightly_digest_time`（默认 23:30 本地时间）触发；
//! 2. 拉取「上次整理时间之后」的全部对话消息（跨会话）；
//! 3. 调模型总结为「主题 / 结论 / 待办 / 用户偏好」结构化笔记（模型失败退化为确定性摘要）；
//! 4. 写入情景记忆（events，可被召回）+ 语义记忆（semantic_memories，可被画像召回）；
//! 5. 记录整理水位（settings 表），保证幂等、不重复整理。
//!
//! 手动触发：`memory_digest_now` 工具（Agent 可调）/ `nightly_digest_run` 命令（前端可调）。

use std::sync::Arc;

use serde_json::{json, Value};

use crate::memory::MemoryStore;
use crate::tools::PermissionClass;

/// 单条消息在喂给模型前的最大字符数
const MAX_MSG_CHARS: usize = 500;
/// 喂给模型的转录总字符上限（约 16k token 内）
const MAX_TRANSCRIPT_CHARS: usize = 12000;
/// 最多携带的消息条数
const MAX_TRANSCRIPT_MSGS: usize = 200;

/// 夜间整理配置（持久化在 settings 表）
pub struct DigestConfig {
    pub enabled: bool,
    /// "HH:MM"（本地时间），默认 23:30
    pub time: String,
}

impl DigestConfig {
    fn load(store: &MemoryStore) -> Self {
        let enabled = store
            .get_setting("nightly_digest_enabled")
            .ok()
            .flatten()
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .unwrap_or(true);
        let time = store
            .get_setting("nightly_digest_time")
            .ok()
            .flatten()
            .filter(|v| valid_hhmm(v))
            .unwrap_or_else(|| "23:30".to_string());
        Self { enabled, time }
    }

    fn save(&self, store: &MemoryStore) -> Result<(), String> {
        if !valid_hhmm(&self.time) {
            return Err(format!("时间格式无效（应为 HH:MM）: {}", self.time));
        }
        store.set_setting(
            "nightly_digest_enabled",
            if self.enabled { "1" } else { "0" },
        )?;
        store.set_setting("nightly_digest_time", self.time.trim())
    }
}

fn valid_hhmm(s: &str) -> bool {
    let t = s.trim();
    let parts: Vec<&str> = t.split(':').collect();
    if parts.len() != 2 {
        return false;
    }
    match (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
        (Ok(h), Ok(m)) => h < 24 && m < 60,
        _ => false,
    }
}

/// 上次整理水位（毫秒时间戳）；无记录时回退到「今天零点」
fn last_watermark(store: &MemoryStore) -> i64 {
    if let Ok(Some(v)) = store.get_setting("nightly_digest_watermark") {
        if let Ok(ts) = v.parse::<i64>() {
            return ts;
        }
    }
    today_start_ms()
}

fn today_start_ms() -> i64 {
    let now = chrono::Local::now();
    let start = now.date_naive().and_hms_opt(0, 0, 0).unwrap_or_default();
    start
        .and_local_timezone(now.timezone())
        .single()
        .map(|d| d.timestamp_millis())
        .unwrap_or_else(|| now.timestamp_millis() - 24 * 3600 * 1000)
}

/// 现在距离下一个 "HH:MM"（本地时间）还有多少毫秒
fn millis_until(hhmm: &str, now: chrono::DateTime<chrono::Local>) -> std::time::Duration {
    let parts: Vec<&str> = hhmm.trim().split(':').collect();
    let (h, m) = match (parts.first().and_then(|s| s.parse::<u32>().ok()), parts.get(1).and_then(|s| s.parse::<u32>().ok())) {
        (Some(h), Some(m)) => (h, m),
        _ => (23, 30),
    };
    let today_target = now
        .date_naive()
        .and_hms_opt(h, m, 0)
        .unwrap_or_default()
        .and_local_timezone(now.timezone())
        .single();
    let target = match today_target {
        Some(t) if t > now => Some(t),
        _ => {
            // 今天的时刻已过 → 明天同时刻
            let tomorrow = now.date_naive().succ_opt().unwrap_or_default();
            tomorrow
                .and_hms_opt(h, m, 0)
                .unwrap_or_default()
                .and_local_timezone(now.timezone())
                .single()
        }
    };
    let secs = target
        .map(|t| (t - now).num_milliseconds().max(0) as u64)
        .unwrap_or(60);
    std::time::Duration::from_millis(secs)
}

/// 拉取水位后的消息并构建转录文本（带会话标题分组）
fn build_transcript(store: &MemoryStore) -> Result<(String, usize), String> {
    let since = last_watermark(store);
    let rows = store.messages_since(since, MAX_TRANSCRIPT_MSGS)?;
    if rows.is_empty() {
        return Ok((String::new(), 0));
    }
    let mut out = String::new();
    let mut count = 0usize;
    let mut last_title = String::new();
    for (title, role, content, _ts) in rows {
        if count >= MAX_TRANSCRIPT_MSGS || out.chars().count() >= MAX_TRANSCRIPT_CHARS {
            break;
        }
        if title != last_title {
            out.push_str(&format!("\n【会话: {}】\n", if title.is_empty() { "未命名" } else { &title }));
            last_title = title;
        }
        let who = if role == "user" { "用户" } else { "白泽" };
        let clipped: String = {
            let mut s: String = content.chars().take(MAX_MSG_CHARS).collect();
            if content.chars().count() > MAX_MSG_CHARS {
                s.push('…');
            }
            s
        };
        out.push_str(&format!("{who}: {clipped}\n"));
        count += 1;
    }
    Ok((out, count))
}

/// 确定性兜底摘要：模型不可用时也能沉淀有用的线索（取用户侧消息为主）
fn fallback_digest(transcript: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in transcript.lines() {
        if let Some(rest) = line.strip_prefix("用户: ") {
            let t = rest.trim();
            if t.chars().count() >= 8 {
                lines.push(clip(t, 120));
            }
            if lines.len() >= 10 {
                break;
            }
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "（模型离线，确定性摘要）当天用户关注的问题：\n{}",
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{}. {}", i + 1, l))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        let mut t: String = s.chars().take(n).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

/// 调模型总结（无工具，纯文本总结）。失败返回 Err。
async fn summarize_with_model(
    router: &crate::model::ModelRouter,
    transcript: &str,
) -> Result<String, String> {
    let prompt = format!(
        "下面是用户与桌面助手「白泽」今天的对话记录。请把它整理成一份结构化日记笔记，\
         用 Markdown 输出，包含且仅包含这几个小节：\n\
         ## 主题\n（今天聊了什么，2-4 条）\n\
         ## 结论与进展\n（完成了什么 / 得出什么结论，2-4 条）\n\
         ## 待办\n（用户提到但没做完的事，没有写「无」）\n\
         ## 用户偏好\n（能长期记住的偏好/习惯/项目背景，没有写「无」）\n\
         只输出笔记本身，不要客套话。\n\n对话记录：\n{transcript}"
    );
    let msgs = vec![crate::model::ChatMessage {
        role: "user".to_string(),
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
    }];
    let resp = router.chat(&msgs, &[]).await?;
    resp.content
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "模型返回为空".to_string())
}

/// 执行一次整理。返回 (笔记, 消息条数)。
pub async fn run_digest(
    store: &Arc<MemoryStore>,
    router: &Arc<crate::model::ModelRouter>,
) -> Result<(String, usize), String> {
    let (transcript, count) = build_transcript(store)?;
    if count == 0 {
        return Ok((String::new(), 0));
    }
    let note = match summarize_with_model(router, &transcript).await {
        Ok(n) => n,
        Err(_) => fallback_digest(&transcript),
    };
    if note.is_empty() {
        return Ok((String::new(), count));
    }

    // 事件 ID 先落，便于语义记忆回溯来源
    let event_id = store
        .record_event("nightly_digest", &note, "", 0.8)
        .ok();
    if let Some(id) = event_id {
        let _ = store.upsert_semantic("daily_note", &note, 0.7, Some(id));
    } else {
        let _ = store.upsert_semantic("daily_note", &note, 0.7, None);
    }

    // 推进水位（只推进到本次拉取覆盖到的时刻，避免正在进行的对话被漏掉/重复）
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let _ = store.set_setting("nightly_digest_watermark", &now.to_string());
    Ok((note, count))
}

// ---------------- 工具与后台线程 ----------------

/// 手动触发一次记忆整理（Agent 可调用；前端亦可经命令触发）
pub struct MemoryDigestNowTool {
    pub store: Arc<MemoryStore>,
    pub model: Arc<crate::model::ModelRouter>,
}

impl MemoryDigestNowTool {
    pub fn new(store: Arc<MemoryStore>, model: Arc<crate::model::ModelRouter>) -> Self {
        Self { store, model }
    }
}

impl crate::tools::Tool for MemoryDigestNowTool {
    fn name(&self) -> &str {
        "memory_digest_now"
    }
    fn description(&self) -> &str {
        "立即执行一次「主动记忆整理」：把上次整理之后的对话沉淀为结构化笔记（主题/结论/待办/偏好），\
         写入情景记忆与语义记忆。每晚定时也会自动执行"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let store = self.store.clone();
        let model = self.model.clone();
        // run() 可能在 tokio 执行上下文里被调用，这里开独立线程 + 临时运行时执行异步整理
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt.block_on(async move { run_digest(&store, &model).await }),
                Err(e) => Err(format!("创建 tokio 运行时失败: {e}")),
            };
            let _ = tx.send(result);
        });
        match rx.recv_timeout(std::time::Duration::from_secs(120)) {
            Ok(Ok((note, count))) => {
                if count == 0 {
                    return Ok(json!({ "ok": true, "messages": 0, "note": "", "message": "上次整理后没有新对话，无需整理" }));
                }
                Ok(json!({ "ok": true, "messages": count, "note": note }))
            }
            Ok(Err(e)) => Err(format!("记忆整理失败: {e}")),
            Err(_) => Err("记忆整理超时（120s）".into()),
        }
    }
}

/// 夜间整理后台循环：每晚到点执行；配置可运行时修改（settings 表）
pub fn spawn_nightly_loop(store: Arc<MemoryStore>, model: Arc<crate::model::ModelRouter>) {
    std::thread::spawn(move || loop {
        let cfg = DigestConfig::load(&store);
        if !cfg.enabled {
            // 未启用：每小时检查一次开关是否被打开
            std::thread::sleep(std::time::Duration::from_secs(3600));
            continue;
        }
        let now = chrono::Local::now();
        let wait = millis_until(&cfg.time, now);
        eprintln!(
            "[夜间记忆整理] 下次执行：{}（{} 分钟后）",
            cfg.time,
            wait.as_secs() / 60
        );
        std::thread::sleep(wait);
        // 睡醒后重新读配置（期间可能被改）
        let cfg = DigestConfig::load(&store);
        if !cfg.enabled {
            continue;
        }
        let store2 = store.clone();
        let model2 = model.clone();
        let handle = tokio::runtime::Handle::try_current();
        let joined: Option<Result<(String, usize), String>> = match handle {
            Ok(h) => Some(h.block_on(async move { run_digest(&store2, &model2).await })),
            Err(_) => {
                // 非 tokio 线程：临时建一个运行时
                match tokio::runtime::Runtime::new() {
                    Ok(rt) => Some(rt.block_on(async move { run_digest(&store2, &model2).await })),
                    Err(e) => Some(Err(format!("创建 tokio 运行时失败: {e}"))),
                }
            }
        };
        match joined {
            Some(Ok((note, count))) if count > 0 => {
                eprintln!("[夜间记忆整理] 完成：{count} 条消息已沉淀为笔记");
                let _ = note;
            }
            Some(Ok(_)) => eprintln!("[夜间记忆整理] 无新对话，跳过"),
            Some(Err(e)) => eprintln!("[夜间记忆整理] 失败: {e}"),
            None => {}
        }
    });
}

// ---------------- 配置命令（供前端/工具层调用，非 Tauri command） ----------------

/// 读取配置
pub fn config_get(store: &MemoryStore) -> Value {
    let cfg = DigestConfig::load(store);
    json!({ "enabled": cfg.enabled, "time": cfg.time })
}

/// 保存配置（enabled 可空 = 不变；time 可空 = 不变）
pub fn config_set(store: &MemoryStore, enabled: Option<bool>, time: Option<&str>) -> Result<Value, String> {
    let mut cfg = DigestConfig::load(store);
    if let Some(e) = enabled {
        cfg.enabled = e;
    }
    if let Some(t) = time {
        if !t.trim().is_empty() {
            cfg.time = t.trim().to_string();
        }
    }
    cfg.save(store)?;
    Ok(config_get(store))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hhmm_validation() {
        assert!(valid_hhmm("23:30"));
        assert!(valid_hhmm("0:05"));
        assert!(!valid_hhmm("24:00"));
        assert!(!valid_hhmm("12:60"));
        assert!(!valid_hhmm("abc"));
    }

    #[test]
    fn millis_until_future_today() {
        let now = chrono::Local::now();
        // 明确未来的时刻：现在 + 1 分钟（取整到分钟可能相等 → 用 +2 分钟）
        let target = (now + chrono::Duration::minutes(2))
            .format("%H:%M")
            .to_string();
        let d = millis_until(&target, now);
        assert!(d.as_secs() <= 2 * 60 + 5);
    }

    #[test]
    fn millis_until_past_goes_tomorrow() {
        let now = chrono::Local::now();
        let target = (now - chrono::Duration::hours(1))
            .format("%H:%M")
            .to_string();
        let d = millis_until(&target, now);
        // 约 23 小时后
        assert!(d.as_secs() > 22 * 3600);
    }
}
