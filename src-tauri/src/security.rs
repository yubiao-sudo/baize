use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::memory::MemoryStore;
use crate::tools::PermissionClass;

/// 记住的权限规则持久化键
const RULES_KEY: &str = "permission_rules";

/// 旧版「按工具名整体记住」留下的宽泛死键：新版已细化到「工具 + 具体情况」，
/// 这些纯工具名键不再被复用，启动时显式清理，以撤销旧的宽泛拒绝/允许。
const LEGACY_BROAD_KEYS: &[&str] = &["software_install", "software_uninstall"];

/// 一次权限请求：把「真实工具调用载荷」展示给用户，防止 Lies-in-the-loop
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    pub tool: String,
    pub args: Value,
    pub class: PermissionClass,
    /// 富信息（可选）：安装软件时附带目标位置/推荐理由/软件名，供前端渲染更友好的确认卡
    pub detail: Option<Value>,
}

/// 权限决策结果
pub enum PermissionDecision {
    /// 直接放行（只读 / 一般读写 / 已记住允许）
    AutoAllow,
    /// 直接拒绝（已记住拒绝）
    AutoDeny,
    /// 需要用户审批
    Prompt(PermissionRequest),
}

/// 权限管理器：pending = 待审批，decisions = 已决策，remembered = 已记住的规则
///
/// 策略：
///   1) ReadOnly 直接放行；
///   2) Write 仅当触及系统目录（系统文件/设置）时审批，普通工作文件读写直接放行；
///   3) HighRisk（Shell / 终端 / Computer Use）始终审批；
///   4) 用户「记住」的决策优先于上述规则，记住后同类命令直接执行或拒绝。
pub struct SecurityManager {
    pending: Mutex<HashMap<String, PermissionRequest>>,
    decisions: Mutex<HashMap<String, bool>>,
    remembered: Mutex<HashMap<String, bool>>,
    store: Arc<MemoryStore>,
}

impl SecurityManager {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            decisions: Mutex::new(HashMap::new()),
            remembered: Mutex::new(load_rules(&store)),
            store,
        }
    }

    /// 判断某次工具调用是否需要审批（见上面策略说明）
    pub fn classify(&self, tool: &str, args: &Value, class: PermissionClass) -> PermissionDecision {
        // 已记住的规则优先级最高；key 按「工具 + 具体情况」区分，
        // 让「记住拒绝/允许」只作用于相同具体情况，不同软件/不同盘符会重新审批
        let key = context_key(tool, args);
        if let Some(allowed) = self.remembered_rule(&key) {
            return if allowed {
                PermissionDecision::AutoAllow
            } else {
                PermissionDecision::AutoDeny
            };
        }

        match class {
            PermissionClass::ReadOnly => PermissionDecision::AutoAllow,
            PermissionClass::HighRisk => {
                PermissionDecision::Prompt(self.new_request(tool, args, class))
            }
            PermissionClass::Write => {
                // 仅当写目标位于系统目录时才需要确认，普通读写直接放行
                let touches_system = tool_paths(tool, args).iter().any(|p| is_system_path(p));
                if touches_system {
                    PermissionDecision::Prompt(self.new_request(tool, args, class))
                } else {
                    PermissionDecision::AutoAllow
                }
            }
        }
    }

    fn new_request(&self, tool: &str, args: &Value, class: PermissionClass) -> PermissionRequest {
        let id = uuid::Uuid::new_v4().to_string();
        // 安装软件时附带富信息（目标盘 + 推荐理由 + 软件名），前端据此渲染专用确认卡
        let detail = match tool {
            "software_install" => Some(crate::software::install_preview(args)),
            _ => None,
        };
        let req = PermissionRequest {
            id: id.clone(),
            tool: tool.to_string(),
            args: args.clone(),
            class,
            detail,
        };
        self.pending.lock().unwrap().insert(id, req.clone());
        req
    }

    pub fn pending(&self) -> Vec<PermissionRequest> {
        self.pending.lock().unwrap().values().cloned().collect()
    }

    pub fn pending_by_id(&self, id: &str) -> Option<PermissionRequest> {
        self.pending.lock().unwrap().get(id).cloned()
    }

    pub fn resolve(&self, id: &str, approved: bool) -> bool {
        self.decisions.lock().unwrap().insert(id.to_string(), approved);
        self.pending.lock().unwrap().remove(id).is_some()
    }

    pub fn decision(&self, id: &str) -> Option<bool> {
        self.decisions.lock().unwrap().get(id).copied()
    }

    pub fn remembered_rule(&self, key: &str) -> Option<bool> {
        self.remembered.lock().unwrap().get(key).copied()
    }

    /// 记住某次操作的权限决定（key 为「工具 + 具体情况」），并持久化到 SQLite
    pub fn remember(&self, tool: &str, args: &Value, allowed: bool) {
        let key = context_key(tool, args);
        let snapshot = {
            let mut map = self.remembered.lock().unwrap();
            map.insert(key, allowed);
            map.clone()
        };
        save_rules(&self.store, &snapshot);
    }
}

