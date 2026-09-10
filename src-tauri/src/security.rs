use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::memory::MemoryStore;
use crate::tools::{PermissionClass, Tool};

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

// ---------------- 细粒度权限策略（PermissionPolicy） ----------------
//
// 在「工具权限级别（ReadOnly/Write/HighRisk）」之上叠加一层用户可配置的策略，
// 实现「允许读文件但禁止联网」这类一键放权场景。三层规则按序匹配，命中即生效：
//   1) tool_rules   按工具名精确匹配（最高优先），如 "http_request": "deny"
//   2) class_rules  按权限级别匹配，如 "HighRisk": "deny"
//   3) default      全局默认（allow / ask / deny）
// 未命中任何规则的调用回退到原有分类逻辑（ReadOnly 放行 / Write 视目录 / HighRisk 审批）。
// 持久化：SQLite settings 表 key = "permission_policy"。

/// 策略持久化键
const POLICY_KEY: &str = "permission_policy";

/// 一条规则的动作
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PolicyAction {
    /// 放行（无需审批）
    #[serde(rename = "allow")]
    Allow,
    /// 走原分类逻辑（ReadOnly 放行 / Write 视目录 / HighRisk 审批）
    #[serde(rename = "ask")]
    Ask,
    /// 直接拒绝
    #[serde(rename = "deny")]
    Deny,
}

/// 细粒度权限策略
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PermissionPolicy {
    /// 工具名 → 动作（最高优先）
    #[serde(default)]
    pub tool_rules: std::collections::BTreeMap<String, PolicyAction>,
    /// 权限级别（"ReadOnly" / "Write" / "HighRisk"）→ 动作
    #[serde(default)]
    pub class_rules: std::collections::BTreeMap<String, PolicyAction>,
    /// 全局默认动作；缺省 = ask（即走原分类逻辑）
    #[serde(default)]
    pub default: Option<PolicyAction>,
}

