//! 飞书（Lark）自建应用通道：鉴权 + WebSocket 长连接收事件 + HTTP 发消息。
//!
//! 协议（据官方 lark_oapi Python SDK 逆向，未做端到端验证，接入前需在飞书开放平台：
//!   1. 创建自建应用，拿到 App ID / App Secret；
//!   2. 订阅事件 `im.message.receive_v1`（接收消息）；
//!   3. 开通机器人能力 + 消息发送权限。
//!
//! 连接流程：
//!   1. token:   POST /open-apis/auth/v3/tenant_access_token/internal  (app_id + app_secret)
//!   2. endpoint:GET /open-apis/callback/ws/endpoint (Bearer token) -> { endpoint, client_id, client_secret }
//!   3. 长连接:   连接 endpoint(wss://) 后收发 protobuf 二进制 `Frame`
//!       - Frame.method: 1=CONTROL(心跳) / 2=DATA(事件)
//!       - CONTROL 帧 header "type"="ping"/"pong" 用于保活
//!       - DATA 帧 payload = 事件 JSON（可能 gzip 压缩，见 header "compressType"）
//!   4. 发消息:   POST /open-apis/im/v1/messages (text / image)

use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_tungstenite::tungstenite::Message;

use crate::memory::MemoryStore;

const APP_BASE: &str = "https://open.feishu.cn";
const K_APP_ID: &str = "feishu_app_id";
const K_APP_SECRET: &str = "feishu_app_secret";
/// 心跳间隔（秒）：飞书长连接需周期性 ping 保活
const HEARTBEAT_SECS: u64 = 30;
/// 重连退避上限（秒）
const MAX_BACKOFF_SECS: u64 = 60;
/// 回复文本上限，超出截断
const MAX_REPLY_CHARS: usize = 8_000;

/// 飞书机器人状态（经 AppState 以 Arc 持有）
pub struct FeishuState {
    client: reqwest::Client,
    app_id: Mutex<Option<String>>,
    app_secret: Mutex<Option<String>>,
    tenant_token: Mutex<Option<String>>,
    tenant_token_expire_ms: Mutex<i64>,
    /// 最近一次发来消息的 chat_id（回传目标）
    last_user: Mutex<Option<String>>,
    /// chat_id -> approval_id（等待飞书二次确认的审批）
    pending_approvals: Mutex<HashMap<String, String>>,
    status: Mutex<String>,
    stopped: Arc<AtomicBool>,
    store: Arc<MemoryStore>,
    /// 消息收发日志池（跨通道共享）
    log: Arc<crate::im::ImLog>,
}

impl FeishuState {
    pub fn new(store: Arc<MemoryStore>, log: Arc<crate::im::ImLog>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("构建飞书 HTTP 客户端失败");

        let app_id = store
            .get_setting(K_APP_ID)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        let app_secret = store
            .get_setting(K_APP_SECRET)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());

