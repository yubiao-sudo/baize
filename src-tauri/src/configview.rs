//! 配置视图（configview）：让模型「知道」自己被配成了什么样。
//!
//! ## 为什么需要它
//! settings 表里的配置**改变运行时行为，但不改变模型认知**：改了视觉模型、加了数据库
//! 别名、接了 MCP，模型一概不知——它只能按提示词里写死的常识去猜，于是出现
//! 「问它用的什么模型只能瞎猜」「db_query 明明支持别名却每次都要用户再报一遍带密码的连接串」。
//!
//! ## 三条硬原则
//! 1. **只暴露会影响决策的项**：当前模型/降级链、视觉开关、数据库别名、MCP、RAG 目录、
//!    浏览器路径。音色、通知、夜间摘要水位线这类「不影响它怎么做事」的不进提示词，
//!    只在模型主动调 `config_get` 时才给。
//! 2. **注入文本控制在 ~200 token**：绝不把整张 settings 表塞进 system（那是 token 灾难，
//!    而且绝大多数项每轮都用不上）。
//! 3. **密钥/令牌/连接串永不进入模型上下文**：`redact()` 是硬边界，按字段名匹配脱敏，
//!    数据库连接只给名字不给串。
//!
//! ## 刷新时机
//! 每轮对话开始时 `refresh()` 一次（几个 get_setting，微秒级），保证注入的一定是最新配置；
//! 工具 description 读的是同一份缓存快照，不额外查库。

use std::sync::{OnceLock, RwLock};

use serde_json::Value;

use crate::memory::MemoryStore;
use crate::model::{ModelConfig, ModelTier};

/// 从 settings 汇总出的「模型该知道的配置」
#[derive(Debug, Clone, Default)]
pub struct ConfigView {
    /// 当前实际调用的端点，如「DeepSeek · deepseek-flash（云端）」
    pub model_label: String,
    /// 失败降级链，如「DeepSeek → 本地 Ollama」
    pub model_chain: String,
    /// 视觉能力描述；空串表示未启用
    pub vision: String,
    /// 数据库连接别名（只列名，不含连接串）
    pub db_aliases: Vec<String>,
    /// MCP 是否启用
    pub mcp_enabled: bool,
    /// MCP 启动命令（不含 args 里的令牌）
    pub mcp_command: String,
    /// RAG 已索引并监听的目录
    pub rag_dir: String,
    /// 受控浏览器的 Chrome 路径（空=未指定，用系统默认）
    pub chrome_path: String,
}

static SNAP: OnceLock<RwLock<ConfigView>> = OnceLock::new();

fn lock() -> &'static RwLock<ConfigView> {
    SNAP.get_or_init(|| RwLock::new(ConfigView::default()))
}

/// 取当前快照（读缓存，不查库）
pub fn snapshot() -> ConfigView {
    lock().read().map(|g| g.clone()).unwrap_or_default()
}

fn get(store: &MemoryStore, key: &str) -> Option<String> {
    store.get_setting(key).ok().flatten()
}

