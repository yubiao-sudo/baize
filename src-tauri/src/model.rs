//! 模型层：本地/云端统一的 ModelProvider 抽象 + 可运行时切换的模型路由
//!
//! 设计（见设计文档 4.6 本地优先路由）：
//! - 本地 Ollama 优先（隐私、免费、低延迟）
//! - 云端 OpenAI 兼容 API 兜底（DeepSeek/OpenAI/Qwen/Moonshot 等）
//! - 配置可运行时修改并持久化，失败自动切换
//! - 支持流式（SSE）输出：边生成边回调 token，替换完整返回后模拟逐字

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use futures_util::future::join_all;
use futures_util::StreamExt;
use serde_json::{json, Value};

/// 兼容 OpenAI 的对话消息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<Value>>,
}

/// 「对话分支」单模型应答（同题对比多模型时返回）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelAnswer {
    /// 提供方显示名（如 "Ollama"、"DeepSeek"）
    pub name: String,
    /// 底层模型标识（如 "qwen2.5:7b"、"deepseek-chat"）
    pub model: String,
    /// "local" | "cloud"
    pub tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ModelTier {
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "cloud")]
    Cloud,
}

/// 厂商协议类型：决定 `to_provider()` 分派到哪个 Provider 实现。
/// `tier`（Local/Cloud）仍用于降级优先级与「是否需要 API Key」判断；`kind` 决定具体 HTTP 协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProviderKind {
    #[serde(rename = "ollama")]
    Ollama,
    #[serde(rename = "openai")]
    OpenAI,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "gemini")]
    Gemini,
}

impl Default for ProviderKind {
    /// 默认 OpenAI 兼容：绝大多数厂商（DeepSeek/豆包/通义/Kimi/GLM/OpenRouter 等）都走此协议，
    /// 旧数据不加该字段时也能平滑迁移。
    fn default() -> Self {
        Self::OpenAI
    }
}

/// 模型提供方统一抽象
#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn name(&self) -> &str;
    /// 底层模型标识（用于「对话分支」对比展示）
    fn model(&self) -> &str;
    fn tier(&self) -> ModelTier;
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<ChatResponse, String>;
    /// 流式对话：边生成边通过 on_token 回调输出 content 片段，返回累积的完整结果
    async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        on_token: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> Result<ChatResponse, String>;

    /// 可取消的流式对话：`cancel` 置位后尽快中断读取，返回已累积的部分结果
    async fn stream_chat_ctl(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        on_token: &(dyn for<'a> Fn(&'a str) + Send + Sync),
        cancel: &AtomicBool,
    ) -> Result<ChatResponse, String> {
        let _ = cancel;
        self.stream_chat(messages, tools, on_token).await
    }
}

/// 单个已保存的模型配置（可注册多个厂商/provider，支持运行时切换）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelProfile {
    /// 唯一标识（如 "local-ollama"、"doubao-pro"）
    pub id: String,
    /// 显示名（如 "白泽本地"、"豆包"）
    pub name: String,
    pub tier: ModelTier,
    /// 厂商协议类型（决定 HTTP 协议）；旧数据缺省按 OpenAI 兼容迁移
    #[serde(default)]
    pub kind: ProviderKind,
    pub base_url: String,
    /// 本地模型可留空；云端模型在持久化时经 vault 加密，内存中才是明文
    #[serde(default)]
    pub api_key: String,
    pub model: String,
    /// 该 profile 专属的视觉模型（可选；不填则复用主 LLM 云连接）
    #[serde(default)]
    pub vision_model: Option<String>,
    /// 该 profile 专属的 embedding 模型（可选；不填则回退到运行时默认）
    #[serde(default)]
    pub embedding_model: Option<String>,
    pub enabled: bool,
    /// 是否已在 vault 保存 API Key（用于前端脱敏展示「已保存」；不入库敏感值）
    #[serde(default)]
    pub has_key: bool,
    /// 该模型本身支持图片输入（多模态）；勾选后视觉调用直接复用该主模型，不再走独立视觉模型
    #[serde(default)]
    pub multimodal: bool,
}

impl ModelProfile {
    /// 依据 profile 构建对应 provider；云端未填 API Key 时返回 None
    pub fn to_provider(&self) -> Option<Arc<dyn ModelProvider>> {
        match self.tier {
            ModelTier::Local => Some(Arc::new(OllamaProvider::new(
                self.base_url.clone(),
                self.model.clone(),
            ))),
            ModelTier::Cloud => {
                if self.api_key.trim().is_empty() {
                    return None;
                }
                let key = self.api_key.trim();
                match self.kind {
                    ProviderKind::Anthropic => Some(Arc::new(AnthropicProvider::new(
                        &self.name,
                        &self.base_url,
                        key,
                        &self.model,
                    ))),
                    ProviderKind::Gemini => Some(Arc::new(GeminiProvider::new(
                        &self.name,
                        &self.base_url,
                        key,
                        &self.model,
                    ))),
                    // OpenAI 兼容（DeepSeek/豆包/通义/Kimi/GLM/OpenRouter 等）
                    _ => Some(Arc::new(CloudProvider::new(
                        &self.name,
                        &self.base_url,
                        key,
                        &self.model,
                    ))),
                }
            }
        }
    }
}

