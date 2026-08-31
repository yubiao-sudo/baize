//! 子代理系统（SubAgent）
//!
//! 主 Agent 可以将复杂任务拆解为多个子代理并行执行，每个子代理拥有独立的上下文和工具集。
//! 子代理完成任务后返回结构化摘要，不污染主 Agent 的对话历史。
//!
//! 子代理类型：
//!   - search: 快速代码库搜索（Glob, Grep, LS, Read），只读无状态
//!   - code-explorer: 深度代码探索（多步搜索 + 文件读取），用于理解复杂模块
//!   - general-purpose: 通用编码任务（完整工具集），可读写文件

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::model::{ChatMessage, ModelRouter};
use crate::tools::{global_cancelled, PermissionClass, Tool, ToolRegistry};

// ───────────────── 执行流透出 ─────────────────

/// 子代理内部步骤的执行流管道：既发实时「思考流」事件，也固化进 assistant 消息的
/// 执行流 trace（经 AppState 的思考日志），让「并行子代理」期间的过程在执行流中可见。
#[derive(Clone, Default)]
pub struct SubAgentTrace {
    app: Option<AppHandle>,
}

impl SubAgentTrace {
    pub fn enabled(app: AppHandle) -> Self {
        Self { app: Some(app) }
    }

    pub fn emit(&self, kind: &str, label: impl Into<String>, detail: impl Into<String>) {
        let Some(app) = &self.app else { return };
        let label = label.into();
        let detail = detail.into();
        let _ = app.emit("thought", json!({ "kind": kind, "label": label, "detail": detail }));
        // 固化到执行流 trace（回放执行流可回看）
        if let Some(state) = app.try_state::<crate::AppState>() {
            state.log_thought(kind, &label, &detail);
        }
    }
}

/// 截断长文本（保持执行流条目轻量）
fn trunc(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

// ───────────────── 子代理类型 ─────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubAgentType {
    /// 快速代码库搜索：Glob, Grep, LS, Read
    #[serde(rename = "search")]
    Search,
    /// 深度代码探索：搜索 + 多步文件读取，可追踪引用关系
    #[serde(rename = "code-explorer")]
    CodeExplorer,
    /// 通用编码任务：完整工具集，可读写文件
    #[serde(rename = "general-purpose")]
    GeneralPurpose,
}

impl SubAgentType {
    pub fn label(&self) -> &str {
        match self {
            SubAgentType::Search => "搜索",
            SubAgentType::CodeExplorer => "代码探索",
            SubAgentType::GeneralPurpose => "通用任务",
        }
    }

    /// 返回该类型允许的工具名列表
    fn allowed_tools(&self) -> Vec<&str> {
        match self {
            SubAgentType::Search => {
                vec!["list_files", "read_file", "search_files", "get_file_info", "list_allowed_directories", "list_directory", "list_directory_with_sizes", "directory_tree"]
            }
            SubAgentType::CodeExplorer => {
                vec!["list_files", "read_file", "search_files", "get_file_info", "list_allowed_directories", "list_directory", "list_directory_with_sizes", "directory_tree", "rag_search"]
            }
            SubAgentType::GeneralPurpose => {
                vec!["list_files", "read_file", "search_files", "get_file_info", "list_allowed_directories", "list_directory", "list_directory_with_sizes", "directory_tree", "write_file", "edit_file", "create_directory", "move_file", "run_command", "rag_search", "rag_index", "read_screen", "capture_screen"]
            }
        }
    }

