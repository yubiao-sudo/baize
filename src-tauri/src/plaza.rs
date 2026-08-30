//! 任务广场（Task Plaza）：统一汇聚「工具 / 工作流 / 技能 / 自研 DIY 工具」。
//!
//! 职责：
//! 1. 浏览与搜索：聚合内置工具（ToolRegistry）、可编排工作流（WorkflowRegistry）、技能库、以及
//!    持久化保存的自研工具，统一成 `PlazaItem` 目录。
//! 2. 自研 DIY：用户或白泽可编写一个命令/脚本工具，保存后动态加载进 `plaza` 命名空间，
//!    白泽即可像内置工具一样调用它。
//! 3. 来源与信任分级：builtin（可信）/ diy（白泽自研）/ market（市场，未受信），
//!    用于前端展示与后续执行安全闸门。
//! 4. 输出路由：声明工具产物可联动到哪些现有窗口组件（文档 / 终端 / 浏览器 / 执行流 / 通知 / 待办 / 剪贴板）。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, State};

use crate::memory::MemoryStore;
use crate::security::PermissionDecision;
use crate::tools::{PermissionClass, ToolRegistry};
use crate::workflow::WorkflowRegistry;
use crate::workmode::{DynamicTool, ToolExec};
use crate::AppState;

// ───────────────────── 数据模型 ─────────────────────

/// 自研工具的载荷：命令 或 脚本（与 `workmode::ToolExec` 对齐）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiyToolSpec {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    /// JSON Schema（工具入参）
    #[serde(default = "default_parameters")]
    pub parameters: Value,
}

fn default_parameters() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// 广场条目：统一描述一个工具 / 工作流 / 技能 / 自研工具
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlazaItem {
    pub id: String,
    pub name: String,
    pub description: String,
    /// "tool" | "workflow" | "skill"
    pub kind: String,
    /// "builtin" | "diy" | "market"
    pub source: String,
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub icon: String,
    /// "trusted" | "authored" | "untrusted"
    pub trust: String,
    /// 输出路由目标：document / terminal / browser / execution_flow / notification / todo / clipboard
    #[serde(default)]
    pub outputs: Vec<String>,
    #[serde(default = "default_true")]
    pub callable: bool,
    /// 工具的入参 JSON Schema（仅 tool 类型有值）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    /// 自研工具载荷（仅 DIY tool 有值）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diy: Option<DiyToolSpec>,
}

fn default_true() -> bool {
    true
}

// ───────────────────── 注册表 ─────────────────────

pub struct PlazaRegistry {
    tools: Arc<ToolRegistry>,
    workflows: Arc<WorkflowRegistry>,
    store: Arc<MemoryStore>,
}

impl PlazaRegistry {
    pub fn new(
        tools: Arc<ToolRegistry>,
        workflows: Arc<WorkflowRegistry>,
        store: Arc<MemoryStore>,
    ) -> Self {
        Self {
            tools,
            workflows,
            store,
        }
    }

