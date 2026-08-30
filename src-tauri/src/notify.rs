//! 通知升级系统（Notification Escalation）
//!
//! 当 Agent 需要用户确认（HITL 审批 / 计划审批）且用户长时间未响应时，
//! 按配置逐级升级通知手段，直到用户响应或达到最高级别。
//!
//! 升级链条（可配置）：
//!   L0: 应用内弹窗（默认存在，30s 后升级）
//!   L1: 系统原生通知（Windows Toast / macOS 通知中心）
//!   L2: 语音播报（TTS 朗读审批内容）+ 系统音量最大化
//!   L3: 邮件通知
//!   L4: 自定义 Webhook（可对接短信、钉钉、飞书等）
//!
//! 同时提供 notify_user 工具，让 Agent 在任何阶段主动通知用户。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::model::{ChatMessage, ModelRouter};
use crate::tools::{PermissionClass, Tool};

// ───────────────── 配置 ─────────────────

/// 通知升级级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EscalationLevel {
    /// 应用内弹窗（默认存在）
    Toast = 0,
    /// 系统原生通知
    SystemNotify = 1,
    /// 语音播报
    Voice = 2,
    /// 邮件通知
    Email = 3,
    /// 自定义 Webhook
    Webhook = 4,
}

impl EscalationLevel {
    pub fn label(&self) -> &str {
        match self {
            EscalationLevel::Toast => "应用弹窗",
            EscalationLevel::SystemNotify => "系统通知",
            EscalationLevel::Voice => "语音播报",
            EscalationLevel::Email => "邮件通知",
            EscalationLevel::Webhook => "自定义通知",
        }
    }
}

/// 通知升级配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// 升级是否启用
    pub enabled: bool,
    /// 各级别超时（秒），从审批请求发出开始计时
    /// 默认: 30s → 120s → 300s → 600s → 900s
    pub timeouts_sec: [u64; 5],
    /// 各级别是否启用
    pub levels_enabled: [bool; 5],
    /// 邮件配置
    pub email: Option<EmailConfig>,
    /// Webhook 配置
    pub webhook: Option<WebhookConfig>,
    /// 自定义语音播报文本（为空则使用 Agent 动态生成的消息）
    pub voice_text: Option<String>,
    /// 自定义音频文件路径（播放歌曲/音效，支持 mp3/wav）
    pub audio_file: Option<String>,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timeouts_sec: [30, 120, 300, 600, 900],
            levels_enabled: [true, true, true, false, false],
            email: None,
            webhook: None,
            voice_text: None,
            audio_file: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub username: String,
    pub password: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    pub url: String,
    /// 额外的 HTTP 请求头（JSON 对象字符串）
    pub headers: Option<String>,
}

// ───────────────── 升级管理器 ─────────────────

/// 一个活跃的升级任务
#[allow(dead_code)]
struct Escalation {
    /// 关联的审批 ID
    approval_id: String,
    /// 审批类型描述（用于通知内容）
    what: String,
    /// 审批详情
    detail: String,
    /// 当前升级级别
    level: EscalationLevel,
    /// 下次升级时间
    next_deadline: std::time::Instant,
    /// 是否已取消
    cancelled: bool,
}

/// 通知升级管理器
pub struct EscalationManager {
    config: Mutex<NotifyConfig>,
    /// 活跃的升级任务（approval_id → Escalation）
    active: Mutex<HashMap<String, Escalation>>,
}

