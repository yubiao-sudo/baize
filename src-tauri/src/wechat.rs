//! 微信机器人（iLink 协议）：扫码登录 + 长轮询收发消息 + 媒体解密 + 二次确认
//!
//! 基于腾讯 iLink Bot 协议（与参考项目 wechat-clawbot 一致）：
//!   - `ilink/bot/get_bot_qrcode`     —— 获取登录二维码（GET）
//!   - `ilink/bot/get_qrcode_status`  —— 轮询扫码状态（GET）
//!   - `ilink/bot/getupdates`         —— 长轮询收消息（POST）
//!   - `ilink/bot/sendmessage`        —— 发送消息（POST）
//! 媒体文件经 AES-128-ECB（PKCS7）加密存储于微信 CDN，下载后需解密。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::memory::MemoryStore;
use crate::tools::{PermissionClass, Tool};

const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";
const DEFAULT_BOT_TYPE: &str = "3";
const CHANNEL_VERSION: &str = "standalone-0.1.0";
const LONG_POLL_TIMEOUT_MS: u64 = 35_000;
const API_TIMEOUT_MS: u64 = 15_000;
const QR_POLL_TIMEOUT_MS: u64 = 35_000;
/// 回复文本上限，超出截断（避免超长回复触发微信侧限制）
const MAX_REPLY_CHARS: usize = 8_000;

// 持久化键
const K_BOT_TOKEN: &str = "wechat_bot_token";
const K_ACCOUNT_ID: &str = "wechat_account_id";
const K_BASE_URL: &str = "wechat_base_url";
const K_SYNC_BUF: &str = "wechat_sync_buf";
const K_CONTEXT_TOKENS: &str = "wechat_context_tokens";

/// 微信机器人状态（经 AppState 以 Arc 持有）
pub struct WeChatState {
    client: reqwest::Client,
    token: Mutex<Option<String>>,
    account_id: Mutex<Option<String>>,
    base_url: Mutex<String>,
    /// userId -> context_token（回复时原样带回，保证消息路由到正确会话）
    context_tokens: Mutex<HashMap<String, String>>,
    /// userId -> approval_id（等待微信二次确认的审批）
    pending_approvals: Mutex<HashMap<String, String>>,
    /// 最近一次发来指令的微信用户
    last_user: Mutex<Option<String>>,
    status: Mutex<String>,
    stopped: Arc<AtomicBool>,
    store: Arc<MemoryStore>,
    /// 消息收发日志池（跨通道共享）
    log: Arc<crate::im::ImLog>,
    /// 入站消息去重：ilink 长轮询在重连/超时后可能重投同一消息，
    /// 不去重会导致同一条指令被处理两次（回复两条「白泽收到」）。
    /// key = "id:{client_id|msg_id}"（重投同 id，保留 10 分钟）或 "hash:{内容摘要}"（保留 2 分钟）
    seen_msgs: Mutex<HashMap<String, std::time::Instant>>,
}

impl WeChatState {
    pub fn new(store: Arc<MemoryStore>, log: Arc<crate::im::ImLog>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("构建微信 HTTP 客户端失败");

        // 从持久化恢复凭证与上下文令牌
        let token = store
            .get_setting(K_BOT_TOKEN)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        let account_id = store
            .get_setting(K_ACCOUNT_ID)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        let base_url = store
            .get_setting(K_BASE_URL)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let context_tokens: HashMap<String, String> = store
            .get_setting(K_CONTEXT_TOKENS)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        let status = if token.is_some() && account_id.is_some() {
            "connected".to_string()
        } else {
            "idle".to_string()
        };

        Self {
            client,
            token: Mutex::new(token),
            account_id: Mutex::new(account_id),
            base_url: Mutex::new(base_url),
            context_tokens: Mutex::new(context_tokens),
            pending_approvals: Mutex::new(HashMap::new()),
            last_user: Mutex::new(None),
            status: Mutex::new(status),
            stopped: Arc::new(AtomicBool::new(false)),
            store,
            log,
            seen_msgs: Mutex::new(HashMap::new()),
        }
    }

    /// 当前是否持有登录凭证
    pub fn has_credentials(&self) -> bool {
        self.token.lock().unwrap().is_some() && self.account_id.lock().unwrap().is_some()
    }

    /// 供前端轮询/展示的连接状态快照
    pub fn snapshot(&self) -> Value {
        json!({
            "status": *self.status.lock().unwrap(),
            "connected": self.has_credentials(),
            "account_id": self.account_id.lock().unwrap().clone(),
        })
    }

    fn set_status(&self, s: &str) {
        *self.status.lock().unwrap() = s.to_string();
    }

    fn persist_credentials(&self, token: &str, account_id: &str, base_url: &str) {
        let _ = self.store.set_setting(K_BOT_TOKEN, token);
        let _ = self.store.set_setting(K_ACCOUNT_ID, account_id);
        if !base_url.is_empty() {
            let _ = self.store.set_setting(K_BASE_URL, base_url);
        }
    }

    /// 停止长轮询 / 扫码轮询
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// 登出：清空凭证与内存状态
    pub fn logout(&self) {
        self.stop();
        *self.token.lock().unwrap() = None;
        *self.account_id.lock().unwrap() = None;
        self.context_tokens.lock().unwrap().clear();
        self.pending_approvals.lock().unwrap().clear();
        *self.last_user.lock().unwrap() = None;
        self.set_status("idle");
        let _ = self.store.set_setting(K_BOT_TOKEN, "");
        let _ = self.store.set_setting(K_ACCOUNT_ID, "");
        let _ = self.store.set_setting(K_SYNC_BUF, "");
        let _ = self.store.set_setting(K_CONTEXT_TOKENS, "{}");
    }