        Self {
            client,
            app_id: Mutex::new(app_id),
            app_secret: Mutex::new(app_secret),
            tenant_token: Mutex::new(None),
            tenant_token_expire_ms: Mutex::new(0),
            last_user: Mutex::new(None),
            pending_approvals: Mutex::new(HashMap::new()),
            status: Mutex::new("idle".to_string()),
            stopped: Arc::new(AtomicBool::new(false)),
            store,
            log,
        }
    }

    /// 当前是否已配置 app_id / app_secret
    pub fn has_credentials(&self) -> bool {
        self.app_id.lock().unwrap().is_some() && self.app_secret.lock().unwrap().is_some()
    }

    /// 保存 / 更新凭证（写入本地库持久化）
    pub fn save_credentials(&self, app_id: &str, app_secret: &str) {
        *self.app_id.lock().unwrap() = if app_id.is_empty() {
            None
        } else {
            Some(app_id.to_string())
        };
        *self.app_secret.lock().unwrap() = if app_secret.is_empty() {
            None
        } else {
            Some(app_secret.to_string())
        };
        // 清空旧 token 缓存，强制下次重新换取
        *self.tenant_token.lock().unwrap() = None;
        *self.tenant_token_expire_ms.lock().unwrap() = 0;
        let _ = self.store.set_setting(K_APP_ID, app_id);
        let _ = self.store.set_setting(K_APP_SECRET, app_secret);
        self.set_status("idle");
    }

    /// 供前端轮询/展示的连接状态快照
    pub fn snapshot(&self) -> Value {
        json!({
            "status": *self.status.lock().unwrap(),
            "connected": self.has_credentials(),
            "app_id": self.app_id.lock().unwrap().clone(),
        })
    }

    fn set_status(&self, s: &str) {
        *self.status.lock().unwrap() = s.to_string();
    }

    /// 停止长连接接收
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// 启动长连接（凭证已配置的自动连接）
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        if !self.has_credentials() {
            self.set_status("idle");
            let _ = app.emit("feishu-status", self.snapshot());
            return;
        }
        self.stopped.store(false, Ordering::SeqCst);
        self.set_status("connecting");
        let _ = app.emit("feishu-status", self.snapshot());
        let st = self.clone();
        tauri::async_runtime::spawn(async move {
            st.ws_loop(app).await;
        });
    }

    /// 获取（带缓存的）tenant_access_token
    async fn get_tenant_token(&self) -> Result<String, String> {
        {
            let expire = *self.tenant_token_expire_ms.lock().unwrap();
            let now = now_ms();
            if let Some(t) = self.tenant_token.lock().unwrap().clone() {
                if !t.is_empty() && now < expire - 60_000 {
                    return Ok(t);
                }
            }
        }
        let app_id = self
            .app_id
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "缺少飞书 app_id".to_string())?;
        let app_secret = self
            .app_secret
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "缺少飞书 app_secret".to_string())?;
        let url = format!("{APP_BASE}/open-apis/auth/v3/tenant_access_token/internal");
        let resp = self
            .client
            .post(&url)
            .json(&json!({ "app_id": app_id, "app_secret": app_secret }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        if v["code"].as_i64().unwrap_or(-1) != 0 {
            return Err(format!(
                "获取 tenant_access_token 失败: {}",
                v["msg"].as_str().unwrap_or("")
            ));
        }
        let token = v["tenant_access_token"].as_str().unwrap_or("").to_string();
        let expire = v["expire"].as_i64().unwrap_or(0);
        if token.is_empty() {
            return Err("响应缺少 tenant_access_token".to_string());
        }
        *self.tenant_token.lock().unwrap() = Some(token.clone());
        *self.tenant_token_expire_ms.lock().unwrap() = now_ms() + expire * 1000;
        Ok(token)
    }

    /// 获取动态 WebSocket endpoint（长连接入口）
    async fn get_ws_endpoint(&self, token: &str) -> Result<(String, String, String), String> {
        let url = format!("{APP_BASE}/open-apis/callback/ws/endpoint");
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        if v["code"].as_i64().unwrap_or(-1) != 0 {
            return Err(format!(
                "获取 WS endpoint 失败: {}",
                v["msg"].as_str().unwrap_or("")
            ));
        }
        let d = &v["data"];
        Ok((
            d["endpoint"].as_str().unwrap_or("").to_string(),
            d["client_id"].as_str().unwrap_or("").to_string(),
            d["client_secret"].as_str().unwrap_or("").to_string(),
        ))
    }

    /// 长连接主循环：断线自动重连（指数退避）
    async fn ws_loop(self: Arc<Self>, app: AppHandle) {
        let mut backoff = 1u64;
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                break;
            }
            match self.clone().connect_once(&app).await {
                Ok(()) => backoff = 1,
                Err(e) => eprintln!("[飞书] 长连接断开: {e}，{backoff} 秒后重连"),
            }
            if self.stopped.load(Ordering::SeqCst) {
                break;
            }
            self.set_status("reconnecting");
            let _ = app.emit("feishu-status", self.snapshot());
            tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(MAX_BACKOFF_SECS);
        }
        self.set_status("idle");
        let _ = app.emit("feishu-status", self.snapshot());
    }

    /// 单次建连 + 收发循环；连接关闭或出错返回
    async fn connect_once(self: Arc<Self>, app: &AppHandle) -> Result<(), String> {
        let token = self.get_tenant_token().await?;
        let (endpoint, client_id, _cs) = self.get_ws_endpoint(&token).await?;
        if endpoint.is_empty() {
            return Err("未获取到飞书 WebSocket endpoint".to_string());
        }
        let endpoint = ensure_wss(&endpoint);
        let (ws_stream, _resp) = tokio_tungstenite::connect_async(&endpoint)
            .await
            .map_err(|e| format!("连接飞书 WS 失败: {e}"))?;

        self.set_status("connected");
        let _ = app.emit("feishu-status", self.snapshot());

        let (mut sink, mut stream) = ws_stream.split();
        let service_id: u64 = client_id.parse().unwrap_or(0);
        let mut hb = tokio::time::interval(std::time::Duration::from_secs(HEARTBEAT_SECS));
        hb.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        hb.tick().await; // 跳过首个立即触发的 tick

        loop {
            if self.stopped.load(Ordering::SeqCst) {
                let _ = sink.close().await;
                return Ok(());
            }
            tokio::select! {
                _ = hb.tick() => {
                    let ping = encode_frame(1, service_id, &[("type", "ping")], &[]);
                    if let Err(e) = sink.send(Message::Binary(ping)).await {
                        return Err(format!("发送心跳失败: {e}"));
                    }
                }
                item = stream.next() => {
                    let Some(item) = item else { return Ok(()); };
                    match item {
                        Ok(Message::Binary(data)) => {
                            if let Some(pong) = self.clone().process_frame(app, service_id, &data).await {
                                if let Err(e) = sink.send(Message::Binary(pong)).await {
                                    return Err(format!("发送 pong 失败: {e}"));
                                }
                            }
                        }
                        Ok(Message::Text(_)) => {}
                        Ok(Message::Ping(p)) => {
                            let _ = sink.send(Message::Pong(p)).await;
                        }
                        Ok(Message::Close(_)) => return Ok(()),
                        Err(e) => return Err(format!("WS 读取错误: {e}")),
                        _ => {}
                    }
                }
            }
        }
    }

    /// 解析一帧二进制数据：CONTROL 帧返回 pong 字节；DATA 帧解压后派发事件。
    async fn process_frame(
        self: Arc<Self>,
        app: &AppHandle,
        service_id: u64,
        data: &[u8],
    ) -> Option<Vec<u8>> {
        let (method, headers, mut payload) = decode_frame(data)?;
        let msg_type = get_header(&headers, "type").unwrap_or("");
        if method == 1 {
            if msg_type == "ping" {
                return Some(encode_frame(1, service_id, &[("type", "pong")], &[]));
            }
            return None;
        }
        let compress = get_header(&headers, "compressType")
            .or_else(|| get_header(&headers, "compress_type"));
        if compress == Some("gzip") {
            if let Ok(p) = gzip_decompress(&payload) {
                payload = p;
            }
        }
        if let Ok(event) = serde_json::from_slice::<Value>(&payload) {
            let app2 = app.clone();
            let st = self.clone();
            tauri::async_runtime::spawn(async move {
                st.dispatch_event(&app2, &event).await;
            });
        }
        None
    }

    /// 分发事件：仅处理 im.message.receive_v1（用户发来消息）
    async fn dispatch_event(self: Arc<Self>, app: &AppHandle, event: &Value) {
        let event_type = event
            .get("header")
            .and_then(|h| h.get("event_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if event_type != "im.message.receive_v1" {
            return;
        }

        let message = event
            .get("event")
            .and_then(|e| e.get("message"))
            .cloned()
            .unwrap_or(json!({}));
        let chat_id = message
            .get("chat_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let message_type = message
            .get("message_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if chat_id.is_empty() {
            return;
        }

        // 记录最近活跃会话（回传目标）
        *self.last_user.lock().unwrap() = Some(chat_id.clone());

        // 提取文本（text 类型的 content 是 JSON 字符串）
        let mut text = String::new();
        if message_type == "text" {
            if let Some(c) = message.get("content").and_then(|v| v.as_str()) {
                if let Ok(v2) = serde_json::from_str::<Value>(c) {
                    text = v2.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                }
            }
        }

        // 记录入站消息到共用日志（手机发来的指令；非文本标注为媒体消息）
        let log_text = if text.trim().is_empty() {
            format!("[{}消息]", if message_type == "image" { "图片" } else { "媒体" })
        } else {
            text.clone()
        };
        self.log.push("in", "feishu", "飞书", &chat_id, &log_text);

        // 飞书二次确认回复（允许 / 拒绝）
        if let Some(approved) = parse_approval(&text) {
            let approval_id = {
                let mut pa = self.pending_approvals.lock().unwrap();
                let id = pa.get(&chat_id).cloned();
                if id.is_some() {
                    pa.remove(&chat_id);
                }
                id
            };
            if let Some(aid) = approval_id {
                app.state::<crate::AppState>().security.resolve(&aid, approved);
                let ack = if approved { "已确认执行。" } else { "已拒绝该操作。" };
                let _ = self.send_text(&chat_id, ack).await;
                return;
            }
        }

        if message_type == "image" {
            let _ = self
                .send_text(&chat_id, "白泽暂时还不能处理飞书图片，请发送文字指令。")
                .await;
            return;
        }

        if text.trim().is_empty() {
            return;
        }

        self.run_agent(app, &chat_id, &text).await;
    }

    /// 委派任务给 Agent 循环，完成后把结果发回飞书
    async fn run_agent(self: Arc<Self>, app: &AppHandle, chat_id: &str, input: &str) {
        let _ = self.send_text(chat_id, "白泽收到，正在处理…").await;
        let state = app.state::<crate::AppState>();
        let answer = crate::agent::Supervisor::new(app, state.inner())
            .run(input, vec![])
            .await;
        match answer {
            Ok(a) => {
                let _ = self.send_text(chat_id, &a).await;
            }
            Err(e) => {
                let _ = self
                    .send_text(chat_id, &format!("白泽处理任务时出错：{e}"))
                    .await;
            }
        }
    }

    /// 向飞书推送高危操作确认，等待用户回复「允许 / 拒绝」。
    /// 返回是否真正推送出去（无活跃会话返回 false，此时仅走桌面端审批）。
    pub async fn push_approval(&self, approval_id: &str, what: &str, detail: &str) -> bool {
        let chat = match self.last_user.lock().unwrap().clone() {
            Some(c) => c,
            None => return false,
        };
        self.pending_approvals
            .lock()
            .unwrap()
            .insert(chat.clone(), approval_id.to_string());
        let text = format!(
            "白泽需要你的确认：\n【{what}】\n{detail}\n\n回复「允许」执行，回复「拒绝」取消。"
        );
        let _ = self.send_text(&chat, &text).await;
        true
    }

    /// 把文本发送给最近一次指挥白泽的飞书会话
    pub async fn send_text_to_last_user(&self, text: &str) -> Result<(), String> {
        let chat = self
            .last_user
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "尚无飞书用户发起过指令".to_string())?;
        self.send_text(&chat, text).await
    }

    /// 把图片发送给最近一次指挥白泽的飞书会话（caption 作为前置文字发出）
    pub async fn send_image_to_last_user(
        &self,
        path: &str,
        caption: Option<&str>,
    ) -> Result<(), String> {
        let chat = self
            .last_user
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "尚无飞书用户发起过指令".to_string())?;
        if let Some(cap) = caption {
            let cap = cap.trim().to_string();
            if !cap.is_empty() {
                self.send_text(&chat, &cap).await?;
            }
        }
        self.send_image(&chat, path).await
    }

    /// 发送文本消息到指定 chat（receive_id_type=chat_id）
    async fn send_text(&self, chat_id: &str, text: &str) -> Result<(), String> {
        let token = self.get_tenant_token().await?;
        let clipped: String = if text.chars().count() > MAX_REPLY_CHARS {
            let mut s: String = text.chars().take(MAX_REPLY_CHARS).collect();
            s.push_str("\n…（回复过长已截断）");
            s
        } else {
            text.to_string()
        };
        let url = format!("{APP_BASE}/open-apis/im/v1/messages?receive_id_type=chat_id");
        let body = json!({
            "receive_id": chat_id,
            "msg_type": "text",
            "content": json!({ "text": clipped }).to_string(),
        });
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        if v["code"].as_i64().unwrap_or(-1) != 0 {
            return Err(format!("发送失败: {}", v["msg"].as_str().unwrap_or("")));
        }
        self.log.push("out", "feishu", "飞书", chat_id, &clipped);
        Ok(())
    }

    /// 上传图片并发送图片消息到指定 chat
    async fn send_image(&self, chat_id: &str, path: &str) -> Result<(), String> {
        let token = self.get_tenant_token().await?;
        let bytes = std::fs::read(path).map_err(|e| format!("读取图片失败: {e}"))?;
        let file_name = std::path::Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("image.png")
            .to_string();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str(mime_of(path))
            .map_err(|e| e.to_string())?;
        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part("image", part);

        let url = format!("{APP_BASE}/open-apis/im/v1/images");
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        if v["code"].as_i64().unwrap_or(-1) != 0 {
            return Err(format!("图片上传失败: {}", v["msg"].as_str().unwrap_or("")));
        }
        let image_key = v["data"]["image_key"].as_str().unwrap_or("").to_string();
        if image_key.is_empty() {
            return Err("图片上传响应缺少 image_key".to_string());
        }

        let msg_url = format!("{APP_BASE}/open-apis/im/v1/messages?receive_id_type=chat_id");
        let body = json!({
            "receive_id": chat_id,
            "msg_type": "image",
            "content": json!({ "image_key": image_key }).to_string(),
        });
        let resp = self
            .client
            .post(&msg_url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let v: Value = resp.json().await.map_err(|e| e.to_string())?;
        if v["code"].as_i64().unwrap_or(-1) != 0 {
            return Err(format!("图片发送失败: {}", v["msg"].as_str().unwrap_or("")));
        }
        Ok(())
    }
}