    /// 用户持久化的自研条目
    fn persisted(&self) -> Vec<PlazaItem> {
        match self.store.list_plaza_items() {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|(_, data)| serde_json::from_str::<PlazaItem>(&data).ok())
                .collect(),
            Err(e) => {
                eprintln!("[任务广场] 加载失败: {e}");
                Vec::new()
            }
        }
    }

    /// 聚合列出全部条目：内置工具 + 工作流 + 技能 + 自研持久化条目
    pub fn all(&self) -> Vec<PlazaItem> {
        let mut v: Vec<PlazaItem> = Vec::new();

        // 自研工具名（plaza 命名空间），从内置工具列表中剔除，避免重复
        let diy_names: HashSet<String> = self.tools.ns_names("plaza").into_iter().collect();

        // 1) 内置工具
        for s in self.tools.schemas() {
            let name = s["function"]["name"].as_str().unwrap_or("").to_string();
            if diy_names.contains(&name) {
                continue;
            }
            let desc = s["function"]["description"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let params = s["function"]["parameters"].clone();
            let category = infer_category(&name, &desc);
            let outputs = infer_outputs(&name);
            v.push(PlazaItem {
                id: format!("tool:{name}"),
                name,
                description: desc,
                kind: "tool".into(),
                source: "builtin".into(),
                category: category.clone(),
                tags: vec![],
                icon: icon_for(&category).to_string(),
                trust: "trusted".into(),
                outputs,
                callable: true,
                parameters: Some(params),
                diy: None,
            });
        }

        // 2) 可编排工作流
        for w in self.workflows.all() {
            let builtin = w.id == "summary_report" || w.id == "write_spec";
            v.push(PlazaItem {
                id: format!("workflow:{}", w.id),
                name: w.name,
                description: w.description,
                kind: "workflow".into(),
                source: if builtin { "builtin" } else { "diy" }.into(),
                category: "工作流".into(),
                tags: vec![],
                icon: "🔀".into(),
                trust: if builtin { "trusted" } else { "authored" }.into(),
                outputs: vec!["document".into(), "execution_flow".into()],
                callable: true,
                parameters: None,
                diy: None,
            });
        }

        // 3) 技能库
        for s in crate::skill::builtin_skills() {
            v.push(PlazaItem {
                id: format!("skill:{}", s.name),
                name: s.name,
                description: s.description,
                kind: "skill".into(),
                source: "builtin".into(),
                category: "技能".into(),
                tags: vec![],
                icon: "🧠".into(),
                trust: "trusted".into(),
                outputs: vec!["todo".into(), "document".into()],
                callable: true,
                parameters: None,
                diy: None,
            });
        }

        // 4) 自研 / 市场持久化条目
        v.extend(self.persisted());
        v
    }

    pub fn get(&self, id: &str) -> Option<PlazaItem> {
        self.all().into_iter().find(|i| i.id == id)
    }

    /// 保存（新建或覆盖）一个条目；若为自研 tool，则同步刷新动态工具注册
    pub fn save(&self, item: PlazaItem) -> Result<(), String> {
        if item.id.trim().is_empty() {
            return Err("条目缺少 id".into());
        }
        if item.name.trim().is_empty() {
            return Err("条目缺少名称".into());
        }
        if item.kind == "tool" && item.diy.is_none() {
            return Err("tool 类型条目必须提供 diy 载荷（命令或脚本）".into());
        }
        let data = serde_json::to_string(&item).map_err(|e| e.to_string())?;
        self.store.upsert_plaza_item(&item.id, &data)?;
        if item.kind == "tool" {
            self.reload_diy_tools();
        }
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<bool, String> {
        let removed = self.store.delete_plaza_item(id)?;
        if removed {
            // 删除后刷新自研工具注册，保证工具集与持久化一致
            self.reload_diy_tools();
        }
        Ok(removed)
    }

    /// 按工具名查全量条目（内置/工作流/技能/自研）
    pub fn item_for(&self, name: &str) -> Option<PlazaItem> {
        self.all().into_iter().find(|i| i.name == name)
    }

    /// 某条目声明的输出路由
    pub fn outputs_for(&self, name: &str) -> Vec<String> {
        self.item_for(name).map(|i| i.outputs).unwrap_or_default()
    }

    /// 重建 plaza 命名空间下的动态工具（与持久化条目保持一致）
    pub fn reload_diy_tools(&self) {
        self.tools.remove_ns("plaza");
        for item in self.persisted() {
            if item.kind != "tool" {
                continue;
            }
            if let Some(spec) = &item.diy {
                if let Some(tool) = build_dynamic_tool(&item, spec) {
                    self.tools.register_ns("plaza", Box::new(tool));
                }
            }
        }
    }
}

/// 从 DiyToolSpec 构造一个可注册的动态工具（非法载荷返回 None）
fn build_dynamic_tool(item: &PlazaItem, spec: &DiyToolSpec) -> Option<DynamicTool> {
    let exec = if let Some(cmd) = spec.command.as_ref().filter(|c| !c.trim().is_empty()) {
        ToolExec::Command(cmd.clone())
    } else {
        let lang = spec.lang.as_ref()?.clone();
        let code = spec.code.as_ref()?.clone();
        if code.trim().is_empty() {
            return None;
        }
        ToolExec::Script { lang, code }
    };
    Some(DynamicTool::new(
        item.name.clone(),
        item.description.clone(),
        spec.parameters.clone(),
        exec,
        item.trust.clone(),
    ))
}

// ───────────────────── 分类 / 图标 / 输出路由推断 ─────────────────────

fn infer_category(name: &str, _desc: &str) -> String {
    let n = name.to_lowercase();
    if n.contains("file") || n.contains("dir") || n.contains("edit") || n.contains("write") || n.contains("move") {
        return "文件".into();
    }
    if n.contains("browser") {
        return "浏览器".into();
    }
    if n.contains("markdown") || n.contains("document") {
        return "文档".into();
    }
    if n.contains("terminal") || n.contains("shell") || n.contains("command") || n.contains("ps_exec") {
        return "终端".into();
    }
    if n.contains("software") || n.contains("install") || n.contains("disk") || n.contains("system") || n.contains("env_check") {
        return "系统·软件".into();
    }
    if n.contains("rag") || n.contains("memory") || n.contains("vault") {
        return "知识·记忆".into();
    }
    if n.contains("mail") || n.contains("notify") || n.contains("http") || n.contains("db") {
        return "网络·数据".into();
    }
    if n.contains("schedule") || n.contains("remind") || n.contains("workflow") {
        return "编排".into();
    }
    if n.contains("screen") || n.contains("window") || n.contains("mouse") || n.contains("click")
        || n.contains("key") || n.contains("type") || n.contains("capture") || n.contains("ground")
        || n.contains("element") || n.contains("read") {
        return "界面自动化".into();
    }
    if n.contains("ocr") || n.contains("image") || n.contains("text_to_image") {
        return "视觉·OCR".into();
    }
    if n.contains("skill") || n.contains("author_tool") {
        return "技能·自研".into();
    }
    "通用".into()
}

fn infer_outputs(name: &str) -> Vec<String> {
    let n = name.to_lowercase();
    let mut o = Vec::new();
    if n.contains("markdown") || n.contains("document") {
        o.push("document".into());
    }
    if n.contains("browser") {
        o.push("browser".into());
    }
    if n.contains("terminal") {
        o.push("terminal".into());
    }
    if n.contains("todo") {
        o.push("todo".into());
    }
    if n.contains("notify") {
        o.push("notification".into());
    }
    if n.contains("clipboard") {
        o.push("clipboard".into());
    }
    if o.is_empty() {
        o.push("execution_flow".into());
    }
    o
}

fn icon_for(category: &str) -> &'static str {
    match category {
        "文件" => "📄",
        "浏览器" => "🌐",
        "文档" => "📝",
        "终端" => "🖥️",
        "系统·软件" => "🧩",
        "知识·记忆" => "🧠",
        "网络·数据" => "🗄️",
        "编排" => "🔀",
        "界面自动化" => "🖱️",
        "视觉·OCR" => "👁",
        "技能·自研" => "⚙️",
        "工作流" => "🔀",
        "技能" => "🧠",
        _ => "🛠️",
    }
}