/// 模型配置（前端可编辑、可持久化）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelConfig {
    pub local_enabled: bool,
    pub local_url: String,
    pub local_model: String,
    pub cloud_enabled: bool,
    pub cloud_name: String,
    pub cloud_base_url: String,
    pub cloud_api_key: String,
    pub cloud_model: String,
    /// "local" = 本地优先；"cloud" = 云端优先
    pub priority: String,
    /// 多模型配置列表（新形态）；为空时回退到上面的单本地/单云端字段做平滑迁移
    #[serde(default)]
    pub profiles: Vec<ModelProfile>,
    /// 当前激活模型 id（全局生效；为空时按 priority 取第一个可用）
    #[serde(default)]
    pub active: String,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            local_enabled: true,
            local_url: "http://127.0.0.1:11434".to_string(),
            local_model: "qwen2.5:7b".to_string(),
            cloud_enabled: false,
            cloud_name: "DeepSeek".to_string(),
            cloud_base_url: "https://api.deepseek.com/v1".to_string(),
            cloud_api_key: String::new(),
            cloud_model: "deepseek-chat".to_string(),
            priority: "local".to_string(),
            profiles: Vec::new(),
            active: String::new(),
        }
    }
}

impl ModelConfig {
    /// 从环境变量构建初始配置（作为默认值；持久化配置会覆盖它）
    pub fn from_env() -> Self {
        let mut c = Self::default();
        c.local_url = std::env::var("BAIZE_OLLAMA_URL").unwrap_or(c.local_url);
        c.local_model = std::env::var("BAIZE_MODEL").unwrap_or(c.local_model);
        if let Ok(key) = std::env::var("BAIZE_CLOUD_API_KEY") {
            if !key.is_empty() {
                c.cloud_enabled = true;
                c.cloud_api_key = key;
                c.cloud_name = std::env::var("BAIZE_CLOUD_NAME").unwrap_or(c.cloud_name);
                c.cloud_base_url = std::env::var("BAIZE_CLOUD_BASE_URL").unwrap_or(c.cloud_base_url);
                c.cloud_model = std::env::var("BAIZE_CLOUD_MODEL").unwrap_or(c.cloud_model);
            }
        }
        c.priority = std::env::var("BAIZE_MODEL_PRIORITY").unwrap_or(c.priority);
        c
    }

    /// 统一返回「当前有效的模型列表」：
    /// - 若已填 `profiles`（新形态）则直接用；
    /// - 否则由旧的单本地/单云端字段合成，实现平滑迁移，旧配置无需重填。
    pub fn effective_profiles(&self) -> Vec<ModelProfile> {
        if !self.profiles.is_empty() {
            return self.profiles.clone();
        }
        let mut v: Vec<ModelProfile> = Vec::new();
        if self.local_enabled {
            v.push(ModelProfile {
                id: "local".to_string(),
                name: "本地 Ollama".to_string(),
                tier: ModelTier::Local,
                kind: ProviderKind::Ollama,
                base_url: self.local_url.clone(),
                api_key: String::new(),
                model: self.local_model.clone(),
                vision_model: None,
                embedding_model: None,
                enabled: true,
                has_key: false,
                multimodal: false,
            });
        }
        if self.cloud_enabled && !self.cloud_api_key.trim().is_empty() {
            v.push(ModelProfile {
                id: "cloud".to_string(),
                name: self.cloud_name.clone(),
                tier: ModelTier::Cloud,
                kind: ProviderKind::OpenAI,
                base_url: self.cloud_base_url.clone(),
                api_key: self.cloud_api_key.trim().to_string(),
                model: self.cloud_model.clone(),
                vision_model: None,
                embedding_model: None,
                enabled: true,
                has_key: true,
                multimodal: false,
            });
        }
        v
    }

    /// 当前激活模型 id：优先取 `active`（若存在且启用），否则按 priority 顺序取第一个启用项
    pub fn active_id(&self) -> String {
        let profiles = self.effective_profiles();
        if !self.active.is_empty() && profiles.iter().any(|p| p.id == self.active && p.enabled) {
            return self.active.clone();
        }
        let cloud_first = self.priority == "cloud";
        let pick = |tier: ModelTier| -> Option<&ModelProfile> {
            profiles.iter().find(|p| p.enabled && p.tier == tier)
        };
        let first = if cloud_first {
            pick(ModelTier::Cloud).or_else(|| pick(ModelTier::Local))
        } else {
            pick(ModelTier::Local).or_else(|| pick(ModelTier::Cloud))
        };
        first
            .map(|p| p.id.clone())
            .or_else(|| profiles.iter().find(|p| p.enabled).map(|p| p.id.clone()))
            .unwrap_or_default()
    }

