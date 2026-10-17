//! 远程知识库客户端：把 knowledge-fabric 底座接到白泽的 RAG 工具上。
//!
//! ## 为什么要有这一层
//!
//! `rag.rs` 原本的本地 RAG 是 demo 级实现：只扫文本文件、400 字符盲切、
//! 暴力余弦（且每次查询克隆全部向量）、阈值 0.2 且**没有拒答机制**、
//! 结果只有 `path`+`content` 没有可核实的字符区间。它当"随便翻翻本地文档"够用，
//! 但当"企业知识库"不够。
//!
//! 这一层只做一件事：**把 `rag_search` 的后端换成底座**，工具名与参数保持不变，
//! 所以 agent 主循环、提示词、技能、工作流都不用改。
//!
//! ## 设计要点
//!
//! * **两级凭证**：控制台生成一次性接入码（8 位、10 分钟），
//!   这里用 `exchange_code()` 换成长期令牌；令牌经 `vault`（Windows DPAPI）
//!   加密后落库，**不存明文**。
//! * **回环不走代理**：底座的 `kb_base_url` 常是 `127.0.0.1`，而代理软件
//!   接管回环会导致连不上——`embedding.rs` 里已有同样的教训，这里照做。
//! * **拒答要能传到 agent**：底座判拒答时必须让 agent 看见 `refused=true`，
//!   否则它会在空证据上凭常识编答案。这是整个联动的成败点。
//! * **回落是显式开关**：`fallback_local` 默认 `false`——一旦配了企业库就以它
//!   为唯一依据，底座挂了就明确报错。本地那份质量差太多，静默回落会让用户
//!   以为查的是企业库、实际拿的是过期本地数据。

use std::time::Duration;

use serde_json::{json, Value};
use std::sync::Arc;

use crate::memory::MemoryStore;

/// 令牌在 vault 里的键名（DPAPI 加密，不落明文）
const TOKEN_KEY: &str = "kf_kb_token";

/// 检索模式
///
/// * `off`    —— 不启用知识库（`rag_*` 工具直接返回空，不打扰 agent）
/// * `local`  —— 原来的本地 RAG（默认，保持向后兼容）
/// * `remote` —— 只走底座；失败即报错
/// * `hybrid` —— 优先底座，失败回落本地（**结果里会标注来源**）
#[derive(Clone, Debug)]
pub struct KbConfig {
    pub mode: String,
    pub base_url: String,
    /// 摄入服务地址。与检索服务是**两个进程/两个端口**（默认 8082 / 8083），
    /// 不从这个推那个——端口映射一变就错，宁可显式配。
    pub ingest_url: String,
    pub tenant: String,
    pub kb: String,
    pub profile: String,
    pub top_k: usize,
    pub timeout_ms: u64,
    /// 底座不可达时是否回落本地索引。**默认 false**：宁可失败，也不给错误依据。
    pub fallback_local: bool,
}

impl Default for KbConfig {
    fn default() -> Self {
        Self {
            mode: "local".to_string(),
            base_url: "http://127.0.0.1:8082".to_string(),
            ingest_url: "http://127.0.0.1:8083".to_string(),
            tenant: "acme".to_string(),
            kb: "demo".to_string(),
            profile: "hybrid_rerank".to_string(),
            top_k: 6,
            timeout_ms: 8000,
            fallback_local: false,
        }
    }
}

