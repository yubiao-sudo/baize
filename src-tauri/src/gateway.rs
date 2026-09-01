//! 本地 AI 网关：把白泽从「App」升级为「本地 AI 基础设施」
//!
//! 在 `127.0.0.1:<port>` 起一个轻量 HTTP 服务（默认 11436，可配置、默认关闭），
//! 向外暴露标准的 OpenAI 兼容接口 + 白泽自有的记忆/工具端点，让 VS Code 插件、
//! Obsidian、数千个 OpenAI 兼容客户端都能直接复用白泽的模型路由 / 记忆 / 只读工具。
//!
//! 端点一览：
//!   GET  /api/health             → 健康检查与网关元信息
//!   GET  /v1/models              → OpenAI 兼容模型列表（当前生效 profile）
//!   POST /v1/chat/completions    → OpenAI 兼容对话（支持 stream SSE、tools 透传）
//!   POST /api/memory/remember    → 写入一条长期记忆
//!   POST /api/memory/search      → 关键词召回记忆
//!   GET  /api/tools              → 列出工具名 / 描述 / 权限分级 / schema
//!   POST /api/tools/execute      → 执行只读工具（写/高危需在白泽界面内审批）
//!
//! 安全：仅监听回环地址；可选 Bearer 令牌（网关令牌）；工具执行仅放行只读类。
//! 实现：手写极简 HTTP/1.1（无新依赖，借鉴 test_engineer 内 mock server 的 TcpListener 做法），
//! 单请求单连接（Connection: close），模型调用经独立 tokio Runtime 桥接异步。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::memory::MemoryStore;
use crate::model::{ChatMessage, ChatResponse, ModelRouter};
use crate::tools::{PermissionClass, ToolRegistry};

/// 网关默认端口（避开 Ollama 的 11434）
const DEFAULT_PORT: u16 = 11436;
/// 网关配置持久化键
const GATEWAY_CONFIG_KEY: &str = "gateway_config";
/// 请求体上限（防异常客户端打爆内存）
const MAX_BODY_LEN: usize = 16 * 1024 * 1024;

/// 网关配置（可运行时修改并持久化到 SQLite）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GatewayConfig {
    pub enabled: bool,
    pub port: u16,
    /// 访问令牌；为空则不校验（仅回环地址可访问）
    pub token: String,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            token: String::new(),
        }
    }
}

/// 网关状态（挂到 AppState，供 tauri 命令与后台服务线程共享）
pub struct GatewayState {
    inner: Arc<GatewayInner>,
}

struct GatewayInner {
    model: Arc<ModelRouter>,
    store: Arc<MemoryStore>,
    tools: Arc<ToolRegistry>,
    /// 独立 tokio Runtime：在同步的 HTTP handler 线程里 `block_on` 异步模型调用
    runtime: tokio::runtime::Runtime,
    enabled: AtomicBool,
    stop: AtomicBool,
    port: Mutex<u16>,
    token: Mutex<String>,
}

impl GatewayState {
    /// 从持久化配置恢复并（若已启用）立即启动监听
    pub fn new(
        model: Arc<ModelRouter>,
        store: Arc<MemoryStore>,
        tools: Arc<ToolRegistry>,
    ) -> Arc<Self> {
        let config = load_config(&store);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("网关 tokio runtime 创建失败");
        let inner = Arc::new(GatewayInner {
            model,
            store,
            tools,
            runtime,
            enabled: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            port: Mutex::new(config.port),
            token: Mutex::new(config.token),
        });
        let state = Arc::new(Self { inner });
        if config.enabled {
            match state.start() {
                Ok(port) => println!("[网关] 随启动开启: http://127.0.0.1:{port}"),
                Err(e) => eprintln!("[网关] 启动失败: {e}"),
            }
        }
        state
    }