    /// 依据配置构建提供方链：激活模型排首位，其余按配置顺序作为失败降级链
    pub fn build_providers(&self) -> Vec<Arc<dyn ModelProvider>> {
        let mut profiles = self.effective_profiles();
        let active = self.active_id();
        // 稳定排序：激活项置顶，其余保持原顺序（作为自动降级链）
        profiles.sort_by_key(|p| if p.id == active { 0u8 } else { 1u8 });

        let mut v: Vec<Arc<dyn ModelProvider>> = Vec::new();
        for p in profiles {
            if p.enabled {
                if let Some(prov) = p.to_provider() {
                    v.push(prov);
                }
            }
        }
        v
    }

    /// 描述当前生效的链路（供展示）
    pub fn chain_label(&self) -> String {
        let providers = self.build_providers();
        let names: Vec<String> = providers
            .iter()
            .map(|p| match p.tier() {
                ModelTier::Local => "本地 Ollama".to_string(),
                ModelTier::Cloud => p.name().to_string(),
            })
            .collect();
        names.join(" → ")
    }

    /// 供视觉模型复用的云端连接：取第一个已配置 API Key 的云端 profile 的 (base_url, api_key)。
    pub fn cloud_conn(&self) -> Option<(String, String)> {
        self.effective_profiles()
            .into_iter()
            .find(|p| p.tier == ModelTier::Cloud && !p.api_key.trim().is_empty())
            .map(|p| (p.base_url.clone(), p.api_key.clone()))
    }

    /// 视觉模型连接（精确匹配优先）：
    /// 1. 激活模型是云端且有 key → 用其 base_url/api_key/vision_model；
    /// 2. 否则找第一个「带 vision_model」的云端 profile；
    /// 3. 退回第一个有 key 的云端 profile（复用主 LLM 连接，视觉模型名留空走运行时默认）。
    pub fn vision_conn(&self) -> Option<(String, String, String)> {
        let profiles = self.effective_profiles();
        let active = self.active_id();
        if let Some(p) = profiles.iter().find(|p| {
            p.id == active
                && p.tier == ModelTier::Cloud
                && !p.api_key.trim().is_empty()
        }) {
            return Some((
                p.base_url.clone(),
                p.api_key.clone(),
                p.vision_model.clone().unwrap_or_default(),
            ));
        }
        if let Some(p) = profiles.iter().find(|p| {
            p.tier == ModelTier::Cloud
                && !p.api_key.trim().is_empty()
                && p.vision_model.as_deref().map(|s| !s.trim().is_empty()) == Some(true)
        }) {
            return Some((
                p.base_url.clone(),
                p.api_key.clone(),
                p.vision_model.clone().unwrap_or_default(),
            ));
        }
        self.cloud_conn()
            .map(|(b, k)| (b, k, String::new()))
    }

    /// 激活模型对应的 embedding 模型名（仅本地 Ollama 有效时返回，用于覆盖运行时默认；
    /// 云端 embedding 尚未接入，返回 None 以回退到默认 Ollama embedding）。
    pub fn embedding_model(&self) -> Option<String> {
        let profiles = self.effective_profiles();
        let active = self.active_id();
        profiles
            .iter()
            .find(|p| p.id == active && p.tier == ModelTier::Local)
            .and_then(|p| p.embedding_model.clone())
            .filter(|m| !m.trim().is_empty())
    }

    /// 「多模态主模型」连接：当激活模型勾选了 multimodal 时返回其 (协议, base_url, api_key, model)。
    /// 用主模型直接做视觉推理，避免再单独调用独立的视觉模型。云端模型无 API Key 时不返回。
    pub fn multimodal_conn(&self) -> Option<(ProviderKind, String, String, String)> {
        let profiles = self.effective_profiles();
        let active = self.active_id();
        let p = profiles
            .iter()
            .find(|p| p.id == active && p.enabled && p.multimodal)?;
        if p.tier == ModelTier::Cloud && p.api_key.trim().is_empty() {
            return None;
        }
        Some((
            p.kind,
            p.base_url.clone(),
            p.api_key.trim().to_string(),
            p.model.clone(),
        ))
    }
}

/// 模型路由：按序尝试，失败自动切换；配置可运行时重建
pub struct ModelRouter {
    providers: RwLock<Vec<Arc<dyn ModelProvider>>>,
    last: Mutex<String>,
    /// 本轮首个失败提供方及原因（降级时设置，供前端执行流透出），每次调用开始清空
    fallback_note: Mutex<Option<String>>,
}