    /// 启动长轮询（凭证已存在的自动连接，或登录成功后调用）
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        if !self.has_credentials() {
            self.set_status("idle");
            let _ = app.emit("wechat-status", self.snapshot());
            return;
        }
        self.stopped.store(false, Ordering::SeqCst);
        self.set_status("connected");
        let _ = app.emit("wechat-status", self.snapshot());
        let st = self.clone();
        tauri::async_runtime::spawn(async move {
            st.updates_loop(app).await;
        });
    }

    /// 完整扫码登录流程：获取二维码 → 推送前端 → 轮询扫码状态 → 保存凭证。
    /// 成功返回 true 并写入凭证；超时 / 取消返回 false。
    pub async fn login_flow(&self, app: &AppHandle) -> Result<bool, String> {
        self.stop();
        self.stopped.store(false, Ordering::SeqCst);
        self.set_status("qr_pending");
        let _ = app.emit("wechat-status", self.snapshot());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(480);
        let mut refresh_count = 0usize;

        let (mut qrcode, img) = self.fetch_qr(DEFAULT_BOT_TYPE).await?;
        let qr_b64 = render_qr_png_base64(&img);
        let _ = app.emit("wechat-qr", json!({ "url": qr_b64 }));

        while std::time::Instant::now() < deadline {
            if self.stopped.load(Ordering::SeqCst) {
                self.set_status("idle");
                let _ = app.emit("wechat-status", self.snapshot());
                return Ok(false);
            }

            let status = self.poll_qr_status(&qrcode).await?;
            match status["status"].as_str().unwrap_or("wait") {
                "confirmed" => {
                    let bot_token = status["bot_token"].as_str().unwrap_or("").to_string();
                    let account_id = status["ilink_bot_id"].as_str().unwrap_or("").to_string();
                    let base_url = status["baseurl"].as_str().unwrap_or("").to_string();
                    if bot_token.is_empty() || account_id.is_empty() {
                        return Err("扫码确认但服务器未返回完整凭证".to_string());
                    }
                    *self.token.lock().unwrap() = Some(bot_token.clone());
                    *self.account_id.lock().unwrap() = Some(account_id.clone());
                    if !base_url.is_empty() {
                        *self.base_url.lock().unwrap() = base_url.clone();
                    }
                    self.persist_credentials(&bot_token, &account_id, &base_url);
                    self.set_status("connected");
                    let _ = app.emit("wechat-status", self.snapshot());
                    return Ok(true);
                }
                "expired" => {
                    refresh_count += 1;
                    if refresh_count > 3 {
                        self.set_status("idle");
                        let _ = app.emit("wechat-status", self.snapshot());
                        return Err("二维码多次过期，请重新发起登录".to_string());
                    }
                    let (q, img2) = self.fetch_qr(DEFAULT_BOT_TYPE).await?;
                    qrcode = q;
                    let qr_b64 = render_qr_png_base64(&img2);
                    let _ = app.emit("wechat-qr", json!({ "url": qr_b64 }));
                }
                _ => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }

        self.set_status("idle");
        let _ = app.emit("wechat-status", self.snapshot());
        Ok(false)
    }

    /// 获取登录二维码，返回 (qrcode, 图片内容)
    async fn fetch_qr(&self, bot_type: &str) -> Result<(String, String), String> {
        let base = self.base_url.lock().unwrap().clone();
        let url = format!(
            "{}/ilink/bot/get_bot_qrcode?bot_type={}",
            base.trim_end_matches('/'),
            bot_type
        );
        let resp = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_millis(QR_POLL_TIMEOUT_MS))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        Ok((
            v["qrcode"].as_str().unwrap_or("").to_string(),
            v["qrcode_img_content"].as_str().unwrap_or("").to_string(),
        ))
    }

    /// 轮询扫码状态
    async fn poll_qr_status(&self, qrcode: &str) -> Result<Value, String> {
        let base = self.base_url.lock().unwrap().clone();
        let mut url = reqwest::Url::parse(&format!(
            "{}/ilink/bot/get_qrcode_status",
            base.trim_end_matches('/')
        ))
        .map_err(|e| e.to_string())?;
        url.query_pairs_mut().append_pair("qrcode", qrcode);

        let resp = self
            .client
            .get(url)
            .header("iLink-App-ClientVersion", "1")
            .timeout(std::time::Duration::from_millis(QR_POLL_TIMEOUT_MS))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        resp.json().await.map_err(|e| e.to_string())
    }

    /// 长轮询主循环：持续 getupdates → 逐条委派给 handle_message
    async fn updates_loop(self: Arc<Self>, app: AppHandle) {
        let mut buf = self
            .store
            .get_setting(K_SYNC_BUF)
            .ok()
            .flatten()
            .unwrap_or_default();
        let mut consecutive_failures = 0u32;
        let mut next_timeout = LONG_POLL_TIMEOUT_MS;

        while !self.stopped.load(Ordering::SeqCst) {
            match self.poll_updates(&mut buf, &mut next_timeout).await {
                Ok(msgs) => {
                    consecutive_failures = 0;
                    for m in msgs {
                        let app2 = app.clone();
                        let st = self.clone();
                        tauri::async_runtime::spawn(async move {
                            st.handle_message(&app2, m).await;
                        });
                    }
                }
                Err(e) => {
                    if self.stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    // 会话过期（errcode -14）：清空凭证，回到未登录
                    if e.contains("-14") || e.to_lowercase().contains("expired") {
                        self.on_session_expired(&app);
                        break;
                    }
                    consecutive_failures += 1;
                    let delay = if consecutive_failures >= 3 {
                        consecutive_failures = 0;
                        std::time::Duration::from_secs(30)
                    } else {
                        std::time::Duration::from_secs(2)
                    };
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    fn on_session_expired(&self, app: &AppHandle) {
        self.stop();
        self.logout();
        let _ = app.emit("wechat-status", self.snapshot());
    }

    /// 单次长轮询：返回新消息列表；客户端超时 / 服务端空闲返回空
    async fn poll_updates(
        &self,
        buf: &mut String,
        next_timeout: &mut u64,
    ) -> Result<Vec<Value>, String> {
        let body = json!({
            "get_updates_buf": *buf,
            "base_info": { "channel_version": CHANNEL_VERSION }
        });
        // 客户端超时给服务端 hold 时长留缓冲，避免提前断开
        let resp = match self.post_raw("getupdates", &body, *next_timeout + 10_000).await {
            Ok(t) => t,
            Err(e) if e.is_timeout() => return Ok(vec![]),
            Err(e) => return Err(e.to_string()),
        };
        let v: Value = if resp.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&resp).map_err(|e| format!("getupdates 解析失败: {e}"))?
        };

        let ret = v["ret"].as_i64().unwrap_or(0);
        let errcode = v["errcode"].as_i64().unwrap_or(0);
        if ret != 0 || errcode != 0 {
            return Err(format!(
                "getupdates failed: ret={ret} errcode={errcode} errmsg={}",
                v["errmsg"].as_str().unwrap_or("")
            ));
        }

        if let Some(t) = v["longpolling_timeout_ms"].as_u64() {
            if t > 0 {
                *next_timeout = t;
            }
        }
        if let Some(nb) = v["get_updates_buf"].as_str() {
            if !nb.is_empty() {
                *buf = nb.to_string();
                let _ = self.store.set_setting(K_SYNC_BUF, nb);
            }
        }
        Ok(v.get("msgs").and_then(|m| m.as_array()).cloned().unwrap_or_default())
    }

    /// 处理一条入站消息：缓存 context_token → 解析审批回复 → 下载媒体 → 委派任务
    async fn handle_message(self: Arc<Self>, app: &AppHandle, msg: Value) {
        let from_user_id = msg["from_user_id"].as_str().unwrap_or("").to_string();
        let context_token = msg["context_token"].as_str().unwrap_or("").to_string();
        let message_type = msg["message_type"].as_i64().unwrap_or(0);

        // 仅处理用户发来的消息，忽略本机 bot 自身的回显
        if from_user_id.is_empty() || message_type != 1 {
            return;
        }

        // 入站消息去重：同一消息重投（长轮询重连/超时重发）直接跳过，避免任务被处理两次
        let id_key = msg["client_id"]
            .as_str()
            .or_else(|| msg["msg_id"].as_str())
            .map(|s| s.to_string());
        let (dedup_key, dedup_ttl) = match &id_key {
            Some(id) => (format!("id:{id}"), 600u64),
            None => {
                let raw = serde_json::to_string(&msg).unwrap_or_default();
                (format!("hash:{}", md5_hex(raw.as_bytes())), 120u64)
            }
        };
        {
            let mut seen = self.seen_msgs.lock().unwrap();
            let now = std::time::Instant::now();
            seen.retain(|k, t| {
                let ttl = if k.starts_with("id:") { 600 } else { dedup_ttl };
                now.duration_since(*t) < std::time::Duration::from_secs(ttl)
            });
            if seen.insert(dedup_key, now).is_some() {
                return; // 重复投递
            }
        }

        if !context_token.is_empty() {
            self.cache_context_token(&from_user_id, &context_token);
        }
        *self.last_user.lock().unwrap() = Some(from_user_id.clone());

        let text = extract_text(&msg);

        // 记录入站消息到共用日志（手机发来的指令；空文本标注为媒体消息）
        let log_text = if text.trim().is_empty() {
            "[图片/媒体消息]".to_string()
        } else {
            text.clone()
        };
        self.log.push("in", "wechat", "微信", &from_user_id, &log_text);

        // 解析微信二次确认回复（允许 / 拒绝）
        if let Some(approved) = parse_approval(&text) {
            // 先取出待审批 id 并释放锁，避免 MutexGuard 跨越 await 导致 future 非 Send
            let approval_id = {
                let mut pa = self.pending_approvals.lock().unwrap();
                let id = pa.get(&from_user_id).cloned();
                if id.is_some() {
                    pa.remove(&from_user_id);
                }
                id
            };
            if let Some(approval_id) = approval_id {
                app.state::<crate::AppState>()
                    .security
                    .resolve(&approval_id, approved);
                let ack = if approved {
                    "已确认执行。"
                } else {
                    "已拒绝该操作。"
                };
                let _ = self.send_text(&from_user_id, ack).await;
                return;
            }
        }

        // 下载媒体附件（图片 / 视频 / 文件 / 语音），附到指令上下文
        let media = self.download_media(&msg).await;
        let mut content = text.clone();
        if let Some((path, kind)) = media {
            let label = match kind.as_str() {
                "image" => "图片",
                "video" => "视频",
                "voice" => "语音",
                _ => "文件",
            };
            content = format!(
                "{}\n\n【微信{label}】完整路径：{path}\n（如需识别或处理，请用对应工具读取该完整路径；图片可用 image_describe 或 ocr 工具）",
                text.trim()
            );
        }

        if content.trim().is_empty() {
            let _ = self
                .send_text(&from_user_id, "白泽收到，但目前只支持文字指令或图片，请发送任务描述。")
                .await;
            return;
        }

        self.run_agent(app, &from_user_id, &content).await;
    }

    /// 委派任务给 Agent 循环（Supervisor），完成后把结果发回微信。
    /// 回复里若包含本地图片路径（截图/生成的图表等），自动转成真实图片消息发出，
    /// 并把文本里的路径替换为「（图片已发送）」——避免把一串保存地址当回复发出去。
    async fn run_agent(self: Arc<Self>, app: &AppHandle, from_user_id: &str, input: &str) {
        let _ = self.send_text(from_user_id, "白泽收到，正在处理…").await;
        let state = app.state::<crate::AppState>();
        let answer = crate::agent::Supervisor::new(app, state.inner())
            .run(input, vec![])
            .await;
        match answer {
            Ok(a) => {
                let (text, images) = extract_reply_images(&a);
                let _ = self.send_text(from_user_id, &text).await;
                for p in images.iter().take(3) {
                    if let Err(e) = self.send_image(from_user_id, p, None).await {
                        // 图片发送失败：把路径补回文本，至少用户能拿到文件位置
                        let _ = self
                            .send_text(from_user_id, &format!("（图片发送失败：{e}）路径：{p}"))
                            .await;
                    }
                }
            }
            Err(e) => {
                let _ = self
                    .send_text(from_user_id, &format!("白泽处理任务时出错：{e}"))
                    .await;
            }
        }
    }

    /// 向微信推送高危操作确认，等待用户回复「允许 / 拒绝」。
    /// 返回是否真正推送出去（无活跃微信用户返回 false，此时仅走桌面端审批）。
    pub async fn push_approval(&self, approval_id: &str, what: &str, detail: &str) -> bool {
        let user = match self.last_user.lock().unwrap().clone() {
            Some(u) => u,
            None => return false, // 无活跃微信用户，仅走桌面端审批
        };
        self.pending_approvals
            .lock()
            .unwrap()
            .insert(user.clone(), approval_id.to_string());
        let text = format!(
            "白泽需要你的确认：\n【{what}】\n{detail}\n\n回复「允许」执行，回复「拒绝」取消。"
        );
        let _ = self.send_text(&user, &text).await;
        true
    }

    fn cache_context_token(&self, user_id: &str, token: &str) {
        {
            let mut map = self.context_tokens.lock().unwrap();
            map.insert(user_id.to_string(), token.to_string());
        }
        // 轻量持久化，避免重启后无法回复（需等用户先发新消息）
        if let Ok(json) = serde_json::to_string(&*self.context_tokens.lock().unwrap()) {
            let _ = self.store.set_setting(K_CONTEXT_TOKENS, &json);
        }
    }

    /// 发送文本消息（需目标用户曾发来消息以取得 context_token）
    async fn send_text(&self, to: &str, text: &str) -> Result<(), String> {
        let ct = self
            .context_tokens
            .lock()
            .unwrap()
            .get(to)
            .cloned()
            .ok_or_else(|| "缺少该用户的 context_token（需对方先发来消息）".to_string())?;

        let clipped: String = if text.chars().count() > MAX_REPLY_CHARS {
            let mut s: String = text.chars().take(MAX_REPLY_CHARS).collect();
            s.push_str("\n…（回复过长已截断）");
            s
        } else {
            text.to_string()
        };

        let body = json!({
            "msg": {
                "from_user_id": "",
                "to_user_id": to,
                "client_id": format!("wechat-ilink:{}", uuid::Uuid::new_v4().simple()),
                "message_type": 2,
                "message_state": 2,
                "context_token": ct,
                "item_list": [ { "type": 1, "text_item": { "text": clipped } } ],
            },
            "base_info": { "channel_version": CHANNEL_VERSION }
        });
        let v = self.post_json("sendmessage", &body, API_TIMEOUT_MS).await?;
        let ret = v["ret"].as_i64().unwrap_or(0);
        let errcode = v["errcode"].as_i64().unwrap_or(0);
        if ret != 0 || errcode != 0 {
            return Err(format!(
                "sendmessage rejected: ret={ret} errcode={errcode} errmsg={}",
                v["errmsg"].as_str().unwrap_or("")
            ));
        }
        self.log.push("out", "wechat", "微信", to, &clipped);
        Ok(())
    }

    /// 把本地图片/视频/文件发送给指定微信用户（完整流程：getuploadurl → AES 加密 → CDN 上传 → sendmessage）。
    /// 参考 wechat-clawbot / wechat-ilink-client 的 uploadMedia + sendImage 实现。
    pub async fn send_image(&self, to: &str, path: &str, caption: Option<&str>) -> Result<(), String> {
        let ct = self
            .context_tokens
            .lock()
            .unwrap()
            .get(to)
            .cloned()
            .ok_or_else(|| "缺少该用户的 context_token（需对方先发来消息）".to_string())?;

        let plaintext = std::fs::read(path).map_err(|e| format!("读取文件失败: {e}"))?;
        if plaintext.is_empty() {
            return Err("文件内容为空".to_string());
        }

        let rawsize = plaintext.len();
        let filesize = aes_ecb_padded_size(rawsize);
        let rawfilemd5 = md5_hex(&plaintext);
        // filekey 与 aeskey 均为 16 随机字节；filekey 用 hex 表示，aeskey 以 hex 传给 getuploadurl
        let filekey = hex::encode(uuid::Uuid::new_v4().as_bytes());
        let aeskey = *uuid::Uuid::new_v4().as_bytes();
        let aeskey_hex = hex::encode(aeskey);
        let (media_type, kind) = infer_upload_media_type(path);

        let v = self
            .get_upload_url(to, &filekey, media_type, rawsize, &rawfilemd5, filesize, &aeskey_hex)
            .await?;
        let upload_url = pick_upload_url(&v, &filekey)
            .ok_or_else(|| format!("getuploadurl 未返回上传地址: {v}"))?;

        let ciphertext = encrypt_aes_ecb_pkcs7(&plaintext, &aeskey)?;
        let download_param = self.upload_to_cdn(&upload_url, &ciphertext).await?;

        // 关键：image_item.media.aes_key = base64(键的 16 字节 hex 字符串的 ASCII)，
        // 与 SDK `Buffer.from(uploaded.aeskey).toString("base64")`（aeskey 为 hex 字符串）一致。
        use base64::Engine;
        let aes_key_b64 = base64::engine::general_purpose::STANDARD.encode(aeskey_hex.as_bytes());

        // 逐条发送（可选 caption 文本 + 媒体），与 SDK 逐 item 各发一条一致
        let mut items: Vec<Value> = Vec::new();
        if let Some(cap) = caption {
            let cap = cap.trim().to_string();
            if !cap.is_empty() {
                items.push(json!({ "type": 1, "text_item": { "text": cap } }));
            }
        }
        let media = json!({
            "encrypt_query_param": download_param,
            "aes_key": aes_key_b64,
            "encrypt_type": 1
        });
        match kind {
            "image" => items.push(json!({
                "type": 2,
                "image_item": { "media": media, "mid_size": filesize }
            })),
            "video" => items.push(json!({
                "type": 5,
                "video_item": { "media": media, "video_size": filesize }
            })),
            _ => {
                let fname = std::path::Path::new(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file")
                    .to_string();
                items.push(json!({
                    "type": 4,
                    "file_item": { "media": media, "file_name": fname, "len": rawsize.to_string() }
                }));
            }
        }

        for item in items {
            let body = json!({
                "msg": {
                    "from_user_id": "",
                    "to_user_id": to,
                    "client_id": format!("wechat-ilink:{}", uuid::Uuid::new_v4().simple()),
                    "message_type": 2,
                    "message_state": 2,
                    "context_token": ct,
                    "item_list": [ item ],
                },
                "base_info": { "channel_version": CHANNEL_VERSION }
            });
            let v = self.post_json("sendmessage", &body, API_TIMEOUT_MS).await?;
            let ret = v["ret"].as_i64().unwrap_or(0);
            let errcode = v["errcode"].as_i64().unwrap_or(0);
            if ret != 0 || errcode != 0 {
                return Err(format!(
                    "sendmessage rejected: ret={ret} errcode={errcode} errmsg={}",
                    v["errmsg"].as_str().unwrap_or("")
                ));
            }
        }
        Ok(())
    }

    /// 把图片发送给最近一次指挥白泽的微信用户（无活跃用户返回错误）
    pub async fn send_image_to_last_user(
        &self,
        path: &str,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let user = self
            .last_user
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "尚无微信用户发起过指令".to_string())?;
        self.send_image(&user, path, caption).await
    }

    /// 把文本发送给最近一次指挥白泽的微信用户（无活跃用户返回错误）
    pub async fn send_text_to_last_user(&self, text: &str) -> Result<(), String> {
        let user = self
            .last_user
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "尚无微信用户发起过指令".to_string())?;
        self.send_text(&user, text).await
    }

    /// 获取 CDN 上传预签名地址（ilink/bot/getuploadurl）
    async fn get_upload_url(
        &self,
        to: &str,
        filekey: &str,
        media_type: u32,
        rawsize: usize,
        rawfilemd5: &str,
        filesize: usize,
        aeskey_hex: &str,
    ) -> Result<Value, String> {
        let body = json!({
            "filekey": filekey,
            "media_type": media_type,
            "to_user_id": to,
            "rawsize": rawsize,
            "rawfilemd5": rawfilemd5,
            "filesize": filesize,
            "no_need_thumb": true,
            "aeskey": aeskey_hex,
            "base_info": { "channel_version": CHANNEL_VERSION }
        });
        let v = self.post_json("getuploadurl", &body, API_TIMEOUT_MS).await?;
        let ret = v["ret"].as_i64().unwrap_or(0);
        let errcode = v["errcode"].as_i64().unwrap_or(0);
        if ret != 0 || errcode != 0 {
            return Err(format!(
                "getuploadurl rejected: ret={ret} errcode={errcode} errmsg={}",
                v["errmsg"].as_str().unwrap_or("")
            ));
        }
        Ok(v)
    }

    /// 把加密后的密文 POST 到 CDN 上传地址，返回下载参数字符串（x-encrypted-param）
    async fn upload_to_cdn(&self, upload_url: &str, ciphertext: &[u8]) -> Result<String, String> {
        let resp = self
            .client
            .post(upload_url)
            .header("Content-Type", "application/octet-stream")
            .body(ciphertext.to_vec())
            .send()
            .await
            .map_err(|e| format!("CDN 上传请求失败: {e}"))?;

        if resp.status().as_u16() != 200 {
            let err = resp
                .headers()
                .get("x-error-message")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            return Err(format!("CDN 上传返回 HTTP {}: {err}", resp.status()));
        }
        let param = resp
            .headers()
            .get("x-encrypted-param")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if param.is_empty() {
            return Err("CDN 上传响应缺少 x-encrypted-param".to_string());
        }
        Ok(param)
    }

    /// 下载并解密入站媒体，返回 (本地路径, kind)；无媒体返回 None
    async fn download_media(&self, msg: &Value) -> Option<(String, String)> {
        let (item, kind) = pick_media_item(msg)?;
        let url = media_url(&item, &kind)?;
        let bytes = self
            .client
            .get(&url)
            .send()
            .await
            .ok()?
            .bytes()
            .await
            .ok()?
            .to_vec();
        if bytes.is_empty() {
            return None;
        }
        let data = match resolve_aes_key(&item, &kind) {
            Some(key) => decrypt_aes_ecb_pkcs7(&bytes, &key).unwrap_or(bytes),
            None => bytes,
        };
        let fname = item
            .get(&format!("{kind}_item"))
            .and_then(|it| it["file_name"].as_str())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let ext = ext_for(&kind, &fname, &data);
        let dir = std::env::temp_dir().join("baize_wechat_media");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!(
            "wechat-{kind}-{}{ext}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, &data).ok()?;
        Some((path.to_string_lossy().to_string(), kind))
    }

    async fn post_json(&self, endpoint: &str, body: &Value, timeout_ms: u64) -> Result<Value, String> {
        let text = self
            .post_raw(endpoint, body, timeout_ms)
            .await
            .map_err(|e| e.to_string())?;
        if text.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&text).map_err(|e| format!("响应解析失败: {e}"))
    }

    async fn post_raw(
        &self,
        endpoint: &str,
        body: &Value,
        timeout_ms: u64,
    ) -> Result<String, reqwest::Error> {
        let body_str = serde_json::to_string(body).expect("序列化 JSON 失败");
        let base = self.base_url.lock().unwrap().clone();
        let url = format!("{}/ilink/bot/{}", base.trim_end_matches('/'), endpoint);
        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("AuthorizationType", "ilink_bot_token")
            .header("X-WECHAT-UIN", random_wechat_uin())
            .timeout(std::time::Duration::from_millis(timeout_ms))
            .body(body_str);
        if let Some(t) = self.token.lock().unwrap().clone() {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        Ok(req.send().await?.text().await?)
    }
}

/// 把扫码登录 URL（iLink 返回的 qrcode_img_content，形如
/// https://liteapp.weixin.qq.com/q/...?qrcode=<token>&bot_type=3）本地渲染成
/// 二维码 PNG，返回 `data:image/png;base64,...` 形式的数据 URL，供前端 <img> 直接显示。
/// 微信扫码后会打开该 liteapp 登录 URL 触发登录流程。生成失败返回空串。
fn render_qr_png_base64(data: &str) -> String {
    use base64::Engine;
    if data.is_empty() {
        return String::new();
    }
    let code = match qrcode::QrCode::new(data) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    // 渲染为灰度图，最小宽度 200 像素（扫码识别友好），保留 4 模块静默区
    let img: image::GrayImage = code
        .render::<image::Luma<u8>>()
        .min_dimensions(200, 200)
        .quiet_zone(true)
        .build();
    let mut cursor = std::io::Cursor::new(Vec::new());
    if image::DynamicImage::ImageLuma8(img)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .is_err()
    {
        return String::new();
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(cursor.into_inner());
    format!("data:image/png;base64,{b64}")
}

/// X-WECHAT-UIN 头：随机 u32 的十进制串再 base64（对齐 SDK）
fn random_wechat_uin() -> String {
    use base64::Engine;
    let b = uuid::Uuid::new_v4();
    let bytes = b.as_bytes();
    let u = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    base64::engine::general_purpose::STANDARD.encode(u.to_string())
}

/// 从消息 item_list 提取文本（TEXT 项，或带转写文本的 VOICE 项）
fn extract_text(msg: &Value) -> String {
    let Some(items) = msg.get("item_list").and_then(|i| i.as_array()) else {
        return String::new();
    };
    for item in items {
        match item["type"].as_i64().unwrap_or(0) {
            1 => {
                return item["text_item"]["text"].as_str().unwrap_or("").to_string();
            }
            3 => {
                if let Some(t) = item["voice_item"]["text"].as_str() {
                    if !t.is_empty() {
                        return t.to_string();
                    }
                }
            }
            _ => {}
        }
    }
    String::new()
}

/// 识别审批回复，返回 Some(true)=允许 / Some(false)=拒绝 / None=非审批语句
fn parse_approval(text: &str) -> Option<bool> {
    let t = text.trim();
    const ALLOW: &[&str] = &["允许", "同意", "确认", "好", "可以", "执行", "y", "Y", "yes", "ok", "OK"];
    const DENY: &[&str] = &["拒绝", "取消", "不行", "算了", "不", "n", "N", "no", "NO"];
    if ALLOW.contains(&t) {
        return Some(true);
    }
    if DENY.contains(&t) {
        return Some(false);
    }
    None
}

/// 选取一条可下载的媒体项（按 image > video > file > voice 优先级）
fn pick_media_item(msg: &Value) -> Option<(Value, String)> {
    let items = msg.get("item_list")?.as_array()?;
    let mut best: Option<(i64, Value, String)> = None;
    for item in items {
        let ty = item["type"].as_i64().unwrap_or(0);
        let (kind, key): (&str, &str) = match ty {
            2 => ("image", "image_item"),
            5 => ("video", "video_item"),
            4 => ("file", "file_item"),
            3 => ("voice", "voice_item"),
            _ => continue,
        };
        let media = &item[key]["media"];
        let downloadable = media["encrypt_query_param"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false)
            || media["full_url"].as_str().map(|s| !s.is_empty()).unwrap_or(false);
        if !downloadable {
            continue;
        }
        let pr = match ty {
            2 => 0,
            5 => 1,
            4 => 2,
            _ => 3,
        };
        if best.as_ref().map(|(p, _, _)| pr < *p).unwrap_or(true) {
            best = Some((pr, item.clone(), kind.to_string()));
        }
    }
    best.map(|(_, item, kind)| (item, kind))
}

/// 构造媒体下载 URL
fn media_url(item: &Value, kind: &str) -> Option<String> {
    let media = &item[&format!("{kind}_item")]["media"];
    if let Some(full) = media["full_url"].as_str() {
        if !full.is_empty() {
            return Some(full.to_string());
        }
    }
    if let Some(param) = media["encrypt_query_param"].as_str() {
        if !param.is_empty() {
            let mut u = reqwest::Url::parse(&format!("{}/download", CDN_BASE_URL)).ok()?;
            u.query_pairs_mut().append_pair("encrypted_query_param", param);
            return Some(u.to_string());
        }
    }
    None
}

/// 解析媒体 AES 密钥为 16 字节（图片 aeskey 为 hex；其余 media.aes_key 为 base64 两种编码）
fn resolve_aes_key(item: &Value, kind: &str) -> Option<Vec<u8>> {
    if kind == "image" {
        if let Some(hexstr) = item["image_item"]["aeskey"].as_str() {
            if let Ok(k) = hex::decode(hexstr) {
                if k.len() == 16 {
                    return Some(k);
                }
            }
        }
    }
    // 通用：media.aes_key 为 base64（16 字节原始，或 32 位 hex 字符串的 base64）
    let b64 = item
        .get(&format!("{kind}_item"))?
        .get("media")?
        .get("aes_key")?
        .as_str()?;
    parse_aes_key_b64(b64)
}

/// 解析密码（base64 编码：16 字节原始密钥，或 32 位 hex 字符串的 base64）
fn parse_aes_key_b64(b64: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if decoded.len() == 16 {
        return Some(decoded);
    }
    if decoded.len() == 32 && decoded.iter().all(|b| b.is_ascii_hexdigit()) {
        if let Ok(k) = hex::decode(&decoded) {
            if k.len() == 16 {
                return Some(k);
            }
        }
    }
    None
}

/// AES-128-ECB 解密（PKCS7 去填充）
fn decrypt_aes_ecb_pkcs7(data: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    use aes::cipher::{generic_array::GenericArray, BlockDecrypt, KeyInit};

    if key.len() != 16 || data.is_empty() || data.len() % 16 != 0 {
        return None;
    }
    let cipher = aes::Aes128::new(GenericArray::from_slice(key));
    let mut out = data.to_vec();
    for chunk in out.chunks_exact_mut(16) {
        let block = GenericArray::from_mut_slice(chunk);
        cipher.decrypt_block(block);
    }
    // PKCS7 去填充；填充非法则返回原样
    let pad = *out.last()? as usize;
    if pad == 0 || pad > 16 {
        return Some(out);
    }
    out.truncate(out.len() - pad);
    Some(out)
}

/// AES-128-ECB 加密（PKCS7 填充），用于把媒体上传到微信 CDN 前加密
fn encrypt_aes_ecb_pkcs7(data: &[u8], key: &[u8]) -> Result<Vec<u8>, String> {
    use aes::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};

    if key.len() != 16 {
        return Err("AES 密钥必须为 16 字节".to_string());
    }
    let cipher = aes::Aes128::new(GenericArray::from_slice(key));
    let pad = (16 - (data.len() % 16)) as u8;
    let mut padded = data.to_vec();
    padded.extend(std::iter::repeat(pad).take(pad as usize));
    for chunk in padded.chunks_exact_mut(16) {
        let block = GenericArray::from_mut_slice(chunk);
        cipher.encrypt_block(block);
    }
    Ok(padded)
}

/// AES-128-ECB 密文大小（PKCS7 填充到 16 字节边界）：ceil((n+1)/16)*16
fn aes_ecb_padded_size(n: usize) -> usize {
    ((n + 16) / 16) * 16
}

/// 计算文件 MD5（hex），getuploadurl 的 rawfilemd5 字段
fn md5_hex(data: &[u8]) -> String {
    format!("{:x}", md5::compute(data))
}

/// 从 Agent 回复中提取本地图片路径（截图 / 生成的图表等）：
/// 返回（替换后的文本, 磁盘上真实存在的图片路径列表）。
/// 文本中的路径统一替换为「（图片已发送）」，图片本体经 CDN 上传后以图片消息发出。
/// 两级匹配：① 盘符绝对路径；② 白泽产物文件名（baize-screenshot-<ts>.png 等，
/// 模型有时只写相对路径或文件名——按工作目录与当前 cwd 兜底解析）。
fn extract_reply_images(answer: &str) -> (String, Vec<String>) {
    let mut images: Vec<String> = Vec::new();
    let mut out = answer.to_string();

    let re_abs = regex::Regex::new(
        r#"(?i)[a-z][a-z]:[\\/][^\r\n"'（）【】\[\]{}<>，。；！？!?]+?\.(?:png|jpe?g|gif|webp|bmp)"#,
    )
    .expect("图片路径正则编译失败");
    let re_name = regex::Regex::new(
        r#"(?i)\bbaize-(?:screenshot|browser|som|ocr-pre|captcha)-\d+\.(?:png|jpe?g)"#,
    )
    .expect("产物文件名正则编译失败");

    let try_push = |raw: &str, out: &mut String, images: &mut Vec<String>| {
        let p = crate::tools::resolve_path(raw);
        if !std::path::Path::new(&p).is_file() {
            return;
        }
        if images.iter().any(|e| e.eq_ignore_ascii_case(&p)) {
            return;
        }
        images.push(p.clone());
        *out = out.replace(raw, "（图片已发送）");
    };

    for m in re_abs.find_iter(answer) {
        try_push(m.as_str(), &mut out, &mut images);
    }
    // 文件名兜底：未被绝对路径覆盖到的产物名，尝试 cwd 与工作空间解析
    for m in re_name.find_iter(answer) {
        let name = m.as_str();
        let mut candidates: Vec<String> = vec![name.to_string()];
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.join(name).to_string_lossy().to_string());
        }
        let hit = candidates
            .iter()
            .find(|c| std::path::Path::new(c).is_file())
            .cloned();
        if let Some(p) = hit {
            if images.iter().any(|e| e.eq_ignore_ascii_case(&p)) {
                continue;
            }
            images.push(p.clone());
            out = out.replace(name, "（图片已发送）");
        }
    }
    (out, images)
}

