//! 技能学习（Skill Learning）
//!
//! 对应《白泽自主进化》功能五：一次演示，永久会用。
//! 白泽把操作过程归纳成「带参数的技能 DSL」持久化到技能库，下次一句话即可触发，实现「自学进化」。
//!
//! 五模块闭环：
//!   录制（操作序列）→ 归纳（参数化 DSL）→ 存库（skills 表）→ 触发（模糊匹配 / 直接命名）→ 反馈（质量分回写）
//!
//! 工具集：
//!   - skill_list   : 列出技能库（内置 + 已学习）
//!   - skill_get    : 查看单个技能详情（触发词 / 参数 / 步骤）
//!   - skill_learn  : 把演示归纳成技能 DSL 并写入技能库（录制 + 归纳 + 存库）
//!   - skill_run    : 按名触发执行，参数绑定，逐步骤调用工具，成功后回写质量分
//!   - skill_delete : 删除一个技能
//!
//! 技能 DSL 示例（skill_learn 的入参）：
//!   {
//!     "name": "整理下载目录",
//!     "triggers": ["整理下载", "归档下载"],
//!     "params": [{"name":"dir","default":"D:/Downloads","type":"path","description":"要整理的目录"}],
//!     "steps": [
//!       {"tool":"list_files","args":{"path":"{dir}"},"description":"列出目录内容"},
//!       {"tool":"notify","args":{"body":"已整理 {dir}"},"description":"通知完成"}
//!     ]
//!   }

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::memory::MemoryStore;
use crate::task::{emit_todo_list, emit_todo_update, Todo};
use crate::tools::{PermissionClass, Tool, ToolRegistry};

// ───────────────── 数据结构：技能 DSL ─────────────────

/// 技能参数（参数化后，同一技能可复用在不同路径/目标上）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillParam {
    pub name: String,
    /// 默认值（触发时未提供则用默认）
    #[serde(default)]
    pub default: String,
    /// path | string | number | bool
    #[serde(default = "default_param_type")]
    pub param_type: String,
    pub description: String,
}

fn default_param_type() -> String {
    "string".into()
}

/// 技能步骤：调用某个工具（args 支持 {param} 变量绑定）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillStep {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
    pub description: String,
}

/// 技能定义（可持久化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// 触发词（用户说这些词即可模糊匹配命中）
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub params: Vec<SkillParam>,
    #[serde(default)]
    pub steps: Vec<SkillStep>,
    /// 执行指导（供 LLM 在无法用工具精确执行时参考）
    #[serde(default)]
    pub prompt: String,
    /// 质量分 0~1（执行成功回写提升，失败回写下降）
    #[serde(default)]
    pub quality: f64,
    #[serde(default)]
    pub run_count: i64,
    #[serde(default)]
    pub success_count: i64,
}

// ───────────────── 内置技能库 ─────────────────

pub fn builtin_skills() -> Vec<Skill> {
    vec![
        Skill {
            name: "research_report".into(),
            description: "调研一个主题并生成 markdown 报告（搜索→整理→写文档）".into(),
            triggers: vec!["调研报告".into(), "写个调研".into(), "研究一下".into()],
            params: vec![SkillParam {
                name: "topic".into(),
                default: String::new(),
                param_type: "string".into(),
                description: "调研主题".into(),
            }],
            steps: vec![],
            prompt: "执行「调研报告」工作流：先用 browser_search 搜索主题 {topic}，再整理关键信息，最后用 markdown_set 写一份结构化 markdown 报告。".into(),
            quality: 0.8,
            run_count: 0,
            success_count: 0,
        },
        Skill {
            name: "file_organize".into(),
            description: "查看目录结构并给出整理建议".into(),
            triggers: vec!["整理文件".into(), "文件归整".into(), "目录太乱".into()],
            params: vec![SkillParam {
                name: "dir".into(),
                default: ".".into(),
                param_type: "path".into(),
                description: "要查看的目录".into(),
            }],
            steps: vec![],
            prompt: "执行「文件整理」工作流：先用 list_files 查看目录 {dir}，再分析文件类型与结构，最后给出整理建议。".into(),
            quality: 0.7,
            run_count: 0,
            success_count: 0,
        },
    ]
}

// ───────────────── 技能库（持久化） ─────────────────

/// 技能库：内置技能 + 已学习技能（持久化到 skills 表）
pub struct SkillLibrary {
    store: Arc<MemoryStore>,
}

