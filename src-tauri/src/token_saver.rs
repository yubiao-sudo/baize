//! Token 节约机制：长上下文压缩 + 工具结果截断。
//!
//! 设计依据（结合论文/业界实践）：
//! - Local-Splitter 测量研究：本地小模型做「triage + prompt 压缩」可省 45–79% 云端 token；
//! - LLMLingua / 摘要压缩：历史超阈值时把早期消息压缩为摘要，保留最近原文；
//! - 工具结果是 Agent 上下文的头号膨胀源，需对超长输出做「首尾保留」截断。
//!
//! 核心原则：**压缩用本地免费模型**（`ModelTier::Local`），在省云端 token 的同时不额外花钱；
//! 本地不可用时自动回退到默认链路。

use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::model::{ChatMessage, ModelTier};
use crate::AppState;

/// Token 节约配置（前端可编辑、可持久化；字段带默认值避免旧配置反序列化失败）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSaverConfig {
    /// 总开关
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 长对话自动压缩
    #[serde(default = "default_true")]
    pub auto_compress: bool,
    /// 历史总字符数超过该阈值时触发压缩（约等于 token 数量级）
    #[serde(default = "default_compress_threshold")]
    pub compress_threshold_chars: usize,
    /// 压缩时保留最近多少字符的原文（其余摘要化）
    #[serde(default = "default_keep_recent")]
    pub keep_recent_chars: usize,
    /// 单条工具结果最大字符数，超出部分做「首尾保留」截断（0 表示不截断）
    #[serde(default = "default_tool_result_cap")]
    pub max_tool_result_chars: usize,
    /// 压缩是否只使用本地模型（免费）；关闭则用默认链路
    #[serde(default = "default_true")]
    pub local_only_compress: bool,
    /// 精简回复：约束模型输出风格（结论先行、不复述工具结果、不客套），直接省输出 token
    #[serde(default = "default_true")]
    pub concise_reply: bool,
}

fn default_true() -> bool {
    true
}
fn default_compress_threshold() -> usize {
    16_000
}
fn default_keep_recent() -> usize {
    4_000
}
fn default_tool_result_cap() -> usize {
    4_000
}

impl Default for TokenSaverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_compress: true,
            compress_threshold_chars: default_compress_threshold(),
            keep_recent_chars: default_keep_recent(),
            max_tool_result_chars: default_tool_result_cap(),
            local_only_compress: true,
            concise_reply: true,
        }
    }
}

/// 进程内配置缓存（启动时从持久化加载，运行时经 set_config 更新）
static CONFIG: OnceLock<RwLock<TokenSaverConfig>> = OnceLock::new();

pub fn config() -> TokenSaverConfig {
    let lock = CONFIG.get_or_init(|| RwLock::new(TokenSaverConfig::default()));
    lock.read().unwrap().clone()
}

pub fn set_config(c: TokenSaverConfig) {
    let lock = CONFIG.get_or_init(|| RwLock::new(TokenSaverConfig::default()));
    *lock.write().unwrap() = c;
}

/// 截断过长文本：保留头部 2/3 与尾部 1/3，中间插入省略提示。
/// 用于限制工具结果大小，避免失控输出撑爆上下文。
pub fn truncate_text(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars || max_chars == 0 {
        return text.to_string();
    }
    let head_len = max_chars * 2 / 3;
    let tail_len = max_chars - head_len;
    let head: String = text.chars().take(head_len).collect();
    let tail: String = text.chars().skip(count.saturating_sub(tail_len)).collect();
    let omitted = count - head_len - tail_len;
    format!("{head}\n\n……（中间省略 {omitted} 字）……\n\n{tail}")
}

/// 压缩统计（用于前端执行流展示节省量）
#[derive(Debug, Clone)]
pub struct CompressStats {
    pub before_chars: usize,
    pub after_chars: usize,
}

impl CompressStats {
    pub fn saved(&self) -> usize {
        self.before_chars.saturating_sub(self.after_chars)
    }
}

/// 长对话压缩：历史总字符数超阈值时，把早期消息摘要化、保留最近一段原文。
/// 返回压缩后的历史；未触发压缩时返回 `(原历史, None)`。
pub async fn compress_history(
    state: &AppState,
    history: Vec<ChatMessage>,
) -> (Vec<ChatMessage>, Option<CompressStats>) {
    let cfg = config();
    if !cfg.enabled || !cfg.auto_compress {
        return (history, None);
    }

    let before_chars: usize = history.iter().map(|m| m.content.chars().count()).sum();
    if before_chars <= cfg.compress_threshold_chars {
        return (history, None);
    }

    // 从后往前累计，留足 keep_recent_chars 的原文（至少保留最近一条）
    let mut recent: Vec<ChatMessage> = Vec::new();
    let mut acc = 0usize;
    let mut split = history.len();
    for m in history.iter().rev() {
        let c = m.content.chars().count();
        if acc + c > cfg.keep_recent_chars && !recent.is_empty() {
            break;
        }
        acc += c;
        recent.push(m.clone());
        split -= 1;
    }
    recent.reverse();

    let early = &history[..split];
    if early.is_empty() {
        return (history, None);
    }

    let summary = summarize(state, early).await;

    let mut out = Vec::new();
    if !summary.trim().is_empty() {
        out.push(ChatMessage {
            role: "system".into(),
            content: format!("【以下是更早对话的摘要，避免重复外部上下文】\n{summary}"),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    out.extend(recent);

    let after_chars: usize = out.iter().map(|m| m.content.chars().count()).sum();
    (out, Some(CompressStats { before_chars, after_chars }))
}

/// 用模型把一段历史压缩为摘要（压缩用本地免费模型；失败返回空，不阻塞主流程）
async fn summarize(state: &AppState, history: &[ChatMessage]) -> String {
    let cfg = config();
    let text = history
        .iter()
        .map(|m| {
            let c = m.content.chars().take(500).collect::<String>();
            format!("{}：{}", m.role, c)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "请把下面的对话历史压缩成一段简短摘要（保留关键结论、数字、用户偏好/要求、任务上下文，250 字以内）：\n\n{text}"
    );
    let msgs = vec![ChatMessage {
        role: "user".into(),
        content: prompt,
        tool_calls: None,
        tool_call_id: None,
    }];

    let result = if cfg.local_only_compress {
        state.model.chat_with_tier(ModelTier::Local, &msgs, &[]).await
    } else {
        state.model.chat(&msgs, &[]).await
    };

    result
        .map(|r| r.content.unwrap_or_default())
        .unwrap_or_default()
}

/// 限制单条工具结果大小：读取当前配置并截断超长输出。
/// 仅影响喂给模型的内容；审计/前端展示仍保留完整输出以保证透明。
pub fn cap_tool_result(content: &str) -> String {
    let cfg = config();
    if !cfg.enabled {
        return content.to_string();
    }
    truncate_text(content, cfg.max_tool_result_chars)
}