    pub fn enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::SeqCst)
    }

    pub fn port(&self) -> u16 {
        *self.inner.port.lock().unwrap()
    }

    pub fn set_token(&self, token: &str) {
        *self.inner.token.lock().unwrap() = token.to_string();
    }

    pub fn set_port(&self, port: u16) {
        *self.inner.port.lock().unwrap() = port;
    }

    /// 启动监听（幂等：已启用则直接返回当前端口）
    pub fn start(&self) -> Result<u16, String> {
        if self.enabled() {
            return Ok(self.port());
        }
        let port = self.port();
        let listener = TcpListener::bind(("127.0.0.1", port))
            .map_err(|e| format!("绑定 127.0.0.1:{port} 失败: {e}"))?;
        let actual_port = listener.local_addr().map_err(|e| e.to_string())?.port();
        *self.inner.port.lock().unwrap() = actual_port;
        self.inner.stop.store(false, Ordering::SeqCst);
        self.inner.enabled.store(true, Ordering::SeqCst);

        let inner = self.inner.clone();
        std::thread::Builder::new()
            .name("baize-gateway".into())
            .spawn(move || serve(listener, inner))
            .map_err(|e| e.to_string())?;
        Ok(actual_port)
    }

    /// 停止监听（服务线程在 ~30ms 内退出并释放端口）
    pub fn stop(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.enabled.store(false, Ordering::SeqCst);
    }

    pub fn status(&self) -> Value {
        let token = self.inner.token.lock().unwrap();
        json!({
            "enabled": self.enabled(),
            "port": self.port(),
            "has_token": !token.is_empty(),
            "base_url": format!("http://127.0.0.1:{}", self.port()),
            "endpoints": {
                "health": "/api/health",
                "models": "/v1/models",
                "chat_completions": "/v1/chat/completions",
                "memory_remember": "/api/memory/remember",
                "memory_search": "/api/memory/search",
                "tools": "/api/tools",
                "tools_execute": "/api/tools/execute"
            }
        })
    }
}

fn load_config(store: &MemoryStore) -> GatewayConfig {
    store
        .get_setting(GATEWAY_CONFIG_KEY)
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str::<GatewayConfig>(&j).ok())
        .unwrap_or_default()
}

fn persist_config(store: &MemoryStore, config: &GatewayConfig) -> Result<(), String> {
    let json = serde_json::to_string(config).map_err(|e| e.to_string())?;
    store.set_setting(GATEWAY_CONFIG_KEY, &json)
}

// ---------------- HTTP 服务核心 ----------------