// ───────────────────── 市场仓库（内置目录） ─────────────────────

/// 市场仓库的内置目录：代表「市面厂库」当前上架的现成工具。
/// 全部标记 source=market、trust=untrusted；安装进广场后，执行会经审批闸门。
pub fn market_catalog() -> Vec<PlazaItem> {
    vec![
        market_tool(
            "uuid_v4",
            "生成一个随机 UUID v4（无入参）",
            "通用",
            vec!["uuid", "标识"],
            "nodejs",
            r#"const { randomUUID } = require("crypto");
console.log(randomUUID());"#,
            json!({ "type": "object", "properties": {} }),
            vec!["document", "clipboard"],
        ),
        market_tool(
            "current_time",
            "输出当前时间（ISO / Unix 毫秒 / 本地时间 / 时区）",
            "通用",
            vec!["时间", "时钟"],
            "nodejs",
            r#"const now = new Date();
console.log(JSON.stringify({
  iso: now.toISOString(),
  unix_ms: now.getTime(),
  local: now.toString(),
  tz: Intl.DateTimeFormat().resolvedOptions().timeZone
}));"#,
            json!({ "type": "object", "properties": {} }),
            vec!["document", "notification"],
        ),
        market_tool(
            "unix_to_date",
            "把 Unix 时间戳转换为可读日期时间",
            "通用",
            vec!["时间", "转换"],
            "nodejs",
            r#"const ts = {ts};
const d = new Date(ts < 1e12 ? ts * 1000 : ts);
console.log(JSON.stringify({ iso: d.toISOString(), local: d.toString() }));"#,
            json!({
                "type": "object",
                "properties": { "ts": { "type": "number", "description": "Unix 秒或毫秒时间戳" } },
                "required": ["ts"]
            }),
            vec!["document"],
        ),
        market_tool(
            "http_status",
            "探测一个 URL 的 HTTP 状态码与响应耗时",
            "网络·数据",
            vec!["http", "网络", "可用性"],
            "nodejs",
            r#"const t0 = Date.now();
fetch("{url}")
  .then(async r => {
    const body = await r.text();
    console.log(JSON.stringify({ status: r.status, ms: Date.now() - t0, bytes: body.length, ok: r.ok }));
  })
  .catch(e => console.log(JSON.stringify({ error: String(e) })));"#,
            json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "要探测的完整 URL" } },
                "required": ["url"]
            }),
            vec!["document", "execution_flow"],
        ),
        market_tool(
            "text_hash",
            "计算文本的 MD5 / SHA256 摘要与长度",
            "网络·数据",
            vec!["hash", "摘要", "校验"],
            "nodejs",
            r#"const crypto = require("crypto");
const s = "{text}";
console.log(JSON.stringify({
  md5: crypto.createHash("md5").update(s, "utf8").digest("hex"),
  sha256: crypto.createHash("sha256").update(s, "utf8").digest("hex"),
  length: s.length
}));"#,
            json!({
                "type": "object",
                "properties": { "text": { "type": "string", "description": "待计算摘要的文本" } },
                "required": ["text"]
            }),
            vec!["document", "clipboard"],
        ),
        market_tool(
            "platform_info",
            "输出当前系统平台信息（操作系统 / 架构 / Python 版本）",
            "系统·软件",
            vec!["系统", "环境"],
            "python",
            r#"import platform, sys, json
print(json.dumps({
  "os": platform.system(),
  "release": platform.release(),
  "machine": platform.machine(),
  "python": sys.version.split()[0]
}))"#,
            json!({ "type": "object", "properties": {} }),
            vec!["document"],
        ),
    ]
}