/// 重新读取 settings 并重建快照。每轮对话开始时调用一次即可。
pub fn refresh(store: &MemoryStore) {
    let mut v = ConfigView::default();

    // ---- 当前模型：直接读 model_config，取 active profile ----
    let mut profile_multimodal = false;
    if let Some(json) = get(store, "model_config") {
        if let Ok(cfg) = serde_json::from_str::<ModelConfig>(&json) {
            let active = cfg.active_id();
            let profiles = cfg.effective_profiles();
            if let Some(p) = profiles.iter().find(|p| p.id == active) {
                let tier = if p.tier == ModelTier::Local {
                    "本地"
                } else {
                    "云端"
                };
                v.model_label = format!("{} · {}（{}）", p.name, p.model, tier);
                profile_multimodal = p.multimodal;
            } else if !profiles.is_empty() {
                v.model_label = "未选定（按优先级自动挑选）".to_string();
            }
            v.model_chain = cfg.chain_label();
        }
    }

    // ---- 视觉：主模型多模态 > 独立视觉模型 ----
    if profile_multimodal || crate::visual_grounding::multimodal_main().is_some() {
        v.vision = "主模型自身多模态，视觉调用由它自己完成".to_string();
    } else {
        // 以持久化设置为准、全局运行时变量兜底（与 commands::get_runtime_config 同一套取值顺序）：
        // 设置里明说了开关，就不该被进程内默认值盖过去
        let enabled = get(store, "vision_enabled")
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or_else(crate::visual_grounding::vision_enabled);
        if enabled {
            let provider = get(store, "vision_provider").unwrap_or_else(|| "ollama".to_string());
            let model =
                get(store, "vision_model").unwrap_or_else(crate::visual_grounding::vision_model);
            v.vision = format!("{model}（{provider}）");
        }
    }

    // ---- 数据库别名：只取名字，绝不把连接串（含密码）带进上下文 ----
    if let Some(json) = get(store, "db_connections") {
        if let Ok(list) = serde_json::from_str::<Vec<Value>>(&json) {
            v.db_aliases = list
                .iter()
                .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
                .collect();
        }
    }

    // ---- MCP ----
    if let Some(json) = get(store, "mcp_config") {
        if let Ok(m) = serde_json::from_str::<crate::mcp::McpConfig>(&json) {
            v.mcp_enabled = m.enabled;
            v.mcp_command = m.command.clone();
        }
    }

    v.rag_dir = get(store, "rag_watch_dir").unwrap_or_default();
    v.chrome_path = get(store, "browser_chrome_path").unwrap_or_default();

    if let Ok(mut g) = lock().write() {
        *g = v;
    }
}

/// 生成注入 system 的摘要文本（~200 token）。没有任何可说的内容时返回空串，避免空占 token。
pub fn summary() -> String {
    let v = snapshot();
    let mut lines: Vec<String> = Vec::new();

    if !v.model_label.is_empty() {
        let mut s = format!("- 当前接入的模型端点：{}", v.model_label);
        if !v.model_chain.is_empty() {
            s.push_str(&format!("；失败降级链：{}", v.model_chain));
        }
        lines.push(s);
    }
    lines.push(format!(
        "- 视觉能力：{}",
        if !v.vision.is_empty() {
            format!("已启用（{}）", v.vision)
        } else {
            "未启用".to_string()
        }
    ));
    if v.db_aliases.is_empty() {
        lines.push("- 数据库连接：未配置任何别名，db_query 等需要完整连接串".to_string());
    } else {
        lines.push(format!(
            "- 数据库连接别名（db_query/db_schema/db_execute 的 connection 可直接填这些名字）：{}",
            v.db_aliases.join("、")
        ));
    }
    lines.push(format!(
        "- MCP 工具服务器：{}",
        if v.mcp_enabled {
            format!("已启用（{}）", v.mcp_command)
        } else {
            "未启用".to_string()
        }
    ));
    lines.push(format!(
        "- 知识库已索引目录：{}",
        if v.rag_dir.is_empty() {
            "无（rag_search 前需先 rag_index）".to_string()
        } else {
            v.rag_dir.clone()
        }
    ));
    if !v.chrome_path.is_empty() {
        lines.push(format!("- 受控浏览器：{}", v.chrome_path));
    }

    if lines.is_empty() {
        return String::new();
    }
    format!(
        "\n\n【当前配置摘要（系统已为你读好，不要再凭常识猜）】\n{}\n\
         （「模型端点」只是白泽当前接入的接口，不改变你是白泽这一身份；\
         想看更多配置项用 config_get 工具查询；密钥/令牌/连接串一律不展示，\
         也不要因为拿不到就向用户索要——让用户自己在设置里填。）",
        lines.join("\n")
    )
}

// ---------------------------------------------------------------------------
// config_get：按需详查（脱敏）
// ---------------------------------------------------------------------------

/// 字段名里出现这些片段即视为敏感，值一律替换为占位串。
///
/// `webhook` 单独列出：企业微信/飞书机器人的 webhook 地址本身就是凭证
/// （形如 `...send?key=xxxx`），字段名却不含 key/token，按名匹配会漏掉。
const SENSITIVE_HINTS: &[&str] = &[
    "key", "token", "secret", "password", "passwd", "pwd", "auth", "credential", "cookie",
    "signature", "connection", "webhook",
];