fn serve(listener: TcpListener, inner: Arc<GatewayInner>) {
    listener.set_nonblocking(true).ok();
    loop {
        if inner.stop.load(Ordering::SeqCst) {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let inner = inner.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle_connection(stream, &inner) {
                        eprintln!("[网关] 请求处理失败: {e}");
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(e) => {
                eprintln!("[网关] accept 失败: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    println!("[网关] 服务线程已退出");
}

fn handle_connection(mut stream: TcpStream, inner: &Arc<GatewayInner>) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .map_err(|e| e.to_string())?;

    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);

    // 请求行
    let mut req_line = String::new();
    if reader.read_line(&mut req_line).map_err(|e| e.to_string())? == 0 {
        return Ok(());
    }
    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    if method.is_empty() {
        return Ok(());
    }

    // 头
    let mut content_length = 0usize;
    let mut auth: Option<String> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| e.to_string())?;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => content_length = v.trim().parse::<usize>().unwrap_or(0),
                "authorization" => auth = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    if content_length > MAX_BODY_LEN {
        return write_json(&mut stream, 413, &json!({ "error": "请求体过大" }));
    }

    // 请求体
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    }
    let body = String::from_utf8_lossy(&body).to_string();

    // 鉴权（仅在有令牌时校验所有端点）
    if !authorized(inner, auth.as_deref()) {
        return write_json(&mut stream, 401, &json!({ "error": "unauthorized：网关令牌无效" }));
    }

    let (m, p) = (method.as_str(), path.as_str());
    let result = match (m, p) {
        ("GET", "/api/health") => write_json(&mut stream, 200, &json!({
            "ok": true,
            "name": "baize",
            "version": env!("CARGO_PKG_VERSION"),
            "object": "local-ai-gateway"
        })),
        ("GET", "/v1/models") => inner.route_models(&mut stream),
        ("POST", "/v1/chat/completions") => inner.route_chat(&mut stream, &body),
        ("POST", "/api/memory/remember") => inner.route_remember(&mut stream, &body),
        ("POST", "/api/memory/search") => inner.route_search(&mut stream, &body),
        ("GET", "/api/tools") => inner.route_tools(&mut stream),
        ("POST", "/api/tools/execute") => inner.route_execute(&mut stream, &body),
        _ => write_json(&mut stream, 404, &json!({ "error": "not found" })),
    };

    // 兜底：端点处理失败时返回明确 JSON 错误，而不是断开连接让客户端拿到 empty reply
    if let Err(e) = result {
        let _ = write_json(&mut stream, 400, &json!({ "error": e }));
    }
    Ok(())
}

fn authorized(inner: &GatewayInner, auth: Option<&str>) -> bool {
    let token = inner.token.lock().unwrap();
    if token.is_empty() {
        return true;
    }
    match auth {
        Some(a) => a.trim_start_matches("Bearer ").trim() == token.as_str(),
        None => false,
    }
}

// ---------------- 端点实现 ----------------

impl GatewayInner {
    /// OpenAI 兼容模型列表（实时读取当前模型配置）
    fn route_models(&self, stream: &mut TcpStream) -> Result<(), String> {
        let config = crate::load_model_config(&self.store);
        let data: Vec<Value> = config
            .effective_profiles()
            .into_iter()
            .filter(|p| p.enabled)
            .map(|p| {
                json!({
                    "id": p.id,
                    "object": "model",
                    "created": 0,
                    "owned_by": "baize",
                    "name": p.name,
                    "model": p.model,
                    "tier": p.tier
                })
            })
            .collect();
        write_json(stream, 200, &json!({ "object": "list", "data": data }))
    }

    fn route_chat(&self, stream: &mut TcpStream, body: &str) -> Result<(), String> {
        let req: ChatRequest =
            serde_json::from_str(body).map_err(|e| format!("请求解析失败: {e}"))?;
        let messages = req.to_chat_messages();
        let tools = req.tools.clone().unwrap_or_default();
        let model_name = req.model.clone().unwrap_or_else(|| "baize".to_string());
        let created = unix_now();
        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());

        if req.stream.unwrap_or(false) {
            self.stream_completion(stream, &messages, &tools, &id, &model_name, created)
        } else {
            let res = self
                .runtime
                .block_on(async { self.model.chat(&messages, &tools).await })?;
            let (content, tool_calls) = match res {
                ChatResponse { content, tool_calls } => (content, tool_calls),
            };
            let finish_reason = if tool_calls.is_some() {
                "tool_calls"
            } else {
                "stop"
            };
            let mut message = json!({ "role": "assistant" });
            if let Some(c) = &content {
                message["content"] = json!(c);
            }
            if let Some(tc) = &tool_calls {
                message["tool_calls"] = json!(tc);
            }
            let resp = json!({
                "id": id,
                "object": "chat.completion",
                "created": created,
                "model": model_name,
                "choices": [{
                    "index": 0,
                    "message": message,
                    "finish_reason": finish_reason
                }],
                "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }
            });
            write_json(stream, 200, &resp)
        }
    }

    /// SSE 流式：首块带 role → token 增量 → 末块（tool_calls / stop）→ [DONE]
    fn stream_completion(
        &self,
        stream: &mut TcpStream,
        messages: &[ChatMessage],
        tools: &[Value],
        id: &str,
        model_name: &str,
        created: i64,
    ) -> Result<(), String> {
        let writer = Arc::new(Mutex::new(stream));
        {
            let mut w = writer.lock().unwrap();
            w.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
            )
            .map_err(|e| e.to_string())?;
            w.flush().map_err(|e| e.to_string())?;
        }
        // 首块（role）
        write_sse(
            &mut *writer.lock().unwrap(),
            &json!({
                "id": id, "object": "chat.completion.chunk", "created": created, "model": model_name,
                "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}]
            }),
        )?;

        let writer2 = writer.clone();
        let cbid = id.to_string();
        let cbmodel = model_name.to_string();
        let callback = move |tok: &str| {
            let chunk = json!({
                "id": cbid,
                "object": "chat.completion.chunk",
                "created": created,
                "model": cbmodel,
                "choices": [{"index": 0, "delta": {"content": tok}, "finish_reason": null}]
            });
            let mut w = writer2.lock().unwrap();
            let _ = write_sse(&mut *w, &chunk);
        };

        let res = self
            .runtime
            .block_on(async move { self.model.stream_chat(messages, tools, &callback).await });

        match res {
            Ok(ChatResponse { tool_calls, .. }) => {
                if let Some(tc) = tool_calls {
                    write_sse(
                        &mut *writer.lock().unwrap(),
                        &json!({
                            "id": id, "object": "chat.completion.chunk", "created": created, "model": model_name,
                            "choices": [{"index": 0, "delta": {"tool_calls": tc}, "finish_reason": "tool_calls"}]
                        }),
                    )?;
                } else {
                    write_sse(
                        &mut *writer.lock().unwrap(),
                        &json!({
                            "id": id, "object": "chat.completion.chunk", "created": created, "model": model_name,
                            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]
                        }),
                    )?;
                }
            }
            Err(e) => {
                write_sse(
                    &mut *writer.lock().unwrap(),
                    &json!({ "id": id, "object": "chat.completion.chunk", "error": e }),
                )?;
            }
        }
        writer
            .lock()
            .unwrap()
            .write_all(b"data: [DONE]\n\n")
            .map_err(|e| e.to_string())?;
        let flushed = writer.lock().unwrap().flush().map_err(|e| e.to_string());
        flushed
    }

    fn route_remember(&self, stream: &mut TcpStream, body: &str) -> Result<(), String> {
        let v: Value = serde_json::from_str(body).map_err(|e| format!("请求解析失败: {e}"))?;
        let text = v
            .get("text")
            .and_then(|t| t.as_str())
            .ok_or("缺少 text 字段")?;
        let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("fact");
        let outcome = self.store.smart_remember(text, kind)?;
        let label = match outcome {
            crate::memory::RememberOutcome::Created => "Created",
            crate::memory::RememberOutcome::Reinforced => "Reinforced",
            crate::memory::RememberOutcome::Filtered => "Filtered",
        };
        write_json(stream, 200, &json!({ "outcome": label }))
    }

    fn route_search(&self, stream: &mut TcpStream, body: &str) -> Result<(), String> {
        let v: Value = serde_json::from_str(body).map_err(|e| format!("请求解析失败: {e}"))?;
        let query = v
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or("缺少 query 字段")?;
        let top_k = v.get("top_k").and_then(|n| n.as_u64()).unwrap_or(5) as usize;
        let rows = self.store.recall(query, top_k)?;
        let results: Vec<Value> = rows
            .iter()
            .map(|m| {
                json!({
                    "id": m.mem_id,
                    "content": m.content,
                    "kind": m.kind,
                    "salience": m.salience,
                    "last_access": m.last_access
                })
            })
            .collect();
        write_json(stream, 200, &json!({ "results": results }))
    }

    fn route_tools(&self, stream: &mut TcpStream) -> Result<(), String> {
        let list: Vec<Value> = self
            .tools
            .names()
            .into_iter()
            .filter_map(|name| {
                let tool = self.tools.get(&name)?;
                Some(json!({
                    "name": name,
                    "description": tool.description(),
                    "permission": permission_str(tool.permission()),
                    "parameters": tool.schema()
                }))
            })
            .collect();
        write_json(stream, 200, &json!({ "tools": list }))
    }

    fn route_execute(&self, stream: &mut TcpStream, body: &str) -> Result<(), String> {
        let v: Value = serde_json::from_str(body).map_err(|e| format!("请求解析失败: {e}"))?;
        let name = v
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or("缺少 name 字段")?
            .to_string();
        let args = v.get("args").cloned().unwrap_or(Value::Null);
        let tool = self
            .tools
            .get(&name)
            .ok_or_else(|| format!("工具不存在: {name}"))?;
        let perm = tool.permission();
        if perm != PermissionClass::ReadOnly {
            return write_json(
                stream,
                403,
                &json!({
                    "error": "网关 v1 仅执行只读工具；该工具需人工审批，请在白泽界面或 IM 中批准后执行",
                    "permission": permission_str(perm)
                }),
            );
        }
        match tool.run(args) {
            Ok(result) => write_json(stream, 200, &json!({ "result": result })),
            Err(e) => write_json(stream, 500, &json!({ "error": e })),
        }
    }
}