fn get_str(store: &MemoryStore, key: &str, default: &str) -> String {
    store
        .get_setting(key)
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

impl KbConfig {
    pub fn load(store: &MemoryStore) -> Self {
        let d = Self::default();
        let top_k = store
            .get_setting("kb_top_k")
            .ok()
            .flatten()
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(d.top_k)
            .clamp(1, 50);
        let timeout_ms = store
            .get_setting("kb_timeout_ms")
            .ok()
            .flatten()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(d.timeout_ms)
            .clamp(1000, 120_000);
        // 老版本没有这个键 → 默认 false（不回落）
        let fallback_local = store
            .get_setting("kb_fallback_local")
            .ok()
            .flatten()
            .map(|s| s.trim() == "1" || s.eq_ignore_ascii_case("true"))
            .unwrap_or(d.fallback_local);
        Self {
            mode: get_str(store, "kb_mode", &d.mode).to_lowercase(),
            base_url: get_str(store, "kb_base_url", &d.base_url)
                .trim_end_matches('/')
                .to_string(),
            ingest_url: get_str(store, "kb_ingest_url", &d.ingest_url)
                .trim_end_matches('/')
                .to_string(),
            tenant: get_str(store, "kb_tenant", &d.tenant),
            kb: get_str(store, "kb_id", &d.kb),
            profile: get_str(store, "kb_profile", &d.profile),
            top_k,
            timeout_ms,
            fallback_local,
        }
    }

    pub fn save(&self, store: &MemoryStore) -> Result<(), String> {
        store.set_setting("kb_mode", &self.mode)?;
        store.set_setting("kb_base_url", &self.base_url)?;
        store.set_setting("kb_ingest_url", &self.ingest_url)?;
        store.set_setting("kb_tenant", &self.tenant)?;
        store.set_setting("kb_id", &self.kb)?;
        store.set_setting("kb_profile", &self.profile)?;
        store.set_setting("kb_top_k", &self.top_k.to_string())?;
        store.set_setting("kb_timeout_ms", &self.timeout_ms.to_string())?;
        store.set_setting(
            "kb_fallback_local",
            if self.fallback_local { "1" } else { "0" },
        )?;
        Ok(())
    }

    /// 是否应该尝试走底座
    pub fn uses_remote(&self) -> bool {
        matches!(self.mode.as_str(), "remote" | "hybrid")
    }

    pub fn is_off(&self) -> bool {
        self.mode == "off"
    }

    /// 给前端/日志用的脱敏视图（**不含令牌**）
    pub fn to_json(&self, has_token: bool) -> Value {
        json!({
            "mode": self.mode,
            "base_url": self.base_url,
            "ingest_url": self.ingest_url,
            "tenant": self.tenant,
            "kb": self.kb,
            "profile": self.profile,
            "top_k": self.top_k,
            "timeout_ms": self.timeout_ms,
            "fallback_local": self.fallback_local,
            "has_token": has_token,
        })
    }
}

// ───────────────────────────── 令牌（vault） ─────────────────────────────

pub fn load_token(store: &MemoryStore) -> Option<String> {
    crate::vault::get_plain(store, TOKEN_KEY).ok().filter(|s| !s.is_empty())
}

pub fn save_token(store: &MemoryStore, token: &str) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("令牌不能为空".into());
    }
    let cipher = crate::vault::seal(TOKEN_KEY, token)?;
    store.vault_set(TOKEN_KEY, &cipher)?;
    Ok(())
}

pub fn clear_token(store: &MemoryStore) -> Result<bool, String> {
    store.vault_delete(TOKEN_KEY)
}

// ───────────────────────────── HTTP 基础 ─────────────────────────────

/// 发一个 JSON 请求（阻塞，独立线程执行）。
///
/// 与 `embedding.rs` 同样的两个关键点：
/// 1. **`.no_proxy()`** —— 回环地址绝不走代理；
/// 2. 用独立线程 + join，避免在 async 上下文里 drop tokio 运行时导致 panic。
fn http_json(
    method: &'static str,
    url: String,
    token: Option<String>,
    body: Option<Value>,
    timeout_ms: u64,
) -> Result<(u16, Value), String> {
    std::thread::spawn(move || -> Result<(u16, Value), String> {
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_millis(timeout_ms.min(10_000)))
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

        let mut req = match method {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            other => return Err(format!("不支持的 method: {other}")),
        };
        if let Some(t) = token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().map_err(|e| {
            if e.is_timeout() {
                format!("请求超时（{} ms）: {url}", timeout_ms)
            } else if e.is_connect() {
                format!("连不上底座（{url}）。确认服务已启动，或检查 kb_base_url")
            } else {
                format!("请求失败: {e}")
            }
        })?;
        let status = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        let data: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
        Ok((status, data))
    })
    .join()
    .map_err(|_| "HTTP 线程异常退出".to_string())?
}

fn detail_of(data: &Value) -> String {
    data.get("detail")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("raw").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string()
}

// ───────────────────────────── 检索结果 ─────────────────────────────

/// 一次知识库查询的结果（本地 / 远程共用同一形状，便于工具层无差别返回）
#[derive(Clone, Debug)]
pub struct KbAnswer {
    pub refused: bool,
    pub reason: Option<String>,
    /// `knowledge-fabric` | `local` | `local-fallback` | `none`
    pub source: &'static str,
    pub answerability: Option<f64>,
    pub threshold: Option<f64>,
    pub hits: Vec<Value>,
}

impl KbAnswer {
    pub fn empty(source: &'static str) -> Self {
        Self {
            refused: false,
            reason: None,
            source,
            answerability: None,
            threshold: None,
            hits: Vec::new(),
        }
    }