fn is_sensitive(key: &str) -> bool {
    let k = key.to_lowercase();
    SENSITIVE_HINTS.iter().any(|s| k.contains(s))
}

/// 递归脱敏：命中敏感字段名就替换值（保留「已配置」这个事实，不泄露内容）。
fn redact(v: &Value) -> Value {
    match v {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, val)| {
                    if is_sensitive(k) {
                        let s = val
                            .as_str()
                            .map(|s| !s.trim().is_empty())
                            .unwrap_or(!val.is_null());
                        (k.clone(), Value::from(if s { "***（已配置，不展示）" } else { "" }))
                    } else {
                        (k.clone(), redact(val))
                    }
                })
                .collect(),
        ),
        Value::Array(arr) => Value::Array(arr.iter().map(redact).collect()),
        other => other.clone(),
    }
}

/// 只读工具 `config_get`：让模型在需要时能自己查配置，而不是凭常识猜或反复问用户。
pub struct ConfigGetTool {
    store: std::sync::Arc<MemoryStore>,
}

impl ConfigGetTool {
    pub fn new(store: std::sync::Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl crate::tools::Tool for ConfigGetTool {
    fn name(&self) -> &str {
        "config_get"
    }
    fn description(&self) -> &str {
        "查询白泽自身的配置（settings 表）：省略 key 时列出全部可公开的配置项键名；\
         传 key 时返回该项的值。密钥/令牌/连接串一律以「***（已配置，不展示）」代替——\
         不要试图绕过它，也不要因为这些字段拿不到就向用户索要，让用户自己在设置里填。\
         常见键：mcp_config / notify_config / tts_config / voice / runtime_node / runtime_python / env_report"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "要查询的配置键名；省略则返回全部可公开键名列表"
                }
            }
        })
    }
    fn permission(&self) -> crate::tools::PermissionClass {
        crate::tools::PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let key = args.get("key").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        query(&self.store, key)
    }
}