/// 构造一个市场来源工具条目（脚本执行，未受信、需审批）
fn market_tool(
    name: &str,
    description: &str,
    category: &str,
    tags: Vec<&str>,
    lang: &str,
    code: &str,
    parameters: Value,
    outputs: Vec<&str>,
) -> PlazaItem {
    PlazaItem {
        id: format!("market:{name}"),
        name: name.to_string(),
        description: description.to_string(),
        kind: "tool".into(),
        source: "market".into(),
        category: category.to_string(),
        tags: tags.into_iter().map(|s| s.to_string()).collect(),
        icon: icon_for(category).to_string(),
        trust: "untrusted".into(),
        outputs: outputs.into_iter().map(|s| s.to_string()).collect(),
        callable: true,
        parameters: Some(parameters.clone()),
        diy: Some(DiyToolSpec {
            command: None,
            lang: Some(lang.to_string()),
            code: Some(code.to_string()),
            parameters,
        }),
    }
}

// ───────────────────── 命令 ─────────────────────

/// 列出任务广场全部条目（内置工具 + 工作流 + 技能 + 自研）
#[tauri::command]
pub fn plaza_list(state: State<'_, AppState>) -> Vec<PlazaItem> {
    state.plaza.all()
}

/// 市场仓库目录（未安装也可浏览；安装后出现在 plaza_list 中）
#[tauri::command]
pub fn plaza_market_catalog(_state: State<'_, AppState>) -> Vec<PlazaItem> {
    market_catalog()
}

/// 从市场仓库安装一个工具到广场（持久化 + 动态加载，标记未受信）
#[tauri::command]
pub fn plaza_market_install(state: State<'_, AppState>, id: String) -> Result<String, String> {
    let item = market_catalog()
        .into_iter()
        .find(|i| i.id == id)
        .ok_or_else(|| format!("市场目录中未找到: {id}"))?;
    state.plaza.save(item.clone())?;
    Ok(format!("已从市场安装「{}」", item.name))
}

/// 保存一个条目（新建/覆盖）；自研 tool 会同步动态加载
#[tauri::command]
pub fn plaza_save_item(state: State<'_, AppState>, item: PlazaItem) -> Result<String, String> {
    state.plaza.save(item.clone())?;
    Ok(format!("已保存「{}」", item.name))
}

/// 删除一个自研条目（内置条目不可删除）
#[tauri::command]
pub fn plaza_delete_item(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    if id.starts_with("tool:") || id.starts_with("workflow:") || id.starts_with("skill:") {
        return Err("内置条目不可删除，仅可删除自研（diy/market）条目".into());
    }
    state.plaza.delete(&id)
}