    pub fn refused(source: &'static str, reason: impl Into<String>) -> Self {
        Self {
            refused: true,
            reason: Some(reason.into()),
            source,
            answerability: None,
            threshold: None,
            hits: Vec::new(),
        }
    }

    /// 对外的 JSON：**旧字段（count/hits）保持不变**，只增加新字段，
    /// 这样调用方与前端不必同时改。
    pub fn to_json(&self) -> Value {
        json!({
            "count": self.hits.len(),
            "refused": self.refused,
            "reason": self.reason,
            "source": self.source,
            "answerability": self.answerability,
            "threshold": self.threshold,
            "hits": self.hits,
        })
    }
}

// ───────────────────────────── 底座客户端 ─────────────────────────────

pub struct RemoteKb {
    cfg: KbConfig,
    token: Option<String>,
}

impl RemoteKb {
    pub fn new(cfg: KbConfig, token: Option<String>) -> Self {
        Self { cfg, token }
    }

    pub fn from_store(store: &MemoryStore) -> Self {
        let cfg = KbConfig::load(store);
        let token = load_token(store);
        Self::new(cfg, token)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.base_url.trim_end_matches('/'), path)
    }

    /// 检索。底座判拒答时返回 `refused=true` 且 hits 为空。
    pub fn search(&self, query: &str, top_k: usize) -> KbAnswer {
        let body = json!({
            "query": query,
            "tenant": self.cfg.tenant,
            "kb": self.cfg.kb,
            "profile": self.cfg.profile,
            "top_k": if top_k > 0 { top_k } else { self.cfg.top_k },
        });
        let (status, data) = match http_json(
            "POST",
            self.url("/v1/search"),
            self.token.clone(),
            Some(body),
            self.cfg.timeout_ms,
        ) {
            Ok(v) => v,
            Err(e) => return KbAnswer::refused("knowledge-fabric", e),
        };
        if status == 401 {
            return KbAnswer::refused(
                "knowledge-fabric",
                "底座要求接入令牌（401）。请在 设置 → 知识与数据 里用接入码重新连接",
            );
        }
        if status != 200 {
            return KbAnswer::refused(
                "knowledge-fabric",
                format!("底座返回 HTTP {status} {}", detail_of(&data)),
            );
        }

        let trace = data.get("trace").cloned().unwrap_or_else(|| json!({}));
        let refused = trace
            .get("refused")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let answerability = trace.get("answerability").and_then(|v| v.as_f64());
        let threshold = trace.get("threshold_value").and_then(|v| v.as_f64());

        let hits: Vec<Value> = data
            .get("evidence")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().map(evidence_to_hit).collect())
            .unwrap_or_default();

        // 拒答时把理由讲清楚，agent 才能如实转达给用户
        let reason = if refused {
            let notes = trace
                .get("notes")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join("；")
                })
                .unwrap_or_default();
            Some(if notes.is_empty() {
                format!(
                    "底座判定依据不足（可达分 {} < 阈值 {}）",
                    answerability.map(|v| format!("{v:.4}")).unwrap_or_else(|| "N/A".into()),
                    threshold.map(|v| format!("{v:.4}")).unwrap_or_else(|| "N/A".into())
                )
            } else {
                notes
            })
        } else if hits.is_empty() {
            Some("底座没有返回证据".to_string())
        } else {
            None
        };

        KbAnswer {
            refused,
            reason,
            source: "knowledge-fabric",
            answerability,
            threshold,
            hits,
        }
    }

    /// 列出底座上可用的知识库（agent 不必连数据库）
    pub fn list_kbs(&self) -> Result<Value, String> {
        let (status, data) = http_json(
            "GET",
            self.url("/v1/kbs"),
            self.token.clone(),
            None,
            self.cfg.timeout_ms,
        )?;
        if status == 401 {
            return Err("底座要求接入令牌（401），请先用接入码连接".into());
        }
        if status != 200 {
            return Err(format!("列知识库失败：HTTP {status} {}", detail_of(&data)));
        }
        Ok(data)
    }

    pub fn get_kb(&self) -> Result<Value, String> {
        let path = format!("/v1/kb/{}/{}", self.cfg.tenant, self.cfg.kb);
        let (status, data) = http_json(
            "GET",
            self.url(&path),
            self.token.clone(),
            None,
            self.cfg.timeout_ms,
        )?;
        if status == 404 {
            return Err(format!(
                "知识库不存在：{}/{}",
                self.cfg.tenant, self.cfg.kb
            ));
        }
        if status != 200 {
            return Err(format!("读取知识库失败：HTTP {status} {}", detail_of(&data)));
        }
        Ok(data)
    }

    pub fn health(&self) -> Result<Value, String> {
        let (status, data) = http_json("GET", self.url("/health"), None, None, 5000)?;
        if status != 200 {
            return Err(format!("底座 /health 返回 HTTP {status}"));
        }
        Ok(json!({
            "reachable": true,
            "status": data.get("status"),
            "auth": data.get("auth"),
            "reranker": data.get("reranker"),
            "embed": data.get("embed"),
        }))
    }

    // ───────────────────────── 摄入（上传到底座） ─────────────────────────

    fn ingest_url_of(&self, path: &str) -> String {
        format!("{}{}", self.cfg.ingest_url.trim_end_matches('/'), path)
    }

    /// 上传**单个文件**到底座的摄入服务，返回精简摘要。
    ///
    /// 与检索不同，摄入是 `multipart/form-data`：底座那边要按扩展名选解析器、
    /// 落临时文件、跑完整流水线。这里只负责把文件推过去并解读结果。
    pub fn upload_file(&self, path: &str, doc_id: &str) -> Result<Value, String> {
        let path = path.trim();
        if path.is_empty() {
            return Err("路径不能为空".into());
        }
        let suffix = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if !SUPPORTED_INGEST_SUFFIXES.contains(&suffix.as_str()) {
            return Err(format!(
                "不支持的格式 .{suffix}（底座支持 pdf / md / markdown / mdown）"
            ));
        }
        if !std::path::Path::new(path).is_file() {
            return Err(format!("文件不存在或不是文件: {path}"));
        }

        let cfg = self.cfg.clone();
        let token = self.token.clone();
        let doc_id = doc_id.trim().to_string();
        let path_owned = path.to_string();

        std::thread::spawn(move || -> Result<Value, String> {
            let client = reqwest::blocking::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(10))
                // 摄入比检索慢得多（要解析 + 嵌入 + 索引），单独放宽
                .timeout(Duration::from_millis(cfg.timeout_ms.max(60_000) * 6))
                .build()
                .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

            // 先把文件读进内存再组 multipart。
            // 用 `Form::file()`（惰性文件流）实测会**卡死**：请求根本没到服务端，
            // 客户端一直等到超时。文件本来就小（知识文档通常 < 10MB），
            // 读成字节更稳，也避免文件句柄生命周期的问题。
            let bytes = std::fs::read(&path_owned)
                .map_err(|e| format!("读取文件失败: {e}"))?;
            let fname = std::path::Path::new(&path_owned)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "upload.pdf".to_string());
            let mime = match std::path::Path::new(&path_owned)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase()
                .as_str()
            {
                "pdf" => "application/pdf",
                _ => "text/markdown",
            };
            let part = reqwest::blocking::multipart::Part::bytes(bytes)
                .file_name(fname.clone())
                .mime_str(mime)
                .map_err(|e| format!("构造 multipart 失败: {e}"))?;

            let mut form = reqwest::blocking::multipart::Form::new()
                .text("tenant", cfg.tenant.clone())
                .text("kb", cfg.kb.clone())
                .text("version", "1".to_string())
                .text("source_uri", fname.clone());
            if !doc_id.is_empty() {
                form = form.text("doc_id", doc_id.clone());
            }
            form = form.part("file", part);

            let url = format!("{}/v1/ingest", cfg.ingest_url.trim_end_matches('/'));
            let mut req = client.post(&url).multipart(form);
            if let Some(t) = token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            let resp = req.send().map_err(|e| {
                if e.is_timeout() {
                    format!("摄入超时: {url}")
                } else if e.is_connect() {
                    format!("连不上摄入服务（{url}）。确认 ingest 已启动，或检查 kb_ingest_url")
                } else {
                    format!("摄入请求失败: {e}")
                }
            })?;
            let status = resp.status().as_u16();
            let text = resp.text().unwrap_or_default();
            let data: Value =
                serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }));
            if status != 200 {
                return Err(format!(
                    "底座摄入失败：HTTP {status} {}",
                    detail_of(&data)
                ));
            }
            let counts = data.get("counts").cloned().unwrap_or_else(|| json!({}));
            Ok(json!({
                "ok": data.get("ok").and_then(|v| v.as_bool()).unwrap_or(true),
                "file": fname,
                "doc_id": if doc_id.is_empty() {
                    data.get("doc_id").cloned().unwrap_or(Value::Null)
                } else {
                    json!(doc_id)
                },
                "parents": counts.get("parents"),
                "children": counts.get("children"),
                "chunks": counts.get("embedded"),
                "errors": data.get("errors").cloned().unwrap_or_else(|| json!([])),
            }))
        })
        .join()
        .map_err(|_| "摄入线程异常退出".to_string())?
    }

    /// 摄入一个**路径**：单文件直接传；目录则挑受支持的文件逐个传。
    ///
    /// 为什么要有上限：一次把几千个文件推过去会把底座和调用方都拖死，
    /// 而且 agent 工具调用通常有超时。这里默认最多 20 个，并在返回里
    /// **如实说明被截断**（不静默丢弃）。
    pub fn ingest_path(&self, path: &str, max_files: usize) -> Result<Value, String> {
        let p = std::path::Path::new(path.trim());
        if !p.exists() {
            return Err(format!("路径不存在: {}", path.trim()));
        }
        let limit = max_files.max(1);

        let files: Vec<std::path::PathBuf> = if p.is_file() {
            vec![p.to_path_buf()]
        } else if p.is_dir() {
            let mut found = Vec::new();
            collect_ingestable(p, &mut found, 0, limit);
            if found.is_empty() {
                return Err(format!(
                    "目录里没有受支持的文档（只用 pdf / md / markdown / mdown）: {}",
                    p.display()
                ));
            }
            found
        } else {
            return Err(format!("既不是文件也不是目录: {}", p.display()));
        };

        let mut results = Vec::new();
        let mut ok_count = 0usize;
        for f in files.iter().take(limit) {
            let fp = f.to_string_lossy().to_string();
            match self.upload_file(&fp, "") {
                Ok(v) => {
                    if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
                        ok_count += 1;
                    }
                    results.push(v);
                }
                Err(e) => results.push(json!({ "ok": false, "file": fp, "error": e })),
            }
        }

        Ok(json!({
            "ok": ok_count > 0,
            "mode": "remote",
            "tenant": self.cfg.tenant,
            "kb": self.cfg.kb,
            "ingest_url": self.cfg.ingest_url,
            "total": files.len(),
            "attempted": files.len().min(limit),
            "succeeded": ok_count,
            "truncated": files.len() > limit,
            "note": if files.len() > limit {
                format!("目录内受支持文件共 {} 个，本次只传了前 {} 个（单次上限 {}）",
                        files.len(), limit, limit)
            } else {
                String::new()
            },
            "files": results,
        }))
    }
}