impl ModelRouter {
    pub fn new(providers: Vec<Arc<dyn ModelProvider>>) -> Self {
        Self {
            providers: RwLock::new(providers),
            last: Mutex::new("未使用".to_string()),
            fallback_note: Mutex::new(None),
        }
    }

    /// 运行时重建提供方链（配置变更后调用）
    pub fn rebuild(&self, config: &ModelConfig) {
        *self.providers.write().unwrap() = config.build_providers();
    }

    pub fn provider_names(&self) -> Vec<String> {
        self.providers
            .read()
            .unwrap()
            .iter()
            .map(|p| p.name().to_string())
            .collect()
    }

    pub fn last_used(&self) -> String {
        self.last.lock().unwrap().clone()
    }

    /// 取走本轮降级备注（首个失败提供方 + 原因），无则 None。每次调用开始时清空。
    pub fn take_fallback_note(&self) -> Option<String> {
        self.fallback_note.lock().unwrap().take()
    }

    /// 记录降级备注（只记第一次失败——通常是激活模型，其失败原因最值得关注）
    fn note_fallback(&self, provider: &str, err: &str) {
        // 控制台同步留痕，便于后端日志排查
        eprintln!("[模型降级] {provider} 调用失败（{err}），尝试下一个提供方");
        let brief: String = if err.chars().count() > 160 {
            let s: String = err.chars().take(160).collect();
            format!("{s}…")
        } else {
            err.to_string()
        };
        let mut note = self.fallback_note.lock().unwrap();
        if note.is_none() {
            *note = Some(format!("{provider} 调用失败：{brief}"));
        }
    }

    fn record(&self, p: &dyn ModelProvider) {
        let tier = match p.tier() {
            ModelTier::Local => "本地",
            ModelTier::Cloud => "云端",
        };
        *self.last.lock().unwrap() = format!("{}（{}）", p.name(), tier);
    }

    /// 只用指定 tier 的提供方（用于「强模型规划、快模型执行」分工）；无该 tier 则回退到全部
    pub async fn chat_with_tier(
        &self,
        tier: ModelTier,
        messages: &[ChatMessage],
        tools: &[Value],
    ) -> Result<ChatResponse, String> {
        let providers: Vec<Arc<dyn ModelProvider>> = self.providers.read().unwrap().clone();
        let mut candidates: Vec<Arc<dyn ModelProvider>> = providers
            .iter()
            .filter(|p| p.tier() == tier)
            .cloned()
            .collect();
        if candidates.is_empty() {
            candidates = providers;
        }
        self.fallback_note.lock().unwrap().take(); // 清空上轮备注
        let mut errors = Vec::new();
        for p in candidates {
            match p.chat(messages, tools).await {
                Ok(resp) => {
                    self.record(p.as_ref());
                    return Ok(resp);
                }
                Err(e) => {
                    self.note_fallback(p.name(), &e);
                    errors.push(format!("[{}] {e}", p.name()));
                }
            }
        }
        Err(format!("模型调用失败：{}", errors.join("；")))
    }