impl EscalationManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(NotifyConfig::default()),
            active: Mutex::new(HashMap::new()),
        })
    }

    pub fn get_config(&self) -> NotifyConfig {
        self.config.lock().unwrap().clone()
    }

    pub fn set_config(&self, config: NotifyConfig) {
        *self.config.lock().unwrap() = config;
    }

    /// 开始一个升级任务：审批请求发出时调用
    /// 返回到达第一级的时间，供 runtime 调整 timeout
    pub fn start_escalation(
        self: &Arc<Self>,
        app: &AppHandle,
        approval_id: &str,
        what: &str,
        detail: &str,
    ) -> Duration {
        let config = self.config.lock().unwrap().clone();
        if !config.enabled {
            return Duration::MAX; // 禁用升级，不干预原有超时
        }

        let first_timeout = Duration::from_secs(config.timeouts_sec[0]);

        let mut active = self.active.lock().unwrap();
        // 如果已有同名审批的升级，先取消
        if let Some(old) = active.get_mut(approval_id) {
            old.cancelled = true;
        }

        active.insert(
            approval_id.to_string(),
            Escalation {
                approval_id: approval_id.to_string(),
                what: what.to_string(),
                detail: detail.to_string(),
                level: EscalationLevel::Toast,
                next_deadline: std::time::Instant::now() + first_timeout,
                cancelled: false,
            },
        );

        // 启动后台升级循环
        let manager = self.clone();
        let app_clone = app.clone();
        let id = approval_id.to_string();
        tokio::spawn(async move {
            manager.escalation_loop(&app_clone, &id).await;
        });

        first_timeout
    }

    /// 取消一个升级任务：用户响应审批时调用
    pub fn cancel_escalation(&self, approval_id: &str) {
        let mut active = self.active.lock().unwrap();
        if let Some(esc) = active.get_mut(approval_id) {
            esc.cancelled = true;
        }
        active.remove(approval_id);
    }

    /// 取消升级并通知前端（需要 AppHandle）
    pub fn cancel_escalation_with_event(&self, app: &AppHandle, approval_id: &str) {
        self.cancel_escalation(approval_id);
        let _ = app.emit(
            "escalation-cancelled",
            json!({
                "approval_id": approval_id,
            }),
        );
    }

    /// 升级循环：按时间逐级升级
    async fn escalation_loop(self: Arc<Self>, app: &AppHandle, approval_id: &str) {
        loop {
            // 检查是否被取消，并获取等待时间
            let wait_duration = {
                let active = self.active.lock().unwrap();
                let esc = match active.get(approval_id) {
                    Some(e) if !e.cancelled => e,
                    _ => return, // 已取消或已移除
                };
                let now = std::time::Instant::now();
                if now < esc.next_deadline {
                    Some(esc.next_deadline - now)
                } else {
                    None
                }
            }; // MutexGuard 在此 drop

            if let Some(wait) = wait_duration {
                tokio::time::sleep(wait).await;
            }

            // 执行升级
            let (current_level, detail, what) = {
                let mut active = self.active.lock().unwrap();
                let esc = match active.get_mut(approval_id) {
                    Some(e) if !e.cancelled => e,
                    _ => return,
                };

                let now = std::time::Instant::now();
                if now < esc.next_deadline {
                    continue;
                }

                // 执行当前级别的通知
                let level = esc.level;
                let what = esc.what.clone();
                let detail = esc.detail.clone();

                // 升级到下一级
                let config = self.config.lock().unwrap().clone();
                let next_level_idx = level as usize + 1;
                if next_level_idx >= 5 {
                    // 已到最高级，不再升级
                    // 语音级别每 15s 重复播报；其他级别每 5 分钟
                    let repeat_interval = if level == EscalationLevel::Voice {
                        Duration::from_secs(15)
                    } else {
                        Duration::from_secs(300)
                    };
                    esc.next_deadline = now + repeat_interval;
                    drop(active);
                    let _ = app.emit(
                        "escalation-update",
                        json!({
                            "approval_id": approval_id,
                            "level": level as u8,
                            "level_label": level.label(),
                            "max_level": true,
                        }),
                    );
                    self.execute_level(app, level, &what, &detail);
                    continue;
                }

                if !config.levels_enabled[next_level_idx] {
                    // 跳过被禁用的级别
                    esc.level = match next_level_idx {
                        1 => EscalationLevel::SystemNotify,
                        2 => EscalationLevel::Voice,
                        3 => EscalationLevel::Email,
                        4 => EscalationLevel::Webhook,
                        _ => EscalationLevel::Webhook,
                    };
                    esc.next_deadline = now + Duration::from_secs(config.timeouts_sec[next_level_idx]);
                    continue;
                }

                esc.level = match next_level_idx {
                    1 => EscalationLevel::SystemNotify,
                    2 => EscalationLevel::Voice,
                    3 => EscalationLevel::Email,
                    4 => EscalationLevel::Webhook,
                    _ => EscalationLevel::Webhook,
                };
                esc.next_deadline = now + Duration::from_secs(config.timeouts_sec[next_level_idx]);

                (esc.level, esc.detail.clone(), esc.what.clone())
            }; // MutexGuard 在此 drop

            self.execute_level(app, current_level, &what, &detail);

            // 发送升级事件给前端
            let _ = app.emit(
                "escalation-update",
                json!({
                    "approval_id": approval_id,
                    "level": current_level as u8,
                    "level_label": current_level.label(),
                    "max_level": current_level == EscalationLevel::Webhook,
                }),
            );
        }
    }

    /// 执行某级别的通知动作
    /// `what`: 简短标题（如"需要你确认一个操作"）
    /// `detail`: Agent 生成的带上下文的人性化消息（如"我正帮你生成季度报告，已完成数据分析…"）
    fn execute_level(&self, app: &AppHandle, level: EscalationLevel, what: &str, detail: &str) {
        let config = self.config.lock().unwrap().clone();
        let title = format!("白泽 · {}", level.label());
        let body = if detail.len() > 5 && !detail.starts_with("工具:") {
            detail.to_string()
        } else {
            format!("「{what}」需要你的确认")
        };

        match level {
            EscalationLevel::Toast => {
                let _ = app.emit(
                    "escalation-level",
                    json!({
                        "level": level as u8,
                        "level_label": level.label(),
                        "title": title,
                        "body": body,
                        "detail": detail,
                    }),
                );
            }
            EscalationLevel::SystemNotify => {
                // 系统提示音效
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("powershell")
                        .args(["-c", "[System.Media.SystemSounds]::Asterisk.Play()"])
                        .spawn();
                }
                let _ = app.emit(
                    "escalation-level",
                    json!({
                        "level": level as u8,
                        "level_label": level.label(),
                        "title": title,
                        "body": body,
                        "detail": detail,
                        "action": "system_notify",
                    }),
                );
                println!("[通知升级] L1 系统通知（含音效）: {what}");
            }
            EscalationLevel::Voice => {
                // 语音播报：优先用自定义语音文本，其次用 Agent 动态消息
                let tts_text = config.voice_text.clone().unwrap_or_else(|| {
                    if detail.len() > 5 && !detail.starts_with("工具:") {
                        format!("白泽提醒：{}", detail)
                    } else {
                        format!("白泽提醒：{}，{}，请尽快确认。", what, detail)
                    }
                });
                let _ = app.emit(
                    "escalation-level",
                    json!({
                        "level": level as u8,
                        "level_label": level.label(),
                        "title": title,
                        "body": body,
                        "detail": detail,
                        "action": "voice",
                        "tts_text": tts_text,
                        "audio_file": config.audio_file,
                        "repeat": true,
                    }),
                );
                #[cfg(windows)]
                {
                    let _ = std::process::Command::new("powershell")
                        .args(["-c", "[System.Media.SystemSounds]::Hand.Play()"])
                        .spawn();
                }
                println!("[通知升级] L2 语音播报（循环）: {what}");
            }
            EscalationLevel::Email => {
                let config = self.config.lock().unwrap().clone();
                if let Some(ref email_cfg) = config.email {
                    let subject = format!("[白泽] {what}");
                    let email_body = if detail.len() > 5 && !detail.starts_with("工具:") {
                        format!(
                            "{detail}\n\n—— 白泽通知升级系统（{}）", level.label()
                        )
                    } else {
                        format!(
                            "白泽 Agent 正在等待你的审批确认。\n\n\
                             任务: {what}\n\
                             详情: {detail}\n\n\
                             请尽快回到白泽桌面应用进行确认。\n\n\
                             —— 白泽通知升级系统"
                        )
                    };
                    let cfg = email_cfg.clone();
                    tokio::spawn(async move {
                        if let Err(e) = send_email(&cfg, &subject, &email_body).await {
                            eprintln!("[通知升级] L3 邮件发送失败: {e}");
                        }
                    });
                }
                println!("[通知升级] L3 邮件通知: {what}");
            }
            EscalationLevel::Webhook => {
                let config = self.config.lock().unwrap().clone();
                if let Some(ref webhook_cfg) = config.webhook {
                    let payload = json!({
                        "title": title,
                        "body": body,
                        "what": what,
                        "detail": detail,
                        "timestamp": std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis())
                            .unwrap_or(0),
                    });
                    let cfg = webhook_cfg.clone();
                    tokio::spawn(async move {
                        if let Err(e) = send_webhook(&cfg, &payload).await {
                            eprintln!("[通知升级] L4 Webhook 发送失败: {e}");
                        }
                    });
                }
                println!("[通知升级] L4 Webhook: {what}");
            }
        }
    }
}