/// 依据文件扩展名推断上传媒体类型：返回 (media_type, kind)，
/// 与 wechat-ilink-client 的 UploadMediaType（IMAGE=1 / VIDEO=2 / FILE=3）一致
fn infer_upload_media_type(path: &str) -> (u32, &'static str) {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => (1, "image"),
        "mp4" | "mov" | "webm" | "mkv" | "avi" => (2, "video"),
        _ => (3, "file"),
    }
}

/// 从 getuploadurl 响应中取出 CDN 上传地址：
/// 优先直接 URL（upload_full_url / full_upload_url / upload_url），否则用 upload_param + filekey 拼接
fn pick_upload_url(v: &Value, filekey: &str) -> Option<String> {
    for k in ["upload_full_url", "full_upload_url", "upload_url"] {
        if let Some(s) = v[k].as_str() {
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    let param = v["upload_param"]
        .as_str()
        .or(v["uploadParam"].as_str())
        .or(v["encrypted_query_param"].as_str())?;
    if param.is_empty() {
        return None;
    }
    let mut u = reqwest::Url::parse(&format!("{}/upload", CDN_BASE_URL)).ok()?;
    u.query_pairs_mut()
        .append_pair("encrypted_query_param", param);
    u.query_pairs_mut().append_pair("filekey", filekey);
    if let Some(t) = v["taskid"].as_str().or(v["task_id"].as_str()) {
        if !t.is_empty() {
            u.query_pairs_mut().append_pair("taskid", t);
        }
    }
    Some(u.to_string())
}

/// 依据媒体类型 / 原始文件名 / 文件头魔数推断扩展名
fn ext_for(kind: &str, fname: &str, data: &[u8]) -> String {
    if !fname.is_empty() {
        if let Some(ext) = std::path::Path::new(fname)
            .extension()
            .and_then(|e| e.to_str())
        {
            if !ext.is_empty() {
                return format!(".{}", ext.to_lowercase());
            }
        }
    }
    match kind {
        "image" => detect_image_ext(data).to_string(),
        "video" => ".mp4".to_string(),
        "voice" => ".silk".to_string(),
        _ => ".bin".to_string(),
    }
}

/// 依据魔数判断图片扩展名（微信图片解密后可能无原始文件名）
fn detect_image_ext(data: &[u8]) -> &'static str {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return ".png";
    }
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return ".jpg";
    }
    if data.len() >= 6 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        return ".gif";
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return ".webp";
    }
    if data.len() >= 2 && data[0] == 0x42 && data[1] == 0x4D {
        return ".bmp";
    }
    ".png"
}