    /// 只用指定 tier 的提供方做流式对话；无该 tier 则回退到全部（供管线阶段边生成边广播进度）
    pub async fn stream_chat_with_tier(
        &self,
        tier: ModelTier,
        messages: &[ChatMessage],
        tools: &[Value],
        on_token: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> Result<ChatResponse, String> {
        let providers: Vec<Arc<dyn ModelProvider>> = self.providers.read().unwrap().clone();
        let mut candidates: Vec<Arc<dyn ModelProvider>> = providers
            .iter()
            .filter(|p| p.tier() == tier)
            .cloned()
            .collect();
        if candidates.is_empty() {
            candidates = providers;
        }
        self.fallback_note.lock().unwrap().take(); // 清空上轮备注
        let mut errors = Vec::new();
        for p in candidates {
            match p.stream_chat(messages, tools, on_token).await {
                Ok(resp) => {
                    self.record(p.as_ref());
                    return Ok(resp);
                }
                Err(e) => {
                    self.note_fallback(p.name(), &e);
                    errors.push(format!("[{}] {e}", p.name()));
                }
            }
        }
        Err(format!("模型调用失败：{}", errors.join("；")))
    }

    pub async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<ChatResponse, String> {
        let providers: Vec<Arc<dyn ModelProvider>> = self.providers.read().unwrap().clone();
        self.fallback_note.lock().unwrap().take(); // 清空上轮备注
        let mut errors = Vec::new();
        for p in providers {
            match p.chat(messages, tools).await {
                Ok(resp) => {
                    self.record(p.as_ref());
                    return Ok(resp);
                }
                Err(e) => {
                    self.note_fallback(p.name(), &e);
                    errors.push(format!("[{}] {e}", p.name()));
                }
            }
        }
        Err(format!("所有模型提供方均失败：{}", errors.join("；")))
    }

    pub async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        on_token: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> Result<ChatResponse, String> {
        let providers: Vec<Arc<dyn ModelProvider>> = self.providers.read().unwrap().clone();
        self.fallback_note.lock().unwrap().take(); // 清空上轮备注
        let mut errors = Vec::new();
        for p in providers {
            match p.stream_chat(messages, tools, on_token).await {
                Ok(resp) => {
                    self.record(p.as_ref());
                    return Ok(resp);
                }
                Err(e) => {
                    self.note_fallback(p.name(), &e);
                    errors.push(format!("[{}] {e}", p.name()));
                }
            }
        }
        Err(format!("所有模型提供方均失败：{}", errors.join("；")))
    }

    /// 「对话分支」：同一问题并行对比所有可用模型，各自返回独立结果（互不失败透传）
    pub async fn compare(&self, messages: &[ChatMessage], tools: &[Value]) -> Vec<ModelAnswer> {
        let providers: Vec<Arc<dyn ModelProvider>> = self.providers.read().unwrap().clone();
        let futures = providers.into_iter().map(|p| {
            let msgs = messages.to_vec();
            let tools = tools.to_vec();
            let name = p.name().to_string();
            let model = p.model().to_string();
            let tier = match p.tier() {
                ModelTier::Local => "local",
                ModelTier::Cloud => "cloud",
            }
            .to_string();
            async move {
                let (content, error) = match p.chat(&msgs, &tools).await {
                    Ok(resp) => (resp.content, None),
                    Err(e) => (None, Some(e)),
                };
                ModelAnswer {
                    name,
                    model,
                    tier,
                    content,
                    error,
                }
            }
        });
        join_all(futures).await
    }
}

/// 解析 OpenAI 兼容的 /v1/chat/completions 响应
fn parse_openai_response(text: &str) -> Result<ChatResponse, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("解析响应失败: {e}"))?;
    if let Some(err) = v.get("error") {
        return Err(format!("API 错误: {err}"));
    }
    let msg = &v["choices"][0]["message"];
    let content = msg["content"].as_str().map(|s| s.to_string());
    let tool_calls = msg["tool_calls"].as_array().cloned();
    Ok(ChatResponse { content, tool_calls })
}

/// 消费 OpenAI 兼容的流式（SSE）响应：累积 content + tool_calls，content 片段实时回调
async fn stream_openai_compat(
    resp: reqwest::Response,
    on_token: &(dyn Fn(&str) + Send + Sync),
    cancel: &AtomicBool,
) -> Result<ChatResponse, String> {
    let mut content = String::new();
    let mut tc_map: BTreeMap<usize, Value> = BTreeMap::new();

    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        // 取消打断：置位后立即停止读取，返回已累积的部分内容
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let chunk = chunk.map_err(|e| format!("流式读取失败: {e}"))?;
        buf.extend_from_slice(&chunk);

        // 按 \n\n 字节序列分割完整 SSE 事件，避免跨 chunk 的多字节 UTF-8 字符被截断成乱码
        while let Some(pos) = find_subsequence(&buf, b"\n\n") {
            let event_bytes: Vec<u8> = buf[..pos].to_vec();
            buf.drain(..pos + 2);
            let event = String::from_utf8_lossy(&event_bytes);

            for line in event.lines() {
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if let Some(err) = v.get("error") {
                    return Err(format!("流式 API 错误: {err}"));
                }

                // content 片段
                if let Some(delta) = v["choices"][0]["delta"]["content"].as_str() {
                    if !delta.is_empty() {
                        content.push_str(delta);
                        on_token(delta);
                    }
                }

                // tool_calls 片段（按 index 累积）
                if let Some(tcs) = v["choices"][0]["delta"]["tool_calls"].as_array() {
                    for tc in tcs {
                        let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                        let entry = tc_map.entry(idx).or_insert_with(|| {
                            json!({
                                "id": "",
                                "type": "function",
                                "function": { "name": "", "arguments": "" }
                            })
                        });
                        if let Some(id) = tc["id"].as_str() {
                            entry["id"] = Value::String(id.to_string());
                        }
                        if let Some(name) = tc["function"]["name"].as_str() {
                            if !name.is_empty() {
                                entry["function"]["name"] = Value::String(name.to_string());
                            }
                        }
                        if let Some(arg) = tc["function"]["arguments"].as_str() {
                            let cur = entry["function"]["arguments"].as_str().unwrap_or("");
                            entry["function"]["arguments"] =
                                Value::String(format!("{cur}{arg}"));
                        }
                    }
                }
            }
        }
    }

    let content = if content.is_empty() { None } else { Some(content) };
    let tool_calls = if tc_map.is_empty() {
        None
    } else {
        Some(tc_map.into_values().collect())
    };
    Ok(ChatResponse { content, tool_calls })
}