// ───────────────── 邮件发送 ─────────────────

async fn send_email(cfg: &EmailConfig, subject: &str, body: &str) -> Result<(), String> {
    let client = reqwest::Client::new();

    // 使用简单 HTTP 方式的邮件发送（通过第三方 API 或 SMTP relay）
    // 这里提供两种方式：直接 SMTP 或通过 HTTP API
    // 如果 smtp_host 是 URL 格式的 HTTP API，直接 POST
    if cfg.smtp_host.starts_with("http") {
        let resp = client
            .post(&cfg.smtp_host)
            .header("Content-Type", "application/json")
            .json(&json!({
                "from": cfg.from,
                "to": cfg.to,
                "subject": subject,
                "body": body,
            }))
            .send()
            .await
            .map_err(|e| format!("邮件请求失败: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("邮件 API 返回 HTTP {}", resp.status()));
        }
    } else {
        // SMTP 方式：使用 lettre 或直接构造 SMTP 请求
        // 这里使用简单的 SMTP 直连（需要 TLS）
        use std::io::{Read, Write};
        use std::net::TcpStream;

        let addr = format!("{}:{}", cfg.smtp_host, cfg.smtp_port);
        let mut stream =
            TcpStream::connect(&addr).map_err(|e| format!("SMTP 连接失败: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .ok();

        let mut buf = [0u8; 1024];

        // 读欢迎消息
        let _ = stream.read(&mut buf);

        // EHLO
        write!(stream, "EHLO baize\r\n").ok();
        let _ = stream.read(&mut buf);

        // AUTH LOGIN (简化：使用 base64 编码的认证)
        // 对于大多数 SMTP 服务器，需要 TLS 连接
        // 这里提供基础的 SMTP 认证流程
        write!(stream, "AUTH LOGIN\r\n").ok();
        let _ = stream.read(&mut buf);

        let username_b64 = base64_encode(&cfg.username);
        let password_b64 = base64_encode(&cfg.password);

        write!(stream, "{username_b64}\r\n").ok();
        let _ = stream.read(&mut buf);
        write!(stream, "{password_b64}\r\n").ok();
        let _ = stream.read(&mut buf);

        // MAIL FROM
        write!(stream, "MAIL FROM:<{}>\r\n", cfg.from).ok();
        let _ = stream.read(&mut buf);

        // RCPT TO
        write!(stream, "RCPT TO:<{}>\r\n", cfg.to).ok();
        let _ = stream.read(&mut buf);

        // DATA
        write!(stream, "DATA\r\n").ok();
        let _ = stream.read(&mut buf);

        // 邮件内容
        let message = format!(
            "From: {}\r\nTo: {}\r\nSubject: =?UTF-8?B?{}?=\r\n\
             Content-Type: text/plain; charset=UTF-8\r\n\r\n{}\r\n.\r\n",
            cfg.from,
            cfg.to,
            base64_encode(subject),
            body,
        );
        write!(stream, "{message}").ok();
        let _ = stream.read(&mut buf);

        // QUIT
        write!(stream, "QUIT\r\n").ok();
    }
    Ok(())
}

fn base64_encode(s: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = s.as_bytes();
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

// ───────────────── Webhook 发送 ─────────────────

async fn send_webhook(cfg: &WebhookConfig, payload: &Value) -> Result<(), String> {
    let client = reqwest::Client::new();
    let mut req = client
        .post(&cfg.url)
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(10));

    if let Some(ref headers_str) = cfg.headers {
        if let Ok(headers) = serde_json::from_str::<HashMap<String, String>>(headers_str) {
            for (k, v) in headers {
                req = req.header(&k, &v);
            }
        }
    }

    let resp = req.json(payload).send().await.map_err(|e| format!("Webhook 请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Webhook 返回 HTTP {}", resp.status()));
    }
    Ok(())
}

// ───────────────── 动态通知消息生成 ─────────────────

/// 用模型生成一条带上下文的人性化审批通知消息。
/// 失败时回退到原始 tool_name + args 的固定模板。
pub async fn generate_approval_message(
    model: &ModelRouter,
    user_message: &str,
    tool_name: &str,
    args: &Value,
    recent_context: &str,
) -> (String, String) {
    let prompt = format!(
        "你正在帮用户执行任务，现在需要用户确认一个操作才能继续。\n\n\
         用户原始请求：{user_message}\n\
         当前已完成：{recent_context}\n\
         需要确认的操作：{tool_name}\n\
         操作参数：{args}\n\n\
         请生成一段通知文字（40字以内），用第一人称告诉用户你为什么需要这个确认，以及当前进度如何。\
         语气自然、带点人情味，不要机械重复工具名和参数。只输出通知文字，不要任何解释。"
    );

    let msgs = vec![ChatMessage {
        role: "user".into(),
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
    }];

    // 动态生成通知文字最多等 2 秒，超时立即回退固定模板，避免拖慢审批链
    match tokio::time::timeout(Duration::from_secs(2), model.chat(&msgs, &[])).await {
        Ok(Ok(resp)) => {
            if let Some(text) = resp.content {
                let msg = text.trim().to_string();
                if !msg.is_empty() {
                    let short_title = if msg.chars().count() > 15 {
                        msg.chars().take(15).collect::<String>() + "…"
                    } else {
                        msg.clone()
                    };
                    return (short_title, msg);
                }
            }
        }
        Ok(Err(e)) => {
            eprintln!("[通知升级] 生成动态消息失败，回退: {e}");
        }
        Err(_) => {
            eprintln!("[通知升级] 生成动态消息超时，回退固定模板");
        }
    }

    // 回退：固定模板
    (
        format!("{tool_name} 需要确认"),
        format!("工具: {tool_name}, 参数: {args}"),
    )
}

// ───────────────── notify_user 工具 ─────────────────

/// notify_user 工具：让 Agent 在任何阶段主动通知用户
pub struct NotifyUserTool {
    app: AppHandle,
    escalation: Arc<EscalationManager>,
}

impl NotifyUserTool {
    pub fn new(app: AppHandle, escalation: Arc<EscalationManager>) -> Self {
        Self { app, escalation }
    }
}

impl Tool for NotifyUserTool {
    fn name(&self) -> &str {
        "notify_user"
    }
    fn description(&self) -> &str {
        "主动通知用户，用于需要用户关注但当前没有审批弹窗的场景（如任务完成、发现异常、需要用户决策等）。\
         支持多种通知级别：toast（应用内弹窗）、system（系统通知）、voice（语音播报）、email（邮件）。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "通知标题" },
                "body": { "type": "string", "description": "通知内容" },
                "level": {
                    "type": "string",
                    "enum": ["toast", "system", "voice", "email"],
                    "description": "通知级别：toast=应用弹窗, system=系统通知, voice=语音播报, email=邮件（需配置SMTP）"
                },
                "require_response": {
                    "type": "boolean",
                    "description": "是否需要用户响应（true 则创建审批请求等待用户确认）"
                }
            },
            "required": ["title", "body"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let title = args["title"].as_str().unwrap_or("白泽通知").to_string();
        let body = args["body"].as_str().unwrap_or("").to_string();
        let level_str = args["level"].as_str().unwrap_or("toast").to_string();
        let require_response = args["require_response"].as_bool().unwrap_or(false);

        let level = match level_str.as_str() {
            "system" => EscalationLevel::SystemNotify,
            "voice" => EscalationLevel::Voice,
            "email" => EscalationLevel::Email,
            _ => EscalationLevel::Toast,
        };

        self.escalation
            .execute_level(&self.app, level, &title, &body);

        let id = if require_response {
            // 创建一个简化的审批请求
            let req_id = uuid::Uuid::new_v4().to_string();
            let _ = self.app.emit(
                "notify-user-request",
                json!({
                    "id": req_id,
                    "title": title,
                    "body": body,
                    "level": level_str,
                }),
            );
            req_id
        } else {
            String::new()
        };

        Ok(json!({
            "ok": true,
            "level": level_str,
            "request_id": id,
        }))
    }
}

// ───────────────── speak 工具（桌面助手开口说话） ─────────────────

/// speak 工具：语音播报一段文字，复用通知升级系统的语音通道（前端 Web Speech 朗读）
pub struct SpeakTool {
    app: AppHandle,
}

impl SpeakTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for SpeakTool {
    fn name(&self) -> &str {
        "speak"
    }
    fn description(&self) -> &str {
        "语音播报一段文字（桌面助手开口说话）。默认经前端 Web Speech 朗读；offline=true 时改用后端本地离线语音（Windows System.Speech，不依赖前端/网络），并写一条思考日志。repeat=true 时每 10 秒循环播报直到用户响应"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "要朗读的文字" },
                "repeat": { "type": "boolean", "description": "是否循环播报，默认 false" },
                "offline": { "type": "boolean", "description": "是否用后端本地离线语音朗读（Windows System.Speech），默认 false" }
            },
            "required": ["text"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let text = args["text"].as_str().ok_or("缺少参数 text")?.to_string();
        let repeat = args["repeat"].as_bool().unwrap_or(false);
        let offline = args["offline"].as_bool().unwrap_or(false);
        if offline {
            // 后端本地离线朗读（Windows System.Speech），不依赖前端 Web Speech / 网络
            speak_local(&text);
            let _ = self.app.emit(
                "thought",
                json!({
                    "kind": "voice",
                    "label": "本地语音播报",
                    "detail": text,
                }),
            );
        } else {
            // 默认：交给前端 Web Speech 朗读
            let _ = self.app.emit(
                "escalation-level",
                json!({
                    "level": 2u8,
                    "level_label": "语音播报",
                    "title": "白泽语音",
                    "body": text,
                    "detail": text,
                    "action": "voice",
                    "tts_text": text,
                    "audio_file": null,
                    "repeat": repeat,
                }),
            );
        }
        Ok(json!({ "ok": true, "repeat": repeat, "offline": offline }))
    }
}

/// 后端本地离线语音朗读：Windows 下用 System.Speech（SAPI）朗读，不依赖前端 Web Speech 或网络。
/// 通过环境变量传递文本，规避命令行转义问题；spawn 异步执行，不阻塞 Agent 循环。
#[cfg(windows)]
pub fn speak_local(text: &str) {
    if text.trim().is_empty() {
        return;
    }
    let _ = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Add-Type -AssemblyName System.Speech; $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; $s.Speak($env:BAIZE_TTS)",
        ])
        .env("BAIZE_TTS", text)
        .spawn();
}

#[cfg(not(windows))]
pub fn speak_local(_text: &str) {}