    /// 系统提示词
    fn system_prompt(&self) -> &str {
        match self {
            SubAgentType::Search => {
                "你是一个专业的代码库搜索助手。你的任务是快速搜索代码库，找到相关文件、函数、类或模式。\n\
                 规则：\n\
                 1. 只使用搜索和读取工具，不要修改任何文件\n\
                 2. 用多轮搜索逐步缩小范围\n\
                 3. 发现关键信息后立即读取相关文件\n\
                 4. 最后返回一个简洁的摘要，列出找到的所有相关文件和关键代码位置\n\
                 5. 不要提问，直接完成任务并返回结果"
            }
            SubAgentType::CodeExplorer => {
                "你是一个代码库探索专家。你的任务是深入理解代码库的某个模块或功能。\n\
                 规则：\n\
                 1. 从搜索关键符号开始，逐步追踪引用和依赖关系\n\
                 2. 读取所有相关文件，理解完整的调用链\n\
                 3. 分析代码结构、设计模式和潜在问题\n\
                 4. 最后返回一个结构化的分析报告，包含：模块概览、关键文件、数据流、设计模式\n\
                 5. 不要修改任何文件，只做分析和报告"
            }
            SubAgentType::GeneralPurpose => {
                "你是一个全栈软件工程师。你的任务是完成用户分配的编程任务。\n\
                 规则：\n\
                 1. 先理解任务目标，再搜索相关代码\n\
                 2. 制定修改计划，然后执行\n\
                 3. 修改代码后验证正确性\n\
                 4. 最后返回一个总结，说明完成了什么、修改了哪些文件\n\
                 5. 保持修改最小化，不要过度工程化"
            }
        }
    }
}

// ───────────────── 子代理结果 ─────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentResult {
    pub agent_type: String,
    pub task: String,
    pub summary: String,
    pub files_examined: Vec<String>,
    pub success: bool,
    pub duration_ms: u64,
}

// ───────────────── 子代理执行器 ─────────────────

/// 启动一个子代理，执行任务并返回结果（内部步骤经 trace 透出到执行流）
pub async fn run_subagent(
    agent_type: SubAgentType,
    task: &str,
    model: &ModelRouter,
    tools: &ToolRegistry,
    trace: &SubAgentTrace,
) -> SubAgentResult {
    trace.emit(
        "subagent",
        format!("子代理[{}] 启动", agent_type.label()),
        trunc(task, 120).to_string(),
    );
    let result = run_subagent_inner(agent_type, task, model, tools, trace).await;
    let status = if result.success { "完成" } else { "失败" };
    trace.emit(
        "subagent",
        format!("子代理[{}] {}", agent_type.label(), status),
        format!("{} · {:.1}s", trunc(&result.summary, 100), result.duration_ms as f64 / 1000.0),
    );
    result
}