/// 在字节切片中查找子序列，返回起始位置
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// 本地 Ollama 提供方
pub struct OllamaProvider {
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(base_url: String, model: String) -> Self {
        Self {
            base_url,
            model,
            client: reqwest::Client::builder()
                .build()
                .expect("构建 HTTP 客户端失败"),
        }
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn name(&self) -> &str {
        "Ollama"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn tier(&self) -> ModelTier {
        ModelTier::Local
    }
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<ChatResponse, String> {
        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));
        let body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "stream": false,
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("请求失败（请确认已 ollama serve）: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
        if !status.is_success() {
            return Err(format!("Ollama 错误 {status}: {text}"));
        }
        parse_openai_response(&text)
    }

    async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        on_token: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> Result<ChatResponse, String> {
        let no_cancel = AtomicBool::new(false);
        self.stream_chat_ctl(messages, tools, on_token, &no_cancel)
            .await
    }

    async fn stream_chat_ctl(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        on_token: &(dyn for<'a> Fn(&'a str) + Send + Sync),
        cancel: &AtomicBool,
    ) -> Result<ChatResponse, String> {
        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));
        let body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "stream": true,
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("请求失败（请确认已 ollama serve）: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
            return Err(format!("Ollama 错误 {status}: {text}"));
        }
        stream_openai_compat(resp, on_token, cancel).await
    }
}

/// 云端 OpenAI 兼容提供方
pub struct CloudProvider {
    name: String,
    base_url: String,
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl CloudProvider {
    pub fn new(name: &str, base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            client: reqwest::Client::builder()
                .build()
                .expect("构建 HTTP 客户端失败"),
        }
    }
}

#[async_trait]
impl ModelProvider for CloudProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn tier(&self) -> ModelTier {
        ModelTier::Cloud
    }
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<ChatResponse, String> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "stream": false,
        });
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("云端请求失败: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
        if !status.is_success() {
            return Err(format!("云端错误 {status}: {text}"));
        }
        parse_openai_response(&text)
    }

    async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        on_token: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> Result<ChatResponse, String> {
        let no_cancel = AtomicBool::new(false);
        self.stream_chat_ctl(messages, tools, on_token, &no_cancel)
            .await
    }

    async fn stream_chat_ctl(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        on_token: &(dyn for<'a> Fn(&'a str) + Send + Sync),
        cancel: &AtomicBool,
    ) -> Result<ChatResponse, String> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "stream": true,
        });
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("云端请求失败: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
            return Err(format!("云端错误 {status}: {text}"));
        }
        stream_openai_compat(resp, on_token, cancel).await
    }
}

/// OpenAI 风格 tool 定义 → Anthropic Messages `tools` 数组格式
fn tools_to_anthropic(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|t| {
            let f = t.get("function")?;
            Some(json!({
                "name": f.get("name").cloned().unwrap_or(Value::Null),
                "description": f.get("description").cloned().unwrap_or(Value::Null),
                "input_schema": f.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"})),
            }))
        })
        .collect()
}

/// OpenAI 风格 tool 定义 → Gemini `tools` 数组格式
fn tools_to_gemini(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|t| {
            let f = t.get("function")?;
            Some(json!({
                "name": f.get("name").cloned().unwrap_or(Value::Null),
                "description": f.get("description").cloned().unwrap_or(Value::Null),
                "parameters": f.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"})),
            }))
        })
        .collect()
}

