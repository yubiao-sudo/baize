//! 通用文档抽取：把 txt/md/csv/docx/pdf 等文件读到文本，并做敏感信息检测与脱敏。
//!
//! 供两个场景复用：
//!   ① `ingest_document` 工具：白泽直接读取并解析上传/指定的文档文件；
//!   ② 测试工程师管线的需求文档读取（`test_engineer.rs`）。

use serde_json::{json, Value};

use crate::tools::{PermissionClass, Tool};

/// 返回给模型的最大文档字符数（超长截断，避免撑爆上下文）
const MAX_DOC_CHARS: usize = 20_000;

// ───────────────────── 文本抽取 ─────────────────────

/// 根据扩展名抽取文档文本：纯文本直接读，docx/pdf 走专用抽取
pub fn extract_text(path: &str) -> Result<String, String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "docx" => extract_docx_text(path),
        "pdf" => extract_pdf_text(path),
        _ => std::fs::read_to_string(path).map_err(|e| format!("读取文档失败: {e}")),
    }
}

/// 从 .docx 抽取文本（docx 本质是 zip，正文在 word/document.xml 的 <w:t> 标签里）
fn extract_docx_text(path: &str) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("打开 docx 失败: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("解析 docx 失败: {e}"))?;
    let mut doc = archive
        .by_name("word/document.xml")
        .map_err(|e| format!("读取 document.xml 失败: {e}"))?;
    let mut xml = String::new();
    use std::io::Read;
    doc.read_to_string(&mut xml)
        .map_err(|e| format!("读取 document.xml 失败: {e}"))?;
    Ok(extract_docx_paragraphs(&xml))
}

/// 按段落抽取 <w:t> 标签文本
fn extract_docx_paragraphs(xml: &str) -> String {
    let re = regex::Regex::new(r"<w:t[^>]*>([^<]*)</w:t>").unwrap();
    let mut out = String::new();
    for para in xml.split("</w:p>") {
        let mut line = String::new();
        for cap in re.captures_iter(para) {
            if let Some(t) = cap.get(1) {
                line.push_str(t.as_str());
            }
        }
        if !line.trim().is_empty() {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// 从 .pdf 抽取文本（尽力而为，扫描版 PDF 可能无文本层）
fn extract_pdf_text(path: &str) -> Result<String, String> {
    pdf_extract::extract_text(path).map_err(|e| format!("抽取 pdf 文本失败: {e}"))
}

// ───────────────────── 敏感信息检测与脱敏 ─────────────────────

/// 把文本中的密钥/令牌/凭据脱敏，返回 (脱敏后的文本, 命中的敏感类型列表)。
/// 命中的敏感值替换为 `[REDACTED]`，避免敏感内容进入模型上下文。
pub fn redact_secrets(text: &str) -> (String, Vec<String>) {
    // 整段替换的 Token 模式
    const WHOLE: &[(&str, &str)] = &[
        ("OpenAI Key", r"\bsk-[A-Za-z0-9_-]{20,}\b"),
        ("GitHub Token", r"\bgh[pousr]_[A-Za-z0-9]{20,}\b"),
        ("AWS Access Key", r"\bAKIA[0-9A-Z]{16}\b"),
        ("Private Key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    ];
    // 保留键名、仅替换值的赋值模式（group1 = 键名）
    const VALUE: &[(&str, &str)] = &[(
        "凭据赋值",
        r"(?i)\b(api[_-]?key|apikey|secret|token|password|passwd)\b\s*[:=]\s*\S{6,}",
    )];

    let mut out = text.to_string();
    let mut found: Vec<String> = Vec::new();
    for (label, pat) in WHOLE {
        if let Ok(re) = regex::Regex::new(pat) {
            let n = re.find_iter(text).count();
            if n > 0 {
                out = re.replace_all(&out, "[REDACTED]").to_string();
                found.push(format!("{label}×{n}"));
            }
        }
    }
    for (label, pat) in VALUE {
        if let Ok(re) = regex::Regex::new(pat) {
            let n = re.find_iter(text).count();
            if n > 0 {
                out = re.replace_all(&out, "${1}=[REDACTED]").to_string();
                found.push(format!("{label}×{n}"));
            }
        }
    }
    (out, found)
}

// ───────────────────── 工具：ingest_document ─────────────────────

/// 读取并解析任意受支持的文档文件：抽取纯文本，检测并脱敏敏感信息后返回。
pub struct IngestDocumentTool;

impl Tool for IngestDocumentTool {
    fn name(&self) -> &str {
        "ingest_document"
    }
    fn description(&self) -> &str {
        "读取并解析一个文档文件（支持 txt/md/csv/docx/pdf），返回其文本内容；其中的密钥/令牌会被自动打码"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "要解析的文档本地路径" }
            },
            "required": ["path"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let path = args["path"].as_str().ok_or("缺少参数 path")?;
        let text = extract_text(path)?;
        let (redacted, sensitive) = redact_secrets(&text);
        let total = redacted.chars().count();
        let truncated = total > MAX_DOC_CHARS;
        let content: String = if truncated {
            redacted.chars().take(MAX_DOC_CHARS).collect()
        } else {
            redacted
        };
        Ok(json!({
            "ok": true,
            "path": path,
            "chars": content.chars().count(),
            "total_chars": total,
            "truncated": truncated,
            "sensitive_detected": sensitive,
            "content": content,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_openai_key_and_keeps_key_name() {
        let text = "api_key = abcdef123456\nopenai 用的 sk-abcdefghijklmnopqrstuvwxyz123456\n普通句子";
        let (out, found) = redact_secrets(text);
        assert!(out.contains("api_key=[REDACTED]"));
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("sk-abcdefghijklmnopqrstuvwxyz123456"));
        assert!(out.contains("普通句子"));
        assert!(!found.is_empty());
    }

    #[test]
    fn leaves_plain_text_untouched() {
        let text = "这是一段没有任何凭据的普通描述。";
        let (out, found) = redact_secrets(text);
        assert_eq!(out, text);
        assert!(found.is_empty());
    }
}