/// 供 config_get 工具使用：不传 key 列全部键名；传 key 返回脱敏后的值。
pub fn query(store: &MemoryStore, key: Option<&str>) -> Result<Value, String> {
    match key {
        Some(k) => match store.get_setting(k) {
            Ok(Some(raw)) => {
                let parsed: Value = serde_json::from_str(&raw)
                    .unwrap_or_else(|_| Value::String(raw.clone()));
                Ok(redact(&parsed))
            }
            Ok(None) => Err(format!("配置项不存在：{k}")),
            Err(e) => Err(e),
        },
        None => {
            let mut keys: Vec<Value> = Vec::new();
            for k in store.all_setting_keys() {
                if is_sensitive(&k) {
                    continue;
                }
                // 草稿键是内部实现细节，对模型无意义
                if k.starts_with("chat_draft:") || k.starts_with("checkpoint:") {
                    continue;
                }
                keys.push(Value::String(k));
            }
            Ok(Value::Array(keys))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> MemoryStore {
        let dir = std::env::temp_dir().join(format!(
            "baize-configview-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        MemoryStore::open(path.to_string_lossy().as_ref()).expect("打开临时库失败")
    }

    /// 脱敏是硬边界：密钥/令牌/连接串绝不能进模型上下文
    #[test]
    fn redact_hides_secrets_but_keeps_fact() {
        let v = serde_json::json!({
            "provider": "doubao",
            "api_key": "sk-real-secret",
            "db_token": "tok-123",
            "nested": { "password": "pw", "model": "llava" }
        });
        let r = redact(&v);
        assert_eq!(r["provider"], "doubao");
        assert_eq!(r["nested"]["model"], "llava");
        assert_eq!(r["api_key"], "***（已配置，不展示）");
        assert_eq!(r["db_token"], "***（已配置，不展示）");
        assert_eq!(r["nested"]["password"], "***（已配置，不展示）");
    }

    /// 数据库别名只给名字，不给连接串（连接串带密码）
    #[test]
    fn db_aliases_never_leak_connection_string() {
        let store = temp_store("db");
        store
            .set_setting(
                "db_connections",
                r#"[{"name":"baize","connection":"mysql://root:682257yu@localhost:3306"}]"#,
            )
            .unwrap();

        refresh(&store);
        let v = snapshot();
        assert_eq!(v.db_aliases, vec!["baize".to_string()]);
        assert!(summary().contains("baize"));

        let got = query(&store, Some("db_connections")).unwrap();
        let s = got.to_string();
        assert!(!s.contains("682257yu"), "密码绝不能出现在返回值里：{s}");
        assert!(!s.contains("mysql://"), "连接串绝不能出现在返回值里：{s}");
    }

    /// 敏感键不出现在键名列表里；草稿/检查点这类内部实现细节也不列
    #[test]
    fn key_list_filters_secrets_and_internals() {
        let store = temp_store("keys");
        store.set_setting("voice", "Huihui").unwrap();
        store.set_setting("wechat_bot_token", "secret").unwrap();
        store.set_setting("chat_draft:abc", "x").unwrap();

        let list = query(&store, None).unwrap();
        let s = list.to_string();
        assert!(s.contains("voice"));
        assert!(!s.contains("wechat_bot_token"));
        assert!(!s.contains("chat_draft"));
    }

    /// 「什么都没配」时摘要不能崩，并且要明确告诉模型「需要完整连接串」「视觉没开」
    #[test]
    fn summary_says_unconfigured_when_empty() {
        let store = temp_store("empty");
        store.set_setting("vision_enabled", "false").unwrap();
        refresh(&store);
        let s = summary();
        assert!(s.contains("视觉能力：未启用"), "{s}");
        assert!(s.contains("未配置任何别名"), "{s}");
        assert!(s.contains("MCP 工具服务器：未启用"), "{s}");
        assert!(s.contains("无（rag_search 前需先 rag_index）"), "{s}");
    }

    /// 独立视觉模型：以持久化的 vision_* 为准，不被进程内默认值盖过去
    #[test]
    fn vision_reads_persisted_setting_first() {
        let store = temp_store("vision");
        store.set_setting("vision_enabled", "true").unwrap();
        store.set_setting("vision_model", "qwen2.5vl:7b").unwrap();
        store.set_setting("vision_provider", "ollama").unwrap();

        refresh(&store);
        assert_eq!(snapshot().vision, "qwen2.5vl:7b（ollama）");

        store.set_setting("vision_enabled", "false").unwrap();
        refresh(&store);
        assert!(snapshot().vision.is_empty());
        assert!(summary().contains("视觉能力：未启用"));
    }

    /// 当前模型身份能被正确读出——这是此前「问它用的什么模型只能瞎猜」的根因
    #[test]
    fn summary_reports_active_model() {
        let store = temp_store("model");
        let cfg = serde_json::json!({
            "local_enabled": true,
            "local_url": "http://127.0.0.1:11434",
            "local_model": "qwen2.5:7b",
            "cloud_enabled": true,
            "cloud_name": "DeepSeek",
            "cloud_base_url": "https://api.deepseek.com/v1",
            "cloud_api_key": "sk-should-never-leak",
            "cloud_model": "deepseek-chat",
            "priority": "local",
            "profiles": [
                {"id":"p1","name":"DeepSeek","tier":"cloud","kind":"openai",
                 "base_url":"https://api.deepseek.com/v1","api_key":"sk-should-never-leak",
                 "model":"deepseek-flash","enabled":true,"has_key":true,"multimodal":true},
                {"id":"local","name":"本地 Ollama","tier":"local","kind":"ollama",
                 "base_url":"http://127.0.0.1:11434","api_key":"","model":"qwen2.5:7b",
                 "enabled":true,"has_key":false,"multimodal":false}
            ],
            "active": "p1"
        });
        store.set_setting("model_config", &cfg.to_string()).unwrap();

        refresh(&store);
        let s = summary();
        assert!(s.contains("deepseek-flash"), "必须说出实际调用的端点：{s}");
        assert!(!s.contains("sk-should-never-leak"));
        // 当前是云端多模态主模型 → 视觉段应走「主模型自身多模态」
        assert!(s.contains("主模型自身多模态"), "{s}");
    }
}