// ───────────────── ImChannel 实现（消息总线统一描述接口）─────────────────

impl crate::im::ImChannel for FeishuState {
    fn id(&self) -> &'static str {
        "feishu"
    }
    fn label(&self) -> &'static str {
        "飞书"
    }
    fn has_credentials(&self) -> bool {
        FeishuState::has_credentials(self)
    }
    fn snapshot(&self) -> Value {
        FeishuState::snapshot(self)
    }
}

// ───────────────── Tauri 命令 ─────────────────

/// 查询飞书连接状态
#[tauri::command]
pub fn feishu_get_status(state: State<'_, crate::AppState>) -> Value {
    state.feishu.snapshot()
}

/// 保存 / 更新飞书自建应用凭证
#[tauri::command]
pub fn feishu_save_credentials(
    state: State<'_, crate::AppState>,
    app_id: String,
    app_secret: String,
) -> Value {
    state.feishu.save_credentials(&app_id, &app_secret);
    state.feishu.snapshot()
}

/// 启动飞书长连接
#[tauri::command]
pub fn feishu_start(state: State<'_, crate::AppState>, app: AppHandle) -> Value {
    state.feishu.start(app);
    state.feishu.snapshot()
}

/// 停止飞书长连接（保留凭证）
#[tauri::command]
pub fn feishu_stop(state: State<'_, crate::AppState>, app: AppHandle) -> Value {
    state.feishu.stop();
    state.feishu.set_status("disconnected");
    let _ = app.emit("feishu-status", state.feishu.snapshot());
    state.feishu.snapshot()
}