// ---------------- 请求模型 ----------------

#[derive(Debug, serde::Deserialize)]
struct ChatRequest {
    model: Option<String>,
    messages: Vec<Value>,
    #[serde(default)]
    tools: Option<Vec<Value>>,
    #[serde(default)]
    stream: Option<bool>,
}

impl ChatRequest {
    fn to_chat_messages(&self) -> Vec<ChatMessage> {
        self.messages
            .iter()
            .map(|m| ChatMessage {
                role: m
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("user")
                    .to_string(),
                content: m.get("content").map(content_to_string).unwrap_or_default(),
                tool_calls: m.get("tool_calls").and_then(|tc| tc.as_array()).cloned(),
                tool_call_id: m
                    .get("tool_call_id")
                    .and_then(|id| id.as_str())
                    .map(|s| s.to_string()),
            })
            .collect()
    }
}

/// 兼容 OpenAI 的 content：字符串直接取，数组拼接各 text 片段（多模态/富文本降级）
fn content_to_string(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn permission_str(p: PermissionClass) -> &'static str {
    match p {
        PermissionClass::ReadOnly => "read-only",
        PermissionClass::Write => "write",
        PermissionClass::HighRisk => "high-risk",
    }
}

// ---------------- 响应写回 ----------------

fn write_json(out: &mut dyn Write, status: u16, body: &Value) -> Result<(), String> {
    let data = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    };
    write!(
        out,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        data.len()
    )
    .map_err(|e| e.to_string())?;
    out.write_all(&data).map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