impl SkillLibrary {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }

    /// 列出全部技能：内置技能优先，再叠加数据库中已学习的技能（同名覆盖内置）
    pub fn all(&self) -> Vec<Skill> {
        let mut map: HashMap<String, Skill> = HashMap::new();
        for s in builtin_skills() {
            map.insert(s.name.clone(), s);
        }
        match self.store.list_skills() {
            Ok(rows) => {
                for (name, data) in rows {
                    if let Ok(skill) = serde_json::from_str::<Skill>(&data) {
                        map.insert(name, skill);
                    }
                }
            }
            Err(e) => eprintln!("[技能] 加载失败: {e}"),
        }
        let mut list: Vec<Skill> = map.into_values().collect();
        list.sort_by(|a, b| {
            b.quality
                .partial_cmp(&a.quality)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        list
    }

    /// 按名查找（先在数据库查，再回退内置）
    pub fn get(&self, name: &str) -> Option<Skill> {
        if let Ok(rows) = self.store.list_skills() {
            for (n, data) in rows {
                if n == name {
                    return serde_json::from_str::<Skill>(&data).ok();
                }
            }
        }
        builtin_skills().into_iter().find(|s| s.name == name)
    }

    /// 保存（写入数据库）
    pub fn save(&self, skill: &Skill) -> Result<(), String> {
        let data = serde_json::to_string(skill).map_err(|e| e.to_string())?;
        self.store.upsert_skill(&skill.name, &data)
    }

    /// 删除（返回是否删除成功）
    pub fn delete(&self, name: &str) -> bool {
        self.store.delete_skill(name).unwrap_or(false)
    }

    /// 模糊匹配：用户的自然语言里是否包含某技能的触发词
    pub fn match_trigger(&self, text: &str) -> Option<Skill> {
        let lower = text.to_lowercase();
        let mut best: Option<Skill> = None;
        let mut best_len = 0usize;
        for s in self.all() {
            for t in &s.triggers {
                let tl = t.to_lowercase();
                if !tl.is_empty() && lower.contains(&tl) && tl.chars().count() > best_len {
                    best_len = tl.chars().count();
                    best = Some(s.clone());
                }
            }
        }
        best
    }

    /// 反馈闭环：记录一次执行结果，回写质量分（滑动平均逼近成功/失败）
    pub fn record_run(&self, name: &str, success: bool) {
        let Some(mut s) = self.get(name) else {
            return;
        };
        s.run_count += 1;
        if success {
            s.success_count += 1;
        }
        let target = if success { 1.0 } else { 0.0 };
        // 质量分朝本次结果滑动（学习率 0.2），避免单次波动过大
        s.quality = (s.quality * 0.8 + target * 0.2).clamp(0.0, 1.0);
        let _ = self.save(&s);
    }
}

// ───────────────── 变量绑定 ─────────────────

/// 递归把 JSON 字符串里的 {var} 占位符替换为变量值
fn bind_json(v: &Value, vars: &HashMap<String, String>) -> Value {
    match v {
        Value::String(s) => {
            let mut out = s.clone();
            for (k, val) in vars {
                out = out.replace(&format!("{{{k}}}"), val);
            }
            Value::String(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| bind_json(x, vars)).collect()),
        Value::Object(o) => {
            let mut m = serde_json::Map::new();
            for (k, val) in o {
                m.insert(k.clone(), bind_json(val, vars));
            }
            Value::Object(m)
        }
        other => other.clone(),
    }
}

// ───────────────── 工具集 ─────────────────

/// skill_list：列出技能库
pub struct SkillListTool {
    lib: Arc<SkillLibrary>,
}

impl SkillListTool {
    pub fn new(lib: Arc<SkillLibrary>) -> Self {
        Self { lib }
    }
}

impl Tool for SkillListTool {
    fn name(&self) -> &str {
        "skill_list"
    }
    fn description(&self) -> &str {
        "列出白泽技能库中全部可复用技能（内置 + 已学习），每个技能含 name、description、触发词 triggers、质量分 quality。用于处理常见复合任务，或用 skill_run 按名触发"
    }
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, _args: Value) -> Result<Value, String> {
        let arr: Vec<Value> = self
            .lib
            .all()
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "description": s.description,
                    "triggers": s.triggers,
                    "params": s.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
                    "quality": s.quality,
                    "run_count": s.run_count,
                })
            })
            .collect();
        Ok(json!(arr))
    }
}

/// skill_get：查看技能详情
pub struct SkillGetTool {
    lib: Arc<SkillLibrary>,
}

impl SkillGetTool {
    pub fn new(lib: Arc<SkillLibrary>) -> Self {
        Self { lib }
    }
}