/// 计算权限记忆的「情况指纹」：让「记住」只作用于相同具体情况。
/// 软件管家类工具细化到「软件 + 目标盘」，不同软件/不同盘符独立记忆、互不覆盖；
/// 其余工具回退到工具名整体记忆（保持原有行为）。
fn context_key(tool: &str, args: &Value) -> String {
    match tool {
        "software_install" => {
            let id = args["id"].as_str().unwrap_or("").to_string();
            let drive = crate::software::install_preview(args)["drive"]
                .as_str()
                .unwrap_or("")
                .to_string();
            format!("software_install|id={id}|drive={drive}")
        }
        "software_uninstall" => {
            let id = args["id"].as_str().unwrap_or("").to_string();
            format!("software_uninstall|id={id}")
        }
        _ => tool.to_string(),
    }
}

/// 从 SQLite 加载已记住的权限规则，并清理旧版「宽泛工具名」死键（见 LEGACY_BROAD_KEYS）
fn load_rules(store: &MemoryStore) -> HashMap<String, bool> {
    let mut rules: HashMap<String, bool> = match store.get_setting(RULES_KEY) {
        Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_default(),
        _ => HashMap::new(),
    };
    let before = rules.len();
    for key in LEGACY_BROAD_KEYS {
        rules.remove(*key);
    }
    // 清理后回写持久化，避免死键常驻 SQLite
    if rules.len() != before {
        save_rules(store, &rules);
    }
    rules
}

/// 持久化权限规则为 JSON
fn save_rules(store: &MemoryStore, rules: &HashMap<String, bool>) {
    if let Ok(json) = serde_json::to_string(rules) {
        let _ = store.set_setting(RULES_KEY, &json);
    }
}

/// 提取工具调用涉及的目标路径（用于判断是否触及系统目录）
fn tool_paths(tool: &str, args: &Value) -> Vec<String> {
    match tool {
        "write_file" | "edit_file" | "create_directory" | "csv_write" | "xlsx_write" => {
            args["path"].as_str().map(|s| s.to_string()).into_iter().collect()
        }
        "move_file" => ["from", "to"]
            .iter()
            .filter_map(|k| args[*k].as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// 判断路径是否属于受保护的系统目录（Windows / Unix / macOS 常见系统根）
fn is_system_path(p: &str) -> bool {
    let norm = p.trim().to_lowercase().replace('\\', "/");
    // 去掉 Windows 盘符（如 c:）
    let stripped = if norm.len() >= 2 && norm.as_bytes()[1] == b':' {
        &norm[2..]
    } else {
        norm.as_str()
    };
    const ROOTS: &[&str] = &[
        "/windows",
        "/program files",
        "/program files (x86)",
        "/programdata",
        "/etc",
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/boot",
        "/var",
        "/opt",
        "/root",
        "/system",
        "/library",
        "/applications",
    ];
    ROOTS
        .iter()
        .any(|r| stripped == *r || stripped.starts_with(&format!("{r}/")))
}

/// 审计条目（不可删、可回放）；持久化到 SQLite 由 memory::MemoryStore 负责
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub ts: u128,
    pub subject: String,
    pub tool: String,
    pub args: Value,
    pub decision: String, // auto-allow / approved / denied / timeout
    pub result: String,
}