// ───────────────── protobuf Frame 编解码（手写最小 wire format）─────────────────
// 仅覆盖 pbbp2.Frame 用到的字段：
//   Header { string key = 1; string value = 2; }
//   Frame  { repeated Header headers = 1; uint32 service = 2; FrameType method = 3;
//            uint64 SeqID = 4; uint64 LogID = 5; bytes payload = 6; }

fn write_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

fn read_varint(data: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut result = 0u64;
    let mut shift = 0u32;
    loop {
        if i >= data.len() {
            return None;
        }
        let b = data[i];
        i += 1;
        result |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some((result, i));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
}

fn write_string_field(out: &mut Vec<u8>, field: u32, s: &str) {
    write_varint(out, ((field as u64) << 3) | 2);
    write_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

/// 编码一帧（method: 1=CONTROL / 2=DATA）
fn encode_frame(method: u64, service: u64, headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in headers {
        let mut h = Vec::new();
        write_string_field(&mut h, 1, k);
        write_string_field(&mut h, 2, v);
        write_varint(&mut out, (1u64 << 3) | 2);
        write_varint(&mut out, h.len() as u64);
        out.extend_from_slice(&h);
    }
    if service != 0 {
        write_varint(&mut out, (2u64 << 3) | 0);
        write_varint(&mut out, service);
    }
    write_varint(&mut out, (3u64 << 3) | 0);
    write_varint(&mut out, method);
    if !payload.is_empty() {
        write_varint(&mut out, (6u64 << 3) | 2);
        write_varint(&mut out, payload.len() as u64);
        out.extend_from_slice(payload);
    }
    out
}

fn parse_header(bytes: &[u8]) -> Option<(String, String)> {
    let mut key = String::new();
    let mut value = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let (tag, ni) = read_varint(bytes, i)?;
        i = ni;
        let field = (tag >> 3) as u32;
        let wire = (tag & 7) as u8;
        match wire {
            2 => {
                let (len, ni) = read_varint(bytes, i)?;
                i = ni;
                let len = len as usize;
                if i + len > bytes.len() {
                    return None;
                }
                let s = std::str::from_utf8(&bytes[i..i + len]).ok()?.to_string();
                i += len;
                if field == 1 {
                    key = s;
                } else if field == 2 {
                    value = s;
                }
            }
            0 => {
                let (_v, ni) = read_varint(bytes, i)?;
                i = ni;
            }
            _ => return None,
        }
    }
    Some((key, value))
}

/// 解码一帧，返回 (method, headers, payload)
fn decode_frame(data: &[u8]) -> Option<(u64, Vec<(String, String)>, Vec<u8>)> {
    let mut method = 0u64;
    let mut headers = Vec::new();
    let mut payload = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let (tag, ni) = read_varint(data, i)?;
        i = ni;
        let field = (tag >> 3) as u32;
        let wire = (tag & 7) as u8;
        match wire {
            0 => {
                let (v, ni) = read_varint(data, i)?;
                i = ni;
                if field == 3 {
                    method = v;
                }
            }
            2 => {
                let (len, ni) = read_varint(data, i)?;
                i = ni;
                let len = len as usize;
                if i + len > data.len() {
                    return None;
                }
                let bytes = &data[i..i + len];
                if field == 1 {
                    headers.push(parse_header(bytes)?);
                } else if field == 6 {
                    payload = bytes.to_vec();
                }
                i += len;
            }
            1 => {
                if i + 8 > data.len() {
                    return None;
                }
                i += 8;
            }
            5 => {
                if i + 4 > data.len() {
                    return None;
                }
                i += 4;
            }
            _ => return None,
        }
    }
    Some((method, headers, payload))
}

fn get_header<'a>(headers: &'a [(String, String)], key: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = flate2::read::GzDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

fn ensure_wss(endpoint: &str) -> String {
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        endpoint.to_string()
    } else if endpoint.starts_with("http://") {
        endpoint.replacen("http://", "ws://", 1)
    } else if endpoint.starts_with("https://") {
        endpoint.replacen("https://", "wss://", 1)
    } else {
        format!("wss://{endpoint}")
    }
}

fn mime_of(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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