/// Tauri 命令：查询微信连接状态
#[tauri::command]
pub fn wechat_get_status(state: State<'_, crate::AppState>) -> Value {
    state.wechat.snapshot()
}

/// Tauri 命令：扫码登录（成功返回连接后快照；成功后自动启动长轮询）
#[tauri::command]
pub async fn wechat_login(state: State<'_, crate::AppState>, app: AppHandle) -> Result<Value, String> {
    let ok = state.wechat.login_flow(&app).await?;
    if ok {
        state.wechat.start(app.clone());
    }
    Ok(state.wechat.snapshot())
}

/// Tauri 命令：断开长轮询（保留凭证，可再启动）
#[tauri::command]
pub fn wechat_stop(state: State<'_, crate::AppState>, app: AppHandle) -> Value {
    state.wechat.stop();
    state.wechat.set_status("disconnected");
    let _ = app.emit("wechat-status", state.wechat.snapshot());
    state.wechat.snapshot()
}

/// Tauri 命令：重新启动长轮询（凭证已存在时）
#[tauri::command]
pub fn wechat_start(state: State<'_, crate::AppState>, app: AppHandle) -> Value {
    state.wechat.start(app);
    state.wechat.snapshot()
}

/// Tauri 命令：登出（清空凭证与内存状态）
#[tauri::command]
pub fn wechat_logout(state: State<'_, crate::AppState>, app: AppHandle) -> Value {
    state.wechat.logout();
    let _ = app.emit("wechat-status", state.wechat.snapshot());
    state.wechat.snapshot()
}