impl PermissionPolicy {
    /// 加载持久化策略（无 / 损坏时返回默认空策略）
    fn load(store: &MemoryStore) -> Self {
        match store.get_setting(POLICY_KEY) {
            Ok(Some(json)) => serde_json::from_str(&json).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    /// 决策一次工具调用；返回 None 表示无规则命中（继续走原分类逻辑）
    fn decide(&self, tool: &str, class: PermissionClass) -> Option<PolicyAction> {
        if let Some(a) = self.tool_rules.get(tool) {
            return Some(*a);
        }
        let class_key = match class {
            PermissionClass::ReadOnly => "ReadOnly",
            PermissionClass::Write => "Write",
            PermissionClass::HighRisk => "HighRisk",
        };
        if let Some(a) = self.class_rules.get(class_key) {
            return Some(*a);
        }
        self.default
    }
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
    /// 细粒度权限策略（工具名/级别/默认三层规则），RwLock 支持运行时修改
    policy: std::sync::RwLock<PermissionPolicy>,
    store: Arc<MemoryStore>,
}

impl SecurityManager {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            decisions: Mutex::new(HashMap::new()),
            remembered: Mutex::new(load_rules(&store)),
            policy: std::sync::RwLock::new(PermissionPolicy::load(&store)),
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

        // 细粒度策略层：工具名规则 > 权限级别规则 > 全局默认，命中即生效；
        // 未命中（None / Ask）回退到原有分类逻辑
        let policy_action = {
            let policy = self.policy.read().unwrap();
            policy.decide(tool, class)
        };
        match policy_action {
            Some(PolicyAction::Allow) => return PermissionDecision::AutoAllow,
            Some(PolicyAction::Deny) => return PermissionDecision::AutoDeny,
            _ => {}
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

    /// 注册一个外部构造的审批请求（如 plan_confirm 计划确认），进入统一审批链：
    /// 前端审批卡 / 消息中心 / IM 回复「允许」均可 resolve
    pub fn submit_request(&self, req: PermissionRequest) {
        self.pending.lock().unwrap().insert(req.id.clone(), req);
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

    /// 列出全部已记住的权限规则（白名单可视化）
    pub fn rules_list(&self) -> Vec<(String, bool)> {
        let map = self.remembered.lock().unwrap();
        let mut v: Vec<(String, bool)> = map.iter().map(|(k, b)| (k.clone(), *b)).collect();
        v.sort();
        v
    }

    /// 删除一条规则，返回是否存在
    pub fn rules_remove(&self, key: &str) -> bool {
        let snapshot = {
            let mut map = self.remembered.lock().unwrap();
            let existed = map.remove(key).is_some();
            if !existed {
                return false;
            }
            map.clone()
        };
        save_rules(&self.store, &snapshot);
        true
    }

    /// 直接添加/更新一条规则（手动白名单/黑名单；key 为工具名或「工具|参数指纹」）
    pub fn rules_set(&self, key: &str, allowed: bool) {
        let snapshot = {
            let mut map = self.remembered.lock().unwrap();
            map.insert(key.to_string(), allowed);
            map.clone()
        };
        save_rules(&self.store, &snapshot);
    }

    // ---------------- 细粒度权限策略（PermissionPolicy）管理 ----------------

    /// 读取当前策略快照
    pub fn policy_get(&self) -> PermissionPolicy {
        self.policy.read().unwrap().clone()
    }

    /// 整体替换策略并持久化（前端「一键放权」配置面板 / Agent 工具都会走这里）
    pub fn policy_set(&self, policy: PermissionPolicy) -> Result<(), String> {
        let json = {
            let mut guard = self.policy.write().unwrap();
            *guard = policy;
            serde_json::to_string(&*guard).map_err(|e| format!("策略序列化失败: {e}"))?
        };
        self.store.set_setting(POLICY_KEY, &json)
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

#[cfg(test)]
mod policy_tests {
    use super::*;
    use serde_json::json;

    fn parse_action(s: &str) -> PolicyAction {
        serde_json::from_value(json!(s)).unwrap()
    }

    #[test]
    fn policy_action_serde() {
        assert_eq!(parse_action("allow"), PolicyAction::Allow);
        assert_eq!(parse_action("ask"), PolicyAction::Ask);
        assert_eq!(parse_action("deny"), PolicyAction::Deny);
    }

    #[test]
    fn decide_tool_rule_wins_over_class_rule() {
        let p = PermissionPolicy {
            tool_rules: [("http_request".to_string(), PolicyAction::Allow)]
                .into_iter()
                .collect(),
            class_rules: [("HighRisk".to_string(), PolicyAction::Deny)]
                .into_iter()
                .collect(),
            default: None,
        };
        // 工具级 allow 优先于级别 deny
        assert_eq!(
            p.decide("http_request", PermissionClass::HighRisk),
            Some(PolicyAction::Allow)
        );
        // 其它 HighRisk 工具被级别规则拒绝
        assert_eq!(
            p.decide("run_command", PermissionClass::HighRisk),
            Some(PolicyAction::Deny)
        );
        // 无命中的只读工具无规则 → None（走原分类）
        assert_eq!(p.decide("list_files", PermissionClass::ReadOnly), None);
    }

    #[test]
    fn decide_default_applies_when_no_rules() {
        let p = PermissionPolicy {
            tool_rules: Default::default(),
            class_rules: Default::default(),
            default: Some(PolicyAction::Deny),
        };
        assert_eq!(
            p.decide("anything", PermissionClass::Write),
            Some(PolicyAction::Deny)
        );
    }
}

// ---------------- 细粒度权限策略：Agent 工具 ----------------

/// 查询当前细粒度权限策略（只读）
pub struct PolicyGetTool {
    store: Arc<MemoryStore>,
}

impl PolicyGetTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for PolicyGetTool {
    fn name(&self) -> &str {
        "permission_policy_get"
    }
    fn description(&self) -> &str {
        "查询当前的细粒度权限策略：tool_rules（按工具名）/ class_rules（按权限级别 ReadOnly/Write/HighRisk）/ default，\
         取值 allow（放行）/ ask（走原审批逻辑）/ deny（拒绝）"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let policy = PermissionPolicy::load(&self.store);
        serde_json::to_value(policy).map_err(|e| e.to_string())
    }
}

/// 修改细粒度权限策略（需要审批）：实现「允许读文件但禁止联网」类一键放权。
/// 传整体策略对象（tool_rules / class_rules / default），传空对象等价于清空对应层；
/// "clear": true 一键恢复默认（全部走原审批逻辑）。
pub struct PolicySetTool {
    store: Arc<MemoryStore>,
}

impl PolicySetTool {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl Tool for PolicySetTool {
    fn name(&self) -> &str {
        "permission_policy_set"
    }
    fn description(&self) -> &str {
        "修改细粒度权限策略（高危，需用户审批）。参数为完整策略：\
         { \"tool_rules\": {\"http_request\": \"deny\"}, \"class_rules\": {\"ReadOnly\": \"allow\"}, \"default\": \"ask\", \"clear\": false }。\
         典型用法：「允许读文件但禁止联网」= class_rules.ReadOnly=allow + tool_rules.http_request/web_search=deny"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "tool_rules": {
                    "type": "object",
                    "description": "工具名 → allow/ask/deny，如 {\"http_request\": \"deny\"}",
                    "additionalProperties": { "type": "string", "enum": ["allow", "ask", "deny"] }
                },
                "class_rules": {
                    "type": "object",
                    "description": "权限级别（ReadOnly/Write/HighRisk）→ allow/ask/deny",
                    "additionalProperties": { "type": "string", "enum": ["allow", "ask", "deny"] }
                },
                "default": { "type": "string", "enum": ["allow", "ask", "deny"], "description": "全局默认动作" },
                "clear": { "type": "boolean", "description": "true 时清空全部策略，恢复原审批逻辑" }
            }
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::HighRisk
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        if args["clear"].as_bool().unwrap_or(false) {
            let empty = PermissionPolicy::default();
            let json = serde_json::to_string(&empty).map_err(|e| e.to_string())?;
            self.store.set_setting(POLICY_KEY, &json)?;
            return Ok(json!({ "ok": true, "cleared": true, "policy": empty }));
        }
        // 允许传部分层：未传的层沿用当前持久化策略
        let mut policy = PermissionPolicy::load(&self.store);
        if let Some(obj) = args["tool_rules"].as_object() {
            let mut map = std::collections::BTreeMap::new();
            for (k, v) in obj {
                let action: PolicyAction = serde_json::from_value(v.clone())
                    .map_err(|_| format!("tool_rules.{k} 的取值应为 allow/ask/deny"))?;
                map.insert(k.clone(), action);
            }
            policy.tool_rules = map;
        }
        if let Some(obj) = args["class_rules"].as_object() {
            let mut map = std::collections::BTreeMap::new();
            for (k, v) in obj {
                if !matches!(k.as_str(), "ReadOnly" | "Write" | "HighRisk") {
                    return Err(format!("class_rules 的键应为 ReadOnly/Write/HighRisk，收到 {k}"));
                }
                let action: PolicyAction = serde_json::from_value(v.clone())
                    .map_err(|_| format!("class_rules.{k} 的取值应为 allow/ask/deny"))?;
                map.insert(k.clone(), action);
            }
            policy.class_rules = map;
        }
        if !args["default"].is_null() {
            let action: PolicyAction = serde_json::from_value(args["default"].clone())
                .map_err(|_| "default 的取值应为 allow/ask/deny".to_string())?;
            policy.default = Some(action);
        }
        let json = serde_json::to_string(&policy).map_err(|e| e.to_string())?;
        self.store.set_setting(POLICY_KEY, &json)?;
        Ok(json!({ "ok": true, "policy": policy }))
    }
}