/// 底座摄入支持的格式（与 `services/ingest/kf_ingest/parse.py` 的
/// `SUPPORTED_SUFFIXES` 保持一致；不一致会得到"上传成功但解析失败"的困惑）
const SUPPORTED_INGEST_SUFFIXES: [&str; 4] = ["pdf", "md", "markdown", "mdown"];

/// 递归收集可摄入的文件（深度上限 4，与底座目录摄入的默认一致）
fn collect_ingestable(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>,
                      depth: usize, limit: usize) {
    if depth > 4 || out.len() >= limit.saturating_mul(5) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = entries.flatten().collect();
    items.sort_by_key(|e| e.path());
    for entry in items {
        let path = entry.path();
        if path.is_dir() {
            collect_ingestable(&path, out, depth + 1, limit);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if SUPPORTED_INGEST_SUFFIXES.contains(&ext.to_lowercase().as_str()) {
                out.push(path);
            }
        }
    }
}

/// 把底座的一条证据转成 `rag_search` 的 hit 形状。
///
/// **必须保留 `path` / `content` / `score` 这三个旧字段**（本地 RAG 用的就是它们），
/// 否则前端和提示词里的既有假设会断。新增字段全部是增量的。
fn evidence_to_hit(ev: &Value) -> Value {
    let content = ev.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let uri = ev
        .get("source_uri")
        .and_then(|v| v.as_str())
        .or_else(|| ev.get("source").and_then(|v| v.as_str()))
        .unwrap_or("");
    let page = ev.get("page").and_then(|v| v.as_i64());
    let range: Option<Vec<i64>> = ev
        .get("char_range")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_i64()).collect());
    let score_raw = ev.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
    // 本地 RAG 的 score 是 0~100 整数（相似度 ×100），这里保持同一量纲
    let score = if score_raw <= 1.0 {
        (score_raw * 100.0).round() as i64
    } else {
        score_raw.round() as i64
    };
    let citation = match (&uri, page, &range) {
        (u, Some(p), Some(r)) if r.len() == 2 => format!("{u} p{p} [{}-{}]", r[0], r[1]),
        (u, Some(p), _) => format!("{u} p{p}"),
        (u, _, Some(r)) if r.len() == 2 => format!("{u} [{}-{}]", r[0], r[1]),
        (u, _, _) => u.to_string(),
    };
    let section = ev
        .get("section_path")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default();

    json!({
        // —— 兼容旧字段 ——
        "path": uri,
        "content": content,
        "score": score,
        // —— 增量字段 ——
        "score_raw": score_raw,
        "page": page,
        "char_range": range,
        "citation": citation,
        "section": section,
        "chunk_id": ev.get("chunk_id").cloned().unwrap_or(Value::Null),
    })
}