/// 云端 Anthropic (Claude) 提供方：Messages API（x-api-key + anthropic-version）。
pub struct AnthropicProvider {
    name: String,
    base_url: String,
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(name: &str, base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            client: reqwest::Client::builder()
                .build()
                .expect("构建 HTTP 客户端失败"),
        }
    }

    /// 把 chat 消息拆成 Anthropic 的 system + user/assistant messages
    fn split_messages(messages: &[ChatMessage]) -> (String, Vec<Value>) {
        let mut system = String::new();
        let mut msgs: Vec<Value> = Vec::new();
        for m in messages {
            if m.role == "system" {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&m.content);
            } else {
                let role = if m.role == "assistant" { "assistant" } else { "user" };
                msgs.push(json!({ "role": role, "content": m.content }));
            }
        }
        // Anthropic 要求首条消息为 user；自动补齐空 user 首位，避免 400。
        if msgs.first().map(|v| v["role"] == "assistant").unwrap_or(false) {
            msgs.insert(0, json!({ "role": "user", "content": "" }));
        }
        (system, msgs)
    }

    fn parse_response(v: &Value) -> Result<ChatResponse, String> {
        if let Some(err) = v.get("error") {
            return Err(format!("Anthropic API 错误: {err}"));
        }
        let mut content = String::new();
        let mut tool_calls: Vec<Value> = Vec::new();
        if let Some(blocks) = v["content"].as_array() {
            for b in blocks {
                if b["type"] == "text" {
                    if let Some(t) = b["text"].as_str() {
                        content.push_str(t);
                    }
                } else if b["type"] == "tool_use" {
                    tool_calls.push(json!({
                        "id": b["id"].as_str().unwrap_or("").to_string(),
                        "type": "function",
                        "function": {
                            "name": b["name"].as_str().unwrap_or("").to_string(),
                            "arguments": b["input"].to_string()
                        }
                    }));
                }
            }
        }
        Ok(ChatResponse {
            content: if content.is_empty() { None } else { Some(content) },
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        })
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn tier(&self) -> ModelTier {
        ModelTier::Cloud
    }
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<ChatResponse, String> {
        let url = format!("{}/messages", self.base_url);
        let (system, msgs) = Self::split_messages(messages);
        let mut body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "messages": msgs,
        });
        if !system.is_empty() {
            body["system"] = Value::String(system);
        }
        let atools = tools_to_anthropic(tools);
        if !atools.is_empty() {
            body["tools"] = Value::Array(atools);
        }
        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Anthropic 请求失败: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
        if !status.is_success() {
            return Err(format!("Anthropic 错误 {status}: {text}"));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("解析响应失败: {e}"))?;
        Self::parse_response(&v)
    }

    async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        on_token: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> Result<ChatResponse, String> {
        let resp = self.chat(messages, tools).await?;
        if let Some(c) = &resp.content {
            on_token(c);
        }
        Ok(resp)
    }
}

/// 云端 Google Gemini 提供方：generateContent（?key= 查询参数）。
pub struct GeminiProvider {
    name: String,
    base_url: String,
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(name: &str, base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            name: name.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            client: reqwest::Client::builder()
                .build()
                .expect("构建 HTTP 客户端失败"),
        }
    }

    fn split_messages(messages: &[ChatMessage]) -> (String, Vec<Value>) {
        let mut system = String::new();
        let mut contents: Vec<Value> = Vec::new();
        for m in messages {
            if m.role == "system" {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&m.content);
            } else {
                let role = if m.role == "assistant" { "model" } else { "user" };
                contents.push(json!({
                    "role": role,
                    "parts": [{ "text": m.content }]
                }));
            }
        }
        (system, contents)
    }

    fn parse_response(v: &Value) -> Result<ChatResponse, String> {
        if let Some(err) = v.get("error") {
            return Err(format!("Gemini API 错误: {err}"));
        }
        let parts = &v["candidates"][0]["content"]["parts"];
        let mut content = String::new();
        let mut tool_calls: Vec<Value> = Vec::new();
        if let Some(arr) = parts.as_array() {
            for p in arr {
                if let Some(t) = p["text"].as_str() {
                    content.push_str(t);
                }
                if let Some(fc) = p.get("functionCall") {
                    tool_calls.push(json!({
                        "id": format!("call_{}", tool_calls.len()),
                        "type": "function",
                        "function": {
                            "name": fc["name"].as_str().unwrap_or("").to_string(),
                            "arguments": fc.get("args")
                                .map(|a| a.to_string())
                                .unwrap_or_else(|| "{}".to_string())
                        }
                    }));
                }
            }
        }
        Ok(ChatResponse {
            content: if content.is_empty() { None } else { Some(content) },
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        })
    }
}

#[async_trait]
impl ModelProvider for GeminiProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn tier(&self) -> ModelTier {
        ModelTier::Cloud
    }
    async fn chat(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<ChatResponse, String> {
        let url = format!("{}/models/{}:generateContent", self.base_url, self.model);
        let (system, contents) = Self::split_messages(messages);
        let mut body = json!({ "contents": contents });
        if !system.is_empty() {
            body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
        }
        let gtools = tools_to_gemini(tools);
        if !gtools.is_empty() {
            body["tools"] = json!([{ "functionDeclarations": gtools }]);
        }
        let resp = self
            .client
            .post(&url)
            .query(&[("key", &self.api_key)])
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Gemini 请求失败: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| format!("读取响应失败: {e}"))?;
        if !status.is_success() {
            return Err(format!("Gemini 错误 {status}: {text}"));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("解析响应失败: {e}"))?;
        Self::parse_response(&v)
    }

    async fn stream_chat(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        on_token: &(dyn for<'a> Fn(&'a str) + Send + Sync),
    ) -> Result<ChatResponse, String> {
        let resp = self.chat(messages, tools).await?;
        if let Some(c) = &resp.content {
            on_token(c);
        }
        Ok(resp)
    }
}