impl Tool for SkillGetTool {
    fn name(&self) -> &str {
        "skill_get"
    }
    fn description(&self) -> &str {
        "查看某个技能的完整定义（触发词、参数、步骤、质量分），用于执行前了解该技能会做什么。name 见 skill_list"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "技能名" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?;
        let skill = self
            .lib
            .get(name)
            .ok_or_else(|| format!("未知技能: {name}，请用 skill_list 查看"))?;
        Ok(json!({
            "name": skill.name,
            "description": skill.description,
            "triggers": skill.triggers,
            "params": skill.params.iter().map(|p| json!({
                "name": p.name, "default": p.default, "type": p.param_type, "description": p.description
            })).collect::<Vec<_>>(),
            "steps": skill.steps.iter().map(|s| json!({
                "tool": s.tool, "args": s.args, "description": s.description
            })).collect::<Vec<_>>(),
            "quality": skill.quality,
            "run_count": skill.run_count,
            "success_count": skill.success_count,
        }))
    }
}

/// skill_learn：录制 + 归纳 + 存库（把演示抽象为带参数的技能 DSL）
pub struct SkillLearnTool {
    lib: Arc<SkillLibrary>,
}

impl SkillLearnTool {
    pub fn new(lib: Arc<SkillLibrary>) -> Self {
        Self { lib }
    }
}