fn write_sse(out: &mut dyn Write, obj: &Value) -> Result<(), String> {
    let data = serde_json::to_string(obj).map_err(|e| e.to_string())?;
    write!(out, "data: {data}\n\n").map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------- tauri 命令 ----------------

#[tauri::command]
pub fn gateway_get_status(state: tauri::State<'_, crate::AppState>) -> Value {
    state.gateway.status()
}

#[tauri::command]
pub fn gateway_start(state: tauri::State<'_, crate::AppState>) -> Result<Value, String> {
    let port = state.gateway.start()?;
    let config = GatewayConfig {
        enabled: true,
        port,
        token: state.gateway.inner.token.lock().unwrap().clone(),
    };
    persist_config(&state.store, &config)?;
    Ok(state.gateway.status())
}

#[tauri::command]
pub fn gateway_stop(state: tauri::State<'_, crate::AppState>) -> Result<Value, String> {
    state.gateway.stop();
    let config = GatewayConfig {
        enabled: false,
        port: state.gateway.port(),
        token: state.gateway.inner.token.lock().unwrap().clone(),
    };
    persist_config(&state.store, &config)?;
    Ok(state.gateway.status())
}

#[tauri::command]
pub fn gateway_set_config(
    state: tauri::State<'_, crate::AppState>,
    config: GatewayConfig,
) -> Result<Value, String> {
    state.gateway.set_token(&config.token);
    state.gateway.set_port(config.port);
    persist_config(&state.store, &config)?;

    if config.enabled {
        // 重启以应用新端口/令牌
        if state.gateway.enabled() {
            state.gateway.stop();
        }
        state.gateway.start()?;
    } else {
        state.gateway.stop();
    }
    Ok(state.gateway.status())
}