//! 文本增强：纯本地变换 + 本地 AI 加工。
//!
//! - `text_transform`：无需模型的确定性文本处理（大小写/去重/排序/编码/格式化/正则提取）。
//! - `text_ai`：接模型做摘要/翻译/润色/纠错/解释/关键词，**本地模型优先**，
//!   敏感数据默认不出本机；本地不可用时自动回退云端链路。

use std::collections::HashSet;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use regex::Regex;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

use crate::model::{ChatMessage, ModelTier};
use crate::tools::{PermissionClass, Tool};
use crate::AppState;

// ───────────────────── 纯函数变换 ─────────────────────

/// 对文本做确定性变换，返回结果字符串。
pub fn transform_text(text: &str, operation: &str) -> Result<String, String> {
    match operation {
        "upper" => Ok(text.to_uppercase()),
        "lower" => Ok(text.to_lowercase()),
        "title" => Ok(title_case(text)),
        "trim" => Ok(text.trim().to_string()),
        "strip_blank" => Ok(non_blank_lines(text)),
        "dedupe_lines" => Ok(dedupe_lines(text)),
        "sort_lines" => Ok(sort_lines(text, false, false)),
        "sort_lines_desc" => Ok(sort_lines(text, true, false)),
        "sort_lines_unique" => Ok(sort_lines(text, false, true)),
        "reverse_lines" => Ok(text.lines().rev().collect::<Vec<_>>().join("\n")),
        "add_line_numbers" => Ok(add_line_numbers(text)),
        "base64_encode" => Ok(B64.encode(text.as_bytes())),
        "base64_decode" => {
            let raw = B64
                .decode(text.trim())
                .map_err(|e| format!("base64 解码失败（输入可能不是合法 base64）: {e}"))?;
            String::from_utf8(raw).map_err(|e| format!("base64 解码结果不是 UTF-8 文本: {e}"))
        }
        "url_encode" => Ok(url_encode(text)),
        "url_decode" => Ok(url_decode(text)),
        "json_format" => format_json(text, true),
        "json_minify" => format_json(text, false),
        "extract_email" => extract(text, r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
        "extract_url" => extract(text, r#"https?://[^\s"'<>]+"#),
        "extract_number" => extract(text, r"-?\d+(?:\.\d+)?"),
        "extract_ip" => extract(text, r"\b(?:\d{1,3}\.){3}\d{1,3}\b"),
        "extract_phone" => extract(text, r"(?:\+?86[- ]?)?1[3-9]\d{9}"),
        other => Err(format!("未知操作: {other}")),
    }
}

/// 每个空白分隔的 token 首字母大写、其余小写（中文字符原样保留）
fn title_case(text: &str) -> String {
    text.split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) if c.is_ascii_alphabetic() => {
                    c.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
                }
                Some(c) => c.to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn non_blank_lines(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn dedupe_lines(text: &str) -> String {
    let mut seen = HashSet::new();
    text.lines()
        .filter(|l| seen.insert(l.to_string()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn sort_lines(text: &str, desc: bool, unique: bool) -> String {
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| l.to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if unique {
        let mut seen = HashSet::new();
        lines.retain(|l| seen.insert(l.clone()));
    }
    lines.sort();
    if desc {
        lines.reverse();
    }
    lines.join("\n")
}

fn add_line_numbers(text: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(i, l)| format!("{}|{}", i + 1, l))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_json(text: &str, pretty: bool) -> Result<String, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("无法解析 JSON: {e}"))?;
    if pretty {
        serde_json::to_string_pretty(&v).map_err(|e| e.to_string())
    } else {
        serde_json::to_string(&v).map_err(|e| e.to_string())
    }
}

fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(&text[i + 1..i + 3], 16) {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 用正则提取匹配项，去重后每行一个返回；无匹配返回空串提示。
fn extract(text: &str, pattern: &str) -> Result<String, String> {
    let re = Regex::new(pattern).map_err(|e| format!("正则错误: {e}"))?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in re.find_iter(text) {
        if seen.insert(m.as_str().to_string()) {
            out.push(m.as_str().to_string());
        }
    }
    if out.is_empty() {
        Ok(String::new())
    } else {
        Ok(out.join("\n"))
    }
}

// ───────────────────── AI 加工 ─────────────────────

fn system_prompt(action: &str, target_lang: &str) -> Result<String, String> {
    let lang = if target_lang.trim().is_empty() {
        "中文"
    } else {
        target_lang
    };
    let prompt = match action {
        "summarize" => {
            "你是文本摘要助手。请把用户提供的文本概括为核心要点，用简洁中文输出要点式摘要。".to_string()
        }
        "translate" => format!("你是翻译助手。请把用户提供的文本翻译成{lang}，只输出译文，不要附加任何说明。"),
        "polish" => "你是文字润色助手。在不改变原意的前提下，把用户提供的文本润色得更流畅、准确、专业，直接输出润色后的文本。".to_string(),
        "fix" => "你是校对助手。请纠正用户文本中的错别字、语法错误和标点问题，直接输出修正后的完整文本。".to_string(),
        "explain" => "请用通俗易懂的中文解释用户提供的文本的含义与背景，结构清晰。".to_string(),
        "keywords" => "请从用户提供的文本中提取关键词/关键信息，用顿号或逗号分隔输出，不要输出多余说明。".to_string(),
        other => {
            return Err(format!(
                "未知的 AI 动作: {other}（支持 summarize / translate / polish / fix / explain / keywords）"
            ))
        }
    };
    Ok(prompt)
}

/// 对文本做 AI 增强：本地模型优先（隐私），失败回退默认链路。
async fn run_text_ai(
    state: &AppState,
    action: &str,
    text: &str,
    target_lang: &str,
) -> Result<String, String> {
    let sys = system_prompt(action, target_lang)?;
    let msgs = vec![
        ChatMessage {
            role: "system".into(),
            content: sys,
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".into(),
            content: text.to_string(),
            tool_calls: None,
            tool_call_id: None,
        },
    ];
    match state.model.chat_with_tier(ModelTier::Local, &msgs, &[]).await {
        Ok(r) => Ok(r.content.unwrap_or_default()),
        Err(_) => state
            .model
            .chat(&msgs, &[])
            .await
            .map(|r| r.content.unwrap_or_default()),
    }
}

// ───────────────────── 工具 ─────────────────────

/// 纯本地文本变换工具
pub struct TextTransformTool;

impl Tool for TextTransformTool {
    fn name(&self) -> &str {
        "text_transform"
    }
    fn description(&self) -> &str {
        "对文本做纯本地确定性变换（无需模型）。operation: upper / lower / title / trim / strip_blank / dedupe_lines / sort_lines / sort_lines_desc / sort_lines_unique / reverse_lines / add_line_numbers / base64_encode / base64_decode / url_encode / url_decode / json_format / json_minify / extract_email / extract_url / extract_number / extract_ip / extract_phone"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "要处理的文本" },
                "operation": { "type": "string", "description": "变换操作名（见 description）" }
            },
            "required": ["text", "operation"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let text = args["text"].as_str().ok_or("缺少参数 text")?;
        let operation = args["operation"].as_str().ok_or("缺少参数 operation")?;
        let result = transform_text(text, operation)?;
        Ok(json!({ "ok": true, "operation": operation, "chars": result.chars().count(), "result": result }))
    }
}

/// AI 文本增强工具（摘要/翻译/润色/纠错/解释/关键词）
pub struct TextAiTool {
    app: AppHandle,
}

impl TextAiTool {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl Tool for TextAiTool {
    fn name(&self) -> &str {
        "text_ai"
    }
    fn description(&self) -> &str {
        "对文本做 AI 增强加工（本地模型优先，隐私数据默认不出本机）。action: summarize 摘要 / translate 翻译 / polish 润色 / fix 纠错 / explain 解释 / keywords 提取关键词；target_lang 为翻译目标语言（默认中文）"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "要处理的文本" },
                "action": { "type": "string", "enum": ["summarize", "translate", "polish", "fix", "explain", "keywords"], "description": "加工动作" },
                "target_lang": { "type": "string", "description": "翻译目标语言（仅 translate 生效），默认中文" }
            },
            "required": ["text", "action"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let text = args["text"].as_str().ok_or("缺少参数 text")?;
        let action = args["action"].as_str().ok_or("缺少参数 action")?;
        let target_lang = args["target_lang"].as_str().unwrap_or("");
        let app = self.app.clone();
        let text = text.to_string();
        let action = action.to_string();
        let target_lang = target_lang.to_string();

        tauri::async_runtime::block_on(async move {
            let state = app.state::<AppState>();
            let result = run_text_ai(state.inner(), &action, &text, &target_lang).await?;
            Ok(json!({ "ok": true, "action": action, "chars": result.chars().count(), "result": result }))
        })
    }
}

// ───────────────────── 命令 ─────────────────────

/// 纯本地文本变换（供前端面板即时调用）
#[tauri::command]
pub fn text_transform(text: String, operation: String) -> Result<String, String> {
    transform_text(&text, &operation)
}

/// AI 文本增强（本地优先）
#[tauri::command]
pub async fn text_ai(
    state: State<'_, AppState>,
    text: String,
    action: String,
    target_lang: Option<String>,
) -> Result<String, String> {
    run_text_ai(
        state.inner(),
        &action,
        &text,
        target_lang.as_deref().unwrap_or(""),
    )
    .await
}