impl Tool for SkillLearnTool {
    fn name(&self) -> &str {
        "skill_learn"
    }
    fn description(&self) -> &str {
        "学习一个新技能：把刚刚演示/归纳出的操作流程抽象成带参数的技能 DSL 并存入技能库，下次一句话即可复用。name 为技能名；triggers 为触发词数组（用户说这些词命中）；params 为参数数组（每项 {name, default, type, description}，default 可省略）；steps 为步骤数组（每项 {tool, description, args}，args 中用 {参数名} 占位符绑定变量）。步骤 tool 可为任意工具名或 notify"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "技能名（唯一）" },
                "description": { "type": "string", "description": "技能用途说明" },
                "triggers": { "type": "array", "items": { "type": "string" }, "description": "触发词，用户说这些词即可命中" },
                "params": {
                    "type": "array",
                    "items": { "type": "object" },
                    "description": "参数列表，每项 {name, default, type, description}"
                },
                "steps": {
                    "type": "array",
                    "items": { "type": "object" },
                    "description": "步骤列表，每项 {tool, description, args}"
                },
                "prompt": { "type": "string", "description": "可选：执行指导文本（供无法用工具精确执行时参考）" }
            },
            "required": ["name", "description", "steps"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?.trim().to_string();
        if name.is_empty() {
            return Err("技能名不能为空".into());
        }
        let description = args["description"].as_str().unwrap_or("").to_string();

        let triggers: Vec<String> = args
            .get("triggers")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let params: Vec<SkillParam> = args
            .get("params")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        Some(SkillParam {
                            name: p.get("name")?.as_str()?.to_string(),
                            default: p.get("default").and_then(|d| d.as_str()).unwrap_or("").to_string(),
                            param_type: p.get("type").and_then(|d| d.as_str()).unwrap_or("string").to_string(),
                            description: p.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let steps: Vec<SkillStep> = args
            .get("steps")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| {
                        Some(SkillStep {
                            tool: s.get("tool")?.as_str()?.to_string(),
                            args: s.get("args").cloned().unwrap_or_else(|| json!({})),
                            description: s.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        if steps.is_empty() {
            return Err("至少需要一个步骤".into());
        }

        let skill = Skill {
            name: name.clone(),
            description,
            triggers,
            params,
            steps,
            prompt: args["prompt"].as_str().unwrap_or("").to_string(),
            quality: 0.5,
            run_count: 0,
            success_count: 0,
        };
        self.lib.save(&skill)?;
        Ok(json!({ "ok": true, "name": name, "steps": skill.steps.len() }))
    }
}

/// skill_run：按名触发执行技能（参数绑定 + 逐步骤调用工具 + 反馈回写）
pub struct SkillRunTool {
    app: AppHandle,
    lib: Arc<SkillLibrary>,
    tools: Arc<ToolRegistry>,
    todos: Arc<Mutex<Vec<Todo>>>,
}

impl SkillRunTool {
    pub fn new(
        app: AppHandle,
        lib: Arc<SkillLibrary>,
        tools: Arc<ToolRegistry>,
        todos: Arc<Mutex<Vec<Todo>>>,
    ) -> Self {
        Self {
            app,
            lib,
            tools,
            todos,
        }
    }
}

impl Tool for SkillRunTool {
    fn name(&self) -> &str {
        "skill_run"
    }
    fn description(&self) -> &str {
        "触发执行一个技能（先 skill_list 查看可用技能，也可直接用技能名或触发词/自然语言描述触发，白泽会精确匹配 name、命中后回退触发词模糊匹配）。参数：name 必填；params 为 {参数名: 值} 对象；也可直接把参数名作为顶层字段传入。白泽会按技能 DSL 逐步骤调用工具执行，成功后自动回写质量分"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "技能名（见 skill_list）" },
                "params": { "type": "object", "description": "技能参数（键值对，覆盖默认值）" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?.to_string();
        let skill = self
            .lib
            .get(&name)
            .or_else(|| self.lib.match_trigger(&name))
            .ok_or_else(|| format!("未知技能: {name}，请用 skill_list 查看可用技能"))?;

        // 1) 构建参数绑定表：优先 args["params"] 对象，其次顶层同名字段，最后默认值
        let mut vars: HashMap<String, String> = HashMap::new();
        for p in &skill.params {
            let val = args
                .get("params")
                .and_then(|o| o.get(&p.name))
                .and_then(|v| v.as_str())
                .or_else(|| args.get(&p.name).and_then(|v| v.as_str()))
                .unwrap_or(&p.default);
            vars.insert(p.name.clone(), val.to_string());
        }
        vars.insert("name".into(), name.clone());

        // 2) 步骤 → todo，展示执行进度
        let todos: Vec<Todo> = if skill.steps.is_empty() {
            vec![Todo {
                id: 0,
                title: format!("执行技能「{name}」"),
                status: "in_progress".into(),
            }]
        } else {
            skill
                .steps
                .iter()
                .enumerate()
                .map(|(i, s)| Todo {
                    id: i,
                    title: if s.description.is_empty() {
                        s.tool.clone()
                    } else {
                        s.description.clone()
                    },
                    status: if i == 0 { "in_progress".into() } else { "pending".into() },
                })
                .collect()
        };
        *self.todos.lock().unwrap() = todos.clone();
        emit_todo_list(&self.app, &todos);

        // 3) 逐步骤执行
        let mut results: Vec<Value> = Vec::new();
        if skill.steps.is_empty() {
            results.push(json!({
                "instruction": bind_json(&json!(skill.prompt), &vars),
            }));
        } else {
            let mut todo_state = todos;
            for (i, step) in skill.steps.iter().enumerate() {
                // 当前步骤 in_progress，其余 pending
                for (j, t) in todo_state.iter_mut().enumerate() {
                    t.status = if j == i { "in_progress".into() } else { "pending".into() };
                }
                emit_todo_update(&self.app, &todo_state);

                let bound = bind_json(&step.args, &vars);
                let r = match step.tool.as_str() {
                    "notify" => {
                        let body = bound["body"].as_str().or_else(|| bound["msg"].as_str()).unwrap_or("");
                        let title = bound["title"].as_str().unwrap_or("白泽技能已执行");
                        let _ = self.app.emit(
                            "proactive",
                            json!({
                                "id": uuid::Uuid::new_v4().to_string(),
                                "title": title,
                                "body": body,
                                "files": [],
                                "action": body,
                            }),
                        );
                        Ok(json!({ "ok": true, "notify": body }))
                    }
                    tool_name => self.tools.run(tool_name, bound),
                };
                match r {
                    Ok(v) => results.push(json!({ "tool": step.tool, "ok": true, "result": v })),
                    Err(e) => {
                        results.push(json!({ "tool": step.tool, "ok": false, "error": e }));
                    }
                }
            }
        }

        // 4) 反馈闭环：只要有失败则判失败
        let success = results.iter().all(|r| r["ok"] != false);
        self.lib.record_run(&name, success);

        Ok(json!({
            "ok": success,
            "name": name,
            "steps_total": skill.steps.len().max(1),
            "results": results,
        }))
    }
}

/// skill_delete：删除技能
pub struct SkillDeleteTool {
    lib: Arc<SkillLibrary>,
}

impl SkillDeleteTool {
    pub fn new(lib: Arc<SkillLibrary>) -> Self {
        Self { lib }
    }
}

impl Tool for SkillDeleteTool {
    fn name(&self) -> &str {
        "skill_delete"
    }
    fn description(&self) -> &str {
        "删除一个已学习的技能（内置技能删除后重启会恢复）。name 见 skill_list"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "技能名" }
            },
            "required": ["name"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::Write
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let name = args["name"].as_str().ok_or("缺少参数 name")?;
        let removed = self.lib.delete(name);
        Ok(json!({ "ok": true, "name": name, "removed": removed }))
    }
}