// ───────────────── wechat_send_image 工具 ─────────────────

/// wechat_send_image 工具：让 Agent 把图片发送到微信（发给最近一次指挥白泽的用户）。
/// 触发方式二（显式指令）与截图来源二（用户微信发图 → 识别后回图）都经此工具落地：
///   - path 留空：截取「当前屏幕」发送（截图来源一）
///   - path 指定：发送该本地图片（例如用户微信发来的图片的完整路径、或生成的图表）
pub struct WeChatSendImageTool {
    app: AppHandle,
}

impl WeChatSendImageTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for WeChatSendImageTool {
    fn name(&self) -> &str {
        "wechat_send_image"
    }
    fn description(&self) -> &str {
        "把图片发送到微信（发给最近一次指挥白泽的微信用户）。path 留空则截取当前屏幕发送；\
         也可传入本地图片的完整路径（用户从微信发来的图片路径、或生成的图表），实现「识别后回图」。\
         支持 caption 附加说明文字。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "要发送的图片本地完整路径；留空则自动截取当前屏幕发送"
                },
                "caption": {
                    "type": "string",
                    "description": "随图附加的说明文字（可选）"
                }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path_opt = args["path"]
            .as_str()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let caption = args["caption"].as_str().map(|s| s.to_string());

        let state = self.app.state::<crate::AppState>();
        let wechat = state.wechat.clone();
        let capability = state.capability.clone();

        // 截图来源：显式路径 → 当前屏幕截图
        let path = match path_opt {
            Some(p) => crate::tools::resolve_path(&p),
            None => capability
                .capture_screen()
                .map_err(|e| format!("截屏失败: {e}"))?
                .path,
        };
        let path_clone = path.clone();

        tauri::async_runtime::block_on(async move {
            wechat.send_image_to_last_user(&path, caption.as_deref()).await
        })
        .map(|_| json!({ "ok": true, "path": path_clone, "source": "sent_to_wechat" }))
    }
}

// ───────────────── ImChannel 实现（消息总线统一描述接口）─────────────────

impl crate::im::ImChannel for WeChatState {
    fn id(&self) -> &'static str {
        "wechat"
    }
    fn label(&self) -> &'static str {
        "微信"
    }
    fn has_credentials(&self) -> bool {
        WeChatState::has_credentials(self)
    }
    fn snapshot(&self) -> Value {
        WeChatState::snapshot(self)
    }
}