// ───────────────────────────── 接入码兑换 ─────────────────────────────

/// 用接入码换长期令牌。
///
/// 底座侧的限制：接入码**只能兑换一次**、10 分钟有效；签发只允许本机。
/// 这里不写库，交给调用方决定（命令层会写 vault）。
pub fn exchange_code(base_url: &str, code: &str, timeout_ms: u64) -> Result<Value, String> {
    let code = code.trim().to_uppercase();
    if code.is_empty() {
        return Err("接入码不能为空".into());
    }
    let url = format!("{}/v1/auth/exchange", base_url.trim_end_matches('/'));
    let (status, data) = http_json(
        "POST",
        url,
        None,
        Some(json!({ "code": code, "name": "baize" })),
        timeout_ms.max(8000),
    )?;
    if status == 401 {
        return Err(detail_of(&data).if_empty("接入码无效或已过期，请在底座控制台重新生成"));
    }
    if status != 200 {
        return Err(format!("兑换失败：HTTP {status} {}", detail_of(&data)));
    }
    let token = data
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or("底座响应缺少 token")?
        .to_string();
    if !token.starts_with("kf_") {
        return Err("底座返回的 token 前缀不合法（应以 kf_ 开头）".into());
    }
    // 按**字符**截前缀，不用字节切片：`&token[..8]` 在 token 含多字节字符时
    // 会落在非 UTF-8 边界上，直接 panic。令牌当前是 ASCII，但这里不该依赖它。
    let prefix: String = token.chars().take(8).collect();
    Ok(json!({
        "ok": true,
        "tenant": data.get("tenant_id").cloned().unwrap_or(Value::Null),
        "key_id": data.get("key_id").cloned().unwrap_or(Value::Null),
        "token_prefix": prefix,
        // 令牌明文回给命令层落 vault，不写日志
        "token": token,
    }))
}