async fn run_subagent_inner(
    agent_type: SubAgentType,
    task: &str,
    model: &ModelRouter,
    tools: &ToolRegistry,
    trace: &SubAgentTrace,
) -> SubAgentResult {
    let start = std::time::Instant::now();
    let system_prompt = agent_type.system_prompt();
    let allowed = agent_type.allowed_tools();

    let mut messages: Vec<ChatMessage> = vec![
        ChatMessage {
            role: "system".into(),
            content: system_prompt.into(),
            tool_calls: None,
            tool_call_id: None,
        },
        ChatMessage {
            role: "user".into(),
            content: task.to_string(),
            tool_calls: None,
            tool_call_id: None,
        },
    ];

    let max_rounds = match agent_type {
        SubAgentType::Search => 6,
        SubAgentType::CodeExplorer => 10,
        SubAgentType::GeneralPurpose => 12,
    };

    let mut files_examined: Vec<String> = Vec::new();

    for round in 0..max_rounds {
        // 用户点击停止：立即终止子代理，不再发起下一轮模型请求
        if global_cancelled() {
            return SubAgentResult {
                agent_type: agent_type.label().to_string(),
                task: task.to_string(),
                summary: "已被用户停止".to_string(),
                files_examined,
                success: false,
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
        // 调用模型
        let schemas = tools.schemas_filtered(&allowed);
        let resp = match model.chat(&messages, &schemas).await {
            Ok(r) => r,
            Err(e) => {
                return SubAgentResult {
                    agent_type: agent_type.label().to_string(),
                    task: task.to_string(),
                    summary: format!("子代理出错: {e}"),
                    files_examined,
                    success: false,
                    duration_ms: start.elapsed().as_millis() as u64,
                };
            }
        };

        // 如果有 tool_calls，执行它们
        if let Some(ref tool_calls) = resp.tool_calls {
            let assistant_msg = ChatMessage {
                role: "assistant".into(),
                content: resp.content.unwrap_or_default(),
                tool_calls: Some(tool_calls.clone()),
                tool_call_id: None,
            };
            messages.push(assistant_msg);

            for tc in tool_calls {
                // 停止请求：跳过本子代理剩余工具调用
                if global_cancelled() {
                    break;
                }
                let tool_name = tc["function"]["name"].as_str().unwrap_or("unknown");
                let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                let args: Value = serde_json::from_str(args_str).unwrap_or(Value::Null);
                let tc_id = tc["id"].as_str().unwrap_or("").to_string();

                // 内部步骤透出到执行流：实时思考流 + trace 固化，让用户看见子代理在做什么
                trace.emit(
                    "tool_call",
                    format!("子代理[{}] · {}", agent_type.label(), tool_name),
                    trunc(args_str, 240),
                );

                let result = if allowed.contains(&tool_name) {
                    tools.run(tool_name, args.clone()).unwrap_or_else(|e| {
                        json!({"error": e})
                    })
                } else {
                    json!({"error": format!("子代理类型 {} 不允许使用工具 {}", agent_type.label(), tool_name)})
                };

                trace.emit(
                    "tool_result",
                    format!("工具完成 · 子代理[{}] · {}", agent_type.label(), tool_name),
                    trunc(&result.to_string(), 400),
                );

                // 记录被检查的文件
                if tool_name == "read_file" || tool_name == "search_files" || tool_name == "list_files" {
                    if let Some(path) = args.get("file_path").or_else(|| args.get("path")).and_then(|v| v.as_str()) {
                        if !files_examined.contains(&path.to_string()) {
                            files_examined.push(path.to_string());
                        }
                    }
                }

                messages.push(ChatMessage {
                    role: "tool".into(),
                    content: result.to_string(),
                    tool_calls: None,
                    tool_call_id: Some(tc_id),
                });
            }
        } else {
            // 没有 tool_calls，模型返回最终答案
            let summary = resp.content.unwrap_or_else(|| "子代理完成但未返回内容".to_string());
            return SubAgentResult {
                agent_type: agent_type.label().to_string(),
                task: task.to_string(),
                summary,
                files_examined,
                success: true,
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }

        // 安全检查：如果连续多轮没有进展，尽早退出
        if round >= max_rounds - 1 {
            // 最后一轮：让模型总结
            messages.push(ChatMessage {
                role: "user".into(),
                content: "请总结你的发现，返回一个简洁的摘要。".to_string(),
                tool_calls: None,
                tool_call_id: None,
            });
            let final_resp = match model.chat(&messages, &[]).await {
                Ok(r) => r.content.unwrap_or_else(|| "子代理超时".to_string()),
                Err(e) => format!("子代理总结失败: {e}"),
            };
            return SubAgentResult {
                agent_type: agent_type.label().to_string(),
                task: task.to_string(),
                summary: final_resp,
                files_examined,
                success: true,
                duration_ms: start.elapsed().as_millis() as u64,
            };
        }
    }

    SubAgentResult {
        agent_type: agent_type.label().to_string(),
        task: task.to_string(),
        summary: "子代理达到最大轮数但未返回最终结果".to_string(),
        files_examined,
        success: false,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

// ───────────────── spawn_subagent 工具 ─────────────────

/// 解析子代理类型参数（供工具与运行期共用）
pub fn parse_agent_type(args: &Value) -> Result<SubAgentType, String> {
    match args["subagent_type"].as_str() {
        Some("search") => Ok(SubAgentType::Search),
        Some("code-explorer") => Ok(SubAgentType::CodeExplorer),
        Some("general-purpose") => Ok(SubAgentType::GeneralPurpose),
        other => Err(format!("未知的子代理类型: {other:?}")),
    }
}

/// spawn_subagent 工具：主 Agent 调用此工具启动子代理并同步等待其结果。
/// 单个子代理在此工具内联执行（block_on）；当一轮内派发多个子代理时，
/// 由 AgentLoop 运行期改走 execute_subagents_parallel 并发执行。
pub struct SpawnSubAgentTool {
    model: Arc<ModelRouter>,
    tools: Arc<ToolRegistry>,
    trace: SubAgentTrace,
}

impl SpawnSubAgentTool {
    pub fn new(model: Arc<ModelRouter>, tools: Arc<ToolRegistry>, trace: SubAgentTrace) -> Self {
        Self {
            model,
            tools,
            trace,
        }
    }
}

impl Tool for SpawnSubAgentTool {
    fn name(&self) -> &str {
        "spawn_subagent"
    }
    fn description(&self) -> &str {
        "启动一个子代理来执行独立的子任务。子代理拥有自己的上下文和工具集，不污染主对话历史。\
         支持三种类型：\
         - search: 快速搜索代码库（Glob, Grep, LS, Read）\
         - code-explorer: 深度代码探索（多步搜索 + 追踪引用）\
         - general-purpose: 通用编码任务（完整工具集，可读写文件）\
         可以同时启动多个子代理并行执行，用 Promise.all 模式。"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "subagent_type": {
                    "type": "string",
                    "enum": ["search", "code-explorer", "general-purpose"],
                    "description": "子代理类型"
                },
                "description": {
                    "type": "string",
                    "description": "简短描述（3-5个字），用于日志标识"
                },
                "task": {
                    "type": "string",
                    "description": "详细的任务说明，子代理将独立完成此任务"
                }
            },
            "required": ["subagent_type", "description", "task"]
        })
    }
    fn permission(&self) -> PermissionClass {
        PermissionClass::ReadOnly // 子代理本身是只读的（general-purpose 也由主 Agent 审批）
    }
    fn run(&self, args: Value) -> Result<Value, String> {
        let agent_type = parse_agent_type(&args)?;
        let task = args["task"]
            .as_str()
            .ok_or("缺少 task 参数")?
            .to_string();

        // run 是同步的（spawn_blocking 线程内执行），用 block_on 同步等待异步子代理跑完
        let model = self.model.clone();
        let tools = self.tools.clone();
        let trace = self.trace.clone();
        let result = tauri::async_runtime::block_on(async move {
            run_subagent(agent_type, &task, &model, &tools, &trace).await
        });

        Ok(json!({
            "ok": result.success,
            "agent_type": result.agent_type,
            "summary": result.summary,
            "files_examined": result.files_examined,
            "duration_ms": result.duration_ms,
        }))
    }
}

/// 批量并行执行多个子代理
pub async fn execute_subagents_parallel(
    specs: Vec<(SubAgentType, String)>,
    model: Arc<ModelRouter>,
    tools: Arc<ToolRegistry>,
    trace: SubAgentTrace,
) -> Vec<SubAgentResult> {
    let futures: Vec<_> = specs
        .into_iter()
        .map(|(agent_type, task)| {
            let model = model.clone();
            let tools = tools.clone();
            let trace = trace.clone();
            tokio::spawn(async move { run_subagent(agent_type, &task, &model, &tools, &trace).await })
        })
        .collect();

    let mut results = Vec::new();
    for f in futures {
        match f.await {
            Ok(r) => results.push(r),
            Err(e) => results.push(SubAgentResult {
                agent_type: "unknown".to_string(),
                task: "".to_string(),
                summary: format!("子代理 panic: {e}"),
                files_examined: vec![],
                success: false,
                duration_ms: 0,
            }),
        }
    }
    results
}