/// 直接运行广场中的一个工具（按工具名），并按声明的输出路由联动窗口组件。
/// 信任分级执行闸门：未受信（untrusted，市场来源）工具执行前需经用户审批确认。
#[tauri::command]
pub async fn plaza_run(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    args: Option<Value>,
) -> Result<Value, String> {
    let tool = state
        .tools
        .get(&name)
        .ok_or_else(|| format!("未找到工具: {name}"))?;
    let args = args.unwrap_or_else(|| json!({}));

    // 未受信工具：复用 security 高危审批链路，恢复后按决策执行
    let untrusted = state
        .plaza
        .item_for(&name)
        .map(|i| i.trust == "untrusted")
        .unwrap_or(false);
    if untrusted {
        match state
            .security
            .classify(&name, &args, PermissionClass::HighRisk)
        {
            PermissionDecision::AutoDeny => {
                return Err("该市场工具此前已被记住拒绝，本次自动拒绝".into());
            }
            PermissionDecision::Prompt(req) => {
                let _ = app.emit("permission-request", &req);
                // 轮询等待审批（60s 超时默认拒绝）
                let deadline = Instant::now() + Duration::from_secs(60);
                let mut approved = false;
                loop {
                    if let Some(d) = state.security.decision(&req.id) {
                        approved = d;
                        break;
                    }
                    if Instant::now() > deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                if !approved {
                    return Err("未获用户确认，已取消运行该未受信工具".into());
                }
            }
            PermissionDecision::AutoAllow => {}
        }
    }

    let _ = app.emit(
        "thought",
        json!({ "kind": "tool_call", "label": format!("任务广场 · 运行 {name}"), "detail": "执行中…" }),
    );

    let result = tool.run(args)?;

    // 按条目声明的输出路由联动窗口组件（文档/终端/浏览器/通知/待办/剪贴板）
    route_outputs(&app, &state, &name, &result);

    let _ = app.emit(
        "thought",
        json!({ "kind": "tool_result", "label": format!("任务广场 · {name}"), "detail": summarize(&result) }),
    );

    Ok(result)
}

/// 把工具执行结果规整为可写入文档的文本（优先取 stdout 字段）
fn to_document_text(result: &Value) -> String {
    if let Some(s) = result.get("stdout").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    if result.is_string() {
        return result.as_str().unwrap_or("").to_string();
    }
    serde_json::to_string_pretty(result).unwrap_or_default()
}

/// 按条目声明的输出路由，把执行结果联动到对应窗口组件。
/// document=文档窗口、browser=浏览器窗口、terminal=终端窗口、notification=应用弹窗、
/// todo=待办列表、clipboard=系统剪贴板、execution_flow=执行流（由 thought 事件负责）。
fn route_outputs(app: &AppHandle, state: &AppState, name: &str, result: &Value) {
    let outputs = state.plaza.outputs_for(name);
    for target in &outputs {
        match target.as_str() {
            "document" => {
                let content = to_document_text(result);
                if !content.trim().is_empty() {
                    crate::markdown::write_document(
                        app,
                        &state.markdown,
                        &format!("任务广场 · {name}"),
                        &content,
                    );
                }
            }
            "browser" => {
                let html = to_browser_html(result);
                let snap = {
                    let mut b = state.browser.lock().unwrap();
                    b.open_tab("html", &format!("任务广场 · {name}"), &html);
                    b.snapshot()
                };
                crate::windows::ensure_browser_window(app);
                let _ = app.emit_to("browser", "browser-update", &snap);
            }
            "terminal" => {
                crate::windows::ensure_terminal_window(app, state.terminal.clone());
            }
            "notification" => {
                let body = summarize(result);
                let _ = app.emit(
                    "escalation-level",
                    json!({
                        "level": 0,
                        "level_label": "应用弹窗",
                        "title": "白泽 · 任务广场",
                        "body": format!("「{name}」执行完成"),
                        "detail": body,
                    }),
                );
            }
            "todo" => {
                if let Some(todos) = extract_todos(result) {
                    crate::task::emit_todo_list(app, &todos);
                }
            }
            "clipboard" => {
                let text = to_document_text(result);
                if !text.trim().is_empty() {
                    let _ = crate::clipboard::clipboard_set_text(text);
                }
            }
            // execution_flow 由 thought 事件（tool_call / tool_result）承载，无需额外处理
            _ => {}
        }
    }
}

/// 把结果规整为可在浏览器窗口渲染的 HTML（优先取结果中的 html 字段，否则按文本转义渲染）
fn to_browser_html(result: &Value) -> String {
    if let Some(html) = result.get("html").and_then(|v| v.as_str()) {
        return html.to_string();
    }
    let text = to_document_text(result);
    let escaped = text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"></head>\
         <body style=\"font-family: -apple-system,'Segoe UI',monospace; padding:24px; \
         white-space:pre-wrap; word-break:break-all; line-height:1.6;\">{escaped}</body></html>"
    )
}

/// 结果摘要（用于通知 / 执行流文案，超长截断）
fn summarize(result: &Value) -> String {
    let text = to_document_text(result);
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(180).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 从结果中提取待办列表（仅当结果带 `todos` 数组时生效）
fn extract_todos(result: &Value) -> Option<Vec<crate::task::Todo>> {
    let arr = result.get("todos").and_then(|v| v.as_array())?;
    let mut todos = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        let title = item
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        if !title.is_empty() {
            todos.push(crate::task::Todo {
                id: i,
                title,
                status: "pending".into(),
            });
        }
    }
    if todos.is_empty() {
        None
    } else {
        Some(todos)
    }
}