/// 小工具：空串时取默认
trait IfEmpty {
    fn if_empty(self, alt: &str) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, alt: &str) -> String {
        if self.trim().is_empty() {
            alt.to_string()
        } else {
            self
        }
    }
}

/// 供后台线程/命令层复用的便捷构造
pub fn remote_from_store(store: &Arc<MemoryStore>) -> RemoteKb {
    RemoteKb::from_store(store.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(content: &str, uri: &str, page: i64, range: (i64, i64), score: f64) -> Value {
        json!({
            "content": content,
            "source_uri": uri,
            "page": page,
            "char_range": [range.0, range.1],
            "score": score,
            "chunk_id": "c1",
            "section_path": ["第 1 章", "(根)"],
        })
    }

    #[test]
    fn hit_keeps_legacy_fields() {
        let h = evidence_to_hit(&ev("abc", "kf_ab.pdf", 0, (16, 37), 0.8721));
        // 旧字段必须在，否则前端与提示词的既有假设会断
        assert_eq!(h["path"], "kf_ab.pdf");
        assert_eq!(h["content"], "abc");
        assert_eq!(h["score"], 87);
    }

    #[test]
    fn hit_adds_citation_and_range() {
        let h = evidence_to_hit(&ev("abc", "kf_ab.pdf", 0, (16, 37), 0.8721));
        assert_eq!(h["citation"], "kf_ab.pdf p0 [16-37]");
        assert_eq!(h["char_range"][0], 16);
        assert_eq!(h["section"], "第 1 章 / (根)");
    }

    #[test]
    fn score_below_one_is_scaled_to_percent() {
        let h = evidence_to_hit(&ev("x", "u", 0, (0, 1), 0.5));
        assert_eq!(h["score"], 50);
        // 已经是百分制的不再乘
        let h2 = evidence_to_hit(&json!({ "content": "x", "score": 87.0 }));
        assert_eq!(h2["score"], 87);
    }

    #[test]
    fn citation_degrades_gracefully() {
        let h = evidence_to_hit(&json!({ "content": "x", "source_uri": "a.md" }));
        assert_eq!(h["citation"], "a.md");
    }

    #[test]
    fn refused_answer_json_shape() {
        let a = KbAnswer::refused("knowledge-fabric", "依据不足");
        let j = a.to_json();
        assert_eq!(j["count"], 0);
        assert_eq!(j["refused"], true);
        assert_eq!(j["reason"], "依据不足");
        assert_eq!(j["source"], "knowledge-fabric");
    }

    #[test]
    fn empty_answer_is_not_refused() {
        let j = KbAnswer::empty("local").to_json();
        assert_eq!(j["refused"], false);
        assert_eq!(j["count"], 0);
    }

    #[test]
    fn config_defaults_are_safe() {
        let c = KbConfig::default();
        assert_eq!(c.mode, "local", "默认保持原有本地行为");
        assert!(!c.fallback_local, "默认不回落：宁可失败也不给错误依据");
        assert!(c.timeout_ms >= 3000);
    }

    #[test]
    fn config_remote_flag() {
        let mut c = KbConfig::default();
        assert!(!c.uses_remote());
        c.mode = "remote".into();
        assert!(c.uses_remote());
        c.mode = "hybrid".into();
        assert!(c.uses_remote());
        c.mode = "off".into();
        assert!(!c.uses_remote());
        assert!(c.is_off());
    }

    #[test]
    fn config_json_hides_token() {
        let c = KbConfig::default();
        let j = c.to_json(true);
        assert_eq!(j["has_token"], true);
        // 绝不能把令牌带出来（这个函数只描述配置）
        assert!(j.get("token").is_none());
    }

    #[test]
    fn exchange_rejects_empty_code() {
        assert!(exchange_code("http://127.0.0.1:8082", "   ", 5000).is_err());
    }

    #[test]
    fn ingest_suffixes_match_base() {
        // 与 services/ingest/kf_ingest/parse.py 的 SUPPORTED_SUFFIXES 对齐；
        // 不一致会出现"上传成功但解析失败"这种最难查的错。
        for s in ["pdf", "md", "markdown", "mdown"] {
            assert!(SUPPORTED_INGEST_SUFFIXES.contains(&s), "缺少格式 {s}");
        }
        assert!(!SUPPORTED_INGEST_SUFFIXES.contains(&"docx"));
    }

    #[test]
    fn upload_rejects_unsupported_extension() {
        let cfg = KbConfig::default();
        let remote = RemoteKb::new(cfg, None);
        let err = remote.upload_file("C:/tmp/a.docx", "").unwrap_err();
        assert!(err.contains("不支持"), "应明确说不支持，实际: {err}");
    }

    #[test]
    fn upload_rejects_missing_file() {
        let remote = RemoteKb::new(KbConfig::default(), None);
        let err = remote.upload_file("C:/definitely/not/here.pdf", "").unwrap_err();
        assert!(err.contains("不存在"), "实际: {err}");
    }

    #[test]
    fn ingest_path_rejects_missing_path() {
        let remote = RemoteKb::new(KbConfig::default(), None);
        let err = remote.ingest_path("C:/definitely/not/here", 5).unwrap_err();
        assert!(err.contains("不存在"), "实际: {err}");
    }

    #[test]
    fn collect_ingestable_finds_only_supported() {
        let dir = std::env::temp_dir().join(format!("kb_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("a.md"), "x");
        let _ = std::fs::write(dir.join("b.pdf"), "x");
        let _ = std::fs::write(dir.join("c.docx"), "x");
        let _ = std::fs::write(dir.join("d.txt"), "x");
        let mut out = Vec::new();
        collect_ingestable(&dir, &mut out, 0, 10);
        let names: Vec<String> = out
            .iter()
            .filter_map(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"a.md".to_string()));
        assert!(names.contains(&"b.pdf".to_string()));
        // 底座不解析 docx / txt，收进来只会白跑一趟
        assert!(!names.contains(&"c.docx".to_string()));
        assert!(!names.contains(&"d.txt".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真机摄入测试：会**真的往底座写入文档**，所以单独 ignore 且显式开启。
    ///
    /// ```powershell
    /// $env:KF_KB_TEST_BASE / KF_KB_TEST_TOKEN / KF_KB_TEST_TENANT / KF_KB_TEST_KB
    /// $env:KF_KB_TEST_FILE = "D:\some.md"
    /// cargo test --offline kb::tests::live_ingest -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn live_ingest() {
        let base = std::env::var("KF_KB_TEST_BASE").unwrap_or_default();
        let token = std::env::var("KF_KB_TEST_TOKEN").unwrap_or_default();
        let file = std::env::var("KF_KB_TEST_FILE").unwrap_or_default();
        if base.is_empty() || token.is_empty() || file.is_empty() {
            eprintln!("跳过：需要 KF_KB_TEST_BASE / TOKEN / FILE");
            return;
        }
        let cfg = KbConfig {
            mode: "remote".into(),
            base_url: base,
            ingest_url: std::env::var("KF_KB_TEST_INGEST")
                .unwrap_or_else(|_| "http://127.0.0.1:8083".into()),
            tenant: std::env::var("KF_KB_TEST_TENANT").unwrap_or_else(|_| "acme".into()),
            kb: std::env::var("KF_KB_TEST_KB").unwrap_or_else(|_| "demo".into()),
            profile: "hybrid_rerank".into(),
            top_k: 5,
            timeout_ms: 60000,
            fallback_local: false,
        };
        let remote = RemoteKb::new(cfg, Some(token));
        let out = remote.ingest_path(&file, 3).expect("摄入应成功");
        eprintln!("ingest -> {out}");
        assert_eq!(out["succeeded"].as_u64().unwrap_or(0) >= 1, true);
    }

    /// 真机集成测试：需要一个**运行中**的底座 + 接入令牌。
    ///
    /// ```powershell
    /// $env:KF_KB_TEST_BASE  = "http://127.0.0.1:8082"
    /// $env:KF_KB_TEST_TOKEN = "kf_xxx"   # 底座控制台「接入 Agent」→ 生成接入码 → 兑换
    /// $env:KF_KB_TEST_TENANT = "acme"; $env:KF_KB_TEST_KB = "demo"
    /// cargo test --offline kb::tests::live -- --ignored --nocapture
    /// ```
    ///
    /// 默认 `#[ignore]`：CI 与日常 `cargo test` 不该依赖外部服务。
    #[test]
    #[ignore]
    fn live_search_and_refuse() {
        let base = std::env::var("KF_KB_TEST_BASE").unwrap_or_default();
        let token = std::env::var("KF_KB_TEST_TOKEN").unwrap_or_default();
        if base.is_empty() || token.is_empty() {
            eprintln!("跳过：未设置 KF_KB_TEST_BASE / KF_KB_TEST_TOKEN");
            return;
        }
        let cfg = KbConfig {
            mode: "remote".into(),
            base_url: base,
            ingest_url: std::env::var("KF_KB_TEST_INGEST")
                .unwrap_or_else(|_| "http://127.0.0.1:8083".into()),
            tenant: std::env::var("KF_KB_TEST_TENANT").unwrap_or_else(|_| "acme".into()),
            kb: std::env::var("KF_KB_TEST_KB").unwrap_or_else(|_| "demo".into()),
            profile: "hybrid_rerank".into(),
            top_k: 5,
            timeout_ms: 20000,
            fallback_local: false,
        };
        let remote = RemoteKb::new(cfg, Some(token));

        // 1) 可达性
        let health = remote.health().expect("底座 /health 应可达");
        eprintln!("health = {health}");

        // 2) 可答问题：应有证据，且每条带 citation
        let ok = remote.search("密码长度要求是多少位", 5);
        eprintln!("可答 -> refused={} hits={} src={}", ok.refused, ok.hits.len(), ok.source);
        assert!(!ok.refused, "可答问题不该被拒答：{:?}", ok.reason);
        assert!(!ok.hits.is_empty(), "可答问题应返回证据");
        for h in &ok.hits {
            assert!(h.get("citation").and_then(|v| v.as_str()).unwrap_or("").len() > 0,
                    "每条证据都应有 citation");
            assert!(h.get("path").is_some(), "必须保留旧的 path 字段");
        }

        // 3) 越界问题：应拒答且 hits 为空
        let bad = remote.search("如何用 Rust 写一个 WebSocket 服务器", 5);
        eprintln!("越界 -> refused={} hits={} reason={:?}", bad.refused, bad.hits.len(), bad.reason);
        assert!(bad.refused, "越界问题应被拒答");
        assert!(bad.hits.is_empty(), "拒答时不应返回证据");
        assert!(bad.reason.is_some(), "拒答必须带理由，否则 agent 无法如实转达");
    }
}