/// 厂商预设模板：统一描述各厂商接入所需的协议 / 地址 / 推荐模型，供前端下拉选择一键填充。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VendorPreset {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub tier: ModelTier,
    pub base_url: String,
    pub models: Vec<String>,
    pub note: String,
}

/// 内置厂商清单（静态常量；前端通过 get_vendor_presets 读取）
pub fn vendor_presets() -> Vec<VendorPreset> {
    vec![
        VendorPreset {
            id: "ollama".into(),
            name: "本地 Ollama".into(),
            kind: ProviderKind::Ollama,
            tier: ModelTier::Local,
            base_url: "http://127.0.0.1:11434".into(),
            models: vec!["qwen2.5:7b".into(), "llama3.2".into(), "deepseek-r1:7b".into()],
            note: "本地部署，隐私免费，无需 API Key".into(),
        },
        VendorPreset {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            kind: ProviderKind::OpenAI,
            tier: ModelTier::Cloud,
            base_url: "https://api.deepseek.com/v1".into(),
            models: vec!["deepseek-chat".into(), "deepseek-reasoner".into()],
            note: "platform.deepseek.com".into(),
        },
        VendorPreset {
            id: "openai".into(),
            name: "OpenAI".into(),
            kind: ProviderKind::OpenAI,
            tier: ModelTier::Cloud,
            base_url: "https://api.openai.com/v1".into(),
            models: vec!["gpt-4o".into(), "gpt-4o-mini".into(), "o1".into()],
            note: "platform.openai.com".into(),
        },
        VendorPreset {
            id: "doubao".into(),
            name: "豆包（火山方舟）".into(),
            kind: ProviderKind::OpenAI,
            tier: ModelTier::Cloud,
            base_url: "https://ark.cn-beijing.volces.com/api/v3".into(),
            models: vec!["doubao-1.5-pro-32k".into()],
            note: "console.volcengine.com/ark".into(),
        },
        VendorPreset {
            id: "qwen".into(),
            name: "通义千问".into(),
            kind: ProviderKind::OpenAI,
            tier: ModelTier::Cloud,
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            models: vec!["qwen-max".into(), "qwen-plus".into()],
            note: "bailian.console.aliyun.com".into(),
        },
        VendorPreset {
            id: "kimi".into(),
            name: "Kimi（月之暗面）".into(),
            kind: ProviderKind::OpenAI,
            tier: ModelTier::Cloud,
            base_url: "https://api.moonshot.cn/v1".into(),
            models: vec!["moonshot-v1-8k".into(), "moonshot-v1-32k".into()],
            note: "platform.moonshot.cn".into(),
        },
        VendorPreset {
            id: "glm".into(),
            name: "智谱 GLM".into(),
            kind: ProviderKind::OpenAI,
            tier: ModelTier::Cloud,
            base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
            models: vec!["glm-4-plus".into(), "glm-4-flash".into()],
            note: "open.bigmodel.cn".into(),
        },
        VendorPreset {
            id: "openrouter".into(),
            name: "OpenRouter".into(),
            kind: ProviderKind::OpenAI,
            tier: ModelTier::Cloud,
            base_url: "https://openrouter.ai/api/v1".into(),
            models: vec!["openai/gpt-4o".into(), "anthropic/claude-3.5-sonnet".into()],
            note: "openrouter.ai（聚合多厂）".into(),
        },
        VendorPreset {
            id: "anthropic".into(),
            name: "Anthropic（Claude）".into(),
            kind: ProviderKind::Anthropic,
            tier: ModelTier::Cloud,
            base_url: "https://api.anthropic.com".into(),
            models: vec!["claude-3-5-sonnet-latest".into(), "claude-3-haiku-latest".into()],
            note: "console.anthropic.com".into(),
        },
        VendorPreset {
            id: "gemini".into(),
            name: "Google Gemini".into(),
            kind: ProviderKind::Gemini,
            tier: ModelTier::Cloud,
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            models: vec!["gemini-1.5-pro".into(), "gemini-1.5-flash".into()],
            note: "aistudio.google.com".into(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_content() {
        let text = r#"{"choices":[{"message":{"content":"你好"}}]}"#;
        let resp = parse_openai_response(text).unwrap();
        assert_eq!(resp.content.as_deref(), Some("你好"));
        assert!(resp.tool_calls.is_none());
    }

    #[test]
    fn parse_tool_calls() {
        let text = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"1","type":"function","function":{"name":"list_files","arguments":"{\"path\":\"D:\\\\\"}"}}]}}]}"#;
        let resp = parse_openai_response(text).unwrap();
        assert!(resp.tool_calls.is_some());
        let calls = resp.tool_calls.unwrap();
        assert_eq!(calls[0]["function"]["name"], "list_files");
    }

    #[test]
    fn parse_api_error() {
        let text = r#"{"error":{"message":"bad request"}}"#;
        assert!(parse_openai_response(text).is_err());
    }
}
